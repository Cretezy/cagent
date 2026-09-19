//! Conversation retention inventory and cleanup.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, NaiveDateTime, Utc};
use fs2::FileExt as _;
use rusqlite::{Connection, OpenFlags};

use crate::config::{CleanupAction, ConversationCleanupConfig};
use crate::runtime::{AgentRuntime, SessionHandle};
use crate::{ConversationId, RuntimeError};

/// A conversation that cleanup could not safely inspect or remove.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CleanupIssue {
    pub conversation_id: Option<ConversationId>,
    pub path: PathBuf,
    pub message: String,
}

/// Limits still exceeded after eligible conversations were processed.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CleanupOverage {
    pub bytes: u64,
    pub conversations: usize,
}

/// Structured result of a conversation cleanup pass.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CleanupReport {
    pub removed: Vec<ConversationId>,
    pub removed_bytes: u64,
    pub skipped: Vec<CleanupIssue>,
    pub failures: Vec<CleanupIssue>,
    pub remaining: CleanupOverage,
}

#[derive(Clone, Debug)]
struct InventoryItem {
    id: ConversationId,
    database: PathBuf,
    bytes: u64,
    updated_at: SystemTime,
    empty: bool,
}

impl AgentRuntime {
    /// Applies the effective conversation retention policy.
    ///
    /// The full removal set is selected before any writer lock or filesystem
    /// mutation is attempted. Open conversations therefore remain safe and are
    /// reported as skipped.
    pub async fn cleanup_conversations(&self) -> Result<CleanupReport, RuntimeError> {
        self.cleanup_conversations_protecting(None).await
    }

    pub(crate) async fn cleanup_conversations_protecting(
        &self,
        protected: Option<ConversationId>,
    ) -> Result<CleanupReport, RuntimeError> {
        if !self.persist_conversations {
            return Ok(CleanupReport::default());
        }
        let _cleanup = self.cleanup_gate.lock().await;
        let storage = self.conversation_storage_dir.clone();
        let policy = self.config_snapshot().conversation_cleanup();
        let (inventory, mut report) = tokio::task::spawn_blocking(move || inventory(&storage))
            .await
            .map_err(|_| RuntimeError::RuntimeStopped)??;
        let selectable = inventory
            .iter()
            .filter(|item| Some(item.id) != protected)
            .cloned()
            .collect::<Vec<_>>();
        let selected = select_with_totals(
            &selectable,
            inventory.iter().map(|item| item.bytes).sum::<u64>(),
            inventory.len(),
            policy,
            SystemTime::now(),
        );
        let storage = self.conversation_storage_dir.clone();
        let action = policy.action();
        let outcomes = tokio::task::spawn_blocking(move || {
            selected
                .into_iter()
                .map(|item| remove_item(&storage, item, action))
                .collect::<Vec<_>>()
        })
        .await
        .map_err(|_| RuntimeError::RuntimeStopped)?;

        let mut retained_count = inventory.len();
        let mut retained_bytes = inventory.iter().map(|item| item.bytes).sum::<u64>();
        for outcome in outcomes {
            match outcome {
                RemovalOutcome::Removed(item) => {
                    retained_count = retained_count.saturating_sub(1);
                    retained_bytes = retained_bytes.saturating_sub(item.bytes);
                    report.removed_bytes = report.removed_bytes.saturating_add(item.bytes);
                    report.removed.push(item.id);
                }
                RemovalOutcome::Skipped(issue) => report.skipped.push(issue),
                RemovalOutcome::Failed(issue) => report.failures.push(issue),
            }
        }
        if !report.removed.is_empty() {
            self.global_store
                .remove_conversation_projections(report.removed.clone())
                .await?;
            let root = self.temporary_dir.clone();
            let removed = report.removed.clone();
            let failures = tokio::task::spawn_blocking(move || {
                removed
                    .into_iter()
                    .filter_map(|id| {
                        crate::scratchpad::remove_conversation(&root, id)
                            .err()
                            .map(|error| (id, error))
                    })
                    .collect::<Vec<_>>()
            })
            .await
            .map_err(|_| RuntimeError::RuntimeStopped)?;
            for (id, error) in failures {
                tracing::warn!(%id, %error, "failed to remove cleaned conversation scratchpad");
                report.failures.push(CleanupIssue {
                    conversation_id: Some(id),
                    path: self.temporary_dir.clone(),
                    message: format!(
                        "conversation removed, but scratchpad cleanup failed: {error}"
                    ),
                });
            }
        }
        report.remaining = overage(retained_bytes, retained_count, policy);
        Ok(report)
    }
}

