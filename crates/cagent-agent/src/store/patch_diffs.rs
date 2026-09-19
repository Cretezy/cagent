//! Durable successful mutation output, independent of the current files and branch tip.

#[allow(clippy::wildcard_imports)]
use super::*;
use crate::tools::{ConversationDiff, SemanticDiff};

const MAX_DIFF_BYTES: i64 = 5 * 1024 * 1024;
const MAX_DIFF_RECORDS: i64 = 10_000;

pub(super) fn record_patch_diff(
    transaction: &Transaction<'_>,
    conversation_id: ConversationId,
    tool: &str,
    output: &serde_json::Value,
    is_error: bool,
) -> Result<(), RuntimeError> {
    if tool != "apply_patch" || is_error {
        return Ok(());
    }
    if let Some(diff) = successful_diff(output) {
        insert_diff(transaction, &conversation_id.to_string(), Some(&diff))?;
    }
    Ok(())
}

fn successful_diff(output: &serde_json::Value) -> Option<SemanticDiff> {
    // A legacy compact result can still contain a valid-looking preview diff.
    // Only its blob (when retained) is an authoritative complete result.
    if output["truncated"].as_bool() == Some(true)
        || output["summary"].as_str() == Some("large tool output retained for lazy loading")
    {
        return None;
    }
    serde_json::from_value(output.get("diff")?.clone()).ok()
}

fn insert_diff(
    transaction: &Transaction<'_>,
    conversation_id: &str,
    diff: Option<&SemanticDiff>,
) -> Result<(), RuntimeError> {
    transaction.execute(
        "INSERT INTO conversation_patch_diffs(conversation_id, diff_json) VALUES (?1, ?2)",
        params![
            conversation_id,
            diff.map(serde_json::to_string).transpose()?
        ],
    )?;
    Ok(())
}

/// Runs once, inside the schema migration transaction. Never reconstruct from
/// arguments: they describe intent, not what successfully reached the disk.
pub(super) fn migrate_patch_diffs(transaction: &Transaction<'_>) -> Result<(), RuntimeError> {
    let exists: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = 'conversation_patch_diffs')",
        [],
        |row| row.get(0),
    )?;
    if exists {
        return Ok(());
    }
    transaction.execute_batch(include_str!(
        "../../migrations/conversation/0012_patch_diffs.sql"
    ))?;
    let mut statement = transaction.prepare(
        "SELECT conversation_id, content_json FROM (
           SELECT conversation_id, content_json, created_at, sequence, 0 AS source
           FROM nodes WHERE kind = 'tool_result'
             AND json_extract(content_json, '$.name') = 'apply_patch'
           UNION ALL
           SELECT r.conversation_id, e.content_json, e.created_at, e.sequence, 1 AS source
           FROM agent_run_events e JOIN agent_runs r ON r.id = e.run_id
           WHERE e.kind = 'tool' AND json_extract(e.content_json, '$.tool') = 'apply_patch'
         ) ORDER BY created_at, source, sequence",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let conversation_id: String = row.get(0)?;
        let content: serde_json::Value = decode_stored_json(row.get::<_, String>(1)?)?;
        if content["is_error"].as_bool() != Some(false) {
            continue;
        }
        let output = if let Some(blob_id) = content["output_blob_id"].as_str() {
            let bytes: Option<Vec<u8>> = transaction
                .query_row(
                    "SELECT bytes FROM blobs WHERE id = ?1 AND codec = 'json'",
                    [blob_id],
                    |row| row.get(0),
                )
                .optional()?;
            bytes.and_then(|bytes| serde_json::from_slice(&bytes).ok())
        } else {
            Some(content["output"].clone())
        };
        let diff = output.as_ref().and_then(successful_diff);
        insert_diff(transaction, &conversation_id, diff.as_ref())?;
    }
    Ok(())
}

pub(super) fn load_conversation_diff(
    connection: &mut Connection,
    conversation_id: ConversationId,
) -> Result<ConversationDiff, RuntimeError> {
    // Keep the size check and hydration in one snapshot, including for observers
    // reading while another process is recording tools or clearing tracking.
    let transaction = connection.transaction()?;
    let id = conversation_id.to_string();
    let (bytes, records): (i64, i64) = transaction.query_row(
        "SELECT COALESCE(SUM(length(CAST(diff_json AS BLOB))), 0), COUNT(*)
         FROM conversation_patch_diffs WHERE conversation_id = ?1",
        [&id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if bytes > MAX_DIFF_BYTES || records > MAX_DIFF_RECORDS {
        return Err(RuntimeError::InvalidOption(
            "conversation diff is too large to display (limit: 5 MiB or 10,000 recorded patches); use /diff clear to reset tracking".into(),
        ));
    }
    let mut result = ConversationDiff::default();
    let mut statement = transaction.prepare(
        "SELECT diff_json FROM conversation_patch_diffs WHERE conversation_id = ?1 ORDER BY sequence",
    )?;
    let mut rows = statement.query([&id])?;
    while let Some(row) = rows.next()? {
        if let Some(encoded) = row.get::<_, Option<String>>(0)? {
            result.diff.merge(decode_stored_json(&encoded)?);
        } else {
            result.incomplete_history = true;
        }
    }
    Ok(result)
}

pub(super) fn clear_conversation_diff(
    connection: &Connection,
    conversation_id: ConversationId,
) -> Result<(), RuntimeError> {
    connection.execute(
        "DELETE FROM conversation_patch_diffs WHERE conversation_id = ?1",
        [conversation_id.to_string()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
