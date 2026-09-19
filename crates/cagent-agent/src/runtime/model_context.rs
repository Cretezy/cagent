//! Bounded, provider-visible conversation context.
//!
//! Durable transcript data deliberately remains lossless. This type is the
//! separate, bounded representation sent to providers.

use std::collections::HashMap;

use crate::provider::ModelInput;

pub(crate) const MAX_TOOL_RESULT_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug)]
pub(super) struct ModelContext {
    input: Vec<ModelInput>,
}

impl ModelContext {
    pub(super) fn from_input(input: Vec<ModelInput>) -> Self {
        let mut context = Self { input: Vec::new() };
        let names = input
            .iter()
            .filter_map(|item| match item {
                ModelInput::ToolCall { call_id, name, .. } => Some((call_id.clone(), name.clone())),
                _ => None,
            })
            .collect::<HashMap<_, _>>();
        for item in input {
            match item {
                ModelInput::ToolResult {
                    call_id,
                    output,
                    is_error,
                } => {
                    context.record_tool_result(
                        call_id.clone(),
                        names.get(&call_id).map(String::as_str),
                        output,
                        is_error,
                    );
                }
                other => context.input.push(other),
            }
        }
        context
    }

    pub(super) fn snapshot(&self) -> Vec<ModelInput> {
        self.input.clone()
    }

    pub(super) fn push(&mut self, input: ModelInput) {
        match input {
            ModelInput::ToolResult {
                call_id,
                output,
                is_error,
            } => self.record_tool_result(call_id, None, output, is_error),
            other => self.input.push(other),
        }
    }

    pub(super) fn record_tool_result(
        &mut self,
        call_id: String,
        tool_name: Option<&str>,
        output: serde_json::Value,
        is_error: bool,
    ) {
        self.input.push(ModelInput::ToolResult {
            call_id: call_id.clone(),
            output: bounded_tool_output(&call_id, tool_name, output),
            is_error,
        });
    }
}

/// The durable bounded form of one result. Never load the full transcript blob
/// when rebuilding a provider request.
pub(crate) fn bounded_tool_output(
    call_id: &str,
    tool_name: Option<&str>,
    output: serde_json::Value,
) -> serde_json::Value {
    let output = model_visible_tool_output(tool_name, output);
    let bytes = json_bytes(&output);
    if bytes <= MAX_TOOL_RESULT_BYTES {
        return output;
    }
    tracing::debug!(call_id, tool = ?tool_name, original_bytes = bytes, retained_limit = MAX_TOOL_RESULT_BYTES, "tool result reduced for model context");
    preview_output(call_id, tool_name, &output, bytes, "per_result_limit")
}

/// Removes frontend-only terminal styling before a tool result becomes model
/// context. The durable result retains it for terminal presentation.
fn model_visible_tool_output(
    tool_name: Option<&str>,
    mut output: serde_json::Value,
) -> serde_json::Value {
    if tool_name == Some("terminal_output")
        && let Some(object) = output.as_object_mut()
    {
        object.remove("ansi_output");
    }
    output
}

fn json_bytes(value: &serde_json::Value) -> usize {
    serde_json::to_vec(value).map_or(0, |value| value.len())
}

fn omitted_output(
    call_id: &str,
    tool_name: Option<&str>,
    original_bytes: usize,
    reason: &str,
) -> serde_json::Value {
    serde_json::json!({
        "cagent": "tool_output_omitted",
        "reason": reason,
        "call_id": call_id,
        "tool": tool_name,
        "original_bytes": original_bytes,
    })
}

fn preview_output(
    call_id: &str,
    tool_name: Option<&str>,
    output: &serde_json::Value,
    original_bytes: usize,
    reason: &str,
) -> serde_json::Value {
    let preview = match tool_name {
        Some("read") => serde_json::json!({
            "path": output.get("path"), "start_line": output.get("start_line"),
            "end_line": output.get("end_line"), "truncated": output.get("truncated"),
            "next_start_line": output.get("next_start_line"),
            "content": output.get("content").and_then(serde_json::Value::as_str).map(|text| head_tail(text, 7_000)),
        }),
        Some("grep") => serde_json::json!({
            "matches": output.get("matches").and_then(serde_json::Value::as_array).map(|items| items.iter().take(12).map(|item| compact_value(item, 800)).collect::<Vec<_>>()),
            "truncated": output.get("truncated"), "next_cursor": output.get("next_cursor"), "discarded_bytes": output.get("discarded_bytes"),
        }),
        Some("list") => serde_json::json!({
            "path": output.get("path"),
            "entries": output.get("entries").and_then(serde_json::Value::as_array).map(|items| items.iter().take(30).map(|item| compact_value(item, 400)).collect::<Vec<_>>()),
            "truncated": output.get("truncated"), "next_cursor": output.get("next_cursor"), "discarded_bytes": output.get("discarded_bytes"),
        }),
        _ => compact_value(output, 7_000),
    };
    let candidate = serde_json::json!({
        "cagent": "tool_output_preview", "reason": reason, "call_id": call_id,
        "tool": tool_name, "original_bytes": original_bytes, "preview": preview,
    });
    fit_preview(candidate, call_id, tool_name, original_bytes, reason)
}

fn compact_value(value: &serde_json::Value, text_limit: usize) -> serde_json::Value {
    match value {
        serde_json::Value::String(text) => serde_json::Value::String(head_tail(text, text_limit)),
        serde_json::Value::Array(values) => serde_json::Value::Array(
            values
                .iter()
                .take(16)
                .map(|value| compact_value(value, text_limit / 2))
                .collect(),
        ),
        serde_json::Value::Object(values) => serde_json::Value::Object(
            values
                .iter()
                .take(16)
                .map(|(key, value)| (key.clone(), compact_value(value, text_limit / 2)))
                .collect(),
        ),
        _ => value.clone(),
    }
}

