//! End-to-end tests of `ForwardingProxy`: connects it to a hand-rolled "remote"
//! MCP server over an in-memory duplex transport, then drives it from a
//! separate "caller" client over a second duplex transport.
//!
//! Both ends of every connection must be brought up concurrently (`tokio::join!`)
//! since `serve_server(...).await` blocks until it receives the peer's
//! `initialize` request — awaiting it before starting the client side deadlocks.

use std::sync::{Arc, Mutex};

use mcp_proxy::proxy_server::{new_progress_relay_map, ForwardingProxy, RemoteNotificationRelay};
use rmcp::model::*;
use rmcp::service::{NotificationContext, RequestContext, RunningService};
use rmcp::{
    serve_client, serve_server, ClientHandler, ErrorData as McpError, RoleClient, RoleServer,
    ServerHandler, ServiceExt,
};

// ---------------------------------------------------------------------------
// Minimal remote server (tools + prompts only)
// ---------------------------------------------------------------------------

struct TestRemoteServer;

impl ServerHandler for TestRemoteServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().enable_prompts().build())
            .with_server_info(Implementation::new("test-remote", "0.0.0"))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(vec![Tool::new(
            "echo",
            "echoes its input",
            Arc::new(Default::default()),
        )]))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        if request.name != "echo" {
            return Err(McpError::method_not_found::<CallToolRequestMethod>());
        }
        // `_meta` (including `progressToken`) is extracted into `context.meta` by the
        // service layer; `request.progress_token()` is always None.
        if let Some(token) = context.meta.get_progress_token() {
            for step in [0.5, 1.0] {
                let _ = context
                    .peer
                    .notify_progress(ProgressNotificationParam {
                        progress_token: token.clone(),
                        progress: step,
                        total: Some(1.0),
                        message: Some(format!("step {step}")),
                    })
                    .await;
            }
        }
        Ok(CallToolResult::success(vec![Content::text("echoed")]))
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        Ok(ListPromptsResult::default())
    }
}

// ---------------------------------------------------------------------------
// Full-capability remote server (all MCP capabilities)
// ---------------------------------------------------------------------------

struct FullCapabilityRemoteServer;

impl ServerHandler for FullCapabilityRemoteServer {
    #[allow(deprecated)]
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_prompts()
                .enable_resources()
                .enable_logging()
                .build(),
        )
        .with_server_info(Implementation::new("full-remote", "0.0.0"))
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        Ok(ListPromptsResult::with_all_items(vec![Prompt::new(
            "prompt1",
            None::<String>,
            None,
        )]))
    }

    async fn get_prompt(
        &self,
        _request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResult, McpError> {
        Ok(GetPromptResult::new(vec![PromptMessage::new_text(
            PromptMessageRole::User,
            "hello",
        )]))
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult::with_all_items(vec![
            RawResource::new("scheme://test", "test-resource").no_annotation(),
        ]))
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        Ok(ListResourceTemplatesResult::with_all_items(vec![
            RawResourceTemplate::new("scheme://{name}", "test-template").no_annotation(),
        ]))
    }

    async fn read_resource(
        &self,
        _request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        Ok(ReadResourceResult::new(vec![
            ResourceContents::TextResourceContents {
                uri: "scheme://test".to_string(),
                mime_type: Some("text/plain".to_string()),
                text: "resource-content".to_string(),
                meta: None,
            },
        ]))
    }

    async fn subscribe(
        &self,
        _request: SubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        Ok(())
    }

    async fn unsubscribe(
        &self,
        _request: UnsubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        Ok(())
    }

    #[allow(deprecated)]
    async fn set_level(
        &self,
        _request: SetLevelRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        Ok(())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(vec![
            Tool::new("ok-tool", "returns success", Arc::new(Default::default())),
            Tool::new(
                "fail-tool",
                "returns an internal error (becomes CallToolResult::error)",
                Arc::new(Default::default()),
            ),
        ]))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        match request.name.as_ref() {
            "ok-tool" => Ok(CallToolResult::success(vec![Content::text("ok")])),
            "fail-tool" => Err(McpError::internal_error("tool failed", None)),
            _ => Err(McpError::method_not_found::<CallToolRequestMethod>()),
        }
    }

    async fn complete(
        &self,
        _request: CompleteRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CompleteResult, McpError> {
        Ok(CompleteResult::new(CompletionInfo::default()))
    }
}

// ---------------------------------------------------------------------------
// Recording client handler (captures progress notifications from proxy)
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct RecordingClientHandler {
    progress_events: Arc<Mutex<Vec<ProgressNotificationParam>>>,
}

