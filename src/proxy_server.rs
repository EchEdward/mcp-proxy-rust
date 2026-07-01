//! Generic MCP forwarding proxy: exposes a remote MCP server (reached via
//! `Peer<RoleClient>`) as a local `ServerHandler`, mirroring its declared
//! capabilities exactly and relaying tool-call progress notifications.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

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

/// Exposes a pool of connected remote MCP peers as a single local
/// `ServerHandler`, forwarding every request via round-robin across the pool.
///
/// Why a pool? The underlying streamable-HTTP transport in rmcp processes one
/// outbound POST at a time per `Peer` connection (single-threaded worker,
/// head-of-line blocking). A pool of N independent connections allows up to N
/// requests to be in flight concurrently; a slow or stuck request on one
/// connection only blocks the 1/N of traffic routed through it.
///
/// All peers in the pool must already be initialized (their `peer_info()` set)
/// and must talk to the same upstream server. Capabilities are read once from
/// the first peer and assumed identical across the pool.
#[derive(Clone)]
pub struct ForwardingProxy {
    remotes: Arc<Vec<Peer<RoleClient>>>,
    next: Arc<AtomicUsize>,
    remote_info: Arc<ServerInfo>,
    progress_relay: ProgressRelayMap,
    timeout: Duration,
}

impl ForwardingProxy {
    pub fn new(
        remotes: Vec<Peer<RoleClient>>,
        progress_relay: ProgressRelayMap,
        timeout: Duration,
    ) -> Self {
        assert!(!remotes.is_empty(), "ForwardingProxy requires at least one peer");
        let remote_info = remotes[0]
            .peer_info()
            .expect("all remote peers must complete their handshake before building a ForwardingProxy");
        Self {
            remotes: Arc::new(remotes),
            next: Arc::new(AtomicUsize::new(0)),
            remote_info,
            progress_relay,
            timeout,
        }
    }

    /// Pick the next peer in round-robin order (lock-free).
    fn pick(&self) -> &Peer<RoleClient> {
        let idx = self.next.fetch_add(1, Ordering::Relaxed) % self.remotes.len();
        &self.remotes[idx]
    }

    fn capabilities(&self) -> &ServerCapabilities {
        &self.remote_info.capabilities
    }