impl SessionHandle {
    /// Runs the same cleanup policy used by automatic runtime maintenance.
    pub async fn cleanup_conversations(&self) -> Result<CleanupReport, RuntimeError> {
        self.takeover_runtime.cleanup_conversations().await
    }
}

fn inventory(storage: &Path) -> Result<(Vec<InventoryItem>, CleanupReport), RuntimeError> {
    let conversations = storage.join("conversations");
    let mut items = Vec::new();
    let mut report = CleanupReport::default();
    for entry in std::fs::read_dir(&conversations)? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                report.skipped.push(CleanupIssue {
                    conversation_id: None,
                    path: conversations.clone(),
                    message: error.to_string(),
                });
                continue;
            }
        };
        let database = entry.path();
        if database.extension().and_then(|value| value.to_str()) != Some("db") {
            continue;
        }
        match inspect_database(&database) {
            Ok(item) => items.push(item),
            Err(issue) => {
                report.skipped.push(issue);
            }
        }
    }
    Ok((items, report))
}

fn inspect_database(database: &Path) -> Result<InventoryItem, CleanupIssue> {
    let id = database
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| issue(None, database, "invalid conversation database filename"))?
        .parse::<ConversationId>()
        .map_err(|error| issue(None, database, error.to_string()))?;
    let connection = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| issue(Some(id), database, error.to_string()))?;
    connection
        .pragma_update(None, "query_only", "ON")
        .map_err(|error| issue(Some(id), database, error.to_string()))?;
    let (stored_id, updated_at, title, message_count) = connection
        .query_row(
            "SELECT id, updated_at, COALESCE(title, ''),
                    (SELECT COUNT(*) FROM nodes WHERE kind IN ('user_message', 'assistant_message'))
             FROM conversations LIMIT 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .map_err(|error| issue(Some(id), database, error.to_string()))?;
    if stored_id != id.to_string() {
        return Err(issue(
            Some(id),
            database,
            "filename and stored conversation ID differ",
        ));
    }
    let updated_at = parse_timestamp(&updated_at)
        .ok_or_else(|| issue(Some(id), database, "invalid conversation updated_at"))?;
    let files = database_group(database);
    let bytes = files
        .iter()
        .try_fold(0_u64, |total, path| {
            std::fs::metadata(path).map(|metadata| total.saturating_add(metadata.len()))
        })
        .map_err(|error| issue(Some(id), database, error.to_string()))?;
    Ok(InventoryItem {
        id,
        database: database.to_path_buf(),
        bytes,
        updated_at,
        empty: message_count == 0 && title.trim().is_empty(),
    })
}

fn parse_timestamp(value: &str) -> Option<SystemTime> {
    if let Ok(milliseconds) = value.parse::<u64>() {
        return SystemTime::UNIX_EPOCH.checked_add(std::time::Duration::from_millis(milliseconds));
    }
    if let Ok(value) = DateTime::parse_from_rfc3339(value) {
        return Some(value.with_timezone(&Utc).into());
    }
    NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|value| DateTime::<Utc>::from_naive_utc_and_offset(value, Utc).into())
}

