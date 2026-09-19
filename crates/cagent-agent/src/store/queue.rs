#![allow(clippy::too_many_arguments)] // Queue mutations mirror their complete persisted payloads.

#[allow(clippy::wildcard_imports)]
use super::*;

/// Persistence-only state for a queued row. Frontends deliberately see only
/// `QueuedMessage::blocked_by_startup`, not these database details.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QueuedMessageStatus {
    Queued,
    BlockedByStartup,
    Editing,
    EditingBlockedByStartup,
}

impl QueuedMessageStatus {
    const fn as_sql(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::BlockedByStartup => "blocked_by_startup",
            Self::Editing => "editing",
            Self::EditingBlockedByStartup => "editing_blocked_by_startup",
        }
    }

    fn parse(value: &str) -> Result<Self, RuntimeError> {
        match value {
            "queued" => Ok(Self::Queued),
            "blocked_by_startup" => Ok(Self::BlockedByStartup),
            "editing" => Ok(Self::Editing),
            "editing_blocked_by_startup" => Ok(Self::EditingBlockedByStartup),
            value => Err(RuntimeError::InvalidOption(format!(
                "invalid stored queued-message status: {value}"
            ))),
        }
    }

    const fn blocked_by_startup(self) -> bool {
        matches!(self, Self::BlockedByStartup | Self::EditingBlockedByStartup)
    }
}

pub(super) fn begin_editing_queued(
    connection: &Connection,
    conversation_id: ConversationId,
    id: QueuedMessageId,
) -> Result<(), RuntimeError> {
    let changed = connection.execute(
        "UPDATE queued_messages
         SET status = CASE status
             WHEN 'blocked_by_startup' THEN 'editing_blocked_by_startup'
             ELSE 'editing'
         END, updated_at = ?1
         WHERE id = ?2 AND conversation_id = ?3
           AND status IN ('queued', 'blocked_by_startup')",
        params![now(), id.to_string(), conversation_id.to_string()],
    )?;
    if changed != 1 {
        return Err(RuntimeError::QueuedMessageNotFound(id, conversation_id));
    }
    Ok(())
}

pub(super) fn queue_input(
    connection: &mut Connection,
    conversation_id: ConversationId,
    text: &str,
    target: QueueTarget,
    attachments: &[AttachmentSpec],
    images: &[crate::ImageAttachment],
    image_chips: &[crate::ImageChipRange],
    blocked_by_startup: bool,
    require_subagent: bool,
) -> Result<(QueuedMessage, DurableEvent), RuntimeError> {
    validate_queued_text(text)?;
    let transaction = connection.transaction()?;
    ensure_session_transaction(&transaction, conversation_id)?;
    let next_position: i64 = transaction.query_row(
        "SELECT COALESCE(MAX(position) + 1, 0) FROM queued_messages
         WHERE conversation_id = ?1",
        [conversation_id.to_string()],
        |row| row.get(0),
    )?;
    let message = QueuedMessage {
        id: QueuedMessageId::new(),
        position: sqlite_position(next_position)?,
        target,
        kind: crate::QueuedItemKind::Prompt,
        mode: None,
        command_text: None,
        text: text.into(),
        attachments: attachments.to_vec(),
        images: images.to_vec(),
        image_chips: image_chips.to_vec(),
        blocked_by_startup,
        require_subagent,
    };
    let now = now();
    transaction.execute(
        "INSERT INTO queued_messages (
            id, conversation_id, position, target, status, content_json, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
        params![
            message.id.to_string(),
            conversation_id.to_string(),
            next_position,
            target.as_str(),
            if blocked_by_startup {
                QueuedMessageStatus::BlockedByStartup.as_sql()
            } else {
                QueuedMessageStatus::Queued.as_sql()
            },
            json!({ "text": text, "attachments": attachments, "images": images, "image_chips": image_chips, "require_subagent": require_subagent }).to_string(),
            now,
        ],
    )?;
    insert_composer_history_entry(
        &transaction,
        conversation_id,
        "user_message",
        "prompt",
        text,
        attachments,
        images,
        image_chips,
        None,
        &now,
        None,
        images.is_empty(),
    )?;
    let event = insert_event(
        &transaction,
        conversation_id,
        DurableEventKind::QueuedInputCreated {
            message: message.clone(),
        },
    )?;
    transaction.commit()?;
    Ok((message, event))
}

