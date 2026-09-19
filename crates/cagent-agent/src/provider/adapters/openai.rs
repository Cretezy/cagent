use futures_util::{SinkExt, StreamExt as _};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::sync::Once;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};

use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{Error as WebSocketError, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{
    AuthState, CredentialSource, FinishReason, MessageRole, ModelBackend, ModelCapabilities,
    ModelCatalog, ModelDescriptor, ModelDiscoverySource, ModelInput, ModelRequest, ModelUsage,
    Provider, ProviderDescriptor, ProviderError, ProviderErrorKind, ProviderFuture, ProviderStream,
    ProviderStreamEvent, ResponseMetadata,
};

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
const DEFAULT_KEY_VARIABLE: &str = "OPENAI_API_KEY";
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(5);
const WEBSOCKET_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const WEBSOCKET_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const WEBSOCKET_RECONNECT_ATTEMPTS: u8 = 5;

/// `OpenAI` Responses API adapter. Provider enablement is intentionally owned by configuration,
/// independently from whether the environment key is present.
#[derive(Clone, Debug)]
pub struct OpenAiProvider {
    client: reqwest::Client,
    provider_id: String,
    display_name: String,
    base_url: String,
    key_variable: String,
    credentials: crate::provider::CredentialStore,
    api_key: Arc<RwLock<Option<crate::provider::ManagedApiKey>>>,
    model_discovery: ModelDiscoverySource,
    models_endpoint: Option<String>,
    descriptor: ProviderDescriptor,
    /// Responses WebSockets are a public OpenAI API feature, not an
    /// OpenAI-compatible protocol extension. Keep this explicit so endpoint
    /// overrides and custom Responses adapters never accidentally upgrade.
    responses_transport: ResponsesTransport,
    websocket_lanes: Arc<Mutex<HashMap<String, Arc<Mutex<WebSocketLane>>>>>,
    #[cfg(test)]
    api_key_override: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ResponsesTransport {
    HttpOnly,
    OpenAiWebSocket,
}

type OpenAiWebSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

#[derive(Debug, Default)]
struct WebSocketLane {
    connection: Option<OpenAiWebSocket>,
    /// Once a lane proves incompatible, retain normal SSE behaviour for its
    /// lifetime rather than attempting an upgrade on every turn.
    http_fallback: bool,
}

impl OpenAiProvider {
    #[must_use]
    pub fn new() -> Self {
        Self::with_base_url(DEFAULT_BASE_URL)
    }

    #[must_use]
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self::for_provider(
            "openai",
            "OpenAI",
            base_url,
            DEFAULT_KEY_VARIABLE,
            ModelDiscoverySource::ProviderApi,
        )
    }

    #[must_use]
    pub fn for_provider(
        provider_id: impl Into<String>,
        display_name: impl Into<String>,
        base_url: impl Into<String>,
        key_variable: impl Into<String>,
        model_discovery: ModelDiscoverySource,
    ) -> Self {
        let provider_id = provider_id.into();
        let display_name = display_name.into();
        let base_url = base_url.into().trim_end_matches('/').to_owned();
        let key_variable = key_variable.into();
        let credentials =
            crate::provider::CredentialStore::api_key(Path::new("."), "default", &provider_id);
        Self {
            client: reqwest::Client::new(),
            provider_id: provider_id.clone(),
            display_name: display_name.clone(),
            base_url: base_url.clone(),
            key_variable: key_variable.clone(),
            api_key: Arc::new(RwLock::new(credentials.load_api_key().ok().flatten())),
            credentials,
            model_discovery,
            models_endpoint: (model_discovery == ModelDiscoverySource::ProviderApi)
                .then(|| "models".into()),
            descriptor: ProviderDescriptor {
                id: provider_id.clone(),
                display_name: display_name.clone(),
                default_model_backend: Some(ModelBackend::OpenAiResponses),
                supported_model_backends: vec![ModelBackend::OpenAiResponses],
                model_discovery,
                credential_source: CredentialSource::ApiKey,
                credential_environment_variable: Some(key_variable.clone()),
                supports_managed_api_key: true,
                auth_flows: Vec::new(),
            },
            responses_transport: if provider_id == "openai" && base_url == DEFAULT_BASE_URL {
                ResponsesTransport::OpenAiWebSocket
            } else {
                ResponsesTransport::HttpOnly
            },
            websocket_lanes: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(test)]
            api_key_override: None,
        }
    }

    #[must_use]
    pub fn with_credential_dir(mut self, data_dir: &Path) -> Self {
        self.credentials =
            crate::provider::CredentialStore::api_key(data_dir, "default", &self.provider_id);
        self.api_key = Arc::new(RwLock::new(self.credentials.load_api_key().ok().flatten()));
        self
    }

    #[must_use]
    pub fn for_custom_responses(
        provider_id: impl Into<String>,
        display_name: impl Into<String>,
        base_url: impl Into<String>,
        key_variable: impl Into<String>,
        models_endpoint: Option<String>,
    ) -> Self {
        let mut provider = Self::for_provider(
            provider_id,
            display_name,
            base_url,
            key_variable,
            if models_endpoint.is_some() {
                ModelDiscoverySource::ProviderApi
            } else {
                ModelDiscoverySource::ModelsDev
            },
        );
        provider.models_endpoint = models_endpoint;
        provider
    }

    #[must_use]
    pub fn with_endpoint(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into().trim_end_matches('/').into();
        self.responses_transport =
            if self.provider_id == "openai" && self.base_url == DEFAULT_BASE_URL {
                ResponsesTransport::OpenAiWebSocket
            } else {
                ResponsesTransport::HttpOnly
            };
        self
    }

    #[must_use]
    pub fn open_code() -> Self {
        Self::for_provider(
            "opencode",
            "OpenCode Zen",
            "https://opencode.ai/zen/v1",
            "OPENCODE_API_KEY",
            ModelDiscoverySource::ProviderApi,
        )
    }

    #[must_use]
    pub fn with_environment_variable(mut self, variable: impl Into<String>) -> Self {
        self.key_variable = variable.into();
        self.descriptor.credential_environment_variable = Some(self.key_variable.clone());
        self
    }

    #[cfg(test)]
    fn with_client(base_url: impl Into<String>, client: reqwest::Client) -> Self {
        let mut provider = Self::for_provider(
            "openai",
            "OpenAI",
            base_url,
            DEFAULT_KEY_VARIABLE,
            ModelDiscoverySource::ProviderApi,
        );
        provider.client = client;
        provider
    }

    #[cfg(test)]
    pub(crate) fn with_test_api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key_override = Some(key.into());
        self
    }

    async fn resolved_api_key(&self) -> Result<String, ProviderError> {
        #[cfg(test)]
        if let Some(key) = &self.api_key_override {
            return Ok(key.clone());
        }
        if let Some(key) = self.api_key.read().await.as_ref() {
            return Ok(key.value.clone());
        }
        crate::provider::environment_api_key(&self.key_variable)
    }

    async fn send(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, ProviderError> {
        let response = request
            .send()
            .await
            .map_err(|error| transport_error(&error))?;
        if response.status().is_success() {
            return Ok(response);
        }
        Err(http_error(response).await)
    }
}

impl Default for OpenAiProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl Provider for OpenAiProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn auth_state(&self) -> ProviderFuture<'_, Result<AuthState, ProviderError>> {
        Box::pin(async {
            Ok(if self.resolved_api_key().await.is_ok() {
                AuthState::Available {
                    detail: "key found".into(),
                }
            } else {
                AuthState::Missing
            })
        })
    }

    fn discover_models(&self) -> Option<ProviderFuture<'_, Result<ModelCatalog, ProviderError>>> {
        if self.model_discovery == ModelDiscoverySource::ModelsDev {
            return None;
        }
        Some(Box::pin(async move {
            let key = self.resolved_api_key().await?;
            let endpoint = self.models_endpoint.as_deref().ok_or_else(|| {
                ProviderError::configuration("provider does not expose a model endpoint")
            })?;
            let url = if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
                endpoint.into()
            } else {
                format!("{}/{}", self.base_url, endpoint.trim_start_matches('/'))
            };
            let response = tokio::time::timeout(
                DISCOVERY_TIMEOUT,
                self.send(self.client.get(url).bearer_auth(key)),
            )
            .await
            .map_err(|_| ProviderError {
                kind: ProviderErrorKind::Timeout,
                code: "model_discovery_timeout".into(),
                message: "OpenAI model discovery timed out after five seconds".into(),
                retryable: true,
                retry_after_millis: None,
                status: None,
                metadata: BTreeMap::new(),
            })??;
            let mut payload = response
                .bytes()
                .await
                .map(|bytes| bytes.to_vec())
                .map_err(|error| protocol_error("invalid_models_response", error.to_string()))?;
            let payload = simd_json::serde::from_slice::<Value>(&mut payload)
                .map_err(|error| protocol_error("invalid_models_response", error.to_string()))?;
            parse_model_catalog(&self.provider_id, &payload)
        }))
    }

    fn stream(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> ProviderFuture<'_, Result<ProviderStream, ProviderError>> {
        Box::pin(async move {
            if request.model.provider != self.provider_id {
                return Err(ProviderError::configuration(format!(
                    "{} adapter cannot serve {}",
                    self.display_name, request.model
                )));
            }
            if request
                .backend
                .is_some_and(|backend| backend != ModelBackend::OpenAiResponses)
            {
                return Err(ProviderError::configuration(format!(
                    "{} does not support the requested model backend",
                    self.display_name
                )));
            }
            let key = self.resolved_api_key().await?;
            if self.responses_transport == ResponsesTransport::OpenAiWebSocket {
                let lane_key =
                    crate::provider::prompt_cache::native_cache_key(&request, &self.provider_id)
                        .unwrap_or_else(|| request.model.model.clone());
                let lane = {
                    let mut lanes = self.websocket_lanes.lock().await;
                    lanes
                        .entry(lane_key)
                        .or_insert_with(|| Arc::new(Mutex::new(WebSocketLane::default())))
                        .clone()
                };
                let provider = self.clone();
                let (sender, receiver) = mpsc::channel(64);
                tokio::spawn(async move {
                    stream_openai_websocket(&provider, lane, key, request, cancellation, sender)
                        .await;
                });
                return Ok(Box::pin(ReceiverStream::new(receiver)) as ProviderStream);
            }
            let response = self
                .send(
                    self.client
                        .post(format!("{}/responses", self.base_url))
                        .header(AUTHORIZATION, format!("Bearer {key}"))
                        .header(CONTENT_TYPE, "application/json")
                        .json(&request_body_with_policy(
                            &request,
                            &prompt_cache_policy(self.provider_id == "openai", &request),
                        )?),
                )
                .await?;
            let (sender, receiver) = mpsc::channel(64);
            tokio::spawn(map_sse(response, sender, cancellation));
            Ok(Box::pin(ReceiverStream::new(receiver)) as ProviderStream)
        })
    }

    fn set_api_key(&self, key: String) -> ProviderFuture<'_, Result<(), ProviderError>> {
        Box::pin(async move {
            if key.trim().is_empty() {
                return Err(ProviderError::configuration("API key must not be blank"));
            }
            let key = crate::provider::ManagedApiKey { value: key };
            self.credentials.save_api_key(&key)?;
            *self.api_key.write().await = Some(key);
            Ok(())
        })
    }

    fn remove_api_key(&self) -> ProviderFuture<'_, Result<(), ProviderError>> {
        Box::pin(async move {
            self.credentials.delete_api_key()?;
            *self.api_key.write().await = None;
            Ok(())
        })
    }
    fn has_managed_api_key(&self) -> ProviderFuture<'_, bool> {
        Box::pin(async move { self.api_key.read().await.is_some() })
    }
}

