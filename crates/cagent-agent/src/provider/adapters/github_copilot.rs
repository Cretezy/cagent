use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use serde_json::Value;
use tokio::sync::{RwLock, mpsc};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

use crate::{
    AuthChallenge, AuthFlow, AuthResponse, AuthState, CredentialSource, CredentialStore,
    ManagedCredential, MessageRole, ModelBackend, ModelCapabilities, ModelCatalog, ModelDescriptor,
    ModelDiscoverySource, ModelInput, ModelRequest, Provider, ProviderDescriptor, ProviderError,
    ProviderErrorKind, ProviderFuture, ProviderStream,
};

const CLIENT_ID: &str = "Iv23liDNiqrpzcTQaim2";
const GITHUB_BASE_URL: &str = "https://github.com";
const GITHUB_API_URL: &str = "https://api.github.com";
const COPILOT_BASE_URL: &str = "https://api.githubcopilot.com";
const GITHUB_REST_API_VERSION: &str = "2026-03-10";
const COPILOT_API_VERSION: &str = "2026-06-01";
const USER_AGENT_VALUE: &str = concat!("cagent/", env!("CARGO_PKG_VERSION"));
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone, Debug)]
struct PendingDeviceAuth {
    device_code: String,
    interval: Duration,
    expires_at: Instant,
    cancellation: CancellationToken,
}

/// Compatibility-sensitive GitHub Copilot subscription adapter.
#[derive(Clone)]
pub struct GitHubCopilotProvider {
    client: reqwest::Client,
    github_base_url: String,
    github_api_url: String,
    copilot_base_override: Option<String>,
    credentials: CredentialStore,
    credential: Arc<RwLock<Option<ManagedCredential>>>,
    pending_device: Arc<Mutex<Option<PendingDeviceAuth>>>,
    successful_backends: Arc<RwLock<HashMap<String, ModelBackend>>>,
}

impl std::fmt::Debug for GitHubCopilotProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GitHubCopilotProvider")
            .field("github_base_url", &self.github_base_url)
            .field("github_api_url", &self.github_api_url)
            .finish_non_exhaustive()
    }
}

