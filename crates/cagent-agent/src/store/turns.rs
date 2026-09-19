#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn start_assistant(
    connection: &mut Connection,
    conversation_id: ConversationId,
    parent_id: NodeId,
    turn_id: TurnId,
    attempt: Option<&ModelAttemptSnapshot>,
) -> Result<(NodeId, DurableEvent), RuntimeError> {
    let transaction = connection.transaction()?;
    ensure_active_parent(&transaction, conversation_id, parent_id)?;
    let node_id = NodeId::new();
    let now = now();
    transaction.execute(
        "INSERT INTO nodes (
            id, conversation_id, parent_id, turn_id, kind, status, role, content_json,
            provider, model, effort, agent, mode, request_id, attempt_id,
            context_window_tokens, created_at
         ) VALUES (?1, ?2, ?3, ?4, 'assistant_message', 'streaming', 'assistant',
                   '{\"text\":\"\"}', ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            node_id.to_string(),
            conversation_id.to_string(),
            parent_id.to_string(),
            turn_id.to_string(),
            attempt.map(|attempt| attempt.model.provider.as_str()),
            attempt.map(|attempt| attempt.model.model.as_str()),
            attempt.and_then(|attempt| attempt.effort.as_deref()),
            attempt.map(|attempt| attempt.agent.as_str()),
            attempt.map(|attempt| attempt.mode.as_str()),
            attempt.map(|attempt| attempt.request_id.to_string()),
            attempt.map(|attempt| attempt.attempt_id.to_string()),
            attempt
                .map(|attempt| sqlite_optional_u64(Some(attempt.context_window), "context window"))
                .transpose()?
                .flatten(),
            now,
        ],
    )?;
    set_active(&transaction, conversation_id, node_id, &now)?;
    let event = insert_event(
        &transaction,
        conversation_id,
        DurableEventKind::NodeAppended {
            node_id,
            parent_id: Some(parent_id),
            turn_id: Some(turn_id),
            owner_id: None,
            request_index: None,
            node_kind: NodeKind::AssistantMessage,
            status: "streaming".into(),
            content: json!({ "text": "" }),
        },
    )?;
    transaction.commit()?;
    Ok((node_id, event))
}

pub(super) fn append_compaction_summary(
    connection: &mut Connection,
    conversation_id: ConversationId,
    parent_id: NodeId,
    content: &CompactionSummary,
    metadata: &ResponseMetadata,
) -> MultiEventResult<NodeId> {
    let transaction = connection.transaction()?;
    ensure_active_parent(&transaction, conversation_id, parent_id)?;
    if (content.source_tip_node_id != parent_id
        && content.excluded_node_id != Some(parent_id)
        && content.reapplied_node_id != Some(parent_id))
        || content.summary.trim().is_empty()
    {
        return Err(RuntimeError::InvalidOption(
            "invalid compaction checkpoint provenance or empty summary".into(),
        ));
    }
    let node_id = NodeId::new();
    let turn_id = TurnId::new();
    let now = now();
    transaction.execute(
        "INSERT INTO nodes (
            id, conversation_id, parent_id, turn_id, kind, status, role, content_json,
            provider, model, effort, agent, mode, created_at, completed_at
         ) VALUES (?1, ?2, ?3, ?4, 'compaction_summary', 'completed', 'system', ?5,
                   ?6, ?7, ?8, ?9, ?10, ?11, ?11)",
        params![
            node_id.to_string(),
            conversation_id.to_string(),
            parent_id.to_string(),
            turn_id.to_string(),
            serde_json::to_string(content)?,
            content.summary_model.provider,
            content.summary_model.model,
            content.summary_model.effort,
            content.agent,
            content.mode,
            now,
        ],
    )?;
    insert_model_usage(&transaction, node_id, metadata)?;
    transaction.execute(
        "DELETE FROM response_continuations WHERE conversation_id = ?1",
        [conversation_id.to_string()],
    )?;
    let events = vec![
        insert_event(
            &transaction,
            conversation_id,
            DurableEventKind::NodeAppended {
                node_id,
                parent_id: Some(parent_id),
                turn_id: Some(turn_id),
                owner_id: None,
                request_index: None,
                node_kind: NodeKind::CompactionSummary,
                status: "completed".into(),
                content: serde_json::to_value(content)?,
            },
        )?,
        insert_event(
            &transaction,
            conversation_id,
            DurableEventKind::ModelUsageRecorded {
                node_id,
                usage: metadata.usage.clone(),
            },
        )?,
    ];
    set_active(&transaction, conversation_id, node_id, &now)?;
    transaction.commit()?;
    Ok((node_id, events))
}

pub(super) fn append_assistant_delta(
    connection: &mut Connection,
    conversation_id: ConversationId,
    node_id: NodeId,
    text: &str,
    delta: &str,
) -> Result<((), DurableEvent), RuntimeError> {
    let transaction = connection.transaction()?;
    let changed = transaction.execute(
        "UPDATE nodes SET content_json = ?1
         WHERE id = ?2 AND conversation_id = ?3 AND status = 'streaming'",
        params![
            json!({ "text": text }).to_string(),
            node_id.to_string(),
            conversation_id.to_string(),
        ],
    )?;
    ensure_node_changed(changed, node_id, conversation_id)?;
    let event = insert_event(
        &transaction,
        conversation_id,
        DurableEventKind::AssistantDelta {
            node_id,
            delta: delta.into(),
            document: crate::MarkdownDocument::default(),
        },
    )?;
    transaction.commit()?;
    Ok(((), event))
}