async fn stream_openai_http(
    provider: &OpenAiProvider,
    key: String,
    mut request: ModelRequest,
    cancellation: CancellationToken,
    sender: mpsc::Sender<Result<ProviderStreamEvent, ProviderError>>,
) {
    request.response_transport_continuation = None;
    let body = match request_body_with_policy(
        &request,
        &prompt_cache_policy(provider.provider_id == "openai", &request),
    ) {
        Ok(body) => body,
        Err(error) => {
            let _ = sender.send(Err(error)).await;
            return;
        }
    };
    match provider
        .send(
            provider
                .client
                .post(format!("{}/responses", provider.base_url))
                .header(AUTHORIZATION, format!("Bearer {key}"))
                .header(CONTENT_TYPE, "application/json")
                .json(&body),
        )
        .await
    {
        Ok(response) => map_sse(response, sender, cancellation).await,
        Err(error) => {
            let _ = sender.send(Err(error)).await;
        }
    }
}

async fn stream_openai_websocket(
    provider: &OpenAiProvider,
    lane: Arc<Mutex<WebSocketLane>>,
    key: String,
    mut request: ModelRequest,
    cancellation: CancellationToken,
    sender: mpsc::Sender<Result<ProviderStreamEvent, ProviderError>>,
) {
    let mut reconnects: u8 = 0;
    loop {
        let mut guard = tokio::select! {
            biased;
            () = cancellation.cancelled() => { let _ = sender.send(Err(ProviderError::cancelled())).await; return; }
            guard = lane.lock() => guard,
        };
        if guard.http_fallback {
            drop(guard);
            stream_openai_http(provider, key, request, cancellation, sender).await;
            return;
        }
        let socket = if let Some(socket) = guard.connection.as_mut() {
            socket
        } else {
            match connect_openai_websocket(&provider.base_url, &key, &cancellation).await {
                Ok(socket) => {
                    guard.connection = Some(socket);
                    guard.connection.as_mut().expect("connection inserted")
                }
                Err(error) if error.status == Some(426) => {
                    guard.http_fallback = true;
                    drop(guard);
                    stream_openai_http(provider, key, request, cancellation, sender).await;
                    return;
                }
                Err(error) if error.kind == ProviderErrorKind::Cancelled => {
                    let _ = sender.send(Err(error)).await;
                    return;
                }
                Err(error) => {
                    drop(guard);
                    if reconnects.saturating_add(1) >= WEBSOCKET_RECONNECT_ATTEMPTS {
                        let mut guard = lane.lock().await;
                        guard.http_fallback = true;
                        drop(guard);
                        stream_openai_http(provider, key, request, cancellation, sender).await;
                        return;
                    }
                    reconnects += 1;
                    tracing::debug!(attempt = reconnects, error = %error, "retrying OpenAI Responses WebSocket connection");
                    continue;
                }
            }
        };
        match stream_openai_websocket_request(socket, &request, &cancellation, &sender).await {
            Ok(()) => return,
            Err((error, emitted)) => {
                guard.connection = None;
                drop(guard);
                if cancellation.is_cancelled() || emitted {
                    let _ = sender.send(Err(error)).await;
                    return;
                }
                if error.status == Some(426)
                    || reconnects.saturating_add(1) >= WEBSOCKET_RECONNECT_ATTEMPTS
                {
                    let mut guard = lane.lock().await;
                    guard.http_fallback = true;
                    drop(guard);
                    stream_openai_http(provider, key, request, cancellation, sender).await;
                    return;
                }
                request.response_transport_continuation = None;
                reconnects += 1;
            }
        }
    }
}

pub(crate) fn parse_model_catalog(
    provider_id: &str,
    payload: &Value,
) -> Result<ModelCatalog, ProviderError> {
    let models = payload
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| protocol_error("invalid_models_response", "response did not contain data"))?
        .iter()
        .filter_map(|model| {
            let id = model.get("id")?.as_str()?.to_owned();
            Some(ModelDescriptor {
                display_name: id.clone(),
                id: id.clone(),
                capabilities: ModelCapabilities::default(),
                backend: None,
                raw_metadata: model.clone(),
            })
        })
        .collect();
    Ok(ModelCatalog {
        provider: provider_id.into(),
        models,
        version: payload
            .get("version")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
    })
}

#[cfg(test)]
pub(crate) fn request_body(request: &ModelRequest) -> Result<Value, ProviderError> {
    let policy = if request.model.provider == "chatgpt" {
        crate::provider::prompt_cache::native_cache_key(request, "chatgpt").map_or(
            crate::provider::codecs::openai_responses::PromptCachePolicy::Disabled,
            |key| crate::provider::codecs::openai_responses::PromptCachePolicy::ChatGptCompatible {
                key,
            },
        )
    } else {
        prompt_cache_policy(request.model.provider == "openai", request)
    };
    request_body_with_policy(request, &policy)
}

pub(crate) fn request_body_with_policy(
    request: &ModelRequest,
    cache: &crate::provider::codecs::openai_responses::PromptCachePolicy,
) -> Result<Value, ProviderError> {
    request_body_with_policy_and_protocol(request, cache, false)
}

