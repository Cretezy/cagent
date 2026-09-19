#![allow(clippy::unnecessary_lazy_evaluations)] // Codec fallback values are intentionally kept adjacent to their conversion.
//! Google GenerateContent request and SSE response conversion.

use std::collections::{BTreeMap, HashMap};
use std::ops::Range;

use futures_util::StreamExt as _;
use serde_json::{Map, Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::{
    FinishReason, MessageRole, ModelInput, ModelRequest, ModelUsage, ProviderError,
    ProviderStreamEvent, ResponseMetadata,
};

use super::super::wire::{find_sse_boundary, read_http_error};

/// The documented sentinel lets pre-signature history work with Gemini 3.
pub(crate) const LEGACY_THOUGHT_SIGNATURE: &str = "skip_thought_signature_validator";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GeminiProfile {
    Developer,
    Vertex,
    VertexExpress,
    Zen,
}

impl GeminiProfile {
    pub(crate) const fn uses_function_call_ids(self) -> bool {
        !matches!(self, Self::Vertex)
    }

    pub(crate) const fn streams_function_arguments(self) -> bool {
        // Vertex's preview incremental-argument protocol is deliberately not
        // used. Stable v1 still streams complete functionCall parts.
        true
    }
}

pub(crate) fn request_body(
    request: &ModelRequest,
    profile: GeminiProfile,
) -> Result<Value, ProviderError> {
    request_body_with_cached_content(request, profile, None, 0)
}

/// Builds an immutable Google cached-content resource through `boundary`.
pub(crate) fn cached_content_body(
    request: &ModelRequest,
    profile: GeminiProfile,
    model_resource: &str,
    boundary: usize,
) -> Result<Option<Value>, ProviderError> {
    if boundary > request.input.len() {
        return Ok(None);
    }
    if request.stable_prompt.is_empty()
        && request.tools.is_empty()
        && request.input.iter().all(|input| {
            !matches!(
                input,
                ModelInput::Message {
                    role: MessageRole::System,
                    ..
                }
            )
        })
        && boundary == 0
    {
        return Ok(None);
    }
    let mut body = json!({
        "model": model_resource,
        "ttl": "3600s",
    });
    // GenerateContent forbids overriding system instructions when it uses a
    // cachedContent resource, so every system instruction is cache-owned.
    if let Some(system_instruction) = system_instruction(request, 0..request.input.len(), true) {
        body["systemInstruction"] = system_instruction;
    }
    if let Some((tools, tool_config)) = function_tools(request) {
        body["tools"] = tools;
        body["toolConfig"] = tool_config;
    }
    let contents = render_contents(request, profile, 0..boundary)?;
    if !contents.is_empty() {
        body["contents"] = Value::Array(contents);
    }
    Ok(Some(body))
}

/// Builds a request which references an already-created cached-content resource.
pub(crate) fn request_body_with_cached_content(
    request: &ModelRequest,
    profile: GeminiProfile,
    cached_content: Option<&str>,
    cached_input_boundary: usize,
) -> Result<Value, ProviderError> {
    if cached_input_boundary > request.input.len() {
        return Err(ProviderError::protocol(
            "invalid_cache_boundary",
            "cached Gemini input boundary exceeds the request input",
        ));
    }
    let contents = render_contents(request, profile, cached_input_boundary..request.input.len())?;
    let mut body = json!({ "contents": contents });
    if cached_content.is_none() {
        if let Some(system_instruction) = system_instruction(request, 0..request.input.len(), true)
        {
            body["systemInstruction"] = system_instruction;
        }
        if let Some((tools, tool_config)) = function_tools(request) {
            body["tools"] = tools;
            body["toolConfig"] = tool_config;
        }
    }
    if let Some(cached_content) = cached_content {
        body["cachedContent"] = Value::String(cached_content.into());
    }
    let mut generation = Map::new();
    if let Some(effort) = &request.effort {
        generation.insert(
            "thinkingConfig".into(),
            json!({ "thinkingLevel": effort.to_uppercase() }),
        );
    }
    if let Some(output) = &request.structured_output {
        generation.insert(
            "responseMimeType".into(),
            Value::String("application/json".into()),
        );
        generation.insert("responseJsonSchema".into(), project_schema(&output.schema));
    }
    if !generation.is_empty() {
        body["generationConfig"] = Value::Object(generation);
    }
    Ok(body)
}

fn render_contents(
    request: &ModelRequest,
    profile: GeminiProfile,
    range: Range<usize>,
) -> Result<Vec<Value>, ProviderError> {
    let mut calls = HashMap::<String, String>::new();
    for input in &request.input {
        if let ModelInput::ToolCall { call_id, name, .. } = input {
            calls.insert(call_id.clone(), name.clone());
        }
    }
    let mut contents = Vec::new();
    let mut index = range.start;
    while index < range.end {
        let input = &request.input[index];
        match input {
            ModelInput::Message { role, content } => {
                match role {
                    MessageRole::System => {}
                    MessageRole::User => contents.push(content_with_text("user", content)),
                    MessageRole::Assistant => contents.push(content_with_text("model", content)),
                }
                index += 1;
            }
            ModelInput::MultimodalMessage { role, content } => {
                let role = if matches!(role, MessageRole::Assistant) {
                    "model"
                } else {
                    "user"
                };
                let parts = content
                    .iter()
                    .map(|part| match part {
                        crate::ModelContentPart::Text { text } => json!({ "text": text }),
                        crate::ModelContentPart::Image {
                            mime_type, data, ..
                        } => json!({
                            "inlineData": { "mimeType": mime_type, "data": data }
                        }),
                    })
                    .collect::<Vec<_>>();
                contents.push(json!({ "role": role, "parts": parts }));
                index += 1;
            }
            ModelInput::ToolCall { .. } => {
                let mut parts = Vec::new();
                while index < range.end {
                    let ModelInput::ToolCall {
                        call_id,
                        name,
                        arguments,
                        provider_metadata,
                    } = &request.input[index]
                    else {
                        break;
                    };
                    let mut function_call = json!({ "name": name, "args": arguments });
                    if profile.uses_function_call_ids() {
                        function_call["id"] = Value::String(call_id.clone());
                    }
                    let thought_signature = provider_metadata
                        .pointer("/gemini/thought_signature")
                        .and_then(Value::as_str)
                        .unwrap_or(LEGACY_THOUGHT_SIGNATURE);
                    parts.push(json!({
                        "functionCall": function_call,
                        "thoughtSignature": thought_signature,
                    }));
                    index += 1;
                }
                contents.push(json!({ "role": "model", "parts": parts }));
            }
            ModelInput::ToolResult { .. } => {
                let mut parts = Vec::new();
                while index < range.end {
                    let ModelInput::ToolResult {
                        call_id,
                        output,
                        is_error,
                    } = &request.input[index]
                    else {
                        break;
                    };
                    let name = calls.get(call_id).ok_or_else(|| {
                        ProviderError::protocol(
                            "orphaned_tool_result",
                            format!(
                                "Google history has no function call for tool result {call_id}"
                            ),
                        )
                    })?;
                    let mut response = json!({
                        "name": name,
                        "response": { "output": output, "is_error": is_error },
                    });
                    if profile.uses_function_call_ids() {
                        response["id"] = Value::String(call_id.clone());
                    }
                    parts.push(json!({ "functionResponse": response }));
                    index += 1;
                }
                contents.push(json!({ "role": "user", "parts": parts }));
            }
            ModelInput::ConfigurationUpdate { .. } | ModelInput::ProviderReasoning { .. } => {
                index += 1
            }
        }
    }
    Ok(contents)
}

fn system_instruction(
    request: &ModelRequest,
    range: Range<usize>,
    include_stable: bool,
) -> Option<Value> {
    let mut system = if include_stable {
        request
            .stable_prompt
            .iter()
            .map(|part| {
                format!(
                    "<cagent:{}>\n{}\n</cagent:{}>",
                    part.identity, part.content, part.identity
                )
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    system.extend(request.input[range].iter().filter_map(|input| match input {
        ModelInput::Message {
            role: MessageRole::System,
            content,
        } => Some(content.clone()),
        _ => None,
    }));
    (!system.is_empty()).then(|| json!({ "parts": [{ "text": system.join("\n\n") }] }))
}

/// Largest complete prefix that leaves the active user/tool-response group in
/// the generation suffix. Parallel call and response groups are indivisible.
pub(crate) fn largest_safe_cache_boundary(input: &[ModelInput]) -> usize {
    let mut groups = Vec::new();
    let mut index = 0;
    while index < input.len() {
        let start = index;
        match &input[index] {
            ModelInput::ToolCall { .. } => {
                while index < input.len() && matches!(input[index], ModelInput::ToolCall { .. }) {
                    index += 1;
                }
            }
            ModelInput::ToolResult { .. } => {
                while index < input.len() && matches!(input[index], ModelInput::ToolResult { .. }) {
                    index += 1;
                }
            }
            ModelInput::Message {
                role: MessageRole::System,
                ..
            } => {
                index += 1;
                continue;
            }
            ModelInput::Message { .. } => index += 1,
            ModelInput::MultimodalMessage { .. } => index += 1,
            ModelInput::ConfigurationUpdate { .. } | ModelInput::ProviderReasoning { .. } => {
                index += 1;
                continue;
            }
        }
        groups.push((start, index));
    }
    groups.last().map_or(0, |(start, _)| *start)
}

pub(crate) fn estimated_input_tokens(input: &[ModelInput]) -> u64 {
    serde_json::to_vec(input)
        .map(|bytes| (bytes.len() as u64).div_ceil(4))
        .unwrap_or_default()
}

fn function_tools(request: &ModelRequest) -> Option<(Value, Value)> {
    if request.tools.is_empty() {
        return None;
    }
    let declarations = request
        .tools
        .iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "parametersJsonSchema": project_schema(&tool.input_schema),
            })
        })
        .collect::<Vec<_>>();
    Some((
        json!([{ "functionDeclarations": declarations }]),
        json!({
            "functionCallingConfig": {
                "mode": "AUTO",
            }
        }),
    ))
}

