#![allow(clippy::result_large_err)] // The compatibility adapter exposes detailed provider errors to its callers.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::Once;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use futures_util::{Sink, SinkExt, StreamExt};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tokio::sync::{Mutex as AsyncMutex, RwLock, mpsc};
use tokio_stream::wrappers::ReceiverStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{Error as WebSocketError, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{
    AuthChallenge, AuthFlow, AuthResponse, AuthState, CredentialSource, CredentialStore,
    FinishReason, ManagedCredential, ModelBackend, ModelCatalog, ModelDiscoverySource,
    ModelRequest, ModelUsage, Provider, ProviderDescriptor, ProviderError, ProviderErrorKind,
    ProviderFuture, ProviderStream, ProviderStreamEvent, ProviderUsageReport, ProviderUsageWindow,
    ProviderWebSearchRequest, ProviderWebSearchResponse, ResponseMetadata,
};

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const AUTH_BASE_URL: &str = "https://auth.openai.com";
const CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
const CALLBACK_URL: &str = "http://localhost:1455/auth/callback";
const CHATGPT_SEARCH_ORIGINATOR: &str = "chatgpt_cca";
const CHATGPT_SEARCH_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_SEARCH_RESPONSE_BYTES: usize = 128 * 1024;
const OAUTH_SCOPE: &str =
    "openid profile email offline_access api.connectors.read api.connectors.invoke";
// The Codex backend filters its catalog by Codex protocol compatibility, not
// by the version of the third-party client making the request. Keep this next
// to the adapter so backend contract changes remain isolated.
const CODEX_CLIENT_VERSION: &str = "0.156.1";

#[derive(Clone, Debug)]
struct PendingBrowserAuth {
    state: String,
    verifier: String,
    callback_url: String,
}

#[derive(Clone, Debug)]
struct PendingDeviceAuth {
    device_code: String,
    user_code: String,
    interval_seconds: u64,
    cancellation: CancellationToken,
}

/// Compatibility-sensitive ChatGPT/Codex subscription adapter.
#[derive(Clone)]
pub struct ChatGptProvider {
    client: reqwest::Client,
    auth_base_url: String,
    codex_base_url: String,
    credentials: CredentialStore,
    credential: Arc<RwLock<Option<ManagedCredential>>>,
    pending_browser: Arc<Mutex<Option<PendingBrowserAuth>>>,
    pending_device: Arc<Mutex<Option<PendingDeviceAuth>>>,
    refresh_lock: Arc<tokio::sync::Mutex<()>>,
    websocket_lanes: Arc<AsyncMutex<HashMap<String, Arc<AsyncMutex<WebSocketLane>>>>>,
    responses_lite_models: Arc<RwLock<HashSet<String>>>,
    websocket_connect_timeout: Duration,
}

impl std::fmt::Debug for ChatGptProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChatGptProvider")
            .field("auth_base_url", &self.auth_base_url)
            .field("codex_base_url", &self.codex_base_url)
            .finish_non_exhaustive()
    }
}

impl ChatGptProvider {
    #[must_use]
    pub fn new(data_dir: &Path) -> Self {
        install_rustls_provider();
        let credentials = CredentialStore::new(data_dir, "default", "chatgpt");
        let credential = credentials.load().ok().flatten();
        Self {
            client: reqwest::Client::new(),
            auth_base_url: AUTH_BASE_URL.into(),
            codex_base_url: CODEX_BASE_URL.into(),
            credentials,
            credential: Arc::new(RwLock::new(credential)),
            pending_browser: Arc::new(Mutex::new(None)),
            pending_device: Arc::new(Mutex::new(None)),
            refresh_lock: Arc::new(tokio::sync::Mutex::new(())),
            websocket_lanes: Arc::new(AsyncMutex::new(HashMap::new())),
            responses_lite_models: Arc::new(RwLock::new(HashSet::new())),
            websocket_connect_timeout: WEBSOCKET_CONNECT_TIMEOUT,
        }
    }

    #[cfg(test)]
    fn with_endpoints(data_dir: &Path, base_url: &str) -> Self {
        let mut provider = Self::new(data_dir);
        provider.auth_base_url = base_url.trim_end_matches('/').into();
        provider.codex_base_url = format!("{}/codex", base_url.trim_end_matches('/'));
        provider
    }

    #[cfg(test)]
    fn with_websocket_connect_timeout(mut self, timeout: Duration) -> Self {
        self.websocket_connect_timeout = timeout;
        self
    }

    async fn usable_credential(&self) -> Result<ManagedCredential, ProviderError> {
        let credential = self.credential.read().await.clone().ok_or_else(|| {
            auth_error(
                "missing_subscription",
                "ChatGPT subscription is not connected",
            )
        })?;
        if !credential.has_codex_entitlement {
            return Err(auth_error(
                "missing_entitlement",
                "ChatGPT account does not have Codex entitlement",
            ));
        }
        if credential
            .expires_at_millis
            .is_some_and(|expiry| expiry <= now_millis().saturating_add(30_000))
        {
            self.refresh_credentials(CancellationToken::new()).await?;
            return self.credential.read().await.clone().ok_or_else(|| {
                auth_error(
                    "missing_subscription",
                    "ChatGPT subscription is not connected",
                )
            });
        }
        Ok(credential)
    }

    async fn exchange(&self, body: &[(&str, &str)]) -> Result<ManagedCredential, ProviderError> {
        let response = self
            .client
            .post(format!("{}/oauth/token", self.auth_base_url))
            .form(body)
            .send()
            .await
            .map_err(transport_error)?;
        if !response.status().is_success() {
            return Err(
                crate::provider::codecs::openai_responses::decode_http_error(response).await,
            );
        }
        let payload = decode_json_response(response, "invalid_oauth_response").await?;
        credential_from_token_response(&payload)
    }

    fn credential_is_web_search_ready(credential: &ManagedCredential) -> bool {
        credential.has_codex_entitlement
            && (credential
                .expires_at_millis
                .is_none_or(|expiry| expiry > now_millis())
                || credential.refresh_token.is_some())
    }

    /// Returns whether the persisted subscription can authenticate Codex web
    /// search. Expired access tokens remain ready when a refresh token exists;
    /// the request path refreshes them before use.
    pub(crate) fn web_search_ready(&self) -> bool {
        self.credential
            .try_read()
            .ok()
            .and_then(|credential| credential.clone())
            .or_else(|| self.credentials.load().ok().flatten())
            .is_some_and(|credential| Self::credential_is_web_search_ready(&credential))
    }

    async fn search(
        &self,
        request: ProviderWebSearchRequest,
        cancellation: CancellationToken,
    ) -> Result<ProviderWebSearchResponse, ProviderError> {
        let credential = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(ProviderError::cancelled()),
            () = tokio::time::sleep(CHATGPT_SEARCH_TIMEOUT) => {
                return Err(ProviderError::timeout(
                    "web_search_timeout",
                    "ChatGPT web search timed out",
                ));
            }
            credential = self.usable_credential() => credential?,
        };
        let search_context = json!({
            "telemetry_attributes": {"model_id": request.model.clone()}
        })
        .to_string();
        let turn_metadata = json!({
            "mcp_request_meta": json!({
                "openai/search_context": search_context
            })
            .to_string()
        })
        .to_string();
        let body = json!({
            "id": request.id,
            "model": request.model,
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "Search the web"}]
            }],
            "commands": {
                "search_query": [{"q": request.query}]
            },
            "settings": {
                "allowed_callers": ["direct"],
                "external_web_access": true
            },
            "max_output_tokens": 2048
        });
        let response = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(ProviderError::cancelled()),
            () = tokio::time::sleep(CHATGPT_SEARCH_TIMEOUT) => {
                return Err(ProviderError::timeout(
                    "web_search_timeout",
                    "ChatGPT web search timed out",
                ));
            }
            response = self
                .client
                .post(format!("{}/alpha/search", self.codex_base_url))
                .header(AUTHORIZATION, format!("Bearer {}", credential.access_token))
                .header("ChatGPT-Account-Id", credential.account_id)
                .header("originator", CHATGPT_SEARCH_ORIGINATOR)
                .header("x-codex-turn-metadata", turn_metadata)
                .header(CONTENT_TYPE, "application/json")
                .json(&body)
                .send() => response.map_err(transport_error)?,
        };
        if !response.status().is_success() {
            return Err(
                crate::provider::codecs::openai_responses::decode_http_error(response).await,
            );
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_SEARCH_RESPONSE_BYTES as u64)
        {
            return Err(ProviderError::protocol(
                "web_search_response_too_large",
                "ChatGPT web search response exceeded the output limit",
            ));
        }
        let mut bytes = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(ProviderError::cancelled()),
            () = tokio::time::sleep(CHATGPT_SEARCH_TIMEOUT) => {
                return Err(ProviderError::timeout(
                    "web_search_timeout",
                    "ChatGPT web search timed out",
                ));
            }
            bytes = response.bytes() => bytes.map(|bytes| bytes.to_vec()).map_err(|error| {
                ProviderError::protocol("invalid_web_search_response", error.to_string())
            })?,
        };
        if bytes.len() > MAX_SEARCH_RESPONSE_BYTES {
            return Err(ProviderError::protocol(
                "web_search_response_too_large",
                "ChatGPT web search response exceeded the output limit",
            ));
        }
        let payload: Value = simd_json::serde::from_slice(&mut bytes).map_err(|error| {
            ProviderError::protocol("invalid_web_search_response", error.to_string())
        })?;
        let results = match payload.get("results") {
            None | Some(Value::Null) => Vec::new(),
            Some(value) => value.as_array().cloned().ok_or_else(|| {
                ProviderError::protocol(
                    "invalid_web_search_response",
                    "ChatGPT web search response contained invalid results",
                )
            })?,
        };
        Ok(ProviderWebSearchResponse { results })
    }

    async fn save_credential(&self, credential: ManagedCredential) -> Result<(), ProviderError> {
        self.credentials.save(&credential)?;
        *self.credential.write().await = Some(credential);
        Ok(())
    }

    fn begin_browser_auth(&self, callback_url: String) -> Result<AuthChallenge, ProviderError> {
        let state = uuid::Uuid::now_v7().simple().to_string();
        let verifier = format!(
            "{}{}",
            uuid::Uuid::now_v7().simple(),
            uuid::Uuid::now_v7().simple()
        );
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes()));
        let mut url = reqwest::Url::parse(&format!("{}/oauth/authorize", self.auth_base_url))
            .map_err(|error| ProviderError::configuration(error.to_string()))?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", CLIENT_ID)
            .append_pair("redirect_uri", &callback_url)
            .append_pair("scope", OAUTH_SCOPE)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("id_token_add_organizations", "true")
            .append_pair("codex_cli_simplified_flow", "true")
            .append_pair("state", &state)
            .append_pair("originator", "cagent");
        *self
            .pending_browser
            .lock()
            .map_err(|_| ProviderError::configuration("authentication state lock failed"))? =
            Some(PendingBrowserAuth {
                state: state.clone(),
                verifier,
                callback_url: callback_url.clone(),
            });
        Ok(AuthChallenge::Browser {
            authorization_url: url.into(),
            state,
            callback_url,
        })
    }
}

