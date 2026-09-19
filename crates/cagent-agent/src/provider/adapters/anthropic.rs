use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::header::CONTENT_TYPE;
use serde_json::{Value, json};
use tokio::sync::RwLock;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

use crate::{
    AuthState, CredentialSource, FinishReason, MessageRole, ModelBackend, ModelCapabilities,
    ModelCatalog, ModelDescriptor, ModelDiscoverySource, ModelInput, ModelRequest, ModelUsage,
    Provider, ProviderDescriptor, ProviderError, ProviderErrorKind, ProviderFuture, ProviderStream,
    ProviderStreamEvent, ResponseMetadata,
};

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com/v1";
const DEFAULT_KEY_VARIABLE: &str = "ANTHROPIC_API_KEY";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(5);

/// Anthropic Messages adapter with normalized text, tool, cache, and usage events.
#[derive(Clone, Debug)]
pub struct AnthropicProvider {
    client: reqwest::Client,
    base_url: String,
    key_variable: String,
    credentials: crate::provider::CredentialStore,
    api_key: Arc<RwLock<Option<crate::provider::ManagedApiKey>>>,
    descriptor: ProviderDescriptor,
    #[cfg(test)]
    api_key_override: Option<String>,
}

impl AnthropicProvider {
    #[must_use]
    pub fn new() -> Self {
        let credentials =
            crate::provider::CredentialStore::api_key(Path::new("."), "default", "anthropic");
        Self {
            client: reqwest::Client::new(),
            base_url: DEFAULT_BASE_URL.into(),
            key_variable: DEFAULT_KEY_VARIABLE.into(),
            api_key: Arc::new(RwLock::new(credentials.load_api_key().ok().flatten())),
            credentials,
            descriptor: ProviderDescriptor {
                id: "anthropic".into(),
                display_name: "Anthropic".into(),
                default_model_backend: Some(ModelBackend::AnthropicMessages),
                supported_model_backends: vec![ModelBackend::AnthropicMessages],
                model_discovery: ModelDiscoverySource::ProviderApi,
                credential_source: CredentialSource::ApiKey,
                credential_environment_variable: Some(DEFAULT_KEY_VARIABLE.into()),
                supports_managed_api_key: true,
                auth_flows: Vec::new(),
            },
            #[cfg(test)]
            api_key_override: None,
        }
    }

    #[must_use]
    pub fn with_credential_dir(mut self, data_dir: &Path) -> Self {
        self.credentials =
            crate::provider::CredentialStore::api_key(data_dir, "default", "anthropic");
        self.api_key = Arc::new(RwLock::new(self.credentials.load_api_key().ok().flatten()));
        self
    }

    #[must_use]
    pub fn with_environment_variable(mut self, variable: impl Into<String>) -> Self {
        self.key_variable = variable.into();
        self.descriptor.credential_environment_variable = Some(self.key_variable.clone());
        self
    }

    #[cfg(test)]
    pub(crate) fn with_test_endpoint(
        mut self,
        base_url: impl Into<String>,
        client: reqwest::Client,
        key: impl Into<String>,
    ) -> Self {
        self.base_url = base_url.into().trim_end_matches('/').into();
        self.client = client;
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
}

impl Default for AnthropicProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl Provider for AnthropicProvider {
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
        Some(Box::pin(async move {
            let key = self.resolved_api_key().await?;
            let response = tokio::time::timeout(
                DISCOVERY_TIMEOUT,
                self.client
                    .get(format!("{}/models", self.base_url))
                    .header("x-api-key", key)
                    .header("anthropic-version", ANTHROPIC_VERSION)
                    .send(),
            )
            .await
            .map_err(|_| {
                ProviderError::timeout(
                    "model_discovery_timeout",
                    "Anthropic model discovery timed out after five seconds",
                )
            })?
            .map_err(transport_error)?;
            if !response.status().is_success() {
                return Err(http_error(response).await);
            }
            let mut payload = response
                .bytes()
                .await
                .map(|bytes| bytes.to_vec())
                .map_err(transport_error)?;
            let payload = simd_json::serde::from_slice::<Value>(&mut payload).map_err(|error| {
                ProviderError::protocol("invalid_models_response", error.to_string())
            })?;
            parse_model_catalog(&payload)
        }))
    }