fn content_with_text(role: &str, text: &str) -> Value {
    json!({ "role": role, "parts": [{ "text": text }] })
}

/// Keeps the portable OpenAPI subset accepted by both Gemini API surfaces.
pub(crate) fn project_schema(schema: &Value) -> Value {
    match schema {
        Value::Object(object) => {
            let mut projected = Map::new();
            for (key, value) in object {
                if matches!(
                    key.as_str(),
                    "$schema"
                        | "$id"
                        | "$defs"
                        | "definitions"
                        | "oneOf"
                        | "anyOf"
                        | "allOf"
                        | "not"
                        | "patternProperties"
                ) {
                    continue;
                }
                let value = if key == "type" {
                    let Some(value) = project_type(value) else {
                        continue;
                    };
                    value
                } else {
                    project_schema(value)
                };
                projected.insert(key.clone(), value);
            }
            Value::Object(projected)
        }
        Value::Array(values) => Value::Array(values.iter().map(project_schema).collect()),
        value => value.clone(),
    }
}

/// Google accepts one uppercase OpenAPI primitive type. Optional Rust fields
/// become JSON Schema's `["<type>", "null"]`, so retain their concrete type
/// and omit unsupported unions rather than serializing an invalid empty type.
fn project_type(value: &Value) -> Option<Value> {
    let type_name = match value {
        Value::String(value) => Some(value.as_str()),
        Value::Array(values) => {
            let mut non_null = values
                .iter()
                .filter_map(Value::as_str)
                .filter(|value| *value != "null");
            let type_name = non_null.next()?;
            non_null.next().is_none().then_some(type_name)
        }
        _ => None,
    }?;
    Some(Value::String(
        match type_name {
            "object" => "OBJECT",
            "array" => "ARRAY",
            "string" => "STRING",
            "integer" => "INTEGER",
            "number" => "NUMBER",
            "boolean" => "BOOLEAN",
            other => other,
        }
        .into(),
    ))
}

