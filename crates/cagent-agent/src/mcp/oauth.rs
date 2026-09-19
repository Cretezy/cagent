use async_trait::async_trait;
use rmcp::transport::auth::{
    AuthError, AuthorizationManager, AuthorizationMetadata, AuthorizationRequest,
    AuthorizationSession, CredentialStore, OAuthTokenResponse, StoredCredentials,
};
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::{McpConfiguredValue, McpOAuthConfig, McpSecretStatus, McpSecretStore};
use crate::RuntimeError;

#[derive(Clone)]
struct ManagedOAuthStore {
    store: McpSecretStore,
    reference: String,
}

#[async_trait]
impl CredentialStore for ManagedOAuthStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        self.store
            .get(&self.reference)
            .map_err(store_error)?
            .map(|value| serde_json::from_str(&value).map_err(store_error))
            .transpose()
    }

    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        let value = serde_json::to_string(&credentials).map_err(store_error)?;
        self.store.set(&self.reference, &value).map_err(store_error)
    }

    async fn clear(&self) -> Result<(), AuthError> {
        self.store.delete(&self.reference).map_err(store_error)
    }
}

fn store_error(error: impl std::fmt::Display) -> AuthError {
    AuthError::InternalError(format!(
        "managed MCP OAuth credential storage failed: {error}"
    ))
}

fn credential_reference(resource: &str) -> String {
    format!("mcp/oauth/{:x}", Sha256::digest(resource.as_bytes()))
}

/// Frontend-safe instructions for a pending MCP OAuth flow.
#[derive(Clone, Debug)]
pub enum McpOAuthPrompt {
    Browser {
        authorization_url: String,
        callback_url: String,
    },
    Device {
        verification_url: String,
        user_code: String,
    },
}

#[derive(Clone, Debug)]
pub enum McpOAuthCompletion {
    Waiting,
    Connected,
    Failed(String),
}

pub struct McpOAuthAttempt {
    pub prompt: McpOAuthPrompt,
    pub completion: tokio::sync::watch::Receiver<McpOAuthCompletion>,
}

pub struct McpOAuthFlow {
    pub prompt: McpOAuthPrompt,
    inner: McpOAuthFlowInner,
}

#[allow(clippy::large_enum_variant)] // Browser and device OAuth retain distinct state without indirection.
enum McpOAuthFlowInner {
    Browser(McpBrowserOAuthFlow),
    Device(McpDeviceOAuthFlow),
}

struct McpBrowserOAuthFlow {
    listener: tokio::net::TcpListener,
    session: AuthorizationSession,
}

pub struct McpDeviceOAuthFlow {
    client_id: String,
    device_code: String,
    token_endpoint: String,
    interval: std::time::Duration,
    expires_in: std::time::Duration,
    scopes: Vec<String>,
    issuer: Option<String>,
    store: ManagedOAuthStore,
}

impl McpDeviceOAuthFlow {
    pub async fn complete(self) -> Result<(), RuntimeError> {
        let client = reqwest::Client::new();
        let deadline = tokio::time::Instant::now() + self.expires_in;
        let mut interval = self.interval;
        loop {
            if tokio::time::Instant::now() >= deadline {
                return Err(RuntimeError::InvalidOption(
                    "MCP device authorization expired".into(),
                ));
            }
            tokio::time::sleep(interval).await;
            let response = client
                .post(&self.token_endpoint)
                .header(reqwest::header::ACCEPT, "application/json")
                .form(&[
                    ("client_id", self.client_id.as_str()),
                    ("device_code", self.device_code.as_str()),
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ])
                .send()
                .await
                .map_err(oauth_error)?;
            let status = response.status();
            let body = response.bytes().await.map_err(oauth_error)?;
            let value: serde_json::Value = serde_json::from_slice(&body).map_err(oauth_error)?;
            if status.is_success() && value.get("access_token").is_some() {
                let token: OAuthTokenResponse =
                    serde_json::from_value(value).map_err(oauth_error)?;
                let credentials = StoredCredentials::new(
                    self.client_id,
                    Some(token),
                    self.scopes,
                    Some(epoch_seconds()),
                )
                .with_issuer(self.issuer);
                self.store.save(credentials).await.map_err(oauth_error)?;
                return Ok(());
            }
            match value.get("error").and_then(serde_json::Value::as_str) {
                Some("authorization_pending") => {}
                Some("slow_down") => interval += std::time::Duration::from_secs(5),
                Some("expired_token") => {
                    return Err(RuntimeError::InvalidOption(
                        "MCP device authorization expired".into(),
                    ));
                }
                Some("access_denied") => {
                    return Err(RuntimeError::InvalidOption(
                        "MCP device authorization was denied".into(),
                    ));
                }
                Some(error) => {
                    return Err(RuntimeError::InvalidOption(format!(
                        "MCP device authorization failed: {error}"
                    )));
                }
                None => {
                    return Err(RuntimeError::InvalidOption(format!(
                        "MCP device authorization failed with HTTP {status}"
                    )));
                }
            }
        }
    }
}

