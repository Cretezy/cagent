use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

use super::{
    ComposerHistoryRecord, configure_global, migrate_global, sqlite_nonnegative, sqlite_u64,
};
use crate::provider::prompt_cache::{NamedCacheIdentity, NamedCacheResource};
use crate::{
    CachedModelCatalog, ConversationId, ConversationQuery, ConversationSummary, ModelCatalog,
    RuntimeError, SessionUsage, UsageBreakdown, UsageOverview,
};

const GLOBAL_RETRY_BACKOFF: std::time::Duration = std::time::Duration::from_millis(100);
const GLOBAL_RETRY_BACKOFF_CAP: std::time::Duration = std::time::Duration::from_secs(5);
const GLOBAL_RETRY_WINDOW: std::time::Duration = std::time::Duration::from_secs(30);
const MODEL_RECENTS_PER_PROVIDER: usize = 100;

/// Access to disposable cross-conversation state.
///
/// No SQLite connection is retained: every operation opens `global.db`, performs
/// one bounded transaction/query, and closes it again.
#[derive(Clone, Debug)]
pub(crate) struct GlobalStore {
    storage_dir: PathBuf,
    operation_gate: std::sync::Arc<tokio::sync::Mutex<()>>,
    projection_watchers: std::sync::Arc<
        std::sync::Mutex<std::collections::HashMap<PathBuf, tokio_util::sync::CancellationToken>>,
    >,
    #[cfg(test)]
    composer_seed_delay_millis: std::sync::Arc<std::sync::atomic::AtomicU64>,
    #[cfg(test)]
    composer_seed_failure: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl GlobalStore {
    pub(crate) async fn open(storage_dir: &Path) -> Result<Self, RuntimeError> {
        tokio::fs::create_dir_all(storage_dir.join("conversations")).await?;
        tokio::fs::create_dir_all(storage_dir.join("conversation-writer-locks")).await?;
        tokio::fs::create_dir_all(storage_dir.join("conversation-open-locks")).await?;
        let store = Self {
            storage_dir: storage_dir.to_path_buf(),
            operation_gate: std::sync::Arc::new(tokio::sync::Mutex::new(())),
            projection_watchers: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            #[cfg(test)]
            composer_seed_delay_millis: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            #[cfg(test)]
            composer_seed_failure: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        Ok(store)
    }

    async fn with_read_connection<T, F>(&self, fallback: T, operation: F) -> Result<T, RuntimeError>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T, RuntimeError> + Send + 'static,
    {
        let path = self.storage_dir.join("global.db");
        tokio::task::spawn_blocking(move || {
            if !path.exists() {
                return Ok(fallback);
            }
            let connection =
                match Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
                    Ok(connection) => connection,
                    Err(error) if is_corrupt_sqlite_error(&error) => {
                        quarantine_global(&path);
                        return Ok(fallback);
                    }
                    Err(error) if is_best_effort_sqlite_error(&error) => {
                        return Ok(fallback);
                    }
                    Err(error) => return Err(error.into()),
                };
            if let Err(error) = connection.pragma_update(None, "query_only", "ON") {
                return if is_best_effort_sqlite_error(&error) {
                    Ok(fallback)
                } else {
                    Err(error.into())
                };
            }
            match operation(&connection) {
                Err(error) if is_corrupt_error(&error) => {
                    quarantine_global(&path);
                    Ok(fallback)
                }
                Err(error) if is_best_effort_read_error(&error) => Ok(fallback),
                result => result,
            }
        })
        .await
        .map_err(|_| RuntimeError::RuntimeStopped)?
    }

    async fn with_write_connection<T, F>(&self, mut operation: F) -> Result<T, RuntimeError>
    where
        T: Send + 'static,
        F: FnMut(&mut Connection) -> Result<T, RuntimeError> + Send + 'static,
    {
        let _operation = self.operation_gate.lock().await;
        let path = self.storage_dir.join("global.db");
        tokio::task::spawn_blocking(move || {
            let deadline = std::time::Instant::now() + GLOBAL_RETRY_WINDOW;
            let mut backoff = GLOBAL_RETRY_BACKOFF;
            loop {
                let result = (|| {
                    let mut connection = match open_checked_global(&path) {
                        Ok(connection) => connection,
                        Err(error) if is_corrupt_error(&error) => {
                            quarantine_global(&path);
                            tracing::warn!(%error, path = %path.display(), "recreating corrupt global database");
                            open_checked_global(&path)?
                        }
                        Err(error) => return Err(error),
                    };
                    operation(&mut connection)
                })();
                if !is_global_contention(&result) || std::time::Instant::now() >= deadline {
                    return result;
                }
                tracing::debug!(?backoff, "global database write deferred by contention");
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(GLOBAL_RETRY_BACKOFF_CAP);
            }
        })
        .await
        .map_err(|_| RuntimeError::RuntimeStopped)?
    }

    pub(crate) async fn save_model_catalog(
        &self,
        catalog: ModelCatalog,
        fetched_at_millis: u64,
    ) -> Result<(), RuntimeError> {
        let catalog_json = serde_json::to_string(&catalog)?;
        let provider = catalog.provider;
        let version = catalog.version;
        let fetched_at_millis = sqlite_u64(fetched_at_millis, "catalog fetch time")?;
        self.with_write_connection(move |connection| {
            connection.execute(
                "INSERT INTO model_catalog_cache(provider, catalog_json, fetched_at_millis, version)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(provider) DO UPDATE SET
                   catalog_json = excluded.catalog_json,
                   fetched_at_millis = excluded.fetched_at_millis,
                   version = excluded.version",
                params![
                    provider.clone(),
                    catalog_json.clone(),
                    fetched_at_millis,
                    version.clone(),
                ],
            )?;
            Ok(())
        })
        .await
    }

    pub(crate) async fn load_model_catalog(
        &self,
        provider: &str,
    ) -> Result<Option<CachedModelCatalog>, RuntimeError> {
        let provider = provider.to_owned();
        self.with_read_connection(None, move |connection| {
            connection
                .query_row(
                    "SELECT catalog_json, fetched_at_millis FROM model_catalog_cache WHERE provider = ?1",
                    [provider],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                )
                .optional()?
                .map(|(json, fetched)| {
                    Ok(CachedModelCatalog {
                        catalog: serde_json::from_str(&json)?,
                        fetched_at_millis: sqlite_nonnegative(fetched, "catalog fetch time")?,
                    })
                })
                .transpose()
        })
        .await
    }

    pub(crate) async fn invalidate_model_catalog(
        &self,
        provider: &str,
    ) -> Result<(), RuntimeError> {
        let provider = provider.to_owned();
        self.with_write_connection(move |connection| {
            connection.execute(
                "DELETE FROM model_catalog_cache WHERE provider = ?1",
                [provider.clone()],
            )?;
            Ok(())
        })
        .await
    }

    pub(crate) async fn load_prompt_cache_resource(
        &self,
        identity: NamedCacheIdentity,
    ) -> Result<Option<NamedCacheResource>, RuntimeError> {
        self.with_read_connection(None, move |connection| {
            connection.query_row(
                "SELECT prefix_hash, cached_input_boundary, resource_name, cached_token_count, created_at_millis, expires_at_millis
                 FROM prompt_cache_resources
                 WHERE provider = ?1 AND credential_scope_hash = ?2 AND model = ?3 AND conversation_key_hash = ?4",
                params![identity.provider, identity.credential_scope_hash, identity.model, identity.conversation_key_hash],
                |row| {
                    Ok(NamedCacheResource {
                        provider: identity.provider.clone(),
                        credential_scope_hash: identity.credential_scope_hash.clone(),
                        model: identity.model.clone(),
                        conversation_key_hash: identity.conversation_key_hash.clone(),
                        prefix_hash: row.get(0)?,
                        cached_input_boundary: usize::try_from(row.get::<_, i64>(1)?).map_err(|error| rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Integer, Box::new(error)))?,
                        resource_name: row.get(2)?,
                        cached_token_count: sqlite_nonnegative(row.get(3)?, "cached token count").map_err(|error| rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Integer, Box::new(error)))?,
                        created_at_millis: sqlite_nonnegative(row.get(4)?, "cache creation time").map_err(|error| rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Integer, Box::new(error)))?,
                        expires_at_millis: sqlite_nonnegative(row.get(5)?, "cache expiry time").map_err(|error| rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Integer, Box::new(error)))?,
                    })
                },
            ).optional().map_err(RuntimeError::from)
        }).await
    }

    pub(crate) async fn save_prompt_cache_resource(
        &self,
        resource: NamedCacheResource,
    ) -> Result<(), RuntimeError> {
        let boundary = sqlite_u64(
            resource.cached_input_boundary as u64,
            "cached input boundary",
        )?;
        let tokens = sqlite_u64(resource.cached_token_count, "cached token count")?;
        let created = sqlite_u64(resource.created_at_millis, "cache creation time")?;
        let expires = sqlite_u64(resource.expires_at_millis, "cache expiry time")?;
        self.with_write_connection(move |connection| {
            connection.execute(
                "INSERT INTO prompt_cache_resources(provider, credential_scope_hash, model, conversation_key_hash, prefix_hash, cached_input_boundary, resource_name, cached_token_count, created_at_millis, expires_at_millis)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT(provider, credential_scope_hash, model, conversation_key_hash) DO UPDATE SET
                   prefix_hash = excluded.prefix_hash,
                   cached_input_boundary = excluded.cached_input_boundary,
                   resource_name = excluded.resource_name,
                   cached_token_count = excluded.cached_token_count,
                   created_at_millis = excluded.created_at_millis,
                   expires_at_millis = excluded.expires_at_millis",
                params![resource.provider, resource.credential_scope_hash, resource.model, resource.conversation_key_hash, resource.prefix_hash, boundary, resource.resource_name, tokens, created, expires],
            )?;
            Ok(())
        }).await
    }

    pub(crate) async fn delete_prompt_cache_resource(
        &self,
        identity: NamedCacheIdentity,
    ) -> Result<(), RuntimeError> {
        self.with_write_connection(move |connection| {
            connection.execute(
                "DELETE FROM prompt_cache_resources WHERE provider = ?1 AND credential_scope_hash = ?2 AND model = ?3 AND conversation_key_hash = ?4",
                params![identity.provider, identity.credential_scope_hash, identity.model, identity.conversation_key_hash],
            )?;
            Ok(())
        }).await
    }

    pub(crate) async fn conversations(
        &self,
        query: ConversationQuery,
    ) -> Result<Vec<ConversationSummary>, RuntimeError> {
        self.with_read_connection(Vec::new(), move |connection| {
            let workspace = query
                .workspace
                .map(|path| path.to_string_lossy().into_owned());
            let search = query.search.map(|value| normalize_search(&value));
            let like = search
                .as_ref()
                .map(|value| format!("%{}%", escape_like(value)));
            let mut statement = connection.prepare(
                "SELECT i.conversation_id, i.workspace, i.title, i.created_at, i.updated_at,
                        i.active_node_id, i.active_status, i.agent, i.mode, i.provider, i.model,
                        i.active_branch_message_count, i.active_branch_preview, i.archived,
                        i.favourite
                 FROM conversation_index i
                 WHERE (?1 IS NULL OR i.workspace = ?1)
                   AND (?3 OR i.archived = 0)
                   AND (?2 IS NULL OR i.title_search LIKE ?2 ESCAPE '\\'
                        OR EXISTS (
                          SELECT 1 FROM conversation_search_messages m
                          WHERE m.conversation_id = i.conversation_id
                            AND m.normalized_user_text LIKE ?2 ESCAPE '\\'
                        ))
                 ORDER BY i.favourite DESC, i.updated_at DESC, i.conversation_id DESC",
            )?;
            statement
                .query_map(params![workspace, like, query.include_archived], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, Option<String>>(9)?,
                        row.get::<_, Option<String>>(10)?,
                        row.get::<_, i64>(11)?,
                        row.get::<_, String>(12)?,
                        row.get::<_, bool>(13)?,
                        row.get::<_, bool>(14)?,
                    ))
                })?
                .map(|row| {
                    let (
                        id,
                        workspace,
                        title,
                        created_at,
                        updated_at,
                        active_node_id,
                        status,
                        agent,
                        mode,
                        provider,
                        model,
                        count,
                        preview,
                        archived,
                        favourite,
                    ) = row?;
                    Ok(ConversationSummary {
                        id: id.parse().map_err(|error| {
                            RuntimeError::InvalidOption(format!(
                                "invalid indexed conversation id: {error}"
                            ))
                        })?,
                        workspace: workspace.into(),
                        title,
                        created_at,
                        updated_at,
                        active_node_id: active_node_id.parse().map_err(|error| {
                            RuntimeError::InvalidOption(format!("invalid indexed node id: {error}"))
                        })?,
                        status,
                        agent,
                        mode,
                        model: provider
                            .zip(model)
                            .map(|(provider, model)| format!("{provider}/{model}")),
                        message_count: sqlite_nonnegative(count, "conversation message count")?,
                        archived,
                        favourite,
                        preview,
                    })
                })
                .collect()
        })
        .await
    }

    pub(crate) async fn conversation_by_title(
        &self,
        title: String,
    ) -> Result<Option<ConversationId>, RuntimeError> {
        self.with_read_connection(None, move |connection| {
            let title = normalize_search(&title);
            if title.is_empty() {
                return Ok(None);
            }
            let like = format!("%{}%", escape_like(&title));
            let id = connection
                .query_row(
                    "SELECT conversation_id
                     FROM conversation_index
                     WHERE title_search LIKE ?2 ESCAPE '\\'
                     ORDER BY CASE WHEN title_search = ?1 THEN 0 ELSE 1 END,
                              updated_at DESC, conversation_id DESC
                     LIMIT 1",
                    params![title, like],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            id.map(|id| {
                id.parse().map_err(|error| {
                    RuntimeError::InvalidOption(format!("invalid indexed conversation id: {error}"))
                })
            })
            .transpose()
        })
        .await
    }

    pub(crate) async fn delete_conversation(
        &self,
        conversation_id: crate::ConversationId,
    ) -> Result<(), RuntimeError> {
        let id = conversation_id.to_string();
        self.with_write_connection(move |connection| {
            let transaction = connection.transaction()?;
            transaction.execute(
                "DELETE FROM conversation_search_messages WHERE conversation_id = ?1",
                [&id],
            )?;
            transaction.execute(
                "DELETE FROM composer_recent WHERE conversation_id = ?1",
                [&id],
            )?;
            transaction.execute(
                "DELETE FROM conversation_index WHERE conversation_id = ?1",
                [&id],
            )?;
            transaction.commit()?;
            Ok(())
        })
        .await
    }

    pub(crate) async fn load_composer_seed(
        &self,
    ) -> Result<Vec<ComposerHistoryRecord>, RuntimeError> {
        #[cfg(test)]
        {
            let delay = self
                .composer_seed_delay_millis
                .load(std::sync::atomic::Ordering::Acquire);
            if delay > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            }
            if self
                .composer_seed_failure
                .load(std::sync::atomic::Ordering::Acquire)
            {
                return Err(RuntimeError::InvalidOption(
                    "injected composer seed failure".into(),
                ));
            }
        }
        self.with_read_connection(Vec::new(), move |connection| {
            let mut statement = connection.prepare(
                "SELECT entry_id, input_kind, text, attachment_specs_json, created_at FROM composer_recent
                 ORDER BY created_at, entry_id",
            )?;
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                })?
                .map(|row| {
                    let (entry_id, entry_kind, text, specs, created_at) = row?;
                    Ok(ComposerHistoryRecord {
                        entry_id,
                        kind: if entry_kind == "bash" {
                            crate::ComposerInputKind::Bash
                        } else {
                            crate::ComposerInputKind::Prompt
                        },
                        text,
                        attachment_specs: serde_json::from_str(&specs)?,
                        images: Vec::new(),
                        image_chips: Vec::new(),
                        created_at,
                    })
                })
                .collect()
        })
        .await
    }

    #[cfg(test)]
    pub(crate) fn set_composer_seed_test_behavior(&self, delay_millis: u64, fail: bool) {
        self.composer_seed_delay_millis
            .store(delay_millis, std::sync::atomic::Ordering::Release);
        self.composer_seed_failure
            .store(fail, std::sync::atomic::Ordering::Release);
    }

    pub(crate) async fn load_recent_models(
        &self,
        provider: &str,
        limit: usize,
    ) -> Result<Vec<String>, RuntimeError> {
        let provider = provider.to_owned();
        let limit = i64::try_from(limit)
            .map_err(|_| RuntimeError::InvalidOption("recent model limit is too large".into()))?;
        self.with_read_connection(Vec::new(), move |connection| {
            let mut statement = connection.prepare(
                "SELECT model FROM model_recents WHERE provider = ?1 ORDER BY recency DESC, selection_count DESC, model ASC LIMIT ?2",
            )?;
            statement.query_map(params![provider, limit], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(RuntimeError::from)
        }).await
    }

    pub(crate) async fn usage_overview(
        &self,
        conversation_id: ConversationId,
    ) -> Result<UsageOverview, RuntimeError> {
        let conversation_id = conversation_id.to_string();
        self.with_read_connection(UsageOverview::default(), move |connection| {
            let now = unix_millis();
            Ok(UsageOverview {
                current_conversation: query_usage(connection, Some(&conversation_id), None)?,
                rolling_24_hours: query_usage(
                    connection,
                    None,
                    Some(now.saturating_sub(86_400_000)),
                )?,
                rolling_7_days: query_usage(
                    connection,
                    None,
                    Some(now.saturating_sub(7 * 86_400_000)),
                )?,
                rolling_30_days: query_usage(
                    connection,
                    None,
                    Some(now.saturating_sub(30 * 86_400_000)),
                )?,
                total: query_usage(connection, None, None)?,
            })
        })
        .await
    }

    pub(crate) async fn usage_by_project(&self) -> Result<Vec<UsageBreakdown>, RuntimeError> {
        self.usage_breakdown("project", "project").await
    }

    pub(crate) async fn usage_by_model(&self) -> Result<Vec<UsageBreakdown>, RuntimeError> {
        self.usage_breakdown("COALESCE(provider, '') || CASE WHEN provider IS NULL OR provider = '' THEN '' ELSE '/' END || COALESCE(model, 'unknown')", "provider, model").await
    }

    async fn usage_breakdown(
        &self,
        label_expression: &'static str,
        group_expression: &'static str,
    ) -> Result<Vec<UsageBreakdown>, RuntimeError> {
        self.with_read_connection(Vec::new(), move |connection| {
            let reset = usage_reset_watermark(connection)?;
            let sql = format!(
                "SELECT {label_expression}, node_id, input_tokens, non_cached_input_tokens,
                        cache_read_input_tokens, cache_write_input_tokens, output_tokens,
                        reasoning_tokens, total_tokens, input_cost, cache_read_cost,
                        cache_write_cost, output_cost, reasoning_cost, total_cost, currency,
                        pricing_source, pricing_version
                 FROM usage_projection WHERE created_at_millis > ?1
                 ORDER BY {group_expression}, created_at_millis, node_id"
            );
            let mut statement = connection.prepare(&sql)?;
            let rows = statement.query_map([reset], usage_row_with_label)?;
            let mut grouped = std::collections::BTreeMap::<String, SessionUsage>::new();
            for row in rows {
                let (label, usage) = row?;
                grouped.entry(label).or_default().add(&usage);
            }
            let mut rows = grouped
                .into_iter()
                .map(|(label, usage)| UsageBreakdown { label, usage })
                .collect::<Vec<_>>();
            rows.sort_by(|left, right| {
                right
                    .usage
                    .total_tokens
                    .unwrap_or_default()
                    .cmp(&left.usage.total_tokens.unwrap_or_default())
                    .then_with(|| left.label.cmp(&right.label))
            });
            Ok(rows)
        })
        .await
    }

    pub(crate) async fn reset_usage(&self) -> Result<(), RuntimeError> {
        self.with_write_connection(|connection| {
            let transaction = connection.transaction()?;
            transaction.execute(
                "UPDATE usage_state SET reset_at_millis = ?1 WHERE singleton = 1",
                [unix_millis()],
            )?;
            transaction.execute("DELETE FROM usage_projection", [])?;
            transaction.commit()?;
            Ok(())
        })
        .await
    }

    pub(crate) async fn project_conversation(&self, path: PathBuf) -> Result<(), RuntimeError> {
        self.project_conversation_with_mode(path, false).await
    }

    async fn project_conversation_with_mode(
        &self,
        path: PathBuf,
        force_equal_revision: bool,
    ) -> Result<(), RuntimeError> {
        let projection = tokio::task::spawn_blocking(move || read_projection(&path))
            .await
            .map_err(|_| RuntimeError::RuntimeStopped)??;
        self.with_write_connection(move |connection| {
            write_projection(connection, projection.clone(), force_equal_revision)
        })
        .await
    }

    pub(crate) async fn remove_conversation_projections(
        &self,
        conversation_ids: Vec<crate::ConversationId>,
    ) -> Result<(), RuntimeError> {
        let ids = conversation_ids
            .into_iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>();
        self.with_write_connection(move |connection| {
            let transaction = connection.transaction()?;
            for id in &ids {
                transaction.execute(
                    "DELETE FROM conversation_index WHERE conversation_id = ?1",
                    [id],
                )?;
                transaction.execute(
                    "DELETE FROM conversation_search_messages WHERE conversation_id = ?1",
                    [id],
                )?;
                transaction.execute(
                    "DELETE FROM composer_recent WHERE conversation_id = ?1",
                    [id],
                )?;
                transaction.execute(
                    "DELETE FROM usage_projection WHERE conversation_id = ?1",
                    [id],
                )?;
            }
            transaction.commit()?;
            connection.execute_batch("VACUUM")?;
            Ok(())
        })
        .await
    }

    #[allow(dead_code)]
    pub(crate) async fn reconcile(&self) -> Result<(), RuntimeError> {
        let mut entries = tokio::fs::read_dir(self.storage_dir.join("conversations")).await?;
        let mut paths = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().is_some_and(|extension| extension == "db") {
                paths.push(path);
            }
        }
        let present = paths
            .iter()
            .filter_map(|path| path.file_stem())
            .map(|name| name.to_string_lossy().into_owned())
            .collect::<std::collections::HashSet<_>>();
        self.with_write_connection(move |connection| {
            let transaction = connection.transaction()?;
            let indexed = transaction
                .prepare("SELECT conversation_id FROM conversation_index")?
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            for id in indexed {
                if !present.contains(&id) {
                    transaction.execute(
                        "DELETE FROM conversation_index WHERE conversation_id = ?1",
                        [&id],
                    )?;
                    transaction.execute(
                        "DELETE FROM conversation_search_messages WHERE conversation_id = ?1",
                        [&id],
                    )?;
                    transaction.execute(
                        "DELETE FROM composer_recent WHERE conversation_id = ?1",
                        [&id],
                    )?;
                    transaction.execute(
                        "DELETE FROM usage_projection WHERE conversation_id = ?1",
                        [&id],
                    )?;
                }
            }
            transaction.execute("DELETE FROM model_recents", [])?;
            transaction.commit()?;
            Ok(())
        })
        .await?;
        for path in paths {
            if let Err(error) = self.project_conversation_with_mode(path, true).await {
                tracing::warn!(%error, "failed to reconcile conversation projection");
            }
        }
        Ok(())
    }

    /// Watches only the canonical conversation file. `global.db` is opened
    /// after its revision changes, and a contended projection is retried with
    /// capped exponential backoff without delaying the conversation writer.
    pub(crate) fn start_projection_watcher(&self, path: PathBuf) {
        let cancellation = {
            let mut watchers = self
                .projection_watchers
                .lock()
                .expect("projection watcher registry lock poisoned");
            if watchers.contains_key(&path) {
                return;
            }
            let cancellation = tokio_util::sync::CancellationToken::new();
            watchers.insert(path.clone(), cancellation.clone());
            cancellation
        };
        let store = self.clone();
        tokio::spawn(async move {
            let mut projected_revision = 0_i64;
            let mut retry = std::time::Duration::from_millis(100);
            loop {
                if cancellation.is_cancelled() {
                    break;
                }
                let watched_path = path.clone();
                let revision = tokio::task::spawn_blocking(move || {
                    let connection = Connection::open_with_flags(
                        watched_path,
                        OpenFlags::SQLITE_OPEN_READ_ONLY,
                    )?;
                    connection
                        .query_row(
                            "SELECT index_revision FROM conversations LIMIT 1",
                            [],
                            |row| row.get::<_, i64>(0),
                        )
                        .map_err(RuntimeError::from)
                })
                .await;
                let Ok(Ok(revision)) = revision else {
                    break;
                };
                if revision > projected_revision {
                    match store.project_conversation(path.clone()).await {
                        Ok(()) => {
                            projected_revision = revision;
                            retry = std::time::Duration::from_millis(100);
                        }
                        Err(error) => {
                            tracing::debug!(%error, "conversation projection deferred");
                            tokio::select! {
                                () = cancellation.cancelled() => break,
                                () = tokio::time::sleep(retry) => {}
                            }
                            retry = (retry * 2).min(std::time::Duration::from_secs(5));
                            continue;
                        }
                    }
                }
                tokio::select! {
                    () = cancellation.cancelled() => break,
                    () = tokio::time::sleep(std::time::Duration::from_millis(100)) => {}
                }
            }
            let mut watchers = store
                .projection_watchers
                .lock()
                .expect("projection watcher registry lock poisoned");
            if watchers.get(&path) == Some(&cancellation) {
                watchers.remove(&path);
            }
        });
    }

    pub(crate) fn stop_projection_watcher(&self, path: &Path) {
        if let Some(cancellation) = self
            .projection_watchers
            .lock()
            .expect("projection watcher registry lock poisoned")
            .remove(path)
        {
            cancellation.cancel();
        }
    }

    #[cfg(test)]
    pub(crate) fn projection_watcher_count(&self) -> usize {
        self.projection_watchers
            .lock()
            .expect("projection watcher registry lock poisoned")
            .len()
    }
}