pub(super) fn queue_mode_input(
    connection: &mut Connection,
    conversation_id: ConversationId,
    mode: &str,
    command_text: &str,
    text: &str,
    target: QueueTarget,
    attachments: &[AttachmentSpec],
) -> Result<(QueuedMessage, DurableEvent), RuntimeError> {
    validate_queued_text(text)?;
    let transaction = connection.transaction()?;
    ensure_session_transaction(&transaction, conversation_id)?;
    let next_position: i64 = transaction.query_row(
        "SELECT COALESCE(MAX(position) + 1, 0) FROM queued_messages WHERE conversation_id = ?1",
        [conversation_id.to_string()],
        |row| row.get(0),
    )?;
    let message = QueuedMessage {
        id: QueuedMessageId::new(),
        position: sqlite_position(next_position)?,
        target,
        kind: crate::QueuedItemKind::ModePrompt,
        mode: Some(mode.into()),
        command_text: Some(command_text.into()),
        text: text.into(),
        attachments: attachments.to_vec(),
        images: Vec::new(),
        image_chips: Vec::new(),
        blocked_by_startup: false,
        require_subagent: false,
    };
    let now = now();
    transaction.execute(
        "INSERT INTO queued_messages (
            id, conversation_id, position, target, status, content_json, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, 'queued', ?5, ?6, ?6)",
        params![
            message.id.to_string(),
            conversation_id.to_string(),
            next_position,
            target.as_str(),
            json!({
                "kind": "mode_prompt",
                "mode": mode,
                "command_text": command_text,
                "text": text,
                "attachments": attachments,
            })
            .to_string(),
            now,
        ],
    )?;
    let event = insert_event(
        &transaction,
        conversation_id,
        DurableEventKind::QueuedInputCreated {
            message: message.clone(),
        },
    )?;
    transaction.commit()?;
    Ok((message, event))
}

pub(super) fn queue_compact(
    connection: &mut Connection,
    conversation_id: ConversationId,
    target: QueueTarget,
    instructions: Option<&str>,
) -> Result<(QueuedMessage, DurableEvent), RuntimeError> {
    let instructions = instructions
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let transaction = connection.transaction()?;
    ensure_session_transaction(&transaction, conversation_id)?;
    let next_position: i64 = transaction.query_row(
        "SELECT COALESCE(MAX(position) + 1, 0) FROM queued_messages WHERE conversation_id = ?1",
        [conversation_id.to_string()],
        |row| row.get(0),
    )?;
    let message = QueuedMessage {
        id: QueuedMessageId::new(),
        position: sqlite_position(next_position)?,
        target,
        kind: crate::QueuedItemKind::Compact,
        mode: None,
        command_text: None,
        text: instructions.unwrap_or_default().into(),
        attachments: Vec::new(),
        images: Vec::new(),
        image_chips: Vec::new(),
        blocked_by_startup: false,
        require_subagent: false,
    };
    let now = now();
    transaction.execute(
        "INSERT INTO queued_messages (
            id, conversation_id, position, target, status, content_json, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, 'queued', ?5, ?6, ?6)",
        params![
            message.id.to_string(),
            conversation_id.to_string(),
            next_position,
            target.as_str(),
            json!({ "kind": "compact", "text": instructions.unwrap_or_default() }).to_string(),
            now,
        ],
    )?;
    let event = insert_event(
        &transaction,
        conversation_id,
        DurableEventKind::QueuedInputCreated {
            message: message.clone(),
        },
    )?;
    transaction.commit()?;
    Ok((message, event))
}