pub(super) fn start_plan(
    connection: &mut Connection,
    conversation_id: ConversationId,
    node_id: NodeId,
) -> Result<((), DurableEvent), RuntimeError> {
    let transaction = connection.transaction()?;
    let changed = transaction.execute(
        "UPDATE nodes SET content_json = '{\"text\":\"\",\"flavor\":\"plan\"}'
         WHERE id = ?1 AND conversation_id = ?2 AND kind = 'assistant_message' AND status = 'streaming'",
        params![node_id.to_string(), conversation_id.to_string()],
    )?;
    ensure_node_changed(changed, node_id, conversation_id)?;
    let event = insert_event(
        &transaction,
        conversation_id,
        DurableEventKind::PlanStarted { node_id },
    )?;
    transaction.commit()?;
    Ok(((), event))
}

pub(super) fn append_plan_delta(
    connection: &mut Connection,
    conversation_id: ConversationId,
    node_id: NodeId,
    text: &str,
    delta: &str,
) -> Result<((), DurableEvent), RuntimeError> {
    let transaction = connection.transaction()?;
    let changed = transaction.execute(
        "UPDATE nodes SET content_json = ?1 WHERE id = ?2 AND conversation_id = ?3 AND kind = 'assistant_message' AND status = 'streaming'",
        params![json!({ "text": text, "flavor": "plan" }).to_string(), node_id.to_string(), conversation_id.to_string()],
    )?;
    ensure_node_changed(changed, node_id, conversation_id)?;
    let event = insert_event(
        &transaction,
        conversation_id,
        DurableEventKind::PlanDelta {
            node_id,
            delta: delta.into(),
            document: crate::MarkdownDocument::default(),
        },
    )?;
    transaction.commit()?;
    Ok(((), event))
}

pub(super) fn append_context_clear(
    connection: &mut Connection,
    conversation_id: ConversationId,
    _mode: &str,
    context_window: u64,
) -> Result<(NodeId, DurableEvent), RuntimeError> {
    let ((node_id, _), mut events) = append_accepted_plan(
        connection,
        conversation_id,
        "",
        None,
        true,
        false,
        context_window,
    )?;
    Ok((
        node_id,
        events.pop().expect("accepted plan emits its node update"),
    ))
}

