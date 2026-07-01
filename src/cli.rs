//! Command-line argument parsing for mcp-proxy.

use std::collections::HashMap;
use std::path::PathBuf;

use clap::{Parser, ValueEnum};

pub const DEFAULT_EXPOSE_HEADERS: &[&str] = &["mcp-session-id"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[clap(rename_all = "lower")]
pub enum Transport {
    Streamablehttp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[clap(rename_all = "UPPER")]
pub enum LogLevel {
    Debug,
    Info,
    Warning,
    Error,
    Critical,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifySsl {
    On,
    Off,
    CaBundle(PathBuf),
}

#[derive(Parser, Debug)]
#[command(
    name = "mcp-proxy",
    about = "Start the MCP proxy in one of two possible modes: as a client or a server.",
    after_help = "Examples:\n  \
        mcp-proxy http://localhost:8080/mcp\n  \
        mcp-proxy --no-verify-ssl https://server.local/mcp\n  \
        mcp-proxy --headers Authorization 'Bearer YOUR_TOKEN' http://localhost:8080/mcp\n  \
        mcp-proxy --port 8080 -- your-command --arg1 value1 --arg2 value2\n  \
        mcp-proxy --named-server fetch 'uvx mcp-server-fetch' --port 8080\n  \
        mcp-proxy your-command --port 8080 -e KEY VALUE -e ANOTHER_KEY ANOTHER_VALUE\n  \
        mcp-proxy your-command --port 8080 --allow-origin='*'\n"
)]
pub struct Cli {
    /// Command or URL to connect to. When a URL, will run a StreamableHTTP client.
    /// Otherwise, if --named-server is not used, this will be the command for the
    /// default stdio client.
    #[arg(env = "MCP_URL")]
    pub command_or_url: Option<String>,

    /// Headers to pass to the remote server. Can be used multiple times.
    #[arg(short = 'H', long = "headers", num_args = 2, value_names = ["KEY", "VALUE"], action = clap::ArgAction::Append)]
    pub headers: Vec<String>,

    /// The transport to use for the client. Default is streamablehttp.
    #[arg(long, value_enum, default_value = "streamablehttp")]
    pub transport: Transport,

    /// OAuth2 client ID for authentication.
    #[arg(long)]
    pub client_id: Option<String>,

    /// OAuth2 client secret for authentication.
    #[arg(long)]
    pub client_secret: Option<String>,

    /// OAuth2 token URL for authentication.
    #[arg(long)]
    pub token_url: Option<String>,

    /// Control SSL verification when acting as a client. Use without a value to force
    /// verification, pass 'false' to disable, or provide a path to a PEM bundle.
    #[arg(long, num_args = 0..=1, default_missing_value = "true", value_name = "VALUE")]
    pub verify_ssl: Option<String>,

    /// Disable SSL verification (alias for --verify-ssl false).
    #[arg(long)]
    pub no_verify_ssl: bool,

    /// Any extra arguments to the command to spawn the default server.
    pub args: Vec<String>,

    /// Environment variables used when spawning the default server. Can be used
    /// multiple times.
    #[arg(short = 'e', long = "env", num_args = 2, value_names = ["KEY", "VALUE"], action = clap::ArgAction::Append)]
    pub env: Vec<String>,

    /// The working directory to use when spawning the default server process.
    #[arg(long)]
    pub cwd: Option<PathBuf>,

    /// Pass through all environment variables when spawning all server processes.
    #[arg(long, default_value_t = false)]
    pub pass_environment: bool,

    /// Set the log level. Default is INFO.
    #[arg(long, value_enum, default_value = "INFO")]
    pub log_level: LogLevel,

    /// Enable debug mode with detailed logging output. Equivalent to --log-level DEBUG.
    /// If both --debug and --log-level are provided, --debug takes precedence.
    #[arg(long, default_value_t = false)]
    pub debug: bool,

    /// Define a named stdio server: NAME COMMAND_STRING. Can be used multiple times.
    #[arg(long = "named-server", num_args = 2, value_names = ["NAME", "COMMAND_STRING"], action = clap::ArgAction::Append)]
    pub named_server_definitions: Vec<String>,

    /// Path to a JSON configuration file for named stdio servers.
    #[arg(long)]
    pub named_server_config: Option<PathBuf>,

    /// Port to expose the server on. Default is a random port.
    #[arg(long, default_value_t = 0)]
    pub port: u16,

    /// Host to expose the server on. Default is 127.0.0.1.
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,

    /// Enable stateless mode for the Streamable HTTP transport. Default is False.
    #[arg(long, default_value_t = false)]
    pub stateless: bool,

    /// Allowed origins for the server. Can be used multiple times. Default is no CORS allowed.
    #[arg(long = "allow-origin", num_args = 1.., action = clap::ArgAction::Append)]
    pub allow_origin: Vec<String>,

    /// Headers to expose via Access-Control-Expose-Headers. Defaults to 'mcp-session-id'.
    #[arg(long = "expose-header", action = clap::ArgAction::Append)]
    pub expose_headers: Option<Vec<String>>,

    /// Number of independent upstream HTTP connections to open in client mode.
    /// Requests are round-robin distributed across the pool.
    ///
    /// The streamable-HTTP client transport in rmcp processes one outbound POST
    /// at a time per connection (head-of-line blocking). A pool of N connections
    /// allows up to N requests to be in flight concurrently; a slow or stuck
    /// request on one connection only blocks 1/N of the traffic.
    #[arg(long = "upstream-pool-size", default_value_t = 4)]
    pub upstream_pool_size: usize,

    /// Timeout in seconds for a single forwarded upstream request in client mode.
    /// If the upstream server takes longer than this, the call returns an error
    /// instead of blocking indefinitely.
    #[arg(long = "upstream-timeout-secs", default_value_t = 30)]
    pub upstream_timeout_secs: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error(
        "either a command_or_url for a default server or at least one --named-server \
         (or --named-server-config) must be provided for stdio mode"
    )]
    NoServerSpecified,

}