pub(crate) fn request_body_with_policy_and_protocol(
    request: &ModelRequest,
    cache: &crate::provider::codecs::openai_responses::PromptCachePolicy,
    responses_lite: bool,
) -> Result<Value, ProviderError> {
    let instructions = stable_instructions(&request.stable_prompt);
    let explicit = matches!(
        cache,
        crate::provider::codecs::openai_responses::PromptCachePolicy::Explicit { .. }
    );
    let mut input = Vec::new();
    let input_start = request
        .response_transport_continuation
        .as_ref()
        .map_or(0, |continuation| continuation.input_suffix_start);
    let continuation = request.response_transport_continuation.is_some();
    if explicit && !continuation && !instructions.is_empty() {
        input.push(json!({
            "type": "message",
            "role": "system",
            "content": [{
                "type": "input_text",
                "text": instructions,
                "prompt_cache_breakpoint": { "mode": "explicit" },
            }],
        }));
    }
    input.extend(
        request
            .input
            .get(input_start..)
            .unwrap_or(&request.input)
            .iter()
            // The assistant's function calls are already represented by the
            // previous Responses response. A continuation only needs the newly
            // produced outputs (and any user-message suffix).
            .filter(|item| !continuation || !matches!(item, ModelInput::ToolCall { .. }))
            .filter(|item| match item {
                ModelInput::ProviderReasoning { source, item } => {
                    matches!(request.model.provider.as_str(), "openai" | "chatgpt")
                        && source == &request.model
                        && crate::EncryptedReasoningItem::from_value(item.as_value()).is_some()
                }
                _ => true,
            })
            .map(|item| match item {
                ModelInput::ProviderReasoning { item, .. } => Ok(item.as_value().clone()),
                ModelInput::Message { role, content } => {
                    let assistant = matches!(role, MessageRole::Assistant);
                    let role = match role {
                        MessageRole::System => "system",
                        MessageRole::User => "user",
                        MessageRole::Assistant => "assistant",
                    };
                    Ok(if assistant {
                        json!({
                            "type": "message",
                            "role": role,
                            "content": [{
                                "type": "output_text",
                                "text": content,
                            }],
                        })
                    } else if explicit {
                        json!({
                            "type": "message",
                            "role": role,
                            "content": [{
                                "type": "input_text",
                                "text": content,
                                "prompt_cache_breakpoint": { "mode": "explicit" },
                            }],
                        })
                    } else {
                        json!({ "role": role, "content": content })
                    })
                }
                ModelInput::MultimodalMessage { role, content } => {
                    let role = match role {
                        MessageRole::System => "system",
                        MessageRole::User => "user",
                        MessageRole::Assistant => "assistant",
                    };
                    Ok(json!({
                        "type": "message",
                        "role": role,
                        "content": content.iter().map(|part| match part {
                            crate::ModelContentPart::Text { text } => json!({
                                "type": if role == "assistant" { "output_text" } else { "input_text" },
                                "text": text,
                            }),
                            crate::ModelContentPart::Image { mime_type, data, .. } => json!({
                                "type": "input_image",
                                "image_url": format!("data:{mime_type};base64,{data}"),
                            }),
                        }).collect::<Vec<_>>(),
                    }))
                }
                ModelInput::ToolCall {
                    call_id,
                    name,
                    arguments,
                    ..
                } => Ok(json!({
                    "type": "function_call",
                    "call_id": call_id,
                    "name": name,
                    "arguments": serde_json::to_string(arguments).map_err(|error| {
                        ProviderError::configuration(format!("invalid tool arguments: {error}"))
                    })?,
                })),
                ModelInput::ToolResult {
                    call_id,
                    output,
                    is_error,
                } => Ok(json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": serde_json::to_string(&json!({
                        "output": output,
                        "is_error": is_error,
                    })).map_err(|error| ProviderError::configuration(format!(
                        "invalid tool result: {error}"
                    )))?,
                })),
                ModelInput::ConfigurationUpdate { effort } => Ok(json!({
                    "type": "configuration_update",
                    "reasoning": { "effort": effort },
                })),
            })
            .collect::<Result<Vec<_>, ProviderError>>()?,
    );
    let tools = request
        .tools
        .iter()
        .map(|tool| {
            let mut encoded = json!({
                "type": "function",
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.input_schema,
                "strict": supports_strict_schema(&tool.input_schema),
            });
            // Lite cannot inject the acknowledgments used by async tools.
            if !responses_lite
                && tool.asynchronous
                && supports_async_tool_calling(&request.model.model)
            {
                encoded["async"] = Value::Bool(true);
            }
            encoded
        })
        .collect::<Vec<_>>();
    let mut body = json!({
        "model": request.model.model,
        "input": input,
        "tools": tools,
        "stream": true,
        "parallel_tool_calls": request.allow_parallel_tools,
        // Response IDs are Cagent's optional transport optimization for a
        // tool-result continuation. OpenAI only accepts `previous_response_id`
        // for responses it retained, while the ChatGPT Codex backend requires
        // its requests to be non-stored.
        "store": request.model.provider == "openai",
    });
    if let Some(continuation) = &request.response_transport_continuation {
        body["previous_response_id"] = Value::String(continuation.response_id.clone());
    }
    if matches!(request.model.provider.as_str(), "openai" | "chatgpt") {
        body["include"] = json!(["reasoning.encrypted_content"]);
    }
    if !instructions.is_empty() && !explicit {
        body["instructions"] = Value::String(instructions.clone());
    }
    match cache {
        crate::provider::codecs::openai_responses::PromptCachePolicy::Disabled => {}
        crate::provider::codecs::openai_responses::PromptCachePolicy::ChatGptCompatible { key } => {
            body["prompt_cache_key"] = Value::String(key.clone());
        }
        crate::provider::codecs::openai_responses::PromptCachePolicy::Automatic {
            key,
            retention,
        } => {
            body["prompt_cache_key"] = Value::String(key.clone());
            if let Some(retention) = retention {
                body["prompt_cache_retention"] = Value::String((*retention).into());
            }
        }
        crate::provider::codecs::openai_responses::PromptCachePolicy::Explicit { key, ttl } => {
            body["prompt_cache_key"] = Value::String(key.clone());
            body["prompt_cache_options"] = json!({ "mode": "explicit", "ttl": ttl });
        }
    }
    if let Some(effort) = &request.effort {
        body["reasoning"] = json!({ "effort": effort });
    }
    if let Some(service_tier) = &request.service_tier {
        body["service_tier"] = Value::String(service_tier.clone());
    }
    if let Some(output) = &request.structured_output {
        body["text"] = json!({
            "format": {
                "type": "json_schema",
                "name": "cagent_output",
                "strict": true,
                "schema": output.schema,
            }
        });
    }
    if responses_lite {
        apply_responses_lite_protocol(&mut body, request, &instructions)?;
    }
    Ok(body)
}

fn supports_async_tool_calling(model: &str) -> bool {
    model == "gpt-6-astra" || model.starts_with("gpt-6-astra-")
}

fn apply_responses_lite_protocol(
    body: &mut Value,
    request: &ModelRequest,
    instructions: &str,
) -> Result<(), ProviderError> {
    let functions = body
        .as_object_mut()
        .and_then(|body| body.remove("tools"))
        .and_then(|tools| tools.as_array().cloned())
        .unwrap_or_default();
    let namespaced_tools = if functions.is_empty() {
        Vec::new()
    } else {
        vec![json!({
            "type": "namespace",
            "name": "functions",
            "description": "",
            "tools": functions,
        })]
    };
    let tools_id = stable_responses_lite_item_id("at", request, &namespaced_tools)?;
    let mut prefix = vec![json!({
        "id": tools_id,
        "type": "additional_tools",
        "role": "developer",
        "tools": namespaced_tools,
    })];
    if !instructions.is_empty() {
        prefix.push(json!({
            "id": stable_responses_lite_item_id("msg", request, &instructions)?,
            "type": "message",
            "role": "developer",
            "content": [{ "type": "input_text", "text": instructions }],
            "internal_chat_message_metadata_passthrough": {
                "content_item_kinds": ["model.base_instructions"],
            },
        }));
    }
    let input = body
        .get_mut("input")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| ProviderError::configuration("Responses input is not an array"))?;
    input.splice(0..0, prefix);
    body.as_object_mut()
        .expect("Responses request is an object")
        .remove("instructions");
    // Lite requires an explicit false, even for requests without tools.
    // Omitting the field lets the backend default trigger unsupported_value.
    body["parallel_tool_calls"] = Value::Bool(false);
    if body.get("reasoning").is_none() {
        body["reasoning"] = json!({});
    }
    body["reasoning"]["context"] = Value::String("all_turns".into());
    Ok(())
}

fn stable_responses_lite_item_id(
    prefix: &str,
    request: &ModelRequest,
    value: &impl serde::Serialize,
) -> Result<String, ProviderError> {
    let payload =
        serde_json::to_vec(&(responses_lite_thread_id(request), value)).map_err(|error| {
            ProviderError::configuration(format!("invalid Responses Lite prefix: {error}"))
        })?;
    // Keep 128 hash bits for compact, deterministic IDs, comfortably below
    // the 64-character Responses limit with either the `at_` or `msg_` prefix.
    let digest = format!("{:x}", Sha256::digest(payload));
    Ok(format!("{prefix}_{}", &digest[..32]))
}

fn responses_lite_thread_id(request: &ModelRequest) -> String {
    request
        .response_transport_continuation
        .as_ref()
        .map(|continuation| continuation.conversation_id.to_string())
        .or_else(|| request.prompt_cache.as_ref().map(|cache| cache.key.clone()))
        .unwrap_or_else(|| request.request_id.to_string())
}