impl Provider for ChatGptProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        static DESCRIPTOR: std::sync::LazyLock<ProviderDescriptor> =
            std::sync::LazyLock::new(|| ProviderDescriptor {
                id: "chatgpt".into(),
                display_name: "ChatGPT subscription".into(),
                default_model_backend: Some(ModelBackend::OpenAiResponses),
                supported_model_backends: vec![ModelBackend::OpenAiResponses],
                model_discovery: ModelDiscoverySource::ProviderApi,
                credential_source: CredentialSource::Subscription,
                credential_environment_variable: None,
                supports_managed_api_key: false,
                auth_flows: vec![AuthFlow::BrowserPkce, AuthFlow::DeviceCode],
            });
        &DESCRIPTOR
    }

    fn auth_state(&self) -> ProviderFuture<'_, Result<AuthState, ProviderError>> {
        Box::pin(async move {
            Ok(match self.credential.read().await.as_ref() {
                None => AuthState::Missing,
                Some(credential) if !credential.has_codex_entitlement => AuthState::Error {
                    message: "connected account has no Codex entitlement".into(),
                },
                Some(credential)
                    if credential
                        .expires_at_millis
                        .is_some_and(|expiry| expiry <= now_millis()) =>
                {
                    AuthState::Expired {
                        detail: format!("account {}", credential.account_id),
                    }
                }
                Some(credential) => AuthState::Connected {
                    detail: format!("account {}", credential.account_id),
                },
            })
        })
    }

    fn provider_usage(
        &self,
    ) -> Option<ProviderFuture<'_, Result<ProviderUsageReport, ProviderError>>> {
        Some(Box::pin(async move {
            let credential = self.usable_credential().await?;
            let response = self
                .client
                .get(format!("{}/usage", self.codex_base_url))
                .header(AUTHORIZATION, format!("Bearer {}", credential.access_token))
                .header("ChatGPT-Account-Id", credential.account_id)
                .header("originator", "cagent")
                .send()
                .await
                .map_err(transport_error)?;
            if !response.status().is_success() {
                return Err(
                    crate::provider::codecs::openai_responses::decode_http_error(response).await,
                );
            }
            let payload = decode_json_response(response, "invalid_usage_response").await?;
            Ok(decode_usage_report(&payload))
        }))
    }

    fn web_search_ready(&self) -> bool {
        Self::web_search_ready(self)
    }

    fn web_search(
        &self,
        request: ProviderWebSearchRequest,
        cancellation: CancellationToken,
    ) -> ProviderFuture<'_, Result<ProviderWebSearchResponse, ProviderError>> {
        Box::pin(self.search(request, cancellation))
    }

    fn begin_auth(
        &self,
        flow: AuthFlow,
    ) -> ProviderFuture<'_, Result<AuthChallenge, ProviderError>> {
        Box::pin(async move {
            match flow {
                AuthFlow::BrowserPkce => {
                    if let Some(pending) = self
                        .pending_device
                        .lock()
                        .map_err(|_| {
                            ProviderError::configuration("authentication state lock failed")
                        })?
                        .take()
                    {
                        pending.cancellation.cancel();
                    }
                    self.begin_browser_auth(CALLBACK_URL.to_owned())
                }
                AuthFlow::DeviceCode => {
                    let response = self
                        .client
                        .post(format!(
                            "{}/api/accounts/deviceauth/usercode",
                            self.auth_base_url
                        ))
                        .json(&json!({ "client_id": CLIENT_ID }))
                        .send()
                        .await
                        .map_err(transport_error)?;
                    if !response.status().is_success() {
                        return Err(
                            crate::provider::codecs::openai_responses::decode_http_error(response)
                                .await,
                        );
                    }
                    let payload = decode_json_response(response, "invalid_device_response").await?;
                    let user_code = required(&payload, "user_code")?.to_owned();
                    let device_code = required(&payload, "device_auth_id")
                        .or_else(|_| required(&payload, "device_code"))?
                        .to_owned();
                    let interval_seconds = payload
                        .get("interval")
                        .and_then(Value::as_u64)
                        .or_else(|| {
                            payload
                                .get("interval")
                                .and_then(Value::as_str)
                                .and_then(|value| value.parse().ok())
                        })
                        .unwrap_or(5);
                    *self.pending_device.lock().map_err(|_| {
                        ProviderError::configuration("authentication state lock failed")
                    })? = Some(PendingDeviceAuth {
                        device_code: device_code.clone(),
                        user_code: user_code.clone(),
                        interval_seconds,
                        cancellation: CancellationToken::new(),
                    });
                    Ok(AuthChallenge::Device {
                        verification_url: payload
                            .get("verification_uri")
                            .or_else(|| payload.get("verification_url"))
                            .and_then(Value::as_str)
                            .unwrap_or("https://auth.openai.com/codex/device")
                            .into(),
                        user_code,
                        device_code,
                        interval_seconds,
                    })
                }
            }
        })
    }

    fn begin_auth_with_callback(
        &self,
        flow: AuthFlow,
        callback_url: Option<String>,
    ) -> ProviderFuture<'_, Result<AuthChallenge, ProviderError>> {
        Box::pin(async move {
            match (flow, callback_url) {
                (AuthFlow::BrowserPkce, Some(callback_url)) => {
                    self.begin_browser_auth(callback_url)
                }
                _ => self.begin_auth(flow).await,
            }
        })
    }

    #[allow(clippy::too_many_lines)]
    fn complete_auth(
        &self,
        response: AuthResponse,
    ) -> ProviderFuture<'_, Result<(), ProviderError>> {
        Box::pin(async move {
            match response {
                AuthResponse::AuthorizationCode { code, state } => {
                    let pending = {
                        self.pending_browser
                            .lock()
                            .map_err(|_| {
                                ProviderError::configuration("authentication state lock failed")
                            })?
                            .take()
                            .ok_or_else(|| {
                                auth_error(
                                    "missing_oauth_state",
                                    "no browser authentication is pending",
                                )
                            })?
                    };
                    if state != pending.state {
                        return Err(auth_error(
                            "oauth_state_mismatch",
                            "OAuth state did not match",
                        ));
                    }
                    let credential = self
                        .exchange(&[
                            ("grant_type", "authorization_code"),
                            ("client_id", CLIENT_ID),
                            ("code", &code),
                            ("redirect_uri", &pending.callback_url),
                            ("code_verifier", &pending.verifier),
                        ])
                        .await?;
                    self.save_credential(credential).await
                }
                AuthResponse::OAuthError {
                    error,
                    description,
                    state,
                } => {
                    let pending = self
                        .pending_browser
                        .lock()
                        .map_err(|_| {
                            ProviderError::configuration("authentication state lock failed")
                        })?
                        .take()
                        .ok_or_else(|| {
                            auth_error(
                                "missing_oauth_state",
                                "no browser authentication is pending",
                            )
                        })?;
                    if state != pending.state {
                        return Err(auth_error(
                            "oauth_state_mismatch",
                            "OAuth state did not match",
                        ));
                    }
                    let missing_entitlement = error == "access_denied"
                        && description.as_deref().is_some_and(|detail| {
                            detail
                                .to_ascii_lowercase()
                                .contains("missing_codex_entitlement")
                        });
                    if missing_entitlement {
                        Err(auth_error(
                            "missing_entitlement",
                            "ChatGPT account does not have Codex entitlement",
                        ))
                    } else {
                        Err(auth_error(
                            "oauth_authorization_failed",
                            &description.unwrap_or(error),
                        ))
                    }
                }
                AuthResponse::DeviceCode { device_code } => {
                    let pending = self
                        .pending_device
                        .lock()
                        .map_err(|_| {
                            ProviderError::configuration("authentication state lock failed")
                        })?
                        .clone()
                        .ok_or_else(|| {
                            auth_error(
                                "missing_device_state",
                                "no device authentication is pending",
                            )
                        })?;
                    if device_code != pending.device_code {
                        return Err(auth_error(
                            "device_code_mismatch",
                            "device code did not match the pending authentication",
                        ));
                    }
                    let started = tokio::time::Instant::now();
                    let response = loop {
                        let response = self
                            .client
                            .post(format!(
                                "{}/api/accounts/deviceauth/token",
                                self.auth_base_url
                            ))
                            .json(&json!({
                                "device_auth_id": device_code,
                                "user_code": pending.user_code,
                            }))
                            .send()
                            .await
                            .map_err(transport_error)?;
                        if response.status().is_success() {
                            break response;
                        }
                        if !matches!(
                            response.status(),
                            reqwest::StatusCode::FORBIDDEN | reqwest::StatusCode::NOT_FOUND
                        ) {
                            return Err(
                                crate::provider::codecs::openai_responses::decode_http_error(
                                    response,
                                )
                                .await,
                            );
                        }
                        if started.elapsed() >= std::time::Duration::from_secs(15 * 60) {
                            return Err(ProviderError::timeout(
                                "device_auth_timeout",
                                "device authentication timed out after 15 minutes",
                            ));
                        }
                        tokio::select! {
                            () = pending.cancellation.cancelled() => return Err(ProviderError::cancelled()),
                            () = tokio::time::sleep(std::time::Duration::from_secs(pending.interval_seconds)) => {}
                        }
                    };
                    let payload = decode_json_response(response, "invalid_device_token").await?;
                    let authorization_code = required(&payload, "authorization_code")?;
                    let code_verifier = required(&payload, "code_verifier")?;
                    let redirect_uri = format!("{}/deviceauth/callback", self.auth_base_url);
                    let credential = self
                        .exchange(&[
                            ("grant_type", "authorization_code"),
                            ("client_id", CLIENT_ID),
                            ("code", authorization_code),
                            ("redirect_uri", &redirect_uri),
                            ("code_verifier", code_verifier),
                        ])
                        .await?;
                    *self.pending_device.lock().map_err(|_| {
                        ProviderError::configuration("authentication state lock failed")
                    })? = None;
                    self.save_credential(credential).await
                }
                AuthResponse::Cancel => {
                    *self.pending_browser.lock().map_err(|_| {
                        ProviderError::configuration("authentication state lock failed")
                    })? = None;
                    if let Some(pending) = self
                        .pending_device
                        .lock()
                        .map_err(|_| {
                            ProviderError::configuration("authentication state lock failed")
                        })?
                        .take()
                    {
                        pending.cancellation.cancel();
                    }
                    Err(ProviderError::cancelled())
                }
            }
        })
    }

    fn disconnect(&self) -> ProviderFuture<'_, Result<(), ProviderError>> {
        Box::pin(async move {
            self.credentials.delete()?;
            *self.credential.write().await = None;
            Ok(())
        })
    }

    fn subscription_plan(&self) -> ProviderFuture<'_, Option<String>> {
        Box::pin(async move {
            self.credential
                .read()
                .await
                .as_ref()
                .and_then(|credential| credential.subscription_plan.clone())
        })
    }

    fn discover_models(&self) -> Option<ProviderFuture<'_, Result<ModelCatalog, ProviderError>>> {
        Some(Box::pin(async move {
            let credential = self.usable_credential().await?;
            let response = self
                .client
                .get(format!("{}/models", self.codex_base_url))
                .query(&[("client_version", CODEX_CLIENT_VERSION)])
                .bearer_auth(&credential.access_token)
                .header("ChatGPT-Account-Id", credential.account_id)
                .send()
                .await
                .map_err(transport_error)?;
            if !response.status().is_success() {
                return Err(
                    crate::provider::codecs::openai_responses::decode_http_error(response).await,
                );
            }
            let payload = decode_json_response(response, "invalid_models_response").await?;
            let catalog = parse_codex_models(&payload)?;
            *self.responses_lite_models.write().await = catalog
                .models
                .iter()
                .filter(|model| {
                    model
                        .raw_metadata
                        .get("use_responses_lite")
                        .and_then(Value::as_bool)
                        == Some(true)
                })
                .map(|model| model.id.clone())
                .collect();
            Ok(catalog)
        }))
    }

    fn stream(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> ProviderFuture<'_, Result<ProviderStream, ProviderError>> {
        Box::pin(async move {
            if request.model.provider != "chatgpt" {
                return Err(ProviderError::configuration(
                    "ChatGPT adapter received another provider's model",
                ));
            }
            if request.effort.as_deref() == Some("ultra")
                || request.input.iter().any(|item| {
                    matches!(item, crate::ModelInput::ConfigurationUpdate { effort } if effort == "ultra")
                })
            {
                return Err(ProviderError::configuration(
                    "ChatGPT ultra requires Codex orchestration, which Cagent does not support; select another reasoning effort",
                ));
            }
            let credential = self.usable_credential().await?;
            let responses_lite = self
                .responses_lite_models
                .read()
                .await
                .contains(&request.model.model);
            let (sender, receiver) = mpsc::channel(64);
            let mut lane_key = crate::provider::prompt_cache::native_cache_key(&request, "chatgpt")
                .unwrap_or_else(|| request.model.model.clone());
            lane_key.push_str("|routing=");
            lane_key.push_str(&codex_routing_hint(&request));
            let lane = {
                let mut lanes = self.websocket_lanes.lock().await;
                lanes
                    .entry(lane_key)
                    .or_insert_with(|| Arc::new(AsyncMutex::new(WebSocketLane::default())))
                    .clone()
            };
            let provider = self.clone();
            tokio::spawn(async move {
                provider
                    .stream_chatgpt(
                        lane,
                        credential,
                        request,
                        responses_lite,
                        cancellation,
                        sender,
                    )
                    .await;
            });
            Ok(Box::pin(ReceiverStream::new(receiver)) as ProviderStream)
        })
    }

    fn refresh_credentials(
        &self,
        cancellation: CancellationToken,
    ) -> ProviderFuture<'_, Result<bool, ProviderError>> {
        Box::pin(async move {
            let _guard = tokio::select! { biased; () = cancellation.cancelled() => return Err(ProviderError::cancelled()), guard = self.refresh_lock.lock() => guard };
            let current = self.credential.read().await.clone().ok_or_else(|| {
                auth_error(
                    "missing_subscription",
                    "ChatGPT subscription is not connected",
                )
            })?;
            if current
                .expires_at_millis
                .is_some_and(|expiry| expiry > now_millis().saturating_add(30_000))
            {
                return Ok(true);
            }
            let refresh = current.refresh_token.as_deref().ok_or_else(|| {
                auth_error(
                    "missing_refresh_token",
                    "ChatGPT credential cannot be refreshed",
                )
            })?;
            let mut credential = self
                .exchange(&[
                    ("grant_type", "refresh_token"),
                    ("client_id", CLIENT_ID),
                    ("refresh_token", refresh),
                ])
                .await?;
            if credential.refresh_token.is_none() {
                credential.refresh_token = current.refresh_token;
            }
            if credential.subscription_plan.is_none() {
                credential.subscription_plan = current.subscription_plan;
            }
            self.save_credential(credential).await?;
            Ok(true)
        })
    }
}