impl Cli {
    /// Cross-field validation that can't be expressed declaratively via clap derive.
    pub fn validate(&self) -> Result<(), CliError> {
        if self.command_or_url.is_none()
            && self.named_server_definitions.is_empty()
            && self.named_server_config.is_none()
        {
            return Err(CliError::NoServerSpecified);
        }
        Ok(())
    }

    pub fn is_client_mode(&self) -> bool {
        matches!(&self.command_or_url, Some(value) if value.starts_with("http://") || value.starts_with("https://"))
    }

    /// --debug takes precedence over --log-level.
    pub fn effective_log_level(&self) -> LogLevel {
        if self.debug {
            LogLevel::Debug
        } else {
            self.log_level
        }
    }

    /// Reconstructs key-value pairs from a flattened repeated-pair argument
    /// (clap's `num_args = 2` flattens `-H A 1 -H B 2` into `[A, 1, B, 2]`).
    fn pairs(flat: &[String]) -> Vec<(String, String)> {
        flat.chunks_exact(2)
            .map(|pair| (pair[0].clone(), pair[1].clone()))
            .collect()
    }

    pub fn header_pairs(&self) -> Vec<(String, String)> {
        Self::pairs(&self.headers)
    }

    pub fn env_pairs(&self) -> Vec<(String, String)> {
        Self::pairs(&self.env)
    }

    pub fn named_server_pairs(&self) -> Vec<(String, String)> {
        Self::pairs(&self.named_server_definitions)
    }

    /// Headers to send to the remote server in client mode, with `$API_ACCESS_TOKEN`
    /// (if set) overriding any explicit `-H Authorization ...` flag, exactly matching
    /// the original Python's dict-update ordering.
    pub fn effective_client_headers(&self) -> HashMap<String, String> {
        let mut headers: HashMap<String, String> = self.header_pairs().into_iter().collect();
        if let Ok(token) = std::env::var("API_ACCESS_TOKEN") {
            headers.insert("Authorization".to_string(), format!("Bearer {token}"));
        }
        headers
    }