#[derive(Default)]
struct StreamState {
    response_id: Option<String>,
    finish_reason: Option<FinishReason>,
    usage: ModelUsage,
    tool_calls: BTreeMap<String, String>,
    next_call: u64,
}

pub(crate) async fn map_sse(
    response: reqwest::Response,
    sender: mpsc::Sender<Result<ProviderStreamEvent, ProviderError>>,
    cancellation: CancellationToken,
    profile: GeminiProfile,
    cache_write_tokens: Option<u64>,
) {
    let mut bytes = response.bytes_stream();
    let mut buffer = Vec::new();
    let mut state = StreamState::default();
    state.usage.cache_write_input_tokens = cache_write_tokens;
    loop {
        tokio::select! {
            _ = cancellation.cancelled() => { let _ = sender.send(Err(ProviderError::cancelled())).await; return; }
            chunk = bytes.next() => match chunk {
                Some(Ok(chunk)) => {
                    buffer.extend_from_slice(&chunk);
                    while let Some((boundary, width)) = find_sse_boundary(&buffer) {
                        let frame = buffer.drain(..boundary + width).collect::<Vec<_>>();
                        let data = frame.split(|byte| *byte == b'\n')
                            .filter_map(|line| line.strip_prefix(b"data: "))
                            .collect::<Vec<_>>().join(&b'\n');
                        if data.is_empty() || data == b"[DONE]" { continue; }
                        let Ok(payload) = serde_json::from_slice::<Value>(&data) else {
                            let _ = sender.send(Err(ProviderError::protocol("invalid_sse_json", "Google returned invalid SSE JSON"))).await; return;
                        };
                        if let Err(error) = map_chunk(&payload, &mut state, &sender, profile).await {
                            let _ = sender.send(Err(error)).await; return;
                        }
                    }
                }
                Some(Err(error)) => { let _ = sender.send(Err(ProviderError::connection("stream_read", error.to_string()))).await; return; }
                None => {
                    let metadata = ResponseMetadata { provider_request_id: state.response_id, finish_reason: state.finish_reason.unwrap_or_else(|| if state.tool_calls.is_empty() { FinishReason::Stop } else { FinishReason::ToolCalls }), usage: state.usage };
                    let _ = sender.send(Ok(ProviderStreamEvent::Completed { metadata })).await;
                    return;
                }
            }
        }
    }
}

