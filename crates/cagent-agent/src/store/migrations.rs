#[allow(clippy::wildcard_imports)]
use super::*;

/// Allow a second Cagent process time to finish its short SQLite transaction.
///
/// SQLite permits only one writer across processes. The store task serializes
/// writes inside a runtime, but a concurrently starting frontend or `exec`
/// invocation can briefly contend for the same database file.
const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const GLOBAL_BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const CURRENT_CONVERSATION_SCHEMA_VERSION: i64 = 13;

pub(super) fn configure_conversation(connection: &Connection) -> Result<(), RuntimeError> {
    // This setting is per connection, so it must be installed before
    // `journal_mode`: changing the journal mode itself can need a write lock.
    connection.busy_timeout(BUSY_TIMEOUT)?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}

pub(super) fn configure_global(connection: &Connection) -> Result<(), RuntimeError> {
    connection.busy_timeout(GLOBAL_BUSY_TIMEOUT)?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}

pub(super) fn migrate_conversation(connection: &mut Connection) -> Result<(), RuntimeError> {
    let has_migration_table = connection.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM sqlite_master
             WHERE type = 'table' AND name = 'schema_migrations'
         )",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    if has_migration_table {
        let current = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version = ?1)",
            [CURRENT_CONVERSATION_SCHEMA_VERSION],
            |row| row.get::<_, bool>(0),
        )?;
        if current {
            tracing::trace!(
                version = CURRENT_CONVERSATION_SCHEMA_VERSION,
                "conversation schema already current"
            );
            return Ok(());
        }
        let previous = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version IN (7, 8, 9, 10, 11, 12))",
            [],
            |row| row.get::<_, bool>(0),
        )?;
        if !previous {
            return Err(RuntimeError::InvalidOption(format!(
                "unsupported conversation schema; expected node-history schema version {CURRENT_CONVERSATION_SCHEMA_VERSION}"
            )));
        }
    }
    let transaction = connection.transaction()?;
    transaction.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
             version INTEGER PRIMARY KEY NOT NULL,
             applied_at TEXT NOT NULL
         );",
    )?;
    // Recreate this trigger while upgrading: development databases may carry
    // an earlier definition that did not permit the assistant-to-plan
    // lifecycle transition. The current-version gate above makes this a
    // one-time repair instead of resume-time work.
    transaction.execute_batch("DROP TRIGGER IF EXISTS nodes_preserve_identity_and_ancestry;")?;
    transaction.execute_batch(CONVERSATION_SCHEMA)?;
    // Opaque provider state must never enter node content or transcript events.
    transaction.execute_batch(
        "CREATE TABLE IF NOT EXISTS assistant_reasoning (
             node_id TEXT PRIMARY KEY NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
             reasoning_json TEXT NOT NULL CHECK(json_valid(reasoning_json) AND json_type(reasoning_json) = 'array')
         );",
    )?;
    let has_title_generation_state = transaction
        .prepare("PRAGMA table_info(conversations)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .any(|name| name == "title_generation_state");
    if !has_title_generation_state {
        transaction.execute(
            "ALTER TABLE conversations ADD COLUMN title_generation_state TEXT NOT NULL DEFAULT 'complete'",
            [],
        )?;
    }
    let conversation_columns = transaction
        .prepare("PRAGMA table_info(conversations)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    if !conversation_columns
        .iter()
        .any(|name| name == "project_dir")
    {
        transaction.execute("ALTER TABLE conversations ADD COLUMN project_dir TEXT", [])?;
        transaction.execute("UPDATE conversations SET project_dir = workspace", [])?;
    }
    if !conversation_columns.iter().any(|name| name == "cwd") {
        transaction.execute("ALTER TABLE conversations ADD COLUMN cwd TEXT", [])?;
        transaction.execute("UPDATE conversations SET cwd = workspace", [])?;
    }
    if !conversation_columns
        .iter()
        .any(|name| name == "worktree_json")
    {
        transaction.execute(
            "ALTER TABLE conversations ADD COLUMN worktree_json TEXT",
            [],
        )?;
    }
    if !conversation_columns.iter().any(|name| name == "archived") {
        transaction.execute(
            "ALTER TABLE conversations ADD COLUMN archived INTEGER NOT NULL DEFAULT 0 CHECK(archived IN (0, 1))",
            [],
        )?;
    }
    if !conversation_columns.iter().any(|name| name == "favourite") {
        transaction.execute(
            "ALTER TABLE conversations ADD COLUMN favourite INTEGER NOT NULL DEFAULT 0 CHECK(favourite IN (0, 1))",
            [],
        )?;
    }
    let has_slash_command_user_text = transaction
        .prepare("PRAGMA table_info(composer_history)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .any(|name| name == "slash_command_user_text");
    if !has_slash_command_user_text {
        transaction.execute(
            "ALTER TABLE composer_history ADD COLUMN slash_command_user_text TEXT",
            [],
        )?;
    }
    let has_publish_to_global = transaction
        .prepare("PRAGMA table_info(composer_history)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .any(|name| name == "publish_to_global");
    if !has_publish_to_global {
        transaction.execute(
            "ALTER TABLE composer_history ADD COLUMN publish_to_global INTEGER NOT NULL DEFAULT 1 CHECK(publish_to_global IN (0, 1))",
            [],
        )?;
    }
    let composer_columns = transaction
        .prepare("PRAGMA table_info(composer_history)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    if !composer_columns.iter().any(|name| name == "images_json") {
        transaction.execute(
            "ALTER TABLE composer_history ADD COLUMN images_json TEXT NOT NULL DEFAULT '[]'",
            [],
        )?;
    }
    if !composer_columns
        .iter()
        .any(|name| name == "image_chips_json")
    {
        transaction.execute(
            "ALTER TABLE composer_history ADD COLUMN image_chips_json TEXT NOT NULL DEFAULT '[]'",
            [],
        )?;
    }
    transaction.execute(
        "INSERT OR IGNORE INTO schema_migrations(version, applied_at) VALUES (2, datetime('now'))",
        [],
    )?;
    transaction.execute_batch(IMAGE_BLOB_HASHES_MIGRATION)?;
    transaction.execute(
        "INSERT OR IGNORE INTO schema_migrations(version, applied_at) VALUES (3, datetime('now'))",
        [],
    )?;
    transaction.execute(
        "INSERT OR IGNORE INTO schema_migrations(version, applied_at) VALUES (4, datetime('now'))",
        [],
    )?;
    let node_columns = transaction
        .prepare("PRAGMA table_info(nodes)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    if !node_columns
        .iter()
        .any(|name| name == "context_window_tokens")
    {
        transaction.execute(
            "ALTER TABLE nodes ADD COLUMN context_window_tokens INTEGER",
            [],
        )?;
    }
    let agent_run_columns = transaction
        .prepare("PRAGMA table_info(agent_runs)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    if !agent_run_columns.iter().any(|name| name == "effort") {
        transaction.execute("ALTER TABLE agent_runs ADD COLUMN effort TEXT", [])?;
    }
    let terminal_columns = transaction
        .prepare("PRAGMA table_info(background_terminals)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    if !terminal_columns
        .iter()
        .any(|name| name == "owner_agent_run_id")
    {
        transaction.execute(
            "ALTER TABLE background_terminals ADD COLUMN owner_agent_run_id TEXT REFERENCES agent_runs(id)",
            [],
        )?;
    }
    if !terminal_columns.iter().any(|name| name == "read_safe") {
        transaction.execute(
            "ALTER TABLE background_terminals ADD COLUMN read_safe INTEGER",
            [],
        )?;
    }
    if !terminal_columns.iter().any(|name| name == "preview_output") {
        transaction.execute(
            "ALTER TABLE background_terminals ADD COLUMN preview_output TEXT NOT NULL DEFAULT ''",
            [],
        )?;
        let retained_ids = {
            let mut statement = transaction
                .prepare("SELECT id FROM background_terminals WHERE length(output) > 0")?;
            statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        for id in retained_ids {
            let output = transaction.query_row(
                "SELECT output FROM background_terminals WHERE id = ?1",
                [&id],
                |row| row.get::<_, Vec<u8>>(0),
            )?;
            let output = String::from_utf8_lossy(&output);
            transaction.execute(
                "UPDATE background_terminals SET preview_output = ?1 WHERE id = ?2",
                params![crate::output_preview(&output), id],
            )?;
        }
    }
    if !terminal_columns.iter().any(|name| name == "detached") {
        transaction.execute(
            "ALTER TABLE background_terminals ADD COLUMN detached INTEGER NOT NULL DEFAULT 0 CHECK(detached IN (0, 1))",
            [],
        )?;
    }
    if !terminal_columns.iter().any(|name| name == "anchor_node_id") {
        transaction.execute(
            "ALTER TABLE background_terminals ADD COLUMN anchor_node_id TEXT REFERENCES nodes(id)",
            [],
        )?;
    }
    if !composer_columns.iter().any(|name| name == "input_kind") {
        transaction.execute(
            "ALTER TABLE composer_history ADD COLUMN input_kind TEXT NOT NULL DEFAULT 'prompt' CHECK(input_kind IN ('prompt', 'bash'))",
            [],
        )?;
    }
    // Older development builds used two transient startup statuses. A row is
    // now either ordinarily queued or durably blocked, so normalize in place.
    transaction.execute(
        "UPDATE queued_messages SET status = 'blocked_by_startup' WHERE status = 'waiting_startup'",
        [],
    )?;
    transaction.execute(
        "UPDATE queued_messages SET status = 'queued' WHERE status = 'released_startup'",
        [],
    )?;
    transaction.execute(
        "INSERT OR IGNORE INTO schema_migrations(version, applied_at) VALUES (1, datetime('now'))",
        [],
    )?;
    transaction.execute(
        "INSERT OR IGNORE INTO schema_migrations(version, applied_at) VALUES (?1, datetime('now'))",
        [CURRENT_CONVERSATION_SCHEMA_VERSION],
    )?;
    super::patch_diffs::migrate_patch_diffs(&transaction)?;
    tracing::trace!(
        version = CURRENT_CONVERSATION_SCHEMA_VERSION,
        "conversation schema migration applied"
    );
    transaction.commit()?;
    Ok(())
}

pub(super) fn migrate_global(connection: &mut Connection) -> Result<(), RuntimeError> {
    let transaction = connection.transaction()?;
    transaction.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
             version INTEGER PRIMARY KEY NOT NULL,
             applied_at TEXT NOT NULL
         );",
    )?;
    transaction.execute_batch(GLOBAL_SCHEMA)?;
    let has_archived = transaction
        .prepare("PRAGMA table_info(conversation_index)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .any(|name| name == "archived");
    if !has_archived {
        transaction.execute(
            "ALTER TABLE conversation_index ADD COLUMN archived INTEGER NOT NULL DEFAULT 0 CHECK(archived IN (0, 1))",
            [],
        )?;
    }
    let has_favourite = transaction
        .prepare("PRAGMA table_info(conversation_index)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .any(|name| name == "favourite");
    if !has_favourite {
        transaction.execute(
            "ALTER TABLE conversation_index ADD COLUMN favourite INTEGER NOT NULL DEFAULT 0 CHECK(favourite IN (0, 1))",
            [],
        )?;
    }
    let composer_columns = transaction
        .prepare("PRAGMA table_info(composer_recent)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    if !composer_columns.iter().any(|name| name == "input_kind") {
        transaction.execute(
            "ALTER TABLE composer_recent ADD COLUMN input_kind TEXT NOT NULL DEFAULT 'prompt' CHECK(input_kind IN ('prompt', 'bash'))",
            [],
        )?;
    }
    transaction.execute(
        "INSERT OR IGNORE INTO schema_migrations(version, applied_at) VALUES (1, datetime('now'))",
        [],
    )?;
    transaction.commit()?;
    Ok(())
}

#[cfg(test)]
pub(super) fn configure(connection: &Connection) -> Result<(), RuntimeError> {
    configure_conversation(connection)
}

#[cfg(test)]
pub(super) fn migrate(connection: &mut Connection) -> Result<(), RuntimeError> {
    migrate_conversation(connection)
}