    pub fn oauth2_client_credentials(&self) -> Option<(String, String, String)> {
        match (&self.client_id, &self.client_secret, &self.token_url) {
            (Some(id), Some(secret), Some(url)) => {
                Some((id.clone(), secret.clone(), url.clone()))
            }
            _ => None,
        }
    }

    /// Normalizes `--verify-ssl`/`--no-verify-ssl` into a single tri-state value.
    /// `None` means "use the default" (verification on), matching httpx's default.
    pub fn verify_ssl(&self) -> Option<VerifySsl> {
        if self.no_verify_ssl {
            return Some(VerifySsl::Off);
        }
        let raw = self.verify_ssl.as_ref()?;
        let lowered = raw.trim().to_lowercase();
        Some(match lowered.as_str() {
            "1" | "true" | "yes" | "on" => VerifySsl::On,
            "0" | "false" | "no" | "off" => VerifySsl::Off,
            _ => VerifySsl::CaBundle(PathBuf::from(raw)),
        })
    }

    pub fn effective_expose_headers(&self) -> Vec<String> {
        match &self.expose_headers {
            Some(headers) if !headers.is_empty() => headers.clone(),
            _ => DEFAULT_EXPOSE_HEADERS.iter().map(|s| s.to_string()).collect(),
        }
    }

    pub fn effective_allow_origins(&self) -> Option<Vec<String>> {
        if self.allow_origin.is_empty() {
            None
        } else {
            Some(self.allow_origin.clone())
        }
    }

    pub fn effective_bind_host(&self) -> &str {
        &self.host
    }

    pub fn effective_port(&self) -> u16 {
        self.port
    }