fn head_tail(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_owned();
    }
    let head_limit = limit / 2;
    let head = truncate_utf8(value, head_limit);
    let tail_start = value.len().saturating_sub(head_limit);
    let mut start = tail_start;
    while start < value.len() && !value.is_char_boundary(start) {
        start += 1;
    }
    format!("{}\n… [preview truncated] …\n{}", head, &value[start..])
}

fn truncate_utf8(value: &str, limit: usize) -> &str {
    let mut end = limit.min(value.len());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn fit_preview(
    mut value: serde_json::Value,
    call_id: &str,
    tool_name: Option<&str>,
    original_bytes: usize,
    reason: &str,
) -> serde_json::Value {
    if json_bytes(&value) <= MAX_TOOL_RESULT_BYTES {
        return value;
    }
    // The bounded representation must be safe even for unusually long paths or IDs.
    value = serde_json::json!({
        "cagent": "tool_output_preview", "reason": reason, "call_id": truncate_utf8(call_id, 512),
        "tool": tool_name.map(|name| truncate_utf8(name, 256)), "original_bytes": original_bytes,
        "preview": "preview exceeded the provider result limit",
    });
    if json_bytes(&value) <= MAX_TOOL_RESULT_BYTES {
        value
    } else {
        omitted_output("<oversized-call-id>", None, original_bytes, reason)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appending_results_never_rewrites_earlier_history() {
        let mut context = ModelContext::from_input(vec![ModelInput::ToolCall {
            call_id: "a".into(),
            name: "read".into(),
            arguments: serde_json::json!({}),
            provider_metadata: serde_json::Value::Null,
        }]);
        let mut previous = context.snapshot();
        for number in 0..8 {
            let id = format!("{number}");
            context.push(ModelInput::ToolCall {
                call_id: id.clone(),
                name: "read".into(),
                arguments: serde_json::json!({}),
                provider_metadata: serde_json::Value::Null,
            });
            context.record_tool_result(
                id,
                Some("read"),
                serde_json::json!("x".repeat(12 * 1024)),
                false,
            );
            assert!(context.snapshot().starts_with(&previous));
            previous = context.snapshot();
        }
        let snapshot = context.snapshot();
        let total = snapshot
            .iter()
            .filter_map(|item| match item {
                ModelInput::ToolResult { output, .. } => Some(json_bytes(output)),
                _ => None,
            })
            .sum::<usize>();
        assert!(total > 64 * 1024);
        assert!(snapshot.iter().all(|item| match item {
            ModelInput::ToolResult { output, .. } => json_bytes(output) <= MAX_TOOL_RESULT_BYTES,
            _ => true,
        }));
        assert_eq!(
            snapshot
                .iter()
                .filter(|item| matches!(item, ModelInput::ToolResult { .. }))
                .count(),
            8
        );
        assert!(matches!(&snapshot[1], ModelInput::ToolCall { .. }));
    }

    #[test]
    fn normalizing_history_is_idempotent() {
        let input = vec![ModelInput::ToolResult {
            call_id: "call-1".into(),
            output: serde_json::json!({"content": "x".repeat(32 * 1024)}),
            is_error: false,
        }];
        let normalized = ModelContext::from_input(input).snapshot();
        let reconstructed = ModelContext::from_input(normalized.clone()).snapshot();
        assert_eq!(reconstructed, normalized);
        assert!(matches!(
            &normalized[0],
            ModelInput::ToolResult { output, .. } if json_bytes(output) <= MAX_TOOL_RESULT_BYTES
        ));
    }

    #[test]
    fn previews_an_individual_oversized_result() {
        let output = bounded_tool_output(
            "call-1",
            Some("read"),
            serde_json::json!("x".repeat(MAX_TOOL_RESULT_BYTES)),
        );
        assert_eq!(output["cagent"], "tool_output_preview");
        assert_eq!(output["reason"], "per_result_limit");
        assert_eq!(output["call_id"], "call-1");
        assert!(json_bytes(&output) <= MAX_TOOL_RESULT_BYTES);
    }

    #[test]
    fn terminal_output_excludes_frontend_ansi_data_from_model_context() {
        let output = bounded_tool_output(
            "call-1",
            Some("terminal_output"),
            serde_json::json!({
                "output": "plain text",
                "ansi_output": "\u{1b}[32mplain text\u{1b}[0m",
                "cursor": 10,
            }),
        );
        assert_eq!(output["output"], "plain text");
        assert!(output.get("ansi_output").is_none());
    }

    #[test]
    fn read_preview_keeps_continuation_and_useful_content() {
        let output = bounded_tool_output(
            "call-1",
            Some("read"),
            serde_json::json!({
                "path": "src/large.rs", "start_line": 50, "end_line": 500,
                "truncated": true, "next_start_line": 501,
                "content": format!("head-marker\\n{}\\ntail-marker", "x".repeat(32 * 1024)),
            }),
        );
        assert_eq!(output["preview"]["path"], "src/large.rs");
        assert_eq!(output["preview"]["next_start_line"], 501);
        assert!(
            output["preview"]["content"]
                .as_str()
                .unwrap()
                .contains("head-marker")
        );
        assert!(
            output["preview"]["content"]
                .as_str()
                .unwrap()
                .contains("tail-marker")
        );
        assert!(json_bytes(&output) <= MAX_TOOL_RESULT_BYTES);
    }
}