#[derive(serde::Deserialize)]
struct DeviceAuthorizationResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    expires_in: u64,
    #[serde(default = "default_device_interval")]
    interval: u64,
}

const fn default_device_interval() -> u64 {
    5
}

fn epoch_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl McpBrowserOAuthFlow {
    /// Waits up to five minutes for the one-time loopback callback, validates
    /// state and PKCE through rmcp, and persists the resulting credentials.
    pub async fn complete(self) -> Result<(), RuntimeError> {
        tokio::time::timeout(std::time::Duration::from_secs(5 * 60), async move {
            loop {
                let (mut stream, _) = self.listener.accept().await.map_err(oauth_error)?;
                let mut request = Vec::with_capacity(1024);
                while request.len() <= 8 * 1024 {
                    let mut buffer = [0_u8; 1024];
                    let read = stream.read(&mut buffer).await.map_err(oauth_error)?;
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let target = std::str::from_utf8(&request)
                    .ok()
                    .and_then(|request| request.lines().next())
                    .and_then(|line| line.split_whitespace().nth(1))
                    .ok_or_else(|| {
                        RuntimeError::InvalidOption("invalid MCP OAuth callback".into())
                    })?;
                let callback = format!("http://localhost{target}");
                let path = url::Url::parse(&callback).map_err(oauth_error)?;
                if path.path() != "/auth/callback" {
                    write_response(&mut stream, "404 Not Found", "Invalid callback").await;
                    continue;
                }
                let result = self
                    .session
                    .handle_callback_url(&callback)
                    .await
                    .map(|_| ())
                    .map_err(oauth_error);
                let (status, message) = if result.is_ok() {
                    (
                        "200 OK",
                        "MCP authentication completed. You can close this page.",
                    )
                } else {
                    ("400 Bad Request", oauth_browser_error(&result))
                };
                write_response(&mut stream, status, message).await;
                return result;
            }
        })
        .await
        .map_err(|_| RuntimeError::InvalidOption("MCP OAuth callback timed out".into()))?
    }
}

impl McpOAuthFlow {
    pub async fn complete(self) -> Result<(), RuntimeError> {
        match self.inner {
            McpOAuthFlowInner::Browser(flow) => flow.complete().await,
            McpOAuthFlowInner::Device(flow) => flow.complete().await,
        }
    }
}

fn oauth_browser_error(result: &Result<(), RuntimeError>) -> &'static str {
    let message = result
        .as_ref()
        .err()
        .map(ToString::to_string)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if message.contains("token exchange")
        || message.contains("invalid_client")
        || message.contains("client secret")
    {
        "MCP authentication failed during token exchange. Check the OAuth client ID, client secret, and callback URL, then return to Cagent."
    } else if message.contains("state") || message.contains("csrf") {
        "MCP authentication expired or the callback state did not match. Start Login with OAuth again in Cagent."
    } else {
        "MCP authentication failed. Check the MCP OAuth configuration and return to Cagent."
    }
}

async fn write_response(stream: &mut tokio::net::TcpStream, status: &str, message: &str) {
    let body = format!("<!doctype html><title>Cagent</title><p>{message}</p>");
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
}