pub(super) fn replace_queued(
    connection: &mut Connection,
    conversation_id: ConversationId,
    id: QueuedMessageId,
    text: &str,
    target: Option<QueueTarget>,
    attachments: &[AttachmentSpec],
    images: &[crate::ImageAttachment],
    image_chips: &[crate::ImageChipRange],
) -> Result<(QueuedMessage, DurableEvent), RuntimeError> {
    validate_queued_text(text)?;
    let transaction = connection.transaction()?;
    let mut message = load_queued(&transaction, conversation_id, id)?;
    if message.kind == crate::QueuedItemKind::Compact {
        return Err(RuntimeError::InvalidOption(
            "queued compaction actions cannot be edited as messages".into(),
        ));
    }
    let target = target.unwrap_or(message.target);
    let changed = transaction.execute(
        "UPDATE queued_messages SET content_json = ?1, target = ?2,
             status = CASE status WHEN 'editing_blocked_by_startup' THEN 'blocked_by_startup' ELSE 'queued' END,
             updated_at = ?3
         WHERE id = ?4 AND conversation_id = ?5 AND status IN (?6, ?7, ?8, ?9)",
        params![
            json!({ "text": text, "attachments": attachments, "images": images, "image_chips": image_chips, "require_subagent": message.require_subagent }).to_string(),
            target.as_str(),
            now(),
            id.to_string(),
            conversation_id.to_string(),
            QueuedMessageStatus::Queued.as_sql(),
            QueuedMessageStatus::BlockedByStartup.as_sql(),
            QueuedMessageStatus::Editing.as_sql(),
            QueuedMessageStatus::EditingBlockedByStartup.as_sql(),
        ],
    )?;
    if changed != 1 {
        return Err(RuntimeError::QueuedMessageNotFound(id, conversation_id));
    }
    message.text = text.into();
    message.kind = crate::QueuedItemKind::Prompt;
    message.mode = None;
    message.command_text = None;
    message.target = target;
    message.attachments = attachments.to_vec();
    message.images = images.to_vec();
    message.image_chips = image_chips.to_vec();
    let event = insert_event(
        &transaction,
        conversation_id,
        DurableEventKind::QueuedInputReplaced {
            message: message.clone(),
        },
    )?;
    transaction.commit()?;
    Ok((message, event))
}

pub(super) fn replace_queued_mode_input(
    connection: &mut Connection,
    conversation_id: ConversationId,
    id: QueuedMessageId,
    mode: &str,
    command_text: &str,
    text: &str,
    target: Option<QueueTarget>,
    attachments: &[AttachmentSpec],
) -> Result<(QueuedMessage, DurableEvent), RuntimeError> {
    validate_queued_text(text)?;
    let transaction = connection.transaction()?;
    let mut message = load_queued(&transaction, conversation_id, id)?;
    if message.kind == crate::QueuedItemKind::Compact {
        return Err(RuntimeError::InvalidOption(
            "queued compaction actions cannot be edited as messages".into(),
        ));
    }
    let target = target.unwrap_or(message.target);
    let changed = transaction.execute(
        "UPDATE queued_messages SET content_json = ?1, target = ?2,
             status = CASE status WHEN 'editing_blocked_by_startup' THEN 'blocked_by_startup' ELSE 'queued' END,
             updated_at = ?3
         WHERE id = ?4 AND conversation_id = ?5 AND status IN (?6, ?7, ?8, ?9)",
        params![
            json!({
                "kind": "mode_prompt",
                "mode": mode,
                "command_text": command_text,
                "text": text,
                "attachments": attachments,
            })
            .to_string(),
            target.as_str(),
            now(),
            id.to_string(),
            conversation_id.to_string(),
            QueuedMessageStatus::Queued.as_sql(),
            QueuedMessageStatus::BlockedByStartup.as_sql(),
            QueuedMessageStatus::Editing.as_sql(),
            QueuedMessageStatus::EditingBlockedByStartup.as_sql(),
        ],
    )?;
    if changed != 1 {
        return Err(RuntimeError::QueuedMessageNotFound(id, conversation_id));
    }
    message.kind = crate::QueuedItemKind::ModePrompt;
    message.mode = Some(mode.into());
    message.command_text = Some(command_text.into());
    message.text = text.into();
    message.target = target;
    message.attachments = attachments.to_vec();
    let event = insert_event(
        &transaction,
        conversation_id,
        DurableEventKind::QueuedInputReplaced {
            message: message.clone(),
        },
    )?;
    transaction.commit()?;
    Ok((message, event))
}

