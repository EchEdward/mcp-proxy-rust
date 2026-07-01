use std::collections::HashMap;
use std::io::IsTerminal;
use std::time::Duration;

use axum::Router;
use axum::serve::{Listener, ListenerExt};
use clap::Parser;
use rmcp::service::RunningService;
use rmcp::transport::{
    TokioChildProcess,
    streamable_http_client::StreamableHttpClientTransportConfig,
    streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService,
        session::local::LocalSessionManager,
        session::never::NeverSessionManager,
    },
    stdio, StreamableHttpClientTransport,
};
use rmcp::{RoleClient, serve_client, serve_server};
use tokio_util::sync::CancellationToken;
use tower_http::cors::{AllowHeaders, AllowMethods, CorsLayer, ExposeHeaders};

use mcp_proxy::{
    cli::{Cli, LogLevel},
    config_loader::{StdioServerConfig, load_named_server_configs_from_file},
    http_client::{build_http_client, fetch_oauth2_client_credentials_token, log_masked_headers},
    proxy_server::{ForwardingProxy, RemoteNotificationRelay, new_progress_relay_map},
};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    setup_tracing(&cli);

    if let Err(e) = cli.validate() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }

    let result = if cli.is_client_mode() {
        run_client_mode(cli).await
    } else {
        run_server_mode(cli).await
    };

    if let Err(e) = result {
        eprintln!("fatal: {e}");
        std::process::exit(1);
    }
}

fn setup_tracing(cli: &Cli) {
    let level = match cli.effective_log_level() {
        LogLevel::Debug => tracing::Level::DEBUG,
        LogLevel::Info => tracing::Level::INFO,
        LogLevel::Warning => tracing::Level::WARN,
        LogLevel::Error | LogLevel::Critical => tracing::Level::ERROR,
    };
    let use_ansi = std::io::stderr().is_terminal();
    tracing_subscriber::fmt()
        .with_max_level(level)
        .with_writer(std::io::stderr)
        .with_ansi(use_ansi)
        .with_target(true)
        .init();
}

// ---------------------------------------------------------------------------
// Client mode: connect to a remote StreamableHTTP MCP server and proxy it
// locally over stdio so that a local MCP host can talk to it.
// ---------------------------------------------------------------------------

async fn run_client_mode(cli: Cli) -> Result<(), BoxError> {
    let url = cli.command_or_url.as_deref().unwrap();
    let mut headers = cli.effective_client_headers();

    if let Some((id, secret, token_url)) = cli.oauth2_client_credentials() {
        let token = fetch_oauth2_client_credentials_token(&id, &secret, &token_url).await?;
        headers.insert("Authorization".to_string(), format!("Bearer {token}"));
    }

    log_masked_headers(&headers);
    let verify_ssl = cli.verify_ssl();
    let pool_size = cli.upstream_pool_size.max(1);
    let timeout = Duration::from_secs(cli.upstream_timeout_secs);

    // Open `pool_size` independent upstream connections.  rmcp's streamable-HTTP
    // client transport processes one outbound POST at a time per connection
    // (head-of-line blocking inside its single-threaded worker loop).  With N
    // connections the proxy can have up to N requests in flight concurrently;
    // a slow/stuck request on one connection only blocks 1/N of the traffic.
    tracing::info!(pool_size, "opening upstream connection pool");
    let relay = new_progress_relay_map();
    let mut runnings = Vec::with_capacity(pool_size);
    for _ in 0..pool_size {
        let http_client = build_http_client(&headers, verify_ssl.as_ref())?;
        let config = StreamableHttpClientTransportConfig::with_uri(url);
        let transport = StreamableHttpClientTransport::with_client(http_client, config);
        let running = serve_client(RemoteNotificationRelay::new(relay.clone()), transport).await?;
        runnings.push(running);
    }

    let peers: Vec<_> = runnings.iter().map(|r| r.peer().clone()).collect();
    let proxy = ForwardingProxy::new(peers, relay, timeout);

    // Keep all `running` instances alive for the duration of the stdio server
    // so the upstream connections are not dropped.
    let (stdin, stdout) = stdio();
    let server = serve_server(proxy, (stdin, stdout)).await?;
    server.waiting().await?;
    drop(runnings);
    Ok(())
}

// ---------------------------------------------------------------------------
// Server mode: spawn local stdio MCP processes and expose them over HTTP.
// ---------------------------------------------------------------------------

/// Holds a connected ForwardingProxy together with the RunningService that
/// keeps the upstream child-process connection alive.
struct ProxyInstance {
    proxy: ForwardingProxy,
    _client: RunningService<RoleClient, RemoteNotificationRelay>,
}

async fn connect_to_stdio_server(config: &StdioServerConfig) -> Result<ProxyInstance, BoxError> {
    let mut command = tokio::process::Command::new(&config.command);
    command.args(&config.args);
    command.envs(&config.env);
    if let Some(cwd) = &config.cwd {
        command.current_dir(cwd);
    }
    let child = TokioChildProcess::new(command)?;
    let relay = new_progress_relay_map();
    let running = serve_client(RemoteNotificationRelay::new(relay.clone()), child).await?;
    // Server mode: single upstream stdio connection is sufficient; the
    // head-of-line blocking issue in rmcp's HTTP client doesn't apply here
    // since the upstream is a child process (not a network HTTP worker).
    let proxy = ForwardingProxy::new(
        vec![running.peer().clone()],
        relay,
        Duration::from_secs(30),
    );
    Ok(ProxyInstance { proxy, _client: running })
}