impl GitHubCopilotProvider {
    #[must_use]
    pub fn new(data_dir: &Path) -> Self {
        let credentials = CredentialStore::new(data_dir, "default", "github-copilot");
        let credential = credentials.load().ok().flatten();
        Self {
            client: reqwest::Client::new(),
            github_base_url: GITHUB_BASE_URL.into(),
            github_api_url: GITHUB_API_URL.into(),
            copilot_base_override: None,
            credentials,
            credential: Arc::new(RwLock::new(credential)),
            pending_device: Arc::new(Mutex::new(None)),
            successful_backends: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    #[cfg(test)]
    fn with_endpoints(data_dir: &Path, base_url: &str) -> Self {
        let mut provider = Self::new(data_dir);
        provider.github_base_url = base_url.trim_end_matches('/').into();
        provider.github_api_url = base_url.trim_end_matches('/').into();
        provider.copilot_base_override = Some(base_url.trim_end_matches('/').into());
        provider
    }

    async fn save_credential(&self, credential: ManagedCredential) -> Result<(), ProviderError> {
        self.credentials.save(&credential)?;
        *self.credential.write().await = Some(credential);
        Ok(())
    }

    async fn usable_credential(&self) -> Result<ManagedCredential, ProviderError> {
        self.credential.read().await.clone().ok_or_else(|| {
            auth_error(
                "missing_copilot_subscription",
                "GitHub Copilot is not connected",
            )
        })
    }

    async fn github_login(&self, github_token: &str) -> Result<String, ProviderError> {
        let response = self
            .client
            .get(format!("{}/user", self.github_api_url))
            .header(ACCEPT, "application/vnd.github+json")
            .header(USER_AGENT, USER_AGENT_VALUE)
            .header("X-GitHub-Api-Version", GITHUB_REST_API_VERSION)
            .bearer_auth(github_token)
            .send()
            .await
            .map_err(|error| transport_error(&error))?;
        if !response.status().is_success() {
            return Err(copilot_http_error(response).await);
        }
        let payload = decode_json_response(response, "invalid_github_user").await?;
        Ok(required(&payload, "login")?.to_owned())
    }

    fn copilot_base_url(&self) -> String {
        if let Some(base_url) = &self.copilot_base_override {
            return base_url.clone();
        }
        COPILOT_BASE_URL.into()
    }

    fn copilot_request(
        builder: reqwest::RequestBuilder,
        request: &ModelRequest,
        token: &str,
    ) -> reqwest::RequestBuilder {
        builder
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .header(ACCEPT, "application/json")
            .header(CONTENT_TYPE, "application/json")
            .header(USER_AGENT, USER_AGENT_VALUE)
            .header("X-GitHub-Api-Version", COPILOT_API_VERSION)
            .header("Openai-Intent", "conversation-edits")
            .header("X-Initiator", request_initiator(request))
    }

    fn inference_request(
        &self,
        base_url: &str,
        request: &ModelRequest,
        backend: ModelBackend,
    ) -> Result<reqwest::RequestBuilder, ProviderError> {
        Ok(match backend {
            ModelBackend::OpenAiResponses => self
                .client
                .post(format!("{base_url}/responses"))
                .json(&crate::provider::codecs::openai_responses::encode(
                    request,
                    &crate::provider::codecs::openai_responses::PromptCachePolicy::Disabled,
                )?),
            ModelBackend::OpenAiCompatible => self
                .client
                .post(format!("{base_url}/chat/completions"))
                .json(&crate::provider::codecs::openai_chat::encode(
                    request,
                    &crate::provider::codecs::openai_chat::PromptCachePolicy::Disabled,
                )?),
            ModelBackend::AnthropicMessages => self
                .client
                .post(format!("{base_url}/v1/messages"))
                .header("anthropic-version", "2023-06-01")
                .header("anthropic-beta", "interleaved-thinking-2025-05-14")
                .json(&crate::provider::codecs::anthropic_messages::encode(
                    request,
                    crate::provider::codecs::anthropic_messages::PromptCachePolicy::Disabled,
                )?),
            ModelBackend::Gemini => {
                return Err(ProviderError::configuration(
                    "GitHub Copilot does not expose the Gemini GenerateContent backend",
                ));
            }
        })
    }

    async fn run_with_fallback(
        self,
        request: ModelRequest,
        backends: Vec<ModelBackend>,
        token: String,
        sender: mpsc::Sender<Result<crate::ProviderStreamEvent, ProviderError>>,
        cancellation: CancellationToken,
    ) {
        let base_url = self.copilot_base_url();
        for (index, backend) in backends.iter().copied().enumerate() {
            let response = match self.inference_request(&base_url, &request, backend) {
                Ok(builder) => {
                    tokio::select! {
                        biased;
                        () = cancellation.cancelled() => return,
                        response = Self::copilot_request(builder, &request, &token).send() => response,
                    }
                }
                Err(error) => {
                    let _ = tokio::select! {
                        biased;
                        () = cancellation.cancelled() => return,
                        result = sender.send(Err(error)) => result,
                    };
                    return;
                }
            };
            let response = match response {
                Ok(response) => response,
                Err(error) => {
                    let _ = tokio::select! {
                        biased;
                        () = cancellation.cancelled() => return,
                        result = sender.send(Err(transport_error(&error))) => result,
                    };
                    return;
                }
            };
            if !response.status().is_success() {
                let error = tokio::select! {
                    biased;
                    () = cancellation.cancelled() => return,
                    error = backend_http_error(response, backend) => error,
                };
                if endpoint_mismatch(&error) && index + 1 < backends.len() {
                    tracing::debug!(
                        model = %request.model.model,
                        ?backend,
                        "Copilot rejected an advertised endpoint; trying the next one"
                    );
                    continue;
                }
                let _ = tokio::select! {
                    biased;
                    () = cancellation.cancelled() => return,
                    result = sender.send(Err(error)) => result,
                };
                return;
            }

            let (attempt_sender, mut attempt_receiver) = mpsc::channel(64);
            match backend {
                ModelBackend::OpenAiResponses => {
                    tokio::spawn(crate::provider::codecs::openai_responses::decode_stream(
                        response,
                        attempt_sender,
                        cancellation.clone(),
                    ));
                }
                ModelBackend::OpenAiCompatible => {
                    tokio::spawn(crate::provider::codecs::openai_chat::decode_stream(
                        response,
                        attempt_sender,
                        cancellation.clone(),
                    ));
                }
                ModelBackend::AnthropicMessages => {
                    tokio::spawn(crate::provider::codecs::anthropic_messages::decode_stream(
                        response,
                        attempt_sender,
                        cancellation.clone(),
                    ));
                }
                ModelBackend::Gemini => {
                    unreachable!("unsupported Copilot backend was rejected before streaming")
                }
            }

            let mut emitted = false;
            let mut retry_endpoint = false;
            while let Some(event) = tokio::select! {
                biased;
                () = cancellation.cancelled() => return,
                event = attempt_receiver.recv() => event,
            } {
                if let Err(error) = &event
                    && !emitted
                    && endpoint_mismatch(error)
                    && index + 1 < backends.len()
                {
                    tracing::debug!(
                        model = %request.model.model,
                        ?backend,
                        "Copilot stream rejected an advertised endpoint; trying the next one"
                    );
                    retry_endpoint = true;
                    break;
                }
                if event.is_ok() && !emitted {
                    emitted = true;
                    self.successful_backends
                        .write()
                        .await
                        .insert(request.model.model.clone(), backend);
                }
                let sent = tokio::select! {
                    biased;
                    () = cancellation.cancelled() => return,
                    result = sender.send(event) => result,
                };
                if sent.is_err() {
                    return;
                }
            }
            if retry_endpoint {
                continue;
            }
            return;
        }
    }
}

impl Provider for GitHubCopilotProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        static DESCRIPTOR: std::sync::LazyLock<ProviderDescriptor> =
            std::sync::LazyLock::new(|| ProviderDescriptor {
                id: "github-copilot".into(),
                display_name: "GitHub Copilot".into(),
                default_model_backend: None,
                supported_model_backends: vec![
                    ModelBackend::OpenAiResponses,
                    ModelBackend::OpenAiCompatible,
                    ModelBackend::AnthropicMessages,
                ],
                model_discovery: ModelDiscoverySource::ProviderApi,
                credential_source: CredentialSource::Subscription,
                credential_environment_variable: None,
                supports_managed_api_key: false,
                auth_flows: vec![AuthFlow::DeviceCode],
            });
        &DESCRIPTOR
    }