    pub fn base_env(&self) -> HashMap<String, String> {
        if self.pass_environment {
            std::env::vars().collect()
        } else {
            HashMap::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        let mut full = vec!["mcp-proxy"];
        full.extend_from_slice(args);
        Cli::parse_from(full)
    }

    #[test]
    fn verify_ssl_unset_is_none() {
        let cli = parse(&["http://localhost:8080/mcp", "--transport", "streamablehttp"]);
        assert_eq!(cli.verify_ssl(), None);
    }

    #[test]
    fn verify_ssl_bare_flag_is_true() {
        // `--verify-ssl` must come last: clap's optional value (num_args = 0..=1)
        // would otherwise greedily swallow the next positional as its value.
        let cli = parse(&[
            "http://localhost:8080/mcp",
            "--transport",
            "streamablehttp",
            "--verify-ssl",
        ]);
        assert_eq!(cli.verify_ssl(), Some(VerifySsl::On));
    }

    #[test]
    fn verify_ssl_false_string() {
        let cli = parse(&[
            "--verify-ssl",
            "false",
            "http://localhost:8080/mcp",
            "--transport",
            "streamablehttp",
        ]);
        assert_eq!(cli.verify_ssl(), Some(VerifySsl::Off));
    }

    #[test]
    fn verify_ssl_case_insensitive() {
        let cli = parse(&[
            "--verify-ssl",
            "OFF",
            "http://localhost:8080/mcp",
            "--transport",
            "streamablehttp",
        ]);
        assert_eq!(cli.verify_ssl(), Some(VerifySsl::Off));
    }

    #[test]
    fn verify_ssl_path_to_ca_bundle() {
        let cli = parse(&[
            "--verify-ssl",
            "/etc/ssl/cert.pem",
            "http://localhost:8080/mcp",
            "--transport",
            "streamablehttp",
        ]);
        assert_eq!(cli.verify_ssl(), Some(VerifySsl::CaBundle(PathBuf::from("/etc/ssl/cert.pem"))));
    }

    #[test]
    fn no_verify_ssl_alias() {
        let cli = parse(&[
            "--no-verify-ssl",
            "http://localhost:8080/mcp",
            "--transport",
            "streamablehttp",
        ]);
        assert_eq!(cli.verify_ssl(), Some(VerifySsl::Off));
    }

    #[test]
    fn expose_header_default_is_mcp_session_id() {
        let cli = parse(&["mycommand"]);
        assert_eq!(cli.effective_expose_headers(), vec!["mcp-session-id".to_string()]);
    }

    #[test]
    fn expose_header_custom_overrides_default() {
        let cli = parse(&["mycommand", "--expose-header", "X-Foo", "--expose-header", "X-Bar"]);
        assert_eq!(
            cli.effective_expose_headers(),
            vec!["X-Foo".to_string(), "X-Bar".to_string()]
        );
    }

    #[test]
    fn header_pairs_reconstructed_from_flat_repeated_args() {
        let cli = parse(&[
            "http://localhost:8080/mcp",
            "--transport",
            "streamablehttp",
            "-H",
            "Authorization",
            "Bearer abc",
            "-H",
            "X-Custom",
            "value",
        ]);
        assert_eq!(
            cli.header_pairs(),
            vec![
                ("Authorization".to_string(), "Bearer abc".to_string()),
                ("X-Custom".to_string(), "value".to_string()),
            ]
        );
    }

    #[test]
    fn named_server_pairs_reconstructed() {
        let cli = parse(&[
            "--named-server",
            "fetch",
            "uvx mcp-server-fetch",
            "--named-server",
            "github",
            "npx server-github",
        ]);
        assert_eq!(
            cli.named_server_pairs(),
            vec![
                ("fetch".to_string(), "uvx mcp-server-fetch".to_string()),
                ("github".to_string(), "npx server-github".to_string()),
            ]
        );
    }

    #[test]
    fn debug_overrides_log_level() {
        let cli = parse(&["mycommand", "--log-level", "WARNING", "--debug"]);
        assert_eq!(cli.effective_log_level(), LogLevel::Debug);
    }

    #[test]
    fn validate_requires_at_least_one_server_source() {
        let cli = parse(&[]);
        assert!(matches!(cli.validate(), Err(CliError::NoServerSpecified)));
    }

    #[test]
    fn validate_passes_with_command_or_url() {
        let cli = parse(&["mycommand"]);
        assert!(cli.validate().is_ok());
    }

    #[test]
    fn validate_passes_with_named_server() {
        let cli = parse(&["--named-server", "fetch", "uvx mcp-server-fetch"]);
        assert!(cli.validate().is_ok());
    }

    #[test]
    fn validate_passes_with_http_url_default_transport() {
        let cli = parse(&["http://localhost:8080/mcp"]);
        assert!(cli.validate().is_ok(), "client mode with default transport should succeed");
    }

    #[test]
    fn is_client_mode_detects_http_url() {
        assert!(parse(&["http://localhost:8080/mcp"]).is_client_mode());
        assert!(parse(&["https://localhost:8080/mcp"]).is_client_mode());
        assert!(!parse(&["mycommand"]).is_client_mode());
    }

    #[test]
    fn oauth2_requires_all_three_fields() {
        let cli = parse(&["mycommand", "--client-id", "id", "--client-secret", "secret"]);
        assert_eq!(cli.oauth2_client_credentials(), None);

        let cli = parse(&[
            "mycommand",
            "--client-id",
            "id",
            "--client-secret",
            "secret",
            "--token-url",
            "https://example.com/token",
        ]);
        assert_eq!(
            cli.oauth2_client_credentials(),
            Some((
                "id".to_string(),
                "secret".to_string(),
                "https://example.com/token".to_string()
            ))
        );
    }

    #[test]
    fn allow_origin_empty_is_none() {
        let cli = parse(&["mycommand"]);
        assert_eq!(cli.effective_allow_origins(), None);
    }

    #[test]
    fn allow_origin_collects_multiple_values() {
        let cli = parse(&["mycommand", "--allow-origin", "https://a.com", "https://b.com"]);
        assert_eq!(
            cli.effective_allow_origins(),
            Some(vec!["https://a.com".to_string(), "https://b.com".to_string()])
        );
    }

    #[test]
    fn extra_args_collected_for_default_server() {
        let cli = parse(&["--", "mycommand", "--arg1", "value1", "--arg2", "value2"]);
        assert_eq!(cli.command_or_url, Some("mycommand".to_string()));
        assert_eq!(
            cli.args,
            vec!["--arg1".to_string(), "value1".to_string(), "--arg2".to_string(), "value2".to_string()]
        );
    }
}