/// Starts standards-based MCP browser authorization using RFC 9728/RFC 8414
/// discovery, S256 PKCE, resource indicators, and DCR where advertised.
/// Static endpoint metadata and a pre-registered client can be configured for
/// authorization servers that do not publish the required discovery document.
pub async fn begin_oauth(
    resource: &str,
    config: &McpOAuthConfig,
    store: McpSecretStore,
) -> Result<McpOAuthFlow, RuntimeError> {
    super::validate_http_url(resource, false).map_err(RuntimeError::InvalidOption)?;
    if config.device_authorization_endpoint.is_some() {
        return begin_device_oauth(resource, config, store).await;
    }
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(oauth_error)?;
    let callback_url = format!(
        "http://127.0.0.1:{}/auth/callback",
        listener.local_addr().map_err(oauth_error)?.port()
    );
    let mut manager = AuthorizationManager::new(resource)
        .await
        .map_err(oauth_error)?;
    manager.set_credential_store(ManagedOAuthStore {
        store: store.clone(),
        reference: credential_reference(resource),
    });
    if let (Some(issuer), Some(authorization_endpoint), Some(token_endpoint)) = (
        config.issuer.clone(),
        config.authorization_endpoint.clone(),
        config.token_endpoint.clone(),
    ) {
        manager.set_metadata(static_metadata(
            issuer,
            authorization_endpoint,
            token_endpoint,
        ));
    } else {
        let metadata = manager
            .resolve_metadata()
            .await
            .map_err(oauth_error)?
            .metadata;
        manager.set_metadata(metadata);
    }
    let mut request = AuthorizationRequest::new(&callback_url)
        .with_scopes(config.scopes.clone())
        .with_client_name("Cagent");
    if let Some(client_id) = &config.client_id {
        request = request.with_preregistered_client(resolve_value(client_id, &store)?);
    }
    if let Some(client_secret) = &config.client_secret {
        request = request.with_client_secret(resolve_value(client_secret, &store)?);
    }
    let session = AuthorizationSession::new(manager, request)
        .await
        .map_err(|(_, error)| oauth_error(error))?;
    let authorization_url = session.get_authorization_url().to_owned();
    Ok(McpOAuthFlow {
        prompt: McpOAuthPrompt::Browser {
            authorization_url,
            callback_url,
        },
        inner: McpOAuthFlowInner::Browser(McpBrowserOAuthFlow { listener, session }),
    })
}

async fn begin_device_oauth(
    resource: &str,
    config: &McpOAuthConfig,
    store: McpSecretStore,
) -> Result<McpOAuthFlow, RuntimeError> {
    let endpoint = config
        .device_authorization_endpoint
        .as_deref()
        .ok_or_else(|| {
            RuntimeError::InvalidOption("missing device authorization endpoint".into())
        })?;
    super::validate_http_url(endpoint, false).map_err(RuntimeError::InvalidOption)?;
    let token_endpoint = config.token_endpoint.clone().ok_or_else(|| {
        RuntimeError::InvalidOption("device OAuth requires oauth.token_endpoint".into())
    })?;
    super::validate_http_url(&token_endpoint, false).map_err(RuntimeError::InvalidOption)?;
    let client_id = config
        .client_id
        .as_ref()
        .ok_or_else(|| RuntimeError::InvalidOption("device OAuth requires oauth.client_id".into()))
        .and_then(|value| resolve_value(value, &store))?;
    let scope = config.scopes.join(" ");
    let response = reqwest::Client::new()
        .post(endpoint)
        .header(reqwest::header::ACCEPT, "application/json")
        .form(&[("client_id", client_id.as_str()), ("scope", scope.as_str())])
        .send()
        .await
        .map_err(oauth_error)?;
    let status = response.status();
    let body = response.bytes().await.map_err(oauth_error)?;
    if !status.is_success() {
        return Err(RuntimeError::InvalidOption(format!(
            "MCP device authorization request failed with HTTP {status}"
        )));
    }
    let value: serde_json::Value = serde_json::from_slice(&body).map_err(oauth_error)?;
    if let Some(error) = value.get("error").and_then(serde_json::Value::as_str) {
        let description = value
            .get("error_description")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(error);
        return Err(RuntimeError::InvalidOption(format!(
            "MCP device authorization is unavailable: {description}"
        )));
    }
    let response: DeviceAuthorizationResponse =
        serde_json::from_value(value).map_err(oauth_error)?;
    let verification_url = response
        .verification_uri_complete
        .unwrap_or(response.verification_uri);
    let user_code = response.user_code;
    Ok(McpOAuthFlow {
        prompt: McpOAuthPrompt::Device {
            verification_url: verification_url.clone(),
            user_code: user_code.clone(),
        },
        inner: McpOAuthFlowInner::Device(McpDeviceOAuthFlow {
            client_id,
            device_code: response.device_code,
            token_endpoint,
            interval: std::time::Duration::from_secs(response.interval.max(1)),
            expires_in: std::time::Duration::from_secs(response.expires_in),
            scopes: config.scopes.clone(),
            issuer: config.issuer.clone(),
            store: ManagedOAuthStore {
                store,
                reference: credential_reference(resource),
            },
        }),
    })
}

