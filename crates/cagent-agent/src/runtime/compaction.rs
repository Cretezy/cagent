use std::collections::{HashMap, HashSet};

use futures_util::StreamExt as _;
use tokio_util::sync::CancellationToken;

#[allow(clippy::wildcard_imports)]
use super::*;

const SUMMARY_PROMPT: &str = r"Create a durable context checkpoint for the conversation above.

Return only the checkpoint summary. Preserve all information needed to continue the work accurately:
- the user's goals, requirements, and constraints;
- decisions already made and the rationale for them;
- files inspected or changed, including exact paths;
- completed work and remaining work;
- exact unresolved errors, failing commands, and relevant IDs;
- permission and sandbox facts that affect future actions;
- background terminals, delegated work, and their current states;
- the active provider/model/effort, agent, and collaboration mode;
- branch provenance and any important fork-specific facts.

Treat any <cagent:compaction-summary> in the context as an anchored previous checkpoint. The newest raw suffix is deliberately absent because Cagent will replay it verbatim after your checkpoint; summarize only the prefix supplied here. Update the anchored checkpoint using subsequent prefix work. Do not invent progress. Be concise but complete.";

const RETAINED_SUFFIX_TARGET: u64 = 20_000;

#[derive(Clone, Debug)]
pub(super) struct CompactionResult {
    pub(super) node_id: crate::NodeId,
}

struct TailGroup {
    first: usize,
    tokens: u64,
}

struct TailSelection<'a> {
    retained_from_node_id: crate::NodeId,
    retained_tokens: u64,
    prefix: Vec<&'a crate::store::CompactionPathItem>,
    suffix: Vec<&'a crate::store::CompactionPathItem>,
}

struct CompactionActivity<'a> {
    live_turn: &'a std::sync::Arc<std::sync::RwLock<Option<super::LiveTurn>>>,
    transient: &'a tokio::sync::broadcast::Sender<crate::TransientEvent>,
}

impl Drop for CompactionActivity<'_> {
    fn drop(&mut self) {
        if let Ok(mut live_turn) = self.live_turn.write()
            && let Some(live) = live_turn.as_mut()
        {
            live.compacting = false;
        }
        let _ = self.transient.send(crate::TransientEvent::Working);
    }
}

fn begin_compaction<'a>(
    live_turn: &'a std::sync::Arc<std::sync::RwLock<Option<super::LiveTurn>>>,
    transient: &'a tokio::sync::broadcast::Sender<crate::TransientEvent>,
) -> CompactionActivity<'a> {
    if let Ok(mut current) = live_turn.write()
        && let Some(live) = current.as_mut()
    {
        live.compacting = true;
    }
    let _ = transient.send(crate::TransientEvent::Working);
    CompactionActivity {
        live_turn,
        transient,
    }
}

pub(super) fn threshold_reached(estimated_tokens: u64, context_window: u64, percent: u8) -> bool {
    estimated_tokens.saturating_mul(100) >= context_window.saturating_mul(u64::from(percent))
}

pub(super) async fn has_summarizable_prefix(
    store: &StoreHandle,
    conversation_id: ConversationId,
    resize_images: bool,
    image_estimate: request::ImageTokenEstimate,
) -> Result<bool, RuntimeError> {
    let source = store
        .load_compaction_source(conversation_id, resize_images, false)
        .await?;
    Ok(
        select_retained_tail(&source.tail_candidates, image_estimate).is_some_and(|selection| {
            !selection.prefix.is_empty() || source.previous_checkpoint.is_some()
        }),
    )
}

pub(super) fn is_context_limit_failure(failure: &ProviderError) -> bool {
    failure.kind == crate::ProviderErrorKind::Protocol
        && matches!(
            failure.code.as_str(),
            "context_length_exceeded" | "context_window_exceeded"
        )
}

#[cfg(test)]
fn input_tokens(input: &ModelInput) -> u64 {
    request::estimate_model_input_tokens(input, request::ImageTokenEstimate::Conservative)
}

/// Chooses a newest suffix of complete protocol groups. An oversized group is
/// retained intact, even when it exceeds the preferred target.
#[cfg(test)]
pub(super) fn retained_tail_start(
    items: &[crate::store::CompactionPathItem],
    _context_window: u64,
) -> Result<Option<crate::NodeId>, RuntimeError> {
    Ok(
        select_retained_tail(items, request::ImageTokenEstimate::Conservative)
            .map(|selection| selection.retained_from_node_id),
    )
}