fn decode_usage_report(payload: &Value) -> ProviderUsageReport {
    fn append_windows(report: &mut ProviderUsageReport, rate_limit: &Value, limit_id: &str) {
        for key in ["primary_window", "secondary_window"] {
            let Some(window) = rate_limit.get(key) else {
                continue;
            };
            let Some(used_percent) = window.get("used_percent").and_then(Value::as_f64) else {
                continue;
            };
            let Some(duration_seconds) = window.get("limit_window_seconds").and_then(Value::as_u64)
            else {
                continue;
            };
            let label = usage_window_label(duration_seconds);
            report.windows.push(ProviderUsageWindow {
                id: format!("{limit_id}:{label}"),
                label,
                remaining_percent: remaining_percent(used_percent),
            });
        }
    }

    let mut report = ProviderUsageReport::default();
    if let Some(rate_limit) = payload.get("rate_limit") {
        append_windows(&mut report, rate_limit, "codex");
    }
    if let Some(additional) = payload
        .get("additional_rate_limits")
        .and_then(Value::as_array)
    {
        for limit in additional {
            if let (Some(limit_id), Some(rate_limit)) = (
                limit
                    .get("metered_feature")
                    .and_then(Value::as_str)
                    .or_else(|| limit.get("limit_name").and_then(Value::as_str)),
                limit.get("rate_limit"),
            ) {
                append_windows(&mut report, rate_limit, limit_id);
            }
        }
    }
    report
}

fn remaining_percent(used_percent: f64) -> u64 {
    if !used_percent.is_finite() {
        return 0;
    }
    (100.0 - used_percent).round().clamp(0.0, 100.0) as u64
}

fn usage_window_label(duration_seconds: u64) -> String {
    match duration_seconds {
        86_400 => "daily".into(),
        604_800 => "weekly".into(),
        2_592_000 => "monthly".into(),
        31_536_000 => "annual".into(),
        seconds if seconds % 3_600 == 0 => format!("{}h", seconds / 3_600),
        seconds if seconds % 60 == 0 => format!("{}m", seconds / 60),
        seconds => format!("{seconds}s"),
    }
}

const WEBSOCKET_RECONNECT_ATTEMPTS: u8 = 5;
const WEBSOCKET_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const WEBSOCKET_WRITE_TIMEOUT: Duration = WEBSOCKET_CONNECT_TIMEOUT;
const WEBSOCKET_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5 * 60);

type ChatGptWebSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

#[derive(Default)]
struct WebSocketLane {
    connection: Option<ChatGptWebSocket>,
    /// HTTP fallback is a property of this conversation lane. A bad socket for
    /// one conversation must not disable efficient streaming for every other
    /// ChatGPT conversation in this process.
    http_fallback: bool,
}

impl ChatGptProvider {
    async fn stream_chatgpt(
        &self,
        lane: Arc<AsyncMutex<WebSocketLane>>,
        credential: ManagedCredential,
        mut request: ModelRequest,
        responses_lite: bool,
        cancellation: CancellationToken,
        sender: mpsc::Sender<Result<crate::ProviderStreamEvent, ProviderError>>,
    ) {
        let mut reconnects = 0;
        loop {
            let mut lane_guard = match lock_websocket_lane(&lane, &cancellation).await {
                Ok(lane_guard) => lane_guard,
                Err(error) => {
                    let _ = sender.send(Err(error)).await;
                    return;
                }
            };
            if lane_guard.http_fallback {
                drop(lane_guard);
                self.stream_http(credential, request, cancellation, sender)
                    .await;
                return;
            }
            let connection = if let Some(connection) = lane_guard.connection.as_mut() {
                connection
            } else {
                match connect_websocket(
                    &self.codex_base_url,
                    &credential,
                    &request,
                    &cancellation,
                    self.websocket_connect_timeout,
                )
                .await
                {
                    Ok(connection) => {
                        lane_guard.connection = Some(connection);
                        lane_guard
                            .connection
                            .as_mut()
                            .expect("connection was inserted")
                    }
                    Err(error) if error.status == Some(426) => {
                        lane_guard.http_fallback = true;
                        drop(lane_guard);
                        self.stream_http(credential, request, cancellation, sender)
                            .await;
                        return;
                    }
                    Err(error) if error.kind == ProviderErrorKind::Cancelled => {
                        drop(lane_guard);
                        let _ = sender.send(Err(error)).await;
                        return;
                    }
                    Err(error) => {
                        drop(lane_guard);
                        request.response_transport_continuation = None;
                        if reconnects >= WEBSOCKET_RECONNECT_ATTEMPTS {
                            // This lane remains on stateless HTTP until its
                            // conversation key changes or the runtime exits.
                            let mut lane_guard =
                                match lock_websocket_lane(&lane, &cancellation).await {
                                    Ok(lane_guard) => lane_guard,
                                    Err(error) => {
                                        let _ = sender.send(Err(error)).await;
                                        return;
                                    }
                                };
                            lane_guard.http_fallback = true;
                            drop(lane_guard);
                            self.stream_http(credential, request, cancellation, sender)
                                .await;
                            return;
                        }
                        reconnects += 1;
                        tracing::debug!(attempt = reconnects, error = %error, "retrying ChatGPT Responses WebSocket connection");
                        continue;
                    }
                }
            };

            let result = stream_websocket_request(
                connection,
                &request,
                responses_lite,
                &cancellation,
                &sender,
            )
            .await;
            match result {
                Ok(()) => return,
                Err((error, emitted)) => {
                    lane_guard.connection = None;
                    drop(lane_guard);
                    if cancellation.is_cancelled() || emitted {
                        let _ = sender.send(Err(error)).await;
                        return;
                    }
                    if error.status == Some(426) || reconnects >= WEBSOCKET_RECONNECT_ATTEMPTS {
                        let mut lane_guard = match lock_websocket_lane(&lane, &cancellation).await {
                            Ok(lane_guard) => lane_guard,
                            Err(error) => {
                                let _ = sender.send(Err(error)).await;
                                return;
                            }
                        };
                        lane_guard.http_fallback = true;
                        drop(lane_guard);
                        self.stream_http(credential, request, cancellation, sender)
                            .await;
                        return;
                    }
                    request.response_transport_continuation = None;
                    reconnects += 1;
                    tracing::debug!(attempt = reconnects, error = %error, "retrying ChatGPT Responses WebSocket stream");
                }
            }
        }
    }

    async fn stream_http(
        &self,
        credential: ManagedCredential,
        mut request: ModelRequest,
        cancellation: CancellationToken,
        sender: mpsc::Sender<Result<crate::ProviderStreamEvent, ProviderError>>,
    ) {
        // Sticky HTTP fallback must never send a response ID or an incremental
        // suffix. The normalized request is always available in `input`.
        request.response_transport_continuation = None;
        let responses_lite = self
            .responses_lite_models
            .read()
            .await
            .contains(&request.model.model);
        let encoded = if responses_lite {
            crate::provider::codecs::openai_responses::encode_responses_lite(
                &request,
                &chatgpt_cache_policy(&request),
            )
        } else {
            crate::provider::codecs::openai_responses::encode(
                &request,
                &chatgpt_cache_policy(&request),
            )
        };
        let body = match encoded {
            Ok(body) => body,
            Err(error) => {
                let _ = sender.send(Err(error)).await;
                return;
            }
        };
        let mut response_request = self
            .client
            .post(format!("{}/responses", self.codex_base_url))
            .header(AUTHORIZATION, format!("Bearer {}", credential.access_token))
            .header("ChatGPT-Account-Id", credential.account_id)
            .header("originator", "cagent")
            .header("x-codex-routing-hint", codex_routing_hint(&request))
            .header(CONTENT_TYPE, "application/json");
        if responses_lite {
            response_request =
                response_request.header("x-openai-internal-codex-responses-lite", "true");
        }
        let response = match response_request.json(&body).send().await {
            Ok(response) => response,
            Err(error) => {
                let _ = sender.send(Err(transport_error(error))).await;
                return;
            }
        };
        if !response.status().is_success() {
            let _ = sender
                .send(Err(
                    crate::provider::codecs::openai_responses::decode_http_error(response).await,
                ))
                .await;
            return;
        }
        crate::provider::codecs::openai_responses::decode_stream(response, sender, cancellation)
            .await;
    }
}

async fn lock_websocket_lane<'a>(
    lane: &'a AsyncMutex<WebSocketLane>,
    cancellation: &CancellationToken,
) -> Result<tokio::sync::MutexGuard<'a, WebSocketLane>, ProviderError> {
    tokio::select! {
        biased;
        () = cancellation.cancelled() => Err(ProviderError::cancelled()),
        lane_guard = lane.lock() => Ok(lane_guard),
    }
}

