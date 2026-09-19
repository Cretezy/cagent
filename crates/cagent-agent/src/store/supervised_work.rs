#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn upsert_terminal(
    connection: &mut Connection,
    terminal: &crate::TerminalSnapshot,
) -> Result<(), RuntimeError> {
    let transaction = connection.transaction()?;
    let status = terminal_status_name(terminal.status);
    let preview_output = crate::output_preview(&terminal.output);
    transaction.execute(
        "INSERT INTO background_terminals (
            id, conversation_id, tool_call_node_id, owner_agent_run_id, read_safe, detached, anchor_node_id, command, status, created_at, started_at,
            completed_at, exit_code, output_base, output_cursor, output_bytes,
            discarded_bytes, truncated, preview_output, output, ansi_output
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)
         ON CONFLICT(id) DO UPDATE SET
            status = excluded.status,
            owner_agent_run_id = COALESCE(background_terminals.owner_agent_run_id, excluded.owner_agent_run_id),
            read_safe = COALESCE(background_terminals.read_safe, excluded.read_safe),
            completed_at = COALESCE(background_terminals.completed_at, excluded.completed_at),
            exit_code = excluded.exit_code, output_base = excluded.output_base,
            output_cursor = excluded.output_cursor, output_bytes = excluded.output_bytes,
            discarded_bytes = excluded.discarded_bytes, truncated = excluded.truncated,
            preview_output = excluded.preview_output, output = excluded.output,
            ansi_output = excluded.ansi_output",
        params![
            terminal.id.to_string(),
            terminal.owner.to_string(),
            terminal.tool_call_node_id.map(|id| id.to_string()),
            terminal.owner_agent_run_id.map(|id| id.to_string()),
            terminal.read_safe,
            terminal.read_safe.is_none(),
            terminal.tool_call_node_id.map(|id| id.to_string()),
            terminal.command,
            status,
            terminal.created_at,
            terminal.started_at,
            terminal.completed_at,
            terminal.exit_code,
            sqlite_u64(terminal.output_base, "terminal output base")?,
            sqlite_u64(terminal.output_cursor, "terminal output cursor")?,
            sqlite_u64(terminal.output_bytes, "terminal output bytes")?,
            sqlite_u64(terminal.discarded_bytes, "terminal discarded bytes")?,
            terminal.truncated,
            preview_output,
            terminal.output.as_bytes(),
            terminal.ansi_output.as_bytes(),
        ],
    )?;
    // A delegated run consumes its own terminal output. Its eventual agent-run
    // completion is the only result that should be delivered to the parent
    // conversation. Killed and restart-orphaned terminals are durable terminal
    // states, but they must not wake the model.
    if terminal.read_safe.is_some()
        && terminal.owner_agent_run_id.is_none()
        && matches!(
            terminal.status,
            crate::TerminalStatus::Exited | crate::TerminalStatus::TimedOut
        )
        && terminal.completed_at.is_some()
    {
        let envelope = terminal_completion_envelope(terminal);
        transaction.execute(
            "INSERT OR IGNORE INTO completion_mailbox
                (conversation_id, work_kind, work_id, envelope_json, completed_at)
             VALUES (?1, 'terminal', ?2, ?3, ?4)",
            params![
                terminal.owner.to_string(),
                terminal.id.to_string(),
                envelope.to_string(),
                terminal.completed_at
            ],
        )?;
    }
    transaction.commit()?;
    Ok(())
}

pub(super) fn load_terminal(
    connection: &Connection,
    conversation_id: ConversationId,
    id: crate::TerminalId,
) -> Result<crate::TerminalSnapshot, RuntimeError> {
    connection
        .query_row(
            "SELECT tool_call_node_id, owner_agent_run_id, read_safe, detached, anchor_node_id, command, status, created_at, started_at, completed_at,
                exit_code, output_base, output_cursor, output_bytes, discarded_bytes,
                truncated, output, ansi_output
         FROM background_terminals WHERE conversation_id = ?1 AND id = ?2",
            params![conversation_id.to_string(), id.to_string()],
            terminal_from_row(id, conversation_id),
        )
        .map_err(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => {
                RuntimeError::InvalidOption(format!("unknown terminal ID: {id}"))
            }
            other => RuntimeError::Database(other),
        })
}