    /// Await a fallible upstream future, converting ServiceError → McpError and
    /// bounding the wait to `self.timeout`.
    async fn forward<F, T>(&self, fut: F) -> Result<T, McpError>
    where
        F: std::future::Future<Output = Result<T, ServiceError>>,
    {
        match tokio::time::timeout(self.timeout, fut).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => Err(to_mcp_error(error)),
            Err(_elapsed) => Err(McpError::internal_error(
                format!("upstream request timed out after {:?}", self.timeout),
                None,
            )),
        }
    }

    /// Same as `forward`, but maps the outcome into `CallToolResult` so the
    /// caller never sees a JSON-RPC protocol error for transport failures
    /// (mirrors the Python proxy's approach).
    async fn forward_tool_call<F>(&self, fut: F) -> CallToolResult
    where
        F: std::future::Future<Output = Result<CallToolResult, ServiceError>>,
    {
        match tokio::time::timeout(self.timeout, fut).await {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => CallToolResult::error(vec![Content::text(error.to_string())]),
            Err(_elapsed) => CallToolResult::error(vec![Content::text(format!(
                "upstream request timed out after {:?}",
                self.timeout
            ))]),
        }
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
        self.forward(self.pick().list_prompts(request)).await
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResult, McpError> {
        if self.capabilities().prompts.is_none() {
            return Err(McpError::method_not_found::<GetPromptRequestMethod>());
        }
        self.forward(self.pick().get_prompt(request)).await
    }

    async fn list_resources(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        if self.capabilities().resources.is_none() {
            return Err(McpError::method_not_found::<ListResourcesRequestMethod>());
        }
        self.forward(self.pick().list_resources(request)).await
    }

    async fn list_resource_templates(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        if self.capabilities().resources.is_none() {
            return Err(McpError::method_not_found::<ListResourceTemplatesRequestMethod>());
        }
        self.forward(self.pick().list_resource_templates(request)).await
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        if self.capabilities().resources.is_none() {
            return Err(McpError::method_not_found::<ReadResourceRequestMethod>());
        }
        self.forward(self.pick().read_resource(request)).await
    }

    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        if self.capabilities().resources.is_none() {
            return Err(McpError::method_not_found::<SubscribeRequestMethod>());
        }
        self.forward(self.pick().subscribe(request)).await
    }

    async fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        if self.capabilities().resources.is_none() {
            return Err(McpError::method_not_found::<UnsubscribeRequestMethod>());
        }
        self.forward(self.pick().unsubscribe(request)).await
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
        self.forward(self.pick().set_level(request)).await
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        if self.capabilities().tools.is_none() {
            return Err(McpError::method_not_found::<ListToolsRequestMethod>());
        }
        self.forward(self.pick().list_tools(request)).await
    }

    /// Forwards a tool call to the remote server. Any failure (tool-level or
    /// transport-level) becomes `Ok(CallToolResult::error(...))` rather than a
    /// JSON-RPC protocol error, except for the capability guard above (an
    /// unroutable request). A configurable timeout bounds the worst case.
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        if self.capabilities().tools.is_none() {
            return Err(McpError::method_not_found::<CallToolRequestMethod>());
        }

        // Bind a single peer for the whole call so progress relay tokens stay
        // consistent (the `send_cancellable_request` + `await_response` pair
        // must go to the same connection).
        let remote = self.pick();

        // The service layer always extracts `_meta` (including any `progressToken`) into
        // `context.meta` during deserialization, so `request.progress_token()` is always
        // `None`. Read from context instead.
        let Some(caller_token) = context.meta.get_progress_token() else {
            return Ok(self.forward_tool_call(remote.call_tool(request)).await);
        };

        // A progress token is present: bypass the convenience method so we can
        // capture the fresh token rmcp mints for the outbound request, and
        // relay progress notifications tagged with it back to the caller's
        // own token via `RemoteNotificationRelay::on_progress`.
        let caller_peer = context.peer.clone();
        let outbound = ClientRequest::CallToolRequest(CallToolRequest::new(request));
        let handle = match tokio::time::timeout(
            self.timeout,
            remote.send_cancellable_request(outbound, PeerRequestOptions::no_options()),
        )
        .await
        {
            Ok(Ok(handle)) => handle,
            Ok(Err(error)) => return Ok(CallToolResult::error(vec![Content::text(error.to_string())])),
            Err(_elapsed) => return Ok(CallToolResult::error(vec![Content::text(format!(
                "upstream send timed out after {:?}",
                self.timeout
            ))])),
        };
        let remote_token = handle.progress_token.clone();
        self.progress_relay
            .lock()
            .unwrap()
            .insert(remote_token.clone(), (caller_peer, caller_token));

        let response = match tokio::time::timeout(self.timeout, handle.await_response()).await {
            Ok(resp) => resp,
            Err(_elapsed) => {
                self.progress_relay.lock().unwrap().remove(&remote_token);
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "upstream response timed out after {:?}",
                    self.timeout
                ))]));
            }
        };
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
        self.forward(self.pick().complete(request)).await
    }

    /// Reverse direction: the downstream caller is reporting progress on some
    /// request we (or the remote) sent to it. Passed straight through with no
    /// token remapping, mirroring Python's unconditional `_send_progress_notification`.
    async fn on_progress(&self, params: ProgressNotificationParam, _context: NotificationContext<RoleServer>) {
        if let Err(error) = self.pick().notify_progress(params).await {
            tracing::warn!(%error, "failed to forward caller progress notification to remote");
        }
    }
}