async fn connect_websocket(
    base_url: &str,
    credential: &ManagedCredential,
    model_request: &ModelRequest,
    cancellation: &CancellationToken,
    connect_timeout: Duration,
) -> Result<ChatGptWebSocket, ProviderError> {
    install_rustls_provider();
    let mut url = Url::parse(&format!("{}/responses", base_url.trim_end_matches('/')))
        .map_err(|error| ProviderError::configuration(error.to_string()))?;
    let scheme = match url.scheme() {
        "https" => "wss".to_owned(),
        "http" => "ws".to_owned(),
        "wss" | "ws" => url.scheme().to_owned(),
        _ => {
            return Err(ProviderError::configuration(
                "invalid ChatGPT WebSocket URL",
            ));
        }
    };
    url.set_scheme(&scheme)
        .map_err(|()| ProviderError::configuration("invalid ChatGPT WebSocket URL"))?;
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|error| websocket_error(error.to_string(), None))?;
    request.headers_mut().insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", credential.access_token))
            .map_err(|error| websocket_error(error.to_string(), None))?,
    );
    request.headers_mut().insert(
        "ChatGPT-Account-Id",
        HeaderValue::from_str(&credential.account_id)
            .map_err(|error| websocket_error(error.to_string(), None))?,
    );
    request
        .headers_mut()
        .insert("originator", HeaderValue::from_static("cagent"));
    request.headers_mut().insert(
        "x-codex-routing-hint",
        HeaderValue::from_str(&codex_routing_hint(model_request))
            .map_err(|error| websocket_error(error.to_string(), None))?,
    );
    let conversation_id = codex_conversation_id(model_request);
    for (name, value) in [
        ("session-id", &conversation_id),
        ("thread-id", &conversation_id),
    ] {
        request.headers_mut().insert(
            name,
            HeaderValue::from_str(value)
                .map_err(|error| websocket_error(error.to_string(), None))?,
        );
    }
    request.headers_mut().insert(
        "OpenAI-Beta",
        HeaderValue::from_static("responses_websockets=2026-02-06"),
    );
    let handshake = tokio::select! {
        biased;
        () = cancellation.cancelled() => return Err(ProviderError::cancelled()),
        result = tokio::time::timeout(connect_timeout, connect_async(request)) => result,
    };
    handshake
        .map_err(|_| {
            ProviderError::timeout(
                "websocket_connect_timeout",
                "ChatGPT Responses WebSocket connection timed out",
            )
        })?
        .map(|(stream, _)| stream)
        .map_err(|error| match error {
            WebSocketError::Http(response) => websocket_error(
                format!("ChatGPT WebSocket handshake returned {}", response.status()),
                Some(response.status().as_u16()),
            ),
            error => websocket_error(error.to_string(), None),
        })
}

fn codex_routing_hint(request: &ModelRequest) -> String {
    match request.service_tier.as_deref() {
        Some(service_tier) => format!("model={};tier={service_tier}", request.model.model),
        None => format!("model={}", request.model.model),
    }
}

fn install_rustls_provider() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

async fn stream_websocket_request(
    websocket: &mut ChatGptWebSocket,
    request: &ModelRequest,
    responses_lite: bool,
    cancellation: &CancellationToken,
    sender: &mpsc::Sender<Result<crate::ProviderStreamEvent, ProviderError>>,
) -> Result<(), (ProviderError, bool)> {
    let mut payload = if responses_lite {
        crate::provider::codecs::openai_responses::encode_responses_lite(
            request,
            &chatgpt_cache_policy(request),
        )
    } else {
        crate::provider::codecs::openai_responses::encode(request, &chatgpt_cache_policy(request))
    }
    .map_err(|error| (error, false))?;
    payload["type"] = json!("response.create");
    // Codex's Responses WebSocket contract uses this non-secret metadata to
    // keep a conversation on one backend lane. It is deliberately stable for
    // the Cagent conversation and does not expose workspace data or tokens.
    let conversation_id = codex_conversation_id(request);
    payload["client_metadata"] = json!({
        "session_id": conversation_id,
        "thread_id": conversation_id,
        "turn_id": request.request_id.to_string(),
    });
    if responses_lite {
        payload["client_metadata"]["ws_request_header_x_openai_internal_codex_responses_lite"] =
            Value::String("true".into());
    }
    let wire_input_items = payload
        .get("input")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let continuation = payload.get("previous_response_id").is_some();
    let payload = payload.to_string();
    tracing::debug!(
        request_id = %request.request_id,
        continuation,
        wire_input_items,
        payload_bytes = payload.len(),
        "sending ChatGPT Responses WebSocket request"
    );
    send_websocket_message(
        websocket,
        Message::Text(payload.into()),
        cancellation,
        WEBSOCKET_WRITE_TIMEOUT,
    )
    .await
    .map_err(|error| (error, false))?;

    let mut tool_ids = BTreeMap::new();
    let mut emitted = false;
    let mut output_text_done = false;
    let mut saw_tool_call = false;
    let mut response_id = None::<String>;
    let mut steering = None::<tokio::sync::mpsc::UnboundedReceiver<String>>;
    let mut pending_steers = VecDeque::<String>::new();
    loop {
        enum Incoming {
            Message(Message),
            Steer(Option<String>),
        }
        let incoming = tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                return Err((ProviderError::cancelled(), emitted));
            }
            steer = async {
                match steering.as_mut() {
                    Some(receiver) => receiver.recv().await,
                    None => std::future::pending().await,
                }
            } => Incoming::Steer(steer),
            message = tokio::time::timeout(WEBSOCKET_IDLE_TIMEOUT, websocket.next()) => {
                Incoming::Message(match message {
                    Ok(Some(Ok(message))) => message,
                    Ok(Some(Err(error))) => {
                        if complete_text_only_websocket_stream(output_text_done, saw_tool_call, sender)
                            .await
                            .map_err(|error| (error, emitted))?
                        {
                            return Ok(());
                        }
                        return Err((websocket_error(error.to_string(), None), emitted));
                    }
                    Ok(None) => {
                        if complete_text_only_websocket_stream(output_text_done, saw_tool_call, sender)
                            .await
                            .map_err(|error| (error, emitted))?
                        {
                            return Ok(());
                        }
                        return Err((websocket_error("ChatGPT WebSocket closed before response.completed", None), emitted));
                    }
                    Err(_) => return Err((ProviderError::timeout("websocket_idle_timeout", "ChatGPT Responses WebSocket was idle for five minutes"), emitted)),
                })
            }
        };
        let Incoming::Message(message) = incoming else {
            let Incoming::Steer(input) = incoming else {
                unreachable!()
            };
            let Some(input) = input else {
                steering = None;
                continue;
            };
            let Some(previous_response_id) = response_id.as_ref() else {
                continue;
            };
            send_websocket_message(
                websocket,
                Message::Text(
                    json!({
                        "type": "response.steer",
                        "previous_response_id": previous_response_id,
                        "input": input,
                    })
                    .to_string()
                    .into(),
                ),
                cancellation,
                WEBSOCKET_WRITE_TIMEOUT,
            )
            .await
            .map_err(|error| (error, emitted))?;
            pending_steers.push_back(input);
            continue;
        };
        match message {
            Message::Text(text) => {
                let value =
                    match simd_json::serde::from_slice::<Value>(&mut text.as_bytes().to_vec()) {
                        Ok(value) => value,
                        Err(error) => {
                            return Err((
                                ProviderError::protocol(
                                    "invalid_websocket_json",
                                    error.to_string(),
                                ),
                                emitted,
                            ));
                        }
                    };
                output_text_done |=
                    value.get("type").and_then(Value::as_str) == Some("response.output_text.done");
                let event_type = value
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if event_type == "response.created" {
                    response_id = value
                        .pointer("/response/id")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    if steering.is_none()
                        && (request.model.model == "gpt-6-astra"
                            || request.model.model.starts_with("gpt-6-astra-"))
                        && let Some(response_id) = response_id.clone()
                    {
                        let (handle, receiver) = crate::provider::response_steering_channel();
                        steering = Some(receiver);
                        sender
                            .send(Ok(ProviderStreamEvent::Steerable {
                                response_id,
                                handle,
                            }))
                            .await
                            .map_err(|_| (ProviderError::cancelled(), emitted))?;
                    }
                    continue;
                }
                if event_type == "response.steer.accepted" {
                    if let Some(input) = pending_steers.pop_front() {
                        sender
                            .send(Ok(ProviderStreamEvent::SteerAccepted { input }))
                            .await
                            .map_err(|_| (ProviderError::cancelled(), emitted))?;
                    }
                    continue;
                }
                if event_type == "response.steer.failed" {
                    if let Some(input) = pending_steers.pop_front() {
                        sender
                            .send(Ok(ProviderStreamEvent::SteerFailed {
                                input,
                                code: value
                                    .pointer("/error/code")
                                    .and_then(Value::as_str)
                                    .unwrap_or("steering_failed")
                                    .into(),
                                message: value
                                    .pointer("/error/message")
                                    .and_then(Value::as_str)
                                    .unwrap_or("provider rejected steering input")
                                    .into(),
                            }))
                            .await
                            .map_err(|_| (ProviderError::cancelled(), emitted))?;
                    }
                    continue;
                }
                if event_type == "response.incomplete"
                    && value
                        .pointer("/response/incomplete_details/reason")
                        .and_then(Value::as_str)
                        == Some("steered")
                {
                    continue;
                }
                if value.get("type").and_then(Value::as_str) == Some("response.completed") {
                    let requested_service_tier =
                        request.service_tier.as_deref().unwrap_or("default");
                    let response_service_tier = value
                        .pointer("/response/service_tier")
                        .and_then(Value::as_str)
                        .unwrap_or("unreported");
                    tracing::info!(
                        request_id = %request.request_id,
                        requested_service_tier,
                        response_service_tier,
                        "ChatGPT response service tier"
                    );
                }
                if let Some(event) =
                    crate::provider::adapters::openai::encrypted_reasoning_event(&value)
                {
                    sender
                        .send(Ok(event))
                        .await
                        .map_err(|_| (ProviderError::cancelled(), emitted))?;
                    emitted = true;
                }
                match crate::provider::adapters::openai::normalize_event(&value, &mut tool_ids) {
                    Ok(None) => {}
                    Ok(Some(event)) => {
                        saw_tool_call |= matches!(
                            event,
                            ProviderStreamEvent::ToolCallStarted { .. }
                                | ProviderStreamEvent::ToolArgumentsDelta { .. }
                        );
                        emitted = true;
                        if let ProviderStreamEvent::Completed { metadata } = &event {
                            tracing::debug!(
                                request_id = %request.request_id,
                                response_id = metadata.provider_request_id.as_deref().unwrap_or(""),
                                input_tokens = metadata.usage.input_tokens,
                                non_cached_input_tokens = metadata.usage.non_cached_input_tokens,
                                cache_read_input_tokens = metadata.usage.cache_read_input_tokens,
                                cache_write_input_tokens = metadata.usage.cache_write_input_tokens,
                                output_tokens = metadata.usage.output_tokens,
                                "completed ChatGPT Responses WebSocket request"
                            );
                        }
                        let completed = matches!(event, ProviderStreamEvent::Completed { .. });
                        if sender.send(Ok(event)).await.is_err() {
                            return Err((ProviderError::cancelled(), emitted));
                        }
                        if completed {
                            return Ok(());
                        }
                    }
                    Err(error) => return Err((error, emitted)),
                }
            }
            Message::Ping(payload) => {
                send_websocket_message(
                    websocket,
                    Message::Pong(payload),
                    cancellation,
                    WEBSOCKET_WRITE_TIMEOUT,
                )
                .await
                .map_err(|error| (error, emitted))?;
            }
            Message::Pong(_) | Message::Frame(_) => {}
            Message::Binary(_) => {
                return Err((
                    ProviderError::protocol(
                        "invalid_websocket_message",
                        "ChatGPT sent a binary Responses event",
                    ),
                    emitted,
                ));
            }
            Message::Close(_) => {
                if complete_text_only_websocket_stream(output_text_done, saw_tool_call, sender)
                    .await
                    .map_err(|error| (error, emitted))?
                {
                    return Ok(());
                }
                return Err((
                    websocket_error("ChatGPT WebSocket closed before response.completed", None),
                    emitted,
                ));
            }
        }
    }
}

