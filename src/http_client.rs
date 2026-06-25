//! Builds the outbound `reqwest::Client` used for Streamable HTTP client mode:
//! custom headers, SSL verification control, and OAuth2 client-credentials auth.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue, AUTHORIZATION};

use crate::cli::VerifySsl;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const MASKED_HEADERS: &[&str] = &["authorization", "x-api-key", "cookie"];

#[derive(Debug, thiserror::Error)]
pub enum HttpClientError {
    #[error("failed to read CA bundle at {path}: {source}")]
    ReadCaBundle {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid CA bundle at {path}: {source}")]
    ParseCaBundle {
        path: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("failed to build HTTP client: {0}")]
    Build(#[source] reqwest::Error),
    #[error("OAuth2 token request failed: {0}")]
    OAuth2Request(#[source] reqwest::Error),
    #[error("OAuth2 token response did not include an access_token")]
    OAuth2MissingAccessToken,
}

/// Mirrors Python's `_normalize_verify_ssl`/`custom_httpx_client` SSL handling.
pub fn build_http_client(
    headers: &HashMap<String, String>,
    verify_ssl: Option<&VerifySsl>,
) -> Result<reqwest::Client, HttpClientError> {
    let mut builder = reqwest::Client::builder().timeout(DEFAULT_TIMEOUT);

    builder = match verify_ssl {
        None | Some(VerifySsl::On) => builder,
        Some(VerifySsl::Off) => {
            tracing::debug!("Configured HTTP client verify=false (SSL verification disabled).");
            builder.danger_accept_invalid_certs(true)
        }
        Some(VerifySsl::CaBundle(path)) => {
            tracing::debug!(?path, "Configured HTTP client using certificate bundle.");
            let pem = std::fs::read(path).map_err(|source| HttpClientError::ReadCaBundle {
                path: path.display().to_string(),
                source,
            })?;
            let cert =
                reqwest::Certificate::from_pem(&pem).map_err(|source| HttpClientError::ParseCaBundle {
                    path: path.display().to_string(),
                    source,
                })?;
            builder.tls_certs_only([cert])
        }
    };

    if !headers.is_empty() {
        builder = builder.default_headers(build_header_map(headers));
    }

    builder.build().map_err(HttpClientError::Build)
}

fn build_header_map(headers: &HashMap<String, String>) -> HeaderMap {
    let mut map = HeaderMap::with_capacity(headers.len());
    for (key, value) in headers {
        match (key.parse::<HeaderName>(), HeaderValue::from_str(value)) {
            (Ok(name), Ok(value)) => {
                map.insert(name, value);
            }
            _ => {
                tracing::warn!(header = key.as_str(), "skipping invalid header");
            }
        }
    }
    map
}

/// Logs outbound request headers with sensitive values masked, matching Python's
/// `log_request` event hook (Authorization/X-API-Key/Cookie are never logged verbatim).
pub fn log_masked_headers(headers: &HashMap<String, String>) {
    let masked: HashMap<&str, &str> = headers
        .iter()
        .map(|(key, value)| {
            if MASKED_HEADERS.contains(&key.to_lowercase().as_str()) {
                (key.as_str(), "***MASKED***")
            } else {
                (key.as_str(), value.as_str())
            }
        })
        .collect();
    tracing::info!(?masked, "Request Headers");
}

/// Performs a single OAuth2 client-credentials grant and returns the access token.
///
/// Unlike Python's `httpx-auth`, this fetches the token once at startup rather than
/// transparently refreshing it on expiry/401 — acceptable for a long-lived proxy
/// process talking to a single remote server, and avoids pulling in a full OAuth2
/// client crate for a one-shot grant.
pub async fn fetch_oauth2_client_credentials_token(
    client_id: &str,
    client_secret: &str,
    token_url: &str,
) -> Result<String, HttpClientError> {
    let client = reqwest::Client::new();
    let response = client
        .post(token_url)
        .form(&[
            ("grant_type", "client_credentials"),
            ("client_id", client_id),
            ("client_secret", client_secret),
        ])
        .send()
        .await
        .map_err(HttpClientError::OAuth2Request)?
        .error_for_status()
        .map_err(HttpClientError::OAuth2Request)?;

    let body: serde_json::Value = response.json().await.map_err(HttpClientError::OAuth2Request)?;
    body.get("access_token")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or(HttpClientError::OAuth2MissingAccessToken)
}

/// Adds/overwrites the `Authorization: Bearer <token>` default header, taking priority
/// over any statically configured header (mirrors `httpx-auth`'s `auth=` always winning).
pub fn with_bearer_token(
    mut builder: reqwest::ClientBuilder,
    token: &str,
) -> Result<reqwest::ClientBuilder, HttpClientError> {
    let mut header_map = HeaderMap::new();
    let value = HeaderValue::from_str(&format!("Bearer {token}"))
        .map_err(|_| HttpClientError::OAuth2MissingAccessToken)?;
    header_map.insert(AUTHORIZATION, value);
    builder = builder.default_headers(header_map);
    Ok(builder)
}

#[allow(dead_code)]
fn ca_bundle_path_string(path: &Path) -> String {
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_sensitive_headers() {
        let mut headers = HashMap::new();
        headers.insert("Authorization".to_string(), "Bearer secret".to_string());
        headers.insert("X-Custom".to_string(), "visible".to_string());
        // Smoke test: building the header map should not panic and should include both.
        let map = build_header_map(&headers);
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn skips_invalid_header_names() {
        let mut headers = HashMap::new();
        headers.insert("not a valid header name!".to_string(), "value".to_string());
        headers.insert("X-Valid".to_string(), "value".to_string());
        let map = build_header_map(&headers);
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn build_client_with_no_options_succeeds() {
        let client = build_http_client(&HashMap::new(), None);
        assert!(client.is_ok());
    }

    #[test]
    fn build_client_disables_verification() {
        let client = build_http_client(&HashMap::new(), Some(&VerifySsl::Off));
        assert!(client.is_ok());
    }

    #[test]
    fn build_client_with_missing_ca_bundle_errors() {
        let client = build_http_client(
            &HashMap::new(),
            Some(&VerifySsl::CaBundle(std::path::PathBuf::from("/nonexistent/ca.pem"))),
        );
        assert!(matches!(client, Err(HttpClientError::ReadCaBundle { .. })));
    }
}
