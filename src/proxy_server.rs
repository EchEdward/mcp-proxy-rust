//! Generic MCP forwarding proxy: exposes a remote MCP server (reached via
//! `Peer<RoleClient>`) as a local `ServerHandler`, mirroring its declared
//! capabilities exactly and relaying tool-call progress notifications.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rmcp::model::*;
use rmcp::service::{NotificationContext, PeerRequestOptions, RequestContext};
use rmcp::{ClientHandler, ErrorData as McpError, Peer, RoleClient, RoleServer, ServerHandler, ServiceError};

/// Correlates a progress token minted for an outbound request to the remote
/// server with the original caller's peer and the progress token *they* used,
/// so progress notifications from the remote can be relayed back unchanged in
/// spirit (only the token is remapped).
pub type ProgressRelayMap = Arc<Mutex<HashMap<ProgressToken, (Peer<RoleServer>, ProgressToken)>>>;

pub fn new_progress_relay_map() -> ProgressRelayMap {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Handles notifications arriving from the remote server while we are its
/// client. Only progress relay is implemented; everything else uses the
/// `ClientHandler` defaults (mirrors the Python proxy, which never registers
/// a `message_handler`/`logging_callback` on its `ClientSession`).
#[derive(Clone, Default)]
pub struct RemoteNotificationRelay {
    progress_relay: ProgressRelayMap,
}

impl RemoteNotificationRelay {
    pub fn new(progress_relay: ProgressRelayMap) -> Self {
        Self { progress_relay }
    }
}

impl ClientHandler for RemoteNotificationRelay {
    async fn on_progress(&self, params: ProgressNotificationParam, _context: NotificationContext<RoleClient>) {
        let entry = self.progress_relay.lock().unwrap().get(&params.progress_token).cloned();
        let Some((caller_peer, caller_token)) = entry else {
            return;
        };
        let relayed = ProgressNotificationParam {
            progress_token: caller_token,
            progress: params.progress,
            total: params.total,
            message: params.message,
        };
        if let Err(error) = caller_peer.notify_progress(relayed).await {
            tracing::warn!(%error, "failed to relay progress notification to caller");
        }
    }
}

/// Exposes a connected remote MCP server as a local `ServerHandler`,
/// forwarding every request and mirroring the remote's declared capabilities
/// (captured once from its `InitializeResult` at connect time).
#[derive(Clone)]
pub struct ForwardingProxy {
    remote: Peer<RoleClient>,
    remote_info: Arc<ServerInfo>,
    progress_relay: ProgressRelayMap,
}

impl ForwardingProxy {
    /// `remote` must already be initialized (its `peer_info()` must be set) -
    /// true immediately after `serve_client(...).await?` returns.
    pub fn new(remote: Peer<RoleClient>, progress_relay: ProgressRelayMap) -> Self {
        let remote_info = remote
            .peer_info()
            .expect("remote peer must complete its handshake before building a ForwardingProxy");
        Self { remote, remote_info, progress_relay }
    }

    fn capabilities(&self) -> &ServerCapabilities {
        &self.remote_info.capabilities
    }
}

fn to_mcp_error(error: ServiceError) -> McpError {
    match error {
        ServiceError::McpError(error) => error,
        other => McpError::internal_error(other.to_string(), None),
    }
}

impl ServerHandler for ForwardingProxy {
    fn get_info(&self) -> ServerInfo {
        (*self.remote_info).clone()
    }

    async fn list_prompts(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        if self.capabilities().prompts.is_none() {
            return Err(McpError::method_not_found::<ListPromptsRequestMethod>());
        }
        self.remote.list_prompts(request).await.map_err(to_mcp_error)
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResult, McpError> {
        if self.capabilities().prompts.is_none() {
            return Err(McpError::method_not_found::<GetPromptRequestMethod>());
        }
        self.remote.get_prompt(request).await.map_err(to_mcp_error)
    }

    async fn list_resources(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        if self.capabilities().resources.is_none() {
            return Err(McpError::method_not_found::<ListResourcesRequestMethod>());
        }
        self.remote.list_resources(request).await.map_err(to_mcp_error)
    }

    async fn list_resource_templates(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        if self.capabilities().resources.is_none() {
            return Err(McpError::method_not_found::<ListResourceTemplatesRequestMethod>());
        }
        self.remote.list_resource_templates(request).await.map_err(to_mcp_error)
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        if self.capabilities().resources.is_none() {
            return Err(McpError::method_not_found::<ReadResourceRequestMethod>());
        }
        self.remote.read_resource(request).await.map_err(to_mcp_error)
    }

    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        if self.capabilities().resources.is_none() {
            return Err(McpError::method_not_found::<SubscribeRequestMethod>());
        }
        self.remote.subscribe(request).await.map_err(to_mcp_error)
    }

    async fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        if self.capabilities().resources.is_none() {
            return Err(McpError::method_not_found::<UnsubscribeRequestMethod>());
        }
        self.remote.unsubscribe(request).await.map_err(to_mcp_error)
    }

    #[allow(deprecated)]
    async fn set_level(
        &self,
        request: SetLevelRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        if self.capabilities().logging.is_none() {
            return Err(McpError::method_not_found::<SetLevelRequestMethod>());
        }
        self.remote.set_level(request).await.map_err(to_mcp_error)
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        if self.capabilities().tools.is_none() {
            return Err(McpError::method_not_found::<ListToolsRequestMethod>());
        }
        self.remote.list_tools(request).await.map_err(to_mcp_error)
    }

    /// Forwards a tool call to the remote server. Mirrors Python: any failure
    /// (tool-level or transport-level) becomes `Ok(CallToolResult::error(...))`
    /// rather than a JSON-RPC protocol error, except for the capability guard
    /// above (an unroutable request).
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        if self.capabilities().tools.is_none() {
            return Err(McpError::method_not_found::<CallToolRequestMethod>());
        }

        // The service layer always extracts `_meta` (including any `progressToken`) into
        // `context.meta` during deserialization, so `request.progress_token()` is always
        // `None`. Read from context instead.
        let Some(caller_token) = context.meta.get_progress_token() else {
            return Ok(match self.remote.call_tool(request).await {
                Ok(result) => result,
                Err(error) => CallToolResult::error(vec![Content::text(error.to_string())]),
            });
        };

        // A progress token is present: bypass the convenience method so we can
        // capture the fresh token rmcp mints for the outbound request, and
        // relay progress notifications tagged with it back to the caller's
        // own token via `RemoteNotificationRelay::on_progress`.
        let caller_peer = context.peer.clone();
        let outbound = ClientRequest::CallToolRequest(CallToolRequest::new(request));
        let handle = match self
            .remote
            .send_cancellable_request(outbound, PeerRequestOptions::no_options())
            .await
        {
            Ok(handle) => handle,
            Err(error) => return Ok(CallToolResult::error(vec![Content::text(error.to_string())])),
        };
        let remote_token = handle.progress_token.clone();
        self.progress_relay
            .lock()
            .unwrap()
            .insert(remote_token.clone(), (caller_peer, caller_token));

        let response = handle.await_response().await;
        self.progress_relay.lock().unwrap().remove(&remote_token);

        Ok(match response {
            Ok(ServerResult::CallToolResult(result)) => result,
            Ok(_) => CallToolResult::error(vec![Content::text(
                "unexpected response type from remote server",
            )]),
            Err(error) => CallToolResult::error(vec![Content::text(error.to_string())]),
        })
    }

    /// Unguarded: Python registers this handler unconditionally too, regardless
    /// of any declared `completions` capability.
    async fn complete(
        &self,
        request: CompleteRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CompleteResult, McpError> {
        self.remote.complete(request).await.map_err(to_mcp_error)
    }

    /// Reverse direction: the downstream caller is reporting progress on some
    /// request we (or the remote) sent to it. Passed straight through with no
    /// token remapping, mirroring Python's unconditional `_send_progress_notification`.
    async fn on_progress(&self, params: ProgressNotificationParam, _context: NotificationContext<RoleServer>) {
        if let Err(error) = self.remote.notify_progress(params).await {
            tracing::warn!(%error, "failed to forward caller progress notification to remote");
        }
    }
}