async fn map_chunk(
    payload: &Value,
    state: &mut StreamState,
    sender: &mpsc::Sender<Result<ProviderStreamEvent, ProviderError>>,
    profile: GeminiProfile,
) -> Result<(), ProviderError> {
    state.response_id = payload
        .get("responseId")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or(state.response_id.clone());
    merge_usage(&mut state.usage, payload.get("usageMetadata"));
    let Some(candidate) = payload
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
    else {
        return Ok(());
    };
    if let Some(reason) = candidate.get("finishReason").and_then(Value::as_str) {
        state.finish_reason = Some(match reason {
            "STOP" => FinishReason::Stop,
            "MAX_TOKENS" => FinishReason::Length,
            "SAFETY" | "RECITATION" => FinishReason::ContentFilter,
            _ => FinishReason::Other,
        });
    }
    for (index, part) in candidate
        .pointer("/content/parts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        if let Some(text) = part
            .get("text")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            sender
                .send(Ok(
                    if part.get("thought").and_then(Value::as_bool) == Some(true) {
                        ProviderStreamEvent::ReasoningStarted
                    } else {
                        ProviderStreamEvent::TextDelta { delta: text.into() }
                    },
                ))
                .await
                .map_err(|_| ProviderError::cancelled())?;
            if part.get("thought").and_then(Value::as_bool) == Some(true) {
                sender
                    .send(Ok(ProviderStreamEvent::TextDelta { delta: text.into() }))
                    .await
                    .map_err(|_| ProviderError::cancelled())?;
            }
        }
        let Some(call) = part.get("functionCall") else {
            continue;
        };
        if !profile.streams_function_arguments() {
            continue;
        }
        let id = call
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| {
                state.next_call += 1;
                format!("gemini-{}-{index}", state.next_call)
            });
        let name = call
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ProviderError::protocol("invalid_function_call", "Google function call has no name")
            })?
            .to_owned();
        if state.tool_calls.insert(id.clone(), name.clone()).is_none() {
            sender
                .send(Ok(ProviderStreamEvent::ToolCallStarted {
                    id: id.clone(),
                    name,
                    request_index: u64::try_from(index).unwrap_or(u64::MAX),
                }))
                .await
                .map_err(|_| ProviderError::cancelled())?;
        }
        if let Some(signature) = part.get("thoughtSignature").and_then(Value::as_str) {
            sender
                .send(Ok(ProviderStreamEvent::ToolCallMetadata {
                    id: id.clone(),
                    metadata: json!({ "gemini": { "thought_signature": signature } }),
                }))
                .await
                .map_err(|_| ProviderError::cancelled())?;
        }
        let args = call.get("args").cloned().unwrap_or_else(|| json!({}));
        sender
            .send(Ok(ProviderStreamEvent::ToolArgumentsDelta {
                id,
                delta: serde_json::to_string(&args).map_err(|error| {
                    ProviderError::protocol("function_arguments", error.to_string())
                })?,
            }))
            .await
            .map_err(|_| ProviderError::cancelled())?;
    }
    Ok(())
}