async fn send_websocket_message<S>(
    websocket: &mut S,
    message: Message,
    cancellation: &CancellationToken,
    deadline: Duration,
) -> Result<(), ProviderError>
where
    S: Sink<Message, Error = WebSocketError> + Unpin,
{
    tokio::select! {
        biased;
        () = cancellation.cancelled() => Err(ProviderError::cancelled()),
        result = tokio::time::timeout(deadline, websocket.send(message)) => match result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(websocket_error(error.to_string(), None)),
            Err(_) => Err(ProviderError::timeout(
                "websocket_write_timeout",
                "ChatGPT Responses WebSocket write timed out",
            )),
        },
    }
}

/// The Codex backend occasionally closes a text-only WebSocket immediately
/// after its explicit text terminator, without sending `response.completed`.
/// That terminator is sufficient to preserve the text, but not enough to
/// safely infer tool completion or response metadata.
async fn complete_text_only_websocket_stream(
    output_text_done: bool,
    saw_tool_call: bool,
    sender: &mpsc::Sender<Result<ProviderStreamEvent, ProviderError>>,
) -> Result<bool, ProviderError> {
    if !output_text_done || saw_tool_call {
        return Ok(false);
    }
    sender
        .send(Ok(ProviderStreamEvent::Completed {
            metadata: ResponseMetadata {
                provider_request_id: None,
                finish_reason: FinishReason::Stop,
                usage: ModelUsage::default(),
            },
        }))
        .await
        .map_err(|_| ProviderError::cancelled())?;
    Ok(true)
}

fn codex_conversation_id(request: &ModelRequest) -> String {
    request
        .response_transport_continuation
        .as_ref()
        .map(|continuation| continuation.conversation_id.to_string())
        .or_else(|| {
            request.prompt_cache.as_ref().and_then(|cache| {
                let key = cache.key.as_str();
                key.strip_prefix("cagent:conversation:")
                    .and_then(|suffix| suffix.split(':').next())
                    .map(ToOwned::to_owned)
            })
        })
        .unwrap_or_else(|| request.request_id.to_string())
}

fn chatgpt_cache_policy(
    request: &ModelRequest,
) -> crate::provider::codecs::openai_responses::PromptCachePolicy {
    crate::provider::prompt_cache::native_cache_key(request, "chatgpt").map_or(
        crate::provider::codecs::openai_responses::PromptCachePolicy::Disabled,
        |key| crate::provider::codecs::openai_responses::PromptCachePolicy::ChatGptCompatible {
            key,
        },
    )
}

fn websocket_error(message: impl Into<String>, status: Option<u16>) -> ProviderError {
    ProviderError {
        kind: match status {
            Some(401 | 403) => ProviderErrorKind::Authentication,
            Some(408) => ProviderErrorKind::Timeout,
            Some(429) => ProviderErrorKind::RateLimit,
            Some(400..=499) => ProviderErrorKind::InvalidRequest,
            Some(500..=599) => ProviderErrorKind::Server,
            _ => ProviderErrorKind::Connection,
        },
        code: "websocket_transport".into(),
        message: message.into(),
        retryable: true,
        retry_after_millis: None,
        status,
        metadata: BTreeMap::new(),
    }
}

async fn decode_json_response(
    response: reqwest::Response,
    error_code: &'static str,
) -> Result<Value, ProviderError> {
    let mut body = response
        .bytes()
        .await
        .map(|bytes| bytes.to_vec())
        .map_err(|error| ProviderError::protocol(error_code, error.to_string()))?;
    simd_json::serde::from_slice(&mut body)
        .map_err(|error| ProviderError::protocol(error_code, error.to_string()))
}

fn credential_from_token_response(payload: &Value) -> Result<ManagedCredential, ProviderError> {
    let access_token = required(payload, "access_token")?.to_owned();
    let id_token = payload
        .get("id_token")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let claims = id_token
        .as_deref()
        .and_then(jwt_claims)
        .or_else(|| jwt_claims(&access_token))
        .unwrap_or(Value::Null);
    let account_id = claims
        .get("chatgpt_account_id")
        .and_then(Value::as_str)
        .or_else(|| {
            claims
                .get("https://api.openai.com/auth")
                .and_then(|auth| auth.get("chatgpt_account_id"))
                .and_then(Value::as_str)
        })
        .or_else(|| {
            claims
                .pointer("/organizations/0/id")
                .and_then(Value::as_str)
        })
        .ok_or_else(|| {
            auth_error(
                "missing_entitlement",
                "token does not identify a Codex-enabled ChatGPT account",
            )
        })?
        .to_owned();
    Ok(ManagedCredential {
        access_token,
        refresh_token: payload
            .get("refresh_token")
            .and_then(Value::as_str)
            .map(str::to_owned),
        id_token,
        expires_at_millis: payload
            .get("expires_in")
            .and_then(Value::as_u64)
            .map(|seconds| now_millis().saturating_add(seconds.saturating_mul(1_000))),
        account_id,
        has_codex_entitlement: true,
        subscription_plan: claims
            .get("chatgpt_plan_type")
            .and_then(Value::as_str)
            .or_else(|| {
                claims
                    .get("https://api.openai.com/auth")
                    .and_then(|auth| auth.get("chatgpt_plan_type"))
                    .and_then(Value::as_str)
            })
            .map(str::to_owned),
    })
}

fn jwt_claims(token: &str) -> Option<Value> {
    let part = token.split('.').nth(1)?;
    let mut decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(part)
        .ok()?;
    simd_json::serde::from_slice(&mut decoded).ok()
}