fn resolve_value(
    value: &McpConfiguredValue,
    store: &McpSecretStore,
) -> Result<String, RuntimeError> {
    let value = value.value();
    if let Some(name) = value
        .strip_prefix("${env:")
        .and_then(|value| value.strip_suffix('}'))
    {
        return std::env::var(name).map_err(|_| {
            RuntimeError::InvalidOption(format!("missing MCP OAuth environment variable: {name}"))
        });
    }
    if let Some(reference) = value
        .strip_prefix("${secret:")
        .and_then(|value| value.strip_suffix('}'))
    {
        super::secrets::referenced_secrets(&format!("${{secret:{reference}}}"))?;
        return store.get(reference)?.ok_or_else(|| {
            RuntimeError::InvalidOption(format!("missing managed MCP OAuth secret: {reference}"))
        });
    }
    Ok(value.to_owned())
}

pub fn oauth_status(
    resource: &str,
    store: &McpSecretStore,
) -> Result<McpSecretStatus, RuntimeError> {
    store.status(&credential_reference(resource))
}

pub fn clear_oauth(resource: &str, store: &McpSecretStore) -> Result<(), RuntimeError> {
    store.delete(&credential_reference(resource))
}

pub(crate) async fn authorized_client(
    resource: &str,
    config: &McpOAuthConfig,
    store: McpSecretStore,
) -> Result<Option<rmcp::transport::AuthClient<reqwest::Client>>, RuntimeError> {
    if oauth_status(resource, &store)? == McpSecretStatus::Missing {
        return Ok(None);
    }
    let mut manager = AuthorizationManager::new(resource)
        .await
        .map_err(oauth_error)?;
    manager.set_credential_store(ManagedOAuthStore {
        store,
        reference: credential_reference(resource),
    });
    if let (Some(issuer), Some(authorization_endpoint), Some(token_endpoint)) = (
        config.issuer.clone(),
        config.authorization_endpoint.clone(),
        config.token_endpoint.clone(),
    ) {
        manager.set_metadata(static_metadata(
            issuer,
            authorization_endpoint,
            token_endpoint,
        ));
    }
    if !manager.initialize_from_store().await.map_err(oauth_error)? {
        return Ok(None);
    }
    Ok(Some(rmcp::transport::AuthClient::new(
        reqwest::Client::new(),
        manager,
    )))
}

fn oauth_error(error: impl std::fmt::Display) -> RuntimeError {
    RuntimeError::InvalidOption(format!("MCP OAuth failed: {error}"))
}

fn static_metadata(
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
) -> AuthorizationMetadata {
    let mut metadata = AuthorizationMetadata::default();
    metadata.issuer = Some(issuer);
    metadata.authorization_endpoint = authorization_endpoint;
    metadata.token_endpoint = token_endpoint;
    metadata.code_challenge_methods_supported = Some(vec!["S256".into()]);
    metadata
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn device_flow_returns_code_and_persists_token_without_client_secret() {
        let router = axum::Router::new()
            .route(
                "/device/code",
                axum::routing::post(|| async {
                    axum::Json(serde_json::json!({
                        "device_code": "device-code",
                        "user_code": "ABCD-EFGH",
                        "verification_uri": "https://github.com/login/device",
                        "expires_in": 30,
                        "interval": 1
                    }))
                }),
            )
            .route(
                "/token",
                axum::routing::post(|| async {
                    axum::Json(serde_json::json!({
                        "access_token": "fixture-token",
                        "token_type": "bearer",
                        "scope": "repo read:user"
                    }))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let resource = format!("http://{address}/mcp");
        let store_dir = tempfile::tempdir().unwrap();
        let store = McpSecretStore::new(store_dir.path());
        let config = McpOAuthConfig {
            client_id: Some("public-client".into()),
            scopes: vec!["repo".into(), "read:user".into()],
            device_authorization_endpoint: Some(format!("http://{address}/device/code")),
            issuer: Some(format!("http://{address}")),
            authorization_endpoint: Some(format!("http://{address}/authorize")),
            token_endpoint: Some(format!("http://{address}/token")),
            ..McpOAuthConfig::default()
        };

        let flow = begin_oauth(&resource, &config, store.clone())
            .await
            .unwrap();
        assert!(matches!(
            &flow.prompt,
            McpOAuthPrompt::Device {
                verification_url,
                user_code
            } if verification_url == "https://github.com/login/device" && user_code == "ABCD-EFGH"
        ));
        flow.complete().await.unwrap();
        assert_eq!(
            oauth_status(&resource, &store).unwrap(),
            McpSecretStatus::Configured
        );
        let client = authorized_client(&resource, &config, store.clone())
            .await
            .unwrap()
            .expect("saved Device Flow credentials should initialize an OAuth client");
        assert_eq!(client.get_access_token().await.unwrap(), "fixture-token");
        clear_oauth(&resource, &store).unwrap();
        server.abort();
    }
}