#[derive(Clone)]
struct Projection {
    id: String,
    workspace: String,
    title: String,
    created_at: String,
    updated_at: String,
    active_node_id: String,
    status: String,
    preview: String,
    message_count: i64,
    agent: String,
    mode: String,
    provider: Option<String>,
    model: Option<String>,
    archived: bool,
    favourite: bool,
    revision: i64,
    search: Vec<(String, String)>,
    composer: Vec<(String, String, String, String, String)>,
    recents: Vec<(String, String, i64, i64)>,
    usage: Vec<ProjectedUsage>,
}

#[derive(Clone)]
struct ProjectedUsage {
    node_id: String,
    created_at_millis: i64,
    provider: Option<String>,
    model: Option<String>,
    usage: crate::ModelUsage,
}

fn read_projection(path: &Path) -> Result<Projection, RuntimeError> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    connection.pragma_update(None, "query_only", "ON")?;
    let mut projection = connection.query_row(
        "WITH RECURSIVE branch(id, parent_id, kind, content_json, depth) AS (
           SELECT n.id, n.parent_id, n.kind, n.content_json, 0
           FROM nodes n JOIN conversations c ON c.active_node_id = n.id
           UNION ALL
           SELECT n.id, n.parent_id, n.kind, n.content_json, branch.depth + 1
           FROM nodes n JOIN branch ON branch.parent_id = n.id
         )
         SELECT c.id, c.project_dir, COALESCE(c.title, ''), c.created_at, c.updated_at,
                c.active_node_id, n.status,
                COALESCE((SELECT json_extract(content_json, '$.text') FROM branch
                          WHERE kind IN ('user_message', 'assistant_message')
                            AND TRIM(COALESCE(json_extract(content_json, '$.text'), '')) != ''
                          ORDER BY depth LIMIT 1), ''),
                (SELECT COUNT(*) FROM branch WHERE kind IN ('user_message', 'assistant_message')),
                c.active_agent, c.active_mode,
                COALESCE(
                  (SELECT provider FROM mode_model_selections
                   WHERE conversation_id = c.id AND mode = c.active_mode),
                  CASE WHEN c.active_mode = 'plan' THEN c.plan_provider ELSE c.normal_provider END
                ),
                COALESCE(
                  (SELECT model FROM mode_model_selections
                   WHERE conversation_id = c.id AND mode = c.active_mode),
                  CASE WHEN c.active_mode = 'plan' THEN c.plan_model ELSE c.normal_model END
                ),
                c.archived, c.favourite, c.index_revision
         FROM conversations c JOIN nodes n ON n.id = c.active_node_id LIMIT 1",
        [],
        |row| {
            Ok(Projection {
                id: row.get(0)?,
                workspace: row.get(1)?,
                title: row.get(2)?,
                created_at: row.get(3)?,
                updated_at: row.get(4)?,
                active_node_id: row.get(5)?,
                status: row.get(6)?,
                preview: row.get(7)?,
                message_count: row.get(8)?,
                agent: row.get(9)?,
                mode: row.get(10)?,
                provider: row.get(11)?,
                model: row.get(12)?,
                archived: row.get(13)?,
                favourite: row.get(14)?,
                revision: row.get(15)?,
                search: Vec::new(),
                composer: Vec::new(),
                recents: Vec::new(),
                usage: Vec::new(),
            })
        },
    )?;
    projection.search = connection.prepare(
        "SELECT id, COALESCE(json_extract(content_json, '$.text'), '') FROM nodes WHERE kind = 'user_message'",
    )?.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
        .map(|row| row.map(|(id, text)| (id, normalize_search(&text))))
        .collect::<Result<_, _>>()?;
    projection.composer = connection
        .prepare(
            "SELECT id, input_kind, text, attachment_specs_json, created_at
             FROM composer_history WHERE publish_to_global = 1",
        )?
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })?
        .collect::<Result<_, _>>()?;
    projection.recents = connection
        .prepare(
            "SELECT provider, model, recency, selection_count FROM model_recents
             ORDER BY recency, provider, model",
        )?
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect::<Result<_, _>>()?;
    projection.usage = connection
        .prepare(
            "SELECT n.id, CAST(n.created_at AS INTEGER), n.provider, n.model,
                    u.input_tokens, u.non_cached_input_tokens, u.cache_read_input_tokens,
                    u.cache_write_input_tokens, u.output_tokens, u.reasoning_tokens,
                    u.total_tokens, u.input_cost, u.cache_read_cost, u.cache_write_cost,
                    u.output_cost, u.reasoning_cost, u.total_cost, u.currency,
                    u.pricing_source, u.pricing_version
             FROM model_usage u JOIN nodes n ON n.id = u.node_id
             ORDER BY CAST(n.created_at AS INTEGER), n.id",
        )?
        .query_map([], projected_usage_row)?
        .collect::<Result<_, _>>()?;
    let delegated = connection
        .prepare(
            "SELECT id, CAST(COALESCE(completed_at, started_at, created_at) AS INTEGER),
                    provider, model, usage_json
             FROM agent_runs WHERE usage_json IS NOT NULL
             ORDER BY CAST(COALESCE(completed_at, started_at, created_at) AS INTEGER), id",
        )?
        .query_map([], |row| {
            let usage_json = row.get::<_, String>(4)?;
            let usage = serde_json::from_str(&usage_json).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    4,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?;
            Ok(ProjectedUsage {
                node_id: format!("agent:{}", row.get::<_, String>(0)?),
                created_at_millis: row.get(1)?,
                provider: row.get(2)?,
                model: row.get(3)?,
                usage,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    projection.usage.extend(delegated);
    Ok(projection)
}

fn write_projection(
    connection: &mut Connection,
    projection: Projection,
    force_equal_revision: bool,
) -> Result<(), RuntimeError> {
    let transaction = connection.transaction()?;
    let current = transaction
        .query_row(
            "SELECT source_revision FROM conversation_index WHERE conversation_id = ?1",
            [&projection.id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    if current.is_some_and(|revision| {
        revision > projection.revision || (!force_equal_revision && revision == projection.revision)
    }) {
        return Ok(());
    }
    transaction.execute(
        "INSERT INTO conversation_index(
           conversation_id, workspace, title, title_search, created_at, updated_at,
           active_node_id, active_status, active_branch_preview, active_branch_message_count,
           agent, mode, provider, model, archived, favourite, source_revision)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
         ON CONFLICT(conversation_id) DO UPDATE SET
           workspace=excluded.workspace, title=excluded.title, title_search=excluded.title_search,
           created_at=excluded.created_at, updated_at=excluded.updated_at,
           active_node_id=excluded.active_node_id, active_status=excluded.active_status,
           active_branch_preview=excluded.active_branch_preview,
           active_branch_message_count=excluded.active_branch_message_count,
           agent=excluded.agent, mode=excluded.mode, provider=excluded.provider, model=excluded.model,
           archived=excluded.archived, favourite=excluded.favourite,
           source_revision=excluded.source_revision
         WHERE excluded.source_revision > conversation_index.source_revision",
        params![projection.id, projection.workspace, projection.title,
            normalize_search(&projection.title), projection.created_at, projection.updated_at,
            projection.active_node_id, projection.status, projection.preview,
            projection.message_count, projection.agent, projection.mode, projection.provider,
            projection.model, projection.archived, projection.favourite, projection.revision],
    )?;
    transaction.execute(
        "DELETE FROM conversation_search_messages WHERE conversation_id = ?1",
        [&projection.id],
    )?;
    transaction.execute(
        "DELETE FROM usage_projection WHERE conversation_id = ?1",
        [&projection.id],
    )?;
    let reset = usage_reset_watermark(&transaction)?;
    for projected in &projection.usage {
        if projected.created_at_millis <= reset {
            continue;
        }
        let usage = &projected.usage;
        let cost = usage.cost.as_ref();
        transaction.execute(
            "INSERT INTO usage_projection(
                conversation_id, node_id, created_at_millis, project, provider, model,
                input_tokens, non_cached_input_tokens, cache_read_input_tokens,
                cache_write_input_tokens, output_tokens, reasoning_tokens, total_tokens,
                input_cost, cache_read_cost, cache_write_cost, output_cost, reasoning_cost,
                total_cost, currency, pricing_source, pricing_version)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                     ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)",
            params![
                projection.id,
                projected.node_id,
                projected.created_at_millis,
                projection.workspace,
                projected.provider,
                projected.model,
                usage.input_tokens,
                usage.non_cached_input_tokens,
                usage.cache_read_input_tokens,
                usage.cache_write_input_tokens,
                usage.output_tokens,
                usage.reasoning_tokens,
                usage.total_tokens,
                cost.and_then(|value| value.input_cost.as_deref()),
                cost.and_then(|value| value.cache_read_cost.as_deref()),
                cost.and_then(|value| value.cache_write_cost.as_deref()),
                cost.and_then(|value| value.output_cost.as_deref()),
                cost.and_then(|value| value.reasoning_cost.as_deref()),
                cost.and_then(|value| value.total_cost.as_deref()),
                cost.map(|value| value.currency.as_str()),
                cost.map(|value| value.pricing_source.as_str()),
                cost.map(|value| value.pricing_version.as_str())
            ],
        )?;
    }
    for (node_id, text) in projection.search {
        transaction.execute(
            "INSERT INTO conversation_search_messages(conversation_id, node_id, normalized_user_text) VALUES (?1, ?2, ?3)",
            params![projection.id, node_id, text],
        )?;
    }
    for (entry_id, kind, text, specs, created_at) in projection.composer {
        transaction.execute(
            "INSERT OR IGNORE INTO composer_recent(entry_id, conversation_id, entry_kind, input_kind, text, attachment_specs_json, created_at)
             VALUES (?1, ?2, 'user_message', ?3, ?4, ?5, ?6)",
            params![entry_id, projection.id, kind, text, specs, created_at],
        )?;
    }
    transaction.execute(
        "DELETE FROM composer_recent WHERE entry_id NOT IN (
           SELECT entry_id FROM composer_recent ORDER BY created_at DESC, entry_id DESC LIMIT 100
         )",
        [],
    )?;
    for (provider, model, _local_recency, count) in projection.recents {
        let projected_count = transaction
            .query_row(
                "SELECT selection_count FROM model_recents WHERE provider = ?1 AND model = ?2",
                params![provider, model],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        if projected_count.is_none_or(|existing| existing < count) {
            let next_recency = transaction.query_row(
                "SELECT COALESCE(MAX(recency), 0) + 1 FROM model_recents",
                [],
                |row| row.get::<_, i64>(0),
            )?;
            transaction.execute(
                "INSERT INTO model_recents(provider, model, recency, selection_count) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(provider, model) DO UPDATE SET
                   recency=excluded.recency, selection_count=excluded.selection_count",
                params![provider, model, next_recency, count],
            )?;
        }
    }
    transaction.execute(
        "DELETE FROM model_recents WHERE rowid IN (
           SELECT rowid FROM (
             SELECT rowid, ROW_NUMBER() OVER (
               PARTITION BY provider ORDER BY recency DESC, selection_count DESC, model ASC
             ) AS rank
             FROM model_recents
           ) WHERE rank > ?1
         )",
        [i64::try_from(MODEL_RECENTS_PER_PROVIDER).expect("recent-model bound fits SQLite")],
    )?;
    transaction.commit()?;
    Ok(())
}

fn normalize_search(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn unix_millis() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(i64::MAX)
}

fn usage_reset_watermark(connection: &Connection) -> Result<i64, rusqlite::Error> {
    connection.query_row(
        "SELECT reset_at_millis FROM usage_state WHERE singleton = 1",
        [],
        |row| row.get(0),
    )
}

fn optional_tokens(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Option<u64>> {
    row.get::<_, Option<i64>>(index)
        .map(|value| value.map(|value| u64::try_from(value).unwrap_or_default()))
}

fn model_usage_from_row(
    row: &rusqlite::Row<'_>,
    offset: usize,
) -> rusqlite::Result<crate::ModelUsage> {
    let total_cost = row.get::<_, Option<String>>(offset + 12)?;
    let currency = row.get::<_, Option<String>>(offset + 13)?;
    let pricing_source = row.get::<_, Option<String>>(offset + 14)?;
    let pricing_version = row.get::<_, Option<String>>(offset + 15)?;
    let cost_present = total_cost.is_some() || currency.is_some() || pricing_source.is_some();
    Ok(crate::ModelUsage {
        input_tokens: optional_tokens(row, offset)?,
        non_cached_input_tokens: optional_tokens(row, offset + 1)?,
        cache_read_input_tokens: optional_tokens(row, offset + 2)?,
        cache_write_input_tokens: optional_tokens(row, offset + 3)?,
        output_tokens: optional_tokens(row, offset + 4)?,
        reasoning_tokens: optional_tokens(row, offset + 5)?,
        total_tokens: optional_tokens(row, offset + 6)?,
        provider_usage: serde_json::Value::Null,
        cost: cost_present.then(|| crate::ModelCost {
            input_cost: row.get(offset + 7).unwrap_or_default(),
            cache_read_cost: row.get(offset + 8).unwrap_or_default(),
            cache_write_cost: row.get(offset + 9).unwrap_or_default(),
            output_cost: row.get(offset + 10).unwrap_or_default(),
            reasoning_cost: row.get(offset + 11).unwrap_or_default(),
            total_cost,
            currency: currency.unwrap_or_default(),
            pricing_source: pricing_source.unwrap_or_default(),
            pricing_version: pricing_version.unwrap_or_default(),
        }),
    })
}

fn usage_row_with_label(row: &rusqlite::Row<'_>) -> rusqlite::Result<(String, crate::ModelUsage)> {
    Ok((row.get(0)?, model_usage_from_row(row, 2)?))
}

fn projected_usage_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectedUsage> {
    Ok(ProjectedUsage {
        node_id: row.get(0)?,
        created_at_millis: row.get(1)?,
        provider: row.get(2)?,
        model: row.get(3)?,
        usage: model_usage_from_row(row, 4)?,
    })
}

fn query_usage(
    connection: &Connection,
    conversation_id: Option<&str>,
    since: Option<i64>,
) -> Result<SessionUsage, RuntimeError> {
    let reset = usage_reset_watermark(connection)?;
    let since = since.unwrap_or_default().max(reset);
    let mut statement = connection.prepare(
        "SELECT node_id, input_tokens, non_cached_input_tokens, cache_read_input_tokens,
                cache_write_input_tokens, output_tokens, reasoning_tokens, total_tokens,
                input_cost, cache_read_cost, cache_write_cost, output_cost, reasoning_cost,
                total_cost, currency, pricing_source, pricing_version
         FROM usage_projection
         WHERE created_at_millis > ?1 AND (?2 IS NULL OR conversation_id = ?2)
         ORDER BY created_at_millis, node_id",
    )?;
    let rows = statement.query_map(params![since, conversation_id], |row| {
        model_usage_from_row(row, 1)
    })?;
    let mut usage = SessionUsage::default();
    for row in rows {
        usage.add(&row?);
    }
    Ok(usage)
}

fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn open_checked_global(path: &Path) -> Result<Connection, RuntimeError> {
    let mut connection = Connection::open(path)?;
    configure_global(&connection)?;
    migrate_global(&mut connection)?;
    let check: String = connection.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    if check != "ok" {
        return Err(RuntimeError::InvalidOption(format!(
            "global database integrity check failed: {check}"
        )));
    }
    Ok(connection)
}

fn quarantine_global(path: &Path) {
    if !path.exists() {
        return;
    }
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let quarantine = path.with_file_name(format!("global.db.corrupt-{suffix}"));
    let _ = std::fs::rename(path, quarantine);
}

fn is_corrupt_error(error: &RuntimeError) -> bool {
    matches!(error,
        RuntimeError::Database(rusqlite::Error::SqliteFailure(code, _))
        if matches!(code.code,
            rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase))
}

fn is_global_contention<T>(result: &Result<T, RuntimeError>) -> bool {
    matches!(
        result,
        Err(RuntimeError::Database(rusqlite::Error::SqliteFailure(code, _)))
            if matches!(code.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    )
}

fn is_best_effort_sqlite_error(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(code, _)
            if matches!(code.code,
                rusqlite::ErrorCode::DatabaseBusy
                    | rusqlite::ErrorCode::DatabaseLocked
                    | rusqlite::ErrorCode::DatabaseCorrupt
                    | rusqlite::ErrorCode::CannotOpen
                    | rusqlite::ErrorCode::NotADatabase)
    )
}

fn is_corrupt_sqlite_error(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(code, _)
            if matches!(
                code.code,
                rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase
            )
    )
}

fn is_best_effort_read_error(error: &RuntimeError) -> bool {
    matches!(error, RuntimeError::Database(_))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn global_usage_aggregates_groups_and_resets() {
        let temporary = tempfile::tempdir().unwrap();
        let store = GlobalStore::open(temporary.path()).await.unwrap();
        let first = crate::ConversationId::new();
        let second = crate::ConversationId::new();
        let now = unix_millis();
        store
            .with_write_connection(move |connection| {
                for (conversation, node, project, provider, model, tokens, cost) in [
                    (
                        first.to_string(),
                        "one",
                        "/a",
                        "openai",
                        "gpt",
                        10_i64,
                        Some("0.01"),
                    ),
                    (
                        second.to_string(),
                        "two",
                        "/b",
                        "anthropic",
                        "claude",
                        20_i64,
                        None,
                    ),
                ] {
                    connection.execute(
                        "INSERT INTO usage_projection(conversation_id, node_id, created_at_millis,
                        project, provider, model, input_tokens, output_tokens, total_tokens,
                        total_cost, currency, pricing_source, pricing_version)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?7, ?8, ?9, ?10, ?11)",
                        params![
                            conversation,
                            node,
                            now - 1_000,
                            project,
                            provider,
                            model,
                            tokens,
                            cost,
                            cost.map(|_| "USD"),
                            cost.map(|_| "provider_reported"),
                            cost.map(|_| "test")
                        ],
                    )?;
                }
                Ok(())
            })
            .await
            .unwrap();

        let overview = store.usage_overview(first).await.unwrap();
        assert_eq!(overview.current_conversation.total_tokens, Some(10));
        assert_eq!(overview.total.total_tokens, Some(30));
        assert!(overview.total.total_cost().is_none());
        assert_eq!(store.usage_by_project().await.unwrap().len(), 2);
        assert_eq!(
            store.usage_by_model().await.unwrap()[0].label,
            "anthropic/claude"
        );

        store.reset_usage().await.unwrap();
        assert_eq!(
            store.usage_overview(first).await.unwrap(),
            UsageOverview::default()
        );
    }

    #[test]
    fn projection_excludes_conversation_local_composer_entries() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("conversation.db");
        let mut connection = Connection::open(&path).unwrap();
        super::super::migrate_conversation(&mut connection).unwrap();
        let conversation_id = super::super::create_session(
            &mut connection,
            &crate::NewSession {
                workspace: temporary.path().into(),
            },
        )
        .unwrap();
        super::super::record_slash_command(
            &mut connection,
            conversation_id,
            "/model mock/echo",
            None,
            true,
        )
        .unwrap();
        super::super::record_slash_command(
            &mut connection,
            conversation_id,
            "/not-a-command",
            None,
            false,
        )
        .unwrap();
        drop(connection);

        let projection = read_projection(&path).unwrap();
        let texts = projection
            .composer
            .into_iter()
            .map(|(_, _, text, _, _)| text)
            .collect::<Vec<_>>();
        assert_eq!(texts, ["/model mock/echo"]);
    }

    #[tokio::test]
    async fn prompt_cache_resources_round_trip_and_delete() {
        let temporary = tempfile::tempdir().unwrap();
        let store = GlobalStore::open(temporary.path()).await.unwrap();
        let resource = NamedCacheResource {
            provider: "google".into(),
            credential_scope_hash: "credential".into(),
            model: "gemini-2.5-flash".into(),
            conversation_key_hash: "conversation".into(),
            prefix_hash: "prefix".into(),
            cached_input_boundary: 7,
            resource_name: "cachedContents/one".into(),
            cached_token_count: 4_321,
            created_at_millis: 10,
            expires_at_millis: 20,
        };
        let identity = resource.identity();
        store
            .save_prompt_cache_resource(resource.clone())
            .await
            .unwrap();
        assert_eq!(
            store
                .load_prompt_cache_resource(identity.clone())
                .await
                .unwrap(),
            Some(resource)
        );
        let mut other_credential = identity.clone();
        other_credential.credential_scope_hash = "other-account".into();
        assert_eq!(
            store
                .load_prompt_cache_resource(other_credential)
                .await
                .unwrap(),
            None
        );
        store
            .delete_prompt_cache_resource(identity.clone())
            .await
            .unwrap();
        assert_eq!(
            store.load_prompt_cache_resource(identity).await.unwrap(),
            None
        );
    }
}