fn stable_instructions(parts: &[crate::StablePromptPart]) -> String {
    parts
        .iter()
        .map(|part| {
            format!(
                "<cagent:{}>\n{}\n</cagent:{}>",
                part.identity, part.content, part.identity
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn prompt_cache_policy(
    supported: bool,
    request: &ModelRequest,
) -> crate::provider::codecs::openai_responses::PromptCachePolicy {
    use crate::provider::codecs::openai_responses::PromptCachePolicy;
    let Some(key) = supported
        .then(|| crate::provider::prompt_cache::native_cache_key(request, "openai"))
        .flatten()
    else {
        return PromptCachePolicy::Disabled;
    };
    if uses_explicit_prompt_caching(&request.model.model) {
        PromptCachePolicy::Explicit { key, ttl: "30m" }
    } else {
        PromptCachePolicy::Automatic {
            key,
            retention: supports_extended_retention(&request.model.model)
                .then_some(crate::provider::prompt_cache::OPENAI_EXTENDED_RETENTION),
        }
    }
}

fn uses_explicit_prompt_caching(model: &str) -> bool {
    model.starts_with("gpt-5.6") || model == "gpt-6-astra" || model.starts_with("gpt-6-astra-")
}

fn supports_extended_retention(model: &str) -> bool {
    [
        "gpt-5.5", "gpt-5.4", "gpt-5.2", "gpt-5.1", "gpt-5", "gpt-4.1",
    ]
    .iter()
    .any(|prefix| model == *prefix || model.starts_with(&format!("{prefix}-")))
}

fn supports_strict_schema(schema: &Value) -> bool {
    match schema {
        Value::Object(object) => {
            let is_object_schema = object.get("type").is_some_and(|kind| {
                kind == "object"
                    || kind
                        .as_array()
                        .is_some_and(|kinds| kinds.iter().any(|kind| kind == "object"))
            });
            let requires_every_property = object
                .get("properties")
                .and_then(Value::as_object)
                .is_none_or(|properties| {
                    if properties.is_empty() {
                        return true;
                    }
                    let Some(required) = object.get("required").and_then(Value::as_array) else {
                        return false;
                    };
                    required.len() == properties.len()
                        && required.iter().all(|name| {
                            name.as_str()
                                .is_some_and(|name| properties.contains_key(name))
                        })
                });
            (!is_object_schema
                || (object.get("additionalProperties") == Some(&Value::Bool(false))
                    && requires_every_property))
                && object.values().all(supports_strict_schema)
        }
        Value::Array(values) => values.iter().all(supports_strict_schema),
        _ => true,
    }
}

async fn connect_openai_websocket(
    base_url: &str,
    api_key: &str,
    cancellation: &CancellationToken,
) -> Result<OpenAiWebSocket, ProviderError> {
    install_rustls_provider();
    let mut url = Url::parse(&format!("{}/responses", base_url.trim_end_matches('/')))
        .map_err(|error| ProviderError::configuration(error.to_string()))?;
    let scheme = match url.scheme() {
        "https" => "wss",
        "http" => "ws",
        "wss" | "ws" => url.scheme(),
        _ => return Err(ProviderError::configuration("invalid OpenAI WebSocket URL")),
    }
    .to_owned();
    url.set_scheme(&scheme)
        .map_err(|()| ProviderError::configuration("invalid OpenAI WebSocket URL"))?;
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|error| websocket_error(error.to_string(), None))?;
    request.headers_mut().insert(
        AUTHORIZATION,
        reqwest::header::HeaderValue::from_str(&format!("Bearer {api_key}"))
            .map_err(|error| websocket_error(error.to_string(), None))?,
    );
    let result = tokio::select! {
        biased;
        () = cancellation.cancelled() => return Err(ProviderError::cancelled()),
        result = tokio::time::timeout(WEBSOCKET_CONNECT_TIMEOUT, connect_async(request)) => result,
    };
    result
        .map_err(|_| {
            ProviderError::timeout(
                "websocket_connect_timeout",
                "OpenAI Responses WebSocket connection timed out",
            )
        })?
        .map(|(socket, _)| socket)
        .map_err(|error| match error {
            WebSocketError::Http(response) => websocket_error(
                format!("OpenAI WebSocket handshake returned {}", response.status()),
                Some(response.status().as_u16()),
            ),
            error => websocket_error(error.to_string(), None),
        })
}

fn install_rustls_provider() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

async fn stream_openai_websocket_request(
    websocket: &mut OpenAiWebSocket,
    request: &ModelRequest,
    cancellation: &CancellationToken,
    sender: &mpsc::Sender<Result<ProviderStreamEvent, ProviderError>>,
) -> Result<(), (ProviderError, bool)> {
    let mut payload = request_body_with_policy(request, &prompt_cache_policy(true, request))
        .map_err(|error| (error, false))?;
    // The WebSocket protocol is event-streaming by definition; unlike SSE,
    // the public `response.create` frame must not carry `stream: true`.
    payload
        .as_object_mut()
        .expect("Responses request is an object")
        .remove("stream");
    payload["type"] = json!("response.create");
    websocket
        .send(Message::Text(payload.to_string().into()))
        .await
        .map_err(|error| (websocket_error(error.to_string(), None), false))?;
    let mut tool_ids = BTreeMap::new();
    let mut emitted = false;
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
            () = cancellation.cancelled() => return Err((ProviderError::cancelled(), emitted)),
            steer = async {
                match steering.as_mut() {
                    Some(receiver) => receiver.recv().await,
                    None => std::future::pending().await,
                }
            } => Incoming::Steer(steer),
            message = tokio::time::timeout(WEBSOCKET_IDLE_TIMEOUT, websocket.next()) => Incoming::Message(match message {
                Ok(Some(Ok(message))) => message,
                Ok(Some(Err(error))) => return Err((websocket_error(error.to_string(), None), emitted)),
                Ok(None) => return Err((websocket_error("OpenAI WebSocket closed before response.completed", None), emitted)),
                Err(_) => return Err((ProviderError::timeout("websocket_idle_timeout", "OpenAI Responses WebSocket was idle for five minutes"), emitted)),
            })
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
            websocket
                .send(Message::Text(
                    json!({
                        "type": "response.steer",
                        "previous_response_id": previous_response_id,
                        "input": input,
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .map_err(|error| (websocket_error(error.to_string(), None), emitted))?;
            pending_steers.push_back(input);
            continue;
        };
        match message {
            Message::Text(text) => {
                let value = simd_json::serde::from_slice::<Value>(&mut text.as_bytes().to_vec())
                    .map_err(|error| {
                        (
                            protocol_error("invalid_websocket_json", error.to_string()),
                            emitted,
                        )
                    })?;
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
                        && supports_async_tool_calling(&request.model.model)
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
                if let Some(event) = encrypted_reasoning_event(&value) {
                    sender
                        .send(Ok(event))
                        .await
                        .map_err(|_| (ProviderError::cancelled(), emitted))?;
                    emitted = true;
                }
                if let Some(event) =
                    normalize_event(&value, &mut tool_ids).map_err(|error| (error, emitted))?
                {
                    let completed = matches!(event, ProviderStreamEvent::Completed { .. });
                    emitted = true;
                    sender
                        .send(Ok(event))
                        .await
                        .map_err(|_| (ProviderError::cancelled(), emitted))?;
                    if completed {
                        return Ok(());
                    }
                }
            }
            Message::Ping(payload) => websocket
                .send(Message::Pong(payload))
                .await
                .map_err(|error| (websocket_error(error.to_string(), None), emitted))?,
            Message::Pong(_) | Message::Frame(_) => {}
            Message::Binary(_) => {
                return Err((
                    protocol_error(
                        "invalid_websocket_message",
                        "OpenAI sent a binary Responses event",
                    ),
                    emitted,
                ));
            }
            Message::Close(_) => {
                return Err((
                    websocket_error("OpenAI WebSocket closed before response.completed", None),
                    emitted,
                ));
            }
        }
    }
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

pub(crate) async fn map_sse(
    response: reqwest::Response,
    sender: mpsc::Sender<Result<ProviderStreamEvent, ProviderError>>,
    cancellation: CancellationToken,
) {
    let mut bytes = response.bytes_stream();
    let mut buffer = Vec::new();
    let mut tool_ids = BTreeMap::new();
    loop {
        let chunk = tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                let _ = sender.send(Err(ProviderError::cancelled())).await;
                return;
            }
            chunk = bytes.next() => chunk,
        };
        let Some(chunk) = chunk else {
            if !buffer.is_empty() && !emit_sse_blocks(&mut buffer, &sender, &mut tool_ids).await {
                return;
            }
            let _ = sender
                .send(Err(protocol_error(
                    "stream_ended",
                    "OpenAI stream ended before response.completed",
                )))
                .await;
            return;
        };
        match chunk {
            Ok(chunk) => buffer.extend_from_slice(&chunk),
            Err(error) => {
                let _ = sender.send(Err(transport_error(&error))).await;
                return;
            }
        }
        if !emit_sse_blocks(&mut buffer, &sender, &mut tool_ids).await {
            return;
        }
    }
}

async fn emit_sse_blocks(
    buffer: &mut Vec<u8>,
    sender: &mpsc::Sender<Result<ProviderStreamEvent, ProviderError>>,
    tool_ids: &mut BTreeMap<String, String>,
) -> bool {
    while let Some((end, delimiter_length)) = find_sse_boundary(buffer) {
        let block = match String::from_utf8(buffer[..end].to_vec()) {
            Ok(block) => block.replace("\r\n", "\n"),
            Err(error) => {
                let _ = sender
                    .send(Err(protocol_error("invalid_sse_utf8", error.to_string())))
                    .await;
                return false;
            }
        };
        buffer.drain(..end + delimiter_length);
        let data = block
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(str::trim_start)
            .collect::<Vec<_>>()
            .join("\n");
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let event = match simd_json::serde::from_slice::<Value>(&mut data.into_bytes()) {
            Ok(value) => {
                tracing::debug!(
                    event_type = value.get("type").and_then(serde_json::Value::as_str),
                    "received OpenAI Responses SSE event"
                );
                if let Some(event) = encrypted_reasoning_event(&value)
                    && sender.send(Ok(event)).await.is_err()
                {
                    return false;
                }
                normalize_event(&value, tool_ids)
            }
            Err(error) => Err(protocol_error("invalid_sse_json", error.to_string())),
        };
        match event {
            Ok(None) => {}
            Ok(Some(event)) => {
                let completed = matches!(event, ProviderStreamEvent::Completed { .. });
                if sender.send(Ok(event)).await.is_err() {
                    return false;
                }
                if completed {
                    return false;
                }
            }
            Err(error) => {
                let _ = sender.send(Err(error)).await;
                return false;
            }
        }
    }
    true
}

fn find_sse_boundary(buffer: &[u8]) -> Option<(usize, usize)> {
    crate::provider::wire::find_sse_boundary(buffer)
}

pub(crate) fn encrypted_reasoning_event(value: &Value) -> Option<ProviderStreamEvent> {
    let (items, replace) = match value.get("type").and_then(Value::as_str)? {
        "response.output_item.done" => (
            vec![crate::EncryptedReasoningItem::from_value(
                value.get("item")?,
            )?],
            false,
        ),
        "response.completed" => {
            let items = value
                .pointer("/response/output")?
                .as_array()?
                .iter()
                .filter_map(crate::EncryptedReasoningItem::from_value)
                .collect::<Vec<_>>();
            // Some compatible streams omit private items in their final snapshot.
            // Retain the completed item events in that case.
            if items.is_empty() {
                return None;
            }
            (items, true)
        }
        _ => return None,
    };
    Some(ProviderStreamEvent::EncryptedReasoning { items, replace })
}

pub(crate) fn normalize_event(
    value: &Value,
    tool_ids: &mut BTreeMap<String, String>,
) -> Result<Option<ProviderStreamEvent>, ProviderError> {
    let event_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match event_type {
        "response.output_item.added"
            if value.pointer("/item/type").and_then(Value::as_str) == Some("reasoning") =>
        {
            Ok(Some(ProviderStreamEvent::ReasoningStarted))
        }
        "response.reasoning_summary_text.delta" | "response.reasoning_summary_part.added" => {
            Ok(Some(ProviderStreamEvent::ReasoningStarted))
        }
        "response.output_text.delta" => Ok(Some(ProviderStreamEvent::TextDelta {
            delta: required_string(value, "delta")?.into(),
        })),
        "response.output_item.added"
            if value.pointer("/item/type").and_then(Value::as_str) == Some("function_call") =>
        {
            let item = &value["item"];
            let call_id = item
                .get("call_id")
                .and_then(Value::as_str)
                .ok_or_else(|| protocol_error("invalid_tool_call", "missing call ID"))?;
            if let Some(item_id) = item.get("id").and_then(Value::as_str) {
                tool_ids.insert(item_id.into(), call_id.into());
            }
            Ok(Some(ProviderStreamEvent::ToolCallStarted {
                id: call_id.into(),
                name: required_string(item, "name")?.into(),
                request_index: value
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            }))
        }
        "response.function_call_arguments.delta" => {
            Ok(Some(ProviderStreamEvent::ToolArgumentsDelta {
                id: value
                    .get("call_id")
                    .and_then(Value::as_str)
                    .map(Into::into)
                    .or_else(|| {
                        value
                            .get("item_id")
                            .and_then(Value::as_str)
                            .and_then(|item_id| tool_ids.get(item_id).cloned())
                    })
                    .ok_or_else(|| {
                        protocol_error(
                            "invalid_tool_arguments_delta",
                            "missing or unknown tool item ID",
                        )
                    })?,
                delta: required_string(value, "delta")?.into(),
            }))
        }
        "response.output_item.done"
            if value.pointer("/item/type").and_then(Value::as_str) == Some("function_call")
                && value.pointer("/item/async").and_then(Value::as_bool) == Some(true) =>
        {
            let item = &value["item"];
            Ok(Some(ProviderStreamEvent::ToolCallMetadata {
                id: required_string(item, "call_id")?.into(),
                metadata: json!({ "openai": { "async": true } }),
            }))
        }
        "response.completed" => Ok(Some(ProviderStreamEvent::Completed {
            metadata: completion_metadata(&value["response"]),
        })),
        "response.failed" | "response.incomplete" | "error" => Err(api_stream_error(value)),
        _ => Ok(None),
    }
}

fn completion_metadata(response: &Value) -> ResponseMetadata {
    let usage = response.get("usage").unwrap_or(&Value::Null);
    let mut provider_usage = usage.clone();
    if let Some(service_tier) = response.get("service_tier").and_then(Value::as_str) {
        if !provider_usage.is_object() {
            provider_usage = json!({});
        }
        provider_usage
            .as_object_mut()
            .expect("provider usage was initialized as an object")
            .insert("service_tier".into(), Value::String(service_tier.into()));
    }
    let input_tokens = usage.get("input_tokens").and_then(Value::as_u64);
    let cache_read = usage
        .pointer("/input_tokens_details/cached_tokens")
        .and_then(Value::as_u64);
    let cache_write = usage
        .pointer("/input_tokens_details/cache_write_tokens")
        .and_then(Value::as_u64);
    ResponseMetadata {
        provider_request_id: response.get("id").and_then(Value::as_str).map(Into::into),
        finish_reason: match response.get("status").and_then(Value::as_str) {
            Some("incomplete") => FinishReason::Length,
            Some("cancelled") => FinishReason::Cancelled,
            _ if response
                .get("output")
                .and_then(Value::as_array)
                .is_some_and(|output| {
                    output.iter().any(|item| {
                        item.get("type").and_then(Value::as_str) == Some("function_call")
                    })
                }) =>
            {
                FinishReason::ToolCalls
            }
            Some("completed") | None => FinishReason::Stop,
            Some(_) => FinishReason::Other,
        },
        usage: {
            let mut normalized = ModelUsage {
                input_tokens,
                cache_read_input_tokens: cache_read,
                cache_write_input_tokens: cache_write,
                output_tokens: usage.get("output_tokens").and_then(Value::as_u64),
                reasoning_tokens: usage
                    .pointer("/output_tokens_details/reasoning_tokens")
                    .and_then(Value::as_u64),
                total_tokens: usage.get("total_tokens").and_then(Value::as_u64),
                provider_usage,
                cost: None,
                ..ModelUsage::default()
            };
            normalized.normalize();
            normalized
        },
    }
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, ProviderError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| protocol_error("invalid_stream_event", format!("missing {field}")))
}

fn api_stream_error(value: &Value) -> ProviderError {
    let error = value
        .pointer("/response/error")
        .or_else(|| value.get("error"))
        .unwrap_or(value);
    let incomplete_reason = value
        .pointer("/response/incomplete_details/reason")
        .and_then(Value::as_str);
    ProviderError {
        kind: ProviderErrorKind::Server,
        code: error
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("response_failed")
            .into(),
        message: incomplete_reason
            .map(|reason| format!("OpenAI response was incomplete: {reason}"))
            .or_else(|| error.get("message").and_then(Value::as_str).map(Into::into))
            .unwrap_or_else(|| "OpenAI response failed".into()),
        retryable: true,
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
        code: if error.is_timeout() {
            "timeout"
        } else {
            "connection"
        }
        .into(),
        message: error.to_string(),
        retryable: true,
        retry_after_millis: None,
        status: error.status().map(|status| status.as_u16()),
        metadata: BTreeMap::new(),
    }
}

pub(crate) async fn http_error(response: reqwest::Response) -> ProviderError {
    let error_body = crate::provider::wire::read_http_error(response).await;
    let status = error_body.status;
    let retry_after_millis = error_body.retry_after_millis;
    let body = error_body.body.as_str();
    let parsed =
        simd_json::serde::from_slice::<Value>(&mut body.as_bytes().to_vec()).unwrap_or(Value::Null);
    let message = parsed
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or({
            if body.is_empty() {
                "OpenAI request failed"
            } else {
                body
            }
        });
    let code = parsed
        .pointer("/error/code")
        .and_then(Value::as_str)
        .unwrap_or("http_error");
    ProviderError {
        kind: match status.as_u16() {
            401 | 403 => ProviderErrorKind::Authentication,
            408 => ProviderErrorKind::Timeout,
            429 => ProviderErrorKind::RateLimit,
            500..=599 => ProviderErrorKind::Server,
            _ => ProviderErrorKind::InvalidRequest,
        },
        code: code.into(),
        message: message.into(),
        retryable: matches!(status.as_u16(), 408 | 429 | 500..=599),
        retry_after_millis,
        status: Some(status.as_u16()),
        metadata: BTreeMap::new(),
    }
}

fn protocol_error(code: impl Into<String>, message: impl Into<String>) -> ProviderError {
    ProviderError {
        kind: ProviderErrorKind::Protocol,
        code: code.into(),
        message: message.into(),
        retryable: false,
        retry_after_millis: None,
        status: None,
        metadata: BTreeMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AttemptId, ModelRef, RequestId, StablePromptPart, ToolDefinition};
    use tokio_tungstenite::{accept_async, connect_async};

    fn test_request() -> ModelRequest {
        ModelRequest {
            request_id: RequestId::new(),
            attempt_id: AttemptId::new(),
            model: ModelRef::parse("openai/gpt-test").unwrap(),
            backend: Some(ModelBackend::OpenAiResponses),
            backend_candidates: vec![ModelBackend::OpenAiResponses],
            effort: Some("high".into()),
            service_tier: None,
            input: vec![ModelInput::Message {
                role: MessageRole::User,
                content: "hello".into(),
            }],
            tools: vec![ToolDefinition {
                name: "read".into(),
                description: "Read a file".into(),
                input_schema: json!({"type": "object"}),
                asynchronous: false,
            }],
            stable_prompt: vec![StablePromptPart {
                identity: "instructions-v1".into(),
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

    #[test]
    fn encrypted_reasoning_replays_only_to_its_source_and_not_twice_in_continuations() {
        let mut request = test_request();
        let wire = json!({"type":"reasoning", "id":"rs_test", "summary":[], "encrypted_content":"opaque-test-state"});
        request.input.push(ModelInput::ProviderReasoning {
            source: request.model.clone(),
            item: crate::EncryptedReasoningItem::from_value(&wire).unwrap(),
        });
        assert!(!format!("{:?}", request.input).contains("opaque-test-state"));
        let cache = crate::provider::codecs::openai_responses::PromptCachePolicy::Disabled;
        let body = request_body_with_policy(&request, &cache).unwrap();
        assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
        assert_eq!(body["input"][1], wire);
        request.response_transport_continuation = Some(crate::ResponseTransportContinuation {
            conversation_id: crate::ConversationId::new(),
            response_id: "resp_previous".into(),
            input_suffix_start: request.input.len(),
        });
        request.input.push(ModelInput::Message {
            role: MessageRole::User,
            content: "next".into(),
        });
        let delta = request_body_with_policy(&request, &cache).unwrap();
        assert_eq!(delta["input"].as_array().unwrap().len(), 1);
        assert!(!delta.to_string().contains("opaque-test-state"));
        request.response_transport_continuation = None;
        assert_eq!(
            request_body_with_policy(&request, &cache).unwrap()["input"][1],
            wire
        );

        for model in ["openai/other-model", "chatgpt/gpt-test", "custom/gpt-test"] {
            request.model = ModelRef::parse(model).unwrap();
            assert!(
                !request_body_with_policy(&request, &cache)
                    .unwrap()
                    .to_string()
                    .contains("opaque-test-state")
            );
        }
        request.model = ModelRef::parse("chatgpt/gpt-test").unwrap();
        if let ModelInput::ProviderReasoning { source, .. } = &mut request.input[1] {
            *source = request.model.clone();
        }
        let lite =
            crate::provider::codecs::openai_responses::encode_responses_lite(&request, &cache)
                .unwrap();
        assert_eq!(lite["input"][3], wire);
        let anthropic = crate::provider::adapters::anthropic::request_body(&request).unwrap();
        let compatible =
            crate::provider::adapters::openai_compatible::request_body(&request, false).unwrap();
        assert!(!anthropic.to_string().contains("opaque-test-state"));
        assert!(!compatible.to_string().contains("opaque-test-state"));
        let gemini = crate::provider::codecs::gemini::request_body(
            &request,
            crate::provider::codecs::gemini::GeminiProfile::Developer,
        )
        .unwrap();
        assert!(!gemini.to_string().contains("opaque-test-state"));
    }

    #[tokio::test]
    async fn encrypted_reasoning_sse_captures_completed_items_and_final_snapshot() {
        let wire = json!({"type":"reasoning", "id":"rs_test", "summary":[], "encrypted_content":"opaque-test-state"});
        assert!(
            encrypted_reasoning_event(&json!({"type":"response.output_item.added", "item":wire}))
                .is_none()
        );
        assert!(encrypted_reasoning_event(&json!({"type":"response.output_item.done", "item":{"type":"reasoning", "summary":[]}})).is_none());
        let mut bytes = format!(
            "data: {}\n\ndata: {}\n\n",
            json!({"type":"response.output_item.done", "item":wire}),
            json!({"type":"response.completed", "response":{"id":"resp_test", "output":[wire]}})
        )
        .into_bytes();
        let (sender, mut receiver) = mpsc::channel(8);
        assert!(!emit_sse_blocks(&mut bytes, &sender, &mut BTreeMap::new()).await);
        assert!(
            matches!(receiver.recv().await.unwrap().unwrap(), ProviderStreamEvent::EncryptedReasoning { items, replace: false } if items[0].as_value() == &wire)
        );
        assert!(
            matches!(receiver.recv().await.unwrap().unwrap(), ProviderStreamEvent::EncryptedReasoning { items, replace: true } if items[0].as_value() == &wire)
        );
        assert!(matches!(
            receiver.recv().await.unwrap().unwrap(),
            ProviderStreamEvent::Completed { .. }
        ));
    }

    #[test]
    fn request_maps_to_responses_wire_format_without_provider_state() {
        let body = request_body(&test_request()).unwrap();
        assert_eq!(body["model"], "gpt-test");
        assert_eq!(body["input"][0]["role"], "user");
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["reasoning"]["effort"], "high");
        assert_eq!(body["store"], true);
        assert!(
            body["instructions"]
                .as_str()
                .unwrap()
                .contains("instructions-v1")
        );
        assert!(body.get("prompt_cache_key").is_some());
        assert!(body.get("service_tier").is_none());
    }

    #[test]
    fn request_includes_priority_service_tier() {
        let mut request = test_request();
        request.service_tier = Some("priority".into());
        let body = request_body(&request).unwrap();
        assert_eq!(body["service_tier"], "priority");
    }

    #[test]
    fn request_includes_previous_response_id_for_a_continuation() {
        let mut request = test_request();
        request.input = vec![
            ModelInput::ToolCall {
                call_id: "call-1".into(),
                name: "read".into(),
                arguments: json!({"path":"note.txt"}),
                provider_metadata: Value::Null,
            },
            ModelInput::ToolResult {
                call_id: "call-1".into(),
                output: json!("contents"),
                is_error: false,
            },
        ];
        request.response_transport_continuation = Some(crate::ResponseTransportContinuation {
            conversation_id: crate::ConversationId::new(),
            response_id: "resp_previous".into(),
            input_suffix_start: 0,
        });

        let body = request_body(&request).unwrap();

        assert_eq!(body["previous_response_id"], "resp_previous");
        assert_eq!(body["input"].as_array().unwrap().len(), 1);
        assert_eq!(body["input"][0]["type"], "function_call_output");
        assert!(
            serde_json::to_value(&request)
                .unwrap()
                .get("response_transport_continuation")
                .is_none()
        );
    }

    #[test]
    fn structured_output_uses_strict_responses_json_schema_format() {
        let mut request = test_request();
        request.structured_output = Some(crate::StructuredOutputRequest {
            schema: json!({"type": "object", "additionalProperties": false}),
        });
        let body = request_body(&request).unwrap();
        assert_eq!(
            body.pointer("/text/format/type"),
            Some(&json!("json_schema"))
        );
        assert_eq!(
            body.pointer("/text/format/name"),
            Some(&json!("cagent_output"))
        );
        assert_eq!(body.pointer("/text/format/strict"), Some(&json!(true)));
        assert_eq!(
            body.pointer("/text/format/schema/type"),
            Some(&json!("object"))
        );
    }

    #[test]
    fn gpt_56_marks_the_stable_prefix_and_uses_explicit_thirty_minute_caching() {
        let mut request = test_request();
        request.model = ModelRef::parse("openai/gpt-5.6-sol").unwrap();
        let body = request_body(&request).unwrap();

        assert!(body.get("instructions").is_none());
        assert_eq!(body["prompt_cache_options"]["mode"], "explicit");
        assert_eq!(body["prompt_cache_options"]["ttl"], "30m");
        assert_eq!(body["input"][0]["role"], "system");
        assert_eq!(
            body.pointer("/input/0/content/0/prompt_cache_breakpoint/mode"),
            Some(&json!("explicit"))
        );
        assert_eq!(body["input"][1]["role"], "user");
    }

    #[test]
    fn gpt_6_astra_marks_the_stable_prefix_and_uses_explicit_thirty_minute_caching() {
        let mut request = test_request();
        request.model = ModelRef::parse("openai/gpt-6-astra").unwrap();
        let body = request_body(&request).unwrap();

        assert!(body.get("instructions").is_none());
        assert_eq!(body["prompt_cache_options"]["mode"], "explicit");
        assert_eq!(body["prompt_cache_options"]["ttl"], "30m");
        assert_eq!(body["input"][0]["role"], "system");
        assert_eq!(
            body.pointer("/input/0/content/0/prompt_cache_breakpoint/mode"),
            Some(&json!("explicit"))
        );
    }

    #[test]
    fn astra_encodes_async_tools_and_configuration_updates() {
        let mut request = test_request();
        request.model = ModelRef::parse("openai/gpt-6-astra").unwrap();
        request.tools[0].asynchronous = true;
        request.input.push(ModelInput::ConfigurationUpdate {
            effort: "xhigh".into(),
        });

        let body = request_body(&request).unwrap();

        assert_eq!(body["tools"][0]["async"], true);
        assert_eq!(
            body["input"].as_array().unwrap().last().unwrap(),
            &json!({
                "type": "configuration_update",
                "reasoning": { "effort": "xhigh" },
            })
        );
    }

    #[test]
    fn astra_async_call_marker_is_retained_as_provider_metadata() {
        let mut tool_ids = BTreeMap::new();
        let event = normalize_event(
            &json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "function_call",
                    "call_id": "call-async",
                    "name": "web_fetch",
                    "arguments": "{}",
                    "async": true
                }
            }),
            &mut tool_ids,
        )
        .unwrap();

        assert!(matches!(
            event,
            Some(ProviderStreamEvent::ToolCallMetadata { id, metadata })
                if id == "call-async" && metadata["openai"]["async"] == true
        ));
    }

    #[test]
    fn non_astra_models_do_not_emit_async_tool_extensions() {
        let mut request = test_request();
        request.model = ModelRef::parse("openai/gpt-5.6").unwrap();
        request.tools[0].asynchronous = true;

        let body = request_body(&request).unwrap();

        assert!(body["tools"][0].get("async").is_none());
    }

    #[test]
    fn responses_lite_moves_tools_and_instructions_into_a_stable_input_prefix() {
        let mut request = test_request();
        request.model.provider = "chatgpt".into();
        let body = crate::provider::codecs::openai_responses::encode_responses_lite(
            &request,
            &crate::provider::codecs::openai_responses::PromptCachePolicy::Disabled,
        )
        .unwrap();

        assert!(body.get("tools").is_none());
        assert!(body.get("instructions").is_none());
        assert_eq!(body["parallel_tool_calls"], false);
        assert_eq!(body["reasoning"]["context"], "all_turns");
        assert_eq!(body["input"][0]["type"], "additional_tools");
        assert_eq!(body["input"][0]["role"], "developer");
        assert_eq!(body["input"][0]["tools"][0]["type"], "namespace");
        assert_eq!(body["input"][0]["tools"][0]["name"], "functions");
        assert_eq!(body["input"][1]["role"], "developer");
        for (index, prefix) in [(0, "at_"), (1, "msg_")] {
            let id = body["input"][index]["id"].as_str().unwrap();
            assert!(id.starts_with(prefix));
            assert_eq!(id.len(), prefix.len() + 32);
            assert!(
                id[prefix.len()..]
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit())
            );
            assert!(id.len() <= 64, "Responses Lite item ID is too long: {id}");
        }
        assert_eq!(
            body.pointer(
                "/input/1/internal_chat_message_metadata_passthrough/content_item_kinds/0"
            ),
            Some(&json!("model.base_instructions"))
        );
    }

    #[test]
    fn responses_lite_disables_parallel_calls_with_and_without_tools() {
        for allow_parallel_tools in [false, true] {
            for with_tools in [false, true] {
                let mut request = test_request();
                request.model.provider = "chatgpt".into();
                request.allow_parallel_tools = allow_parallel_tools;
                if !with_tools {
                    // Title generation and other text-only requests use the same contract.
                    request.tools.clear();
                    request.stable_prompt.clear();
                    request.effort = None;
                }
                let cache = crate::provider::codecs::openai_responses::PromptCachePolicy::Disabled;
                let lite = crate::provider::codecs::openai_responses::encode_responses_lite(
                    &request, &cache,
                )
                .unwrap();
                assert_eq!(lite["parallel_tool_calls"], false);
                assert_eq!(lite["reasoning"]["context"], "all_turns");
                assert!(lite.get("tools").is_none());
                assert!(lite.get("instructions").is_none());
                if !with_tools {
                    assert_eq!(lite["input"][0]["tools"], json!([]));
                    assert_eq!(lite["input"][1]["role"], "user");
                }

                let standard =
                    crate::provider::codecs::openai_responses::encode(&request, &cache).unwrap();
                assert_eq!(standard["parallel_tool_calls"], allow_parallel_tools);
                assert!(standard["reasoning"].get("context").is_none());
                assert!(standard.get("tools").is_some());
            }
        }
    }

    #[test]
    fn responses_lite_keeps_async_tools_available_without_async_extension() {
        for model in ["gpt-6-astra", "gpt-6-astra-test"] {
            for asynchronous in [false, true] {
                let mut request = test_request();
                request.model = ModelRef::parse(&format!("chatgpt/{model}")).unwrap();
                request.tools[0].asynchronous = asynchronous;
                let cache = crate::provider::codecs::openai_responses::PromptCachePolicy::Disabled;
                let lite = crate::provider::codecs::openai_responses::encode_responses_lite(
                    &request, &cache,
                )
                .unwrap();
                let tools = lite["input"][0]["tools"][0]["tools"].as_array().unwrap();
                assert_eq!(tools.len(), request.tools.len());
                assert_eq!(tools[0]["name"], request.tools[0].name);
                assert_eq!(tools[0]["parameters"], request.tools[0].input_schema);
                assert!(tools.iter().all(|tool| tool.get("async").is_none()));
                let standard =
                    crate::provider::codecs::openai_responses::encode(&request, &cache).unwrap();
                assert_eq!(
                    standard["tools"][0].get("async"),
                    asynchronous.then_some(&Value::Bool(true))
                );
            }
        }
    }

    #[test]
    fn responses_lite_images_omit_detail() {
        let mut request = test_request();
        request.model.provider = "chatgpt".into();
        request.input = vec![ModelInput::MultimodalMessage {
            role: MessageRole::User,
            content: vec![crate::ModelContentPart::Image {
                mime_type: "image/png".into(),
                sha256: String::new(),
                data: "aW1hZ2U=".into(),
                width: 1,
                height: 1,
            }],
        }];
        let body = crate::provider::codecs::openai_responses::encode_responses_lite(
            &request,
            &crate::provider::codecs::openai_responses::PromptCachePolicy::Disabled,
        )
        .unwrap();
        let image = &body["input"][2]["content"][0];
        assert_eq!(image["type"], "input_image");
        assert_eq!(image["image_url"], "data:image/png;base64,aW1hZ2U=");
        assert!(image.get("detail").is_none());
    }

    #[test]
    fn responses_lite_item_ids_are_stable_and_scoped_to_content_and_thread() {
        let mut request = test_request();
        for prefix in ["at", "msg"] {
            let id = stable_responses_lite_item_id(prefix, &request, &"original").unwrap();

            request.request_id = RequestId::new();
            request.attempt_id = AttemptId::new();
            assert_eq!(
                id,
                stable_responses_lite_item_id(prefix, &request, &"original").unwrap()
            );
            assert_ne!(
                id,
                stable_responses_lite_item_id(prefix, &request, &"changed").unwrap()
            );

            let mut other_thread = request.clone();
            other_thread
                .prompt_cache
                .as_mut()
                .unwrap()
                .key
                .push_str(":other");
            assert_ne!(
                id,
                stable_responses_lite_item_id(prefix, &other_thread, &"original").unwrap()
            );
        }
    }

    #[test]
    fn gpt_56_encodes_assistant_history_as_output_text_without_cache_breakpoint() {
        let mut request = test_request();
        request.model = ModelRef::parse("openai/gpt-5.6-sol").unwrap();
        request.input = vec![
            ModelInput::Message {
                role: MessageRole::User,
                content: "hello".into(),
            },
            ModelInput::Message {
                role: MessageRole::Assistant,
                content: "hi there".into(),
            },
        ];

        let body = request_body(&request).unwrap();

        assert_eq!(body["input"][0]["role"], "system");
        assert_eq!(body["input"][1]["role"], "user");
        assert_eq!(body["input"][1]["content"][0]["type"], "input_text");
        assert_eq!(body["input"][2]["role"], "assistant");
        assert_eq!(body["input"][2]["content"][0]["type"], "output_text");
        assert_eq!(body["input"][2]["content"][0]["text"], "hi there");
        assert!(
            body["input"][2]["content"][0]
                .get("prompt_cache_breakpoint")
                .is_none()
        );
    }

    #[test]
    fn cache_key_is_unchanged_when_only_the_conversation_suffix_changes() {
        let mut request = test_request();
        request.model = ModelRef::parse("openai/gpt-5.6-terra").unwrap();
        let first = request_body(&request).unwrap();
        request.input.push(ModelInput::ToolResult {
            call_id: "call-1".into(),
            output: json!({"content": "dynamic tool output"}),
            is_error: false,
        });
        let continuation = request_body(&request).unwrap();

        assert_eq!(
            first.get("prompt_cache_key"),
            continuation.get("prompt_cache_key")
        );
    }

    #[test]
    fn gpt_56_continuation_reuses_the_incorporated_system_prefix() {
        let mut request = test_request();
        request.model = ModelRef::parse("openai/gpt-5.6-sol").unwrap();
        request.response_transport_continuation = Some(crate::ResponseTransportContinuation {
            conversation_id: crate::ConversationId::new(),
            response_id: "resp_previous".into(),
            input_suffix_start: 0,
        });

        let body = request_body(&request).unwrap();

        assert_eq!(body["previous_response_id"], "resp_previous");
        assert!(
            body["input"]
                .as_array()
                .unwrap()
                .iter()
                .all(|item| item["role"] != "system")
        );
    }

    #[test]
    fn request_uses_the_conversation_scoped_cache_key() {
        let mut request = test_request();
        request.prompt_cache = Some(crate::PromptCacheRequest {
            key: "cagent:conversation:test-session".into(),
            scope: crate::PromptCacheScope::Conversation,
        });
        request.input.push(ModelInput::Message {
            role: MessageRole::System,
            content: "<cagent:collaboration-mode name=\"plan\">Plan.</cagent:collaboration-mode>"
                .into(),
        });

        let body = request_body(&request).unwrap();
        let expected = crate::provider::prompt_cache::native_cache_key(&request, "openai");

        assert_eq!(
            body.get("prompt_cache_key").and_then(Value::as_str),
            expected.as_deref()
        );
    }

    #[test]
    fn older_openai_models_request_extended_automatic_retention() {
        let mut request = test_request();
        request.model = ModelRef::parse("openai/gpt-5.4").unwrap();

        let body = request_body(&request).unwrap();

        assert_eq!(body["prompt_cache_retention"], "24h");
        assert!(body.get("prompt_cache_options").is_none());
        assert!(body.get("instructions").is_some());
    }

    #[test]
    fn chatgpt_request_uses_a_stable_cache_key_without_explicit_options() {
        let mut request = test_request();
        request.model.provider = "chatgpt".into();
        request.model.model = "gpt-5.6-luna".into();
        request.prompt_cache = Some(crate::PromptCacheRequest {
            key: "cagent:conversation:test-session".into(),
            scope: crate::PromptCacheScope::Conversation,
        });

        let body = request_body(&request).unwrap();
        let expected = crate::provider::prompt_cache::native_cache_key(&request, "chatgpt");

        assert_eq!(
            body.get("prompt_cache_key").and_then(Value::as_str),
            expected.as_deref()
        );
        assert!(
            body["instructions"]
                .as_str()
                .unwrap()
                .contains("Be concise")
        );
        assert!(body.get("prompt_cache_options").is_none());
    }

    #[test]
    fn compatibility_backends_keep_their_existing_instructions_wire_format() {
        let mut request = test_request();
        request.model = ModelRef::parse("github-copilot/gpt-5.6-sol").unwrap();
        let body = request_body(&request).unwrap();

        assert!(body.get("prompt_cache_key").is_none());
        assert!(body.get("prompt_cache_options").is_none());
        assert!(
            body["instructions"]
                .as_str()
                .unwrap()
                .contains("Be concise")
        );
        assert_eq!(body["input"][0]["role"], "user");
    }

    #[test]
    fn request_only_enables_strict_tools_for_closed_fully_required_object_schemas() {
        let mut request = test_request();
        request.tools = vec![
            ToolDefinition {
                name: "closed".into(),
                description: "Closed schema".into(),
                asynchronous: false,
                input_schema: json!({
                    "type": "object",
                    "properties": {"path": {"type": "string"}},
                    "required": ["path"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "optional".into(),
                description: "Closed schema with an optional property".into(),
                asynchronous: false,
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"},
                        "label": {"type": "string"}
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "environment".into(),
                description: "Free-form environment map".into(),
                asynchronous: false,
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "env": {
                            "type": "object",
                            "additionalProperties": {"type": "string"}
                        }
                    },
                    "required": ["env"],
                    "additionalProperties": false
                }),
            },
        ];

        let body = request_body(&request).unwrap();
        assert_eq!(body["tools"][0]["strict"], true);
        assert_eq!(body["tools"][1]["strict"], false);
        assert_eq!(body["tools"][2]["strict"], false);
    }

    #[test]
    fn completed_tool_continuation_is_stable_across_openai_retry_attempts() {
        let mut request = test_request();
        request.input.extend([
            ModelInput::ToolCall {
                call_id: "call-once".into(),
                name: "read".into(),
                arguments: json!({"path": "src/lib.rs"}),
                provider_metadata: Value::Null,
            },
            ModelInput::ToolResult {
                call_id: "call-once".into(),
                output: json!({"content": "source"}),
                is_error: false,
            },
        ]);

        let first = request_body(&request).unwrap();
        request.attempt_id = AttemptId::new();
        let retry = request_body(&request).unwrap();
        assert_eq!(retry, first);

        let outputs = first["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["type"] == "function_call_output")
            .collect::<Vec<_>>();
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0]["call_id"], "call-once");
        assert_eq!(
            serde_json::from_str::<Value>(outputs[0]["output"].as_str().unwrap()).unwrap(),
            json!({"output": {"content": "source"}, "is_error": false})
        );
    }

    #[test]
    fn recorded_events_normalize_text_tools_and_nullable_usage() {
        let mut tool_ids = BTreeMap::new();
        let reasoning = normalize_event(
            &json!({
                "type": "response.output_item.added",
                "item": {"type": "reasoning"},
            }),
            &mut tool_ids,
        )
        .unwrap();
        assert_eq!(reasoning, Some(ProviderStreamEvent::ReasoningStarted));
        let text = normalize_event(
            &json!({
                "type": "response.output_text.delta",
                "delta": "hello",
            }),
            &mut tool_ids,
        )
        .unwrap();
        assert_eq!(
            text,
            Some(ProviderStreamEvent::TextDelta {
                delta: "hello".into()
            })
        );
        let tool = normalize_event(
            &json!({
                "type": "response.output_item.added",
                "output_index": 2,
                "item": {
                    "type": "function_call",
                    "id": "item-1",
                    "call_id": "call-1",
                    "name": "read"
                },
            }),
            &mut tool_ids,
        )
        .unwrap();
        assert!(matches!(
            tool,
            Some(ProviderStreamEvent::ToolCallStarted {
                request_index: 2,
                ..
            })
        ));
        let arguments = normalize_event(
            &json!({
                "type": "response.function_call_arguments.delta",
                "item_id": "item-1",
                "delta": "{}",
            }),
            &mut tool_ids,
        )
        .unwrap();
        assert!(matches!(
            arguments,
            Some(ProviderStreamEvent::ToolArgumentsDelta { id, .. }) if id == "call-1"
        ));
        let completed = normalize_event(
            &json!({
                "type": "response.completed",
                "response": {
                    "id": "resp-1",
                    "status": "completed",
                    "service_tier": "priority",
                    "usage": {
                        "input_tokens": 20,
                        "input_tokens_details": {"cached_tokens": 5, "cache_write_tokens": 3},
                        "output_tokens": 7,
                        "output_tokens_details": {"reasoning_tokens": 2},
                        "total_tokens": 27
                    }
                }
            }),
            &mut tool_ids,
        )
        .unwrap();
        let Some(ProviderStreamEvent::Completed { metadata }) = completed else {
            panic!("expected completion");
        };
        assert_eq!(metadata.provider_request_id.as_deref(), Some("resp-1"));
        assert_eq!(metadata.usage.non_cached_input_tokens, Some(15));
        assert_eq!(metadata.usage.cache_write_input_tokens, Some(3));
        assert_eq!(metadata.usage.reasoning_tokens, Some(2));
        assert_eq!(metadata.usage.provider_usage["service_tier"], "priority");
    }

    #[test]
    fn retryable_statuses_and_retry_after_are_normalized() {
        let error = ProviderError {
            kind: ProviderErrorKind::RateLimit,
            code: "rate_limit".into(),
            message: "slow down".into(),
            retryable: true,
            retry_after_millis: Some(2_000),
            status: Some(429),
            metadata: BTreeMap::new(),
        };
        assert_eq!(error.retry_after(), Some(Duration::from_secs(2)));
    }

    #[test]
    fn sse_boundaries_support_lf_and_crlf() {
        assert_eq!(find_sse_boundary(b"data: {}\n\nrest"), Some((8, 2)));
        assert_eq!(find_sse_boundary(b"data: {}\r\n\r\nrest"), Some((8, 4)));
    }

    #[test]
    fn test_constructor_keeps_injected_client_boundary() {
        let provider =
            OpenAiProvider::with_client("http://127.0.0.1:1/v1/", reqwest::Client::new());
        assert_eq!(provider.base_url, "http://127.0.0.1:1/v1");
    }

    #[test]
    fn opencode_uses_provider_api_discovery_and_the_responses_backend() {
        let provider = OpenAiProvider::open_code();
        let descriptor = provider.descriptor();
        assert_eq!(descriptor.id, "opencode");
        assert_eq!(descriptor.display_name, "OpenCode Zen");
        assert_eq!(
            descriptor.model_discovery,
            ModelDiscoverySource::ProviderApi
        );
        assert_eq!(
            descriptor.default_model_backend,
            Some(ModelBackend::OpenAiResponses)
        );
        assert_eq!(
            descriptor.credential_environment_variable.as_deref(),
            Some("OPENCODE_API_KEY")
        );
    }

    #[test]
    fn provider_model_list_parser_preserves_the_provider_identity() {
        let catalog = parse_model_catalog(
            "opencode",
            &json!({
                "object": "list",
                "data": [{"id": "gpt-5.6-luna", "owned_by": "opencode"}]
            }),
        )
        .unwrap();
        assert_eq!(catalog.provider, "opencode");
        assert_eq!(catalog.models[0].id, "gpt-5.6-luna");
    }

    #[tokio::test]
    async fn openai_uses_its_provider_models_endpoint() {
        assert!(OpenAiProvider::new().discover_models().is_some());
    }

    #[tokio::test]
    async fn websocket_response_create_omits_sse_stream_and_normalizes_events() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let Some(Ok(Message::Text(frame))) = socket.next().await else {
                panic!("expected response.create frame");
            };
            let payload: Value = serde_json::from_str(&frame).unwrap();
            assert_eq!(payload["type"], "response.create");
            assert!(payload.get("stream").is_none());
            socket
                .send(Message::Text(
                    json!({
                        "type": "response.output_text.delta",
                        "delta": "hello"
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            socket
                .send(Message::Text(
                    json!({
                        "type": "response.completed",
                        "response": {"id": "resp_1", "status": "completed", "usage": {}}
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
        });
        let (mut socket, _) = connect_async(format!("ws://{address}")).await.unwrap();
        let (sender, mut receiver) = mpsc::channel(4);
        stream_openai_websocket_request(
            &mut socket,
            &test_request(),
            &CancellationToken::new(),
            &sender,
        )
        .await
        .unwrap();
        assert!(
            matches!(receiver.recv().await.unwrap().unwrap(), ProviderStreamEvent::TextDelta { delta } if delta == "hello")
        );
        assert!(matches!(
            receiver.recv().await.unwrap().unwrap(),
            ProviderStreamEvent::Completed { .. }
        ));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn astra_websocket_sends_mid_turn_steering_and_reports_acceptance() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let Some(Ok(Message::Text(_))) = socket.next().await else {
                panic!("expected response.create frame");
            };
            socket
                .send(Message::Text(
                    json!({"type":"response.created","response":{"id":"resp_1"}})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            let Some(Ok(Message::Text(frame))) = socket.next().await else {
                panic!("expected response.steer frame");
            };
            let payload: Value = serde_json::from_str(&frame).unwrap();
            assert_eq!(
                payload,
                json!({
                    "type": "response.steer",
                    "previous_response_id": "resp_1",
                    "input": "focus on tests",
                })
            );
            socket
                .send(Message::Text(
                    json!({
                        "type":"response.incomplete",
                        "response":{"incomplete_details":{"reason":"steered"}}
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            socket
                .send(Message::Text(
                    json!({"type":"response.steer.accepted"}).to_string().into(),
                ))
                .await
                .unwrap();
            let Some(Ok(Message::Text(frame))) = socket.next().await else {
                panic!("expected second response.steer frame");
            };
            let payload: Value = serde_json::from_str(&frame).unwrap();
            assert_eq!(payload["input"], "keep the API stable");
            socket
                .send(Message::Text(
                    json!({
                        "type":"response.steer.failed",
                        "error":{"code":"busy","message":"try later"}
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            socket
                .send(Message::Text(
                    json!({"type":"response.completed","response":{"id":"resp_2","usage":{}}})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
        });
        let (mut socket, _) = connect_async(format!("ws://{address}")).await.unwrap();
        let (sender, mut receiver) = mpsc::channel(4);
        let mut request = test_request();
        request.model.model = "gpt-6-astra".into();
        let task = tokio::spawn(async move {
            stream_openai_websocket_request(
                &mut socket,
                &request,
                &CancellationToken::new(),
                &sender,
            )
            .await
        });
        let ProviderStreamEvent::Steerable { handle, .. } = receiver.recv().await.unwrap().unwrap()
        else {
            panic!("expected steerable event");
        };
        assert!(handle.send("focus on tests"));
        assert!(handle.send("keep the API stable"));
        assert!(matches!(
            receiver.recv().await.unwrap().unwrap(),
            ProviderStreamEvent::SteerAccepted { input } if input == "focus on tests"
        ));
        assert!(matches!(
            receiver.recv().await.unwrap().unwrap(),
            ProviderStreamEvent::SteerFailed { input, code, message }
                if input == "keep the API stable" && code == "busy" && message == "try later"
        ));
        assert!(matches!(
            receiver.recv().await.unwrap().unwrap(),
            ProviderStreamEvent::Completed { .. }
        ));
        task.await.unwrap().unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn opencode_fetches_its_own_model_endpoint_when_requested() {
        use std::io::{Read as _, Write as _};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let length = stream.read(&mut request).unwrap();
            let request = std::str::from_utf8(&request[..length]).unwrap();
            assert!(request.starts_with("GET /models HTTP/1.1"));
            assert!(request.lines().any(|line| {
                line.split_once(':').is_some_and(|(name, value)| {
                    name.eq_ignore_ascii_case("authorization") && value.trim() == "Bearer test-key"
                })
            }));
            let body = r#"{"object":"list","data":[{"id":"zen-model"}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let provider = OpenAiProvider::open_code()
            .with_endpoint(endpoint)
            .with_test_api_key("test-key");
        let catalog = provider.discover_models().unwrap().await.unwrap();
        server.join().unwrap();

        assert_eq!(catalog.provider, "opencode");
        assert_eq!(catalog.models[0].id, "zen-model");
    }
}
