//! Loads named stdio MCP server definitions from a JSON config file.
//!
//! Expected shape:
//! ```json
//! { "mcpServers": { "name": { "command": "...", "args": [...], "env": {...}, "enabled": true } } }
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StdioServerConfig {
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub cwd: Option<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config file {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid JSON in config file {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("config file {path} is missing a top-level \"mcpServers\" object")]
    MissingMcpServers { path: PathBuf },
}

/// Load named stdio server configs from a JSON file.
///
/// Per-entry problems (non-dict entry, `enabled: false`, missing `command`,
/// non-list `args`) are skipped with a warning rather than failing the whole
/// file. Only file I/O errors, invalid JSON, or a missing/invalid
/// `mcpServers` key fail the entire load.
pub fn load_named_server_configs_from_file(
    config_file_path: impl AsRef<Path>,
    base_env: &HashMap<String, String>,
) -> Result<HashMap<String, StdioServerConfig>, ConfigError> {
    let path = config_file_path.as_ref();
    let contents = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    let document: serde_json::Value =
        serde_json::from_str(&contents).map_err(|source| ConfigError::Json {
            path: path.to_path_buf(),
            source,
        })?;

    let servers = document
        .get("mcpServers")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| ConfigError::MissingMcpServers {
            path: path.to_path_buf(),
        })?;

    let mut result = HashMap::with_capacity(servers.len());
    for (name, entry) in servers {
        match parse_server_entry(name, entry, base_env) {
            Some(config) => {
                result.insert(name.clone(), config);
            }
            None => continue,
        }
    }
    Ok(result)
}

