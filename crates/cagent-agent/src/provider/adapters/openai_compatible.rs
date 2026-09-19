#![allow(clippy::field_reassign_with_default)] // Provider payloads start from a shared default before conditional fields are applied.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
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

const OPENROUTER_BASE_URL: &str = "https://openrouter.ai/api/v1";
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(5);

/// `OpenAI` Chat Completions adapter used by `OpenRouter` and configured compatible providers.
#[derive(Clone, Debug)]
pub struct OpenAiCompatibleProvider {
    client: reqwest::Client,
    provider_id: String,
    display_name: String,
    base_url: String,
    key_variable: String,
    credentials: crate::provider::CredentialStore,
    api_key: Arc<RwLock<Option<crate::provider::ManagedApiKey>>>,
    models_endpoint: Option<String>,
    openrouter: bool,
    descriptor: ProviderDescriptor,
    #[cfg(test)]
    api_key_override: Option<String>,
}

impl OpenAiCompatibleProvider {
    #[must_use]
    pub fn openrouter() -> Self {
        let mut provider = Self::custom(
            "openrouter",
            "OpenRouter",
            OPENROUTER_BASE_URL,
            "OPENROUTER_API_KEY",
            Some("models".into()),
        );
        provider.openrouter = true;
        provider
    }

    #[must_use]
    pub fn custom(
        provider_id: impl Into<String>,
        display_name: impl Into<String>,
        base_url: impl Into<String>,
        key_variable: impl Into<String>,
        models_endpoint: Option<String>,
    ) -> Self {
        let provider_id = provider_id.into();
        let display_name = display_name.into();
        let key_variable = key_variable.into();
        let credentials =
            crate::provider::CredentialStore::api_key(Path::new("."), "default", &provider_id);
        Self {
            client: reqwest::Client::new(),
            provider_id: provider_id.clone(),
            display_name: display_name.clone(),
            base_url: base_url.into().trim_end_matches('/').into(),
            key_variable: key_variable.clone(),
            api_key: Arc::new(RwLock::new(credentials.load_api_key().ok().flatten())),
            credentials,
            models_endpoint,
            openrouter: false,
            descriptor: ProviderDescriptor {
                id: provider_id,
                display_name,
                default_model_backend: Some(ModelBackend::OpenAiCompatible),
                supported_model_backends: vec![ModelBackend::OpenAiCompatible],
                model_discovery: ModelDiscoverySource::ProviderApi,
                credential_source: CredentialSource::ApiKey,
                credential_environment_variable: Some(key_variable),
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
            crate::provider::CredentialStore::api_key(data_dir, "default", &self.provider_id);
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

    fn endpoint(&self, endpoint: &str) -> String {
        if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
            endpoint.into()
        } else {
            format!("{}/{}", self.base_url, endpoint.trim_start_matches('/'))
        }
    }

    async fn send(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, ProviderError> {
        let response = request.send().await.map_err(transport_error)?;
        if response.status().is_success() {
            Ok(response)
        } else {
            Err(http_error(response).await)
        }
    }
}

impl Provider for OpenAiCompatibleProvider {
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
        self.models_endpoint.as_ref()?;
        Some(Box::pin(async move {
            let endpoint = self.models_endpoint.as_deref().ok_or_else(|| {
                ProviderError::configuration(format!(
                    "{} does not expose a model endpoint",
                    self.display_name
                ))
            })?;
            let key = self.resolved_api_key().await?;
            let response = tokio::time::timeout(
                DISCOVERY_TIMEOUT,
                self.send(self.client.get(self.endpoint(endpoint)).bearer_auth(key)),
            )
            .await
            .map_err(|_| {
                ProviderError::timeout(
                    "model_discovery_timeout",
                    "model discovery timed out after five seconds",
                )
            })??;
            let mut payload =
                response
                    .bytes()
                    .await
                    .map(|bytes| bytes.to_vec())
                    .map_err(|error| {
                        ProviderError::protocol("invalid_models_response", error.to_string())
                    })?;
            let payload = simd_json::serde::from_slice::<Value>(&mut payload).map_err(|error| {
                ProviderError::protocol("invalid_models_response", error.to_string())
            })?;
            parse_model_catalog(&self.provider_id, &payload, self.openrouter)
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
                .is_some_and(|backend| backend != ModelBackend::OpenAiCompatible)
            {
                return Err(ProviderError::configuration(format!(
                    "{} does not support the requested model backend",
                    self.display_name
                )));
            }
            let key = self.resolved_api_key().await?;
            let response = self
                .send(
                    self.client
                        .post(self.endpoint("chat/completions"))
                        .header(AUTHORIZATION, format!("Bearer {key}"))
                        .header(CONTENT_TYPE, "application/json")
                        .json(&request_body_with_policy(
                            &request,
                            &prompt_cache_policy(self.openrouter, &request),
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

fn prompt_cache_policy(
    openrouter: bool,
    request: &ModelRequest,
) -> crate::provider::codecs::openai_chat::PromptCachePolicy {
    use crate::provider::codecs::openai_chat::PromptCachePolicy;
    if !openrouter {
        return PromptCachePolicy::Disabled;
    }
    let Some(cache) = request.prompt_cache.as_ref() else {
        return PromptCachePolicy::OpenRouterAutomatic { session_id: None };
    };
    if request.model.model.starts_with("anthropic/") {
        PromptCachePolicy::OpenRouterClaude {
            session_id: cache.key.clone(),
            ttl: "1h",
        }
    } else if request.model.model.starts_with("openai/gpt-5.6") {
        PromptCachePolicy::OpenRouterExplicit {
            session_id: cache.key.clone(),
            ttl: "30m",
        }
    } else {
        PromptCachePolicy::OpenRouterAutomatic {
            session_id: Some(cache.key.clone()),
        }
    }
}

fn parse_model_catalog(
    provider_id: &str,
    payload: &Value,
    openrouter: bool,
) -> Result<ModelCatalog, ProviderError> {
    let entries = payload
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| ProviderError::protocol("invalid_models_response", "missing data array"))?;
    let models = entries
        .iter()
        .filter_map(|model| {
            let id = model.get("id")?.as_str()?.to_owned();
            let supported = model.get("supported_parameters").and_then(Value::as_array);
            let has = |parameter: &str| {
                supported.is_some_and(|values| {
                    values.iter().any(|value| value.as_str() == Some(parameter))
                })
            };
            let native_reasoning_efforts = model
                .pointer("/reasoning/supported_efforts")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .map(ToOwned::to_owned)
                        .collect::<Vec<_>>()
                })
                .filter(|values| !values.is_empty());
            let supports_reasoning =
                native_reasoning_efforts.is_some() || has("reasoning") || has("reasoning_effort");
            let input = model
                .pointer("/architecture/input_modalities")
                .and_then(Value::as_array);
            let output = model
                .pointer("/architecture/output_modalities")
                .and_then(Value::as_array);
            let text = |modalities: Option<&Vec<Value>>| {
                modalities.map(|values| values.iter().any(|value| value.as_str() == Some("text")))
            };
            Some(ModelDescriptor {
                display_name: model
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(&id)
                    .to_owned(),
                id,
                capabilities: if openrouter {
                    ModelCapabilities {
                        context_window: model
                            .pointer("/top_provider/context_length")
                            .and_then(Value::as_u64)
                            .or_else(|| model.get("context_length").and_then(Value::as_u64)),
                        supports_streaming: Some(true),
                        supports_tools: Some(has("tools")),
                        supports_structured_output: supported
                            .is_some()
                            .then(|| has("response_format") || has("structured_outputs")),
                        supports_text_input: text(input),
                        supports_image_input: input.map(|values| {
                            values.iter().any(|value| value.as_str() == Some("image"))
                        }),
                        supports_text_output: text(output),
                        supports_fast_mode: None,
                        reasoning_control: supports_reasoning
                            .then_some(crate::ReasoningControl::Effort),
                        reasoning_efforts: native_reasoning_efforts.or_else(|| {
                            supports_reasoning
                                .then(|| vec!["low".into(), "medium".into(), "high".into()])
                        }),
                    }
                } else {
                    ModelCapabilities::default()
                },
                backend: Some(ModelBackend::OpenAiCompatible),
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
            .map(Into::into),
    })
}

#[cfg(test)]
pub(crate) fn request_body(
    request: &ModelRequest,
    openrouter: bool,
) -> Result<Value, ProviderError> {
    use crate::provider::codecs::openai_chat::PromptCachePolicy;
    let cache = if openrouter {
        let session_id = request.prompt_cache.as_ref().map(|cache| cache.key.clone());
        match session_id {
            None => PromptCachePolicy::OpenRouterAutomatic { session_id: None },
            Some(session_id) if request.model.model.starts_with("anthropic/") => {
                PromptCachePolicy::OpenRouterClaude {
                    session_id,
                    ttl: "1h",
                }
            }
            Some(session_id) if request.model.model.starts_with("openai/gpt-5.6") => {
                PromptCachePolicy::OpenRouterExplicit {
                    session_id,
                    ttl: "30m",
                }
            }
            Some(session_id) => PromptCachePolicy::OpenRouterAutomatic {
                session_id: Some(session_id),
            },
        }
    } else {
        PromptCachePolicy::Disabled
    };
    request_body_with_policy(request, &cache)
}

pub(crate) fn request_body_with_policy(
    request: &ModelRequest,
    cache: &crate::provider::codecs::openai_chat::PromptCachePolicy,
) -> Result<Value, ProviderError> {
    let explicit = matches!(
        cache,
        crate::provider::codecs::openai_chat::PromptCachePolicy::OpenRouterExplicit { .. }
    );
    let mut messages = request
        .stable_prompt
        .iter()
        .enumerate()
        .map(|(index, part)| {
            let text = format!(
                "<cagent:{}>\n{}\n</cagent:{}>",
                part.identity, part.content, part.identity
            );
            if explicit && index + 1 == request.stable_prompt.len() {
                json!({
                    "role": "system",
                    "content": [{
                        "type": "text",
                        "text": text,
                        "prompt_cache_breakpoint": { "mode": "explicit" },
                    }],
                })
            } else {
                json!({ "role": "system", "content": text })
            }
        })
        .collect::<Vec<_>>();
    for input in &request.input {
        messages.push(match input {
            ModelInput::Message { role, content } => {
                let role = match role {
                    MessageRole::System => "system",
                    MessageRole::User => "user",
                    MessageRole::Assistant => "assistant",
                };
                if explicit {
                    json!({
                        "role": role,
                        "content": [{
                            "type": "text",
                            "text": content,
                            "prompt_cache_breakpoint": { "mode": "explicit" },
                        }],
                    })
                } else {
                    json!({ "role": role, "content": content })
                }
            }
            ModelInput::MultimodalMessage { role, content } => {
                let role = match role {
                    MessageRole::System => "system",
                    MessageRole::User => "user",
                    MessageRole::Assistant => "assistant",
                };
                json!({
                    "role": role,
                    "content": content.iter().map(|part| match part {
                        crate::ModelContentPart::Text { text } => json!({ "type": "text", "text": text }),
                        crate::ModelContentPart::Image { mime_type, data, .. } => json!({
                            "type": "image_url",
                            "image_url": { "url": format!("data:{mime_type};base64,{data}") },
                        }),
                    }).collect::<Vec<_>>(),
                })
            }
            ModelInput::ToolCall { call_id, name, arguments, .. } => json!({
                "role": "assistant",
                "tool_calls": [{
                    "id": call_id,
                    "type": "function",
                    "function": { "name": name, "arguments": serde_json::to_string(arguments).map_err(|error| ProviderError::configuration(error.to_string()))? },
                }],
            }),
            ModelInput::ToolResult { call_id, output, is_error } => json!({
                "role": "tool",
                "tool_call_id": call_id,
                "content": serde_json::to_string(&json!({ "output": output, "is_error": is_error })).map_err(|error| ProviderError::configuration(error.to_string()))?,
            }),
            ModelInput::ConfigurationUpdate { .. } | ModelInput::ProviderReasoning { .. } => continue,
        });
    }
    let tools = request
        .tools
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.input_schema,
                },
            })
        })
        .collect::<Vec<_>>();
    let mut body = json!({
        "model": request.model.model,
        "messages": messages,
        "tools": tools,
        "stream": true,
        "stream_options": { "include_usage": true },
        "parallel_tool_calls": request.allow_parallel_tools,
    });
    match cache {
        crate::provider::codecs::openai_chat::PromptCachePolicy::Disabled => {}
        crate::provider::codecs::openai_chat::PromptCachePolicy::OpenRouterAutomatic {
            session_id,
        } => {
            if let Some(session_id) = session_id {
                body["session_id"] = Value::String(session_id.clone());
            }
        }
        crate::provider::codecs::openai_chat::PromptCachePolicy::OpenRouterClaude {
            session_id,
            ttl,
        } => {
            body["session_id"] = Value::String(session_id.clone());
            body["cache_control"] = json!({ "type": "ephemeral", "ttl": ttl });
        }
        crate::provider::codecs::openai_chat::PromptCachePolicy::OpenRouterExplicit {
            session_id,
            ttl,
        } => {
            body["session_id"] = Value::String(session_id.clone());
            body["prompt_cache_key"] = Value::String(session_id.clone());
            body["prompt_cache_options"] = json!({ "mode": "explicit", "ttl": ttl });
        }
    }
    if let Some(effort) = &request.effort {
        if !matches!(
            cache,
            crate::provider::codecs::openai_chat::PromptCachePolicy::Disabled
        ) {
            body["reasoning"] = match effort.as_str() {
                "off" => json!({ "enabled": false }),
                "on" => json!({ "enabled": true }),
                _ => json!({ "effort": effort }),
            };
        } else {
            body["reasoning_effort"] = Value::String(effort.clone());
        }
    }
    if let Some(service_tier) = &request.service_tier {
        body["service_tier"] = Value::String(service_tier.clone());
    }
    if let Some(output) = &request.structured_output {
        body["response_format"] = json!({
            "type": "json_schema",
            "json_schema": {
                "name": "cagent_output",
                "strict": true,
                "schema": output.schema,
            }
        });
    }
    Ok(body)
}

#[derive(Default)]
struct StreamState {
    request_id: Option<String>,
    finish_reason: Option<FinishReason>,
    usage: ModelUsage,
    tools: BTreeMap<u64, String>,
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
            () = cancellation.cancelled() => {
                let _ = sender.send(Err(ProviderError::cancelled())).await;
                return;
            }
            chunk = bytes.next() => chunk,
        };
        let Some(chunk) = chunk else {
            let _ = sender
                .send(Err(ProviderError::protocol(
                    "stream_ended",
                    "chat completion stream ended before [DONE]",
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
            if data == "[DONE]" {
                let _ = sender
                    .send(Ok(ProviderStreamEvent::Completed {
                        metadata: ResponseMetadata {
                            provider_request_id: state.request_id,
                            finish_reason: state.finish_reason.unwrap_or(FinishReason::Stop),
                            usage: state.usage,
                        },
                    }))
                    .await;
                return;
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
            if let Some(error) = value.get("error") {
                let _ = sender.send(Err(stream_error(error))).await;
                return;
            }
            state.request_id = value
                .get("id")
                .and_then(Value::as_str)
                .map(Into::into)
                .or(state.request_id);
            update_usage(&mut state.usage, value.get("usage"));
            let Some(choice) = value
                .get("choices")
                .and_then(Value::as_array)
                .and_then(|choices| choices.first())
            else {
                continue;
            };
            if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                state.finish_reason = Some(match reason {
                    "stop" => FinishReason::Stop,
                    "tool_calls" => FinishReason::ToolCalls,
                    "length" => FinishReason::Length,
                    "content_filter" => FinishReason::ContentFilter,
                    _ => FinishReason::Other,
                });
            }
            let delta = &choice["delta"];
            let reasoning_delta = delta
                .get("reasoning_content")
                .or_else(|| delta.get("reasoning"))
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty())
                || delta
                    .get("reasoning_details")
                    .and_then(Value::as_array)
                    .is_some_and(|details| !details.is_empty());
            if reasoning_delta
                && sender
                    .send(Ok(ProviderStreamEvent::ReasoningStarted))
                    .await
                    .is_err()
            {
                return;
            }
            if let Some(content) = delta.get("content").and_then(Value::as_str)
                && sender
                    .send(Ok(ProviderStreamEvent::TextDelta {
                        delta: content.into(),
                    }))
                    .await
                    .is_err()
            {
                return;
            }
            if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for tool in tool_calls {
                    let index = tool.get("index").and_then(Value::as_u64).unwrap_or(0);
                    if let Some(id) = tool.get("id").and_then(Value::as_str) {
                        state.tools.insert(index, id.into());
                        let name = tool
                            .pointer("/function/name")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        if sender
                            .send(Ok(ProviderStreamEvent::ToolCallStarted {
                                id: id.into(),
                                name: name.into(),
                                request_index: index,
                            }))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    if let Some(arguments) =
                        tool.pointer("/function/arguments").and_then(Value::as_str)
                        && !arguments.is_empty()
                    {
                        let Some(id) = state.tools.get(&index) else {
                            let _ = sender
                                .send(Err(ProviderError::protocol(
                                    "invalid_tool_delta",
                                    "tool arguments preceded the tool ID",
                                )))
                                .await;
                            return;
                        };
                        if sender
                            .send(Ok(ProviderStreamEvent::ToolArgumentsDelta {
                                id: id.clone(),
                                delta: arguments.into(),
                            }))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                }
            }
        }
    }
}

fn update_usage(target: &mut ModelUsage, usage: Option<&Value>) {
    let Some(usage) = usage.filter(|usage| !usage.is_null()) else {
        return;
    };
    let mut fragment = ModelUsage::default();
    fragment.input_tokens = usage.get("prompt_tokens").and_then(Value::as_u64);
    fragment.cache_read_input_tokens = usage
        .pointer("/prompt_tokens_details/cached_tokens")
        .and_then(Value::as_u64);
    fragment.cache_write_input_tokens = usage
        .pointer("/prompt_tokens_details/cache_write_tokens")
        .and_then(Value::as_u64);
    fragment.output_tokens = usage.get("completion_tokens").and_then(Value::as_u64);
    fragment.reasoning_tokens = usage
        .pointer("/completion_tokens_details/reasoning_tokens")
        .and_then(Value::as_u64);
    fragment.total_tokens = usage.get("total_tokens").and_then(Value::as_u64);
    fragment.provider_usage = usage.clone();
    let reported_cost = usage.get("cost").and_then(|cost| {
        cost.as_str()
            .map(str::to_owned)
            .or_else(|| cost.as_number().map(ToString::to_string))
    });
    if let Some(total_cost) = reported_cost {
        fragment.cost = Some(crate::ModelCost {
            total_cost: Some(total_cost),
            currency: "USD".into(),
            pricing_source: "provider_reported".into(),
            pricing_version: "response".into(),
            ..crate::ModelCost::default()
        });
    }
    target.merge_fragment(fragment);
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
    let parsed = simd_json::serde::from_slice::<Value>(&mut error_body.body.into_bytes())
        .unwrap_or(Value::Null);
    let message = parsed
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("provider request failed");
    ProviderError {
        kind: match status.as_u16() {
            401 | 403 => ProviderErrorKind::Authentication,
            408 => ProviderErrorKind::Timeout,
            429 => ProviderErrorKind::RateLimit,
            500..=599 => ProviderErrorKind::Server,
            _ => ProviderErrorKind::InvalidRequest,
        },
        code: parsed
            .pointer("/error/code")
            .and_then(Value::as_str)
            .unwrap_or("http_error")
            .into(),
        message: message.into(),
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
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("stream_error")
            .into(),
        message: error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("provider stream failed")
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
    fn openrouter_catalog_uses_its_api_capabilities_and_pricing() {
        let adapter = OpenAiCompatibleProvider::openrouter().with_test_endpoint(
            "http://127.0.0.1:9",
            reqwest::Client::new(),
            "canary",
        );
        assert_eq!(adapter.base_url, "http://127.0.0.1:9");
        let payload = json!({ "data": [{
            "id": "anthropic/claude-test",
            "name": "Claude Test",
            "context_length": 100_000,
            "top_provider": { "context_length": 80_000 },
            "architecture": { "input_modalities": ["text"], "output_modalities": ["text"] },
            "supported_parameters": ["tools", "reasoning"],
            "pricing": { "prompt": "0.000003", "completion": "0.000015" }
        }] });
        let catalog = parse_model_catalog("openrouter", &payload, true).unwrap();
        let model = &catalog.models[0];
        assert_eq!(model.capabilities.context_window, Some(80_000));
        assert_eq!(model.capabilities.supports_tools, Some(true));
        assert_eq!(
            model.capabilities.reasoning_efforts.as_ref().unwrap().len(),
            3
        );
        assert_eq!(
            model
                .raw_metadata
                .pointer("/pricing/prompt")
                .and_then(Value::as_str),
            Some("0.000003")
        );
    }

    #[test]
    fn openrouter_catalog_uses_provider_advertised_reasoning_efforts() {
        let payload = json!({ "data": [{
            "id": "openai/gpt-6-astra",
            "name": "OpenAI: GPT-6 Astra",
            "context_length": 1_050_000,
            "architecture": { "input_modalities": ["text", "image"], "output_modalities": ["text"] },
            "supported_parameters": ["tools", "reasoning", "reasoning_effort"],
            "reasoning": {
                "mandatory": true,
                "supported_efforts": ["max", "xhigh", "high", "medium", "low"]
            }
        }] });

        let catalog = parse_model_catalog("openrouter", &payload, true).unwrap();
        let capabilities = &catalog.models[0].capabilities;

        assert_eq!(
            capabilities.reasoning_control,
            Some(crate::ReasoningControl::Effort)
        );
        assert_eq!(
            capabilities.reasoning_efforts.as_deref(),
            Some(
                ["max", "xhigh", "high", "medium", "low"]
                    .map(str::to_owned)
                    .as_slice()
            )
        );
    }

    #[test]
    fn openrouter_uses_nested_reasoning_parameter() {
        let request = ModelRequest {
            request_id: crate::RequestId::new(),
            attempt_id: crate::AttemptId::new(),
            model: ModelRef::parse("openrouter/openai/test").unwrap(),
            backend: Some(ModelBackend::OpenAiCompatible),
            backend_candidates: vec![ModelBackend::OpenAiCompatible],
            effort: Some("high".into()),
            service_tier: None,
            input: Vec::new(),
            tools: Vec::new(),
            stable_prompt: Vec::new(),
            prompt_cache: None,
            response_transport_continuation: None,
            allow_parallel_tools: true,
            structured_output: None,
        };
        let body = request_body(&request, true).unwrap();
        assert_eq!(
            body.pointer("/reasoning/effort").and_then(Value::as_str),
            Some("high")
        );
        assert!(body.get("reasoning_effort").is_none());
        assert!(body.get("service_tier").is_none());
    }

    #[test]
    fn compatible_request_includes_priority_service_tier() {
        let request = ModelRequest {
            request_id: crate::RequestId::new(),
            attempt_id: crate::AttemptId::new(),
            model: ModelRef::parse("openrouter/openai/test").unwrap(),
            backend: Some(ModelBackend::OpenAiCompatible),
            backend_candidates: vec![ModelBackend::OpenAiCompatible],
            effort: None,
            service_tier: Some("priority".into()),
            input: Vec::new(),
            tools: Vec::new(),
            stable_prompt: Vec::new(),
            prompt_cache: None,
            response_transport_continuation: None,
            allow_parallel_tools: true,
            structured_output: None,
        };
        let body = request_body(&request, true).unwrap();
        assert_eq!(body["service_tier"], "priority");
    }

    #[test]
    fn openrouter_uses_the_conversation_key_for_prompt_cache_stickiness() {
        let request = ModelRequest {
            request_id: crate::RequestId::new(),
            attempt_id: crate::AttemptId::new(),
            model: ModelRef::parse("openrouter/anthropic/claude-test").unwrap(),
            backend: Some(ModelBackend::OpenAiCompatible),
            backend_candidates: vec![ModelBackend::OpenAiCompatible],
            effort: None,
            service_tier: None,
            input: Vec::new(),
            tools: Vec::new(),
            stable_prompt: Vec::new(),
            prompt_cache: Some(crate::PromptCacheRequest {
                key: "cagent:conversation:test".into(),
                scope: crate::PromptCacheScope::Conversation,
            }),
            response_transport_continuation: None,
            allow_parallel_tools: true,
            structured_output: None,
        };

        let body = request_body(&request, true).unwrap();

        assert_eq!(
            body.get("session_id").and_then(Value::as_str),
            Some("cagent:conversation:test")
        );
        assert_eq!(
            body.pointer("/cache_control/type").and_then(Value::as_str),
            Some("ephemeral")
        );
        assert_eq!(
            body.pointer("/cache_control/ttl").and_then(Value::as_str),
            Some("1h")
        );
    }

    #[test]
    fn partial_usage_fragments_merge_without_losing_cache_fields() {
        let mut usage = ModelUsage::default();
        update_usage(
            &mut usage,
            Some(&json!({
                "prompt_tokens": 20,
                "prompt_tokens_details": {"cached_tokens": 5, "cache_write_tokens": 3}
            })),
        );
        update_usage(&mut usage, Some(&json!({ "completion_tokens": 7 })));

        assert_eq!(usage.input_tokens, Some(20));
        assert_eq!(usage.non_cached_input_tokens, Some(15));
        assert_eq!(usage.cache_read_input_tokens, Some(5));
        assert_eq!(usage.cache_write_input_tokens, Some(3));
        assert_eq!(usage.output_tokens, Some(7));
        assert_eq!(usage.total_tokens, Some(27));
    }

    #[test]
    fn openrouter_gpt_56_uses_explicit_cache_controls() {
        let request = ModelRequest {
            request_id: crate::RequestId::new(),
            attempt_id: crate::AttemptId::new(),
            model: ModelRef::parse("openrouter/openai/gpt-5.6-sol").unwrap(),
            backend: Some(ModelBackend::OpenAiCompatible),
            backend_candidates: vec![ModelBackend::OpenAiCompatible],
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
                key: "cagent:conversation:test".into(),
                scope: crate::PromptCacheScope::Conversation,
            }),
            response_transport_continuation: None,
            allow_parallel_tools: true,
            structured_output: None,
        };

        let body = request_body(&request, true).unwrap();

        assert_eq!(body["session_id"], "cagent:conversation:test");
        assert_eq!(body["prompt_cache_key"], "cagent:conversation:test");
        assert_eq!(body["prompt_cache_options"]["mode"], "explicit");
        assert_eq!(body["prompt_cache_options"]["ttl"], "30m");
        assert_eq!(
            body.pointer("/messages/0/content/0/prompt_cache_breakpoint/mode"),
            Some(&json!("explicit"))
        );
    }

    #[test]
    fn structured_output_uses_strict_chat_completion_json_schema_format() {
        let mut request = ModelRequest {
            request_id: crate::RequestId::new(),
            attempt_id: crate::AttemptId::new(),
            model: ModelRef::parse("openrouter/openai/test").unwrap(),
            backend: Some(ModelBackend::OpenAiCompatible),
            backend_candidates: vec![ModelBackend::OpenAiCompatible],
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
        let body = request_body(&request, true).unwrap();
        assert_eq!(
            body.pointer("/response_format/type"),
            Some(&json!("json_schema"))
        );
        assert_eq!(
            body.pointer("/response_format/json_schema/name"),
            Some(&json!("cagent_output"))
        );
        assert_eq!(
            body.pointer("/response_format/json_schema/strict"),
            Some(&json!(true))
        );
    }

    #[test]
    fn openrouter_uses_enabled_for_toggle_reasoning() {
        let mut request = ModelRequest {
            request_id: crate::RequestId::new(),
            attempt_id: crate::AttemptId::new(),
            model: ModelRef::parse("openrouter/z-ai/glm-5.1").unwrap(),
            backend: Some(ModelBackend::OpenAiCompatible),
            backend_candidates: vec![ModelBackend::OpenAiCompatible],
            effort: Some("on".into()),
            service_tier: None,
            input: Vec::new(),
            tools: Vec::new(),
            stable_prompt: Vec::new(),
            prompt_cache: None,
            response_transport_continuation: None,
            allow_parallel_tools: true,
            structured_output: None,
        };
        let enabled = request_body(&request, true).unwrap();
        assert_eq!(
            enabled.pointer("/reasoning/enabled"),
            Some(&Value::Bool(true))
        );

        request.effort = Some("off".into());
        let disabled = request_body(&request, true).unwrap();
        assert_eq!(
            disabled.pointer("/reasoning/enabled"),
            Some(&Value::Bool(false))
        );
        assert!(disabled.pointer("/reasoning/effort").is_none());
    }

    #[tokio::test]
    async fn recorded_chat_completion_stream_normalizes_tools_usage_and_provider_cost() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let Ok(listener) = tokio::net::TcpListener::bind("127.0.0.1:0").await else {
            return;
        };
        let address = listener.local_addr().unwrap();
        let first = json!({"id":"request_1","choices":[{"delta":{"content":"hello","tool_calls":[{"index":0,"id":"call_1","function":{"name":"read","arguments":"{\"path\":"}}]}}]});
        let second = json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"a\"}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":3,"prompt_tokens_details":{"cached_tokens":1,"cache_write_tokens":2},"completion_tokens":5,"total_tokens":8,"cost":"0.00125"}});
        let body = format!("data: {first}\n\ndata: {second}\n\ndata: [DONE]\n\n");
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
        let provider = OpenAiCompatibleProvider::openrouter().with_test_endpoint(
            format!("http://{address}"),
            reqwest::Client::new(),
            "secret-canary",
        );
        let request = ModelRequest {
            request_id: crate::RequestId::new(),
            attempt_id: crate::AttemptId::new(),
            model: ModelRef::parse("openrouter/test").unwrap(),
            backend: Some(ModelBackend::OpenAiCompatible),
            backend_candidates: vec![ModelBackend::OpenAiCompatible],
            effort: Some("low".into()),
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
        let metadata = events
            .iter()
            .find_map(|event| match event {
                ProviderStreamEvent::Completed { metadata } => Some(metadata),
                _ => None,
            })
            .unwrap();
        assert_eq!(metadata.finish_reason, FinishReason::ToolCalls);
        assert_eq!(metadata.usage.total_tokens, Some(8));
        assert_eq!(metadata.usage.cache_read_input_tokens, Some(1));
        assert_eq!(metadata.usage.cache_write_input_tokens, Some(2));
        assert_eq!(
            metadata
                .usage
                .cost
                .as_ref()
                .and_then(|cost| cost.total_cost.as_deref()),
            Some("0.00125")
        );
    }
}
