#![allow(clippy::too_many_arguments)] // Store commands keep their database fields explicit.

#[allow(clippy::wildcard_imports)]
use super::*;
use fs2::FileExt as _;
use tracing::Instrument as _;

impl StoreHandle {
    pub(crate) async fn load_conversation_diff(
        &self,
        conversation_id: ConversationId,
    ) -> Result<crate::tools::ConversationDiff, RuntimeError> {
        self.request(|reply| StoreCommand::LoadConversationDiff {
            conversation_id,
            reply,
        })
        .await
    }

    pub(crate) async fn clear_conversation_diff(
        &self,
        conversation_id: ConversationId,
    ) -> Result<(), RuntimeError> {
        self.request(|reply| StoreCommand::ClearConversationDiff {
            conversation_id,
            reply,
        })
        .await
    }

    pub(crate) fn load_conversation_permissions(
        &self,
    ) -> Result<Vec<crate::PermissionRule>, RuntimeError> {
        let path = self.database_path.as_ref().ok_or_else(|| {
            RuntimeError::InvalidOption("conversation database is not persistent".into())
        })?;
        let connection = Connection::open(path)?;
        connection.busy_timeout(std::time::Duration::from_secs(30))?;
        let mut statement = connection
            .prepare("SELECT rule_json FROM conversation_permissions ORDER BY created_at, rowid")?;
        statement
            .query_map([], |row| row.get::<_, String>(0))?
            .map(|value| {
                let value = value?;
                serde_json::from_str(&value).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub(crate) fn save_conversation_permission(
        &self,
        rule: &crate::PermissionRule,
    ) -> Result<(), RuntimeError> {
        let path = self.database_path.as_ref().ok_or_else(|| {
            RuntimeError::InvalidOption("conversation database is not persistent".into())
        })?;
        let connection = Connection::open(path)?;
        connection.busy_timeout(std::time::Duration::from_secs(30))?;
        connection.execute(
            "INSERT INTO conversation_permissions(id, rule_json) VALUES (?1, ?2)
             ON CONFLICT(id) DO UPDATE SET rule_json = excluded.rule_json",
            rusqlite::params![rule.id, serde_json::to_string(rule)?],
        )?;
        Ok(())
    }

    pub(crate) fn delete_conversation_permission(&self, id: &str) -> Result<(), RuntimeError> {
        let path = self.database_path.as_ref().ok_or_else(|| {
            RuntimeError::InvalidOption("conversation database is not persistent".into())
        })?;
        let connection = Connection::open(path)?;
        connection.busy_timeout(std::time::Duration::from_secs(30))?;
        connection.execute("DELETE FROM conversation_permissions WHERE id = ?1", [id])?;
        Ok(())
    }

    pub(crate) fn subscribe_live(&self) -> broadcast::Receiver<DurableEvent> {
        self.live_events.subscribe()
    }

    pub(crate) fn database_path(&self) -> Option<std::path::PathBuf> {
        self.database_path.clone()
    }

    pub(crate) async fn append_configuration_changed(
        &self,
        conversation_id: ConversationId,
        path: Option<std::path::PathBuf>,
    ) -> Result<(), RuntimeError> {
        self.request(|reply| StoreCommand::AppendConfigurationChanged {
            conversation_id,
            path,
            reply,
        })
        .await
    }

    pub(crate) async fn append_transcript_notice(
        &self,
        conversation_id: ConversationId,
        message: String,
    ) -> Result<(), RuntimeError> {
        self.request(|reply| StoreCommand::AppendTranscriptNotice {
            conversation_id,
            message,
            reply,
        })
        .await
    }

    pub(crate) async fn append_recap(
        &self,
        conversation_id: ConversationId,
        parent_id: NodeId,
        text: String,
    ) -> Result<bool, RuntimeError> {
        self.request(|reply| StoreCommand::AppendRecap {
            conversation_id,
            parent_id,
            text,
            reply,
        })
        .await
    }

    pub(crate) async fn append_workspace_transition(
        &self,
        conversation_id: ConversationId,
        transition: crate::runtime::worktrees::WorkspaceTransition,
    ) -> Result<(), RuntimeError> {
        self.request(|reply| StoreCommand::AppendWorkspaceTransition {
            conversation_id,
            transition,
            reply,
        })
        .await
    }

    pub(crate) async fn synchronize_local_context(
        &self,
        conversation_id: ConversationId,
        snapshot: crate::LocalContextSnapshot,
    ) -> Result<Option<LocalContextSync>, RuntimeError> {
        self.request(|reply| StoreCommand::SynchronizeLocalContext {
            conversation_id,
            snapshot,
            reply,
        })
        .await
    }

    pub(crate) async fn load_conversation_title(
        &self,
        conversation_id: ConversationId,
    ) -> Result<Option<String>, RuntimeError> {
        self.request(|reply| StoreCommand::LoadConversationTitle {
            conversation_id,
            reply,
        })
        .await
    }

    pub(crate) async fn rename_conversation(
        &self,
        conversation_id: ConversationId,
        title: String,
    ) -> Result<String, RuntimeError> {
        self.request(|reply| StoreCommand::RenameConversation {
            conversation_id,
            title,
            reply,
        })
        .await
    }

    pub(crate) async fn claim_title_generation(
        &self,
        conversation_id: ConversationId,
    ) -> Result<Option<TitleGenerationClaim>, RuntimeError> {
        self.request(|reply| StoreCommand::ClaimTitleGeneration {
            conversation_id,
            reply,
        })
        .await
    }

    pub(crate) async fn finish_title_generation(
        &self,
        conversation_id: ConversationId,
        title: Option<String>,
    ) -> Result<bool, RuntimeError> {
        self.request(|reply| StoreCommand::FinishTitleGeneration {
            conversation_id,
            title,
            reply,
        })
        .await
    }

    #[allow(dead_code)]
    #[tracing::instrument(level = "trace", name = "store.open", skip_all)]
    pub(crate) async fn open(
        database_path: &Path,
        command_capacity: usize,
        event_capacity: usize,
    ) -> Result<Self, RuntimeError> {
        if let Some(parent) = database_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let mut connection = Connection::open(database_path)?;
        configure_conversation(&connection)?;
        migrate_conversation(&mut connection)?;
        recover(&mut connection)?;

        let (tx, rx) = mpsc::channel(command_capacity);
        let (live_events, _) = broadcast::channel(event_capacity);
        tokio::spawn(run_store(connection, rx, live_events.clone()));
        Ok(Self {
            tx,
            live_events,
            database_path: Some(database_path.to_path_buf()),
            _writer_lock: None,
            _open_lock: None,
        })
    }

    #[allow(dead_code)]
    pub(crate) async fn open_in_memory(
        command_capacity: usize,
        event_capacity: usize,
    ) -> Result<Self, RuntimeError> {
        let mut connection = Connection::open_in_memory()?;
        configure_conversation(&connection)?;
        migrate_conversation(&mut connection)?;
        recover(&mut connection)?;

        let (tx, rx) = mpsc::channel(command_capacity);
        let (live_events, _) = broadcast::channel(event_capacity);
        tokio::spawn(run_store(connection, rx, live_events.clone()));
        Ok(Self {
            tx,
            live_events,
            database_path: None,
            _writer_lock: None,
            _open_lock: None,
        })
    }

    /// Opens a canonical conversation after attempting its OS writer lock.
    /// Recovery and migrations are performed only by the lock owner.
    #[tracing::instrument(level = "trace", name = "store.conversation.open", skip_all)]
    pub(crate) async fn open_conversation(
        database_path: &Path,
        writer_lock_path: &Path,
        open_lock_path: &Path,
        command_capacity: usize,
        event_capacity: usize,
    ) -> Result<(Self, crate::SessionAccess), RuntimeError> {
        let database_path = database_path.to_path_buf();
        let writer_lock_path = writer_lock_path.to_path_buf();
        let open_lock_path = open_lock_path.to_path_buf();
        let read_database_path = database_path.clone();
        let parent_span = tracing::Span::current();
        let (connection, lock, open_lock, owner) = tokio::task::spawn_blocking(move || {
            {
                let _span = tracing::trace_span!(
                    parent: &parent_span,
                    "store.conversation.directory_setup"
                )
                .entered();
                if let Some(parent) = database_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                if let Some(parent) = writer_lock_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                if let Some(parent) = open_lock_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
            }
            let open_lock = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&open_lock_path)?;
            open_lock.lock_shared()?;
            let (lock, owner) = {
                let _span = tracing::trace_span!(
                    parent: &parent_span,
                    "store.conversation.lock_acquisition"
                )
                .entered();
                let lock = std::fs::OpenOptions::new()
                    .create(true)
                    .truncate(false)
                    .read(true)
                    .write(true)
                    .open(&writer_lock_path)?;
                // A just-closed session may still be unwinding its command
                // task and releasing the writer lock. Give that normal
                // shutdown a brief grace period before exposing a read-only
                // observer to a restart.
                let mut owner = false;
                for _ in 0..40 {
                    if lock.try_lock_exclusive().is_ok() {
                        owner = true;
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                (lock, owner)
            };
            let mut connection = if owner {
                Connection::open(&database_path)?
            } else {
                Connection::open_with_flags(
                    &database_path,
                    rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
                )?
            };
            {
                let _span = tracing::trace_span!(
                    parent: &parent_span,
                    "store.conversation.sqlite_configuration"
                )
                .entered();
                if owner {
                    configure_conversation(&connection)?;
                } else {
                    connection.busy_timeout(std::time::Duration::from_millis(250))?;
                    connection.pragma_update(None, "query_only", "ON")?;
                }
            }
            if owner {
                {
                    let _span = tracing::trace_span!(
                        parent: &parent_span,
                        "store.conversation.migration"
                    )
                    .entered();
                    migrate_conversation(&mut connection)?;
                }
                {
                    let _span = tracing::trace_span!(
                        parent: &parent_span,
                        "store.conversation.recovery"
                    )
                    .entered();
                    recover(&mut connection)?;
                }
            }
            Ok::<_, RuntimeError>((connection, lock, open_lock, owner))
        })
        .await
        .map_err(|_| RuntimeError::RuntimeStopped)??;
        let (tx, rx) = mpsc::channel(command_capacity);
        let (live_events, _) = broadcast::channel(event_capacity);
        tokio::spawn(run_store(connection, rx, live_events.clone()));
        let access = if owner {
            crate::SessionAccess::Owner
        } else {
            crate::SessionAccess::Observer {
                takeover_available: false,
            }
        };
        Ok((
            Self {
                tx,
                live_events,
                database_path: Some(read_database_path),
                _writer_lock: owner.then(|| std::sync::Arc::new(lock)),
                _open_lock: Some(std::sync::Arc::new(open_lock)),
            },
            access,
        ))
    }

    #[allow(dead_code)]
    pub(crate) async fn list_conversations(
        &self,
        workspace: Option<std::path::PathBuf>,
    ) -> Result<Vec<crate::ConversationSummary>, RuntimeError> {
        self.request(|reply| StoreCommand::ListConversations { workspace, reply })
            .await
    }

    pub(crate) async fn set_conversation_archived(
        &self,
        conversation_id: ConversationId,
        archived: bool,
    ) -> Result<(), RuntimeError> {
        self.request(|reply| StoreCommand::SetConversationArchived {
            conversation_id,
            archived,
            reply,
        })
        .await
    }

    pub(crate) async fn set_conversation_favourite(
        &self,
        conversation_id: ConversationId,
        favourite: bool,
    ) -> Result<(), RuntimeError> {
        self.request(|reply| StoreCommand::SetConversationFavourite {
            conversation_id,
            favourite,
            reply,
        })
        .await
    }

    pub(crate) async fn load_composer_history(
        &self,
        conversation_id: ConversationId,
        new_session_seed: bool,
    ) -> Result<Vec<ComposerHistoryEntry>, RuntimeError> {
        self.request(|reply| StoreCommand::LoadComposerHistory {
            conversation_id,
            new_session_seed,
            reply,
        })
        .await
    }

    pub(crate) async fn record_slash_command(
        &self,
        conversation_id: ConversationId,
        text: String,
        user_text: Option<String>,
        publish_to_global: bool,
    ) -> Result<(), RuntimeError> {
        self.request(|reply| StoreCommand::RecordSlashCommand {
            conversation_id,
            text,
            user_text,
            publish_to_global,
            reply,
        })
        .await
    }

    pub(crate) async fn record_bash_command(
        &self,
        conversation_id: ConversationId,
        command: String,
    ) -> Result<NodeId, RuntimeError> {
        self.request(|reply| StoreCommand::RecordBashCommand {
            conversation_id,
            command,
            reply,
        })
        .await
    }

    pub(crate) async fn load_history(
        &self,
        conversation_id: ConversationId,
    ) -> Result<Vec<crate::HistoryNode>, RuntimeError> {
        self.request(|reply| StoreCommand::LoadHistory {
            conversation_id,
            reply,
        })
        .await
    }

    pub(crate) async fn load_history_hydration(
        &self,
        conversation_id: ConversationId,
    ) -> Result<HistoryHydration, RuntimeError> {
        let Some(database_path) = self.database_path.clone() else {
            return Ok(HistoryHydration {
                revision: self.load_durable_cursor(conversation_id).await?,
                history: self.load_history(conversation_id).await?,
                workspace: self.load_workspace(conversation_id).await?,
                terminals: self.list_terminals(conversation_id).await?,
                agent_runs: self.list_agent_runs(conversation_id).await?,
            });
        };
        tokio::task::spawn_blocking(move || {
            let mut connection = Connection::open_with_flags(
                database_path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )?;
            connection.busy_timeout(std::time::Duration::from_millis(250))?;
            connection.pragma_update(None, "query_only", "ON")?;
            let transaction =
                connection.transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)?;
            let revision = transaction
                .query_row(
                    "SELECT revision FROM conversations WHERE id = ?1",
                    [conversation_id.to_string()],
                    |row| row.get::<_, i64>(0),
                )?
                .try_into()
                .map(EventCursor)
                .map(Some)
                .map_err(|_| {
                    RuntimeError::InvalidOption("negative conversation revision".into())
                })?;
            let history = load_history(&transaction, conversation_id)?;
            let workspace = load_workspace(&transaction, conversation_id)?;
            let terminals = list_terminal_previews(&transaction, conversation_id)?;
            let agent_runs = list_agent_runs(&transaction, conversation_id)?;
            transaction.commit()?;
            Ok(HistoryHydration {
                revision,
                history,
                workspace,
                terminals,
                agent_runs,
            })
        })
        .instrument(tracing::trace_span!("agent.history.query"))
        .await
        .map_err(|_| RuntimeError::RuntimeStopped)?
    }

    pub(crate) async fn load_durable_cursor(
        &self,
        conversation_id: ConversationId,
    ) -> Result<Option<EventCursor>, RuntimeError> {
        self.request(|reply| StoreCommand::LoadDurableCursor {
            conversation_id,
            reply,
        })
        .await
    }

    pub(crate) async fn load_session_stats(
        &self,
        conversation_id: ConversationId,
    ) -> Result<StoredSessionStats, RuntimeError> {
        self.request(|reply| StoreCommand::LoadSessionStats {
            conversation_id,
            reply,
        })
        .await
    }

    pub(crate) async fn load_session_hydration(
        &self,
        conversation_id: ConversationId,
    ) -> Result<SessionHydration, RuntimeError> {
        self.request(|reply| StoreCommand::LoadSessionHydration {
            conversation_id,
            reply,
        })
        .await
    }

    pub(crate) async fn load_transcript_page(
        &self,
        conversation_id: ConversationId,
        cursor: Option<crate::TranscriptCursor>,
    ) -> Result<TranscriptPageHydration, RuntimeError> {
        self.request(|reply| StoreCommand::LoadTranscriptPage {
            conversation_id,
            cursor,
            reply,
        })
        .await
    }

    pub(crate) async fn fork(
        &self,
        conversation_id: ConversationId,
        at: NodeId,
        planning_modes: Vec<String>,
    ) -> Result<NodeId, RuntimeError> {
        self.request(|reply| StoreCommand::Fork {
            conversation_id,
            at,
            planning_modes,
            reply,
        })
        .await
    }

    pub(crate) async fn hard_fork(
        &self,
        conversation_id: ConversationId,
        new_conversation_id: ConversationId,
        at: NodeId,
        destination: std::path::PathBuf,
        planning_modes: Vec<String>,
    ) -> Result<NodeId, RuntimeError> {
        self.request(|reply| StoreCommand::HardFork {
            conversation_id,
            new_conversation_id,
            at,
            destination,
            planning_modes,
            reply,
        })
        .await
    }

    #[allow(dead_code)]
    pub(crate) async fn create_session(
        &self,
        options: NewSession,
    ) -> Result<ConversationId, RuntimeError> {
        self.request(|reply| StoreCommand::CreateSession { options, reply })
            .await
    }

    pub(crate) async fn create_initialized_session(
        &self,
        conversation_id: ConversationId,
        options: NewSession,
        name: Option<String>,
        agent: String,
        mode: String,
        model_selection: Option<(String, String, Option<String>)>,
        plan_model_selection: Option<(String, String, Option<String>)>,
        mode_selections: Vec<(String, String, String, Option<String>)>,
    ) -> Result<CreatedSession, RuntimeError> {
        self.request(|reply| StoreCommand::CreateInitializedSession {
            conversation_id,
            options,
            name,
            agent,
            mode,
            model_selection,
            plan_model_selection,
            mode_selections,
            reply,
        })
        .await
    }

    pub(crate) async fn load_resumed_session(
        &self,
        conversation_id: ConversationId,
    ) -> Result<ResumedSession, RuntimeError> {
        self.request(|reply| StoreCommand::LoadResumedSession {
            conversation_id,
            reply,
        })
        .await
    }

    pub(crate) async fn load_workspace(
        &self,
        id: ConversationId,
    ) -> Result<std::path::PathBuf, RuntimeError> {
        self.request(|reply| StoreCommand::LoadWorkspace { id, reply })
            .await
    }

    pub(crate) async fn load_model_selection(
        &self,
        id: ConversationId,
    ) -> Result<StoredModelSelection, RuntimeError> {
        self.request(|reply| StoreCommand::LoadModelSelection { id, reply })
            .await
    }

    pub(crate) async fn load_plan_model_selection(
        &self,
        id: ConversationId,
    ) -> Result<StoredModelSelection, RuntimeError> {
        self.request(|reply| StoreCommand::LoadPlanModelSelection { id, reply })
            .await
    }

    pub(crate) async fn load_mode_selections(
        &self,
        id: ConversationId,
    ) -> Result<std::collections::BTreeMap<String, StoredModelSelection>, RuntimeError> {
        self.request(|reply| StoreCommand::LoadModeSelections { id, reply })
            .await
    }

    pub(crate) async fn load_active_profiles(
        &self,
        id: ConversationId,
    ) -> Result<(String, String), RuntimeError> {
        self.request(|reply| StoreCommand::LoadActiveProfiles { id, reply })
            .await
    }

    pub(crate) async fn set_active_profile(
        &self,
        conversation_id: ConversationId,
        agent: Option<String>,
        mode: Option<String>,
        pending: bool,
    ) -> Result<(), RuntimeError> {
        self.request(|reply| StoreCommand::SetActiveProfile {
            conversation_id,
            agent,
            mode,
            pending,
            reply,
        })
        .await
    }

    pub(crate) async fn create_agent_run(
        &self,
        conversation_id: ConversationId,
        parent_turn_id: TurnId,
        profile: String,
        model: ModelRef,
        effort: Option<String>,
        task: String,
    ) -> Result<crate::AgentRun, RuntimeError> {
        self.request(|reply| StoreCommand::CreateAgentRun {
            conversation_id,
            parent_turn_id,
            profile,
            model,
            effort,
            task,
            reply,
        })
        .await
    }

    pub(crate) async fn set_agent_run_status(
        &self,
        conversation_id: ConversationId,
        id: crate::AgentRunId,
        status: crate::AgentRunStatus,
        result: Option<String>,
        error: Option<String>,
        usage: Option<crate::ModelUsage>,
    ) -> Result<crate::AgentRun, RuntimeError> {
        self.request(|reply| StoreCommand::SetAgentRunStatus {
            conversation_id,
            id,
            status,
            result,
            error,
            usage,
            reply,
        })
        .await
    }

    pub(crate) async fn load_agent_run(
        &self,
        conversation_id: ConversationId,
        id: crate::AgentRunId,
    ) -> Result<crate::AgentRun, RuntimeError> {
        self.request(|reply| StoreCommand::LoadAgentRun {
            conversation_id,
            id,
            reply,
        })
        .await
    }

    pub(crate) async fn list_agent_runs(
        &self,
        conversation_id: ConversationId,
    ) -> Result<Vec<crate::AgentRun>, RuntimeError> {
        self.request(|reply| StoreCommand::ListAgentRuns {
            conversation_id,
            reply,
        })
        .await
    }

    pub(crate) async fn load_agent_run_log_page(
        &self,
        conversation_id: ConversationId,
        id: crate::AgentRunId,
        after: u64,
    ) -> Result<crate::AgentRunLogPage, RuntimeError> {
        self.request(|reply| StoreCommand::LoadAgentRunLogPage {
            conversation_id,
            id,
            after,
            reply,
        })
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn append_agent_run_activity(
        &self,
        conversation_id: ConversationId,
        id: crate::AgentRunId,
        tool: String,
        arguments: serde_json::Value,
        output: serde_json::Value,
        is_error: bool,
        permission_audit: Option<crate::PermissionAudit>,
    ) -> Result<crate::AgentRun, RuntimeError> {
        self.request(|reply| StoreCommand::AppendAgentRunActivity {
            conversation_id,
            id,
            tool,
            arguments,
            output,
            is_error,
            permission_audit: permission_audit.map(Box::new),
            reply,
        })
        .await
    }

    pub(crate) async fn append_agent_run_assistant(
        &self,
        conversation_id: ConversationId,
        id: crate::AgentRunId,
        text: String,
    ) -> Result<crate::AgentRun, RuntimeError> {
        self.request(|reply| StoreCommand::AppendAgentRunAssistant {
            conversation_id,
            id,
            text,
            reply,
        })
        .await
    }

    pub(crate) async fn upsert_terminal(
        &self,
        terminal: crate::TerminalSnapshot,
    ) -> Result<(), RuntimeError> {
        self.request(|reply| StoreCommand::UpsertTerminal { terminal, reply })
            .await
    }

    pub(crate) async fn load_terminal(
        &self,
        conversation_id: ConversationId,
        id: crate::TerminalId,
    ) -> Result<crate::TerminalSnapshot, RuntimeError> {
        self.request(|reply| StoreCommand::LoadTerminal {
            conversation_id,
            id,
            reply,
        })
        .await
    }

    pub(crate) async fn list_terminals(
        &self,
        conversation_id: ConversationId,
    ) -> Result<Vec<crate::TerminalSnapshot>, RuntimeError> {
        self.request(|reply| StoreCommand::ListTerminals {
            conversation_id,
            reply,
        })
        .await
    }

    pub(crate) async fn claim_completion(
        &self,
        conversation_id: ConversationId,
        kind: impl Into<String>,
        id: impl Into<String>,
        claim_kind: impl Into<String>,
    ) -> Result<bool, RuntimeError> {
        self.request(|reply| StoreCommand::ClaimCompletion {
            conversation_id,
            kind: kind.into(),
            id: id.into(),
            claim_kind: claim_kind.into(),
            reply,
        })
        .await
    }

    pub(crate) async fn append_pending_completion_notice(
        &self,
        conversation_id: ConversationId,
    ) -> Result<Option<(NodeId, TurnId)>, RuntimeError> {
        self.request(|reply| StoreCommand::AppendPendingCompletionNotice {
            conversation_id,
            reply,
        })
        .await
    }

    pub(crate) async fn append_interrupt_notice(
        &self,
        conversation_id: ConversationId,
        queued_steering: bool,
    ) -> Result<Option<NodeId>, RuntimeError> {
        self.request(|reply| StoreCommand::AppendInterruptNotice {
            conversation_id,
            queued_steering,
            reply,
        })
        .await
    }

    #[allow(dead_code)]
    pub(crate) async fn claim_agent_completions(
        &self,
        conversation_id: ConversationId,
        ids: Vec<String>,
        claim_kind: impl Into<String>,
    ) -> Result<(), RuntimeError> {
        self.request(|reply| StoreCommand::ClaimAgentCompletions {
            conversation_id,
            ids,
            claim_kind: claim_kind.into(),
            reply,
        })
        .await
    }

    pub(crate) async fn load_retry_context(
        &self,
        id: ConversationId,
    ) -> Result<RetryContext, RuntimeError> {
        self.request(|reply| StoreCommand::LoadRetryContext { id, reply })
            .await
    }

    pub(crate) async fn reconstruct_model_input(
        &self,
        conversation_id: ConversationId,
        node_id: NodeId,
        resize_images: bool,
    ) -> Result<Vec<ModelInput>, RuntimeError> {
        self.request(|reply| StoreCommand::ReconstructModelInput {
            conversation_id,
            node_id,
            resize_images,
            reply,
        })
        .await
    }

    pub(crate) async fn load_compaction_source(
        &self,
        conversation_id: ConversationId,
        resize_images: bool,
        exclude_active_tip: bool,
    ) -> Result<super::CompactionSource, RuntimeError> {
        self.request(|reply| StoreCommand::LoadCompactionSource {
            conversation_id,
            resize_images,
            exclude_active_tip,
            reply,
        })
        .await
    }

    pub(crate) async fn append_compaction_summary(
        &self,
        conversation_id: ConversationId,
        parent_id: NodeId,
        content: crate::CompactionSummary,
        metadata: ResponseMetadata,
    ) -> Result<NodeId, RuntimeError> {
        self.request(|reply| StoreCommand::AppendCompactionSummary {
            conversation_id,
            parent_id,
            content,
            metadata,
            reply,
        })
        .await
    }

    #[allow(dead_code)]
    pub(crate) async fn load_recent_models(
        &self,
        provider: String,
        limit: usize,
    ) -> Result<Vec<String>, RuntimeError> {
        self.request(|reply| StoreCommand::LoadRecentModels {
            provider,
            limit,
            reply,
        })
        .await
    }

    pub(crate) async fn load_blob(&self, id: crate::BlobId) -> Result<Vec<u8>, RuntimeError> {
        self.request(|reply| StoreCommand::LoadBlob { id, reply })
            .await
    }

    pub(crate) async fn store_image_blob(
        &self,
        id: crate::BlobId,
        sha256: String,
        bytes: Vec<u8>,
    ) -> Result<crate::BlobId, RuntimeError> {
        self.request(|reply| StoreCommand::StoreImageBlob {
            id,
            sha256,
            bytes,
            reply,
        })
        .await
    }

    #[cfg(test)]
    pub(crate) async fn append_user(
        &self,
        conversation_id: ConversationId,
        text: String,
    ) -> Result<(NodeId, TurnId), RuntimeError> {
        self.request(|reply| StoreCommand::AppendUser {
            conversation_id,
            text,
            attachments: Vec::new(),
            attachment_specs: Vec::new(),
            deferred_attachment_paths: Vec::new(),
            images: Vec::new(),
            image_chips: Vec::new(),
            reply,
        })
        .await
    }

    pub(crate) async fn append_user_with_attachments(
        &self,
        conversation_id: ConversationId,
        text: String,
        attachments: Vec<CapturedAttachment>,
        attachment_specs: Vec<AttachmentSpec>,
        deferred_attachment_paths: Vec<std::path::PathBuf>,
    ) -> Result<(NodeId, TurnId), RuntimeError> {
        self.request(|reply| StoreCommand::AppendUser {
            conversation_id,
            text,
            attachments,
            attachment_specs,
            deferred_attachment_paths,
            images: Vec::new(),
            image_chips: Vec::new(),
            reply,
        })
        .await
    }

    pub(crate) async fn append_user_draft(
        &self,
        conversation_id: ConversationId,
        draft: crate::UserDraft,
        attachments: Vec<CapturedAttachment>,
        deferred_attachment_paths: Vec<std::path::PathBuf>,
    ) -> Result<(NodeId, TurnId), RuntimeError> {
        self.request(|reply| StoreCommand::AppendUser {
            conversation_id,
            text: draft.text,
            attachments,
            attachment_specs: draft.attachment_specs,
            deferred_attachment_paths,
            images: draft.images,
            image_chips: draft.image_chips,
            reply,
        })
        .await
    }

    #[allow(dead_code)]
    pub(crate) async fn set_model_selection(
        &self,
        conversation_id: ConversationId,
        provider: String,
        model: String,
        effort: Option<String>,
        pending: bool,
        plan: bool,
    ) -> Result<(), RuntimeError> {
        self.request(|reply| StoreCommand::SetModelSelection {
            conversation_id,
            provider,
            model,
            effort,
            pending,
            plan,
            reply,
        })
        .await
    }

    pub(crate) async fn set_mode_model_selection(
        &self,
        conversation_id: ConversationId,
        mode: String,
        provider: String,
        model: String,
        effort: Option<String>,
        pending: bool,
    ) -> Result<(), RuntimeError> {
        self.request(|reply| StoreCommand::SetModeModelSelection {
            conversation_id,
            mode,
            provider,
            model,
            effort,
            pending,
            reply,
        })
        .await
    }

    #[allow(dead_code)]
    pub(crate) async fn initialize_model_selection(
        &self,
        conversation_id: ConversationId,
        provider: String,
        model: String,
        effort: Option<String>,
        plan: bool,
    ) -> Result<(), RuntimeError> {
        self.request(|reply| StoreCommand::InitializeModelSelection {
            conversation_id,
            provider,
            model,
            effort,
            plan,
            reply,
        })
        .await
    }

    pub(crate) async fn initialize_mode_model_selection(
        &self,
        conversation_id: ConversationId,
        mode: String,
        provider: String,
        model: String,
        effort: Option<String>,
    ) -> Result<(), RuntimeError> {
        self.request(|reply| StoreCommand::InitializeModeModelSelection {
            conversation_id,
            mode,
            provider,
            model,
            effort,
            reply,
        })
        .await
    }

    pub(crate) async fn queue_input(
        &self,
        conversation_id: ConversationId,
        text: String,
        target: QueueTarget,
        attachments: Vec<AttachmentSpec>,
        images: Vec<crate::ImageAttachment>,
        image_chips: Vec<crate::ImageChipRange>,
        blocked_by_startup: bool,
        require_subagent: bool,
    ) -> Result<QueuedMessage, RuntimeError> {
        self.request(|reply| StoreCommand::QueueInput {
            conversation_id,
            text,
            target,
            attachments,
            images,
            image_chips,
            blocked_by_startup,
            require_subagent,
            reply,
        })
        .await
    }

    pub(crate) async fn queue_compact(
        &self,
        conversation_id: ConversationId,
        target: QueueTarget,
        instructions: Option<String>,
    ) -> Result<QueuedMessage, RuntimeError> {
        self.request(|reply| StoreCommand::QueueCompact {
            conversation_id,
            target,
            instructions,
            reply,
        })
        .await
    }

    pub(crate) async fn queue_mode_input(
        &self,
        conversation_id: ConversationId,
        mode: String,
        command_text: String,
        text: String,
        target: QueueTarget,
        attachments: Vec<AttachmentSpec>,
    ) -> Result<QueuedMessage, RuntimeError> {
        self.request(|reply| StoreCommand::QueueModeInput {
            conversation_id,
            mode,
            command_text,
            text,
            target,
            attachments,
            reply,
        })
        .await
    }

    pub(crate) async fn replace_queued(
        &self,
        conversation_id: ConversationId,
        id: QueuedMessageId,
        text: String,
        target: Option<QueueTarget>,
        attachments: Vec<AttachmentSpec>,
        images: Vec<crate::ImageAttachment>,
        image_chips: Vec<crate::ImageChipRange>,
    ) -> Result<QueuedMessage, RuntimeError> {
        self.request(|reply| StoreCommand::ReplaceQueued {
            conversation_id,
            id,
            text,
            target,
            attachments,
            images,
            image_chips,
            reply,
        })
        .await
    }

    pub(crate) async fn begin_editing_queued(
        &self,
        conversation_id: ConversationId,
        id: QueuedMessageId,
    ) -> Result<(), RuntimeError> {
        self.request(|reply| StoreCommand::BeginEditingQueued {
            conversation_id,
            id,
            reply,
        })
        .await
    }

    pub(crate) async fn replace_queued_mode_input(
        &self,
        conversation_id: ConversationId,
        id: QueuedMessageId,
        mode: String,
        command_text: String,
        text: String,
        target: Option<QueueTarget>,
        attachments: Vec<AttachmentSpec>,
    ) -> Result<QueuedMessage, RuntimeError> {
        self.request(|reply| StoreCommand::ReplaceQueuedModeInput {
            conversation_id,
            id,
            mode,
            command_text,
            text,
            target,
            attachments,
            reply,
        })
        .await
    }

    pub(crate) async fn delete_queued(
        &self,
        conversation_id: ConversationId,
        id: QueuedMessageId,
    ) -> Result<(), RuntimeError> {
        self.request(|reply| StoreCommand::DeleteQueued {
            conversation_id,
            id,
            reply,
        })
        .await
    }

    pub(crate) async fn promote_queued(
        &self,
        conversation_id: ConversationId,
        id: QueuedMessageId,
    ) -> Result<QueuedMessage, RuntimeError> {
        self.request(|reply| StoreCommand::PromoteQueued {
            conversation_id,
            id,
            reply,
        })
        .await
    }

    pub(crate) async fn dispatch_queued(
        &self,
        conversation_id: ConversationId,
        expected: QueuedMessage,
        attachments: Vec<CapturedAttachment>,
        attachment_specs: Vec<AttachmentSpec>,
        deferred_attachment_paths: Vec<std::path::PathBuf>,
    ) -> Result<(NodeId, TurnId, String, Option<String>), RuntimeError> {
        self.request(|reply| StoreCommand::DispatchQueued {
            conversation_id,
            expected,
            attachments,
            attachment_specs,
            deferred_attachment_paths,
            reply,
        })
        .await
    }

    pub(crate) async fn peek_next_queued(
        &self,
        conversation_id: ConversationId,
        target: QueueTarget,
    ) -> Result<Option<QueuedMessage>, RuntimeError> {
        self.request(|reply| StoreCommand::PeekNextQueued {
            conversation_id,
            target,
            reply,
        })
        .await
    }

    pub(crate) async fn peek_next_startup_queued(
        &self,
        conversation_id: ConversationId,
    ) -> Result<Option<QueuedMessage>, RuntimeError> {
        self.request(|reply| StoreCommand::PeekNextStartupQueued {
            conversation_id,
            reply,
        })
        .await
    }

    pub(crate) async fn start_assistant(
        &self,
        conversation_id: ConversationId,
        parent_id: NodeId,
        turn_id: TurnId,
    ) -> Result<NodeId, RuntimeError> {
        self.request(|reply| StoreCommand::StartAssistant {
            conversation_id,
            parent_id,
            turn_id,
            attempt: None,
            reply,
        })
        .await
    }

    pub(crate) async fn start_model_assistant(
        &self,
        conversation_id: ConversationId,
        parent_id: NodeId,
        turn_id: TurnId,
        attempt: ModelAttemptSnapshot,
    ) -> Result<NodeId, RuntimeError> {
        self.request(|reply| StoreCommand::StartAssistant {
            conversation_id,
            parent_id,
            turn_id,
            attempt: Some(attempt),
            reply,
        })
        .await
    }

    pub(crate) async fn append_assistant_delta(
        &self,
        conversation_id: ConversationId,
        node_id: NodeId,
        text: String,
        delta: String,
    ) -> Result<(), RuntimeError> {
        self.request(|reply| StoreCommand::AppendAssistantDelta {
            conversation_id,
            node_id,
            text,
            delta,
            reply,
        })
        .await
    }

    pub(crate) async fn start_plan(
        &self,
        conversation_id: ConversationId,
        node_id: NodeId,
    ) -> Result<(), RuntimeError> {
        self.request(|reply| StoreCommand::StartPlan {
            conversation_id,
            node_id,
            reply,
        })
        .await
    }

    pub(crate) async fn append_plan_delta(
        &self,
        conversation_id: ConversationId,
        node_id: NodeId,
        text: String,
        delta: String,
    ) -> Result<(), RuntimeError> {
        self.request(|reply| StoreCommand::AppendPlanDelta {
            conversation_id,
            node_id,
            text,
            delta,
            reply,
        })
        .await
    }

    #[allow(dead_code)]
    pub(crate) async fn append_context_clear(
        &self,
        conversation_id: ConversationId,
        mode: &str,
        context_window: u64,
    ) -> Result<NodeId, RuntimeError> {
        self.request(|reply| StoreCommand::AppendContextClear {
            conversation_id,
            mode: mode.into(),
            context_window,
            reply,
        })
        .await
    }

    pub(crate) async fn append_accepted_plan(
        &self,
        conversation_id: ConversationId,
        plan: String,
        note: Option<String>,
        clear_context: bool,
        compact_context: bool,
        context_window: u64,
    ) -> Result<(NodeId, TurnId), RuntimeError> {
        self.request(|reply| StoreCommand::AppendAcceptedPlan {
            conversation_id,
            plan,
            note,
            clear_context,
            compact_context,
            context_window,
            reply,
        })
        .await
    }

    #[cfg(test)]
    pub(crate) async fn complete_assistant(
        &self,
        conversation_id: ConversationId,
        node_id: NodeId,
        metadata: ResponseMetadata,
        tool_calls: Vec<PendingToolCall>,
    ) -> Result<Vec<StoredToolCall>, RuntimeError> {
        self.complete_assistant_with_reasoning(
            conversation_id,
            node_id,
            metadata,
            tool_calls,
            Vec::new(),
        )
        .await
    }

    pub(crate) async fn complete_assistant_with_reasoning(
        &self,
        conversation_id: ConversationId,
        node_id: NodeId,
        metadata: ResponseMetadata,
        tool_calls: Vec<PendingToolCall>,
        reasoning: Vec<ModelInput>,
    ) -> Result<Vec<StoredToolCall>, RuntimeError> {
        self.request(|reply| StoreCommand::CompleteAssistant {
            conversation_id,
            node_id,
            metadata,
            tool_calls,
            reasoning,
            reply,
        })
        .await
    }

    pub(crate) async fn save_response_continuation(
        &self,
        conversation_id: ConversationId,
        assistant_node_id: NodeId,
        continuation: StoredResponseContinuation,
    ) -> Result<(), RuntimeError> {
        self.request(|reply| StoreCommand::SaveResponseContinuation {
            conversation_id,
            assistant_node_id,
            continuation,
            reply,
        })
        .await
    }

    pub(crate) async fn load_response_continuation(
        &self,
        conversation_id: ConversationId,
        branch_tip: NodeId,
    ) -> Result<Option<StoredResponseContinuation>, RuntimeError> {
        self.request(|reply| StoreCommand::LoadResponseContinuation {
            conversation_id,
            branch_tip,
            reply,
        })
        .await
    }

    pub(crate) async fn append_tool_result(
        &self,
        conversation_id: ConversationId,
        tool_call: StoredToolCall,
        output: serde_json::Value,
        is_error: bool,
        duration_millis: u64,
    ) -> Result<NodeId, RuntimeError> {
        self.request(|reply| StoreCommand::AppendToolResult {
            conversation_id,
            tool_call,
            output,
            is_error,
            duration_millis,
            transition: None,
            reply,
        })
        .await
    }

    pub(crate) async fn append_tool_result_transition(
        &self,
        conversation_id: ConversationId,
        tool_call: StoredToolCall,
        output: serde_json::Value,
        is_error: bool,
        duration_millis: u64,
        transition: crate::runtime::worktrees::WorkspaceTransition,
    ) -> Result<NodeId, RuntimeError> {
        self.request(|reply| StoreCommand::AppendToolResult {
            conversation_id,
            tool_call,
            output,
            is_error,
            duration_millis,
            transition: Some(transition),
            reply,
        })
        .await
    }

    pub(crate) async fn append_permission_decision(
        &self,
        conversation_id: ConversationId,
        tool_call: StoredToolCall,
        audit: crate::PermissionAudit,
    ) -> Result<NodeId, RuntimeError> {
        self.request(|reply| StoreCommand::AppendPermissionDecision {
            conversation_id,
            tool_call,
            audit: Box::new(audit),
            reply,
        })
        .await
    }

    pub(crate) async fn cancel_assistant(
        &self,
        conversation_id: ConversationId,
        node_id: NodeId,
    ) -> Result<(), RuntimeError> {
        self.request(|reply| StoreCommand::CancelAssistant {
            conversation_id,
            node_id,
            reply,
        })
        .await
    }

    pub(crate) async fn fail_assistant(
        &self,
        conversation_id: ConversationId,
        node_id: NodeId,
        code: String,
        message: String,
        retryable: bool,
    ) -> Result<(), RuntimeError> {
        self.request(|reply| StoreCommand::FailAssistant {
            conversation_id,
            node_id,
            status: "failed",
            rewind_to_parent: false,
            code,
            message,
            retryable,
            reply,
        })
        .await
    }

    pub(crate) async fn interrupt_assistant(
        &self,
        conversation_id: ConversationId,
        node_id: NodeId,
        code: String,
        message: String,
        retryable: bool,
    ) -> Result<(), RuntimeError> {
        self.request(|reply| StoreCommand::FailAssistant {
            conversation_id,
            node_id,
            status: "interrupted",
            rewind_to_parent: true,
            code,
            message,
            retryable,
            reply,
        })
        .await
    }

    #[tracing::instrument(level = "trace", name = "store.request", skip_all)]
    async fn request<T>(
        &self,
        command: impl FnOnce(oneshot::Sender<Result<T, RuntimeError>>) -> StoreCommand,
    ) -> Result<T, RuntimeError> {
        let (reply, response) = oneshot::channel();
        self.tx
            .send(command(reply))
            .await
            .map_err(|_| RuntimeError::RuntimeStopped)?;
        response.await.map_err(|_| RuntimeError::RuntimeStopped)?
    }
}