fn parse_server_entry(
    name: &str,
    entry: &serde_json::Value,
    base_env: &HashMap<String, String>,
) -> Option<StdioServerConfig> {
    let Some(entry) = entry.as_object() else {
        tracing::warn!(server = name, "skipping named server: entry is not an object");
        return None;
    };

    let enabled = entry.get("enabled").and_then(serde_json::Value::as_bool).unwrap_or(true);
    if !enabled {
        tracing::debug!(server = name, "skipping disabled named server");
        return None;
    }

    let Some(command) = entry.get("command").and_then(serde_json::Value::as_str) else {
        tracing::warn!(server = name, "skipping named server: missing or invalid \"command\"");
        return None;
    };

    let args = match entry.get("args") {
        None => Vec::new(),
        Some(serde_json::Value::Array(values)) => {
            let mut args = Vec::with_capacity(values.len());
            for value in values {
                match value.as_str() {
                    Some(s) => args.push(s.to_owned()),
                    None => {
                        tracing::warn!(
                            server = name,
                            "skipping named server: \"args\" contains a non-string value"
                        );
                        return None;
                    }
                }
            }
            args
        }
        Some(_) => {
            tracing::warn!(server = name, "skipping named server: \"args\" is not a list");
            return None;
        }
    };

    let mut env = base_env.clone();
    if let Some(serde_json::Value::Object(entry_env)) = entry.get("env") {
        for (key, value) in entry_env {
            if let Some(value) = value.as_str() {
                env.insert(key.clone(), value.to_owned());
            }
        }
    }

    Some(StdioServerConfig {
        command: command.to_owned(),
        args,
        env,
        cwd: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp_config(contents: &str) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().expect("create temp file");
        file.write_all(contents.as_bytes()).expect("write temp file");
        file
    }

    #[test]
    fn loads_multiple_enabled_servers() {
        let file = write_temp_config(
            r#"{
                "mcpServers": {
                    "fetch": {"command": "uvx", "args": ["mcp-server-fetch"]},
                    "github": {"command": "npx", "args": ["-y", "@modelcontextprotocol/server-github"]}
                }
            }"#,
        );
        let result = load_named_server_configs_from_file(file.path(), &HashMap::new()).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result["fetch"].command, "uvx");
        assert_eq!(result["fetch"].args, vec!["mcp-server-fetch".to_string()]);
        assert_eq!(
            result["github"].args,
            vec!["-y".to_string(), "@modelcontextprotocol/server-github".to_string()]
        );
    }

    #[test]
    fn skips_disabled_server() {
        let file = write_temp_config(
            r#"{"mcpServers": {"disabled": {"command": "x", "enabled": false}, "ok": {"command": "y"}}}"#,
        );
        let result = load_named_server_configs_from_file(file.path(), &HashMap::new()).unwrap();
        assert_eq!(result.len(), 1);
        assert!(result.contains_key("ok"));
    }

    #[test]
    fn enabled_defaults_to_true() {
        let file = write_temp_config(r#"{"mcpServers": {"ok": {"command": "y"}}}"#);
        let result = load_named_server_configs_from_file(file.path(), &HashMap::new()).unwrap();
        assert!(result.contains_key("ok"));
    }

    #[test]
    fn skips_entry_missing_command() {
        let file = write_temp_config(
            r#"{"mcpServers": {"bad": {"args": ["x"]}, "ok": {"command": "y"}}}"#,
        );
        let result = load_named_server_configs_from_file(file.path(), &HashMap::new()).unwrap();
        assert_eq!(result.len(), 1);
        assert!(result.contains_key("ok"));
    }

    #[test]
    fn skips_entry_with_non_list_args() {
        let file = write_temp_config(
            r#"{"mcpServers": {"bad": {"command": "x", "args": "not-a-list"}, "ok": {"command": "y"}}}"#,
        );
        let result = load_named_server_configs_from_file(file.path(), &HashMap::new()).unwrap();
        assert_eq!(result.len(), 1);
        assert!(result.contains_key("ok"));
    }

    #[test]
    fn skips_non_object_entry() {
        let file = write_temp_config(r#"{"mcpServers": {"bad": "not-an-object", "ok": {"command": "y"}}}"#);
        let result = load_named_server_configs_from_file(file.path(), &HashMap::new()).unwrap();
        assert_eq!(result.len(), 1);
        assert!(result.contains_key("ok"));
    }

    #[test]
    fn empty_mcp_servers_yields_empty_map() {
        let file = write_temp_config(r#"{"mcpServers": {}}"#);
        let result = load_named_server_configs_from_file(file.path(), &HashMap::new()).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn missing_mcp_servers_key_is_an_error() {
        let file = write_temp_config(r#"{"somethingElse": {}}"#);
        let err = load_named_server_configs_from_file(file.path(), &HashMap::new()).unwrap_err();
        assert!(matches!(err, ConfigError::MissingMcpServers { .. }));
    }

    #[test]
    fn invalid_json_is_an_error() {
        let file = write_temp_config("not json");
        let err = load_named_server_configs_from_file(file.path(), &HashMap::new()).unwrap_err();
        assert!(matches!(err, ConfigError::Json { .. }));
    }

    #[test]
    fn missing_file_is_an_error() {
        let err =
            load_named_server_configs_from_file("/nonexistent/path/config.json", &HashMap::new())
                .unwrap_err();
        assert!(matches!(err, ConfigError::Io { .. }));
    }

    #[test]
    fn env_merges_base_then_overlays_entry_env() {
        let file = write_temp_config(
            r#"{"mcpServers": {"ok": {"command": "y", "env": {"A": "entry", "B": "entry-only"}}}}"#,
        );
        let mut base_env = HashMap::new();
        base_env.insert("A".to_string(), "base".to_string());
        base_env.insert("C".to_string(), "base-only".to_string());

        let result = load_named_server_configs_from_file(file.path(), &base_env).unwrap();
        let env = &result["ok"].env;
        assert_eq!(env.get("A").map(String::as_str), Some("entry"));
        assert_eq!(env.get("B").map(String::as_str), Some("entry-only"));
        assert_eq!(env.get("C").map(String::as_str), Some("base-only"));
    }

    #[test]
    fn ignores_unknown_fields_like_timeout_and_transport_type() {
        let file = write_temp_config(
            r#"{"mcpServers": {"ok": {"command": "y", "timeout": 60, "transportType": "stdio"}}}"#,
        );
        let result = load_named_server_configs_from_file(file.path(), &HashMap::new()).unwrap();
        assert!(result.contains_key("ok"));
    }
}
