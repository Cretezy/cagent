#![allow(clippy::items_after_test_module)] // Status conversion helpers remain with their database mapping tests.

#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn create_agent_run(
    connection: &mut Connection,
    conversation_id: ConversationId,
    parent_turn_id: TurnId,
    profile: &str,
    model: &ModelRef,
    effort: Option<&str>,
    task: &str,
) -> Result<crate::AgentRun, RuntimeError> {
    if task.trim().is_empty() {
        return Err(RuntimeError::InvalidOption(
            "delegated task must not be empty".into(),
        ));
    }
    let transaction = connection.transaction()?;
    ensure_session_transaction(&transaction, conversation_id)?;
    let sequence: i64 = transaction.query_row(
        "SELECT COALESCE(MAX(sequence) + 1, 0) FROM agent_runs WHERE conversation_id = ?1 AND parent_turn_id = ?2",
        params![conversation_id.to_string(), parent_turn_id.to_string()],
        |row| row.get(0),
    )?;
    let id = crate::AgentRunId::new();
    let created_at = now();
    transaction.execute(
        "INSERT INTO agent_runs (id, conversation_id, parent_turn_id, sequence, profile, provider, model, effort, task, status, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'queued', ?10)",
        params![id.to_string(), conversation_id.to_string(), parent_turn_id.to_string(), sequence, profile, model.provider, model.model, effort, task, created_at],
    )?;
    transaction.execute(
        "INSERT INTO agent_run_events (run_id, kind, content_json, created_at) VALUES (?1, 'queued', '{}', ?2)",
        params![id.to_string(), created_at],
    )?;
    transaction.commit()?;
    load_agent_run(connection, conversation_id, id)
}

pub(super) fn set_agent_run_status(
    connection: &mut Connection,
    conversation_id: ConversationId,
    id: crate::AgentRunId,
    status: crate::AgentRunStatus,
    result: Option<&str>,
    error: Option<&str>,
    usage: Option<&crate::ModelUsage>,
) -> Result<crate::AgentRun, RuntimeError> {
    let transaction = connection.transaction()?;
    let current: String = transaction
        .query_row(
            "SELECT status FROM agent_runs WHERE id = ?1 AND conversation_id = ?2",
            params![id.to_string(), conversation_id.to_string()],
            |row| row.get(0),
        )
        .map_err(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => {
                RuntimeError::InvalidOption(format!("unknown delegated agent ID: {id}"))
            }
            other => RuntimeError::Database(other),
        })?;
    let current = parse_agent_run_status(&current)?;
    if current.is_terminal() {
        transaction.rollback()?;
        return load_agent_run_summary(connection, conversation_id, id);
    }
    let timestamp = now();
    let started_at = (status == crate::AgentRunStatus::Running).then_some(timestamp.as_str());
    let completed_at = status.is_terminal().then_some(timestamp.as_str());
    let usage_json = usage.map(serde_json::to_string).transpose()?;
    transaction.execute(
        "UPDATE agent_runs SET status = ?1, result = COALESCE(?2, result), error = COALESCE(?3, error),
            usage_json = COALESCE(?4, usage_json), started_at = COALESCE(started_at, ?5),
            completed_at = COALESCE(completed_at, ?6) WHERE id = ?7 AND conversation_id = ?8",
        params![agent_run_status_name(status), result, error, usage_json, started_at, completed_at, id.to_string(), conversation_id.to_string()],
    )?;
    transaction.execute(
        "INSERT INTO agent_run_events (run_id, kind, content_json, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![id.to_string(), agent_run_status_name(status), json!({"result": result, "error": error}).to_string(), timestamp],
    )?;
    if matches!(
        status,
        crate::AgentRunStatus::Completed
            | crate::AgentRunStatus::Failed
            | crate::AgentRunStatus::Cancelled
    ) {
        let profile: String = transaction.query_row(
            "SELECT profile FROM agent_runs WHERE id = ?1",
            [id.to_string()],
            |row| row.get(0),
        )?;
        let envelope = json!({
            "type": "sub_agent_completion",
            "id": id,
            "profile": profile,
            "state": agent_run_status_name(status),
            "message": result.filter(|value| !value.trim().is_empty()),
        });
        transaction.execute(
            "INSERT OR IGNORE INTO completion_mailbox
                (conversation_id, work_kind, work_id, envelope_json, completed_at)
             VALUES (?1, 'agent', ?2, ?3, ?4)",
            params![
                conversation_id.to_string(),
                id.to_string(),
                envelope.to_string(),
                timestamp
            ],
        )?;
    }
    transaction.commit()?;
    load_agent_run_summary(connection, conversation_id, id)
}