fn database_group(database: &Path) -> Vec<PathBuf> {
    let base = database.as_os_str().to_string_lossy();
    [
        database.to_path_buf(),
        PathBuf::from(format!("{base}-wal")),
        PathBuf::from(format!("{base}-shm")),
        PathBuf::from(format!("{base}-journal")),
    ]
    .into_iter()
    .filter(|path| path.exists())
    .collect()
}

#[cfg(test)]
fn select(
    items: &[InventoryItem],
    policy: ConversationCleanupConfig,
    now: SystemTime,
) -> Vec<InventoryItem> {
    select_with_totals(
        items,
        items.iter().map(|item| item.bytes).sum(),
        items.len(),
        policy,
        now,
    )
}

fn select_with_totals(
    items: &[InventoryItem],
    mut retained_bytes: u64,
    mut retained_count: usize,
    policy: ConversationCleanupConfig,
    now: SystemTime,
) -> Vec<InventoryItem> {
    let mut oldest = items.to_vec();
    oldest.sort_by(|left, right| {
        left.updated_at
            .cmp(&right.updated_at)
            .then_with(|| left.id.to_string().cmp(&right.id.to_string()))
    });
    let cutoff = policy.max_age().and_then(|age| now.checked_sub(age));
    let mut selected = Vec::new();
    for item in &oldest {
        if item.empty {
            retained_bytes = retained_bytes.saturating_sub(item.bytes);
            retained_count = retained_count.saturating_sub(1);
            selected.push(item.clone());
        }
    }
    for item in &oldest {
        if !item.empty && cutoff.is_some_and(|cutoff| item.updated_at < cutoff) {
            retained_bytes = retained_bytes.saturating_sub(item.bytes);
            retained_count = retained_count.saturating_sub(1);
            selected.push(item.clone());
        }
    }
    let aged = selected.iter().map(|item| item.id).collect::<HashSet<_>>();
    for item in oldest {
        if aged.contains(&item.id) {
            continue;
        }
        let size_exceeded = policy
            .max_size()
            .is_some_and(|limit| retained_bytes > limit);
        let count_exceeded = policy
            .max_conversations()
            .is_some_and(|limit| retained_count > limit);
        if !size_exceeded && !count_exceeded {
            break;
        }
        retained_bytes = retained_bytes.saturating_sub(item.bytes);
        retained_count = retained_count.saturating_sub(1);
        selected.push(item);
    }
    selected
}

enum RemovalOutcome {
    Removed(InventoryItem),
    Skipped(CleanupIssue),
    Failed(CleanupIssue),
}

fn remove_item(storage: &Path, item: InventoryItem, action: CleanupAction) -> RemovalOutcome {
    let open_lock_path = storage
        .join("conversation-open-locks")
        .join(format!("{}.lock", item.id));
    let open_lock = match std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&open_lock_path)
    {
        Ok(lock) => lock,
        Err(error) => {
            return RemovalOutcome::Failed(issue(
                Some(item.id),
                &open_lock_path,
                error.to_string(),
            ));
        }
    };
    if let Err(error) = open_lock.try_lock_exclusive() {
        return RemovalOutcome::Skipped(issue(
            Some(item.id),
            &item.database,
            format!("conversation is open: {error}"),
        ));
    }
    let lock_path = storage
        .join("conversation-writer-locks")
        .join(format!("{}.lock", item.id));
    let lock = match std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
    {
        Ok(lock) => lock,
        Err(error) => {
            return RemovalOutcome::Failed(issue(Some(item.id), &lock_path, error.to_string()));
        }
    };
    if let Err(error) = lock.try_lock_exclusive() {
        return RemovalOutcome::Skipped(issue(
            Some(item.id),
            &item.database,
            format!("writer lock held: {error}"),
        ));
    }
    let checkpoint = Connection::open(&item.database)
        .and_then(|connection| connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)"));
    if let Err(error) = checkpoint {
        return RemovalOutcome::Failed(issue(
            Some(item.id),
            &item.database,
            format!("checkpoint failed: {error}"),
        ));
    }
    let mut files = database_group(&item.database);
    files.sort_by_key(|path| path == &item.database);
    for path in &files {
        let result = match action {
            CleanupAction::Trash => trash::delete(path).map_err(|error| error.to_string()),
            CleanupAction::Delete => std::fs::remove_file(path).map_err(|error| error.to_string()),
        };
        if let Err(error) = result {
            return RemovalOutcome::Failed(issue(Some(item.id), path, error));
        }
    }
    RemovalOutcome::Removed(item)
}