    fn stream(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> ProviderFuture<'_, Result<ProviderStream, ProviderError>> {
        Box::pin(async move {
            if request.model.provider != "anthropic" {
                return Err(ProviderError::configuration(format!(
                    "Anthropic adapter cannot serve {}",
                    request.model
                )));
            }
            if request
                .backend
                .is_some_and(|backend| backend != ModelBackend::AnthropicMessages)
            {
                return Err(ProviderError::configuration(
                    "Anthropic does not support the requested model backend",
                ));
            }
            let key = self.resolved_api_key().await?;
            let response = self
                .client
                .post(format!("{}/messages", self.base_url))
                .header("x-api-key", key)
                .header("anthropic-version", ANTHROPIC_VERSION)
                .header(CONTENT_TYPE, "application/json")
                .json(&request_body_with_policy(
                    &request,
                    crate::provider::codecs::anthropic_messages::PromptCachePolicy::OneHourRolling,
                )?)
                .send()
                .await
                .map_err(transport_error)?;
            if !response.status().is_success() {
                return Err(http_error(response).await);
            }
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

fn parse_model_catalog(payload: &Value) -> Result<ModelCatalog, ProviderError> {
    let entries = payload
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| ProviderError::protocol("invalid_models_response", "missing data array"))?;
    let models = entries
        .iter()
        .filter_map(|model| {
            let id = model.get("id")?.as_str()?.to_owned();
            Some(ModelDescriptor {
                display_name: model
                    .get("display_name")
                    .and_then(Value::as_str)
                    .unwrap_or(&id)
                    .to_owned(),
                id,
                capabilities: ModelCapabilities::default(),
                backend: Some(ModelBackend::AnthropicMessages),
                raw_metadata: model.clone(),
            })
        })
        .collect();
    Ok(ModelCatalog {
        provider: "anthropic".into(),
        models,
        version: None,
    })
}

#[cfg(test)]
pub(crate) fn request_body(request: &ModelRequest) -> Result<Value, ProviderError> {
    request_body_with_policy(
        request,
        crate::provider::codecs::anthropic_messages::PromptCachePolicy::OneHourRolling,
    )
}

pub(crate) fn request_body_with_policy(
    request: &ModelRequest,
    cache: crate::provider::codecs::anthropic_messages::PromptCachePolicy,
) -> Result<Value, ProviderError> {
    let mut system = request
        .stable_prompt
        .iter()
        .map(|part| json!({
            "type": "text",
            "text": format!("<cagent:{}>\n{}\n</cagent:{}>", part.identity, part.content, part.identity),
        }))
        .collect::<Vec<_>>();
    if cache == crate::provider::codecs::anthropic_messages::PromptCachePolicy::OneHourRolling
        && let Some(last) = system.last_mut()
    {
        last["cache_control"] = json!({ "type": "ephemeral", "ttl": "1h" });
    }
    let mut messages = Vec::new();
    for input in &request.input {
        messages.push(match input {
            ModelInput::Message { role, content } => json!({
                "role": match role {
                    MessageRole::Assistant => "assistant",
                    MessageRole::System | MessageRole::User => "user",
                },
                "content": [{ "type": "text", "text": content }],
            }),
            ModelInput::MultimodalMessage { role, content } => json!({
                "role": match role {
                    MessageRole::Assistant => "assistant",
                    MessageRole::System | MessageRole::User => "user",
                },
                "content": content.iter().map(|part| match part {
                    crate::ModelContentPart::Text { text } => json!({ "type": "text", "text": text }),
                    crate::ModelContentPart::Image { mime_type, data, .. } => json!({
                        "type": "image",
                        "source": { "type": "base64", "media_type": mime_type, "data": data }
                    }),
                }).collect::<Vec<_>>(),
            }),
            ModelInput::ToolCall { call_id, name, arguments, .. } => json!({
                "role": "assistant",
                "content": [{ "type": "tool_use", "id": call_id, "name": name, "input": arguments }],
            }),
            ModelInput::ToolResult { call_id, output, is_error } => json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": call_id,
                    "content": serde_json::to_string(output).map_err(|error| ProviderError::configuration(error.to_string()))?,
                    "is_error": is_error,
                }],
            }),
            ModelInput::ConfigurationUpdate { .. } | ModelInput::ProviderReasoning { .. } => continue,
        });
    }
    let mut tools = request
        .tools
        .iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "input_schema": tool.input_schema,
            })
        })
        .collect::<Vec<_>>();
    // Anthropic's cache boundary includes every preceding system block and
    // tool definition. Put it after the stable tool schema so repeated agent
    // turns reuse both the instructions and the (usually large) tool prefix.
    if cache == crate::provider::codecs::anthropic_messages::PromptCachePolicy::OneHourRolling
        && let Some(last) = tools.last_mut()
    {
        last["cache_control"] = json!({ "type": "ephemeral", "ttl": "1h" });
    }
    let mut body = json!({
        "model": request.model.model,
        "max_tokens": 16_384,
        "messages": messages,
        "system": system,
        "tools": tools,
        "stream": true,
    });
    if cache == crate::provider::codecs::anthropic_messages::PromptCachePolicy::OneHourRolling {
        body["cache_control"] = json!({ "type": "ephemeral", "ttl": "1h" });
    }
    if let Some(effort) = &request.effort {
        if uses_adaptive_thinking(&request.model.model) {
            body["thinking"] = json!({ "type": "adaptive" });
            body["output_config"] = json!({ "effort": effort });
        } else {
            body["thinking"] = json!({
                "type": "enabled",
                "budget_tokens": match effort.as_str() { "low" => 1_024, "high" => 8_192, _ => 4_096 },
            });
        }
    }
    if let Some(output) = &request.structured_output {
        let mut output_config = body
            .get("output_config")
            .cloned()
            .unwrap_or_else(|| json!({}));
        output_config["format"] = json!({
            "type": "json_schema",
            "schema": output.schema,
        });
        body["output_config"] = output_config;
    }
    Ok(body)
}