pub(super) fn load_agent_run(
    connection: &Connection,
    conversation_id: ConversationId,
    id: crate::AgentRunId,
) -> Result<crate::AgentRun, RuntimeError> {
    let mut run = load_agent_run_summary(connection, conversation_id, id)?;
    run.timeline = load_agent_run_timeline(connection, id)?;
    run.activity = load_agent_run_activity(connection, id)?;
    Ok(run)
}

/// Status reads never decode the run's potentially large transcript.
pub(super) fn load_agent_run_summary(
    connection: &Connection,
    conversation_id: ConversationId,
    id: crate::AgentRunId,
) -> Result<crate::AgentRun, RuntimeError> {
    let row = connection.query_row(
        "SELECT parent_turn_id, sequence, profile, provider, model, effort, task, status, result, error, usage_json, created_at, started_at, completed_at
         FROM agent_runs WHERE id = ?1 AND conversation_id = ?2",
        params![id.to_string(), conversation_id.to_string()],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?, row.get::<_, String>(4)?, row.get::<_, Option<String>>(5)?, row.get::<_, String>(6)?, row.get::<_, String>(7)?, row.get::<_, Option<String>>(8)?, row.get::<_, Option<String>>(9)?, row.get::<_, Option<String>>(10)?, row.get::<_, String>(11)?, row.get::<_, Option<String>>(12)?, row.get::<_, Option<String>>(13)?)),
    ).map_err(|error| match error {
        rusqlite::Error::QueryReturnedNoRows => RuntimeError::InvalidOption(format!("unknown delegated agent ID: {id}")),
        other => RuntimeError::Database(other),
    })?;
    Ok(crate::AgentRun {
        id,
        conversation_id,
        parent_turn_id: row.0.parse().map_err(|error| {
            RuntimeError::InvalidOption(format!("invalid stored turn ID: {error}"))
        })?,
        sequence: sqlite_nonnegative(row.1, "agent run sequence")?,
        profile: row.2,
        model: ModelRef {
            provider: row.3,
            model: row.4,
        },
        effort: row.5,
        task: row.6,
        status: parse_agent_run_status(&row.7)?,
        result: row.8,
        error: row.9,
        usage: row.10.map(|usage| decode_stored_json(&usage)).transpose()?,
        created_at: row.11,
        started_at: row.12,
        completed_at: row.13,
        timeline: Vec::new(),
        activity: Vec::new(),
    })
}

pub(super) fn append_agent_run_assistant(
    connection: &mut Connection,
    conversation_id: ConversationId,
    id: crate::AgentRunId,
    text: &str,
) -> Result<crate::AgentRun, RuntimeError> {
    if text.trim().is_empty() {
        return load_agent_run_summary(connection, conversation_id, id);
    }
    let transaction = connection.transaction()?;
    let exists = transaction.query_row(
        "SELECT 1 FROM agent_runs WHERE id = ?1 AND conversation_id = ?2",
        params![id.to_string(), conversation_id.to_string()],
        |_| Ok(()),
    );
    if matches!(exists, Err(rusqlite::Error::QueryReturnedNoRows)) {
        return Err(RuntimeError::InvalidOption(format!(
            "unknown delegated agent ID: {id}"
        )));
    }
    exists?;
    let created_at = now();
    transaction.execute(
        "INSERT INTO agent_run_events (run_id, kind, content_json, created_at) VALUES (?1, 'assistant', ?2, ?3)",
        params![id.to_string(), json!({ "text": text }).to_string(), created_at],
    )?;
    transaction.commit()?;
    load_agent_run_summary(connection, conversation_id, id)
}

pub(super) fn list_agent_runs(
    connection: &Connection,
    conversation_id: ConversationId,
) -> Result<Vec<crate::AgentRun>, RuntimeError> {
    list_agent_runs_with(connection, conversation_id, load_agent_run)
}

pub(super) fn list_agent_run_summaries(
    connection: &Connection,
    conversation_id: ConversationId,
) -> Result<Vec<crate::AgentRun>, RuntimeError> {
    list_agent_runs_with(connection, conversation_id, load_agent_run_summary)
}