fn parse_codex_models(payload: &Value) -> Result<ModelCatalog, ProviderError> {
    if payload.get("data").is_some() {
        return super::openai::parse_model_catalog("chatgpt", payload);
    }
    let models = payload
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            ProviderError::protocol(
                "invalid_models_response",
                "Codex model response has no models array",
            )
        })?
        .iter()
        .filter(|model| {
            model
                .get("visibility")
                .and_then(Value::as_str)
                .is_none_or(|visibility| visibility == "list")
        })
        .filter_map(|model| {
            let id = model
                .get("slug")
                .or_else(|| model.get("id"))?
                .as_str()?
                .to_owned();
            Some(crate::ModelDescriptor {
                id: id.clone(),
                display_name: model
                    .get("display_name")
                    .or_else(|| model.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or(&id)
                    .into(),
                capabilities: crate::ModelCapabilities {
                    supports_streaming: Some(true),
                    supports_tools: Some(true),
                    supports_structured_output: Some(true),
                    supports_text_input: Some(true),
                    supports_image_input: model
                        .get("input_modalities")
                        .and_then(Value::as_array)
                        .map(|values| values.iter().any(|value| value.as_str() == Some("image"))),
                    supports_text_output: Some(true),
                    supports_fast_mode: Some(codex_model_supports_fast_mode(model)),
                    context_window: model.get("context_window").and_then(Value::as_u64),
                    reasoning_control: Some(crate::ReasoningControl::Effort),
                    reasoning_efforts: model
                        .get("supported_reasoning_levels")
                        .or_else(|| model.get("supported_reasoning_efforts"))
                        .and_then(Value::as_array)
                        .map(|values| {
                            values
                                .iter()
                                .filter_map(|value| {
                                    value
                                        .as_str()
                                        .or_else(|| value.get("effort").and_then(Value::as_str))
                                        .map(str::to_owned)
                                })
                                // Codex's ultra is an orchestration mode, not a native effort.
                                .filter(|effort| effort != "ultra")
                                .collect()
                        }),
                },
                backend: Some(ModelBackend::OpenAiResponses),
                raw_metadata: model.clone(),
            })
        })
        .collect();
    Ok(ModelCatalog {
        provider: "chatgpt".into(),
        models,
        version: payload
            .get("version")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

fn codex_model_supports_fast_mode(model: &Value) -> bool {
    model
        .get("service_tiers")
        .and_then(Value::as_array)
        .is_some_and(|tiers| {
            tiers.iter().any(|tier| {
                tier.get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| id == "priority")
            })
        })
        || model
            .get("additional_speed_tiers")
            .and_then(Value::as_array)
            .is_some_and(|tiers| tiers.iter().any(|tier| tier.as_str() == Some("fast")))
}

fn required<'a>(value: &'a Value, name: &str) -> Result<&'a str, ProviderError> {
    value
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| ProviderError::protocol("invalid_auth_response", format!("missing {name}")))
}
fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}
fn auth_error(code: &str, message: &str) -> ProviderError {
    ProviderError {
        kind: ProviderErrorKind::Authentication,
        code: code.into(),
        message: message.into(),
        retryable: false,
        retry_after_millis: None,
        status: None,
        metadata: BTreeMap::new(),
    }
}
#[allow(clippy::needless_pass_by_value)]
fn transport_error(error: reqwest::Error) -> ProviderError {
    ProviderError {
        kind: if error.is_timeout() {
            ProviderErrorKind::Timeout
        } else {
            ProviderErrorKind::Connection
        },
        code: "authentication_transport".into(),
        message: error.to_string(),
        retryable: true,
        retry_after_millis: None,
        status: error.status().map(|status| status.as_u16()),
        metadata: BTreeMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::task::{Context, Poll};

    #[test]
    fn codex_catalog_detects_fast_mode_from_service_tiers() {
        let catalog = parse_codex_models(&json!({
            "models": [
                {
                    "slug": "gpt-fast",
                    "display_name": "GPT Fast",
                    "visibility": "list",
                    "supported_reasoning_levels": [
                        {"effort": "medium", "description": "Balanced"},
                        {"effort": "high", "description": "More reasoning"}
                    ],
                    "service_tiers": [
                        {"id": "priority", "name": "Fast", "description": "Priority processing"}
                    ]
                },
                {
                    "slug": "gpt-standard",
                    "display_name": "GPT Standard",
                    "visibility": "list",
                    "service_tiers": [
                        {"id": "flex", "name": "Flex", "description": "Flexible processing"}
                    ]
                }
            ]
        }))
        .unwrap();

        assert_eq!(
            catalog.models[0].capabilities.supports_fast_mode,
            Some(true)
        );
        assert_eq!(
            catalog.models[0].capabilities.reasoning_efforts.as_deref(),
            Some(["medium".into(), "high".into()].as_slice())
        );
        assert_eq!(
            catalog.models[1].capabilities.supports_fast_mode,
            Some(false)
        );
    }

    #[test]
    fn codex_catalog_detects_legacy_fast_speed_tier() {
        let catalog = parse_codex_models(&json!({
            "models": [{
                "slug": "gpt-fast",
                "visibility": "list",
                "additional_speed_tiers": ["fast"]
            }]
        }))
        .unwrap();

        assert_eq!(
            catalog.models[0].capabilities.supports_fast_mode,
            Some(true)
        );
    }

    #[test]
    fn codex_catalog_excludes_ultra_but_preserves_other_advertised_efforts() {
        for levels in [
            json!(["low", "xhigh", "max", "ultra", "future-effort"]),
            json!([{"effort": "low"}, {"effort": "xhigh"}, {"effort": "max"}, {"effort": "ultra"}, {"effort": "future-effort"}]),
        ] {
            let catalog = parse_codex_models(&json!({
                "models": [{"slug": "future-model", "supported_reasoning_levels": levels}]
            }))
            .unwrap();
            assert_eq!(
                catalog.models[0]
                    .capabilities
                    .reasoning_efforts
                    .as_ref()
                    .unwrap(),
                &["low", "xhigh", "max", "future-effort"]
            );
        }
    }

    #[tokio::test]
    async fn ultra_requests_are_rejected_before_authentication_or_network_access() {
        let temporary = tempfile::TempDir::new().unwrap();
        let provider = ChatGptProvider::with_endpoints(temporary.path(), "http://127.0.0.1:9");
        for in_history in [false, true] {
            let mut request = test_stream_request();
            if in_history {
                request.input.push(crate::ModelInput::ConfigurationUpdate {
                    effort: "ultra".into(),
                });
            } else {
                request.effort = Some("ultra".into());
            }
            let error = match provider.stream(request, CancellationToken::new()).await {
                Ok(_) => panic!("ultra must not reach the provider"),
                Err(error) => error,
            };
            assert!(error.message.contains("ultra requires Codex orchestration"));
        }
    }

    #[test]
    fn codex_catalog_discovers_gpt_6_sol_and_luna_without_an_allowlist() {
        let catalog = parse_codex_models(&json!({
            "models": [
                {
                    "slug": "gpt-6-sol",
                    "display_name": "GPT-6 Sol",
                    "visibility": "list",
                    "context_window": 272_000,
                    "input_modalities": ["text", "image"],
                    "supported_reasoning_levels": ["low", "medium", "high", "xhigh", "max", "ultra"],
                    "service_tiers": [{"id": "priority"}]
                },
                {
                    "slug": "gpt-6-luna",
                    "display_name": "GPT-6 Luna",
                    "visibility": "list",
                    "context_window": 272_000,
                    "input_modalities": ["text", "image"],
                    "supported_reasoning_levels": ["low", "medium", "high", "xhigh", "max"],
                    "additional_speed_tiers": ["fast"]
                }
            ]
        }))
        .unwrap();

        assert_eq!(catalog.models.len(), 2);
        assert_eq!(catalog.models[0].id, "gpt-6-sol");
        assert_eq!(catalog.models[0].capabilities.context_window, Some(272_000));
        assert_eq!(
            catalog.models[0].capabilities.supports_image_input,
            Some(true)
        );
        assert_eq!(
            catalog.models[0].capabilities.supports_fast_mode,
            Some(true)
        );
        assert_eq!(
            catalog.models[0].capabilities.reasoning_efforts.as_deref(),
            Some(
                ["low", "medium", "high", "xhigh", "max"]
                    .map(str::to_owned)
                    .as_slice()
            )
        );
        assert_eq!(catalog.models[1].id, "gpt-6-luna");
        assert_eq!(
            catalog.models[1].capabilities.reasoning_efforts.as_deref(),
            Some(
                ["low", "medium", "high", "xhigh", "max"]
                    .map(str::to_owned)
                    .as_slice()
            )
        );
    }

    #[tokio::test]
    async fn model_discovery_sends_the_required_client_version_and_authentication() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let Ok(listener) = tokio::net::TcpListener::bind("127.0.0.1:0").await else {
            return;
        };
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 4096];
            let length = socket.read(&mut request).await.unwrap();
            let request = std::str::from_utf8(&request[..length]).unwrap();
            assert!(request.starts_with(&format!(
                "GET /codex/models?client_version={} HTTP/1.1",
                CODEX_CLIENT_VERSION
            )));
            let lower = request.to_ascii_lowercase();
            assert!(lower.contains("authorization: bearer access-token\r\n"));
            assert!(lower.contains("chatgpt-account-id: acct-test\r\n"));
            let body = r#"{"models":[{"slug":"gpt-6-sol","display_name":"GPT-6 Sol","visibility":"list","use_responses_lite":true},{"slug":"gpt-6-luna","display_name":"GPT-6 Luna","visibility":"list","use_responses_lite":true}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });

        let temporary = tempfile::TempDir::new().unwrap();
        let provider =
            ChatGptProvider::with_endpoints(temporary.path(), &format!("http://{address}"));
        *provider.credential.write().await = Some(ManagedCredential {
            access_token: "access-token".into(),
            refresh_token: None,
            id_token: None,
            expires_at_millis: None,
            account_id: "acct-test".into(),
            has_codex_entitlement: true,
            subscription_plan: None,
        });

        let catalog = provider.discover_models().unwrap().await.unwrap();
        server.await.unwrap();

        assert_eq!(catalog.models[0].id, "gpt-6-sol");
        assert_eq!(catalog.models[1].id, "gpt-6-luna");
        assert!(
            provider
                .responses_lite_models
                .read()
                .await
                .contains("gpt-6-sol")
        );
        assert!(
            provider
                .responses_lite_models
                .read()
                .await
                .contains("gpt-6-luna")
        );
    }

    struct PendingWriteSink {
        started: Arc<tokio::sync::Notify>,
    }

    impl Sink<Message> for PendingWriteSink {
        type Error = WebSocketError;

        fn poll_ready(
            self: std::pin::Pin<&mut Self>,
            _context: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            self.started.notify_one();
            Poll::Pending
        }

        fn start_send(self: std::pin::Pin<&mut Self>, _item: Message) -> Result<(), Self::Error> {
            unreachable!("a pending sink must not accept a message")
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _context: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Pending
        }

        fn poll_close(
            self: std::pin::Pin<&mut Self>,
            _context: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Pending
        }
    }

    async fn stream_until_websocket_close(
        frames: Vec<Value>,
        send_close_frame: bool,
    ) -> (Result<(), (ProviderError, bool)>, Vec<ProviderStreamEvent>) {
        use tokio_tungstenite::accept_async;

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut websocket = accept_async(stream).await.unwrap();
            let _ = websocket.next().await;
            for frame in frames {
                websocket
                    .send(Message::Text(frame.to_string().into()))
                    .await
                    .unwrap();
            }
            if send_close_frame {
                websocket.send(Message::Close(None)).await.unwrap();
            }
        });
        let (mut websocket, _) = connect_async(format!("ws://{address}")).await.unwrap();
        let (sender, mut receiver) = mpsc::channel(8);
        let result = stream_websocket_request(
            &mut websocket,
            &test_stream_request(),
            false,
            &CancellationToken::new(),
            &sender,
        )
        .await;
        drop(sender);
        server.await.unwrap();
        let mut events = Vec::new();
        while let Ok(event) = receiver.try_recv() {
            events.push(event.unwrap());
        }
        (result, events)
    }

    fn test_stream_request() -> ModelRequest {
        ModelRequest {
            request_id: crate::RequestId::new(),
            attempt_id: crate::AttemptId::new(),
            model: crate::ModelRef::parse("chatgpt/gpt-test").unwrap(),
            backend: Some(ModelBackend::OpenAiResponses),
            backend_candidates: vec![ModelBackend::OpenAiResponses],
            effort: Some("high".into()),
            service_tier: None,
            input: vec![crate::ModelInput::Message {
                role: crate::MessageRole::User,
                content: "hello".into(),
            }],
            tools: Vec::new(),
            stable_prompt: vec![crate::StablePromptPart {
                identity: "instructions".into(),
                content: "Be concise".into(),
            }],
            prompt_cache: Some(crate::PromptCacheRequest {
                key: "cagent:conversation:test".into(),
                scope: crate::PromptCacheScope::Conversation,
            }),
            response_transport_continuation: None,
            allow_parallel_tools: true,
            structured_output: None,
        }
    }

    #[tokio::test]
    async fn cancelled_request_waiting_for_conversation_lane_returns_promptly() {
        let temporary = tempfile::TempDir::new().unwrap();
        let provider = ChatGptProvider::with_endpoints(temporary.path(), "http://127.0.0.1:9");
        let lane = Arc::new(AsyncMutex::new(WebSocketLane::default()));
        let lane_guard = lane.lock().await;
        let cancellation = CancellationToken::new();
        let (sender, mut receiver) = mpsc::channel(1);
        let credential = ManagedCredential {
            access_token: "access-token".into(),
            refresh_token: None,
            id_token: None,
            expires_at_millis: None,
            account_id: "acct-test".into(),
            has_codex_entitlement: true,
            subscription_plan: None,
        };

        let task = tokio::spawn({
            let provider = provider.clone();
            let lane = lane.clone();
            let cancellation = cancellation.clone();
            async move {
                provider
                    .stream_chatgpt(
                        lane,
                        credential,
                        test_stream_request(),
                        false,
                        cancellation,
                        sender,
                    )
                    .await;
            }
        });
        tokio::task::yield_now().await;
        cancellation.cancel();
        let error = tokio::time::timeout(Duration::from_millis(250), receiver.recv())
            .await
            .expect("cancelled request should not wait for the held lane")
            .expect("stream should report cancellation")
            .unwrap_err();
        drop(lane_guard);
        task.await.unwrap();

        assert_eq!(error.kind, ProviderErrorKind::Cancelled);
    }

    #[tokio::test]
    async fn websocket_write_cancellation_interrupts_a_pending_write() {
        let started = Arc::new(tokio::sync::Notify::new());
        let cancellation = CancellationToken::new();
        let write = tokio::spawn({
            let started = started.clone();
            let cancellation = cancellation.clone();
            async move {
                send_websocket_message(
                    &mut PendingWriteSink { started },
                    Message::Text("request".into()),
                    &cancellation,
                    Duration::from_secs(1),
                )
                .await
            }
        });
        started.notified().await;
        cancellation.cancel();
        let error = tokio::time::timeout(Duration::from_millis(250), write)
            .await
            .expect("cancellation should interrupt the pending write")
            .unwrap()
            .unwrap_err();

        assert_eq!(error.kind, ProviderErrorKind::Cancelled);
    }

    #[tokio::test]
    async fn websocket_write_timeout_is_retryable() {
        let error = send_websocket_message(
            &mut PendingWriteSink {
                started: Arc::new(tokio::sync::Notify::new()),
            },
            Message::Text("request".into()),
            &CancellationToken::new(),
            Duration::from_millis(5),
        )
        .await
        .unwrap_err();

        assert_eq!(error.kind, ProviderErrorKind::Timeout);
        assert_eq!(error.code, "websocket_write_timeout");
        assert!(error.retryable);
    }

    #[tokio::test]
    async fn text_done_then_websocket_reset_synthesizes_metadata_less_completion() {
        let (result, events) = stream_until_websocket_close(
            vec![
                json!({"type":"response.output_text.delta","delta":"complete text"}),
                json!({"type":"response.output_text.done","text":"complete text"}),
            ],
            false,
        )
        .await;

        assert!(result.is_ok());
        assert!(matches!(
            events.as_slice(),
            [
                ProviderStreamEvent::TextDelta { delta },
                ProviderStreamEvent::Completed { metadata },
            ] if delta == "complete text"
                && metadata.provider_request_id.is_none()
                && metadata.finish_reason == FinishReason::Stop
                && metadata.usage == ModelUsage::default()
        ));
    }

    #[tokio::test]
    async fn websocket_close_before_text_done_remains_an_error() {
        let (result, events) = stream_until_websocket_close(
            vec![json!({"type":"response.output_text.delta","delta":"truncated"})],
            true,
        )
        .await;

        let error = result.unwrap_err().0;
        assert_eq!(error.code, "websocket_transport");
        assert!(
            events
                .iter()
                .all(|event| !matches!(event, ProviderStreamEvent::Completed { .. }))
        );
    }

    #[tokio::test]
    async fn tool_call_then_websocket_close_remains_an_error() {
        let (result, events) = stream_until_websocket_close(
            vec![
                json!({
                    "type":"response.output_item.added",
                    "output_index":0,
                    "item":{"type":"function_call","id":"item-1","call_id":"call-1","name":"read"}
                }),
                json!({"type":"response.output_text.done","text":""}),
            ],
            true,
        )
        .await;

        let error = result.unwrap_err().0;
        assert_eq!(error.code, "websocket_transport");
        assert!(matches!(
            events.as_slice(),
            [ProviderStreamEvent::ToolCallStarted { .. }]
        ));
    }

    async fn recorded_token_exchange(
        response_body: String,
    ) -> (String, tokio::task::JoinHandle<String>) {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 4096];
            loop {
                let read = stream.read(&mut buffer).await.unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
                let Some(headers_end) = request.windows(4).position(|part| part == b"\r\n\r\n")
                else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&request[..headers_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        line.strip_prefix("content-length: ")
                            .or_else(|| line.strip_prefix("Content-Length: "))
                    })
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(0);
                if request.len() >= headers_end + 4 + content_length {
                    break;
                }
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            String::from_utf8(request).unwrap()
        });
        (format!("http://{address}"), task)
    }

    async fn recorded_search(response_body: String) -> (String, tokio::task::JoinHandle<String>) {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 4096];
            loop {
                let read = stream.read(&mut buffer).await.unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
                let Some(headers_end) = request.windows(4).position(|part| part == b"\r\n\r\n")
                else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&request[..headers_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .map(str::to_owned)
                    })
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(0);
                if request.len() >= headers_end + 4 + content_length {
                    break;
                }
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            String::from_utf8(request).unwrap()
        });
        (format!("http://{address}"), task)
    }

    #[tokio::test]
    async fn web_search_uses_codex_path_body_and_auth_headers() {
        let (base_url, recorded) = recorded_search(
            json!({
                "results": [{
                    "title": "Result",
                    "url": "https://example.com",
                    "snippet": "Snippet"
                }]
            })
            .to_string(),
        )
        .await;
        let temporary = tempfile::TempDir::new().unwrap();
        let provider = ChatGptProvider::with_endpoints(temporary.path(), &base_url);
        *provider.credential.write().await = Some(ManagedCredential {
            access_token: "access-token".into(),
            refresh_token: None,
            id_token: None,
            expires_at_millis: None,
            account_id: "acct-test".into(),
            has_codex_entitlement: true,
            subscription_plan: None,
        });

        let response = provider
            .search(
                ProviderWebSearchRequest {
                    id: "conversation-id".into(),
                    model: "gpt-test".into(),
                    query: "standalone web search".into(),
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(response.results.len(), 1);
        let request = recorded.await.unwrap();
        assert!(request.starts_with("POST /codex/alpha/search HTTP/1.1"));
        let lower = request.to_ascii_lowercase();
        assert!(lower.contains("authorization: bearer access-token"));
        assert!(lower.contains("chatgpt-account-id: acct-test"));
        assert!(lower.contains("originator: chatgpt_cca"));
        assert!(lower.contains("x-codex-turn-metadata:"));
        let body = request
            .split_once("\r\n\r\n")
            .map(|(_, body)| body)
            .unwrap();
        let body: Value = serde_json::from_str(body).unwrap();
        assert_eq!(
            body["commands"]["search_query"][0]["q"],
            "standalone web search"
        );
        assert_eq!(body["settings"]["allowed_callers"][0], "direct");
        assert_eq!(body["input"][0]["content"][0]["text"], "Search the web");
    }

    #[tokio::test]
    async fn browser_flow_matches_the_codex_oauth_contract() {
        let temporary = tempfile::TempDir::new().unwrap();
        let provider = ChatGptProvider::with_endpoints(temporary.path(), "http://127.0.0.1:9");
        let AuthChallenge::Browser {
            authorization_url,
            state,
            ..
        } = provider.begin_auth(AuthFlow::BrowserPkce).await.unwrap()
        else {
            panic!("expected browser challenge");
        };
        let url = reqwest::Url::parse(&authorization_url).unwrap();
        let query = url.query_pairs().collect::<BTreeMap<_, _>>();
        assert_eq!(query.get("client_id").unwrap(), CLIENT_ID);
        assert_eq!(query.get("scope").unwrap(), OAUTH_SCOPE);
        assert_eq!(query.get("state").unwrap(), &state);
        assert_eq!(query.get("code_challenge_method").unwrap(), "S256");
        assert_eq!(query.get("codex_cli_simplified_flow").unwrap(), "true");
    }

    #[tokio::test]
    async fn authorization_code_exchange_is_form_encoded() {
        let claims = json!({ "chatgpt_account_id": "acct-recorded" });
        let token = format!(
            "x.{}.y",
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(serde_json::to_vec(&claims).unwrap())
        );
        let response = json!({
            "access_token": token,
            "refresh_token": "refresh-recorded",
            "expires_in": 3600
        })
        .to_string();
        let (base_url, recorded) = recorded_token_exchange(response).await;
        let temporary = tempfile::TempDir::new().unwrap();
        let provider = ChatGptProvider::with_endpoints(temporary.path(), &base_url);
        let AuthChallenge::Browser { state, .. } =
            provider.begin_auth(AuthFlow::BrowserPkce).await.unwrap()
        else {
            panic!("expected browser challenge");
        };
        provider
            .complete_auth(AuthResponse::AuthorizationCode {
                code: "code with spaces".into(),
                state,
            })
            .await
            .unwrap();
        let request = recorded.await.unwrap();
        assert!(request.starts_with("POST /oauth/token HTTP/1.1"));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("content-type: application/x-www-form-urlencoded")
        );
        assert!(request.contains("grant_type=authorization_code"));
        assert!(request.contains("code=code+with+spaces"));
        assert!(request.contains("code_verifier="));
        let reopened = ChatGptProvider::with_endpoints(temporary.path(), &base_url);
        assert!(matches!(
            reopened.auth_state().await.unwrap(),
            AuthState::Connected { .. }
        ));
        reopened.disconnect().await.unwrap();
    }

    #[tokio::test]
    async fn usage_endpoint_normalizes_primary_secondary_and_additional_windows() {
        let response = json!({
            "rate_limit": {
                "primary_window": {
                    "used_percent": 19.6,
                    "limit_window_seconds": 18_000
                },
                "secondary_window": {
                    "used_percent": 90.4,
                    "limit_window_seconds": 604_800
                }
            },
            "additional_rate_limits": [{
                "limit_name": "codex_other",
                "rate_limit": {
                    "primary_window": {
                        "used_percent": 25,
                        "limit_window_seconds": 2_592_000
                    }
                }
            }, {
                "limit_name": "GPT-5.3-Codex-Spark",
                "metered_feature": "codex_bengalfox",
                "rate_limit": {
                    "primary_window": {
                        "used_percent": 0,
                        "limit_window_seconds": 604_800
                    }
                }
            }]
        })
        .to_string();
        let (base_url, recorded) = recorded_token_exchange(response).await;
        let temporary = tempfile::TempDir::new().unwrap();
        let provider = ChatGptProvider::with_endpoints(temporary.path(), &base_url);
        *provider.credential.write().await = Some(ManagedCredential {
            access_token: "access-token".into(),
            refresh_token: None,
            id_token: None,
            expires_at_millis: None,
            account_id: "acct-test".into(),
            has_codex_entitlement: true,
            subscription_plan: None,
        });

        let report = provider.provider_usage().unwrap().await.unwrap();
        assert_eq!(
            report.windows,
            [
                ProviderUsageWindow {
                    id: "codex:5h".into(),
                    label: "5h".into(),
                    remaining_percent: 80,
                },
                ProviderUsageWindow {
                    id: "codex:weekly".into(),
                    label: "weekly".into(),
                    remaining_percent: 10,
                },
                ProviderUsageWindow {
                    id: "codex_other:monthly".into(),
                    label: "monthly".into(),
                    remaining_percent: 75,
                },
                ProviderUsageWindow {
                    id: "codex_bengalfox:weekly".into(),
                    label: "weekly".into(),
                    remaining_percent: 100,
                },
            ]
        );
        let request = recorded.await.unwrap();
        assert!(request.starts_with("GET /codex/usage HTTP/1.1"));
        let request = request.to_ascii_lowercase();
        assert!(request.contains("authorization: bearer access-token"));
        assert!(request.contains("chatgpt-account-id: acct-test"));
    }

    #[test]
    fn usage_percent_rounding_clamps_invalid_provider_values() {
        assert_eq!(remaining_percent(89.6), 10);
        assert_eq!(remaining_percent(-20.0), 100);
        assert_eq!(remaining_percent(120.0), 0);
        assert_eq!(remaining_percent(f64::NAN), 0);
        assert_eq!(usage_window_label(86_400), "daily");
        assert_eq!(usage_window_label(31_536_000), "annual");
    }

    #[test]
    fn extracts_account_from_namespaced_claim_without_exposing_token() {
        let temporary = tempfile::TempDir::new().unwrap();
        let provider = ChatGptProvider::with_endpoints(temporary.path(), "http://127.0.0.1:9");
        assert_eq!(provider.codex_base_url, "http://127.0.0.1:9/codex");
        let claims = json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "acct-test",
                "chatgpt_plan_type": "pro"
            }
        });
        let token = format!(
            "x.{}.y",
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(serde_json::to_vec(&claims).unwrap())
        );
        let credential = credential_from_token_response(
            &json!({ "access_token": token, "refresh_token": "refresh-canary", "expires_in": 3600 }),
        )
        .unwrap();
        assert_eq!(credential.account_id, "acct-test");
        assert_eq!(credential.subscription_plan.as_deref(), Some("pro"));
        let diagnostic = format!("{credential:?}");
        assert!(!diagnostic.contains(&credential.access_token));
        assert!(!diagnostic.contains("refresh-canary"));
    }

    #[tokio::test]
    async fn web_search_readiness_requires_entitlement_and_allows_refreshable_expiry() {
        let temporary = tempfile::TempDir::new().unwrap();
        let provider = ChatGptProvider::with_endpoints(temporary.path(), "http://127.0.0.1:9");
        assert!(!provider.web_search_ready());

        *provider.credential.write().await = Some(ManagedCredential {
            access_token: "access-token".into(),
            refresh_token: None,
            id_token: None,
            expires_at_millis: None,
            account_id: "acct-test".into(),
            has_codex_entitlement: false,
            subscription_plan: None,
        });
        assert!(!provider.web_search_ready());

        if let Some(credential) = provider.credential.write().await.as_mut() {
            credential.has_codex_entitlement = true;
            credential.expires_at_millis = Some(now_millis().saturating_sub(1));
            credential.refresh_token = Some("refresh-token".into());
        }
        assert!(provider.web_search_ready());
    }

    #[tokio::test]
    async fn browser_flow_rejects_state_mismatch_before_token_exchange() {
        let temporary = tempfile::TempDir::new().unwrap();
        let provider = ChatGptProvider::with_endpoints(temporary.path(), "http://127.0.0.1:9");
        let challenge = provider.begin_auth(AuthFlow::BrowserPkce).await.unwrap();
        assert!(matches!(challenge, AuthChallenge::Browser { .. }));
        let error = provider
            .complete_auth(AuthResponse::AuthorizationCode {
                code: "code".into(),
                state: "wrong".into(),
            })
            .await
            .unwrap_err();
        assert_eq!(error.code, "oauth_state_mismatch");
    }

    #[tokio::test]
    async fn cancellation_is_explicit_and_clears_browser_state() {
        let temporary = tempfile::TempDir::new().unwrap();
        let provider = ChatGptProvider::with_endpoints(temporary.path(), "http://127.0.0.1:9");
        provider.begin_auth(AuthFlow::BrowserPkce).await.unwrap();
        assert_eq!(
            provider
                .complete_auth(AuthResponse::Cancel)
                .await
                .unwrap_err()
                .kind,
            ProviderErrorKind::Cancelled
        );
        let error = provider
            .complete_auth(AuthResponse::AuthorizationCode {
                code: "code".into(),
                state: "state".into(),
            })
            .await
            .unwrap_err();
        assert_eq!(error.code, "missing_oauth_state");
    }

    #[tokio::test]
    async fn stalled_websocket_handshake_times_out() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            std::future::pending::<()>().await;
        });

        let error = tokio::time::timeout(
            Duration::from_millis(250),
            connect_websocket(
                &format!("http://{address}/codex"),
                &ManagedCredential {
                    access_token: "access-token".into(),
                    refresh_token: None,
                    id_token: None,
                    expires_at_millis: None,
                    account_id: "acct-test".into(),
                    has_codex_entitlement: true,
                    subscription_plan: None,
                },
                &test_stream_request(),
                &CancellationToken::new(),
                Duration::from_millis(25),
            ),
        )
        .await
        .expect("handshake should respect its timeout")
        .unwrap_err();
        server.abort();

        assert_eq!(error.kind, ProviderErrorKind::Timeout);
        assert_eq!(error.code, "websocket_connect_timeout");
        assert!(error.retryable);
    }

    #[tokio::test]
    async fn cancelled_websocket_handshake_does_not_retry_or_fallback() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let accepted = Arc::new(AtomicUsize::new(0));
        let accepted_server = accepted.clone();
        let (started, started_receiver) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            accepted_server.fetch_add(1, Ordering::SeqCst);
            let _ = started.send(());
            std::future::pending::<()>().await;
        });

        let temporary = tempfile::TempDir::new().unwrap();
        let provider =
            ChatGptProvider::with_endpoints(temporary.path(), &format!("http://{address}"))
                .with_websocket_connect_timeout(Duration::from_secs(1));
        *provider.credential.write().await = Some(ManagedCredential {
            access_token: "access-token".into(),
            refresh_token: None,
            id_token: None,
            expires_at_millis: None,
            account_id: "acct-test".into(),
            has_codex_entitlement: true,
            subscription_plan: None,
        });
        let cancellation = CancellationToken::new();
        let mut stream = provider
            .stream(test_stream_request(), cancellation.clone())
            .await
            .unwrap();
        started_receiver.await.unwrap();
        cancellation.cancel();
        let error = tokio::time::timeout(Duration::from_millis(250), stream.next())
            .await
            .expect("cancelled handshake should finish promptly")
            .expect("stream should report cancellation")
            .unwrap_err();
        server.abort();

        assert_eq!(error.kind, ProviderErrorKind::Cancelled);
        assert_eq!(accepted.load(Ordering::SeqCst), 1);
        let lane = provider
            .websocket_lanes
            .lock()
            .await
            .values()
            .next()
            .cloned()
            .unwrap();
        assert!(!lane.lock().await.http_fallback);
    }

    #[tokio::test]
    async fn responses_lite_http_uses_lite_contract() {
        let app = axum::Router::new().route(
            "/codex/responses",
            axum::routing::post(
                |headers: axum::http::HeaderMap, axum::Json(body): axum::Json<Value>| async move {
                    assert_eq!(headers["x-openai-internal-codex-responses-lite"], "true");
                    assert_eq!(body["parallel_tool_calls"], false);
                    assert_eq!(body["reasoning"]["context"], "all_turns");
                    assert_eq!(body["store"], false);
                    assert!(body.get("instructions").is_none());
                    assert!(body.get("tools").is_none());
                    assert_eq!(body["input"][0]["type"], "additional_tools");
                    assert_eq!(body["input"][0]["tools"], json!([]));
                    assert_eq!(body["input"][1]["role"], "developer");
                    assert_eq!(body["input"][2]["role"], "user");
                    (
                        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp-lite-http\",\"status\":\"completed\",\"output\":[{\"type\":\"reasoning\",\"id\":\"rs_http\",\"summary\":[],\"encrypted_content\":\"opaque-http-state\"}]}}\n\n",
                    )
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let temporary = tempfile::TempDir::new().unwrap();
        let provider =
            ChatGptProvider::with_endpoints(temporary.path(), &format!("http://{address}"));
        let request = test_stream_request();
        provider
            .responses_lite_models
            .write()
            .await
            .insert(request.model.model.clone());
        let credential = ManagedCredential {
            access_token: "access-token".into(),
            refresh_token: None,
            id_token: None,
            expires_at_millis: None,
            account_id: "acct-test".into(),
            has_codex_entitlement: true,
            subscription_plan: None,
        };
        let (sender, mut receiver) = mpsc::channel(8);
        provider
            .stream_http(credential, request, CancellationToken::new(), sender)
            .await;
        let mut completed = false;
        let mut private_items = 0;
        while let Some(event) = receiver.recv().await {
            let event = event.unwrap();
            if let ProviderStreamEvent::EncryptedReasoning { items, .. } = &event {
                private_items += items.len();
            }
            if let ProviderStreamEvent::Completed { metadata } = event {
                assert_eq!(
                    metadata.provider_request_id.as_deref(),
                    Some("resp-lite-http")
                );
                completed = true;
            }
        }
        server.abort();
        assert!(completed);
        assert_eq!(private_items, 1);
    }

    #[tokio::test]
    async fn responses_websocket_uses_codex_url_and_auth_headers() {
        assert_responses_websocket_contract(false).await;
    }

    #[tokio::test]
    async fn responses_lite_websocket_uses_lite_contract() {
        assert_responses_websocket_contract(true).await;
    }

    async fn assert_responses_websocket_contract(responses_lite: bool) {
        use tokio_tungstenite::accept_hdr_async;
        use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let observed = Arc::new(Mutex::new(
            None::<(
                String,
                String,
                String,
                String,
                String,
                String,
                String,
                String,
            )>,
        ));
        let observed_server = observed.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let callback = |request: &Request, response: Response| {
                *observed_server.lock().unwrap() = Some((
                    request.uri().path().to_owned(),
                    request
                        .headers()
                        .get(AUTHORIZATION)
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default()
                        .to_owned(),
                    request
                        .headers()
                        .get("OpenAI-Beta")
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default()
                        .to_owned(),
                    request
                        .headers()
                        .get("session-id")
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default()
                        .to_owned(),
                    request
                        .headers()
                        .get("thread-id")
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default()
                        .to_owned(),
                    request
                        .headers()
                        .get("ChatGPT-Account-Id")
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default()
                        .to_owned(),
                    request
                        .headers()
                        .get("originator")
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default()
                        .to_owned(),
                    request
                        .headers()
                        .get("x-codex-routing-hint")
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default()
                        .to_owned(),
                ));
                Ok(response)
            };
            let mut websocket = accept_hdr_async(stream, callback).await.unwrap();
            let Some(Ok(Message::Text(frame))) = websocket.next().await else {
                panic!("expected response.create frame");
            };
            let payload: Value = serde_json::from_str(&frame).unwrap();
            assert_eq!(payload["type"], "response.create");
            if responses_lite {
                assert_eq!(payload["parallel_tool_calls"], false);
                assert_eq!(payload["reasoning"]["context"], "all_turns");
                assert!(payload.get("instructions").is_none());
                assert!(payload.get("tools").is_none());
                assert_eq!(payload["input"][0]["type"], "additional_tools");
                assert_eq!(payload["input"][1]["role"], "developer");
                assert_eq!(payload["input"][2]["role"], "user");
                assert_eq!(
                    payload["client_metadata"]["ws_request_header_x_openai_internal_codex_responses_lite"],
                    "true"
                );
            } else {
                assert_eq!(payload["parallel_tool_calls"], true);
                assert!(
                    payload["instructions"]
                        .as_str()
                        .unwrap()
                        .contains("Be concise")
                );
                assert_eq!(payload["input"][0]["role"], "user");
                assert!(
                    payload["client_metadata"]
                        .get("ws_request_header_x_openai_internal_codex_responses_lite")
                        .is_none()
                );
            }
            assert_eq!(payload["store"], false);
            assert_eq!(payload["service_tier"], "priority");
            assert_eq!(payload["client_metadata"]["session_id"], "test");
            assert_eq!(payload["client_metadata"]["thread_id"], "test");
            websocket
                .send(Message::Text(
                    json!({"type":"response.output_text.delta","delta":"hello"})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            websocket
                .send(Message::Text(
                    json!({
                        "type":"response.completed",
                        "response":{"id":"resp-ws","status":"completed","service_tier":"priority","output":[{"type":"reasoning","id":"rs_ws","summary":[],"encrypted_content":"opaque-ws-state"}],"usage":{"input_tokens":11,"output_tokens":3,"total_tokens":14}}
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
        });

        let temporary = tempfile::TempDir::new().unwrap();
        let provider =
            ChatGptProvider::with_endpoints(temporary.path(), &format!("http://{address}"));
        *provider.credential.write().await = Some(ManagedCredential {
            access_token: "access-token".into(),
            refresh_token: None,
            id_token: None,
            expires_at_millis: None,
            account_id: "acct-test".into(),
            has_codex_entitlement: true,
            subscription_plan: None,
        });
        let mut request = test_stream_request();
        request.service_tier = Some("priority".into());
        if responses_lite {
            provider
                .responses_lite_models
                .write()
                .await
                .insert(request.model.model.clone());
        }
        let mut stream = provider
            .stream(request, CancellationToken::new())
            .await
            .unwrap();
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event.unwrap());
        }
        assert!(matches!(
            events.first(),
            Some(crate::ProviderStreamEvent::TextDelta { delta }) if delta == "hello"
        ));
        assert!(
            matches!(&events[1], ProviderStreamEvent::EncryptedReasoning { items, replace: true } if items.len() == 1)
        );
        assert!(matches!(
            events.last(),
            Some(crate::ProviderStreamEvent::Completed { metadata })
                if metadata.provider_request_id.as_deref() == Some("resp-ws")
                    && metadata.usage.input_tokens == Some(11)
                    && metadata.usage.output_tokens == Some(3)
                    && metadata.usage.total_tokens == Some(14)
                    && metadata.usage.provider_usage["service_tier"] == "priority"
        ));
        server.await.unwrap();
        assert_eq!(
            observed.lock().unwrap().as_ref(),
            Some(&(
                "/codex/responses".into(),
                "Bearer access-token".into(),
                "responses_websockets=2026-02-06".into(),
                "test".into(),
                "test".into(),
                "acct-test".into(),
                "cagent".into(),
                "model=gpt-test;tier=priority".into(),
            ))
        );
    }

    #[test]
    fn codex_routing_hint_includes_only_an_explicit_service_tier() {
        let mut request = test_stream_request();
        assert_eq!(codex_routing_hint(&request), "model=gpt-test");

        request.service_tier = Some("priority".into());
        assert_eq!(codex_routing_hint(&request), "model=gpt-test;tier=priority");
    }
}