impl ClientHandler for RecordingClientHandler {
    async fn on_progress(
        &self,
        params: ProgressNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        self.progress_events.lock().unwrap().push(params);
    }
}

// ---------------------------------------------------------------------------
// Test harnesses
// ---------------------------------------------------------------------------

/// Keeps every `RunningService` alive for the duration of a test.
#[allow(dead_code)]
struct Harness {
    remote_server: RunningService<RoleServer, TestRemoteServer>,
    remote_client: RunningService<RoleClient, RemoteNotificationRelay>,
}

#[allow(dead_code)]
struct FullHarness {
    remote_server: RunningService<RoleServer, FullCapabilityRemoteServer>,
    remote_client: RunningService<RoleClient, RemoteNotificationRelay>,
}

async fn build_proxy() -> (ForwardingProxy, Harness) {
    let (remote_client_io, remote_server_io) = tokio::io::duplex(8192);
    let relay = new_progress_relay_map();

    let (remote_server, remote_client) = tokio::join!(
        serve_server(TestRemoteServer, remote_server_io),
        serve_client(RemoteNotificationRelay::new(relay.clone()), remote_client_io),
    );
    let remote_server = remote_server.expect("serve test remote server");
    let remote_client = remote_client.expect("connect to test remote server");

    let proxy = ForwardingProxy::new(remote_client.peer().clone(), relay);
    (proxy, Harness { remote_server, remote_client })
}

async fn build_full_proxy() -> (ForwardingProxy, FullHarness) {
    let (remote_client_io, remote_server_io) = tokio::io::duplex(8192);
    let relay = new_progress_relay_map();

    let (remote_server, remote_client) = tokio::join!(
        serve_server(FullCapabilityRemoteServer, remote_server_io),
        serve_client(RemoteNotificationRelay::new(relay.clone()), remote_client_io),
    );
    let remote_server = remote_server.expect("serve full remote server");
    let remote_client = remote_client.expect("connect to full remote server");

    let proxy = ForwardingProxy::new(remote_client.peer().clone(), relay);
    (proxy, FullHarness { remote_server, remote_client })
}

// ---------------------------------------------------------------------------
// Tests — capability mirroring and basic forwarding
// ---------------------------------------------------------------------------

#[tokio::test]
async fn mirrors_remote_capabilities() {
    let (proxy, _harness) = build_proxy().await;
    let (caller_io, server_io) = tokio::io::duplex(8192);
    let (proxy_server, caller) = tokio::join!(serve_server(proxy, server_io), ().serve(caller_io));
    let _proxy_server = proxy_server.expect("serve proxy");
    let caller = caller.expect("connect to proxy");

    let peer_info = caller.peer_info().expect("peer info available after handshake");
    assert!(peer_info.capabilities.tools.is_some());
    assert!(peer_info.capabilities.prompts.is_some());
    assert!(peer_info.capabilities.resources.is_none());
    assert_eq!(peer_info.server_info.name, "test-remote");
}

#[tokio::test]
async fn forwards_list_tools_and_call_tool() {
    let (proxy, _harness) = build_proxy().await;
    let (caller_io, server_io) = tokio::io::duplex(8192);
    let (proxy_server, caller) = tokio::join!(serve_server(proxy, server_io), ().serve(caller_io));
    let _proxy_server = proxy_server.expect("serve proxy");
    let caller = caller.expect("connect to proxy");

    let tools = caller.list_tools(None).await.expect("list_tools");
    assert_eq!(tools.tools.len(), 1);
    assert_eq!(tools.tools[0].name, "echo");

    let result = caller
        .call_tool(CallToolRequestParams::new("echo"))
        .await
        .expect("call_tool");
    assert_eq!(result.is_error, Some(false));
}

#[tokio::test]
async fn rejects_unsupported_capability_with_method_not_found() {
    let (proxy, _harness) = build_proxy().await;
    let (caller_io, server_io) = tokio::io::duplex(8192);
    let (proxy_server, caller) = tokio::join!(serve_server(proxy, server_io), ().serve(caller_io));
    let _proxy_server = proxy_server.expect("serve proxy");
    let caller = caller.expect("connect to proxy");

    let result = caller.list_resources(None).await;
    assert!(result.is_err(), "resources capability was not declared by the remote, expected an error");
}