fn merge_usage(usage: &mut ModelUsage, metadata: Option<&Value>) {
    let Some(metadata) = metadata else {
        return;
    };
    let fragment = ModelUsage {
        // `promptTokenCount` is Google's effective GenerateContent input.
        // Cache creation is a separate operation and is included only so the
        // normalized session accounting can report its one-time write.
        input_tokens: metadata
            .get("promptTokenCount")
            .and_then(Value::as_u64)
            .map(|tokens| {
                tokens.saturating_add(usage.cache_write_input_tokens.unwrap_or_default())
            }),
        cache_read_input_tokens: metadata
            .get("cachedContentTokenCount")
            .and_then(Value::as_u64),
        output_tokens: metadata.get("candidatesTokenCount").and_then(Value::as_u64),
        reasoning_tokens: metadata.get("thoughtsTokenCount").and_then(Value::as_u64),
        total_tokens: metadata.get("totalTokenCount").and_then(Value::as_u64),
        provider_usage: metadata.clone(),
        ..ModelUsage::default()
    };
    usage.merge_fragment(fragment);
}

pub(crate) async fn http_error(response: reqwest::Response) -> ProviderError {
    let detail = read_http_error(response).await;
    build_http_error(detail.status, detail.body, detail.retry_after_millis)
}

pub(crate) fn http_error_from_parts(status: reqwest::StatusCode, body: String) -> ProviderError {
    build_http_error(status, body, None)
}