fn select_retained_tail(
    items: &[crate::store::CompactionPathItem],
    image_estimate: request::ImageTokenEstimate,
) -> Option<TailSelection<'_>> {
    if items.is_empty() {
        return None;
    }
    let calls = items
        .iter()
        .filter_map(|item| match &item.input {
            ModelInput::ToolCall { call_id, .. } => Some(call_id.clone()),
            _ => None,
        })
        .collect::<HashSet<_>>();
    let results = items
        .iter()
        .filter_map(|item| match &item.input {
            ModelInput::ToolResult { call_id, .. } => Some(call_id.clone()),
            _ => None,
        })
        .collect::<HashSet<_>>();
    let safe = items
        .iter()
        .filter(|item| match &item.input {
            ModelInput::ToolCall { call_id, .. } => results.contains(call_id),
            ModelInput::ToolResult { call_id, .. } => calls.contains(call_id),
            ModelInput::Message { .. }
            | ModelInput::ProviderReasoning { .. }
            | ModelInput::MultimodalMessage { .. }
            | ModelInput::ConfigurationUpdate { .. } => true,
        })
        .collect::<Vec<_>>();
    if safe.is_empty() {
        return None;
    }

    let mut groups = HashMap::<crate::NodeId, TailGroup>::new();
    let mut keys = Vec::with_capacity(safe.len());
    for (index, item) in safe.iter().enumerate() {
        let key = item.group_id;
        let tokens = request::estimate_model_input_tokens(&item.input, image_estimate);
        groups
            .entry(key)
            .and_modify(|group| {
                group.tokens = group.tokens.saturating_add(tokens);
            })
            .or_insert(TailGroup {
                first: index,
                tokens,
            });
        keys.push(key);
    }

    // Turn IDs can be interleaved when steering input is appended while the
    // original tool-running turn continues. A retained suffix must begin at
    // the first occurrence of every turn represented in it, otherwise it
    // would retain only half of a durable turn.
    let close_over_complete_groups = |mut start: usize| {
        loop {
            let expanded = keys[start..]
                .iter()
                .map(|key| groups[key].first)
                .min()
                .unwrap_or(start);
            if expanded == start {
                return start;
            }
            start = expanded;
        }
    };
    let latest_key = *keys.last().expect("a non-empty safe tail has a group");
    let mut start = close_over_complete_groups(groups[&latest_key].first);
    let suffix_tokens = |start: usize| {
        safe[start..]
            .iter()
            .map(|item| request::estimate_model_input_tokens(&item.input, image_estimate))
            .sum::<u64>()
    };
    let mut retained_tokens = suffix_tokens(start);
    if groups[&latest_key].tokens > RETAINED_SUFFIX_TARGET {
        return Some(TailSelection {
            retained_from_node_id: safe[start].node_id,
            retained_tokens,
            prefix: safe[..start].to_vec(),
            suffix: safe[start..].to_vec(),
        });
    }
    while start > 0 {
        let candidate_key = keys[start - 1];
        let candidate = close_over_complete_groups(groups[&candidate_key].first);
        let candidate_tokens = suffix_tokens(candidate);
        if candidate_tokens > RETAINED_SUFFIX_TARGET {
            // A group larger than the target is never split or summarized.
            // Retain it plus everything newer, then stop selecting older work.
            if groups[&candidate_key].tokens > RETAINED_SUFFIX_TARGET {
                start = candidate;
                retained_tokens = candidate_tokens;
            }
            break;
        }
        start = candidate;
        retained_tokens = candidate_tokens;
    }
    Some(TailSelection {
        retained_from_node_id: safe[start].node_id,
        retained_tokens,
        prefix: safe[..start].to_vec(),
        suffix: safe[start..].to_vec(),
    })
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(super) async fn perform_compaction(
    store: &StoreHandle,
    providers: &ProviderRegistry,
    config: &crate::ConfigSnapshot,
    catalog: &crate::provider::catalog::CatalogManager,
    selection: &SessionSelection,
    profiles: &SessionProfiles,
    conversation_id: ConversationId,
    trigger: crate::CompactionTrigger,
    instructions: Option<&str>,
    estimated_input_tokens: u64,
    prospective_request: Option<&ModelRequest>,
    exclude_active_tip: bool,
    live_turn: &std::sync::Arc<std::sync::RwLock<Option<super::LiveTurn>>>,
    transient: &tokio::sync::broadcast::Sender<crate::TransientEvent>,
    cancellation: &CancellationToken,
) -> Result<Option<CompactionResult>, RuntimeError> {
    let _activity = begin_compaction(live_turn, transient);
    let selected = selection
        .clone()
        .for_mode(&profiles.mode, is_planning_mode(config, &profiles.mode)?);
    let model_ref = selected.model.as_ref().ok_or_else(|| {
        RuntimeError::InvalidOption("cannot compact without a selected model".into())
    })?;
    let provider = providers.get(&model_ref.provider).ok_or_else(|| {
        RuntimeError::InvalidOption(format!(
            "provider adapter is unavailable: {}",
            model_ref.provider
        ))
    })?;
    let (model, backend, backend_candidates, effort, context_window, _, pricing, _, _) =
        prepare_request_selection(
            provider.as_ref(),
            &selected,
            providers.is_enabled(&model_ref.provider)
                || selected.allow_disabled_provider.as_deref() == Some(model_ref.provider.as_str()),
            config,
            catalog,
            false,
            false,
        )
        .await
        .map_err(|error| RuntimeError::InvalidOption(error.message))?;
    let source = store
        .load_compaction_source(conversation_id, config.resize_images(), exclude_active_tip)
        .await?;
    let image_estimate = request::ImageTokenEstimate::for_model(&model, backend);
    let estimated_input_tokens = if estimated_input_tokens == 0 {
        request::estimate_model_inputs_tokens(&source.effective_input, image_estimate)
    } else {
        estimated_input_tokens
    };
    let Some(selection_tail) = select_retained_tail(&source.tail_candidates, image_estimate) else {
        return Ok(None);
    };
    if exclude_active_tip
        && selection_tail.prefix.is_empty()
        && source.previous_checkpoint.is_none()
    {
        return Ok(None);
    }
    if selection_tail.prefix.is_empty()
        && source.previous_checkpoint.is_none()
        && trigger == crate::CompactionTrigger::Automatic
    {
        return Ok(None);
    }
    let retained_from_node_id = Some(selection_tail.retained_from_node_id);
    let mut input = Vec::new();
    if let Some((_, checkpoint)) = source.previous_checkpoint.as_ref() {
        input.push(ModelInput::Message {
            role: crate::MessageRole::System,
            content: format!(
                "<cagent:compaction-summary version=\"{}\">\n{}\n</cagent:compaction-summary>",
                checkpoint.version, checkpoint.summary
            ),
        });
    }
    let anchored_input = input;
    let custom_instructions = instructions
        .map(str::trim)
        .filter(|instructions| !instructions.is_empty())
        .map(|instructions| {
            format!(
                "\n\nAdditional user-provided compaction instructions:\n<user-compaction-instructions>\n{instructions}\n</user-compaction-instructions>"
            )
        })
        .unwrap_or_default();
    let summary_instruction = ModelInput::Message {
        role: crate::MessageRole::System,
        content: format!(
            "{SUMMARY_PROMPT}{custom_instructions}\n\nActive settings: provider={}, model={}, effort={}, agent={}, mode={}. Source branch tip: {}. Previous checkpoint: {}.",
            model.provider,
            model.model,
            effort.as_deref().unwrap_or("default"),
            profiles.agent,
            profiles.mode,
            source.source_tip_node_id,
            source
                .previous_checkpoint
                .as_ref()
                .map_or_else(|| "none".into(), |(id, _)| id.to_string()),
        ),
    };
    let mut request = ModelRequest {
        request_id: crate::RequestId::new(),
        attempt_id: crate::AttemptId::new(),
        model: model.clone(),
        backend,
        backend_candidates,
        effort: effort.clone(),
        service_tier: None,
        input: Vec::new(),
        tools: Vec::new(),
        stable_prompt: vec![StablePromptPart {
            identity: "cagent:context-compaction".into(),
            content: "You produce context checkpoints for Cagent. Follow the final compaction instruction exactly and never call tools.".into(),
        }],
        prompt_cache: None,
        response_transport_continuation: None,
        allow_parallel_tools: false,
        structured_output: None,
    };
    #[allow(clippy::large_enum_variant)] // This private retry result is consumed in the same loop.
    enum SummaryAttempt {
        Completed(String, ResponseMetadata),
        Failed(ProviderError, bool),
        Invalid(RuntimeError),
    }
    let mut removable_prefix = selection_tail.prefix.clone();
    let mut transient_attempt = 0_u32;
    let (summary, mut metadata) = loop {
        request.input = anchored_input.clone();
        request
            .input
            .extend(removable_prefix.iter().map(|item| item.input.clone()));
        request.input.push(summary_instruction.clone());
        let stream_result = tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                return Err(RuntimeError::InvalidOption("compaction cancelled".into()));
            }
            result = provider.stream(request.clone(), cancellation.clone()) => result
        };
        let attempt = match stream_result {
            Err(failure) => SummaryAttempt::Failed(failure, false),
            Ok(mut stream) => {
                let mut summary = String::new();
                let mut completion = None;
                let mut failed = None;
                while let Some(event) = tokio::select! {
                    biased;
                    () = cancellation.cancelled() => {
                        return Err(RuntimeError::InvalidOption("compaction cancelled".into()));
                    }
                    event = stream.next() => event
                } {
                    match event {
                        Err(failure) => {
                            failed = Some(SummaryAttempt::Failed(failure, !summary.is_empty()));
                            break;
                        }
                        Ok(ProviderStreamEvent::TextDelta { delta }) => {
                            super::update_live_turn_activity(live_turn, transient);
                            summary.push_str(&delta);
                        }
                        Ok(ProviderStreamEvent::Completed { metadata }) => {
                            completion = Some(metadata);
                        }
                        Ok(
                            ProviderStreamEvent::ReasoningStarted
                            | ProviderStreamEvent::EncryptedReasoning { .. },
                        ) => {}
                        Ok(
                            ProviderStreamEvent::Steerable { .. }
                            | ProviderStreamEvent::SteerAccepted { .. }
                            | ProviderStreamEvent::SteerFailed { .. },
                        ) => {}
                        Ok(
                            ProviderStreamEvent::ToolCallStarted { .. }
                            | ProviderStreamEvent::ToolArgumentsDelta { .. }
                            | ProviderStreamEvent::ToolCallMetadata { .. },
                        ) => {
                            failed = Some(SummaryAttempt::Invalid(RuntimeError::InvalidOption(
                                "compaction response attempted to call a tool".into(),
                            )));
                            break;
                        }
                    }
                }
                failed.unwrap_or_else(|| {
                    completion.map_or_else(
                        || {
                            SummaryAttempt::Invalid(RuntimeError::InvalidOption(
                                "compaction response ended without completion metadata".into(),
                            ))
                        },
                        |metadata| SummaryAttempt::Completed(summary, metadata),
                    )
                })
            }
        };
        match attempt {
            SummaryAttempt::Completed(summary, metadata) => break (summary, metadata),
            SummaryAttempt::Invalid(error) => return Err(error),
            SummaryAttempt::Failed(failure, visible_output) => {
                if visible_output {
                    return Err(RuntimeError::InvalidOption(format!(
                        "compaction provider failed after emitting summary output: {}",
                        failure.message
                    )));
                }
                if is_context_limit_failure(&failure) {
                    let Some(oldest) = removable_prefix.first().map(|item| item.group_id) else {
                        return Err(RuntimeError::InvalidOption(format!(
                            "one protocol-safe group is too large: compaction input has no removable prefix after provider context rejection ({})",
                            failure.message
                        )));
                    };
                    removable_prefix.retain(|item| item.group_id != oldest);
                    continue;
                }
                match super::retry_action(&failure, transient_attempt, false) {
                    super::RetryAction::Backoff(delay) => {
                        transient_attempt = transient_attempt.saturating_add(1);
                        tokio::select! {
                            () = cancellation.cancelled() => {
                                return Err(RuntimeError::InvalidOption("compaction cancelled".into()));
                            }
                            () = tokio::time::sleep(delay) => {}
                        }
                    }
                    super::RetryAction::RefreshCredentials => {
                        let refreshed = tokio::select! {
                            biased;
                            () = cancellation.cancelled() => false,
                            result = provider.refresh_credentials(cancellation.clone()) => {
                                result.unwrap_or(false)
                            }
                        };
                        if !refreshed {
                            return Err(RuntimeError::InvalidOption(format!(
                                "compaction provider failed: {}",
                                failure.message
                            )));
                        }
                        transient_attempt = transient_attempt.saturating_add(1);
                    }
                    super::RetryAction::Stop => {
                        return Err(RuntimeError::InvalidOption(format!(
                            "compaction provider failed: {}",
                            failure.message
                        )));
                    }
                }
            }
        }
    };
    let summary = summary.trim().to_owned();
    if summary.is_empty() {
        return Err(RuntimeError::InvalidOption(
            "compaction response was empty".into(),
        ));
    }
    if metadata.finish_reason != crate::FinishReason::Stop {
        return Err(RuntimeError::InvalidOption(format!(
            "compaction response was truncated or incomplete ({:?})",
            metadata.finish_reason
        )));
    }
    if metadata.usage.cost.is_none() {
        metadata.usage.cost = estimate_model_cost(&metadata.usage, pricing.as_ref());
    }
    let mut projected_request = prospective_request
        .cloned()
        .unwrap_or_else(|| request.clone());
    projected_request.response_transport_continuation = None;
    projected_request.input = vec![ModelInput::Message {
        role: crate::MessageRole::System,
        content: format!(
            "<cagent:compaction-summary version=\"2\">\n{summary}\n</cagent:compaction-summary>"
        ),
    }];
    projected_request
        .input
        .extend(selection_tail.suffix.iter().map(|item| item.input.clone()));
    let projected_tokens = request::estimate_request_tokens(&projected_request, image_estimate);
    if projected_tokens > context_window {
        return Err(RuntimeError::InvalidOption(format!(
            "one protocol-safe group is too large: the complete post-compaction request needs about {projected_tokens} tokens but the model context window is {context_window}"
        )));
    }
    let content = crate::CompactionSummary {
        version: 2,
        summary,
        retained_from_node_id,
        trigger,
        estimated_input_tokens,
        context_window_tokens: context_window,
        threshold_percent: config.compaction().threshold_percent,
        summary_model: crate::CompactionModelSettings {
            provider: model.provider,
            model: model.model,
            effort,
        },
        agent: profiles.agent.clone(),
        mode: profiles.mode.clone(),
        source_tip_node_id: source.source_tip_node_id,
        excluded_node_id: source.excluded_node_id,
        reapplied_node_id: source.reapplied_node_id,
        previous_checkpoint_id: source.previous_checkpoint.map(|(id, _)| id),
        last_summarized_node_id: removable_prefix.last().map(|item| item.node_id),
        retained_token_estimate: Some(selection_tail.retained_tokens),
        retained_token_target: Some(RETAINED_SUFFIX_TARGET),
    };
    let node_id = store
        .append_compaction_summary(
            conversation_id,
            source.checkpoint_parent_id,
            content,
            metadata,
        )
        .await?;
    Ok(Some(CompactionResult { node_id }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(
        node_id: crate::NodeId,
        role: crate::MessageRole,
        chars: usize,
    ) -> crate::store::CompactionPathItem {
        crate::store::CompactionPathItem {
            node_id,
            group_id: node_id,
            input: ModelInput::Message {
                role,
                content: "x".repeat(chars),
            },
        }
    }

    #[test]
    fn threshold_is_inclusive() {
        assert!(!threshold_reached(79, 100, 80));
        assert!(threshold_reached(80, 100, 80));
    }

    #[test]
    fn retained_tail_selects_newest_groups_up_to_twenty_thousand_tokens() {
        let old = crate::NodeId::new();
        let middle = crate::NodeId::new();
        let newest = crate::NodeId::new();
        let items = vec![
            message(old, crate::MessageRole::User, 32_000),
            message(middle, crate::MessageRole::Assistant, 32_000),
            message(newest, crate::MessageRole::System, 32_000),
        ];
        let selection =
            select_retained_tail(&items, request::ImageTokenEstimate::Conservative).unwrap();
        assert_eq!(selection.retained_from_node_id, middle);
        assert_eq!(selection.prefix.len(), 1);
        assert_eq!(selection.suffix.len(), 2);
    }

    #[test]
    fn oversized_group_expands_the_suffix_without_being_split() {
        let old = crate::NodeId::new();
        let oversized = crate::NodeId::new();
        let newest = crate::NodeId::new();
        let items = vec![
            message(old, crate::MessageRole::User, 4_000),
            message(oversized, crate::MessageRole::User, 100_000),
            message(newest, crate::MessageRole::System, 400),
        ];
        let selection =
            select_retained_tail(&items, request::ImageTokenEstimate::Conservative).unwrap();
        assert_eq!(selection.retained_from_node_id, oversized);
        assert!(selection.retained_tokens > RETAINED_SUFFIX_TARGET);
        assert_eq!(selection.prefix.len(), 1);
    }

    #[test]
    fn four_sixty_five_thousand_token_attachments_summarize_the_first_three() {
        let first = crate::NodeId::new();
        let second = crate::NodeId::new();
        let third = crate::NodeId::new();
        let fourth = crate::NodeId::new();
        let items = vec![
            message(first, crate::MessageRole::User, 260_000),
            message(second, crate::MessageRole::User, 260_000),
            message(third, crate::MessageRole::User, 260_000),
            message(fourth, crate::MessageRole::User, 260_000),
        ];

        let selection =
            select_retained_tail(&items, request::ImageTokenEstimate::Conservative).unwrap();

        assert_eq!(selection.retained_from_node_id, fourth);
        assert_eq!(
            selection
                .prefix
                .iter()
                .map(|item| item.node_id)
                .collect::<Vec<_>>(),
            [first, second, third]
        );
        assert_eq!(selection.suffix.len(), 1);
        assert_eq!(selection.retained_tokens, 65_000);
    }

    #[test]
    fn base64_image_size_does_not_inflate_compaction_tokens() {
        let image = crate::NodeId::new();
        let items = vec![crate::store::CompactionPathItem {
            node_id: image,
            group_id: image,
            input: ModelInput::MultimodalMessage {
                role: crate::MessageRole::User,
                content: vec![
                    crate::ModelContentPart::Text {
                        text: "[Image #1] what's this".into(),
                    },
                    crate::ModelContentPart::Image {
                        mime_type: "image/png".into(),
                        sha256: "test-image".into(),
                        data: "a".repeat(2_700_000),
                        width: 1_456,
                        height: 816,
                    },
                ],
            },
        }];

        assert_eq!(retained_tail_start(&items, 32_768).unwrap(), Some(image));
        assert!(input_tokens(&items[0].input) < 8_192);
    }

    #[test]
    fn parallel_tool_batch_is_atomic_and_closes_over_interleaved_steering() {
        let batch = crate::NodeId::new();
        let call_one = crate::NodeId::new();
        let steering = crate::NodeId::new();
        let call_two = crate::NodeId::new();
        let items = vec![
            crate::store::CompactionPathItem {
                node_id: call_one,
                group_id: batch,
                input: ModelInput::ToolCall {
                    call_id: "one".into(),
                    name: "read".into(),
                    arguments: serde_json::json!({}),
                    provider_metadata: serde_json::Value::Null,
                },
            },
            message(steering, crate::MessageRole::User, 4),
            crate::store::CompactionPathItem {
                node_id: call_two,
                group_id: batch,
                input: ModelInput::ToolCall {
                    call_id: "two".into(),
                    name: "read".into(),
                    arguments: serde_json::json!({}),
                    provider_metadata: serde_json::Value::Null,
                },
            },
            crate::store::CompactionPathItem {
                node_id: crate::NodeId::new(),
                group_id: batch,
                input: ModelInput::ToolResult {
                    call_id: "one".into(),
                    output: serde_json::json!({}),
                    is_error: false,
                },
            },
            crate::store::CompactionPathItem {
                node_id: crate::NodeId::new(),
                group_id: batch,
                input: ModelInput::ToolResult {
                    call_id: "two".into(),
                    output: serde_json::json!({}),
                    is_error: false,
                },
            },
        ];
        let selection =
            select_retained_tail(&items, request::ImageTokenEstimate::Conservative).unwrap();
        assert_eq!(selection.retained_from_node_id, call_one);
        assert_eq!(selection.suffix.len(), 5);
    }

    #[test]
    fn incomplete_tool_exchanges_are_not_replayed() {
        let incomplete = crate::NodeId::new();
        let notice = crate::NodeId::new();
        let items = vec![
            crate::store::CompactionPathItem {
                node_id: incomplete,
                group_id: crate::NodeId::new(),
                input: ModelInput::ToolCall {
                    call_id: "missing".into(),
                    name: "read".into(),
                    arguments: serde_json::json!({}),
                    provider_metadata: serde_json::Value::Null,
                },
            },
            message(notice, crate::MessageRole::System, 40),
        ];
        let selection =
            select_retained_tail(&items, request::ImageTokenEstimate::Conservative).unwrap();
        assert_eq!(selection.retained_from_node_id, notice);
        assert_eq!(selection.suffix.len(), 1);
    }

    #[test]
    fn context_limit_classifier_is_narrow() {
        let failure =
            ProviderError::protocol("context_length_exceeded", "maximum context length reached");
        assert!(is_context_limit_failure(&failure));
        assert!(!is_context_limit_failure(&ProviderError::protocol(
            "invalid_request",
            "invalid tool schema",
        )));
    }
}