pub(super) fn append_accepted_plan(
    connection: &mut Connection,
    conversation_id: ConversationId,
    plan: &str,
    note: Option<&str>,
    clear_context: bool,
    compact_context: bool,
    context_window: u64,
) -> MultiEventResult<(NodeId, TurnId)> {
    let transaction = connection.transaction()?;
    let (parent_id, agent, mode, provider, model, effort): (
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = transaction
        .query_row(
            "SELECT n.id, c.active_agent, c.active_mode,
                    c.normal_provider, c.normal_model, c.normal_effort
             FROM conversations c JOIN nodes n ON n.id = c.active_node_id
             WHERE c.id = ?1",
            [conversation_id.to_string()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .map_err(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => {
                RuntimeError::ConversationNotFound(conversation_id)
            }
            other => RuntimeError::Database(other),
        })?;
    let parent_id = parent_id
        .parse::<NodeId>()
        .map_err(|error| RuntimeError::InvalidOption(format!("invalid active node ID: {error}")))?;
    ensure_active_parent(&transaction, conversation_id, parent_id)?;
    let turn_id = TurnId::new();
    let accepted_plan_id = NodeId::new();
    let timestamp = now();
    let hidden_instruction = if clear_context {
        "Implement the following plan."
    } else {
        "Implement this plan."
    };
    let plan_content = serde_json::to_value(crate::AcceptedPlanPayload {
        instruction: hidden_instruction.into(),
        plan_markdown: plan.into(),
        reset_context: clear_context,
        compact_context,
        destination: crate::AcceptedPlanDestination {
            agent: agent.clone(),
            mode: mode.clone(),
            provider: provider.clone(),
            model: model.clone(),
            effort: effort.clone(),
        },
        reset_context_window: clear_context.then_some(context_window),
    })?;
    transaction.execute(
        "INSERT INTO nodes (
            id, conversation_id, parent_id, turn_id, kind, status, role, content_json,
            agent, mode, provider, model, effort, context_window_tokens, created_at, completed_at
         ) VALUES (?1, ?2, ?3, ?4, 'accepted_plan', 'completed', 'user', ?5,
                   ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12)",
        params![
            accepted_plan_id.to_string(),
            conversation_id.to_string(),
            parent_id.to_string(),
            turn_id.to_string(),
            plan_content.to_string(),
            agent,
            mode,
            provider,
            model,
            effort,
            clear_context
                .then(|| sqlite_optional_u64(Some(context_window), "context window"))
                .transpose()?
                .flatten(),
            timestamp,
        ],
    )?;
    set_active(&transaction, conversation_id, accepted_plan_id, &timestamp)?;
    let plan_event = insert_event(
        &transaction,
        conversation_id,
        DurableEventKind::NodeAppended {
            node_id: accepted_plan_id,
            parent_id: Some(parent_id),
            turn_id: Some(turn_id),
            owner_id: None,
            request_index: None,
            node_kind: NodeKind::AcceptedPlan,
            status: "completed".into(),
            content: plan_content,
        },
    )?;
    let mut events = vec![plan_event];
    let (start_id, turn_id) = if let Some(note) = note {
        let ((note_id, note_turn_id), note_event) = append_user_in_transaction(
            &transaction,
            conversation_id,
            note,
            &[],
            &[],
            &[],
            &[],
            &[],
        )?;
        events.push(note_event);
        (note_id, note_turn_id)
    } else {
        (accepted_plan_id, turn_id)
    };
    transaction.commit()?;
    Ok(((start_id, turn_id), events))
}

#[cfg(test)]
pub(super) fn complete_assistant(
    connection: &mut Connection,
    conversation_id: ConversationId,
    node_id: NodeId,
    metadata: &ResponseMetadata,
    tool_calls: Vec<PendingToolCall>,
) -> MultiEventResult<Vec<StoredToolCall>> {
    complete_assistant_with_reasoning(
        connection,
        conversation_id,
        node_id,
        metadata,
        tool_calls,
        Vec::new(),
    )
}

pub(super) fn complete_assistant_with_reasoning(
    connection: &mut Connection,
    conversation_id: ConversationId,
    node_id: NodeId,
    metadata: &ResponseMetadata,
    mut tool_calls: Vec<PendingToolCall>,
    reasoning: Vec<ModelInput>,
) -> MultiEventResult<Vec<StoredToolCall>> {
    let transaction = connection.transaction()?;
    let now = now();
    let (kind, stored_content): (String, String) = transaction
        .query_row(
            "SELECT kind, content_json FROM nodes
             WHERE id = ?1 AND conversation_id = ?2 AND status = 'streaming'",
            params![node_id.to_string(), conversation_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .ok_or(RuntimeError::NodeNotFoundOrInvalidState(
            node_id,
            conversation_id,
        ))?;
    if !reasoning.is_empty() {
        if kind != "assistant_message" {
            return Err(RuntimeError::NodeNotFoundOrInvalidState(
                node_id,
                conversation_id,
            ));
        }
        super::assistant_reasoning::save(&transaction, node_id, &reasoning)?;
    }
    let mut content: serde_json::Value = decode_stored_json(&stored_content)?;
    if content.get("flavor").and_then(serde_json::Value::as_str) == Some("plan") {
        content["text"] = json!(content["text"].as_str().unwrap_or_default().trim());
    }
    content["response"] = json!({
        "provider_request_id": metadata.provider_request_id,
        "finish_reason": metadata.finish_reason,
    });
    let changed = transaction.execute(
        "UPDATE nodes SET status = 'completed', content_json = ?1, completed_at = ?2
         WHERE id = ?3 AND conversation_id = ?4 AND status = 'streaming'",
        params![
            content.to_string(),
            now,
            node_id.to_string(),
            conversation_id.to_string(),
        ],
    )?;
    ensure_node_changed(changed, node_id, conversation_id)?;

    // Segmented assistant nodes are synthetic protocol boundaries, not model
    // responses. Do not turn their default usage into an unknown-priced call.
    let mut events = Vec::new();
    if metadata.usage != crate::ModelUsage::default() {
        insert_model_usage(&transaction, node_id, metadata)?;
        events.push(insert_event(
            &transaction,
            conversation_id,
            DurableEventKind::ModelUsageRecorded {
                node_id,
                usage: metadata.usage.clone(),
            },
        )?);
    }
    tool_calls.sort_by_key(|call| call.request_index);
    let (stored_calls, active_node) = append_tool_calls(
        &transaction,
        conversation_id,
        node_id,
        &now,
        tool_calls,
        &mut events,
    )?;
    events.push(insert_event(
        &transaction,
        conversation_id,
        DurableEventKind::NodeStatusChanged {
            node_id,
            status: "completed".into(),
        },
    )?);
    set_active(&transaction, conversation_id, active_node, &now)?;
    transaction.commit()?;
    Ok((stored_calls, events))
}

fn insert_model_usage(
    transaction: &Transaction<'_>,
    node_id: NodeId,
    metadata: &ResponseMetadata,
) -> Result<(), RuntimeError> {
    let usage = &metadata.usage;
    let provider_usage = (!metadata.usage.provider_usage.is_null())
        .then(|| metadata.usage.provider_usage.to_string());
    transaction.execute(
        "INSERT INTO model_usage (
            node_id, input_tokens, non_cached_input_tokens, cache_read_input_tokens,
            cache_write_input_tokens, output_tokens, reasoning_tokens, total_tokens,
            provider_usage_json, input_cost, cache_read_cost, cache_write_cost,
            output_cost, reasoning_cost, total_cost, currency, pricing_source, pricing_version
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
        params![
            node_id.to_string(),
            sqlite_optional_u64(usage.input_tokens, "input token count")?,
            sqlite_optional_u64(
                usage.non_cached_input_tokens,
                "non-cached input token count"
            )?,
            sqlite_optional_u64(usage.cache_read_input_tokens, "cache-read token count")?,
            sqlite_optional_u64(usage.cache_write_input_tokens, "cache-write token count")?,
            sqlite_optional_u64(usage.output_tokens, "output token count")?,
            sqlite_optional_u64(usage.reasoning_tokens, "reasoning token count")?,
            sqlite_optional_u64(usage.total_tokens, "total token count")?,
            provider_usage,
            usage.cost.as_ref().and_then(|cost| cost.input_cost.as_deref()),
            usage.cost.as_ref().and_then(|cost| cost.cache_read_cost.as_deref()),
            usage.cost.as_ref().and_then(|cost| cost.cache_write_cost.as_deref()),
            usage.cost.as_ref().and_then(|cost| cost.output_cost.as_deref()),
            usage.cost.as_ref().and_then(|cost| cost.reasoning_cost.as_deref()),
            usage.cost.as_ref().and_then(|cost| cost.total_cost.as_deref()),
            usage.cost.as_ref().map(|cost| cost.currency.as_str()),
            usage.cost.as_ref().map(|cost| cost.pricing_source.as_str()),
            usage.cost.as_ref().map(|cost| cost.pricing_version.as_str()),
        ],
    )?;
    Ok(())
}

fn append_tool_calls(
    transaction: &Transaction<'_>,
    conversation_id: ConversationId,
    assistant_id: NodeId,
    now: &str,
    tool_calls: Vec<PendingToolCall>,
    events: &mut Vec<DurableEvent>,
) -> Result<(Vec<StoredToolCall>, NodeId), RuntimeError> {
    let turn_id: String = transaction.query_row(
        "SELECT turn_id FROM nodes WHERE id = ?1 AND conversation_id = ?2",
        params![assistant_id.to_string(), conversation_id.to_string()],
        |row| row.get(0),
    )?;
    let mut parent_id = assistant_id;
    let mut stored_calls = Vec::with_capacity(tool_calls.len());
    for call in tool_calls {
        let request_index = sqlite_u64(call.request_index, "tool-call request index")?;
        let stored = StoredToolCall {
            node_id: NodeId::new(),
            provider_call_id: call.provider_call_id,
            name: call.name,
            arguments: call.arguments,
            request_index: call.request_index,
            provider_metadata: call.provider_metadata,
        };
        // Keep legacy records unchanged unless a provider supplied metadata
        // that must survive a later continuation.
        let mut content = json!({
            "call_id": stored.provider_call_id,
            "name": stored.name,
            "arguments": stored.arguments,
        });
        if !stored.provider_metadata.is_null() {
            content["provider_metadata"] = stored.provider_metadata.clone();
        }
        transaction.execute(
            "INSERT INTO nodes (
                id, conversation_id, parent_id, turn_id, owner_id, request_index,
                kind, status, role, content_json, created_at, completed_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'tool_call', 'completed',
                       'assistant', ?7, ?8, ?8)",
            params![
                stored.node_id.to_string(),
                conversation_id.to_string(),
                parent_id.to_string(),
                turn_id,
                assistant_id.to_string(),
                request_index,
                content.to_string(),
                now,
            ],
        )?;
        events.push(insert_event(
            transaction,
            conversation_id,
            DurableEventKind::NodeAppended {
                node_id: stored.node_id,
                parent_id: Some(parent_id),
                turn_id: Some(turn_id.parse().map_err(|error| {
                    RuntimeError::InvalidOption(format!("invalid stored turn ID: {error}"))
                })?),
                owner_id: Some(assistant_id),
                request_index: Some(stored.request_index),
                node_kind: NodeKind::ToolCall,
                status: "completed".into(),
                content,
            },
        )?);
        parent_id = stored.node_id;
        stored_calls.push(stored);
    }
    Ok((stored_calls, parent_id))
}

pub(super) fn append_permission_decision(
    connection: &mut Connection,
    conversation_id: ConversationId,
    tool_call: &StoredToolCall,
    audit: &crate::PermissionAudit,
) -> Result<(NodeId, DurableEvent), RuntimeError> {
    let transaction = connection.transaction()?;
    let (active_id, turn_id): (String, String) = transaction
        .query_row(
            "SELECT c.active_node_id, n.turn_id
             FROM conversations c
             JOIN nodes n ON n.id = ?1 AND n.conversation_id = c.id
             WHERE c.id = ?2 AND n.kind = 'tool_call'",
            params![tool_call.node_id.to_string(), conversation_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .ok_or(RuntimeError::NodeNotFoundOrInvalidState(
            tool_call.node_id,
            conversation_id,
        ))?;
    let parent_id: NodeId = active_id.parse().map_err(|error| {
        RuntimeError::InvalidOption(format!("invalid stored active node ID: {error}"))
    })?;
    let turn_id: TurnId = turn_id
        .parse()
        .map_err(|error| RuntimeError::InvalidOption(format!("invalid stored turn ID: {error}")))?;
    let node_id = NodeId::new();
    let request_index = sqlite_u64(tool_call.request_index, "permission request index")?;
    let content = serde_json::to_value(audit)?;
    let now = now();
    transaction.execute(
        "INSERT INTO nodes (
            id, conversation_id, parent_id, turn_id, owner_id, request_index,
            kind, status, role, content_json, created_at, completed_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'permission_decision', 'completed',
                   'system', ?7, ?8, ?8)",
        params![
            node_id.to_string(),
            conversation_id.to_string(),
            parent_id.to_string(),
            turn_id.to_string(),
            tool_call.node_id.to_string(),
            request_index,
            content.to_string(),
            now,
        ],
    )?;
    set_active(&transaction, conversation_id, node_id, &now)?;
    let event = insert_event(
        &transaction,
        conversation_id,
        DurableEventKind::NodeAppended {
            node_id,
            parent_id: Some(parent_id),
            turn_id: Some(turn_id),
            owner_id: Some(tool_call.node_id),
            request_index: Some(tool_call.request_index),
            node_kind: NodeKind::PermissionDecision,
            status: "completed".into(),
            content,
        },
    )?;
    transaction.commit()?;
    Ok((node_id, event))
}

pub(super) fn append_tool_result(
    connection: &mut Connection,
    conversation_id: ConversationId,
    tool_call: &StoredToolCall,
    output: &serde_json::Value,
    is_error: bool,
    duration_millis: u64,
    transition: Option<&crate::runtime::worktrees::WorkspaceTransition>,
) -> Result<(NodeId, Vec<DurableEvent>), RuntimeError> {
    let transaction = connection.transaction()?;
    let (active_id, turn_id): (String, String) = transaction
        .query_row(
            "SELECT c.active_node_id, n.turn_id
             FROM conversations c
             JOIN nodes n ON n.id = ?1 AND n.conversation_id = c.id
             WHERE c.id = ?2 AND n.kind = 'tool_call'",
            params![tool_call.node_id.to_string(), conversation_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .ok_or(RuntimeError::NodeNotFoundOrInvalidState(
            tool_call.node_id,
            conversation_id,
        ))?;
    let parent_id: NodeId = active_id.parse().map_err(|error| {
        RuntimeError::InvalidOption(format!("invalid stored active node ID: {error}"))
    })?;
    let turn_id: TurnId = turn_id
        .parse()
        .map_err(|error| RuntimeError::InvalidOption(format!("invalid stored turn ID: {error}")))?;
    let node_id = NodeId::new();
    let request_index = sqlite_u64(tool_call.request_index, "tool-result request index")?;
    let duration_millis = sqlite_u64(duration_millis, "tool duration")?;
    let encoded_output = serde_json::to_vec(output)?;
    super::patch_diffs::record_patch_diff(
        &transaction,
        conversation_id,
        &tool_call.name,
        output,
        is_error,
    )?;
    let model_output = crate::runtime::model_context::bounded_tool_output(
        &tool_call.provider_call_id,
        Some(&tool_call.name),
        output.clone(),
    );
    let (stored_output, output_blob_id) = if encoded_output.len() > INLINE_TOOL_OUTPUT_BYTES {
        let blob_id = crate::BlobId::new();
        transaction.execute(
            "INSERT INTO blobs (id, codec, bytes) VALUES (?1, 'json', ?2)",
            params![blob_id.to_string(), &encoded_output],
        )?;
        (compact_tool_output(output), Some(blob_id))
    } else {
        (output.clone(), None)
    };
    let content = json!({
        "call_id": tool_call.provider_call_id,
        "name": tool_call.name,
        "output": stored_output,
        "output_blob_id": output_blob_id,
        "model_output": model_output,
        "output_bytes": encoded_output.len(),
        "is_error": is_error,
        "duration_millis": duration_millis,
    });
    let now = now();
    transaction.execute(
        "INSERT INTO nodes (
            id, conversation_id, parent_id, turn_id, owner_id, request_index,
            kind, status, role, content_json, created_at, completed_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'tool_result', 'completed',
                   'tool', ?7, ?8, ?8)",
        params![
            node_id.to_string(),
            conversation_id.to_string(),
            parent_id.to_string(),
            turn_id.to_string(),
            tool_call.node_id.to_string(),
            request_index,
            content.to_string(),
            now,
        ],
    )?;
    set_active(&transaction, conversation_id, node_id, &now)?;
    let event = insert_event(
        &transaction,
        conversation_id,
        DurableEventKind::NodeAppended {
            node_id,
            parent_id: Some(parent_id),
            turn_id: Some(turn_id),
            owner_id: Some(tool_call.node_id),
            request_index: Some(tool_call.request_index),
            node_kind: NodeKind::ToolResult,
            status: "completed".into(),
            content,
        },
    )?;
    let mut events = vec![event];
    let mut active_tip_id = node_id;
    if let Some(transition) = transition {
        let worktree_json = transition
            .worktree
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        transaction.execute(
            "UPDATE conversations SET cwd = ?1, worktree_json = ?2, updated_at = ?3 WHERE id = ?4",
            params![
                transition.cwd.to_string_lossy(),
                worktree_json,
                now,
                conversation_id.to_string()
            ],
        )?;
        transaction.execute(
            "DELETE FROM response_continuations WHERE conversation_id = ?1",
            [conversation_id.to_string()],
        )?;
        let notice_id = NodeId::new();
        let message = transition_message(transition);
        let mut notice_content =
            serde_json::to_value(crate::SystemNodePayload::WorkspaceTransition {
                message,
                transition: serde_json::to_value(transition)?,
            })?;
        notice_content["transcript"] = json!("notice");
        notice_content["message"] = json!(transition_message(transition));
        notice_content["workspace_transition"] = serde_json::to_value(transition)?;
        transaction.execute(
            "INSERT INTO nodes (
                id, conversation_id, parent_id, turn_id, kind, status, role, content_json, created_at, completed_at
             ) VALUES (?1, ?2, ?3, ?4, 'system', 'completed', 'system', ?5, ?6, ?6)",
            params![notice_id.to_string(), conversation_id.to_string(), node_id.to_string(), turn_id.to_string(), notice_content.to_string(), now],
        )?;
        set_active(&transaction, conversation_id, notice_id, &now)?;
        active_tip_id = notice_id;
        events.push(insert_event(
            &transaction,
            conversation_id,
            DurableEventKind::NodeAppended {
                node_id: notice_id,
                parent_id: Some(node_id),
                turn_id: Some(turn_id),
                owner_id: None,
                request_index: None,
                node_kind: NodeKind::System,
                status: "completed".into(),
                content: notice_content,
            },
        )?);
    }
    transaction.commit()?;
    // Workspace transitions append a system notice after the tool result and
    // make that notice active. Return the actual branch tip so a caller that
    // continues the turn cannot attach the next assistant to the stale tool
    // result parent.
    Ok((active_tip_id, events))
}

pub(super) fn append_workspace_transition(
    connection: &mut Connection,
    conversation_id: ConversationId,
    transition: &crate::runtime::worktrees::WorkspaceTransition,
) -> Result<((), DurableEvent), RuntimeError> {
    let transaction = connection.transaction()?;
    ensure_session_transaction(&transaction, conversation_id)?;
    let parent_id = active_node(&transaction, conversation_id)?;
    let turn_id = transaction
        .query_row(
            "SELECT turn_id FROM nodes WHERE id = ?1",
            [parent_id.to_string()],
            |row| row.get::<_, Option<String>>(0),
        )?
        .map(|id| {
            id.parse::<TurnId>().map_err(|error| {
                RuntimeError::InvalidOption(format!("invalid stored turn ID: {error}"))
            })
        })
        .transpose()?
        .unwrap_or_else(TurnId::new);
    let timestamp = now();
    let worktree_json = transition
        .worktree
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;
    transaction.execute(
        "UPDATE conversations SET cwd = ?1, worktree_json = ?2, updated_at = ?3 WHERE id = ?4",
        params![
            transition.cwd.to_string_lossy(),
            worktree_json,
            timestamp,
            conversation_id.to_string()
        ],
    )?;
    transaction.execute(
        "DELETE FROM response_continuations WHERE conversation_id = ?1",
        [conversation_id.to_string()],
    )?;
    let node_id = NodeId::new();
    let mut content = serde_json::to_value(crate::SystemNodePayload::WorkspaceTransition {
        message: transition_message(transition),
        transition: serde_json::to_value(transition)?,
    })?;
    content["transcript"] = json!("notice");
    content["message"] = json!(transition_message(transition));
    content["workspace_transition"] = serde_json::to_value(transition)?;
    transaction.execute(
        "INSERT INTO nodes (id, conversation_id, parent_id, turn_id, kind, status, role, content_json, created_at, completed_at)
         VALUES (?1, ?2, ?3, ?4, 'system', 'completed', 'system', ?5, ?6, ?6)",
        params![node_id.to_string(), conversation_id.to_string(), parent_id.to_string(), turn_id.to_string(), content.to_string(), timestamp],
    )?;
    set_active(&transaction, conversation_id, node_id, &timestamp)?;
    let event = insert_event(
        &transaction,
        conversation_id,
        DurableEventKind::NodeAppended {
            node_id,
            parent_id: Some(parent_id),
            turn_id: Some(turn_id),
            owner_id: None,
            request_index: None,
            node_kind: NodeKind::System,
            status: "completed".into(),
            content,
        },
    )?;
    transaction.commit()?;
    Ok(((), event))
}

fn transition_message(transition: &crate::runtime::worktrees::WorkspaceTransition) -> String {
    if transition.reason == "missing_cwd_recovered" {
        transition.warning.clone().unwrap_or_else(|| {
            format!(
                "Recorded working directory was missing; returned to {}",
                transition.cwd.display()
            )
        })
    } else if transition.created {
        format!(
            "Created and entered {} worktree {} at {}",
            match transition.vcs {
                Some(crate::WorkspaceVcs::Git) => "Git",
                Some(crate::WorkspaceVcs::Jujutsu) => "Jujutsu",
                None => "VCS",
            },
            transition.name.as_deref().unwrap_or(""),
            transition.cwd.display()
        )
    } else if transition.reason == "worktree_entered" {
        format!("Entered worktree at {}", transition.cwd.display())
    } else {
        format!(
            "Changed working directory from {} to {}",
            transition.previous_cwd.display(),
            transition.cwd.display()
        )
    }
}

fn compact_tool_output(output: &serde_json::Value) -> serde_json::Value {
    let Some(diff) = output.get("diff") else {
        return json!({ "summary": "large tool output retained for lazy loading" });
    };
    let mut diff = diff.clone();
    if let Some(files) = diff
        .get_mut("files")
        .and_then(serde_json::Value::as_array_mut)
    {
        files.truncate(1);
        if let Some(hunks) = files
            .first_mut()
            .and_then(|file| file.get_mut("hunks"))
            .and_then(serde_json::Value::as_array_mut)
        {
            hunks.truncate(1);
            if let Some(lines) = hunks
                .first_mut()
                .and_then(|hunk| hunk.get_mut("lines"))
                .and_then(serde_json::Value::as_array_mut)
            {
                lines.truncate(40);
                for line in lines {
                    if let Some(text) = line.get_mut("text")
                        && let Some(value) = text.as_str()
                        && value.len() > 500
                    {
                        let end = value
                            .char_indices()
                            .map(|(index, _)| index)
                            .take_while(|index| *index <= 500)
                            .last()
                            .unwrap_or(0);
                        *text = serde_json::Value::String(format!("{}…", &value[..end]));
                    }
                }
            }
        }
    }
    json!({
        "summary": "large tool output retained for lazy loading",
        "diff": diff,
    })
}

pub(super) fn finish_assistant_with_status(
    connection: &mut Connection,
    conversation_id: ConversationId,
    node_id: NodeId,
    status: &str,
) -> Result<((), DurableEvent), RuntimeError> {
    let transaction = connection.transaction()?;
    let now = now();
    let changed = transaction.execute(
        "UPDATE nodes SET status = ?1, completed_at = ?2
         WHERE id = ?3 AND conversation_id = ?4 AND status = 'streaming'",
        params![
            status,
            now,
            node_id.to_string(),
            conversation_id.to_string()
        ],
    )?;
    ensure_node_changed(changed, node_id, conversation_id)?;
    set_active(&transaction, conversation_id, node_id, &now)?;
    let event = insert_event(
        &transaction,
        conversation_id,
        DurableEventKind::NodeStatusChanged {
            node_id,
            status: status.into(),
        },
    )?;
    transaction.commit()?;
    Ok(((), event))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn fail_assistant(
    connection: &mut Connection,
    conversation_id: ConversationId,
    node_id: NodeId,
    status: &str,
    rewind_to_parent: bool,
    code: &str,
    message: &str,
    retryable: bool,
) -> MultiEventResult<()> {
    let transaction = connection.transaction()?;
    let now = now();
    let (stored_content, parent_id): (String, Option<String>) = transaction
        .query_row(
            "SELECT content_json, parent_id FROM nodes
             WHERE id = ?1 AND conversation_id = ?2 AND status = 'streaming'",
            params![node_id.to_string(), conversation_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .ok_or(RuntimeError::NodeNotFoundOrInvalidState(
            node_id,
            conversation_id,
        ))?;
    let mut content: serde_json::Value = decode_stored_json(&stored_content)?;
    content["error"] = json!({
        "code": code,
        "message": message,
        "retryable": retryable,
    });
    let changed = transaction.execute(
        "UPDATE nodes SET status = ?1, content_json = ?2, completed_at = ?3
         WHERE id = ?4 AND conversation_id = ?5 AND status = 'streaming'",
        params![
            status,
            content.to_string(),
            now,
            node_id.to_string(),
            conversation_id.to_string(),
        ],
    )?;
    ensure_node_changed(changed, node_id, conversation_id)?;
    let active_id = if rewind_to_parent {
        parent_id
            .ok_or_else(|| RuntimeError::InvalidOption("assistant node has no parent".into()))?
            .parse()
            .map_err(|error| {
                RuntimeError::InvalidOption(format!("invalid stored parent node ID: {error}"))
            })?
    } else {
        node_id
    };
    set_active(&transaction, conversation_id, active_id, &now)?;
    let failed = insert_event(
        &transaction,
        conversation_id,
        DurableEventKind::AssistantFailed {
            node_id,
            code: code.into(),
            message: message.into(),
            retryable,
        },
    )?;
    let terminal = insert_event(
        &transaction,
        conversation_id,
        DurableEventKind::NodeStatusChanged {
            node_id,
            status: status.into(),
        },
    )?;
    transaction.commit()?;
    Ok(((), vec![failed, terminal]))
}

pub(super) fn set_active(
    transaction: &Transaction<'_>,
    conversation_id: ConversationId,
    node_id: NodeId,
    now: &str,
) -> Result<(), RuntimeError> {
    let changed = transaction.execute(
        "UPDATE conversations SET active_node_id = ?1, updated_at = ?2 WHERE id = ?3",
        params![node_id.to_string(), now, conversation_id.to_string()],
    )?;
    if changed == 1 {
        Ok(())
    } else {
        Err(RuntimeError::ConversationNotFound(conversation_id))
    }
}

fn ensure_active_parent(
    transaction: &Transaction<'_>,
    conversation_id: ConversationId,
    parent_id: NodeId,
) -> Result<(), RuntimeError> {
    let active: Option<String> = transaction
        .query_row(
            "SELECT active_node_id FROM conversations WHERE id = ?1",
            [conversation_id.to_string()],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(RuntimeError::ConversationNotFound(conversation_id))?;
    if active.as_deref() == Some(parent_id.to_string().as_str()) {
        Ok(())
    } else {
        Err(RuntimeError::StaleActiveParent(parent_id, conversation_id))
    }
}

fn ensure_node_changed(
    changed: usize,
    node_id: NodeId,
    conversation_id: ConversationId,
) -> Result<(), RuntimeError> {
    if changed == 1 {
        Ok(())
    } else {
        Err(RuntimeError::NodeNotFoundOrInvalidState(
            node_id,
            conversation_id,
        ))
    }
}

pub(super) fn insert_event(
    transaction: &Transaction<'_>,
    conversation_id: ConversationId,
    kind: DurableEventKind,
) -> Result<DurableEvent, RuntimeError> {
    transaction.execute(
        "UPDATE conversations SET revision = revision + 1 WHERE id = ?1",
        [conversation_id.to_string()],
    )?;
    let revision: i64 = transaction.query_row(
        "SELECT revision FROM conversations WHERE id = ?1",
        [conversation_id.to_string()],
        |row| row.get(0),
    )?;
    let cursor = EventCursor(
        revision
            .try_into()
            .map_err(|_| RuntimeError::InvalidOption("negative conversation revision".into()))?,
    );
    Ok(DurableEvent {
        version: API_VERSION,
        cursor,
        conversation_id,
        kind,
    })
}

pub(super) fn append_configuration_changed(
    connection: &mut Connection,
    conversation_id: ConversationId,
    path: Option<std::path::PathBuf>,
) -> Result<((), DurableEvent), RuntimeError> {
    let transaction = connection.transaction()?;
    ensure_session_transaction(&transaction, conversation_id)?;
    let event = insert_event(
        &transaction,
        conversation_id,
        DurableEventKind::ConfigurationChanged { path },
    )?;
    transaction.commit()?;
    Ok(((), event))
}

pub(super) fn append_transcript_notice(
    connection: &mut Connection,
    conversation_id: ConversationId,
    message: &str,
) -> Result<((), DurableEvent), RuntimeError> {
    let transaction = connection.transaction()?;
    ensure_session_transaction(&transaction, conversation_id)?;
    let parent_id = active_node(&transaction, conversation_id)?;
    let turn_id = transaction
        .query_row(
            "SELECT turn_id FROM nodes WHERE id = ?1",
            [parent_id.to_string()],
            |row| row.get::<_, Option<String>>(0),
        )?
        .map(|id| {
            id.parse::<TurnId>().map_err(|error| {
                RuntimeError::InvalidOption(format!("invalid stored turn ID: {error}"))
            })
        })
        .transpose()?
        .unwrap_or_else(TurnId::new);
    let mut content = serde_json::to_value(crate::SystemNodePayload::TranscriptNotice {
        message: message.into(),
    })?;
    content["transcript"] = json!("notice");
    let node_id = NodeId::new();
    let timestamp = now();
    transaction.execute(
        "INSERT INTO nodes (
            id, conversation_id, parent_id, turn_id, kind, status, role, content_json, created_at, completed_at
         ) VALUES (?1, ?2, ?3, ?4, 'system', 'completed', 'system', ?5, ?6, ?6)",
        params![
            node_id.to_string(),
            conversation_id.to_string(),
            parent_id.to_string(),
            turn_id.to_string(),
            content.to_string(),
            timestamp,
        ],
    )?;
    set_active(&transaction, conversation_id, node_id, &timestamp)?;
    let event = insert_event(
        &transaction,
        conversation_id,
        DurableEventKind::NodeAppended {
            node_id,
            parent_id: Some(parent_id),
            turn_id: Some(turn_id),
            owner_id: None,
            request_index: None,
            node_kind: NodeKind::System,
            status: "completed".into(),
            content,
        },
    )?;
    transaction.commit()?;
    Ok(((), event))
}

pub(super) fn append_recap(
    connection: &mut Connection,
    conversation_id: ConversationId,
    expected_parent_id: NodeId,
    text: &str,
) -> Result<(bool, DurableEvent), RuntimeError> {
    let transaction = connection.transaction()?;
    ensure_session_transaction(&transaction, conversation_id)?;
    let parent_id = active_node(&transaction, conversation_id)?;
    if parent_id != expected_parent_id {
        return Err(RuntimeError::InvalidOption(
            "conversation changed before recap could be stored".into(),
        ));
    }
    let turn_id = transaction
        .query_row(
            "SELECT turn_id FROM nodes WHERE id = ?1",
            [parent_id.to_string()],
            |row| row.get::<_, Option<String>>(0),
        )?
        .map(|id| {
            id.parse::<TurnId>().map_err(|error| {
                RuntimeError::InvalidOption(format!("invalid stored turn ID: {error}"))
            })
        })
        .transpose()?
        .unwrap_or_else(TurnId::new);
    let mut content = serde_json::to_value(crate::SystemNodePayload::Recap { text: text.into() })?;
    content["transcript"] = json!("recap");
    let node_id = NodeId::new();
    let timestamp = now();
    transaction.execute(
        "INSERT INTO nodes (
            id, conversation_id, parent_id, turn_id, kind, status, role, content_json, created_at, completed_at
         ) VALUES (?1, ?2, ?3, ?4, 'system', 'completed', 'system', ?5, ?6, ?6)",
        params![
            node_id.to_string(), conversation_id.to_string(), parent_id.to_string(),
            turn_id.to_string(), content.to_string(), timestamp,
        ],
    )?;
    set_active(&transaction, conversation_id, node_id, &timestamp)?;
    let event = insert_event(
        &transaction,
        conversation_id,
        DurableEventKind::NodeAppended {
            node_id,
            parent_id: Some(parent_id),
            turn_id: Some(turn_id),
            owner_id: None,
            request_index: None,
            node_kind: NodeKind::System,
            status: "completed".into(),
            content,
        },
    )?;
    transaction.commit()?;
    Ok((true, event))
}

pub(super) fn synchronize_local_context(
    connection: &mut Connection,
    conversation_id: ConversationId,
    snapshot: &crate::LocalContextSnapshot,
) -> Result<(Option<LocalContextSync>, Option<DurableEvent>), RuntimeError> {
    // An empty revision is the embedded-runtime sentinel for no configured
    // local-context source. Real reconciliation hashes even an empty catalog.
    if snapshot.revision.is_empty() {
        return Ok((None, None));
    }
    let transaction = connection.transaction()?;
    ensure_session_transaction(&transaction, conversation_id)?;
    let parent_id = active_node(&transaction, conversation_id)?;
    let mut cursor = parent_id;
    let previous = loop {
        let (parent, kind, encoded): (Option<String>, String, String) = transaction.query_row(
            "SELECT parent_id, kind, content_json FROM nodes WHERE id = ?1 AND conversation_id = ?2",
            params![cursor.to_string(), conversation_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        if kind == "accepted_plan"
            && decode_stored_json::<serde_json::Value>(&encoded)?
                .get("reset_context")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        {
            break None;
        }
        if kind == "system"
            && let Ok(crate::SystemNodePayload::LocalModelContext { snapshot, .. }) =
                decode_stored_json(&encoded)
        {
            break Some(snapshot);
        }
        let Some(parent) = parent else { break None };
        cursor = parent.parse().map_err(|error| {
            RuntimeError::InvalidOption(format!("invalid stored parent node ID: {error}"))
        })?;
    };
    if previous
        .as_ref()
        .is_some_and(|old| old.revision == snapshot.revision)
    {
        transaction.commit()?;
        return Ok((None, None));
    }
    let message = previous
        .as_ref()
        .map_or_else(|| snapshot.full_notice(), |old| snapshot.delta_notice(old));
    let turn_id = transaction
        .query_row(
            "SELECT turn_id FROM nodes WHERE id = ?1",
            [parent_id.to_string()],
            |row| row.get::<_, Option<String>>(0),
        )?
        .map(|id| {
            id.parse::<TurnId>().map_err(|error| {
                RuntimeError::InvalidOption(format!("invalid stored turn ID: {error}"))
            })
        })
        .transpose()?
        .unwrap_or_else(TurnId::new);
    let mut content = serde_json::to_value(crate::SystemNodePayload::LocalModelContext {
        message: message.clone(),
        snapshot: snapshot.clone(),
    })?;
    content["transcript"] = json!("hidden");
    content["local_context"] = serde_json::to_value(snapshot)?;
    let node_id = NodeId::new();
    let timestamp = now();
    transaction.execute(
        "INSERT INTO nodes (
            id, conversation_id, parent_id, turn_id, kind, status, role, content_json, created_at, completed_at
         ) VALUES (?1, ?2, ?3, ?4, 'system', 'completed', 'system', ?5, ?6, ?6)",
        params![node_id.to_string(), conversation_id.to_string(), parent_id.to_string(),
            turn_id.to_string(), content.to_string(), timestamp],
    )?;
    set_active(&transaction, conversation_id, node_id, &timestamp)?;
    let event = insert_event(
        &transaction,
        conversation_id,
        DurableEventKind::NodeAppended {
            node_id,
            parent_id: Some(parent_id),
            turn_id: Some(turn_id),
            owner_id: None,
            request_index: None,
            node_kind: NodeKind::System,
            status: "completed".into(),
            content,
        },
    )?;
    transaction.commit()?;
    Ok((
        Some(LocalContextSync {
            node_id,
            message,
            warnings: snapshot.warnings.clone(),
        }),
        Some(event),
    ))
}