pub(super) fn delete_queued(
    connection: &mut Connection,
    conversation_id: ConversationId,
    id: QueuedMessageId,
) -> Result<((), DurableEvent), RuntimeError> {
    let transaction = connection.transaction()?;
    let changed = transaction.execute(
        "DELETE FROM queued_messages WHERE id = ?1 AND conversation_id = ?2",
        params![id.to_string(), conversation_id.to_string()],
    )?;
    if changed != 1 {
        return Err(RuntimeError::QueuedMessageNotFound(id, conversation_id));
    }
    let event = insert_event(
        &transaction,
        conversation_id,
        DurableEventKind::QueuedInputDeleted { id },
    )?;
    transaction.commit()?;
    Ok(((), event))
}

pub(super) fn promote_queued(
    connection: &mut Connection,
    conversation_id: ConversationId,
    id: QueuedMessageId,
) -> Result<(QueuedMessage, DurableEvent), RuntimeError> {
    let transaction = connection.transaction()?;
    let mut message = load_queued(&transaction, conversation_id, id)?;
    let changed = transaction.execute(
        "UPDATE queued_messages SET target = 'next_boundary', updated_at = ?1
         WHERE id = ?2 AND conversation_id = ?3 AND status IN (?4, ?5)",
        params![
            now(),
            id.to_string(),
            conversation_id.to_string(),
            QueuedMessageStatus::Queued.as_sql(),
            QueuedMessageStatus::BlockedByStartup.as_sql()
        ],
    )?;
    if changed != 1 {
        return Err(RuntimeError::QueuedMessageNotFound(id, conversation_id));
    }
    message.target = QueueTarget::NextBoundary;
    let event = insert_event(
        &transaction,
        conversation_id,
        DurableEventKind::QueuedInputPromoted {
            message: message.clone(),
        },
    )?;
    transaction.commit()?;
    Ok((message, event))
}

pub(super) fn dispatch_queued(
    connection: &mut Connection,
    conversation_id: ConversationId,
    expected: &QueuedMessage,
    attachments: &[CapturedAttachment],
    attachment_specs: &[AttachmentSpec],
    deferred_attachment_paths: &[std::path::PathBuf],
) -> MultiEventResult<DispatchedQueued> {
    let transaction = connection.transaction()?;
    let message = load_queued(&transaction, conversation_id, expected.id)?;
    if message != *expected {
        return Err(RuntimeError::QueuedMessageChanged(expected.id));
    }
    let changed = transaction.execute(
        "DELETE FROM queued_messages WHERE id = ?1 AND conversation_id = ?2",
        params![expected.id.to_string(), conversation_id.to_string()],
    )?;
    if changed != 1 {
        return Err(RuntimeError::QueuedMessageNotFound(
            expected.id,
            conversation_id,
        ));
    }
    let mode_changed = if let Some(mode) = &message.mode {
        let changed = transaction.execute(
            "UPDATE conversations SET active_mode = ?1, updated_at = ?2 WHERE id = ?3",
            params![mode, now(), conversation_id.to_string()],
        )?;
        if changed != 1 {
            return Err(RuntimeError::ConversationNotFound(conversation_id));
        }
        Some(insert_event(
            &transaction,
            conversation_id,
            DurableEventKind::ModeChanged {
                mode: mode.clone(),
                pending: true,
            },
        )?)
    } else {
        None
    };
    let ((node_id, turn_id), appended) = append_user_in_transaction(
        &transaction,
        conversation_id,
        &message.text,
        attachments,
        attachment_specs,
        deferred_attachment_paths,
        &message.images,
        &message.image_chips,
    )?;
    let dispatched = insert_event(
        &transaction,
        conversation_id,
        DurableEventKind::QueuedInputDispatched {
            id: expected.id,
            node_id,
        },
    )?;
    transaction.commit()?;
    let mut events = mode_changed.into_iter().collect::<Vec<_>>();
    events.push(appended);
    events.push(dispatched);
    Ok(((node_id, turn_id, message.text, message.mode), events))
}