fn overage(bytes: u64, conversations: usize, policy: ConversationCleanupConfig) -> CleanupOverage {
    CleanupOverage {
        bytes: policy
            .max_size()
            .map_or(0, |limit| bytes.saturating_sub(limit)),
        conversations: policy
            .max_conversations()
            .map_or(0, |limit| conversations.saturating_sub(limit)),
    }
}

fn issue(id: Option<ConversationId>, path: &Path, message: impl Into<String>) -> CleanupIssue {
    CleanupIssue {
        conversation_id: id,
        path: path.to_path_buf(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn item(id: ConversationId, bytes: u64, age: Duration) -> InventoryItem {
        InventoryItem {
            id,
            database: PathBuf::from(format!("{id}.db")),
            bytes,
            updated_at: SystemTime::UNIX_EPOCH + Duration::from_secs(10_000) - age,
            empty: false,
        }
    }

    fn policy(
        max_size: Option<u64>,
        max_age: Option<Duration>,
        max_count: Option<usize>,
    ) -> ConversationCleanupConfig {
        ConversationCleanupConfig::for_test(max_size, max_age, max_count, CleanupAction::Delete)
    }

    #[test]
    fn selection_applies_age_then_oldest_size_and_count_limits() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10_000);
        let oldest = item(ConversationId::new(), 4, Duration::from_secs(300));
        let middle = item(ConversationId::new(), 4, Duration::from_secs(200));
        let newest = item(ConversationId::new(), 4, Duration::from_secs(100));
        let items = vec![newest.clone(), oldest.clone(), middle.clone()];

        assert_eq!(
            select(
                &items,
                policy(None, Some(Duration::from_secs(250)), None),
                now
            )
            .iter()
            .map(|item| item.id)
            .collect::<Vec<_>>(),
            vec![oldest.id]
        );
        assert_eq!(
            select(&items, policy(Some(8), None, None), now)
                .iter()
                .map(|item| item.id)
                .collect::<Vec<_>>(),
            vec![oldest.id]
        );
        assert_eq!(
            select(&items, policy(None, None, Some(2)), now)
                .iter()
                .map(|item| item.id)
                .collect::<Vec<_>>(),
            vec![oldest.id]
        );
        assert_eq!(
            select(&items, policy(Some(7), None, Some(2)), now)
                .iter()
                .map(|item| item.id)
                .collect::<Vec<_>>(),
            vec![oldest.id, middle.id]
        );
    }

    #[test]
    fn timestamps_accept_canonical_unix_milliseconds_and_legacy_text() {
        assert_eq!(
            parse_timestamp("1786821956420"),
            Some(SystemTime::UNIX_EPOCH + Duration::from_millis(1_786_821_956_420)),
        );
        assert!(parse_timestamp("2026-08-15 19:32:03").is_some());
        assert!(parse_timestamp("2026-08-15T19:32:03Z").is_some());
        assert!(parse_timestamp("invalid").is_none());
    }

    #[test]
    fn empty_conversations_are_always_selected_before_retention_limits() {
        let mut empty = item(ConversationId::new(), 5, Duration::from_secs(1));
        empty.empty = true;
        let retained = item(ConversationId::new(), 5, Duration::from_secs(2));
        let selected = select(
            &[retained, empty.clone()],
            policy(None, None, None),
            SystemTime::UNIX_EPOCH + Duration::from_secs(10_000),
        );
        assert_eq!(
            selected.iter().map(|item| item.id).collect::<Vec<_>>(),
            vec![empty.id]
        );
    }

    #[test]
    fn selection_uses_conversation_id_as_equal_timestamp_tiebreaker() {
        let first = item(ConversationId::new(), 1, Duration::from_secs(1));
        let second = item(ConversationId::new(), 1, Duration::from_secs(1));
        let mut expected = [first.id, second.id];
        expected.sort_by_key(ToString::to_string);
        let selected = select(
            &[second, first],
            policy(None, None, Some(0)),
            SystemTime::now(),
        );
        assert_eq!(
            selected.iter().map(|item| item.id).collect::<Vec<_>>(),
            expected
        );
    }

    #[test]
    fn protected_conversation_counts_toward_limit_but_is_not_selected() {
        let protected = item(ConversationId::new(), 1, Duration::from_secs(3));
        let next = item(ConversationId::new(), 1, Duration::from_secs(2));
        let newest = item(ConversationId::new(), 1, Duration::from_secs(1));
        let selected = select_with_totals(
            &[next.clone(), newest],
            3,
            3,
            policy(None, None, Some(2)),
            SystemTime::UNIX_EPOCH + Duration::from_secs(10_000),
        );
        assert_eq!(
            selected.iter().map(|item| item.id).collect::<Vec<_>>(),
            vec![next.id]
        );
        assert_ne!(selected[0].id, protected.id);
    }

    fn create_database(storage: &Path, id: ConversationId) -> PathBuf {
        let directory = storage.join("conversations");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::create_dir_all(storage.join("conversation-writer-locks")).unwrap();
        std::fs::create_dir_all(storage.join("conversation-open-locks")).unwrap();
        let path = directory.join(format!("{id}.db"));
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch("CREATE TABLE conversations(id TEXT, updated_at TEXT, title TEXT); CREATE TABLE nodes(kind TEXT); INSERT INTO conversations VALUES ('placeholder', '1786821956420', NULL); INSERT INTO nodes VALUES ('user_message');").unwrap();
        connection
            .execute("UPDATE conversations SET id = ?1", [id.to_string()])
            .unwrap();
        path
    }

    #[test]
    fn named_conversation_without_messages_is_not_empty() {
        let temporary = tempfile::TempDir::new().unwrap();
        let id = ConversationId::new();
        let database = create_database(temporary.path(), id);
        let connection = Connection::open(&database).unwrap();
        connection
            .execute_batch("DELETE FROM nodes; UPDATE conversations SET title = 'Named';")
            .unwrap();
        drop(connection);
        assert!(!inspect_database(&database).unwrap().empty);

        let connection = Connection::open(&database).unwrap();
        connection
            .execute("UPDATE conversations SET title = NULL", [])
            .unwrap();
        drop(connection);
        assert!(inspect_database(&database).unwrap().empty);
    }

    #[test]
    fn locked_conversation_is_skipped_and_sidecar_is_deleted_with_group() {
        let temporary = tempfile::TempDir::new().unwrap();
        let id = ConversationId::new();
        let database = create_database(temporary.path(), id);
        let locked_item = inspect_database(&database).unwrap();
        let journal = PathBuf::from(format!("{}-journal", database.display()));
        std::fs::write(&journal, b"sidecar").unwrap();
        let lock_path = temporary
            .path()
            .join("conversation-writer-locks")
            .join(format!("{id}.lock"));
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)
            .unwrap();
        lock.try_lock_exclusive().unwrap();
        assert!(matches!(
            remove_item(temporary.path(), locked_item.clone(), CleanupAction::Delete),
            RemovalOutcome::Skipped(_)
        ));
        drop(lock);

        let open_lock_path = temporary
            .path()
            .join("conversation-open-locks")
            .join(format!("{id}.lock"));
        let open_lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(open_lock_path)
            .unwrap();
        open_lock.lock_shared().unwrap();
        assert!(matches!(
            remove_item(temporary.path(), locked_item.clone(), CleanupAction::Delete),
            RemovalOutcome::Skipped(_)
        ));
        drop(open_lock);

        assert!(matches!(
            remove_item(temporary.path(), locked_item, CleanupAction::Delete),
            RemovalOutcome::Removed(_)
        ));
        assert!(!database.exists());
        assert!(!journal.exists());
    }

    #[test]
    fn corrupt_files_and_delete_failures_are_reported_without_removing_canonical_database() {
        let temporary = tempfile::TempDir::new().unwrap();
        let corrupt = temporary
            .path()
            .join("conversations")
            .join(format!("{}.db", ConversationId::new()));
        std::fs::create_dir_all(corrupt.parent().unwrap()).unwrap();
        std::fs::write(&corrupt, b"not sqlite").unwrap();
        let (items, report) = inventory(temporary.path()).unwrap();
        assert!(items.is_empty());
        assert_eq!(report.skipped.len(), 1);

        let id = ConversationId::new();
        let database = create_database(temporary.path(), id);
        let item = inspect_database(&database).unwrap();
        let journal = PathBuf::from(format!("{}-journal", database.display()));
        std::fs::create_dir(&journal).unwrap();
        assert!(matches!(
            remove_item(temporary.path(), item, CleanupAction::Delete),
            RemovalOutcome::Failed(_)
        ));
        assert!(database.exists());
    }

    #[test]
    fn invalid_database_does_not_consume_a_count_limit_slot() {
        let temporary = tempfile::TempDir::new().unwrap();
        for _ in 0..5 {
            create_database(temporary.path(), ConversationId::new());
        }
        let corrupt = temporary
            .path()
            .join("conversations")
            .join(format!("{}.db", ConversationId::new()));
        std::fs::write(corrupt, b"not sqlite").unwrap();

        let (items, report) = inventory(temporary.path()).unwrap();
        assert_eq!(items.len(), 5);
        assert_eq!(report.skipped.len(), 1);
        let policy =
            ConversationCleanupConfig::for_test(None, None, Some(5), CleanupAction::Delete);
        assert!(select(&items, policy, SystemTime::now()).is_empty());
    }

    #[tokio::test]
    async fn automatic_count_cleanup_runs_after_open_without_blocking_it() {
        let temporary = tempfile::TempDir::new().unwrap();
        for _ in 0..6 {
            create_database(temporary.path(), ConversationId::new());
        }
        let config = crate::ConfigSnapshot::parse(
            Path::new("cleanup.toml"),
            "version = 1\n[conversation_cleanup]\nmax_size = false\nmax_age = false\nmax_conversations = 5\naction = 'delete'\n",
        ).unwrap();
        let started = std::time::Instant::now();
        let runtime = crate::runtime::AgentRuntime::open(
            crate::runtime::RuntimeOptions::new(temporary.path().to_path_buf())
                .with_config(crate::ConfigStore::in_memory(config)),
        )
        .await
        .unwrap();
        assert!(started.elapsed() < Duration::from_secs(1));
        let maintenance = runtime.subscribe_conversation_maintenance();

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let count = std::fs::read_dir(temporary.path().join("conversations"))
                    .unwrap()
                    .filter_map(Result::ok)
                    .filter(|entry| {
                        entry
                            .path()
                            .extension()
                            .is_some_and(|extension| extension == "db")
                    })
                    .count();
                if count == 5 && *maintenance.borrow() == 2 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("automatic cleanup should complete");
        assert_eq!(
            *maintenance.borrow(),
            2,
            "indexing and cleanup both completed"
        );
    }

    #[test]
    fn native_trash_and_permanent_delete_surface_filesystem_failures() {
        let missing = PathBuf::from("/path/that/does/not/exist/cagent-cleanup.db");
        assert!(trash::delete(&missing).is_err());
        assert!(std::fs::remove_file(&missing).is_err());
    }
}