    fn auth_state(&self) -> ProviderFuture<'_, Result<AuthState, ProviderError>> {
        Box::pin(async move {
            Ok(match self.credential.read().await.as_ref() {
                None => AuthState::Missing,
                Some(credential) => AuthState::Connected {
                    detail: format!("GitHub user {}", credential.account_id),
                },
            })
        })
    }

    fn begin_auth(
        &self,
        flow: AuthFlow,
    ) -> ProviderFuture<'_, Result<AuthChallenge, ProviderError>> {
        Box::pin(async move {
            if flow != AuthFlow::DeviceCode {
                return Err(ProviderError::configuration(
                    "GitHub Copilot supports device authentication only",
                ));
            }
            if let Some(pending) = self
                .pending_device
                .lock()
                .map_err(|_| ProviderError::configuration("authentication state lock failed"))?
                .take()
            {
                pending.cancellation.cancel();
            }
            let response = self
                .client
                .post(format!("{}/login/device/code", self.github_base_url))
                .header(ACCEPT, "application/json")
                .header(USER_AGENT, USER_AGENT_VALUE)
                .form(&[("client_id", CLIENT_ID), ("scope", "read:user")])
                .send()
                .await
                .map_err(|error| transport_error(&error))?;
            if !response.status().is_success() {
                return Err(copilot_http_error(response).await);
            }
            let payload = decode_json_response(response, "invalid_device_response").await?;
            let device_code = required(&payload, "device_code")?.to_owned();
            let user_code = required(&payload, "user_code")?.to_owned();
            let verification_url = required(&payload, "verification_uri")?.to_owned();
            let interval_seconds = payload
                .get("interval")
                .and_then(Value::as_u64)
                .unwrap_or(DEFAULT_POLL_INTERVAL.as_secs())
                .max(1);
            let expires_in = payload
                .get("expires_in")
                .and_then(Value::as_u64)
                .unwrap_or(15 * 60)
                .max(1);
            *self
                .pending_device
                .lock()
                .map_err(|_| ProviderError::configuration("authentication state lock failed"))? =
                Some(PendingDeviceAuth {
                    device_code: device_code.clone(),
                    interval: Duration::from_secs(interval_seconds),
                    expires_at: Instant::now() + Duration::from_secs(expires_in),
                    cancellation: CancellationToken::new(),
                });
            Ok(AuthChallenge::Device {
                verification_url,
                user_code,
                device_code,
                interval_seconds,
            })
        })
    }

    #[allow(clippy::too_many_lines)]
    fn complete_auth(
        &self,
        response: AuthResponse,
    ) -> ProviderFuture<'_, Result<(), ProviderError>> {
        Box::pin(async move {
            match response {
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
                                "no GitHub device authentication is pending",
                            )
                        })?;
                    if device_code != pending.device_code {
                        return Err(auth_error(
                            "device_code_mismatch",
                            "device code did not match the pending authentication",
                        ));
                    }
                    let mut interval = pending.interval;
                    let github_token = loop {
                        tokio::select! {
                            () = pending.cancellation.cancelled() => return Err(ProviderError::cancelled()),
                            () = tokio::time::sleep(interval) => {}
                        }
                        if Instant::now() >= pending.expires_at {
                            return Err(ProviderError::timeout(
                                "device_auth_timeout",
                                "GitHub device authentication expired",
                            ));
                        }
                        let response = self
                            .client
                            .post(format!("{}/login/oauth/access_token", self.github_base_url))
                            .header(ACCEPT, "application/json")
                            .header(USER_AGENT, USER_AGENT_VALUE)
                            .form(&[
                                ("client_id", CLIENT_ID),
                                ("device_code", device_code.as_str()),
                                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                            ])
                            .send()
                            .await
                            .map_err(|error| transport_error(&error))?;
                        let status = response.status();
                        let payload =
                            decode_json_response(response, "invalid_device_token").await?;
                        if let Some(token) = payload.get("access_token").and_then(Value::as_str) {
                            break token.to_owned();
                        }
                        let error = payload
                            .get("error")
                            .and_then(Value::as_str)
                            .unwrap_or("device_authorization_failed");
                        match error {
                            "authorization_pending" => {}
                            "slow_down" => {
                                interval =
                                    payload.get("interval").and_then(Value::as_u64).map_or_else(
                                        || interval + Duration::from_secs(5),
                                        Duration::from_secs,
                                    );
                            }
                            "expired_token" => {
                                return Err(ProviderError::timeout(
                                    "device_auth_timeout",
                                    "GitHub device authentication expired",
                                ));
                            }
                            "access_denied" => {
                                return Err(auth_error(
                                    "device_auth_denied",
                                    "GitHub device authentication was denied",
                                ));
                            }
                            _ => {
                                return Err(auth_error(
                                    error,
                                    payload
                                        .get("error_description")
                                        .and_then(Value::as_str)
                                        .unwrap_or_else(|| {
                                            status.canonical_reason().unwrap_or(error)
                                        }),
                                ));
                            }
                        }
                    };
                    let account_id = self.github_login(&github_token).await?;
                    self.save_credential(ManagedCredential {
                        access_token: github_token.clone(),
                        refresh_token: Some(github_token),
                        id_token: None,
                        expires_at_millis: None,
                        account_id,
                        has_codex_entitlement: true,
                        subscription_plan: None,
                    })
                    .await?;
                    *self.pending_device.lock().map_err(|_| {
                        ProviderError::configuration("authentication state lock failed")
                    })? = None;
                    Ok(())
                }
                AuthResponse::Cancel => {
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
                AuthResponse::AuthorizationCode { .. } | AuthResponse::OAuthError { .. } => Err(
                    ProviderError::configuration("GitHub Copilot uses device authentication"),
                ),
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

    fn discover_models(&self) -> Option<ProviderFuture<'_, Result<ModelCatalog, ProviderError>>> {
        Some(Box::pin(async move {
            let credential = self.usable_credential().await?;
            let base_url = self.copilot_base_url();
            let response = self
                .client
                .get(format!("{base_url}/models"))
                .header(ACCEPT, "application/json")
                .header(USER_AGENT, USER_AGENT_VALUE)
                .header("X-GitHub-Api-Version", COPILOT_API_VERSION)
                .bearer_auth(&credential.access_token)
                .send()
                .await
                .map_err(|error| transport_error(&error))?;
            if !response.status().is_success() {
                return Err(copilot_http_error(response).await);
            }
            let payload = decode_json_response(response, "invalid_models_response").await?;
            parse_copilot_models(&payload)
        }))
    }

    fn model_backends(&self, model: &ModelDescriptor) -> Vec<ModelBackend> {
        let advertised = advertised_backends(&model.raw_metadata);
        if advertised.is_empty() {
            model.backend.into_iter().collect()
        } else {
            advertised
        }
    }

    fn stream(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> ProviderFuture<'_, Result<ProviderStream, ProviderError>> {
        Box::pin(async move {
            if request.model.provider != "github-copilot" {
                return Err(ProviderError::configuration(
                    "GitHub Copilot adapter received another provider's model",
                ));
            }
            let mut backends = request.backend_candidates.clone();
            if backends.is_empty()
                && let Some(backend) = request.backend
            {
                backends.push(backend);
            }
            if backends.is_empty() {
                return Err(ProviderError::configuration(
                    "GitHub Copilot model has no supported endpoint",
                ));
            }
            if let Some(successful) = self
                .successful_backends
                .read()
                .await
                .get(&request.model.model)
                .copied()
                && let Some(index) = backends.iter().position(|backend| *backend == successful)
            {
                backends.swap(0, index);
            }
            let credential = self.usable_credential().await?;
            let (sender, receiver) = mpsc::channel(64);
            tokio::spawn(self.clone().run_with_fallback(
                request,
                backends,
                credential.access_token,
                sender,
                cancellation,
            ));
            Ok(Box::pin(ReceiverStream::new(receiver)) as ProviderStream)
        })
    }

    fn refresh_credentials(
        &self,
        cancellation: CancellationToken,
    ) -> ProviderFuture<'_, Result<bool, ProviderError>> {
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(ProviderError::cancelled());
            }
            self.credential.read().await.as_ref().ok_or_else(|| {
                auth_error(
                    "missing_copilot_subscription",
                    "GitHub Copilot is not connected",
                )
            })?;
            Ok(true)
        })
    }
}