pub(super) fn peek_next_queued(
    connection: &Connection,
    conversation_id: ConversationId,
    target: QueueTarget,
) -> Result<Option<QueuedMessage>, RuntimeError> {
    let id = connection
        .query_row(
            "SELECT candidate.id FROM queued_messages candidate
             WHERE candidate.conversation_id = ?1 AND candidate.target = ?2 AND candidate.status = ?3
               AND NOT EXISTS (
                   SELECT 1 FROM queued_messages editing
                   WHERE editing.conversation_id = candidate.conversation_id
                     AND editing.position <= candidate.position
                     AND editing.status IN ('editing', 'editing_blocked_by_startup')
               )
             ORDER BY position LIMIT 1",
            params![
                conversation_id.to_string(),
                target.as_str(),
                QueuedMessageStatus::Queued.as_sql()
            ],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let Some(id) = id else {
        return Ok(None);
    };
    let id = id.parse().map_err(|error| {
        RuntimeError::InvalidOption(format!("invalid stored queued-message ID: {error}"))
    })?;
    let transaction = connection.unchecked_transaction()?;
    let message = load_queued(&transaction, conversation_id, id)?;
    transaction.rollback()?;
    Ok(Some(message))
}

pub(super) fn append_user_in_transaction(
    transaction: &Transaction<'_>,
    conversation_id: ConversationId,
    text: &str,
    attachments: &[CapturedAttachment],
    attachment_specs: &[AttachmentSpec],
    deferred_attachment_paths: &[std::path::PathBuf],
    images: &[crate::ImageAttachment],
    image_chips: &[crate::ImageChipRange],
) -> Result<((NodeId, TurnId), DurableEvent), RuntimeError> {
    let (parent_id, agent, mode): (String, String, String) = transaction
        .query_row(
            "SELECT active_node_id, active_agent, active_mode FROM conversations WHERE id = ?1",
            [conversation_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => {
                RuntimeError::ConversationNotFound(conversation_id)
            }
            other => RuntimeError::Database(other),
        })?;
    let parent_id: NodeId = parent_id
        .parse()
        .map_err(|error| RuntimeError::InvalidOption(format!("invalid stored node ID: {error}")))?;
    let node_id = NodeId::new();
    let turn_id = TurnId::new();
    let command = transaction
        .query_row(
            "SELECT id, text FROM composer_history
             WHERE conversation_id = ?1
               AND entry_kind = 'slash_command'
               AND node_id IS NULL
               AND slash_command_user_text = ?2
             ORDER BY id DESC LIMIT 1",
            params![conversation_id.to_string(), text],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    let mut content = if attachments.is_empty()
        && attachment_specs.is_empty()
        && deferred_attachment_paths.is_empty()
        && images.is_empty()
    {
        json!({ "text": text })
    } else {
        json!({
            "text": text,
            "attachments": attachments,
            "attachment_specs": attachment_specs,
            "deferred_attachment_paths": deferred_attachment_paths,
            "images": images,
            "image_chips": image_chips,
        })
    };
    if let Some((_, command_text)) = &command
        && let Some((label, display_text)) = slash_command_presentation(command_text)
    {
        content["display_label"] = json!(label);
        if display_text != text {
            content["display_text"] = json!(display_text);
        }
    }
    let now = now();
    transaction.execute(
        "INSERT INTO nodes (
            id, conversation_id, parent_id, turn_id, kind, status, role, content_json,
            agent, mode, created_at, completed_at
         ) VALUES (?1, ?2, ?3, ?4, 'user_message', 'completed', 'user', ?5, ?6, ?7, ?8, ?8)",
        params![
            node_id.to_string(),
            conversation_id.to_string(),
            parent_id.to_string(),
            turn_id.to_string(),
            content.to_string(),
            agent,
            mode,
            now,
        ],
    )?;
    if let Some((command_history_id, _)) = command {
        transaction.execute(
            "UPDATE composer_history SET node_id = ?1 WHERE id = ?2",
            params![node_id.to_string(), command_history_id],
        )?;
    } else {
        insert_composer_history_entry(
            transaction,
            conversation_id,
            "user_message",
            "prompt",
            text,
            attachment_specs,
            images,
            image_chips,
            Some(node_id),
            &now,
            None,
            images.is_empty(),
        )?;
    }
    let title = fallback_conversation_title(text);
    transaction.execute(
        "UPDATE conversations SET title = ?1, title_source = 'fallback', title_generation_state = 'pending'
         WHERE id = ?2 AND title IS NULL",
        params![title, conversation_id.to_string()],
    )?;
    set_active(transaction, conversation_id, node_id, &now)?;
    let event = insert_event(
        transaction,
        conversation_id,
        DurableEventKind::NodeAppended {
            node_id,
            parent_id: Some(parent_id),
            turn_id: Some(turn_id),
            owner_id: None,
            request_index: None,
            node_kind: NodeKind::UserMessage,
            status: "completed".into(),
            content,
        },
    )?;
    Ok(((node_id, turn_id), event))
}

/// Returns presentation-only data for a slash command that submitted a user
/// prompt. The stored user `text` remains the model-facing prompt.
fn slash_command_presentation(command: &str) -> Option<(String, String)> {
    let (name, remainder) = split_command_head(command.trim())?;
    let name = name.strip_prefix('/')?;
    if name.is_empty() {
        return None;
    }
    let (label, display_text) = if name.eq_ignore_ascii_case("mode") {
        let (mode, text) = split_command_head(&remainder)?;
        (title_case_mode_name(mode), text)
    } else {
        (title_case_mode_name(name), remainder)
    };
    (!display_text.is_empty()).then_some((label, display_text))
}

fn split_command_head(input: &str) -> Option<(&str, String)> {
    let boundary = input.find(char::is_whitespace)?;
    let head = &input[..boundary];
    let tail = input[boundary..].trim_start().to_owned();
    (!head.is_empty()).then_some((head, tail))
}

fn title_case_mode_name(name: &str) -> String {
    name.split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut characters = part.chars();
            let Some(first) = characters.next() else {
                return String::new();
            };
            first.to_uppercase().chain(characters).collect()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn fallback_conversation_title(text: &str) -> String {
    let first_line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let title = first_line
        .chars()
        .filter(|character| !character.is_control())
        .collect::<String>();
    let title = title.trim();
    if title.is_empty() {
        String::new()
    } else {
        title.into()
    }
}

fn load_queued(
    transaction: &Connection,
    conversation_id: ConversationId,
    id: QueuedMessageId,
) -> Result<QueuedMessage, RuntimeError> {
    let row = transaction
        .query_row(
            "SELECT position, target, content_json, status FROM queued_messages
             WHERE id = ?1 AND conversation_id = ?2",
            params![id.to_string(), conversation_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?
        .ok_or(RuntimeError::QueuedMessageNotFound(id, conversation_id))?;
    let content: serde_json::Value = decode_stored_json(&row.2)?;
    let kind = match content.get("kind").and_then(serde_json::Value::as_str) {
        Some("compact") => crate::QueuedItemKind::Compact,
        Some("mode_prompt") => crate::QueuedItemKind::ModePrompt,
        Some("prompt") | None => crate::QueuedItemKind::Prompt,
        Some(value) => {
            return Err(RuntimeError::InvalidOption(format!(
                "invalid queued item kind: {value}"
            )));
        }
    };
    let text = content
        .get("text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if kind == crate::QueuedItemKind::Prompt && text.is_empty() {
        return Err(RuntimeError::InvalidOption(
            "queued message is missing text".into(),
        ));
    }
    let mode = content
        .get("mode")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let command_text = content
        .get("command_text")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    if kind == crate::QueuedItemKind::ModePrompt
        && (mode.as_deref().is_none_or(str::is_empty)
            || command_text.as_deref().is_none_or(str::is_empty)
            || text.is_empty())
    {
        return Err(RuntimeError::InvalidOption(
            "queued mode command is missing required fields".into(),
        ));
    }
    let attachments = content.get("attachments").map_or_else(
        || Ok(Vec::new()),
        |value| serde_json::from_value(value.clone()),
    )?;
    let images = content.get("images").map_or_else(
        || Ok(Vec::new()),
        |value| serde_json::from_value(value.clone()),
    )?;
    let image_chips = content.get("image_chips").map_or_else(
        || Ok(Vec::new()),
        |value| serde_json::from_value(value.clone()),
    )?;
    Ok(QueuedMessage {
        id,
        position: sqlite_position(row.0)?,
        target: row.1.parse()?,
        kind,
        mode,
        command_text,
        text: text.into(),
        attachments,
        images,
        image_chips,
        blocked_by_startup: QueuedMessageStatus::parse(&row.3)?.blocked_by_startup(),
        require_subagent: content
            .get("require_subagent")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    })
}

pub(super) fn list_queued_messages(
    connection: &Connection,
    conversation_id: ConversationId,
) -> Result<Vec<QueuedMessage>, RuntimeError> {
    let ids = connection
        .prepare(
            "SELECT id FROM queued_messages
             WHERE conversation_id = ?1 ORDER BY position",
        )?
        .query_map([conversation_id.to_string()], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|id| parse_stored_id(Some(id), "queued message"))
        .collect::<Result<Vec<QueuedMessageId>, RuntimeError>>()?;
    ids.into_iter()
        .map(|id| load_queued(connection, conversation_id, id))
        .collect()
}

pub(super) fn peek_next_startup_queued(
    connection: &Connection,
    conversation_id: ConversationId,
) -> Result<Option<QueuedMessage>, RuntimeError> {
    let id = connection.query_row(
        "SELECT id FROM queued_messages WHERE conversation_id = ?1 AND status = ?2 ORDER BY position LIMIT 1",
        params![conversation_id.to_string(), QueuedMessageStatus::BlockedByStartup.as_sql()], |row| row.get::<_, String>(0),
    ).optional()?;
    let Some(id) = id else {
        return Ok(None);
    };
    let id = id.parse().map_err(|error| {
        RuntimeError::InvalidOption(format!("invalid stored queued-message ID: {error}"))
    })?;
    let transaction = connection.unchecked_transaction()?;
    let message = load_queued(&transaction, conversation_id, id)?;
    transaction.rollback()?;
    Ok(Some(message))
}

fn validate_queued_text(text: &str) -> Result<(), RuntimeError> {
    if text.trim().is_empty() {
        Err(RuntimeError::EmptyQueuedInput)
    } else {
        Ok(())
    }
}

fn sqlite_position(position: i64) -> Result<u64, RuntimeError> {
    position
        .try_into()
        .map_err(|_| RuntimeError::InvalidOption("negative queued-message position".into()))
}

pub(super) fn ensure_session_transaction(
    transaction: &Transaction<'_>,
    id: ConversationId,
) -> Result<(), RuntimeError> {
    let exists = transaction
        .query_row(
            "SELECT 1 FROM conversations WHERE id = ?1",
            [id.to_string()],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if exists {
        Ok(())
    } else {
        Err(RuntimeError::ConversationNotFound(id))
    }
}