fn list_agent_runs_with(
    connection: &Connection,
    conversation_id: ConversationId,
    load: fn(
        &Connection,
        ConversationId,
        crate::AgentRunId,
    ) -> Result<crate::AgentRun, RuntimeError>,
) -> Result<Vec<crate::AgentRun>, RuntimeError> {
    ensure_session(connection, conversation_id)?;
    let mut statement = connection.prepare(
        "SELECT id FROM agent_runs WHERE conversation_id = ?1 ORDER BY parent_turn_id, sequence",
    )?;
    let ids = statement
        .query_map([conversation_id.to_string()], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    ids.into_iter()
        .map(|id| {
            let id = id.parse().map_err(|error| {
                RuntimeError::InvalidOption(format!("invalid stored agent run ID: {error}"))
            })?;
            load(connection, conversation_id, id)
        })
        .collect()
}

pub(super) fn list_agent_runs_for_turns(
    connection: &Connection,
    conversation_id: ConversationId,
    turn_ids: &[TurnId],
) -> Result<Vec<crate::AgentRun>, RuntimeError> {
    if turn_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = std::iter::repeat_n("?", turn_ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT id FROM agent_runs
         WHERE conversation_id = ? AND parent_turn_id IN ({placeholders})
         ORDER BY parent_turn_id, sequence"
    );
    let mut values = Vec::<rusqlite::types::Value>::with_capacity(turn_ids.len() + 1);
    values.push(conversation_id.to_string().into());
    values.extend(turn_ids.iter().map(ToString::to_string).map(Into::into));
    let ids = connection
        .prepare(&sql)?
        .query_map(rusqlite::params_from_iter(values), |row| {
            row.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    ids.into_iter()
        .map(|id| {
            let id = id.parse().map_err(|error| {
                RuntimeError::InvalidOption(format!("invalid stored agent run ID: {error}"))
            })?;
            load_agent_run_summary(connection, conversation_id, id)
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub(super) fn append_agent_run_activity(
    connection: &mut Connection,
    conversation_id: ConversationId,
    id: crate::AgentRunId,
    tool: &str,
    arguments: &serde_json::Value,
    output: &serde_json::Value,
    is_error: bool,
    permission_audit: Option<&crate::PermissionAudit>,
) -> Result<crate::AgentRun, RuntimeError> {
    let transaction = connection.transaction()?;
    let exists = transaction.query_row(
        "SELECT 1 FROM agent_runs WHERE id = ?1 AND conversation_id = ?2",
        params![id.to_string(), conversation_id.to_string()],
        |_| Ok(()),
    );
    if matches!(exists, Err(rusqlite::Error::QueryReturnedNoRows)) {
        return Err(RuntimeError::InvalidOption(format!(
            "unknown delegated agent ID: {id}"
        )));
    }
    exists?;
    let sequence: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM agent_run_events WHERE run_id = ?1 AND kind = 'tool'",
        [id.to_string()],
        |row| row.get(0),
    )?;
    let created_at = now();
    super::patch_diffs::record_patch_diff(&transaction, conversation_id, tool, output, is_error)?;
    let content = json!({
        "sequence": sequence,
        "tool": tool,
        "arguments": bounded_agent_activity_value(arguments),
        "output": bounded_agent_activity_output(tool, output),
        "is_error": is_error,
        "permission_audit": permission_audit,
    });
    transaction.execute(
        "INSERT INTO agent_run_events (run_id, kind, content_json, created_at) VALUES (?1, 'tool', ?2, ?3)",
        params![id.to_string(), content.to_string(), created_at],
    )?;
    transaction.commit()?;
    load_agent_run_summary(connection, conversation_id, id)
}

fn load_agent_run_activity(
    connection: &Connection,
    id: crate::AgentRunId,
) -> Result<Vec<crate::AgentRunActivity>, RuntimeError> {
    let mut statement = connection.prepare(
        "SELECT content_json, created_at FROM agent_run_events WHERE run_id = ?1 AND kind = 'tool' ORDER BY sequence",
    )?;
    statement
        .query_map([id.to_string()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .map(|row| {
            let (content, created_at) = row?;
            let value: serde_json::Value = decode_stored_json(&content)?;
            Ok(crate::AgentRunActivity {
                sequence: value["sequence"].as_u64().unwrap_or_default(),
                tool: value["tool"].as_str().unwrap_or("unknown").into(),
                arguments: value["arguments"].clone(),
                output: value["output"].clone(),
                is_error: value["is_error"].as_bool().unwrap_or(true),
                permission_audit: value
                    .get("permission_audit")
                    .filter(|audit| !audit.is_null())
                    .cloned()
                    .map(serde_json::from_value)
                    .transpose()?,
                created_at,
            })
        })
        .collect()
}

fn load_agent_run_timeline(
    connection: &Connection,
    id: crate::AgentRunId,
) -> Result<Vec<crate::AgentRunTimelineEntry>, RuntimeError> {
    read_agent_run_timeline(connection, id, 0, i64::MAX)
}

pub(super) fn load_agent_run_log_page(
    connection: &Connection,
    conversation_id: ConversationId,
    id: crate::AgentRunId,
    after: u64,
) -> Result<crate::AgentRunLogPage, RuntimeError> {
    // Validate ownership even for empty pages; IDs from another conversation
    // must never expose its transcript.
    load_agent_run_summary(connection, conversation_id, id)?;
    let mut entries = read_agent_run_timeline(connection, id, after, 65)?;
    let has_more = entries.len() > 64;
    entries.truncate(64);
    let next_after = entries.last().map_or(after, |entry| match entry {
        crate::AgentRunTimelineEntry::Assistant { sequence, .. } => *sequence,
        crate::AgentRunTimelineEntry::Tool { activity } => activity.sequence,
    });
    Ok(crate::AgentRunLogPage {
        entries,
        next_after,
        has_more,
    })
}

fn read_agent_run_timeline(
    connection: &Connection,
    id: crate::AgentRunId,
    after: u64,
    limit: i64,
) -> Result<Vec<crate::AgentRunTimelineEntry>, RuntimeError> {
    let after = i64::try_from(after)
        .map_err(|_| RuntimeError::InvalidOption("invalid delegated log cursor".into()))?;
    let mut statement = connection.prepare(
        "SELECT sequence, kind, content_json, created_at FROM agent_run_events
         WHERE run_id = ?1 AND sequence > ?2 AND kind IN ('assistant', 'tool') ORDER BY sequence LIMIT ?3",
    )?;
    statement
        .query_map(params![id.to_string(), after, limit], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .map(|row| {
            let (sequence, kind, content, created_at) = row?;
            let sequence = sqlite_nonnegative(sequence, "agent run event sequence")?;
            let value: serde_json::Value = decode_stored_json(&content)?;
            match kind.as_str() {
                "assistant" => Ok(crate::AgentRunTimelineEntry::Assistant {
                    sequence,
                    text: value["text"].as_str().unwrap_or_default().to_owned(),
                    created_at,
                }),
                "tool" => Ok(crate::AgentRunTimelineEntry::Tool {
                    activity: crate::AgentRunActivity {
                        sequence,
                        tool: value["tool"].as_str().unwrap_or("unknown").into(),
                        arguments: value["arguments"].clone(),
                        output: value["output"].clone(),
                        is_error: value["is_error"].as_bool().unwrap_or(true),
                        permission_audit: value
                            .get("permission_audit")
                            .filter(|audit| !audit.is_null())
                            .cloned()
                            .map(serde_json::from_value)
                            .transpose()?,
                        created_at,
                    },
                }),
                _ => unreachable!("timeline query filters supported event kinds"),
            }
        })
        .collect()
}

fn bounded_agent_activity_value(value: &serde_json::Value) -> serde_json::Value {
    const LIMIT: usize = 16 * 1024;
    let encoded = serde_json::to_vec(value).unwrap_or_default();
    if encoded.len() <= LIMIT {
        value.clone()
    } else {
        json!({"summary": "delegated tool data exceeded 16 KiB", "bytes": encoded.len(), "truncated": true})
    }
}

/// Bash output has already been bounded by the shell runtime and backs the
/// expanded terminal view, so retain it when the live activity becomes
/// durable. Other delegated tool payloads keep the smaller generic limit.
fn bounded_agent_activity_output(tool: &str, value: &serde_json::Value) -> serde_json::Value {
    const BASH_LIMIT: usize = 5 * 1024 * 1024;
    if tool == "bash" {
        let encoded = serde_json::to_vec(value).unwrap_or_default();
        if encoded.len() <= BASH_LIMIT {
            return value.clone();
        }
    }
    bounded_agent_activity_value(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_bash_output_is_retained_beyond_the_generic_tool_limit() {
        let result = json!({"output": "x".repeat(32 * 1024), "exit_code": 0});

        assert_eq!(bounded_agent_activity_output("bash", &result), result);
        assert!(bounded_agent_activity_output("read", &result)["truncated"] == true);
    }
}

const fn agent_run_status_name(status: crate::AgentRunStatus) -> &'static str {
    match status {
        crate::AgentRunStatus::Queued => "queued",
        crate::AgentRunStatus::Running => "running",
        crate::AgentRunStatus::Completed => "completed",
        crate::AgentRunStatus::Failed => "failed",
        crate::AgentRunStatus::Cancelled => "cancelled",
        crate::AgentRunStatus::Interrupted => "interrupted",
    }
}

fn parse_agent_run_status(status: &str) -> Result<crate::AgentRunStatus, RuntimeError> {
    match status {
        "queued" => Ok(crate::AgentRunStatus::Queued),
        "running" => Ok(crate::AgentRunStatus::Running),
        "completed" => Ok(crate::AgentRunStatus::Completed),
        "failed" => Ok(crate::AgentRunStatus::Failed),
        "cancelled" => Ok(crate::AgentRunStatus::Cancelled),
        "interrupted" => Ok(crate::AgentRunStatus::Interrupted),
        _ => Err(RuntimeError::InvalidOption(format!(
            "invalid stored agent run status: {status}"
        ))),
    }
}