fn uses_adaptive_thinking(model: &str) -> bool {
    model
        .strip_prefix("claude-sonnet-5")
        .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with('-'))
}

#[derive(Default)]
struct StreamState {
    request_id: Option<String>,
    finish_reason: FinishReason,
    usage: ModelUsage,
    tool_ids: BTreeMap<u64, String>,
}

#[allow(clippy::too_many_lines)]
pub(crate) async fn map_sse(
    response: reqwest::Response,
    sender: mpsc::Sender<Result<ProviderStreamEvent, ProviderError>>,
    cancellation: CancellationToken,
) {
    let mut bytes = response.bytes_stream();
    let mut buffer = Vec::new();
    let mut state = StreamState::default();
    loop {
        let chunk = tokio::select! {
            biased;
            () = cancellation.cancelled() => { let _ = sender.send(Err(ProviderError::cancelled())).await; return; }
            chunk = bytes.next() => chunk,
        };
        let Some(chunk) = chunk else {
            let _ = sender
                .send(Err(ProviderError::protocol(
                    "stream_ended",
                    "Anthropic stream ended before message_stop",
                )))
                .await;
            return;
        };
        match chunk {
            Ok(chunk) => buffer.extend_from_slice(&chunk),
            Err(error) => {
                let _ = sender.send(Err(transport_error(error))).await;
                return;
            }
        }
        while let Some((end, delimiter)) = find_sse_boundary(&buffer) {
            let block = match String::from_utf8(buffer[..end].to_vec()) {
                Ok(block) => block.replace("\r\n", "\n"),
                Err(error) => {
                    let _ = sender
                        .send(Err(ProviderError::protocol(
                            "invalid_sse_utf8",
                            error.to_string(),
                        )))
                        .await;
                    return;
                }
            };
            buffer.drain(..end + delimiter);
            let data = block
                .lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .map(str::trim_start)
                .collect::<Vec<_>>()
                .join("\n");
            if data.is_empty() {
                continue;
            }
            let value = match simd_json::serde::from_slice::<Value>(&mut data.into_bytes()) {
                Ok(value) => value,
                Err(error) => {
                    let _ = sender
                        .send(Err(ProviderError::protocol(
                            "invalid_sse_json",
                            error.to_string(),
                        )))
                        .await;
                    return;
                }
            };
            let event_type = value
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let event = match event_type {
                "message_start" => {
                    state.request_id = value
                        .pointer("/message/id")
                        .and_then(Value::as_str)
                        .map(Into::into);
                    update_usage(&mut state.usage, value.pointer("/message/usage"));
                    None
                }
                "content_block_start"
                    if value.pointer("/content_block/type").and_then(Value::as_str)
                        == Some("tool_use") =>
                {
                    let index = value.get("index").and_then(Value::as_u64).unwrap_or(0);
                    let id = value
                        .pointer("/content_block/id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    state.tool_ids.insert(index, id.clone());
                    Some(Ok(ProviderStreamEvent::ToolCallStarted {
                        id,
                        name: value
                            .pointer("/content_block/name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .into(),
                        request_index: index,
                    }))
                }
                "content_block_start"
                    if matches!(
                        value.pointer("/content_block/type").and_then(Value::as_str),
                        Some("thinking" | "redacted_thinking")
                    ) =>
                {
                    Some(Ok(ProviderStreamEvent::ReasoningStarted))
                }
                "content_block_delta"
                    if value.pointer("/delta/type").and_then(Value::as_str)
                        == Some("text_delta") =>
                {
                    Some(Ok(ProviderStreamEvent::TextDelta {
                        delta: value
                            .pointer("/delta/text")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .into(),
                    }))
                }
                "content_block_delta"
                    if matches!(
                        value.pointer("/delta/type").and_then(Value::as_str),
                        Some("thinking_delta" | "signature_delta")
                    ) =>
                {
                    Some(Ok(ProviderStreamEvent::ReasoningStarted))
                }
                "content_block_delta"
                    if value.pointer("/delta/type").and_then(Value::as_str)
                        == Some("input_json_delta") =>
                {
                    let index = value.get("index").and_then(Value::as_u64).unwrap_or(0);
                    match state.tool_ids.get(&index) {
                        Some(id) => Some(Ok(ProviderStreamEvent::ToolArgumentsDelta {
                            id: id.clone(),
                            delta: value
                                .pointer("/delta/partial_json")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .into(),
                        })),
                        None => Some(Err(ProviderError::protocol(
                            "invalid_tool_delta",
                            "tool arguments preceded tool_use",
                        ))),
                    }
                }
                "message_delta" => {
                    update_usage(&mut state.usage, value.get("usage"));
                    state.finish_reason =
                        match value.pointer("/delta/stop_reason").and_then(Value::as_str) {
                            Some("tool_use") => FinishReason::ToolCalls,
                            Some("max_tokens") => FinishReason::Length,
                            Some("end_turn" | "stop_sequence") | None => FinishReason::Stop,
                            Some(_) => FinishReason::Other,
                        };
                    None
                }
                "message_stop" => {
                    let _ = sender
                        .send(Ok(ProviderStreamEvent::Completed {
                            metadata: ResponseMetadata {
                                provider_request_id: state.request_id,
                                finish_reason: state.finish_reason,
                                usage: state.usage,
                            },
                        }))
                        .await;
                    return;
                }
                "error" => Some(Err(stream_error(value.get("error").unwrap_or(&value)))),
                _ => None,
            };
            if let Some(event) = event
                && sender.send(event).await.is_err()
            {
                return;
            }
        }
    }
}

fn update_usage(target: &mut ModelUsage, usage: Option<&Value>) {
    let Some(usage) = usage else {
        return;
    };
    let provider_input = usage.get("input_tokens").and_then(Value::as_u64);
    let cache_read = usage.get("cache_read_input_tokens").and_then(Value::as_u64);
    let cache_write = usage
        .get("cache_creation_input_tokens")
        .and_then(Value::as_u64);
    let input_tokens = provider_input.map(|input| {
        input
            .saturating_add(cache_read.unwrap_or_default())
            .saturating_add(cache_write.unwrap_or_default())
    });
    target.merge_fragment(ModelUsage {
        input_tokens,
        cache_read_input_tokens: cache_read,
        cache_write_input_tokens: cache_write,
        output_tokens: usage.get("output_tokens").and_then(Value::as_u64),
        provider_usage: usage.clone(),
        ..ModelUsage::default()
    });
}

fn find_sse_boundary(buffer: &[u8]) -> Option<(usize, usize)> {
    crate::provider::wire::find_sse_boundary(buffer)
}

#[allow(clippy::needless_pass_by_value)]
fn transport_error(error: reqwest::Error) -> ProviderError {
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
    let payload = simd_json::serde::from_slice::<Value>(&mut error_body.body.into_bytes())
        .unwrap_or(Value::Null);
    ProviderError {
        kind: match status.as_u16() {
            401 | 403 => ProviderErrorKind::Authentication,
            408 => ProviderErrorKind::Timeout,
            429 => ProviderErrorKind::RateLimit,
            500..=599 => ProviderErrorKind::Server,
            _ => ProviderErrorKind::InvalidRequest,
        },
        code: payload
            .pointer("/error/type")
            .and_then(Value::as_str)
            .unwrap_or("http_error")
            .into(),
        message: payload
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("Anthropic request failed")
            .into(),
        retryable: matches!(status.as_u16(), 408 | 429 | 500..=599),
        retry_after_millis,
        status: Some(status.as_u16()),
        metadata: BTreeMap::new(),
    }
}

fn stream_error(error: &Value) -> ProviderError {
    ProviderError {
        kind: ProviderErrorKind::Server,
        code: error
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("stream_error")
            .into(),
        message: error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Anthropic stream failed")
            .into(),
        retryable: true,
        retry_after_millis: None,
        status: None,
        metadata: BTreeMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ModelRef;

    #[test]
    fn model_catalog_uses_provider_ids_and_anthropic_backend() {
        let catalog = parse_model_catalog(&json!({
            "data": [{
                "id": "claude-new",
                "display_name": "Claude New",
                "type": "model"
            }]
        }))
        .unwrap();

        assert_eq!(catalog.provider, "anthropic");
        assert_eq!(catalog.models[0].id, "claude-new");
        assert_eq!(catalog.models[0].display_name, "Claude New");
        assert_eq!(
            catalog.models[0].backend,
            Some(ModelBackend::AnthropicMessages)
        );
        assert_eq!(
            AnthropicProvider::new().descriptor().model_discovery,
            ModelDiscoverySource::ProviderApi
        );
    }

    #[tokio::test]
    async fn model_discovery_uses_anthropics_authenticated_models_endpoint() {
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
            assert!(request.starts_with("GET /models HTTP/1.1"));
            assert!(request.lines().any(|line| {
                line.split_once(':').is_some_and(|(name, value)| {
                    name.eq_ignore_ascii_case("x-api-key") && value.trim() == "secret-canary"
                })
            }));
            assert!(request.lines().any(|line| {
                line.split_once(':').is_some_and(|(name, value)| {
                    name.eq_ignore_ascii_case("anthropic-version")
                        && value.trim() == ANTHROPIC_VERSION
                })
            }));
            let body = r#"{"data":[{"id":"claude-new","display_name":"Claude New","type":"model"}],"has_more":false}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });

        let provider = AnthropicProvider::new().with_test_endpoint(
            format!("http://{address}"),
            reqwest::Client::new(),
            "secret-canary",
        );
        let catalog = provider.discover_models().unwrap().await.unwrap();
        server.await.unwrap();

        assert_eq!(catalog.models[0].id, "claude-new");
    }

    #[test]
    fn request_uses_native_tools_and_cache_control() {
        let adapter = AnthropicProvider::new().with_test_endpoint(
            "http://127.0.0.1:9",
            reqwest::Client::new(),
            "canary",
        );
        assert_eq!(adapter.base_url, "http://127.0.0.1:9");
        let request = ModelRequest {
            request_id: crate::RequestId::new(),
            attempt_id: crate::AttemptId::new(),
            model: ModelRef::parse("anthropic/claude-test").unwrap(),
            backend: Some(ModelBackend::AnthropicMessages),
            backend_candidates: vec![ModelBackend::AnthropicMessages],
            effort: None,
            service_tier: None,
            input: vec![ModelInput::Message {
                role: MessageRole::User,
                content: "hello".into(),
            }],
            tools: vec![crate::ToolDefinition {
                name: "read".into(),
                description: "Read".into(),
                input_schema: json!({"type":"object"}),
                asynchronous: false,
            }],
            stable_prompt: vec![crate::StablePromptPart {
                identity: "core".into(),
                content: "Be useful".into(),
            }],
            prompt_cache: None,
            response_transport_continuation: None,
            allow_parallel_tools: true,
            structured_output: None,
        };
        let body = request_body(&request).unwrap();
        assert_eq!(
            body.pointer("/system/0/cache_control/type")
                .and_then(Value::as_str),
            Some("ephemeral")
        );
        assert_eq!(
            body.pointer("/tools/0/name").and_then(Value::as_str),
            Some("read")
        );
        assert_eq!(
            body.pointer("/tools/0/cache_control/type")
                .and_then(Value::as_str),
            Some("ephemeral")
        );
    }

    #[test]
    fn structured_output_uses_anthropic_json_schema_output_format() {
        let mut request = ModelRequest {
            request_id: crate::RequestId::new(),
            attempt_id: crate::AttemptId::new(),
            model: ModelRef::parse("anthropic/claude-test").unwrap(),
            backend: Some(ModelBackend::AnthropicMessages),
            backend_candidates: vec![ModelBackend::AnthropicMessages],
            effort: None,
            service_tier: None,
            input: Vec::new(),
            tools: Vec::new(),
            stable_prompt: Vec::new(),
            prompt_cache: None,
            response_transport_continuation: None,
            allow_parallel_tools: true,
            structured_output: None,
        };
        request.structured_output = Some(crate::StructuredOutputRequest {
            schema: json!({"type": "object"}),
        });
        let body = request_body(&request).unwrap();
        assert_eq!(
            body.pointer("/output_config/format/type"),
            Some(&json!("json_schema"))
        );
        assert_eq!(
            body.pointer("/output_config/format/schema/type"),
            Some(&json!("object"))
        );
    }

    #[test]
    fn claude_sonnet_five_uses_adaptive_thinking_and_preserves_output_config() {
        let request = ModelRequest {
            request_id: crate::RequestId::new(),
            attempt_id: crate::AttemptId::new(),
            model: ModelRef::parse("anthropic/claude-sonnet-5").unwrap(),
            backend: Some(ModelBackend::AnthropicMessages),
            backend_candidates: vec![ModelBackend::AnthropicMessages],
            effort: Some("medium".into()),
            service_tier: None,
            input: Vec::new(),
            tools: Vec::new(),
            stable_prompt: Vec::new(),
            prompt_cache: None,
            response_transport_continuation: None,
            allow_parallel_tools: true,
            structured_output: Some(crate::StructuredOutputRequest {
                schema: json!({"type": "object"}),
            }),
        };

        let body = request_body(&request).unwrap();
        assert_eq!(
            body.pointer("/thinking/type").and_then(Value::as_str),
            Some("adaptive")
        );
        assert_eq!(
            body.pointer("/output_config/effort")
                .and_then(Value::as_str),
            Some("medium")
        );
        assert_eq!(
            body.pointer("/output_config/format/type")
                .and_then(Value::as_str),
            Some("json_schema")
        );
        assert!(body.pointer("/thinking/budget_tokens").is_none());
    }

    #[test]
    fn usage_distinguishes_cache_read_and_write() {
        let mut usage = ModelUsage::default();
        update_usage(
            &mut usage,
            Some(
                &json!({ "input_tokens": 5, "cache_read_input_tokens": 7, "cache_creation_input_tokens": 11, "output_tokens": 13 }),
            ),
        );
        assert_eq!(usage.input_tokens, Some(23));
        assert_eq!(usage.non_cached_input_tokens, Some(16));
        assert_eq!(usage.total_tokens, Some(36));
    }

    #[tokio::test]
    async fn recorded_messages_stream_normalizes_text_tools_and_cache_usage() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let Ok(listener) = tokio::net::TcpListener::bind("127.0.0.1:0").await else {
            return;
        };
        let address = listener.local_addr().unwrap();
        let body = concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"usage\":{\"input_tokens\":2,\"cache_read_input_tokens\":3}}}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hello\"}}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tool_1\",\"name\":\"read\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"a\\\"}\"}}\n\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":5}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n"
        );
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 8192];
            let _ = socket.read(&mut request).await.unwrap();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        let provider = AnthropicProvider::new().with_test_endpoint(
            format!("http://{address}"),
            reqwest::Client::new(),
            "secret-canary",
        );
        let request = ModelRequest {
            request_id: crate::RequestId::new(),
            attempt_id: crate::AttemptId::new(),
            model: ModelRef::parse("anthropic/test").unwrap(),
            backend: Some(ModelBackend::AnthropicMessages),
            backend_candidates: vec![ModelBackend::AnthropicMessages],
            effort: None,
            service_tier: None,
            input: vec![ModelInput::Message {
                role: MessageRole::User,
                content: "test".into(),
            }],
            tools: vec![],
            stable_prompt: vec![],
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
        assert!(events.iter().any(
            |event| matches!(event, ProviderStreamEvent::TextDelta { delta } if delta == "hello")
        ));
        assert!(events.iter().any(|event| matches!(event, ProviderStreamEvent::ToolCallStarted { id, name, .. } if id == "tool_1" && name == "read")));
        let usage = events
            .iter()
            .find_map(|event| match event {
                ProviderStreamEvent::Completed { metadata } => Some(&metadata.usage),
                _ => None,
            })
            .unwrap();
        assert_eq!(usage.cache_read_input_tokens, Some(3));
        assert_eq!(usage.total_tokens, Some(10));
    }
}