#[tokio::test]
async fn relays_progress_notifications_from_remote_to_caller() {
    let (proxy, _harness) = build_proxy().await;
    let (caller_io, server_io) = tokio::io::duplex(8192);
    let recorder = RecordingClientHandler::default();
    let events = recorder.progress_events.clone();

    let (proxy_server, caller) =
        tokio::join!(serve_server(proxy, server_io), recorder.serve(caller_io));
    let _proxy_server = proxy_server.expect("serve proxy");
    let caller = caller.expect("connect to proxy");

    let result =
        caller.call_tool(CallToolRequestParams::new("echo")).await.expect("call_tool");
    assert_eq!(result.is_error, Some(false));

    // Progress notifications race the final response over the same connection;
    // give the last one a moment to arrive.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let recorded = events.lock().unwrap().clone();
    assert_eq!(recorded.len(), 2, "expected both progress notifications to be relayed");
    assert_eq!(recorded[0].progress, 0.5);
    assert_eq!(recorded[1].progress, 1.0);
}

// ---------------------------------------------------------------------------
// Tests — full-capability forwarding (list_prompts, get_prompt, resources…)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn mirrors_all_capabilities() {
    let (proxy, _harness) = build_full_proxy().await;
    let (caller_io, server_io) = tokio::io::duplex(8192);
    let (proxy_server, caller) = tokio::join!(serve_server(proxy, server_io), ().serve(caller_io));
    let _proxy_server = proxy_server.expect("serve proxy");
    let caller = caller.expect("connect to proxy");

    let info = caller.peer_info().expect("peer info");
    assert!(info.capabilities.tools.is_some());
    assert!(info.capabilities.prompts.is_some());
    assert!(info.capabilities.resources.is_some());
    assert!(info.capabilities.logging.is_some());
}

#[tokio::test]
async fn forwards_list_prompts() {
    let (proxy, _harness) = build_full_proxy().await;
    let (caller_io, server_io) = tokio::io::duplex(8192);
    let (proxy_server, caller) = tokio::join!(serve_server(proxy, server_io), ().serve(caller_io));
    let _proxy_server = proxy_server.expect("serve proxy");
    let caller = caller.expect("connect to proxy");

    let result = caller.list_prompts(None).await.expect("list_prompts");
    assert_eq!(result.prompts.len(), 1);
    assert_eq!(result.prompts[0].name, "prompt1");
}

#[tokio::test]
async fn forwards_get_prompt() {
    let (proxy, _harness) = build_full_proxy().await;
    let (caller_io, server_io) = tokio::io::duplex(8192);
    let (proxy_server, caller) = tokio::join!(serve_server(proxy, server_io), ().serve(caller_io));
    let _proxy_server = proxy_server.expect("serve proxy");
    let caller = caller.expect("connect to proxy");

    let result = caller
        .get_prompt(GetPromptRequestParams::new("prompt1"))
        .await
        .expect("get_prompt");
    assert_eq!(result.messages.len(), 1);
}

#[tokio::test]
async fn forwards_list_resources() {
    let (proxy, _harness) = build_full_proxy().await;
    let (caller_io, server_io) = tokio::io::duplex(8192);
    let (proxy_server, caller) = tokio::join!(serve_server(proxy, server_io), ().serve(caller_io));
    let _proxy_server = proxy_server.expect("serve proxy");
    let caller = caller.expect("connect to proxy");

    let result = caller.list_resources(None).await.expect("list_resources");
    assert_eq!(result.resources.len(), 1);
    assert_eq!(result.resources[0].uri, "scheme://test");
}

#[tokio::test]
async fn forwards_list_resource_templates() {
    let (proxy, _harness) = build_full_proxy().await;
    let (caller_io, server_io) = tokio::io::duplex(8192);
    let (proxy_server, caller) = tokio::join!(serve_server(proxy, server_io), ().serve(caller_io));
    let _proxy_server = proxy_server.expect("serve proxy");
    let caller = caller.expect("connect to proxy");

    let result = caller
        .list_resource_templates(None)
        .await
        .expect("list_resource_templates");
    assert_eq!(result.resource_templates.len(), 1);
    assert_eq!(result.resource_templates[0].uri_template, "scheme://{name}");
}

#[tokio::test]
async fn forwards_read_resource() {
    let (proxy, _harness) = build_full_proxy().await;
    let (caller_io, server_io) = tokio::io::duplex(8192);
    let (proxy_server, caller) = tokio::join!(serve_server(proxy, server_io), ().serve(caller_io));
    let _proxy_server = proxy_server.expect("serve proxy");
    let caller = caller.expect("connect to proxy");

    let result = caller
        .read_resource(ReadResourceRequestParams::new("scheme://test"))
        .await
        .expect("read_resource");
    assert_eq!(result.contents.len(), 1);
    match &result.contents[0] {
        ResourceContents::TextResourceContents { text, .. } => {
            assert_eq!(text, "resource-content");
        }
        _ => panic!("expected text resource contents"),
    }
}