fn make_mcp_router(proxy: ForwardingProxy, stateless: bool, cancel: CancellationToken) -> Router {
    let config = StreamableHttpServerConfig::default()
        .with_stateful_mode(!stateless)
        .with_cancellation_token(cancel)
        .disable_allowed_hosts();

    if stateless {
        let svc = StreamableHttpService::<_, NeverSessionManager>::new(
            move || Ok(proxy.clone()),
            Default::default(),
            config,
        );
        Router::new().nest_service("/mcp", svc)
    } else {
        let svc = StreamableHttpService::<_, LocalSessionManager>::new(
            move || Ok(proxy.clone()),
            Default::default(),
            config,
        );
        Router::new().nest_service("/mcp", svc)
    }
}

async fn run_server_mode(cli: Cli) -> Result<(), BoxError> {
    let stateless = cli.stateless;
    let host = cli.effective_bind_host().to_owned();
    let port = cli.effective_port();
    let allow_origins = cli.effective_allow_origins();
    let expose_headers = cli.effective_expose_headers();
    let base_env = cli.base_env();

    // Resolve server configurations from CLI and/or config file.
    let default_config = build_default_server_config(&cli, &base_env);
    let named_configs = build_named_server_configs(&cli, &base_env)?;

    // Connect to every upstream server in parallel.
    let cancel = CancellationToken::new();
    let mut router = Router::new();
    let mut instances: Vec<ProxyInstance> = Vec::new();

    if let Some(config) = &default_config {
        tracing::info!(command = %config.command, "connecting to default server");
        let instance = connect_to_stdio_server(config).await?;
        let sub_router = make_mcp_router(instance.proxy.clone(), stateless, cancel.child_token());
        router = router.merge(sub_router);
        instances.push(instance);
    }

    for (name, config) in &named_configs {
        tracing::info!(name, command = %config.command, "connecting to named server");
        let instance = connect_to_stdio_server(config).await?;
        let sub_router = make_mcp_router(instance.proxy.clone(), stateless, cancel.child_token());
        router = router.nest(&format!("/servers/{name}"), sub_router);
        instances.push(instance);
    }

    // Apply CORS if origins are specified.
    if let Some(origins) = allow_origins {
        let allow_origin: Vec<http::HeaderValue> = origins
            .iter()
            .filter_map(|o| o.parse().ok())
            .collect();
        let expose: Vec<http::HeaderName> = expose_headers
            .iter()
            .filter_map(|h| h.parse().ok())
            .collect();
        let cors = CorsLayer::new()
            .allow_origin(allow_origin)
            .allow_methods(AllowMethods::mirror_request())
            .allow_headers(AllowHeaders::mirror_request())
            .expose_headers(ExposeHeaders::list(expose));
        router = router.layer(cors);
    }

    let addr = format!("{host}:{port}");
    // Streamable HTTP responses are written as multiple small chunks (an SSE
    // priming event followed by the actual data event). Without TCP_NODELAY,
    // Nagle's algorithm holds the second chunk back waiting for an ACK of the
    // first, colliding with the peer's delayed-ACK timer for a ~40ms stall on
    // every single request.
    let listener = tokio::net::TcpListener::bind(&addr).await?.tap_io(|tcp_stream| {
        if let Err(err) = tcp_stream.set_nodelay(true) {
            tracing::trace!("failed to set TCP_NODELAY on incoming connection: {err:#}");
        }
    });
    let bound = listener.local_addr()?;

    // Log the MCP endpoints.
    let base_url = format!("http://{bound}");
    tracing::info!("Serving MCP servers via Streamable HTTP:");
    if default_config.is_some() {
        tracing::info!("  {base_url}/mcp");
    }
    for name in named_configs.keys() {
        tracing::info!("  {base_url}/servers/{name}/mcp");
    }

    // Graceful shutdown on Ctrl+C.
    let shutdown = {
        let cancel = cancel.clone();
        async move {
            if let Ok(()) = tokio::signal::ctrl_c().await {
                tracing::info!("Shutting down...");
                cancel.cancel();
            }
        }
    };

    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown)
        .await?;

    drop(instances);
    Ok(())
}

fn build_default_server_config(cli: &Cli, base_env: &HashMap<String, String>) -> Option<StdioServerConfig> {
    let command_or_url = cli.command_or_url.as_deref()?;
    if command_or_url.starts_with("http://") || command_or_url.starts_with("https://") {
        return None;
    }
    let mut env = base_env.clone();
    env.extend(cli.env_pairs());
    Some(StdioServerConfig {
        command: command_or_url.to_owned(),
        args: cli.args.clone(),
        env,
        cwd: cli.cwd.clone(),
    })
}

fn build_named_server_configs(
    cli: &Cli,
    base_env: &HashMap<String, String>,
) -> Result<HashMap<String, StdioServerConfig>, BoxError> {
    // --named-server-config takes priority over --named-server CLI pairs.
    if let Some(path) = &cli.named_server_config {
        let configs = load_named_server_configs_from_file(path, base_env)
            .map_err(|e| format!("failed to load named server config: {e}"))?;
        return Ok(configs);
    }

    let mut result = HashMap::new();
    for (name, command_string) in cli.named_server_pairs() {
        let parts = shell_words::split(&command_string)
            .map_err(|e| format!("invalid command string for --named-server '{name}': {e}"))?;
        if parts.is_empty() {
            return Err(format!("empty command string for --named-server '{name}'").into());
        }
        let env = base_env.clone();
        result.insert(name, StdioServerConfig {
            command: parts[0].clone(),
            args: parts[1..].to_vec(),
            env,
            cwd: None,
        });
    }
    Ok(result)
}