fn build_http_error(
    status: reqwest::StatusCode,
    body: String,
    retry_after_millis: Option<u64>,
) -> ProviderError {
    let kind = match status.as_u16() {
        401 | 403 => crate::ProviderErrorKind::Authentication,
        408 | 504 => crate::ProviderErrorKind::Timeout,
        429 => crate::ProviderErrorKind::RateLimit,
        400..=499 => crate::ProviderErrorKind::InvalidRequest,
        _ => crate::ProviderErrorKind::Server,
    };
    ProviderError {
        kind,
        code: format!("http_{}", status.as_u16()),
        message: if body.is_empty() {
            format!("Google request failed with {status}")
        } else {
            body
        },
        retryable: status.is_server_error() || status.as_u16() == 429,
        retry_after_millis,
        status: Some(status.as_u16()),
        metadata: BTreeMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AttemptId, ModelRef, RequestId, ToolDefinition};

    fn request() -> ModelRequest {
        ModelRequest {
            request_id: RequestId::new(),
            attempt_id: AttemptId::new(),
            model: ModelRef {
                provider: "google".into(),
                model: "gemini-2.5-flash".into(),
            },
            backend: None,
            backend_candidates: Vec::new(),
            effort: Some("low".into()),
            service_tier: None,
            input: vec![
                ModelInput::Message {
                    role: MessageRole::System,
                    content: "system".into(),
                },
                ModelInput::Message {
                    role: MessageRole::User,
                    content: "hello".into(),
                },
            ],
            tools: vec![ToolDefinition {
                name: "read".into(),
                description: "Read a file".into(),
                input_schema: json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false}),
                asynchronous: false,
            }],
            stable_prompt: Vec::new(),
            prompt_cache: None,
            response_transport_continuation: None,
            allow_parallel_tools: true,
            structured_output: None,
        }
    }

    #[test]
    fn projects_tools_and_system_messages() {
        let body = request_body(&request(), GeminiProfile::Developer).unwrap();
        assert_eq!(body["systemInstruction"]["parts"][0]["text"], "system");
        assert_eq!(body["contents"][0]["role"], "user");
        assert_eq!(
            body["tools"][0]["functionDeclarations"][0]["parametersJsonSchema"]["type"],
            "OBJECT"
        );
        assert_eq!(body["toolConfig"]["functionCallingConfig"]["mode"], "AUTO");
    }

    #[test]
    fn caches_the_immutable_system_prefix_and_tool_contract() {
        let mut request = request();
        request.stable_prompt.push(crate::StablePromptPart {
            identity: "policy".into(),
            content: "stable policy".into(),
        });

        let cache = cached_content_body(
            &request,
            GeminiProfile::Developer,
            "models/gemini-2.5-flash",
            0,
        )
        .unwrap()
        .unwrap();
        let generation = request_body_with_cached_content(
            &request,
            GeminiProfile::Developer,
            Some("cachedContents/one"),
            0,
        )
        .unwrap();

        assert_eq!(cache["model"], "models/gemini-2.5-flash");
        assert!(
            cache["systemInstruction"]["parts"][0]["text"]
                .as_str()
                .unwrap()
                .contains("stable policy")
        );
        assert!(
            cache["systemInstruction"]["parts"][0]["text"]
                .as_str()
                .unwrap()
                .contains("system")
        );
        assert!(cache.get("tools").is_some());
        assert!(cache.get("toolConfig").is_some());
        assert_eq!(generation["cachedContent"], "cachedContents/one");
        assert!(generation.get("systemInstruction").is_none());
        assert!(generation.get("tools").is_none());
        assert!(generation.get("toolConfig").is_none());
    }

    #[test]
    fn projects_nullable_tool_types_without_an_empty_google_type() {
        let mut request = request();
        request.tools[0].input_schema = json!({
            "type": "object",
            "properties": {
                "timeout_seconds": {"type": ["integer", "null"]}
            }
        });

        let body = request_body(&request, GeminiProfile::Developer).unwrap();
        assert_eq!(
            body["tools"][0]["functionDeclarations"][0]["parametersJsonSchema"]["properties"]["timeout_seconds"]
                ["type"],
            "INTEGER"
        );
    }

    #[test]
    fn vertex_omits_function_wire_ids() {
        let mut request = request();
        request.input.push(ModelInput::ToolCall {
            call_id: "internal".into(),
            name: "read".into(),
            arguments: json!({"path":"x"}),
            provider_metadata: Value::Null,
        });
        let body = request_body(&request, GeminiProfile::Vertex).unwrap();
        let part = &body["contents"][1]["parts"][0];
        let call = &part["functionCall"];
        assert!(call.get("id").is_none());
        assert_eq!(part["thoughtSignature"], LEGACY_THOUGHT_SIGNATURE);
        assert!(call.get("thoughtSignature").is_none());
    }

    #[test]
    fn rolling_cache_keeps_parallel_tool_groups_and_signatures_intact() {
        let mut request = request();
        request.input = vec![
            ModelInput::Message {
                role: MessageRole::User,
                content: "start".into(),
            },
            ModelInput::ToolCall {
                call_id: "one".into(),
                name: "read".into(),
                arguments: json!({"path":"a"}),
                provider_metadata: json!({"gemini":{"thought_signature":"opaque-one"}}),
            },
            ModelInput::ToolCall {
                call_id: "two".into(),
                name: "read".into(),
                arguments: json!({"path":"b"}),
                provider_metadata: json!({"gemini":{"thought_signature":"opaque-two"}}),
            },
            ModelInput::ToolResult {
                call_id: "one".into(),
                output: json!("a"),
                is_error: false,
            },
            ModelInput::ToolResult {
                call_id: "two".into(),
                output: json!("b"),
                is_error: false,
            },
            ModelInput::Message {
                role: MessageRole::User,
                content: "continue".into(),
            },
        ];
        let boundary = largest_safe_cache_boundary(&request.input);
        assert_eq!(boundary, 5);
        let cache = cached_content_body(
            &request,
            GeminiProfile::Developer,
            "models/gemini-2.5-flash",
            boundary,
        )
        .unwrap()
        .unwrap();
        let contents = cache["contents"].as_array().unwrap();
        assert_eq!(contents[1]["parts"].as_array().unwrap().len(), 2);
        assert_eq!(contents[1]["parts"][0]["thoughtSignature"], "opaque-one");
        assert_eq!(contents[1]["parts"][1]["thoughtSignature"], "opaque-two");
        assert_eq!(contents[2]["parts"].as_array().unwrap().len(), 2);
        let suffix = request_body_with_cached_content(
            &request,
            GeminiProfile::Developer,
            Some("cachedContents/rolling"),
            boundary,
        )
        .unwrap();
        assert_eq!(suffix["contents"].as_array().unwrap().len(), 1);
        assert_eq!(suffix["contents"][0]["parts"][0]["text"], "continue");
    }

    #[test]
    fn tool_continuation_caches_complete_calls_and_leaves_responses_in_suffix() {
        let mut request = request();
        request.input = vec![
            ModelInput::Message {
                role: MessageRole::User,
                content: "start".into(),
            },
            ModelInput::ToolCall {
                call_id: "one".into(),
                name: "read".into(),
                arguments: json!({"path":"a"}),
                provider_metadata: json!({"gemini":{"thought_signature":"opaque"}}),
            },
            ModelInput::ToolResult {
                call_id: "one".into(),
                output: json!("a"),
                is_error: false,
            },
        ];
        let boundary = largest_safe_cache_boundary(&request.input);
        assert_eq!(boundary, 2);
        let suffix = request_body_with_cached_content(
            &request,
            GeminiProfile::Vertex,
            Some("projects/p/locations/l/cachedContents/one"),
            boundary,
        )
        .unwrap();
        assert_eq!(
            suffix["contents"][0]["parts"][0]["functionResponse"]["name"],
            "read"
        );
        assert!(
            suffix["contents"][0]["parts"][0]["functionResponse"]
                .get("id")
                .is_none()
        );
    }

    #[tokio::test]
    async fn streamed_function_call_keeps_its_signature_and_usage() {
        let (sender, mut receiver) = mpsc::channel(8);
        let mut state = StreamState::default();
        map_chunk(&json!({
            "responseId": "response-1",
            "usageMetadata": {"promptTokenCount": 7, "candidatesTokenCount": 3, "totalTokenCount": 10},
            "candidates": [{"finishReason": "STOP", "content": {"parts": [{"functionCall": {"id":"call-1", "name":"read", "args":{"path":"SPEC.md"}}, "thoughtSignature":"opaque-signature"}]}}]
        }), &mut state, &sender, GeminiProfile::Developer).await.unwrap();
        assert!(
            matches!(receiver.recv().await, Some(Ok(ProviderStreamEvent::ToolCallStarted { id, .. })) if id == "call-1")
        );
        assert!(
            matches!(receiver.recv().await, Some(Ok(ProviderStreamEvent::ToolCallMetadata { metadata, .. })) if metadata["gemini"]["thought_signature"] == "opaque-signature")
        );
        assert!(
            matches!(receiver.recv().await, Some(Ok(ProviderStreamEvent::ToolArgumentsDelta { delta, .. })) if delta.contains("SPEC.md"))
        );
        assert_eq!(state.usage.total_tokens, Some(10));
    }

    #[tokio::test]
    async fn usage_includes_tool_prompts_and_an_explicit_cache_write() {
        let (sender, _receiver) = mpsc::channel(1);
        let mut state = StreamState::default();
        state.usage.cache_write_input_tokens = Some(30);
        map_chunk(
            &json!({
                "usageMetadata": {
                    "promptTokenCount": 35,
                    "cachedContentTokenCount": 30,
                    "toolUsePromptTokenCount": 7,
                    "candidatesTokenCount": 3,
                    "totalTokenCount": 45
                }
            }),
            &mut state,
            &sender,
            GeminiProfile::Developer,
        )
        .await
        .unwrap();

        assert_eq!(state.usage.input_tokens, Some(65));
        assert_eq!(state.usage.non_cached_input_tokens, Some(35));
        assert_eq!(state.usage.cache_read_input_tokens, Some(30));
        assert_eq!(state.usage.cache_write_input_tokens, Some(30));
        assert_eq!(state.usage.total_tokens, Some(45));
        assert_eq!(state.usage.provider_usage["toolUsePromptTokenCount"], 7);
    }
}