#[tokio::test]
async fn forwards_subscribe_resource() {
    let (proxy, _harness) = build_full_proxy().await;
    let (caller_io, server_io) = tokio::io::duplex(8192);
    let (proxy_server, caller) = tokio::join!(serve_server(proxy, server_io), ().serve(caller_io));
    let _proxy_server = proxy_server.expect("serve proxy");
    let caller = caller.expect("connect to proxy");

    caller
        .subscribe(SubscribeRequestParams::new("scheme://test"))
        .await
        .expect("subscribe should succeed");
}

#[tokio::test]
async fn forwards_unsubscribe_resource() {
    let (proxy, _harness) = build_full_proxy().await;
    let (caller_io, server_io) = tokio::io::duplex(8192);
    let (proxy_server, caller) = tokio::join!(serve_server(proxy, server_io), ().serve(caller_io));
    let _proxy_server = proxy_server.expect("serve proxy");
    let caller = caller.expect("connect to proxy");

    caller
        .unsubscribe(UnsubscribeRequestParams::new("scheme://test"))
        .await
        .expect("unsubscribe should succeed");
}

#[tokio::test]
#[allow(deprecated)]
async fn forwards_set_level() {
    let (proxy, _harness) = build_full_proxy().await;
    let (caller_io, server_io) = tokio::io::duplex(8192);
    let (proxy_server, caller) = tokio::join!(serve_server(proxy, server_io), ().serve(caller_io));
    let _proxy_server = proxy_server.expect("serve proxy");
    let caller = caller.expect("connect to proxy");

    caller
        .set_level(SetLevelRequestParams::new(LoggingLevel::Debug))
        .await
        .expect("set_level should succeed");
}

#[tokio::test]
async fn forwards_complete() {
    let (proxy, _harness) = build_full_proxy().await;
    let (caller_io, server_io) = tokio::io::duplex(8192);
    let (proxy_server, caller) = tokio::join!(serve_server(proxy, server_io), ().serve(caller_io));
    let _proxy_server = proxy_server.expect("serve proxy");
    let caller = caller.expect("connect to proxy");

    let result = caller
        .complete(CompleteRequestParams::new(
            Reference::for_prompt("prompt1"),
            ArgumentInfo { name: "arg".into(), value: "val".into() },
        ))
        .await
        .expect("complete should succeed");
    assert!(result.completion.values.is_empty());
}

// ---------------------------------------------------------------------------
// Tests — error handling
// ---------------------------------------------------------------------------

#[tokio::test]
async fn call_tool_remote_error_becomes_error_result() {
    let (proxy, _harness) = build_full_proxy().await;
    let (caller_io, server_io) = tokio::io::duplex(8192);
    let (proxy_server, caller) = tokio::join!(serve_server(proxy, server_io), ().serve(caller_io));
    let _proxy_server = proxy_server.expect("serve proxy");
    let caller = caller.expect("connect to proxy");

    // "fail-tool" on the remote throws an internal error; the proxy must convert
    // it to a successful protocol response with isError = true.
    let result = caller
        .call_tool(CallToolRequestParams::new("fail-tool"))
        .await
        .expect("call_tool must not fail at the protocol level");
    assert_eq!(result.is_error, Some(true), "expected isError to be true");
    assert!(!result.content.is_empty(), "expected an error message in content");
}

// ---------------------------------------------------------------------------
// Tests — progress notification in reverse direction (caller → remote)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn send_progress_notification_does_not_error() {
    let (proxy, _harness) = build_proxy().await;
    let (caller_io, server_io) = tokio::io::duplex(8192);
    let (proxy_server, caller) = tokio::join!(serve_server(proxy, server_io), ().serve(caller_io));
    let _proxy_server = proxy_server.expect("serve proxy");
    let caller = caller.expect("connect to proxy");

    // Sending a progress notification from caller toward the proxy (and then
    // the remote) must not produce a transport error.
    caller
        .notify_progress(ProgressNotificationParam {
            progress_token: ProgressToken(NumberOrString::Number(1)),
            progress: 0.5,
            total: Some(1.0),
            message: None,
        })
        .await
        .expect("notify_progress should succeed");
}