#[allow(clippy::too_many_lines)]
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

fn parse_copilot_models(payload: &Value) -> Result<ModelCatalog, ProviderError> {
    let entries = payload
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            ProviderError::protocol(
                "invalid_models_response",
                "Copilot model response has no data array",
            )
        })?;
    let mut candidates = Vec::new();
    for entry in entries {
        let Some(id) = entry.get("id").and_then(Value::as_str) else {
            continue;
        };
        let policy = entry.pointer("/policy/state").and_then(Value::as_str);
        if policy == Some("disabled")
            || entry
                .pointer("/capabilities/supports/tool_calls")
                .and_then(Value::as_bool)
                != Some(true)
        {
            continue;
        }
        let Some(backend) = advertised_backends(entry).into_iter().next() else {
            continue;
        };
        let efforts = entry
            .pointer("/capabilities/supports/reasoning_effort")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| value.as_str().map(str::to_owned))
                    .collect::<Vec<_>>()
            })
            .filter(|values| !values.is_empty())
            .or_else(|| {
                (entry
                    .pointer("/capabilities/supports/adaptive_thinking")
                    .and_then(Value::as_bool)
                    == Some(true)
                    || entry
                        .pointer("/capabilities/supports/max_thinking_budget")
                        .and_then(Value::as_u64)
                        .is_some())
                .then(|| vec!["low".into(), "medium".into(), "high".into()])
            });
        let model = ModelDescriptor {
            id: id.into(),
            display_name: entry
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(id)
                .into(),
            capabilities: ModelCapabilities {
                context_window: entry
                    .pointer("/capabilities/limits/max_context_window_tokens")
                    .and_then(Value::as_u64)
                    .or_else(|| {
                        entry
                            .pointer("/capabilities/limits/max_prompt_tokens")
                            .and_then(Value::as_u64)
                    }),
                supports_streaming: Some(
                    entry
                        .pointer("/capabilities/supports/streaming")
                        .and_then(Value::as_bool)
                        .unwrap_or(true),
                ),
                supports_tools: Some(true),
                supports_structured_output: entry
                    .pointer("/capabilities/supports/structured_output")
                    .and_then(Value::as_bool),
                supports_text_input: Some(true),
                supports_image_input: entry
                    .pointer("/capabilities/supports/vision")
                    .and_then(Value::as_bool),
                supports_text_output: Some(true),
                supports_fast_mode: None,
                reasoning_control: efforts.as_ref().map(|_| crate::ReasoningControl::Effort),
                reasoning_efforts: efforts,
            },
            backend: Some(backend),
            raw_metadata: entry.clone(),
        };
        let picker_enabled =
            entry.get("model_picker_enabled").and_then(Value::as_bool) == Some(true);
        candidates.push((model, picker_enabled, policy == Some("enabled")));
    }
    let has_picker_models = candidates.iter().any(|(_, picker, _)| *picker);
    let models = candidates
        .into_iter()
        .filter_map(|(model, picker, policy_enabled)| {
            (picker || (!has_picker_models && policy_enabled)).then_some(model)
        })
        .collect::<Vec<_>>();
    if models.is_empty() {
        return Err(ProviderError::protocol(
            "no_copilot_models",
            "GitHub Copilot returned no picker-enabled or policy-enabled tool models",
        ));
    }
    Ok(ModelCatalog {
        provider: "github-copilot".into(),
        models,
        version: payload
            .get("version")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

fn request_initiator(request: &ModelRequest) -> &'static str {
    if matches!(
        request.input.last(),
        Some(ModelInput::Message {
            role: MessageRole::User,
            ..
        })
    ) {
        "user"
    } else {
        "agent"
    }
}

fn advertised_backends(metadata: &Value) -> Vec<ModelBackend> {
    let metadata = metadata.get("provider").unwrap_or(metadata);
    let Some(endpoints) = metadata
        .get("supported_endpoints")
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    let has = |endpoint: &str| {
        endpoints
            .iter()
            .any(|value| value.as_str() == Some(endpoint))
    };
    let mut backends = Vec::new();
    if has("/v1/messages") {
        backends.push(ModelBackend::AnthropicMessages);
    }
    if has("/responses") || has("ws:/responses") {
        backends.push(ModelBackend::OpenAiResponses);
    }
    if has("/chat/completions") {
        backends.push(ModelBackend::OpenAiCompatible);
    }
    backends
}

fn endpoint_mismatch(error: &ProviderError) -> bool {
    error.code == "model_not_supported"
        || (error.code == "invalid_request_error"
            && error.message.to_ascii_lowercase().contains("model")
            && error.message.to_ascii_lowercase().contains("not supported"))
}

async fn backend_http_error(response: reqwest::Response, backend: ModelBackend) -> ProviderError {
    match backend {
        ModelBackend::AnthropicMessages => {
            crate::provider::codecs::anthropic_messages::decode_http_error(response).await
        }
        ModelBackend::OpenAiResponses => {
            crate::provider::codecs::openai_responses::decode_http_error(response).await
        }
        ModelBackend::OpenAiCompatible => {
            crate::provider::codecs::openai_chat::decode_http_error(response).await
        }
        ModelBackend::Gemini => {
            ProviderError::configuration("GitHub Copilot does not expose Gemini")
        }
    }
}

fn required<'a>(value: &'a Value, name: &str) -> Result<&'a str, ProviderError> {
    value
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| ProviderError::protocol("invalid_auth_response", format!("missing {name}")))
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