pub(super) fn list_terminals(
    connection: &Connection,
    conversation_id: ConversationId,
) -> Result<Vec<crate::TerminalSnapshot>, RuntimeError> {
    ensure_session(connection, conversation_id)?;
    let mut statement = connection.prepare(
        "SELECT id FROM background_terminals WHERE conversation_id = ?1 ORDER BY created_at, id",
    )?;
    let ids = statement
        .query_map([conversation_id.to_string()], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    ids.into_iter()
        .map(|id| {
            let id = id.parse().map_err(|error| {
                RuntimeError::InvalidOption(format!("invalid stored terminal ID: {error}"))
            })?;
            load_terminal(connection, conversation_id, id)
        })
        .collect()
}

pub(super) fn list_terminal_previews(
    connection: &Connection,
    conversation_id: ConversationId,
) -> Result<Vec<crate::TerminalSnapshot>, RuntimeError> {
    ensure_session(connection, conversation_id)?;
    let mut statement = connection.prepare(
        "SELECT id, tool_call_node_id, owner_agent_run_id, read_safe, detached, anchor_node_id, command, status,
                created_at, started_at, completed_at, exit_code, output_base, output_cursor,
                output_bytes, discarded_bytes, truncated,
                CAST(preview_output AS BLOB), CAST(preview_output AS BLOB)
         FROM background_terminals
         WHERE conversation_id = ?1 ORDER BY created_at, id",
    )?;
    statement
        .query_map(
            [conversation_id.to_string()],
            terminal_with_id_from_row(conversation_id),
        )?
        .collect::<Result<Vec<_>, _>>()
        .map_err(RuntimeError::Database)
}

pub(super) fn list_terminal_previews_for_nodes(
    connection: &Connection,
    conversation_id: ConversationId,
    node_ids: &[NodeId],
) -> Result<Vec<crate::TerminalSnapshot>, RuntimeError> {
    if node_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = std::iter::repeat_n("?", node_ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT id, tool_call_node_id, owner_agent_run_id, read_safe, detached, anchor_node_id, command, status,
                created_at, started_at, completed_at, exit_code, output_base, output_cursor,
                output_bytes, discarded_bytes, truncated,
                CAST(preview_output AS BLOB), CAST(preview_output AS BLOB)
         FROM background_terminals
         WHERE conversation_id = ? AND tool_call_node_id IN ({placeholders})
         ORDER BY created_at, id"
    );
    let mut values = Vec::<rusqlite::types::Value>::with_capacity(node_ids.len() + 1);
    values.push(conversation_id.to_string().into());
    values.extend(node_ids.iter().map(ToString::to_string).map(Into::into));
    connection
        .prepare(&sql)?
        .query_map(
            rusqlite::params_from_iter(values),
            terminal_with_id_from_row(conversation_id),
        )?
        .collect::<Result<Vec<_>, _>>()
        .map_err(RuntimeError::Database)
}

pub(super) fn claim_completion(
    connection: &mut Connection,
    conversation_id: ConversationId,
    kind: &str,
    id: &str,
    claim_kind: &str,
) -> Result<bool, RuntimeError> {
    let timestamp = now();
    Ok(connection.execute(
        "UPDATE completion_mailbox SET claimed_at = ?1, claim_kind = ?2
         WHERE conversation_id = ?3 AND work_kind = ?4 AND work_id = ?5
           AND claimed_at IS NULL AND delivered_at IS NULL",
        params![timestamp, claim_kind, conversation_id.to_string(), kind, id],
    )? == 1)
}

pub(super) fn claim_agent_completions(
    connection: &mut Connection,
    conversation_id: ConversationId,
    ids: &[String],
    claim_kind: &str,
) -> Result<(), RuntimeError> {
    let transaction = connection.transaction()?;
    let timestamp = now();
    for id in ids {
        transaction.execute(
            "UPDATE completion_mailbox SET claimed_at = ?1, claim_kind = ?2
             WHERE conversation_id = ?3 AND work_kind = 'agent' AND work_id = ?4
               AND claimed_at IS NULL AND delivered_at IS NULL",
            params![timestamp, claim_kind, conversation_id.to_string(), id],
        )?;
    }
    transaction.commit()?;
    Ok(())
}

pub(super) fn append_pending_completion_notice(
    connection: &mut Connection,
    conversation_id: ConversationId,
) -> Result<Option<(NodeId, TurnId)>, RuntimeError> {
    let transaction = connection.transaction()?;
    let mut statement = transaction.prepare(
        "SELECT sequence, envelope_json FROM completion_mailbox
         WHERE conversation_id = ?1 AND claimed_at IS NULL AND delivered_at IS NULL
         ORDER BY sequence",
    )?;
    let rows = statement
        .query_map([conversation_id.to_string()], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    if rows.is_empty() {
        transaction.rollback()?;
        return Ok(None);
    }
    let (parent, turn): (String, String) = transaction.query_row(
        "SELECT c.active_node_id, n.turn_id FROM conversations c
         JOIN nodes n ON n.id = c.active_node_id WHERE c.id = ?1 AND n.turn_id IS NOT NULL",
        [conversation_id.to_string()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let parent_id: NodeId = parent
        .parse()
        .map_err(|error| RuntimeError::InvalidOption(format!("invalid stored node ID: {error}")))?;
    let turn_id: TurnId = turn
        .parse()
        .map_err(|error| RuntimeError::InvalidOption(format!("invalid stored turn ID: {error}")))?;
    let envelopes = rows
        .iter()
        .map(|(_, value)| decode_stored_json(value))
        .collect::<Result<Vec<serde_json::Value>, _>>()?;
    let content =
        serde_json::to_value(crate::SystemNodePayload::CompletionEnvelopes { envelopes })?;
    let node_id = NodeId::new();
    let timestamp = now();
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
    for (sequence, _) in rows {
        transaction.execute(
            "UPDATE completion_mailbox SET delivered_at = ?1, notice_node_id = ?2
             WHERE sequence = ?3 AND claimed_at IS NULL AND delivered_at IS NULL",
            params![timestamp, node_id.to_string(), sequence],
        )?;
    }
    transaction.commit()?;
    // Completion envelopes are durable model context but are not published to UI subscribers.
    let _ = event;
    Ok(Some((node_id, turn_id)))
}

/// Appends the durable transcript marker after an active task has observed
/// its cancellation token, making the marker a stable reconnect boundary.
pub(super) fn append_interrupt_notice(
    connection: &mut Connection,
    conversation_id: ConversationId,
    queued_steering: bool,
) -> Result<(Option<NodeId>, DurableEvent), RuntimeError> {
    let transaction = connection.transaction()?;
    let parent = transaction.query_row(
        "SELECT active_node_id FROM conversations WHERE id = ?1",
        [conversation_id.to_string()],
        |row| row.get::<_, String>(0),
    )?;
    let parent_id: NodeId = parent
        .parse()
        .map_err(|error| RuntimeError::InvalidOption(format!("invalid stored node ID: {error}")))?;
    let turn_id: Option<TurnId> = transaction
        .query_row(
            "SELECT turn_id FROM nodes WHERE id = ?1",
            [parent_id.to_string()],
            |row| row.get::<_, Option<String>>(0),
        )?
        .map(|id| id.parse::<TurnId>())
        .transpose()
        .map_err(|error| RuntimeError::InvalidOption(format!("invalid stored turn ID: {error}")))?;
    let turn_id = turn_id.or_else(|| Some(TurnId::new()));
    let mut content =
        serde_json::to_value(crate::SystemNodePayload::Interruption { queued_steering })?;
    content["transcript"] = json!("interrupt");
    let node_id = NodeId::new();
    let timestamp = now();
    transaction.execute(
        "INSERT INTO nodes (id, conversation_id, parent_id, turn_id, kind, status, role, content_json, created_at, completed_at)
         VALUES (?1, ?2, ?3, ?4, 'system', 'completed', 'system', ?5, ?6, ?6)",
        params![node_id.to_string(), conversation_id.to_string(), parent_id.to_string(), turn_id.map(|id| id.to_string()), content.to_string(), timestamp],
    )?;
    set_active(&transaction, conversation_id, node_id, &timestamp)?;
    let event = insert_event(
        &transaction,
        conversation_id,
        DurableEventKind::NodeAppended {
            node_id,
            parent_id: Some(parent_id),
            turn_id,
            owner_id: None,
            request_index: None,
            node_kind: NodeKind::System,
            status: "completed".into(),
            content,
        },
    )?;
    transaction.commit()?;
    Ok((Some(node_id), event))
}

fn terminal_completion_envelope(terminal: &crate::TerminalSnapshot) -> serde_json::Value {
    let excerpt = crate::shell_output_excerpt(&terminal.output, crate::DEFAULT_MODEL_OUTPUT_BYTES);
    json!({
        "type": "background_terminal_completion", "id": terminal.id,
        "command": terminal.command, "state": terminal_status_name(terminal.status),
        "exit_code": terminal.exit_code, "output": excerpt.output,
        "truncated": excerpt.truncated || terminal.truncated,
        "discarded_bytes": excerpt.discarded_bytes as u64 + terminal.discarded_bytes,
    })
}

fn terminal_from_row(
    id: crate::TerminalId,
    owner: ConversationId,
) -> impl FnOnce(&rusqlite::Row<'_>) -> rusqlite::Result<crate::TerminalSnapshot> {
    move |row| terminal_from_row_at(row, id, owner, 0)
}

fn terminal_with_id_from_row(
    owner: ConversationId,
) -> impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<crate::TerminalSnapshot> {
    move |row| {
        let encoded: String = row.get(0)?;
        let id = encoded.parse().map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?;
        terminal_from_row_at(row, id, owner, 1)
    }
}

fn terminal_from_row_at(
    row: &rusqlite::Row<'_>,
    id: crate::TerminalId,
    owner: ConversationId,
    offset: usize,
) -> rusqlite::Result<crate::TerminalSnapshot> {
    let node: Option<String> = row.get(offset)?;
    let owner_agent_run_id: Option<String> = row.get(offset + 1)?;
    let _anchor: Option<String> = row.get(offset + 4)?;
    let status: String = row.get(offset + 6)?;
    let bytes: Vec<u8> = row.get(offset + 16)?;
    let ansi_bytes: Vec<u8> = row.get(offset + 17)?;
    Ok(crate::TerminalSnapshot {
        id,
        owner,
        owner_agent_run_id: owner_agent_run_id
            .map(|id| id.parse())
            .transpose()
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    1,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?,
        tool_call_node_id: node
            .map(|node| {
                node.parse().map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })
            })
            .transpose()?,
        // `None` marks explicit detached user Bash. Older snapshots also
        // deserialize as `None`, but never have the required anchor node.
        read_safe: if row.get::<_, bool>(offset + 3)? {
            None
        } else {
            Some(row.get::<_, Option<bool>>(offset + 2)?.unwrap_or(false))
        },
        command: row.get(offset + 5)?,
        status: parse_terminal_status(&status)?,
        created_at: row.get(offset + 7)?,
        started_at: row.get(offset + 8)?,
        completed_at: row.get(offset + 9)?,
        exit_code: row.get(offset + 10)?,
        output_base: row.get::<_, u64>(offset + 11)?,
        output_cursor: row.get::<_, u64>(offset + 12)?,
        output_bytes: row.get::<_, u64>(offset + 13)?,
        discarded_bytes: row.get::<_, u64>(offset + 14)?,
        truncated: row.get(offset + 15)?,
        ansi_output: String::from_utf8_lossy(&ansi_bytes).into_owned(),
        output: String::from_utf8_lossy(&bytes).into_owned(),
    })
}

const fn terminal_status_name(status: crate::TerminalStatus) -> &'static str {
    match status {
        crate::TerminalStatus::Running => "running",
        crate::TerminalStatus::Terminating => "terminating",
        crate::TerminalStatus::Exited => "exited",
        crate::TerminalStatus::Killed => "killed",
        crate::TerminalStatus::TimedOut => "timed_out",
        crate::TerminalStatus::Orphaned => "orphaned",
    }
}

fn parse_terminal_status(value: &str) -> rusqlite::Result<crate::TerminalStatus> {
    match value {
        "running" => Ok(crate::TerminalStatus::Running),
        "terminating" => Ok(crate::TerminalStatus::Terminating),
        "exited" => Ok(crate::TerminalStatus::Exited),
        "killed" => Ok(crate::TerminalStatus::Killed),
        "timed_out" => Ok(crate::TerminalStatus::TimedOut),
        "orphaned" => Ok(crate::TerminalStatus::Orphaned),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}