fn transport_error(error: &reqwest::Error) -> ProviderError {
    ProviderError {
        kind: if error.is_timeout() {
            ProviderErrorKind::Timeout
        } else {
            ProviderErrorKind::Connection
        },
        code: "copilot_transport".into(),
        message: error.to_string(),
        retryable: true,
        retry_after_millis: None,
        status: error.status().map(|status| status.as_u16()),
        metadata: BTreeMap::new(),
    }
}

async fn copilot_http_error(response: reqwest::Response) -> ProviderError {
    let status = response.status();
    let retry_after_millis = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(|seconds| seconds.saturating_mul(1_000));
    let body = response.text().await.unwrap_or_default();
    let message = simd_json::serde::from_slice::<Value>(&mut body.into_bytes())
        .ok()
        .and_then(|value| {
            value
                .get("message")
                .or_else(|| value.pointer("/error/message"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .filter(|message| !message.is_empty())
        .unwrap_or_else(|| {
            status
                .canonical_reason()
                .unwrap_or("Copilot request failed")
                .into()
        });
    let authentication = matches!(status.as_u16(), 401 | 403);
    ProviderError {
        kind: if authentication {
            ProviderErrorKind::Authentication
        } else if status.as_u16() == 429 {
            ProviderErrorKind::RateLimit
        } else if status.is_server_error() {
            ProviderErrorKind::Server
        } else {
            ProviderErrorKind::InvalidRequest
        },
        code: if authentication {
            "copilot_entitlement".into()
        } else {
            format!("copilot_http_{}", status.as_u16())
        },
        message,
        retryable: status.as_u16() == 429 || status.is_server_error(),
        retry_after_millis,
        status: Some(status.as_u16()),
        metadata: BTreeMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ModelRef;
    use futures_util::StreamExt as _;
    use serde_json::json;

    #[test]
    fn descriptor_is_device_only_and_supports_all_current_backends() {
        let temporary = tempfile::TempDir::new().unwrap();
        let provider = GitHubCopilotProvider::new(temporary.path());
        let descriptor = provider.descriptor();
        assert_eq!(descriptor.id, "github-copilot");
        assert_eq!(descriptor.auth_flows, vec![AuthFlow::DeviceCode]);
        assert_eq!(descriptor.supported_model_backends.len(), 3);
    }

    #[test]
    fn parses_picker_models_and_routes_each_supported_endpoint() {
        let catalog = parse_copilot_models(&json!({
            "version": "test",
            "data": [
                model("claude", "/v1/messages", true, "enabled"),
                model("gpt", "/responses", true, "enabled"),
                model("gemini", "/chat/completions", true, "enabled"),
                model("hidden", "/responses", false, "enabled"),
                model("blocked", "/responses", true, "disabled")
            ]
        }))
        .unwrap();
        assert_eq!(catalog.version.as_deref(), Some("test"));
        assert_eq!(catalog.models.len(), 3);
        assert_eq!(
            catalog.models[0].backend,
            Some(ModelBackend::AnthropicMessages)
        );
        assert_eq!(
            catalog.models[1].backend,
            Some(ModelBackend::OpenAiResponses)
        );
        assert_eq!(
            catalog.models[2].backend,
            Some(ModelBackend::OpenAiCompatible)
        );
    }

    #[test]
    fn falls_back_to_enabled_policy_when_picker_flags_are_empty() {
        let catalog = parse_copilot_models(&json!({
            "data": [model("available", "/responses", false, "enabled")]
        }))
        .unwrap();
        assert_eq!(catalog.models[0].id, "available");
    }

    #[test]
    fn preserves_all_advertised_backends_in_provider_preference_order() {
        let mut entry = model("multi-endpoint", "/responses", true, "enabled");
        entry["supported_endpoints"] = json!(["/chat/completions", "/responses"]);
        assert_eq!(
            advertised_backends(&entry),
            vec![
                ModelBackend::OpenAiResponses,
                ModelBackend::OpenAiCompatible
            ]
        );
    }

    #[test]
    fn reads_advertised_backends_from_merged_cached_metadata() {
        let metadata = json!({
            "provider": {
                "supported_endpoints": ["/chat/completions", "/v1/messages"]
            },
            "models_dev": {}
        });
        assert_eq!(
            advertised_backends(&metadata),
            vec![
                ModelBackend::AnthropicMessages,
                ModelBackend::OpenAiCompatible
            ]
        );
    }

    #[test]
    fn retries_only_errors_that_mean_the_endpoint_rejected_the_model() {
        let mismatch = ProviderError {
            kind: ProviderErrorKind::InvalidRequest,
            code: "invalid_request_error".into(),
            message: "The requested model is not supported.".into(),
            retryable: false,
            retry_after_millis: None,
            status: None,
            metadata: BTreeMap::new(),
        };
        assert!(endpoint_mismatch(&mismatch));
        assert!(!endpoint_mismatch(&ProviderError::configuration(
            "invalid tool schema"
        )));
    }

    #[tokio::test]
    async fn retries_a_first_stream_endpoint_rejection_and_remembers_success() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let (path_sender, mut path_receiver) = mpsc::channel(2);
        let server = tokio::spawn(async move {
            for body in [
                "data: {\"type\":\"error\",\"error\":{\"code\":\"model_not_supported\",\"message\":\"The requested model is not supported.\"}}\n\n".to_owned(),
                format!(
                    "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                    json!({"id":"request_1","choices":[{"delta":{"content":"ok"}}]}),
                    json!({"choices":[{"delta":{},"finish_reason":"stop"}]})
                ),
            ] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = vec![0; 16_384];
                let length = socket.read(&mut request).await.unwrap();
                let request = String::from_utf8_lossy(&request[..length]);
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap()
                    .to_owned();
                path_sender.send(path).await.unwrap();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });

        let temporary = tempfile::TempDir::new().unwrap();
        let provider = GitHubCopilotProvider::with_endpoints(temporary.path(), &base_url);
        *provider.credential.write().await = Some(ManagedCredential {
            access_token: "test-token".into(),
            refresh_token: None,
            id_token: None,
            expires_at_millis: None,
            account_id: "octocat".into(),
            has_codex_entitlement: true,
            subscription_plan: None,
        });
        let request = ModelRequest {
            request_id: crate::RequestId::new(),
            attempt_id: crate::AttemptId::new(),
            model: ModelRef::parse("github-copilot/multi-endpoint").unwrap(),
            backend: Some(ModelBackend::OpenAiResponses),
            backend_candidates: vec![
                ModelBackend::OpenAiResponses,
                ModelBackend::OpenAiCompatible,
            ],
            effort: None,
            service_tier: None,
            input: vec![ModelInput::Message {
                role: MessageRole::User,
                content: "hello".into(),
            }],
            tools: Vec::new(),
            stable_prompt: Vec::new(),
            prompt_cache: None,
            response_transport_continuation: None,
            allow_parallel_tools: true,
            structured_output: None,
        };
        let events = provider
            .stream(request, CancellationToken::new())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        server.await.unwrap();

        assert_eq!(path_receiver.recv().await.as_deref(), Some("/responses"));
        assert_eq!(
            path_receiver.recv().await.as_deref(),
            Some("/chat/completions")
        );
        assert!(events.iter().any(
            |event| matches!(event, crate::ProviderStreamEvent::TextDelta { delta } if delta == "ok")
        ));
        assert_eq!(
            provider
                .successful_backends
                .read()
                .await
                .get("multi-endpoint"),
            Some(&ModelBackend::OpenAiCompatible)
        );
    }

    #[tokio::test]
    async fn rejects_browser_auth_without_network_access() {
        let temporary = tempfile::TempDir::new().unwrap();
        let provider =
            GitHubCopilotProvider::with_endpoints(temporary.path(), "http://127.0.0.1:9");
        let error = provider
            .begin_auth(AuthFlow::BrowserPkce)
            .await
            .unwrap_err();
        assert!(error.message.contains("device authentication only"));
    }

    #[test]
    fn uses_the_standard_copilot_api_for_github_com() {
        let temporary = tempfile::TempDir::new().unwrap();
        let provider = GitHubCopilotProvider::new(temporary.path());
        assert_eq!(provider.copilot_base_url(), COPILOT_BASE_URL);
    }

    #[test]
    fn copilot_codecs_do_not_inherit_native_cache_fields() {
        let request = ModelRequest {
            request_id: crate::RequestId::new(),
            attempt_id: crate::AttemptId::new(),
            model: ModelRef::parse("github-copilot/gpt-5.6-sol").unwrap(),
            backend: None,
            backend_candidates: Vec::new(),
            effort: None,
            service_tier: None,
            input: vec![ModelInput::Message {
                role: MessageRole::User,
                content: "hello".into(),
            }],
            tools: Vec::new(),
            stable_prompt: vec![crate::StablePromptPart {
                identity: "core".into(),
                content: "stable".into(),
            }],
            prompt_cache: Some(crate::PromptCacheRequest {
                key: "must-not-leak".into(),
                scope: crate::PromptCacheScope::Conversation,
            }),
            response_transport_continuation: None,
            allow_parallel_tools: true,
            structured_output: None,
        };
        let responses = crate::provider::codecs::openai_responses::encode(
            &request,
            &crate::provider::codecs::openai_responses::PromptCachePolicy::Disabled,
        )
        .unwrap();
        let chat = crate::provider::codecs::openai_chat::encode(
            &request,
            &crate::provider::codecs::openai_chat::PromptCachePolicy::Disabled,
        )
        .unwrap();
        let anthropic = crate::provider::codecs::anthropic_messages::encode(
            &request,
            crate::provider::codecs::anthropic_messages::PromptCachePolicy::Disabled,
        )
        .unwrap();

        for body in [&responses, &chat, &anthropic] {
            assert!(body.get("prompt_cache_key").is_none());
            assert!(body.get("prompt_cache_options").is_none());
            assert!(body.get("prompt_cache_retention").is_none());
            assert!(body.get("cache_control").is_none());
            assert!(body.get("session_id").is_none());
        }
        assert!(anthropic.pointer("/system/0/cache_control").is_none());
    }

    fn model(id: &str, endpoint: &str, picker: bool, policy: &str) -> Value {
        json!({
            "id": id,
            "name": id,
            "model_picker_enabled": picker,
            "supported_endpoints": [endpoint],
            "policy": { "state": policy },
            "capabilities": {
                "limits": { "max_prompt_tokens": 128_000 },
                "supports": {
                    "tool_calls": true,
                    "streaming": true,
                    "reasoning_effort": ["low", "high"]
                }
            }
        })
    }
}
