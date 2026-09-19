#![allow(clippy::too_many_arguments)] // Session construction deliberately keeps its state explicit.

use super::supervised;
#[allow(clippy::wildcard_imports)]
use super::*;

impl AgentRuntime {
    #[cfg(test)]
    pub(crate) async fn live_session_count(&self) -> usize {
        self.sessions.lock().await.len()
    }

    fn conversation_path(&self, id: ConversationId) -> std::path::PathBuf {
        self.conversation_storage_dir
            .join("conversations")
            .join(format!("{id}.db"))
    }

    fn writer_lock_path(&self, id: ConversationId) -> std::path::PathBuf {
        self.conversation_storage_dir
            .join("conversation-writer-locks")
            .join(format!("{id}.lock"))
    }

    fn open_lock_path(&self, id: ConversationId) -> std::path::PathBuf {
        self.conversation_storage_dir
            .join("conversation-open-locks")
            .join(format!("{id}.lock"))
    }

    /// Opens the durable runtime and initializes the current database schema.
    ///
    /// # Errors
    ///
    /// Returns an error when channel capacities are zero or the database cannot be opened,
    /// migrated, or recovered.
    #[tracing::instrument(level = "trace", name = "agent.runtime.open", skip_all)]
    pub async fn open(options: RuntimeOptions) -> Result<Self, RuntimeError> {
        Self::open_with_providers(options, Vec::new()).await
    }

    #[cfg(test)]
    pub(super) async fn open_with(
        options: RuntimeOptions,
        provider: Arc<dyn Provider>,
    ) -> Result<Self, RuntimeError> {
        Self::open_with_providers(options, vec![provider]).await
    }

    #[allow(clippy::too_many_lines)]
    async fn open_with_providers(
        options: RuntimeOptions,
        additional_providers: Vec<Arc<dyn Provider>>,
    ) -> Result<Self, RuntimeError> {
        let config = options.config.snapshot();
        if options.command_channel_capacity == 0 {
            return Err(RuntimeError::InvalidOption(
                "command channel capacity must be non-zero".into(),
            ));
        }
        if options.event_channel_capacity == 0 {
            return Err(RuntimeError::InvalidOption(
                "event channel capacity must be non-zero".into(),
            ));
        }
        tracing::info!(storage_dir = %options.storage_dir.display(), "opening agent runtime");
        tokio::fs::create_dir_all(options.storage_dir.join("conversations")).await?;
        tokio::fs::create_dir_all(options.storage_dir.join("conversation-writer-locks")).await?;
        tokio::fs::create_dir_all(options.storage_dir.join("conversation-open-locks")).await?;
        let global_store = GlobalStore::open(
            options
                .global_storage_dir
                .as_deref()
                .unwrap_or(&options.storage_dir),
        )
        .instrument(tracing::trace_span!(
            "agent.runtime.startup.phase",
            phase = "global_store_opened"
        ))
        .await?;
        let models_dev_cache = options
            .models_dev_cache_path
            .clone()
            .unwrap_or_else(|| options.storage_dir.as_path().join("models.json"));
        let catalog = {
            let _span =
                tracing::trace_span!("agent.runtime.startup.phase", phase = "catalog_initialized")
                    .entered();
            crate::provider::catalog::CatalogManager::new(global_store.clone(), &models_dev_cache)
        };

        let (providers, global_selection) = {
            let _span = tracing::trace_span!(
                "agent.runtime.startup.phase",
                phase = "provider_registry_initialized"
            )
            .entered();
            let overrides = additional_providers
                .into_iter()
                .map(|provider| (provider.descriptor().id.clone(), provider))
                .collect::<std::collections::BTreeMap<_, _>>();
            let credential_dir = options
                .credential_dir
                .clone()
                .unwrap_or_else(|| options.storage_dir.clone());
            let default_model = default_model(&config);
            let default_effort = default_effort(&config);
            let mode_selections = config
                .modes()
                .unwrap_or_default()
                .into_iter()
                .map(|(name, mode)| {
                    (
                        name,
                        ModeSelection {
                            model: mode
                                .model
                                .as_ref()
                                .and_then(exact_model)
                                .or_else(|| default_model.clone()),
                            effort: mode
                                .model
                                .and_then(|selection| selection.effort)
                                .or_else(|| default_effort.clone()),
                            manual: false,
                        },
                    )
                })
                .collect();
            let global_selection = Arc::new(std::sync::RwLock::new(SessionSelection {
                model: default_model,
                effort: default_effort,
                allow_disabled_provider: None,
                plan_model: None,
                plan_effort: None,
                normal_manual: false,
                plan_manual: false,
                mode_selections,
            }));
            let providers =
                ProviderRegistry::new(&config, credential_dir, overrides, global_store.clone());
            (providers, global_selection)
        };

        let config_fallback_path = options.config.path().map_or_else(
            || options.storage_dir.as_path().join("config.toml"),
            std::path::Path::to_path_buf,
        );
        let startup = StartupResourceCoordinator::new(
            options.instructions,
            options.instruction_paths,
            options.config.clone(),
            providers.clone(),
            global_store.clone(),
            catalog.clone(),
        );
        let mcp_secret_dir = options
            .credential_dir
            .clone()
            .unwrap_or_else(|| options.storage_dir.clone());
        let runtime = Self {
            global_store,
            conversation_storage_dir: options.storage_dir,
            persist_conversations: options.persist_conversations,
            publish_conversations: options.publish_conversations,
            sessions: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            catalog,
            config_store: options.config,
            #[cfg(test)]
            config,
            startup,
            providers,
            global_selection,
            command_capacity: options.command_channel_capacity,
            event_capacity: options.event_channel_capacity,
            permissions_path: options.permissions_path,
            temporary_workspace_trust: options.temporary_workspace_trust,
            config_fallback_path,
            mcp_supervisor: crate::McpSupervisor::new()
                .with_secret_store(crate::McpSecretStore::new(&mcp_secret_dir)),
            cleanup_gate: Arc::new(tokio::sync::Mutex::new(())),
            cleanup_protected_conversation: options.cleanup_protected_conversation,
            conversation_maintenance: watch::channel(0).0,
            temporary_dir: crate::scratchpad::configured_root(options.temporary_dir),
        };
        {
            let _span = tracing::trace_span!(
                "agent.runtime.startup.phase",
                phase = "background_services_started"
            )
            .entered();
            runtime.watch_configuration();
            runtime.startup.start_initial_hydration();
            let mut maintenance_runtime = runtime.clone();
            // Background maintenance must not extend any session handle's
            // lifetime (and therefore its conversation writer lock).
            maintenance_runtime.sessions = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
            tokio::spawn(async move {
                if let Err(error) = maintenance_runtime.global_store.reconcile().await {
                    tracing::warn!(%error, "global conversation reconciliation failed");
                }
                maintenance_runtime
                    .conversation_maintenance
                    .send_modify(|revision| *revision = revision.wrapping_add(1));
                if maintenance_runtime.persist_conversations
                    && maintenance_runtime
                        .config_snapshot()
                        .conversation_cleanup()
                        .automatic()
                {
                    match maintenance_runtime
                        .cleanup_conversations_protecting(
                            maintenance_runtime.cleanup_protected_conversation,
                        )
                        .await
                    {
                        Ok(report) => {
                            if !report.failures.is_empty()
                                || !report.skipped.is_empty()
                                || report.remaining.bytes > 0
                                || report.remaining.conversations > 0
                            {
                                tracing::warn!(
                                    removed = report.removed.len(),
                                    skipped = report.skipped.len(),
                                    failures = report.failures.len(),
                                    remaining_bytes = report.remaining.bytes,
                                    remaining_conversations = report.remaining.conversations,
                                    "automatic conversation cleanup completed with retained items"
                                );
                            } else if !report.removed.is_empty() {
                                tracing::info!(
                                    removed = report.removed.len(),
                                    removed_bytes = report.removed_bytes,
                                    "automatic conversation cleanup completed"
                                );
                            }
                        }
                        Err(error) => {
                            tracing::warn!(%error, "automatic conversation cleanup failed")
                        }
                    }
                    maintenance_runtime
                        .conversation_maintenance
                        .send_modify(|revision| *revision = revision.wrapping_add(1));
                }
            });
        }
        Ok(runtime)
    }

    #[cfg(test)]
    pub(super) async fn open_with_provider_set(
        options: RuntimeOptions,
        provider: Arc<dyn Provider>,
        mut additional_providers: Vec<Arc<dyn Provider>>,
    ) -> Result<Self, RuntimeError> {
        additional_providers.push(provider);
        Self::open_with_providers(options, additional_providers).await
    }

    #[must_use]
    pub fn config_snapshot(&self) -> crate::ConfigSnapshot {
        self.config_store.snapshot()
    }

    #[must_use]
    pub fn subscribe_config(&self) -> tokio::sync::watch::Receiver<crate::ConfigSnapshot> {
        self.config_store.subscribe()
    }

    /// Subscribes to completed background conversation indexing and cleanup passes.
    #[must_use]
    pub fn subscribe_conversation_maintenance(&self) -> tokio::sync::watch::Receiver<u64> {
        self.conversation_maintenance.subscribe()
    }

    fn watch_configuration(&self) {
        let mut updates = self.config_store.subscribe();
        let providers = self.providers.clone();
        let selection = self.global_selection.clone();
        let store = self.global_store.clone();
        let startup = self.startup.clone();
        tokio::spawn(async move {
            while updates.changed().await.is_ok() {
                let config = updates.borrow_and_update().clone();
                for provider in providers.rebuild(&config) {
                    if let Err(error) = store.invalidate_model_catalog(&provider).await {
                        tracing::warn!(%provider, %error, "failed to invalidate model catalog after configuration reload");
                    }
                }
                if let Ok(mut selection) = selection.write()
                    && selection
                        .model
                        .as_ref()
                        .is_some_and(|model| !config.provider_enabled(&model.provider))
                {
                    selection.model = default_model(&config);
                    selection.effort = default_effort(&config);
                    selection.normal_manual = false;
                }
                startup.hydrate_instructions().await;
            }
        });
    }

    #[must_use]
    pub fn instruction_snapshot(&self) -> crate::InstructionSnapshot {
        self.startup.instructions_for_dispatch()
    }

    /// Returns current provider authentication and explicit enablement state.
    ///
    /// # Errors
    ///
    /// Returns an error when the provider is unknown or authentication state cannot be resolved.
    pub async fn provider_availability(
        &self,
        provider_id: &str,
    ) -> Result<crate::ProviderAvailability, crate::ProviderError> {
        let provider = self.providers.get(provider_id).ok_or_else(|| {
            crate::ProviderError::configuration(format!("unknown provider: {provider_id}"))
        })?;
        Ok(crate::ProviderAvailability {
            descriptor: provider.descriptor().clone(),
            enabled: self.providers.is_enabled(provider_id),
            auth: provider.auth_state().await?,
            has_managed_api_key: provider.has_managed_api_key().await,
        })
    }

    /// Loads the cached or bundled catalog immediately without network access.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown/unconfigured provider or a cache read failure.
    pub async fn model_catalog(
        &self,
        provider_id: &str,
    ) -> Result<crate::ResolvedModelCatalog, RuntimeError> {
        self.startup.wait_for_models().await;
        let settings = self.provider_settings(provider_id)?;
        let provider = self.providers.get(provider_id).ok_or_else(|| {
            RuntimeError::InvalidOption(format!("unknown provider adapter: {provider_id}"))
        })?;
        let subscription_plan = provider.subscription_plan().await;
        self.catalog
            .current(
                provider.descriptor(),
                &settings,
                subscription_plan.as_deref(),
            )
            .await
    }

    /// Explicitly refreshes an enabled provider catalog, falling back to cache or bundled seeds.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown/unconfigured provider or a cache write/read failure.
    pub async fn refresh_model_catalog(
        &self,
        provider_id: &str,
    ) -> Result<crate::ResolvedModelCatalog, RuntimeError> {
        let mut settings = self.provider_settings(provider_id)?;
        settings.enabled = self.providers.is_enabled(provider_id);
        let provider = self.providers.get(provider_id).ok_or_else(|| {
            RuntimeError::InvalidOption(format!("unknown provider adapter: {provider_id}"))
        })?;
        self.catalog.refresh(provider.clone(), &settings).await
    }

    /// Resolves aliases/manual model IDs and applies request-time capability degradation.
    ///
    /// # Errors
    ///
    /// Returns an error for catalog storage failures or a known incompatible model capability.
    pub async fn prepare_model(
        &self,
        provider_id: &str,
        model_or_alias: &str,
        effort: Option<&str>,
        requires_tools: bool,
    ) -> Result<(crate::ModelDescriptor, crate::PreparedModelCapabilities), RuntimeError> {
        let catalog = self.model_catalog(provider_id).await?;
        let model = catalog.model_or_unknown(model_or_alias);
        let capabilities =
            crate::prepare_model_capabilities(&model, effort, requires_tools, 32_768)
                .map_err(|error| RuntimeError::InvalidOption(error.to_string()))?;
        Ok((model, capabilities))
    }

    fn provider_settings(
        &self,
        provider_id: &str,
    ) -> Result<crate::ProviderSettings, RuntimeError> {
        self.config_store
            .snapshot()
            .provider(provider_id)
            .cloned()
            .ok_or_else(|| {
                RuntimeError::InvalidOption(format!("provider is not configured: {provider_id}"))
            })
    }

    /// Creates and starts a new conversation session.
    ///
    /// # Errors
    ///
    /// Returns an error when the conversation cannot be committed to durable storage.
    pub async fn create_session(&self, options: NewSession) -> Result<SessionHandle, RuntimeError> {
        self.create_session_named(options, None).await
    }

    /// Creates a session with non-persistent MCP servers supplied by a frontend.
    pub async fn create_session_with_mcp(
        &self,
        options: NewSession,
        mcp_servers: std::collections::BTreeMap<String, crate::McpServerDefinition>,
    ) -> Result<SessionHandle, RuntimeError> {
        self.create_session_named_with_mcp(options, None, mcp_servers)
            .await
    }

    /// Creates a frontend session with additional workspace roots and ephemeral MCP servers.
    pub async fn create_frontend_session(
        &self,
        options: NewSession,
        additional_directories: Vec<std::path::PathBuf>,
        mcp_servers: std::collections::BTreeMap<String, crate::McpServerDefinition>,
    ) -> Result<SessionHandle, RuntimeError> {
        self.create_session_named_with_options(options, None, additional_directories, mcp_servers)
            .await
    }

    /// Clones one conversation branch into a new standalone session.
    ///
    /// The source is left unchanged. A user-message target follows ordinary fork semantics: its
    /// parent becomes the cloned tip so a frontend can restore that message as an editable draft.
    pub async fn hard_fork_session(
        &self,
        source: &SessionHandle,
        at: crate::NodeId,
    ) -> Result<SessionHandle, RuntimeError> {
        if !self.persist_conversations {
            return Err(RuntimeError::InvalidOption(
                "hard fork requires persistent conversation storage".into(),
            ));
        }
        let config = self.config_store.snapshot();
        let new_id = ConversationId::new_with_machine_fingerprint(config.machine_fingerprint());
        let planning_modes = config
            .modes()?
            .into_values()
            .filter(|mode| mode.plan)
            .map(|mode| mode.name)
            .collect();
        source
            .store
            .hard_fork(
                source.id(),
                new_id,
                at,
                self.conversation_path(new_id),
                planning_modes,
            )
            .await?;
        if self.publish_conversations {
            self.global_store
                .project_conversation(self.conversation_path(new_id))
                .await?;
        }
        self.resume_session(new_id).await
    }

    /// Creates and starts a new conversation session with an optional initial name.
    ///
    /// The name is initialized as part of the session creation transaction, so automatic title
    /// generation will not replace it.
    #[tracing::instrument(
        level = "info",
        name = "agent.session.create",
        skip_all,
        fields(workspace = %options.workspace.display(), session_id = tracing::field::Empty)
    )]
    pub async fn create_session_named(
        &self,
        options: NewSession,
        name: Option<String>,
    ) -> Result<SessionHandle, RuntimeError> {
        self.create_session_named_with_options(options, name, Vec::new(), Default::default())
            .await
    }

    async fn create_session_named_with_mcp(
        &self,
        options: NewSession,
        name: Option<String>,
        session_mcp_servers: std::collections::BTreeMap<String, crate::McpServerDefinition>,
    ) -> Result<SessionHandle, RuntimeError> {
        self.create_session_named_with_options(options, name, Vec::new(), session_mcp_servers)
            .await
    }

    async fn create_session_named_with_options(
        &self,
        options: NewSession,
        name: Option<String>,
        additional_directories: Vec<std::path::PathBuf>,
        session_mcp_servers: std::collections::BTreeMap<String, crate::McpServerDefinition>,
    ) -> Result<SessionHandle, RuntimeError> {
        let config = self.config_store.snapshot();
        let conversation_id =
            ConversationId::new_with_machine_fingerprint(config.machine_fingerprint());
        tracing::Span::current().record("session_id", tracing::field::display(conversation_id));
        tracing::info!(workspace = %options.workspace.display(), "creating session");
        let tools = {
            let _span = tracing::trace_span!(
                "agent.session.create.phase",
                phase = "workspace_tools_initialized"
            )
            .entered();
            ReadOnlyTools::new(&options.workspace)
                .and_then(|tools| tools.with_allowed_roots(&additional_directories))
                .map_err(|error| RuntimeError::InvalidOption(error.to_string()))?
                .with_attachment_limits(
                    config.attachment_bytes(),
                    config.attachment_hard_cap_bytes(),
                )
        };
        let options = NewSession {
            workspace: tools.workspace().to_path_buf(),
        };
        let project_dir = options.workspace.clone();
        let scratchpad = self
            .persist_conversations
            .then(|| crate::scratchpad::ensure(&self.temporary_dir, &project_dir, conversation_id))
            .transpose()?;
        // A newly created session hydrates composer recall only once. Capture
        // the conversations already open in this process so that hydration
        // can project their latest canonical state before reading global.db.
        // Otherwise an immediate `/new` can race the projection watcher and
        // permanently miss the command that opened the preceding turn.
        let hydration_projection_paths = if self.publish_conversations {
            self.sessions
                .lock()
                .await
                .keys()
                .map(|id| self.conversation_path(*id))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let mut selection = self
            .global_selection
            .read()
            .map_err(|_| RuntimeError::RuntimeStopped)?
            .clone();
        let agent_catalog = config.agent_catalog()?;
        let agent = agent_catalog.get(config.default_agent()).ok_or_else(|| {
            RuntimeError::InvalidOption("configured default agent disappeared".into())
        })?;
        if !agent.enabled || !agent.availability.user_selectable() {
            return Err(RuntimeError::InvalidOption(format!(
                "default agent {} is disabled or not user-selectable",
                config.default_agent()
            )));
        }
        let mode_profiles = config.enabled_modes()?;
        let mode_catalog = mode_profiles
            .iter()
            .map(|mode| (mode.name.clone(), mode.clone()))
            .collect::<std::collections::BTreeMap<_, _>>();
        let global = selection.clone();
        apply_inherited_profile_selection(&mut selection, agent, &mode_catalog, &global);
        let model_selection = selection.model.as_ref().map(|model| {
            (
                model.provider.clone(),
                model.model.clone(),
                selection.effort.clone(),
            )
        });
        let plan_model_selection = selection.plan_model.as_ref().map(|model| {
            (
                model.provider.clone(),
                model.model.clone(),
                selection.plan_effort.clone(),
            )
        });
        let (store, access) = if self.persist_conversations {
            StoreHandle::open_conversation(
                &self.conversation_path(conversation_id),
                &self.writer_lock_path(conversation_id),
                &self.open_lock_path(conversation_id),
                self.command_capacity,
                self.event_capacity,
            )
            .instrument(tracing::trace_span!(
                "agent.session.create.phase",
                phase = "conversation_store_opened"
            ))
            .await?
        } else {
            (
                StoreHandle::open_in_memory(self.command_capacity, self.event_capacity).await?,
                crate::SessionAccess::Owner,
            )
        };
        let created = store
            .create_initialized_session(
                conversation_id,
                options,
                name,
                config.default_agent().into(),
                config.default_mode().into(),
                model_selection,
                plan_model_selection,
                selection
                    .mode_selections
                    .iter()
                    .filter_map(|(mode, value)| {
                        value.model.as_ref().map(|model| {
                            (
                                mode.clone(),
                                model.provider.clone(),
                                model.model.clone(),
                                value.effort.clone(),
                            )
                        })
                    })
                    .collect(),
            )
            .await?;
        let projection_path = self.conversation_path(created.id);
        if self.publish_conversations {
            let projection_store = self.global_store.clone();
            let projection_path_for_task = projection_path.clone();
            tokio::spawn(
                async move {
                    if let Err(error) = projection_store
                        .project_conversation(projection_path_for_task)
                        .await
                    {
                        tracing::debug!(%error, "initial conversation projection deferred");
                    }
                }
                .instrument(tracing::info_span!(
                    "agent.session.projection",
                    session_id = %created.id
                )),
            );
            self.global_store.start_projection_watcher(projection_path);
        }
        let session = {
            let _spawn =
                tracing::trace_span!("agent.session.create.phase", phase = "session_spawn")
                    .entered();
            self.spawn_session(
                store,
                access,
                created.id,
                tools,
                project_dir,
                None,
                selection,
                SessionProfiles {
                    agent: config.default_agent().into(),
                    mode: config.default_mode().into(),
                },
                Vec::new(),
                None,
                scratchpad,
                session_mcp_servers,
            )?
        };
        self.sessions
            .lock()
            .await
            .insert(created.id, session.clone());
        let hydration_history = session.composer_history.clone();
        let hydration_transient = session.transient.clone();
        let global_store = self.global_store.clone();
        tokio::spawn(
            async move {
                for path in hydration_projection_paths {
                    if let Err(error) = global_store.project_conversation(path).await {
                        tracing::debug!(%error, "composer seed projection deferred");
                    }
                }
                match global_store
                    .load_composer_seed()
                    .instrument(tracing::trace_span!(
                        "agent.session.hydration.phase",
                        phase = "global_composer_seed"
                    ))
                    .await
                {
                    Ok(records) => {
                        merge_composer_seed(&hydration_history, &hydration_transient, records)
                    }
                    Err(error) => tracing::debug!(%error, "composer seed hydration deferred"),
                }
            }
            .instrument(tracing::info_span!(
                "agent.session.composer_hydration",
                session_id = %created.id
            )),
        );
        Ok(session)
    }

    /// Resumes an existing conversation session.
    ///
    /// # Errors
    ///
    /// Returns an error when the conversation does not exist or storage is unavailable.
    #[tracing::instrument(
        level = "info",
        name = "agent.session.resume",
        skip_all,
        fields(session_id = %id)
    )]
    pub async fn resume_session(&self, id: ConversationId) -> Result<SessionHandle, RuntimeError> {
        if let Some(session) = self.sessions.lock().await.get(&id).cloned() {
            return Ok(session);
        }
        let config = self.config_store.snapshot();
        let (store, access) = StoreHandle::open_conversation(
            &self.conversation_path(id),
            &self.writer_lock_path(id),
            &self.open_lock_path(id),
            self.command_capacity,
            self.event_capacity,
        )
        .await?;
        if self.publish_conversations {
            self.global_store
                .start_projection_watcher(self.conversation_path(id));
        }
        let resumed = store.load_resumed_session(id).await?;
        let project_dir = resumed.project_dir.clone();
        let mut resumed_worktree = resumed.worktree.clone();
        let workspace = if resumed.cwd.is_dir() {
            resumed.cwd.clone()
        } else if resumed.project_dir.is_dir() {
            let recovered = crate::runtime::worktrees::WorkspaceTransition {
                project_dir: resumed.project_dir.clone(),
                previous_cwd: resumed.cwd.clone(),
                cwd: resumed.project_dir.clone(),
                reason: "missing_cwd_recovered".into(),
                vcs: None,
                name: None,
                branch_or_workspace: None,
                base: None,
                created: false,
                warning: Some(format!(
                    "Recorded working directory {} no longer exists; returned to the project directory.",
                    resumed.cwd.display()
                )),
                worktree: None,
            };
            store.append_workspace_transition(id, recovered).await?;
            resumed_worktree = None;
            resumed.project_dir.clone()
        } else {
            return Err(RuntimeError::InvalidOption(format!(
                "recorded working directory and project directory are both missing: {} and {}",
                resumed.cwd.display(),
                resumed.project_dir.display()
            )));
        };
        let tools = ReadOnlyTools::new(&workspace)
            .map_err(|error| RuntimeError::InvalidOption(error.to_string()))?
            .with_attachment_limits(
                config.attachment_bytes(),
                config.attachment_hard_cap_bytes(),
            );
        let mut selection = resumed.model_selection.map_or_else(
            || SessionSelection {
                model: default_model(&config),
                effort: default_effort(&config),
                allow_disabled_provider: None,
                plan_model: None,
                plan_effort: None,
                normal_manual: false,
                plan_manual: false,
                mode_selections: std::collections::BTreeMap::new(),
            },
            |(provider, model, effort, source)| SessionSelection {
                model: Some(ModelRef { provider, model }),
                effort,
                allow_disabled_provider: None,
                plan_model: None,
                plan_effort: None,
                normal_manual: source != "inherited",
                plan_manual: false,
                mode_selections: std::collections::BTreeMap::new(),
            },
        );
        for (mode_name, stored) in resumed.mode_selections {
            if let Some((provider, model, effort, source)) = stored {
                selection.mode_selections.insert(
                    mode_name,
                    ModeSelection {
                        model: Some(ModelRef { provider, model }),
                        effort,
                        manual: source != "inherited",
                    },
                );
            }
        }
        if let Some((provider, model, effort, source)) = resumed.plan_model_selection {
            selection.plan_model = Some(ModelRef { provider, model });
            selection.plan_effort = effort;
            selection.plan_manual = source != "inherited";
            selection
                .mode_selections
                .entry("plan".into())
                .or_insert(ModeSelection {
                    model: selection.plan_model.clone(),
                    effort: selection.plan_effort.clone(),
                    manual: selection.plan_manual,
                });
        }
        let (agent, mut mode) = resumed.active_profiles;
        let agent_catalog = config.agent_catalog()?;
        let agent_profile = agent_catalog
            .get(&agent)
            .or_else(|| agent_catalog.get(config.default_agent()))
            .ok_or_else(|| RuntimeError::InvalidOption("stored agent profile disappeared".into()))?
            .clone();
        let modes = config
            .modes()?
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>();
        let global = self
            .global_selection
            .read()
            .map_err(|_| RuntimeError::RuntimeStopped)?
            .clone();
        apply_inherited_profile_selection(&mut selection, &agent_profile, &modes, &global);
        if config.enabled_mode(&mode).is_err() {
            mode = config.default_mode().into();
            store
                .set_active_profile(id, None, Some(mode.clone()), false)
                .await?;
        }
        let mut composer_by_id = std::collections::BTreeMap::new();
        for record in self
            .global_store
            .load_composer_seed()
            .await?
            .into_iter()
            .chain(resumed.composer_history)
        {
            composer_by_id.insert(record.entry_id.clone(), record);
        }
        let mut composer_records = composer_by_id.into_values().collect::<Vec<_>>();
        composer_records.sort_by(|left, right| {
            (&left.created_at, &left.entry_id).cmp(&(&right.created_at, &right.entry_id))
        });
        let composer_history = composer_records
            .into_iter()
            .map(crate::store::ComposerHistoryRecord::into_entry)
            .collect();
        let profiles = SessionProfiles { agent, mode };
        let reopened_plan =
            plan_completion_for_resumed_tip(&config, &profiles, resumed.completed_active_plan)?;
        let scratchpad = crate::scratchpad::ensure(&self.temporary_dir, &project_dir, id)?;
        let session = self.spawn_session(
            store,
            access,
            id,
            tools,
            project_dir,
            resumed_worktree,
            selection,
            profiles,
            composer_history,
            reopened_plan,
            Some(scratchpad),
            Default::default(),
        )?;
        let mut sessions = self.sessions.lock().await;
        Ok(sessions
            .entry(id)
            .or_insert_with(|| session.clone())
            .clone())
    }

    /// Lists durable conversations, newest first. When `workspace` is set,
    /// only conversations whose canonical workspace matches are returned.
    ///
    /// # Errors
    ///
    /// Returns a database error when conversation metadata cannot be loaded.
    pub async fn conversations(
        &self,
        workspace: Option<&std::path::Path>,
    ) -> Result<Vec<crate::ConversationSummary>, RuntimeError> {
        self.sync_open_projections().await;
        self.global_store
            .conversations(crate::ConversationQuery {
                workspace: workspace.map(std::path::Path::to_path_buf),
                search: None,
                include_archived: false,
            })
            .await
    }

    /// Marks a durable conversation archived or active.
    pub async fn set_conversation_archived(
        &self,
        id: ConversationId,
        archived: bool,
    ) -> Result<(), RuntimeError> {
        let session = self.resume_session(id).await?;
        session
            .store
            .set_conversation_archived(id, archived)
            .await?;
        self.global_store
            .project_conversation(self.conversation_path(id))
            .await
    }

    /// Marks a durable conversation as a favourite or removes it from favourites.
    pub async fn set_conversation_favourite(
        &self,
        id: ConversationId,
        favourite: bool,
    ) -> Result<(), RuntimeError> {
        let session = self.resume_session(id).await?;
        session
            .store
            .set_conversation_favourite(id, favourite)
            .await?;
        self.global_store
            .project_conversation(self.conversation_path(id))
            .await
    }

    /// Permanently removes a conversation and its rebuildable global metadata.
    pub async fn delete_conversation(&self, id: ConversationId) -> Result<(), RuntimeError> {
        let session = { self.sessions.lock().await.remove(&id) };
        if let Some(session) = session {
            session.end().await?;
            drop(session);
        }
        let path = self.conversation_path(id);
        tokio::fs::remove_file(&path).await.map_err(|error| {
            RuntimeError::InvalidOption(format!("failed to delete {}: {error}", path.display()))
        })?;
        let _ = tokio::fs::remove_file(format!("{}-wal", path.display())).await;
        let _ = tokio::fs::remove_file(format!("{}-shm", path.display())).await;
        self.global_store.delete_conversation(id).await?;
        let root = self.temporary_dir.clone();
        if let Err(error) =
            tokio::task::spawn_blocking(move || crate::scratchpad::remove_conversation(&root, id))
                .await
                .map_err(|_| RuntimeError::RuntimeStopped)?
        {
            tracing::warn!(%id, %error, "failed to remove conversation scratchpad");
        }
        Ok(())
    }

    /// Queries durable conversations by canonical workspace and/or normalized
    /// title and committed-user-message text.
    pub async fn query_conversations(
        &self,
        query: crate::ConversationQuery,
    ) -> Result<Vec<crate::ConversationSummary>, RuntimeError> {
        self.sync_open_projections().await;
        self.global_store.conversations(query).await
    }

    /// Resolves a conversation ID or title query for resuming.
    ///
    /// IDs are returned directly. Title lookup prefers an exact normalized
    /// title, then the most recently updated title containing the query.
    /// Archived conversations are eligible because explicit resume supports them.
    pub async fn resolve_conversation(
        &self,
        selector: &str,
    ) -> Result<ConversationId, RuntimeError> {
        if let Ok(id) = selector.parse() {
            return Ok(id);
        }
        self.sync_open_projections().await;
        self.global_store
            .conversation_by_title(selector.to_owned())
            .await?
            .ok_or_else(|| {
                RuntimeError::InvalidOption(format!("no conversation title matches {selector:?}"))
            })
    }

    async fn sync_open_projections(&self) {
        if !self.publish_conversations {
            return;
        }
        let ids = self
            .sessions
            .lock()
            .await
            .keys()
            .copied()
            .collect::<Vec<_>>();
        for id in ids {
            if let Err(error) = self
                .global_store
                .project_conversation(self.conversation_path(id))
                .await
            {
                tracing::debug!(%id, %error, "same-process conversation projection deferred");
            }
        }
    }

    /// Resumes the most recently updated non-blank conversation for `workspace`.
    ///
    /// # Errors
    ///
    /// Returns an error when no matching conversation exists or its durable state cannot be loaded.
    pub async fn continue_session(
        &self,
        workspace: &std::path::Path,
    ) -> Result<SessionHandle, RuntimeError> {
        let conversation = self
            .conversations(Some(workspace))
            .await?
            .into_iter()
            .find(|conversation| !conversation.is_untitled())
            .ok_or_else(|| {
                RuntimeError::InvalidOption(format!(
                    "no resumable conversation exists for workspace {}",
                    workspace.display()
                ))
            })?;
        self.resume_session(conversation.id).await
    }

    /// Resumes the most recently updated non-blank conversation in a named worktree.
    ///
    /// The launch workspace remains the immutable project scope; `worktree_name`
    /// selects among conversations in that project by their recorded metadata.
    pub async fn continue_session_in_worktree(
        &self,
        workspace: &std::path::Path,
        worktree_name: &str,
    ) -> Result<SessionHandle, RuntimeError> {
        let candidates = self.conversations(Some(workspace)).await?;
        for conversation in candidates
            .into_iter()
            .filter(|conversation| !conversation.is_untitled())
        {
            let path = self.conversation_path(conversation.id);
            let metadata =
                tokio::task::spawn_blocking(move || crate::store::read_conversation_paths(&path))
                    .await
                    .map_err(|error| {
                        RuntimeError::InvalidOption(format!(
                            "failed to inspect conversation worktree metadata: {error}"
                        ))
                    })??;
            if metadata
                .2
                .as_ref()
                .is_some_and(|worktree| worktree.name == worktree_name)
            {
                return self.resume_session(conversation.id).await;
            }
        }
        Err(RuntimeError::InvalidOption(format!(
            "no resumable conversation exists for worktree {worktree_name:?} in workspace {}",
            workspace.display()
        )))
    }

    #[allow(clippy::too_many_lines)]
    fn spawn_session(
        &self,
        store: StoreHandle,
        access: crate::SessionAccess,
        id: ConversationId,
        tools: ReadOnlyTools,
        project_dir: std::path::PathBuf,
        worktree: Option<crate::WorktreeMetadata>,
        selection: SessionSelection,
        profiles: SessionProfiles,
        composer_history: Vec<crate::ComposerHistoryEntry>,
        reopened_plan: Option<crate::InteractionRequest>,
        scratchpad: Option<std::path::PathBuf>,
        session_mcp_servers: std::collections::BTreeMap<String, crate::McpServerDefinition>,
    ) -> Result<SessionHandle, RuntimeError> {
        let config = self.config_store.snapshot();
        let (transient, _) = broadcast::channel(self.event_capacity);
        let mut startup_updates = self.startup.subscribe();
        let startup_transient = transient.clone();
        tokio::spawn(
            async move {
                while startup_updates.changed().await.is_ok() {
                    let _ =
                        startup_transient.send(crate::TransientEvent::StartupResourcesUpdated {
                            status: startup_updates.borrow_and_update().clone(),
                        });
                }
            }
            .instrument(tracing::info_span!(
                "agent.session.startup_updates",
                session_id = %id
            )),
        );
        let mut config_changes = self.config_store.subscribe_changes();
        let config_transient = transient.clone();
        let config_audit_store = store.clone();
        let session_shutdown = CancellationToken::new();
        let config_shutdown = session_shutdown.clone();
        tokio::spawn(async move {
            loop {
                let change = tokio::select! {
                    _ = config_shutdown.cancelled() => break,
                    change = config_changes.recv() => match change {
                        Ok(change) => change,
                        Err(_) => break,
                    },
                };
                let event = match change {
                    crate::ConfigChange::Changed {
                        path,
                        source: crate::ConfigChangeSource::External,
                        ..
                    } => {
                        let config_audit_store = config_audit_store.clone();
                        let span_path = path.clone();
                        async move {
                            if let Err(error) = config_audit_store
                                .append_configuration_changed(id, path.clone())
                                .await
                            {
                                tracing::error!(%id, %error, "failed to record configuration reload audit");
                            }
                            tracing::info!(%id, path = ?path, "configuration reloaded");
                            crate::TransientEvent::ConfigurationChanged { path }
                        }
                        .instrument(tracing::trace_span!(
                            "agent.config.reload",
                            session_id = %id,
                            path = ?span_path
                        ))
                        .await
                    }
                    crate::ConfigChange::Changed { .. } => continue,
                    crate::ConfigChange::Rejected { path, message } => {
                        crate::TransientEvent::ConfigurationRejected { path, message }
                    }
                };
                let _ = config_transient.send(event);
            }
        }
        .instrument(tracing::info_span!(
            "agent.session.config_updates",
            session_id = %id
        )));
        let permission_file = self
            .permissions_path
            .clone()
            .map(|path| crate::PermissionFile::new(path, tools.workspace()))
            .transpose()?;
        let trusted = permission_file
            .as_ref()
            .map(crate::PermissionFile::is_trusted)
            .transpose()?
            .unwrap_or(false)
            || self.temporary_workspace_trust;
        let mcp_config = crate::McpConfigService::from_store(
            self.config_store.clone(),
            self.config_fallback_path.clone(),
            tools.workspace(),
            trusted,
        )?
        .with_session_servers(session_mcp_servers)?;
        let eager_supervisor = self.mcp_supervisor.clone();
        let eager_config = mcp_config.clone();
        let eager_agent = profiles.agent.clone();
        let eager_transient = transient.clone();
        tokio::spawn(
            async move {
                if let Err(error) = eager_supervisor
                    .start_eager(&eager_config, &eager_agent)
                    .await
                {
                    tracing::warn!(%error, "failed to start eager MCP servers");
                }
                if let Ok(servers) = eager_supervisor.describe(&eager_config, &eager_agent).await {
                    if servers.is_empty() {
                        return;
                    }
                    for server in &servers {
                        let _ = eager_transient.send(crate::TransientEvent::McpStatusUpdated {
                            server: server.name.clone(),
                            status: server.status.clone(),
                        });
                    }
                    let _ =
                        eager_transient.send(crate::TransientEvent::McpCatalogUpdated { servers });
                }
            }
            .instrument(tracing::info_span!(
                "agent.session.eager_mcp",
                session_id = %id
            )),
        );
        let (tool_approval_tx, tool_approval_rx) = mpsc::channel(self.command_capacity);
        let filesystem_approval_lock = Arc::new(tokio::sync::Mutex::new(()));
        let live_turn = Arc::new(std::sync::RwLock::new(None));
        let live_context = Arc::new(std::sync::RwLock::new(None));
        let latest_model_request = Arc::new(std::sync::RwLock::new(None));
        let provider_usage = Arc::new(std::sync::RwLock::new(ProviderUsageState::default()));
        let shell = {
            let _span =
                tracing::trace_span!("agent.session.spawn.phase", phase = "shell_inventory")
                    .entered();
            crate::ShellExecutor::new(tools.workspace(), config.provider_credential_variables())
                .map(|shell| {
                    shell
                        .with_protected_credential_variables(
                            config.web_search().credential_variables().into(),
                        )
                        .with_output_limits(
                            config.shell().output_bytes,
                            config.shell().buffer_bytes,
                        )
                        .with_default_timeout(config.shell().timeout_seconds)
                        .with_terminal_mode(config.shell().terminal_mode)
                })
                .map_err(|error| RuntimeError::InvalidOption(error.to_string()))?
        };
        shell.start_inventory_probe_for_session(Some(id));
        let work_completions = Arc::new(tokio::sync::Notify::new());
        let terminals = {
            let _span = tracing::trace_span!(
                "agent.session.spawn.phase",
                phase = "terminal_supervisor_started"
            )
            .entered();
            supervised::start_terminal_supervisor(
                id,
                &shell,
                store.clone(),
                transient.clone(),
                work_completions.clone(),
                session_shutdown.clone(),
            )
        };
        let mut mutations = crate::MutationTools::new(tools.workspace())
            .expect("read-only tools already canonicalized the workspace");
        for root in tools.allowed_roots() {
            mutations = mutations
                .with_allowed_root(root)
                .expect("additional workspace roots were already canonicalized");
        }
        if let Some(scratchpad) = scratchpad.as_deref() {
            mutations = mutations
                .with_allowed_root(scratchpad)
                .expect("scratchpad was already canonicalized");
        }
        let mutations = mutations.with_diff_context_lines(config.diff_context_lines());
        let startup_context = self.startup.instructions_for_dispatch().current();
        let session_context = self
            .startup
            .instruction_paths
            .as_ref()
            .and_then(|(config_dir, _)| {
                crate::LocalContextPaths::resolve(
                    config_dir.clone(),
                    tools.workspace().to_path_buf(),
                )
                .ok()
            })
            .and_then(|paths| {
                crate::LocalContextSnapshot::load(
                    &paths,
                    config.external_agents_compatibility(),
                    config.bundled_skills_enabled(),
                    Some(&startup_context),
                )
                .map(|snapshot| snapshot.with_disabled_skills(config.disabled_skills()))
                .ok()
            })
            .unwrap_or(startup_context);
        let session_instructions = crate::InstructionSnapshot::from_snapshot(session_context);
        let frontend_file_system = Arc::new(std::sync::RwLock::new(None));
        let delegation = DelegationRuntime::new(
            store.clone(),
            self.providers.clone(),
            self.catalog.clone(),
            config.clone(),
            session_instructions.clone(),
            transient.clone(),
            work_completions.clone(),
            session_shutdown.clone(),
        )
        .with_permission_requests(
            permission_file.clone(),
            tool_approval_tx.clone(),
            tools.workspace().to_path_buf(),
            filesystem_approval_lock.clone(),
        )?
        .with_mcp(mcp_config.clone(), self.mcp_supervisor.clone())
        .with_execution(DelegatedExecution {
            mutations: mutations.clone(),
            config_store: self.config_store.clone(),
            shell: shell.clone(),
            terminals: terminals.clone(),
            live_turn: live_turn.clone(),
            frontend_file_system: frontend_file_system.clone(),
        });
        let delegated_live = delegation.live_state();
        let frontend_terminal = Arc::new(std::sync::RwLock::new(None));
        let tool_runtime = ToolRuntime {
            store: store.clone(),
            providers: self.providers.clone(),
            conversation_id: Some(id),
            workspace_state: Arc::new(std::sync::RwLock::new(WorkspaceState {
                project_dir,
                cwd: tools.workspace().to_path_buf(),
                worktree,
                tools: tools.clone(),
                mutations: mutations.clone(),
                permission_file: permission_file.clone(),
                shell: shell.clone(),
                mcp_config: mcp_config.clone(),
                local_context: session_instructions.current(),
                path_completion: PathCompletionSession::new(
                    tools.clone(),
                    self.config_store.clone(),
                ),
                scratchpad,
            })),
            permission_file: permission_file.clone(),
            session_permission_rules: delegation.session_permission_rules.clone(),
            config_store: self.config_store.clone(),
            instructions: session_instructions,
            instruction_config_dir: self
                .startup
                .instruction_paths
                .as_ref()
                .map(|(config_dir, _)| config_dir.clone()),
            approvals: tool_approval_tx.clone(),
            filesystem_approval_lock: filesystem_approval_lock.clone(),
            terminals: terminals.clone(),
            frontend_file_system: frontend_file_system.clone(),
            frontend_terminal: frontend_terminal.clone(),
            transient: transient.clone(),
            live_turn: live_turn.clone(),
            live_context: live_context.clone(),
            latest_model_request: latest_model_request.clone(),
            mcp_supervisor: self.mcp_supervisor.clone(),
            tool_policy: None,
            web_search_credentials: crate::provider::CredentialStore::api_key(
                self.providers.credential_dir.as_ref(),
                "default",
                "exa",
            ),
            allow_workspace_transitions: true,
            pending_workspace_transitions: Arc::new(std::sync::Mutex::new(HashMap::new())),
            delegation: delegation.clone(),
        };
        let session_terminals = tool_runtime.terminals.clone();
        let workspace_state = tool_runtime.workspace_state.clone();
        let session_instructions = tool_runtime.instructions.clone();
        let instruction_config_dir = tool_runtime.instruction_config_dir.clone();
        let (commands, receiver) = mpsc::channel(self.command_capacity);
        let cancel_requests = Arc::new(tokio::sync::Notify::new());
        let cancellation_requested = Arc::new(AtomicBool::new(false));
        let suppress_interrupt_notice = Arc::new(AtomicBool::new(false));
        let (interactions, _) = watch::channel(None);
        let (access, _) = watch::channel(access);
        let selection = Arc::new(std::sync::RwLock::new(selection));
        let profiles = Arc::new(std::sync::RwLock::new(profiles));
        tokio::spawn(run_session(
            id,
            store.clone(),
            self.providers.clone(),
            tools,
            self.startup.clone(),
            self.config_store.clone(),
            config.clone(),
            self.catalog.clone(),
            selection.clone(),
            profiles.clone(),
            provider_usage.clone(),
            self.global_selection.clone(),
            transient.clone(),
            interactions.clone(),
            tool_runtime,
            tool_approval_rx,
            work_completions,
            receiver,
            cancel_requests.clone(),
            cancellation_requested.clone(),
            suppress_interrupt_notice.clone(),
            reopened_plan,
            session_shutdown.clone(),
        ));
        tracing::trace!(
            phase = "session_runtime_spawned",
            session_id = %id,
            "startup milestone reached"
        );
        let takeover_runtime = AgentRuntime {
            sessions: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            ..self.clone()
        };
        Ok(SessionHandle {
            id,
            commands,
            cancel_requests,
            cancellation_requested,
            suppress_interrupt_notice,
            session_cancellation: session_shutdown,
            delegated_live,
            delegation: delegation.clone(),
            live_turn,
            live_context,
            latest_model_request,
            provider_usage,
            store,
            global_store: self.global_store.clone(),
            access,
            writer_lock_path: self.writer_lock_path(id),
            takeover_runtime,
            transient,
            interactions,
            catalog: self.catalog.clone(),
            config_store: self.config_store.clone(),
            selected_model_fast_support: Arc::new(AtomicU8::new(FAST_SUPPORT_UNKNOWN)),
            #[cfg(test)]
            shell: shell.clone(),
            providers: self.providers.clone(),
            workspace_state,
            event_capacity: self.event_capacity,
            pending_oauth: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            terminals: session_terminals,
            mcp_config,
            mcp_supervisor: self.mcp_supervisor.clone(),
            composer_history: Arc::new(std::sync::RwLock::new(composer_history)),
            latest_command: Arc::new(std::sync::RwLock::new(None)),
            startup: self.startup.clone(),
            instructions: session_instructions,
            instruction_config_dir,
            sessions: Arc::downgrade(&self.sessions),
            frontend_file_system,
            frontend_terminal,
        })
    }
}

fn composer_entry_for_command(
    command: &crate::SessionCommand,
) -> Option<crate::ComposerHistoryEntry> {
    use crate::SessionAction;

    let (text, attachment_specs, images, image_chips) = match &command.action {
        SessionAction::SubmitDraft { draft } | SessionAction::QueueDraft { draft, .. } => (
            draft.text.clone(),
            draft.attachment_specs.clone(),
            draft.images.clone(),
            draft.image_chips.clone(),
        ),
        SessionAction::SubmitInput { text }
        | SessionAction::SubmitSpawnInput { text }
        | SessionAction::SubmitWebSearchInput { text }
        | SessionAction::SubmitStructuredInput { text, .. }
        | SessionAction::SubmitExecInput { text, .. }
        | SessionAction::QueueInput { text, .. } => {
            (text.clone(), Vec::new(), Vec::new(), Vec::new())
        }
        SessionAction::SubmitWithAttachments { text, attachments }
        | SessionAction::SubmitSpawnWithAttachments { text, attachments }
        | SessionAction::SubmitWebSearchWithAttachments { text, attachments }
        | SessionAction::QueueInputWithAttachments {
            text, attachments, ..
        } => (text.clone(), attachments.clone(), Vec::new(), Vec::new()),
        _ => return None,
    };
    Some(crate::ComposerHistoryEntry {
        kind: crate::ComposerInputKind::Prompt,
        text,
        attachment_specs,
        images,
        image_chips,
    })
}

impl SessionHandle {
    /// Loads recorded successful patches without inspecting files or adding messages.
    /// Observers may read this snapshot too.
    pub async fn load_conversation_diff(
        &self,
    ) -> Result<crate::tools::ConversationDiff, RuntimeError> {
        self.store.load_conversation_diff(self.id).await
    }

    /// The latest configured default; explicit diff commands do not change it.
    pub fn diff_mode(&self) -> crate::config::UiDiffMode {
        self.config_store.snapshot().diff_mode()
    }

    /// Resets tracking only. Tools persisted after this serialized operation belong
    /// to the new tracking period even if they started before the clear.
    pub async fn clear_conversation_diff(&self) -> Result<(), RuntimeError> {
        self.ensure_owner()?;
        self.store.clear_conversation_diff(self.id).await
    }

    /// Installs a protocol-neutral filesystem backend for authorized text writes.
    pub fn install_frontend_file_system(
        &self,
        filesystem: Arc<dyn crate::frontend::FrontendFileSystem>,
    ) -> Result<(), RuntimeError> {
        *self
            .frontend_file_system
            .write()
            .map_err(|_| RuntimeError::RuntimeStopped)? = Some(filesystem);
        Ok(())
    }

    /// Installs a protocol-neutral terminal backend for ordinary model `bash` calls.
    pub fn install_frontend_terminal(
        &self,
        terminal: Arc<dyn crate::frontend::FrontendTerminal>,
    ) -> Result<(), RuntimeError> {
        *self
            .frontend_terminal
            .write()
            .map_err(|_| RuntimeError::RuntimeStopped)? = Some(terminal);
        Ok(())
    }
    /// Returns current context-window usage for protocol frontends.
    pub fn context_usage(&self) -> Option<crate::ContextUsage> {
        self.live_context
            .read()
            .ok()
            .and_then(|usage| usage.clone())
    }
    /// Returns cumulative token and cost usage prepared for protocol frontends.
    pub async fn session_usage(&self) -> Result<crate::SessionUsage, RuntimeError> {
        Ok(self.session_snapshot().await?.usage)
    }
    /// Replaces additional workspace roots for a live frontend session.
    pub fn install_additional_directories(
        &self,
        directories: &[std::path::PathBuf],
    ) -> Result<(), RuntimeError> {
        let mut state = self
            .workspace_state
            .write()
            .map_err(|_| RuntimeError::RuntimeStopped)?;
        state.tools = state
            .tools
            .clone()
            .with_allowed_roots(directories)
            .map_err(|error| RuntimeError::InvalidOption(error.to_string()))?;
        let config = self.config_store.snapshot();
        let mut mutations = crate::MutationTools::new(state.tools.workspace())
            .map_err(|error| RuntimeError::InvalidOption(error.to_string()))?;
        for directory in directories {
            mutations = mutations
                .with_allowed_root(directory)
                .map_err(|error| RuntimeError::InvalidOption(error.to_string()))?;
        }
        state.mutations = mutations
            .with_optional_allowed_root(state.scratchpad.as_deref())
            .map_err(|error| RuntimeError::InvalidOption(error.to_string()))?
            .with_diff_context_lines(config.diff_context_lines());
        Ok(())
    }

    /// Installs or replaces non-persistent MCP servers for this live session.
    pub async fn install_session_mcp_servers(
        &self,
        servers: std::collections::BTreeMap<String, crate::McpServerDefinition>,
    ) -> Result<(), RuntimeError> {
        self.mcp_config.clone().with_session_servers(servers)?;
        let (agent, _) = self.active_profiles().await?;
        self.mcp_supervisor
            .start_eager(&self.mcp_config, &agent)
            .await
    }
    /// Exports the exact latest provider-neutral model request as pretty JSON.
    ///
    /// The file is created under the runtime's configured data directory. A
    /// request is only available after this process has sent one to a provider.
    ///
    /// # Errors
    /// Returns an error for observer sessions, before the first model request,
    /// or when the context file cannot be serialized or written.
    pub async fn save_context(&self) -> Result<std::path::PathBuf, RuntimeError> {
        self.ensure_owner()?;
        let request = self
            .latest_model_request
            .read()
            .map_err(|_| RuntimeError::RuntimeStopped)?
            .clone()
            .ok_or_else(|| {
                RuntimeError::InvalidOption(
                    "no model request has been sent in the current runtime".into(),
                )
            })?;
        let continuation = request.response_transport_continuation.clone();
        let mut exported = serde_json::to_value(request)?;
        if let Some(continuation) = continuation
            && let Some(object) = exported.as_object_mut()
        {
            object.insert(
                "response_transport_continuation".into(),
                serde_json::to_value(continuation)?,
            );
        }
        let mut encoded = serde_json::to_vec_pretty(&exported)?;
        encoded.push(b'\n');

        let directory = self
            .takeover_runtime
            .conversation_storage_dir
            .join("contexts");
        tokio::fs::create_dir_all(&directory).await?;
        let directory = tokio::fs::canonicalize(directory).await?;
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let path = directory.join(format!(
            "{timestamp}-{}-{}.json",
            self.id,
            uuid::Uuid::now_v7()
        ));
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await?;
        tokio::io::AsyncWriteExt::write_all(&mut file, &encoded).await?;
        tokio::io::AsyncWriteExt::flush(&mut file).await?;
        Ok(path)
    }

    /// Returns global usage totals, including the current conversation.
    pub async fn usage_overview(&self) -> Result<crate::UsageOverview, RuntimeError> {
        self.global_store
            .project_conversation(self.takeover_runtime.conversation_path(self.id))
            .await?;
        self.global_store.usage_overview(self.id).await
    }

    /// Returns all-time global usage grouped by canonical project directory.
    pub async fn usage_by_project(&self) -> Result<Vec<crate::UsageBreakdown>, RuntimeError> {
        self.global_store.usage_by_project().await
    }

    /// Returns all-time global usage grouped by provider/model.
    pub async fn usage_by_model(&self) -> Result<Vec<crate::UsageBreakdown>, RuntimeError> {
        self.global_store.usage_by_model().await
    }

    /// Clears global usage history without modifying canonical conversations.
    pub async fn reset_global_usage(&self) -> Result<(), RuntimeError> {
        self.global_store.reset_usage().await
    }

    async fn apply_workspace_transition(
        &self,
        current: WorkspaceState,
        transition: crate::runtime::worktrees::WorkspaceTransition,
    ) -> Result<crate::runtime::worktrees::WorkspaceTransition, RuntimeError> {
        let config = self.config_store.snapshot();
        let tools = ReadOnlyTools::new(&transition.cwd)
            .map_err(|error| RuntimeError::InvalidOption(error.to_string()))?
            .with_attachment_limits(
                config.attachment_bytes(),
                config.attachment_hard_cap_bytes(),
            );
        let mutations = crate::MutationTools::new(&transition.cwd)
            .map_err(|error| RuntimeError::InvalidOption(error.to_string()))?
            .with_optional_allowed_root(current.scratchpad.as_deref())
            .map_err(|error| RuntimeError::InvalidOption(error.to_string()))?
            .with_diff_context_lines(config.diff_context_lines());
        let permission_file = current
            .permission_file
            .as_ref()
            .map(|file| crate::PermissionFile::new(file.path().to_path_buf(), &transition.cwd))
            .transpose()?;
        let shell =
            crate::ShellExecutor::new(&transition.cwd, config.provider_credential_variables())
                .map(|shell| {
                    shell
                        .with_protected_credential_variables(
                            config.web_search().credential_variables().into(),
                        )
                        .with_output_limits(
                            config.shell().output_bytes,
                            config.shell().buffer_bytes,
                        )
                        .with_default_timeout(config.shell().timeout_seconds)
                        .with_terminal_mode(config.shell().terminal_mode)
                })
                .map_err(|error| RuntimeError::InvalidOption(error.to_string()))?;
        shell.start_inventory_probe();
        let mcp_config = current
            .mcp_config
            .for_workspace(&transition.cwd, current.mcp_config.trusted())?;
        let local_context = if let Some(config_dir) = self.instruction_config_dir.clone() {
            let paths = crate::LocalContextPaths::resolve(config_dir, transition.cwd.clone())?;
            crate::LocalContextSnapshot::load(
                &paths,
                config.external_agents_compatibility(),
                config.bundled_skills_enabled(),
                Some(&self.instructions.current()),
            )?
            .with_disabled_skills(config.disabled_skills())
        } else {
            self.instructions.current()
        };
        let replacement = WorkspaceState {
            project_dir: current.project_dir,
            cwd: transition.cwd.clone(),
            worktree: transition.worktree.clone(),
            tools,
            mutations,
            permission_file,
            shell,
            mcp_config,
            local_context,
            path_completion: PathCompletionSession::new(
                ReadOnlyTools::new(&transition.cwd)
                    .map_err(|error| RuntimeError::InvalidOption(error.to_string()))?,
                self.config_store.clone(),
            ),
            scratchpad: current.scratchpad,
        };
        self.store
            .append_workspace_transition(self.id, transition.clone())
            .await?;
        self.terminals.set_workspace(replacement.cwd.clone());
        self.instructions.replace(replacement.local_context.clone());
        *self
            .workspace_state
            .write()
            .map_err(|_| RuntimeError::RuntimeStopped)? = replacement;
        Ok(transition)
    }

    /// Loads conversation, project, and global permission rules.
    pub fn persistent_permissions(&self) -> Result<crate::PermissionPolicy, RuntimeError> {
        let state = self
            .workspace_state
            .read()
            .map_err(|_| RuntimeError::RuntimeStopped)?;
        let mut policy = state
            .permission_file
            .as_ref()
            .ok_or_else(|| {
                RuntimeError::InvalidOption("persistent permissions file is not configured".into())
            })?
            .load()?;
        policy.session = self.store.load_conversation_permissions()?;
        Ok(policy)
    }

    /// Simulates Bash parsing and permission evaluation without executing it.
    ///
    /// # Errors
    /// Returns an error when the command cannot be parsed or active policy cannot be loaded.
    pub async fn simulate_bash_permissions(&self, command: &str) -> Result<String, RuntimeError> {
        let command = command.trim();
        if command.is_empty() {
            return Err(RuntimeError::InvalidOption(
                "usage: /permissions simulate <bash command>".into(),
            ));
        }
        let analysis = crate::analyze_shell(command)
            .map_err(|error| RuntimeError::InvalidOption(error.to_string()))?;
        let (agent, mode) = self.active_profiles().await?;
        let config = self.config_store.snapshot();
        let mode_profile = config.enabled_mode(&mode)?;
        let state = self
            .workspace_state
            .read()
            .map_err(|_| RuntimeError::RuntimeStopped)?;
        let cwd = state.cwd.clone();
        let request = crate::BashRequest {
            command: command.into(),
            cwd: Some(cwd.clone()),
            env: std::collections::BTreeMap::new(),
            forward_env: Vec::new(),
            timeout: None,
            wait: true,
        };
        let safe = crate::authorize_available_safe_bash_segments(
            &request,
            state.shell.inventory(),
            &analysis,
            config.shell().safe_level,
            config.shell().safe_write,
        );
        let mut policy = state
            .permission_file
            .as_ref()
            .map(crate::PermissionFile::load)
            .transpose()?
            .unwrap_or_default();
        policy.session = self.store.load_conversation_permissions()?;
        policy.agent = config.permission_rules("agents", &agent)?;
        policy.mode = config.permission_rules("modes", &mode)?;
        let run_default = match mode_profile.run {
            crate::RunPolicy::Allow => crate::PermissionEffect::Allow,
            crate::RunPolicy::Ask | crate::RunPolicy::Auto => crate::PermissionEffect::Ask,
            crate::RunPolicy::Deny => crate::PermissionEffect::Deny,
        };
        let mut lines = vec![
            format!("Permission simulation · {agent}/{mode}"),
            format!("Command: {command}"),
            format!(
                "Parse: valid · {} segment(s) · {} path(s){}",
                analysis.segments.len(),
                analysis.paths.len(),
                if analysis.opaque { " · opaque" } else { "" }
            ),
            format!(
                "Safe subset: {}",
                safe.as_ref()
                    .and_then(|value| value.whole.as_ref())
                    .map_or("partial or no", |_| "whole command")
            ),
        ];
        for (index, segment) in analysis.segments.iter().enumerate() {
            let safe_write =
                config.shell().safe_write && mode_profile.write == crate::WritePolicy::Allow;
            let segment_is_safe = safe.as_ref().is_some_and(|value| {
                value.allows_segment(index, config.shell().safe_level, safe_write)
            });
            let default = if segment_is_safe {
                crate::PermissionEffect::Allow
            } else {
                run_default
            };
            let resource = crate::PermissionResource {
                tool: "bash".into(),
                server: None,
                operation: None,
                path: None,
                access: Some(
                    if safe_write
                        && safe
                            .as_ref()
                            .is_some_and(|value| value.safe_write_segment(index))
                    {
                        crate::PermissionAccess::Write
                    } else if segment_is_safe {
                        crate::PermissionAccess::Read
                    } else {
                        crate::PermissionAccess::Execute
                    },
                ),
                mode: mode.clone(),
                agent: agent.clone(),
                command: segment.words.clone(),
                raw_command: Some(segment.raw.clone()),
                cwd: Some(permission_path(&cwd)),
            };
            let decision = policy.evaluate(&resource, default);
            lines.push(format!(
                "Segment {}: {:?} · {}",
                index + 1,
                decision.effect,
                decision.reason
            ));
        }
        for path in &analysis.paths {
            if path.dynamic {
                lines.push(format!("Path {}: dynamic · requires approval", path.source));
                continue;
            }
            let (resolved, outside) = state
                .mutations
                .classify_target(&cwd.join(&path.value))
                .map_err(|error| RuntimeError::InvalidOption(error.to_string()))?;
            let (access, default) = match path.access {
                crate::ShellPathAccess::Read => {
                    (crate::PermissionAccess::Read, mode_profile.read.fallback())
                }
                crate::ShellPathAccess::Write | crate::ShellPathAccess::ReadWrite => (
                    crate::PermissionAccess::Write,
                    mode_profile.write.fallback(),
                ),
            };
            let resource = crate::PermissionResource {
                tool: "bash".into(),
                server: None,
                operation: None,
                path: Some(permission_path(&resolved)),
                access: Some(access),
                mode: mode.clone(),
                agent: agent.clone(),
                command: Vec::new(),
                raw_command: None,
                cwd: Some(permission_path(&cwd)),
            };
            let decision = policy.evaluate_filesystem(&resource, outside, default);
            let explanation = crate::presentation::permission_decision_explanation(&decision);
            lines.push(format!(
                "Path {}: {}",
                resolved.display(),
                explanation.operation
            ));
            if let Some(external) = explanation.external {
                lines.push(format!("  {external}"));
            }
            lines.push(format!("  {}", explanation.result));
        }
        lines.push(
            "Simulation only: this classifies authorization and does not execute or sandbox the command."
                .into(),
        );
        Ok(lines.join("\n"))
    }

    /// Updates one persistent permission rule in its current scope.
    pub fn update_persistent_permission(
        &self,
        scope: crate::PermissionScope,
        id: &str,
        rule: crate::PermissionRule,
    ) -> Result<crate::PermissionRule, RuntimeError> {
        if scope == crate::PermissionScope::Conversation {
            let mut rule = rule;
            rule.id = id.to_owned();
            self.store.save_conversation_permission(&rule)?;
            if let Ok(mut rules) = self.delegation.session_permission_rules.write()
                && let Some(existing) = rules.iter_mut().find(|existing| existing.id == id)
            {
                *existing = rule.clone();
            }
            return Ok(rule);
        }
        let state = self
            .workspace_state
            .read()
            .map_err(|_| RuntimeError::RuntimeStopped)?;
        state
            .permission_file
            .as_ref()
            .ok_or_else(|| {
                RuntimeError::InvalidOption("persistent permissions file is not configured".into())
            })?
            .update_rule(scope, id, rule)
    }

    /// Deletes one persistent permission rule from its current scope.
    pub fn delete_persistent_permission(
        &self,
        scope: crate::PermissionScope,
        id: &str,
    ) -> Result<(), RuntimeError> {
        if scope == crate::PermissionScope::Conversation {
            self.store.delete_conversation_permission(id)?;
            if let Ok(mut rules) = self.delegation.session_permission_rules.write() {
                rules.retain(|rule| rule.id != id);
            }
            return Ok(());
        }
        let state = self
            .workspace_state
            .read()
            .map_err(|_| RuntimeError::RuntimeStopped)?;
        state
            .permission_file
            .as_ref()
            .ok_or_else(|| {
                RuntimeError::InvalidOption("persistent permissions file is not configured".into())
            })?
            .delete_rule(scope, id)
    }

    /// Creates or enters a worktree before ordinary session work begins.
    /// Frontends use this for `--worktree`; in-session model calls use the
    /// identical transition backend and durable notice shape.
    pub async fn enter_worktree(
        &self,
        request: crate::runtime::worktrees::EnterWorktreeRequest,
    ) -> Result<crate::runtime::worktrees::WorkspaceTransition, RuntimeError> {
        let current = self
            .workspace_state
            .read()
            .map_err(|_| RuntimeError::RuntimeStopped)?
            .clone();
        let worktree_config = self.config_store.snapshot().worktree().clone();
        let conversation_name = self.store.load_conversation_title(self.id).await?;
        let project_dir = current.project_dir.clone();
        let cwd = current.cwd.clone();
        let transition = tokio::task::spawn_blocking(move || {
            crate::runtime::worktrees::prepare_worktree_entry(
                &project_dir,
                &cwd,
                &request,
                &worktree_config,
                conversation_name.as_deref(),
            )
        })
        .await
        .map_err(|error| RuntimeError::InvalidOption(format!("worktree task failed: {error}")))??;
        self.apply_workspace_transition(current, transition).await
    }

    /// Changes the working directory used by future session work.
    pub async fn change_working_directory(
        &self,
        requested: impl AsRef<std::path::Path>,
    ) -> Result<crate::runtime::worktrees::WorkspaceTransition, RuntimeError> {
        let current = self
            .workspace_state
            .read()
            .map_err(|_| RuntimeError::RuntimeStopped)?
            .clone();
        let requested = crate::runtime::worktrees::expand_home_path(requested.as_ref())?;
        let project_dir = current.project_dir.clone();
        let cwd = current.cwd.clone();
        let transition = tokio::task::spawn_blocking(move || {
            // `/dir .` follows ordinary relative-path semantics. The model tool's
            // special `.` reset behavior remains in `prepare_directory_change`.
            let requested = if requested == std::path::Path::new(".") {
                cwd.clone()
            } else {
                requested
            };
            crate::runtime::worktrees::prepare_directory_change(&project_dir, &cwd, &requested)
        })
        .await
        .map_err(|error| {
            RuntimeError::InvalidOption(format!("directory task failed: {error}"))
        })??;
        self.apply_workspace_transition(current, transition).await
    }

    /// Returns the directory in which this session currently operates.
    ///
    /// # Errors
    /// Returns an error if the session workspace state is unavailable.
    pub fn working_directory(&self) -> Result<std::path::PathBuf, RuntimeError> {
        self.workspace_state
            .read()
            .map(|state| state.cwd.clone())
            .map_err(|_| RuntimeError::RuntimeStopped)
    }

    /// Archives or restores any durable conversation visible to this runtime.
    pub async fn set_conversation_archived(
        &self,
        id: ConversationId,
        archived: bool,
    ) -> Result<(), RuntimeError> {
        self.takeover_runtime
            .set_conversation_archived(id, archived)
            .await
    }

    /// Favourites or unfavourites any durable conversation visible to this runtime.
    pub async fn set_conversation_favourite(
        &self,
        id: ConversationId,
        favourite: bool,
    ) -> Result<(), RuntimeError> {
        self.takeover_runtime
            .set_conversation_favourite(id, favourite)
            .await
    }

    /// Permanently deletes a durable conversation.
    pub async fn delete_conversation(&self, id: ConversationId) -> Result<(), RuntimeError> {
        self.takeover_runtime.delete_conversation(id).await
    }

    /// Returns all discovered skills in management order, including disabled rows.
    #[must_use]
    pub fn skills(&self) -> Vec<crate::SkillMetadata> {
        let mut skills = self.startup.instructions_for_dispatch().current().skills;
        skills.sort_by(|left, right| {
            left.source
                .cmp(&right.source)
                .then_with(|| left.name.cmp(&right.name))
                .then_with(|| left.path.cmp(&right.path))
        });
        skills
    }

    /// Toggles name-based skill enablement in the global configuration.
    pub fn toggle_skill(&self, name: &str) -> Result<bool, RuntimeError> {
        self.config_store.toggle_skill(name)
    }

    /// Resolves an enabled skill command and expands its body using Claude-compatible arguments.
    pub fn resolve_skill_command(
        &self,
        command: &str,
        arguments: &str,
    ) -> Result<Option<String>, RuntimeError> {
        let name = command.trim_start_matches('/');
        let Some(skill) = self
            .skills()
            .into_iter()
            .find(|skill| skill.enabled && skill.name == name)
        else {
            return Ok(None);
        };
        let source = std::fs::read_to_string(&skill.path)?;
        let body = source
            .split_once("\n---")
            .map_or(source.as_str(), |(_, body)| {
                body.trim_start_matches(['\r', '\n'])
            });
        Ok(Some(body.replace("$ARGUMENTS", arguments)))
    }

    /// Creates a native project or global skill without replacing existing files.
    pub fn create_skill(
        &self,
        project: bool,
        name: &str,
        description: &str,
        content: &str,
    ) -> Result<std::path::PathBuf, RuntimeError> {
        if name.is_empty()
            || !name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
        {
            return Err(RuntimeError::InvalidOption(
                "skill name must contain only letters, numbers, '-' or '_'".into(),
            ));
        }
        if description.trim().is_empty() {
            return Err(RuntimeError::InvalidOption(
                "skill description is required".into(),
            ));
        }
        let snapshot = self.startup.instructions_for_dispatch().current();
        let root = if project {
            snapshot
                .workspace
                .ok_or_else(|| RuntimeError::InvalidOption("workspace is unavailable".into()))?
                .join(".cagent/skills")
        } else {
            self.config_store
                .path()
                .and_then(std::path::Path::parent)
                .ok_or_else(|| {
                    RuntimeError::InvalidOption("global config directory is unavailable".into())
                })?
                .join("skills")
        };
        let directory = root.join(name);
        let path = directory.join("SKILL.md");
        if path.exists() {
            return Err(RuntimeError::InvalidOption(format!(
                "skill already exists: {}",
                path.display()
            )));
        }
        std::fs::create_dir_all(&directory)?;
        let description = serde_json::to_string(&description.replace('\n', " "))?;
        std::fs::write(
            &path,
            format!("---\nname: {name}\ndescription: {description}\n---\n\n{content}\n"),
        )?;
        Ok(path)
    }

    /// Deletes a discovered non-system skill directory.
    pub fn delete_skill(&self, path: &std::path::Path) -> Result<(), RuntimeError> {
        let skill = self
            .skills()
            .into_iter()
            .find(|skill| skill.path == path)
            .ok_or_else(|| RuntimeError::InvalidOption("skill is no longer available".into()))?;
        if skill.source == crate::SkillSource::System {
            return Err(RuntimeError::InvalidOption(
                "system skills cannot be deleted".into(),
            ));
        }
        let directory = skill
            .path
            .parent()
            .ok_or_else(|| RuntimeError::InvalidOption("skill directory is unavailable".into()))?;
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }

    /// Lists the Git worktrees or Jujutsu workspaces for the current repository.
    pub fn worktrees(&self) -> Result<Vec<super::WorktreeInfo>, RuntimeError> {
        let state = self
            .workspace_state
            .read()
            .map_err(|_| RuntimeError::RuntimeStopped)?;
        super::worktrees::list(&state.cwd)
    }

    /// Resolves an existing named worktree, or creates it from `base` when absent.
    pub fn resolve_or_create_worktree(
        &self,
        name: &str,
        base: Option<&str>,
    ) -> Result<std::path::PathBuf, RuntimeError> {
        if name.eq_ignore_ascii_case("root") {
            return self
                .worktrees()?
                .into_iter()
                .find(|worktree| worktree.root)
                .map(|worktree| worktree.path)
                .ok_or_else(|| {
                    RuntimeError::InvalidOption("repository root was not found".into())
                });
        }
        let state = self
            .workspace_state
            .read()
            .map_err(|_| RuntimeError::RuntimeStopped)?
            .clone();
        let request = crate::runtime::worktrees::EnterWorktreeRequest {
            name: Some(name.to_owned()),
            path: None,
            base: base.map(str::to_owned),
        };
        crate::runtime::worktrees::prepare_worktree_entry(
            &state.project_dir,
            &state.cwd,
            &request,
            self.config_store.snapshot().worktree(),
            None,
        )
        .map(|transition| transition.cwd)
    }

    #[cfg(test)]
    pub(super) async fn wait_for_shell_inventory_for_test(&self) {
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            self.shell
                .wait_for_inventory(&tokio_util::sync::CancellationToken::new()),
        )
        .await
        .expect("shell inventory probe should complete")
        .expect("shell inventory wait should not be cancelled");
        assert!(self.shell.inventory().probe_succeeded);
    }

    /// Lists all settings owned by the generic settings editor.
    ///
    /// # Errors
    /// Returns an error when the configuration source cannot be read or resolved.
    pub fn settings(&self) -> Result<Vec<crate::config::SettingRow>, RuntimeError> {
        self.config_store.settings()
    }

    /// Atomically validates and saves one settings override.
    ///
    /// # Errors
    /// Returns an error for an invalid setting or persistence failure.
    pub fn save_setting(&self, key: &str, raw_value: &str) -> Result<(), RuntimeError> {
        self.config_store.save_setting(key, raw_value)
    }

    /// Removes one explicit settings override so its built-in default applies.
    ///
    /// # Errors
    /// Returns an error for an invalid key or persistence failure.
    pub fn reset_setting(&self, key: &str) -> Result<(), RuntimeError> {
        self.config_store.reset_value(key)?;
        Ok(())
    }
    /// Lists the fixed web-search providers without exposing credentials.
    pub fn web_search_providers(&self) -> Vec<crate::web_search::WebSearchProviderStatus> {
        let mut statuses = self
            .config_store
            .snapshot()
            .web_search()
            .provider_statuses();
        if let Some(chatgpt) = statuses
            .iter_mut()
            .find(|status| status.provider == crate::WebSearchProvider::Chatgpt)
        {
            chatgpt.ready = self
                .providers
                .snapshot()
                .get("chatgpt")
                .is_some_and(|entry| entry.adapter.web_search_ready());
        }
        let managed_exa = crate::provider::CredentialStore::api_key(
            self.providers.credential_dir.as_ref(),
            "default",
            "exa",
        )
        .load_api_key()
        .ok()
        .flatten()
        .is_some_and(|key| !key.value.trim().is_empty());
        if managed_exa
            && let Some(exa) = statuses
                .iter_mut()
                .find(|status| status.provider == crate::WebSearchProvider::Exa)
        {
            exa.ready = true;
        }
        statuses
    }

    /// Activates a configured web-search provider atomically.
    pub fn select_web_search_provider(
        &self,
        provider: crate::web_search::WebSearchProvider,
    ) -> Result<(), RuntimeError> {
        if !self
            .web_search_providers()
            .into_iter()
            .any(|status| status.provider == provider && status.ready)
        {
            return Err(RuntimeError::InvalidOption(format!(
                "{} web search is not set up",
                provider.label()
            )));
        }
        self.config_store.select_web_search_provider(provider)?;
        Ok(())
    }

    /// Saves a SearXNG URL and activates it atomically.
    pub fn save_searxng_url(&self, url: &str) -> Result<(), RuntimeError> {
        self.config_store.save_searxng_url(url)?;
        Ok(())
    }

    /// Removes UI-managed SearXNG configuration.
    pub fn remove_searxng_url(&self) -> Result<(), RuntimeError> {
        self.config_store.remove_searxng_url()?;
        Ok(())
    }

    /// Saves an Exa key in Cagent's credential backend and selects Exa.
    pub fn save_exa_api_key(&self, key: &str) -> Result<(), RuntimeError> {
        let key = key.trim();
        if key.is_empty() {
            return self.remove_exa_api_key();
        }
        let credentials = crate::provider::CredentialStore::api_key(
            self.providers.credential_dir.as_ref(),
            "default",
            "exa",
        );
        credentials
            .save_api_key(&crate::provider::ManagedApiKey { value: key.into() })
            .map_err(|error| RuntimeError::InvalidOption(error.to_string()))?;
        self.config_store
            .select_web_search_provider(crate::WebSearchProvider::Exa)?;
        Ok(())
    }

    /// Removes the UI-managed Exa key and clears Exa when it was active.
    pub fn remove_exa_api_key(&self) -> Result<(), RuntimeError> {
        let credentials = crate::provider::CredentialStore::api_key(
            self.providers.credential_dir.as_ref(),
            "default",
            "exa",
        );
        credentials
            .delete_api_key()
            .map_err(|error| RuntimeError::InvalidOption(error.to_string()))?;
        if self.config_store.snapshot().web_search().provider()
            == Some(crate::WebSearchProvider::Exa)
        {
            self.config_store.clear_web_search_provider()?;
        }
        Ok(())
    }
    #[must_use]
    pub fn id(&self) -> ConversationId {
        self.id
    }

    /// Acquires ownership after the prior writer exits and returns a writable
    /// handle. Calling this on an owner is idempotent.
    pub async fn takeover(&self) -> Result<SessionHandle, RuntimeError> {
        self.refresh_observer_access();
        match *self.access.borrow() {
            crate::SessionAccess::Owner => Ok(self.clone()),
            crate::SessionAccess::Observer {
                takeover_available: true,
            } => self.takeover_runtime.resume_session(self.id).await,
            crate::SessionAccess::Observer {
                takeover_available: false,
            } => Err(RuntimeError::ReadOnlyObserver(self.id)),
        }
    }

    /// Waits until the runtime's local instruction and model metadata hydration
    /// has completed. Headless frontends use this before requests whose
    /// capability decision must be deterministic at launch.
    pub async fn wait_for_startup_resources(&self) {
        self.startup.wait_for_models().await;
        let mut updates = self.startup.subscribe();
        while !updates.borrow().ready_for_dispatch() {
            if updates.changed().await.is_err() {
                break;
            }
        }
    }

    /// Attaches a frontend to this session.
    ///
    /// The returned stream is registered before its initial durable replay is
    /// filtered, so updates written while the snapshot is being hydrated are
    /// delivered after the snapshot rather than silently lost. A lagged
    /// transient subscriber receives `ResyncRequired` and should attach again.
    #[tracing::instrument(
        level = "info",
        name = "agent.session.attach",
        skip_all,
        fields(session_id = %self.id)
    )]
    pub async fn attach(&self) -> Result<super::SessionAttachment, RuntimeError> {
        self.refresh_observer_access();
        self.reload_composer_history_entries().await?;
        self.selected_model_supports_fast_mode().await?;
        let snapshot = self.session_snapshot().await?;
        if matches!(snapshot.access, crate::SessionAccess::Observer { .. }) {
            let session = self.clone();
            let mut last_cursor = snapshot.cursor;
            let mut last_access = snapshot.access;
            let (sender, receiver) = mpsc::channel(self.event_capacity);
            tokio::spawn(
                async move {
                    loop {
                        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                        session.refresh_observer_access();
                        let access = *session.access.borrow();
                        let polled = session
                            .store
                            .load_durable_cursor(session.id)
                            .instrument(tracing::trace_span!("agent.session.observer_version_poll"))
                            .await;
                        match polled {
                            Ok(cursor) => {
                                if cursor == last_cursor && access == last_access {
                                    continue;
                                }
                                let next = match session.session_snapshot().await {
                                    Ok(next) => next,
                                    Err(error) => {
                                        let _ = sender.send(Err(error)).await;
                                        break;
                                    }
                                };
                                last_cursor = next.cursor;
                                last_access = next.access;
                                if sender
                                    .send(Ok(crate::SessionUpdate {
                                        origin: None,
                                        cursor: next.cursor,
                                        durability: crate::UpdateDurability::Durable,
                                        notice: None,
                                        history_changed: true,
                                        kind: crate::SessionUpdateKind::Snapshot(next),
                                    }))
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            Err(error) => {
                                let _ = sender.send(Err(error)).await;
                                break;
                            }
                        }
                    }
                }
                .instrument(tracing::info_span!(
                    "agent.session.observer_updates",
                    session_id = %self.id
                )),
            );
            return Ok(super::SessionAttachment {
                snapshot,
                updates: super::SessionUpdateStream {
                    inner: ReceiverStream::new(receiver),
                    pending: None,
                },
            });
        }
        let after = snapshot.cursor;
        // This consumer projects snapshots, not legacy event documents.
        let mut events = self.subscribe_inner(after, false);
        let session = self.clone();
        let (sender, receiver) = mpsc::channel(self.event_capacity);
        tokio::spawn(
            async move {
                let render_interval = std::time::Duration::from_millis(16);
                let mut last_tail_snapshot = tokio::time::Instant::now() - render_interval;
                let mut covered_cursor = after;
                while let Some(event) = events.next().await {
                    match event {
                        Ok(event) => {
                            let history_changed = match &event {
                                crate::RuntimeEvent::Durable(event) => matches!(
                                    event.kind,
                                    crate::DurableEventKind::NodeAppended { .. }
                                        | crate::DurableEventKind::AssistantDelta { .. }
                                        | crate::DurableEventKind::PlanStarted { .. }
                                        | crate::DurableEventKind::PlanDelta { .. }
                                        | crate::DurableEventKind::AssistantFailed { .. }
                                        | crate::DurableEventKind::ActiveNodeChanged { .. }
                                        | crate::DurableEventKind::NodeStatusChanged { .. }
                                ),
                                crate::RuntimeEvent::Transient {
                                    event: crate::TransientEvent::TerminalUpdated { .. },
                                    ..
                                } => true,
                                _ => false,
                            };
                            let streaming_delta = matches!(
                                &event,
                                crate::RuntimeEvent::Durable(crate::DurableEvent {
                                    kind: crate::DurableEventKind::AssistantDelta { .. }
                                        | crate::DurableEventKind::PlanDelta { .. },
                                    ..
                                })
                            );
                            // A snapshot already includes every committed delta
                            // through its cursor. Don't rebuild it for queued
                            // deltas it supersedes. Non-delta events still carry
                            // command/notification semantics and are never skipped.
                            if streaming_delta
                                && let crate::RuntimeEvent::Durable(event) = &event
                                && covered_cursor.is_some_and(|cursor| event.cursor.0 <= cursor.0)
                            {
                                continue;
                            }
                            let (cursor, durability, has_command_origin, notice) = match &event {
                                crate::RuntimeEvent::Durable(event) => (
                                    Some(event.cursor),
                                    crate::UpdateDurability::Durable,
                                    !matches!(
                                        event.kind,
                                        crate::DurableEventKind::ConfigurationChanged { .. }
                                    ),
                                    None,
                                ),
                                crate::RuntimeEvent::Transient { event, .. } => (
                                    None,
                                    crate::UpdateDurability::Transient,
                                    !matches!(
                                        event,
                                        crate::TransientEvent::ConfigurationChanged { .. }
                                            | crate::TransientEvent::ConfigurationRejected { .. }
                                            | crate::TransientEvent::LocalContextReloadFailed { .. }
                                            | crate::TransientEvent::ModelCatalogUpdated { .. }
                                            | crate::TransientEvent::ProviderUsageUpdated
                                            | crate::TransientEvent::ProviderAuthUpdated { .. }
                                            | crate::TransientEvent::McpCatalogUpdated { .. }
                                            | crate::TransientEvent::McpStatusUpdated { .. }
                                            | crate::TransientEvent::StartupResourcesUpdated { .. }
                                    ),
                                    match event {
                                        crate::TransientEvent::TurnCompleted => {
                                            Some(crate::SessionUpdateNotice::TurnCompleted)
                                        }
                                        crate::TransientEvent::ProviderAuthUpdated { .. } => {
                                            Some(crate::SessionUpdateNotice::ProviderAuthUpdated)
                                        }
                                        _ => None,
                                    },
                                ),
                                crate::RuntimeEvent::Interaction { .. } => {
                                    (None, crate::UpdateDurability::Transient, true, None)
                                }
                                crate::RuntimeEvent::InteractionCleared { .. } => {
                                    (None, crate::UpdateDurability::Transient, true, None)
                                }
                            };
                            let origin = has_command_origin
                                .then(|| {
                                    session
                                        .latest_command
                                        .read()
                                        .ok()
                                        .and_then(|origin| *origin)
                                })
                                .flatten();
                            if let crate::RuntimeEvent::Transient {
                                event: crate::TransientEvent::TerminalUpdated { terminal },
                                ..
                            } = &event
                            {
                                let terminal = terminal.preview();
                                let kind = if terminal.owner_agent_run_id.is_some() {
                                    Some(crate::SessionUpdateKind::DelegatedTerminal(terminal))
                                } else if terminal.read_safe.is_none()
                                    && terminal.tool_call_node_id.is_some()
                                {
                                    Some(crate::SessionUpdateKind::Terminal(
                                        crate::TerminalTranscriptUpdate {
                                            id: crate::TranscriptBlockId(format!(
                                                "detached-bash:{}",
                                                terminal.id
                                            )),
                                            terminal,
                                        },
                                    ))
                                } else {
                                    terminal.tool_call_node_id.map(|node_id| {
                                        crate::SessionUpdateKind::Terminal(
                                            crate::TerminalTranscriptUpdate {
                                                id: crate::TranscriptBlockId::derived(
                                                    "tools", node_id,
                                                ),
                                                terminal,
                                            },
                                        )
                                    })
                                };
                                if let Some(kind) = kind {
                                    if sender
                                        .send(Ok(crate::SessionUpdate {
                                            origin,
                                            cursor,
                                            durability,
                                            notice,
                                            history_changed,
                                            kind,
                                        }))
                                        .await
                                        .is_err()
                                    {
                                        break;
                                    }
                                    continue;
                                }
                            }
                            if let crate::RuntimeEvent::Transient {
                                event:
                                    crate::TransientEvent::RetryScheduled {
                                        request_id,
                                        attempt,
                                        reason,
                                        delay_millis,
                                    },
                                ..
                            } = &event
                            {
                                if sender
                                    .send(Ok(crate::SessionUpdate {
                                        origin,
                                        cursor,
                                        durability,
                                        notice,
                                        history_changed: false,
                                        kind: crate::SessionUpdateKind::RetryScheduled(
                                            crate::RetryStatusUpdate {
                                                request_id: *request_id,
                                                next_attempt: *attempt,
                                                max_attempts: 4,
                                                reason: reason.clone(),
                                                delay_millis: *delay_millis,
                                            },
                                        ),
                                    }))
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                                continue;
                            }
                            if let crate::RuntimeEvent::Transient {
                                event: crate::TransientEvent::AgentRunUpdated { run },
                                ..
                            } = &event
                            {
                                let mut run = (**run).clone();
                                run.timeline.clear();
                                run.activity.clear();
                                if sender
                                    .send(Ok(crate::SessionUpdate {
                                        origin,
                                        cursor,
                                        durability,
                                        notice,
                                        history_changed: false,
                                        kind: crate::SessionUpdateKind::DelegatedRun(
                                            crate::DelegatedRunUpdate { run },
                                        ),
                                    }))
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                                continue;
                            }
                            if let crate::RuntimeEvent::Transient {
                                event: crate::TransientEvent::AgentRunTextUpdated { id, text },
                                ..
                            } = &event
                            {
                                if sender
                                    .send(Ok(crate::SessionUpdate {
                                        origin,
                                        cursor,
                                        durability,
                                        notice,
                                        history_changed: false,
                                        kind: crate::SessionUpdateKind::DelegatedText(
                                            crate::DelegatedTextUpdate {
                                                id: *id,
                                                text: text.clone(),
                                            },
                                        ),
                                    }))
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                                continue;
                            }
                            if matches!(
                                &event,
                                crate::RuntimeEvent::Transient {
                                    event: crate::TransientEvent::ProviderUsageUpdated,
                                    ..
                                }
                            ) {
                                let report = session
                                    .provider_usage
                                    .read()
                                    .ok()
                                    .and_then(|usage| usage.report.clone());
                                if sender
                                    .send(Ok(crate::SessionUpdate {
                                        origin,
                                        cursor,
                                        durability,
                                        notice,
                                        history_changed: false,
                                        kind: crate::SessionUpdateKind::ProviderUsage(report),
                                    }))
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                                continue;
                            }
                            if let crate::RuntimeEvent::Transient {
                                event: crate::TransientEvent::StartupResourcesUpdated { status },
                                ..
                            } = &event
                            {
                                if sender
                                    .send(Ok(crate::SessionUpdate {
                                        origin,
                                        cursor,
                                        durability,
                                        notice,
                                        history_changed: false,
                                        kind: crate::SessionUpdateKind::StartupResources(
                                            status.clone(),
                                        ),
                                    }))
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                                continue;
                            }
                            if streaming_delta {
                                tokio::time::sleep_until(last_tail_snapshot + render_interval)
                                    .await;
                            }
                            match session.session_snapshot().await {
                                Ok(snapshot) => {
                                    covered_cursor = snapshot.cursor;
                                    last_tail_snapshot = tokio::time::Instant::now();
                                    if sender
                                        .send(Ok(crate::SessionUpdate {
                                            origin,
                                            cursor,
                                            durability,
                                            notice,
                                            history_changed,
                                            kind: crate::SessionUpdateKind::Snapshot(snapshot),
                                        }))
                                        .await
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                                Err(error) => {
                                    let _ = sender.send(Err(error)).await;
                                    break;
                                }
                            }
                        }
                        // The legacy stream intentionally treats broadcast lag as
                        // best-effort. The new contract makes recovery explicit.
                        Err(_) => {
                            if sender
                                .send(Ok(crate::SessionUpdate {
                                    origin: None,
                                    cursor: None,
                                    durability: crate::UpdateDurability::Transient,
                                    notice: None,
                                    history_changed: true,
                                    kind: crate::SessionUpdateKind::ResyncRequired,
                                }))
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                    }
                }
            }
            .instrument(tracing::info_span!(
                "agent.session.frontend_updates",
                session_id = %self.id
            )),
        );
        tracing::trace!(
            session_id = %self.id,
            transcript_blocks = snapshot.transcript.len(),
            "attached session snapshot"
        );
        Ok(super::SessionAttachment {
            snapshot,
            updates: super::SessionUpdateStream {
                inner: ReceiverStream::new(receiver),
                pending: None,
            },
        })
    }

    fn ensure_owner(&self) -> Result<(), RuntimeError> {
        if matches!(*self.access.borrow(), crate::SessionAccess::Observer { .. }) {
            Err(RuntimeError::ReadOnlyObserver(self.id))
        } else {
            Ok(())
        }
    }

    fn refresh_observer_access(&self) {
        if !matches!(*self.access.borrow(), crate::SessionAccess::Observer { .. }) {
            return;
        }
        let takeover_available = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&self.writer_lock_path)
            .ok()
            .is_some_and(|file| {
                let available = file.try_lock_exclusive().is_ok();
                if available {
                    let _ = file.unlock();
                }
                available
            });
        self.access
            .send_replace(crate::SessionAccess::Observer { takeover_available });
    }

    /// Builds the frontend-neutral active-session projection used by `attach`.
    async fn session_snapshot(&self) -> Result<crate::SessionSnapshot, RuntimeError> {
        let hydration = self
            .store
            .load_session_hydration(self.id)
            .instrument(tracing::trace_span!("agent.session.core_hydration"))
            .await?;
        let cursor = hydration.cursor;
        let durable_turn = hydration.durable_turn.clone();
        let pending_interaction = { self.interactions.borrow().clone() };
        let live_turn = self.live_turn.write().ok().and_then(|mut stored| {
            if let Some(live) = stored.as_mut()
                && live.turn_id.is_none()
                && let crate::TurnState::Working { turn_id, .. } = &durable_turn
            {
                live.turn_id = Some(*turn_id);
            }
            stored.clone()
        });
        let has_live_turn = live_turn.is_some();
        let last_activity_at = live_turn
            .as_ref()
            .map(|live_turn| live_turn.last_activity_at.clone());
        let active_plan = live_turn
            .as_ref()
            .and_then(|live_turn| live_turn.active_plan.clone());
        let turn = projected_turn_state(
            durable_turn,
            live_turn,
            pending_interaction.is_some(),
            self.cancellation_requested.load(Ordering::Acquire),
        );
        let enabled_providers = self
            .config_store
            .snapshot()
            .providers()
            .iter()
            .filter(|(_, settings)| settings.enabled)
            .map(|(id, _)| id.clone())
            .collect();
        let queue = hydration.queue.clone();
        let held_permission =
            pending_interaction
                .as_ref()
                .and_then(|request| match &request.kind {
                    crate::InteractionRequestKind::PermissionApproval { resource, .. } => {
                        Some(resource)
                    }
                    _ => None,
                });
        let terminals = hydration.terminals;
        let tail_hydration = self
            .store
            .load_transcript_page(self.id, None)
            .instrument(tracing::trace_span!(
                "agent.transcript.page_query",
                tail = true
            ))
            .await?;
        let tail_page = {
            let _span = tracing::trace_span!("agent.transcript.node_projection").entered();
            self.project_transcript_page(
                tail_hydration,
                TranscriptPageContext::LiveTail {
                    turn: &turn,
                    terminals: &terminals,
                    agent_runs: &hydration.agent_runs,
                    held_permission,
                },
            )
        };
        let mut transcript = tail_page.blocks;
        let interruption_is_durable = trailing_interrupt_is_durable(&transcript);
        // Cancellation is requested synchronously, while its durable marker is
        // written after the provider task has stopped. Keep the identical
        // agent-owned marker in the snapshot during that short gap so the TUI
        // never has to manufacture a flashing local interruption notice.
        if self.cancellation_requested.load(Ordering::Acquire)
            && !self.suppress_interrupt_notice.load(Ordering::Acquire)
            && !matches!(turn, crate::TurnState::Idle)
            && !interruption_is_durable
        {
            insert_pending_interrupt(&mut transcript, !queue.is_empty());
        }
        let supervised_work = supervised_work_from_transcript(
            &transcript,
            hydration.agent_runs.clone(),
            terminals.clone(),
        );
        let agent_run_summaries = hydration.agent_runs;
        let delegated_live = self
            .delegated_live
            .read()
            .map_or_else(|_| Vec::new(), |runs| runs.values().cloned().collect());
        let workspace_state = self
            .workspace_state
            .read()
            .map_err(|_| RuntimeError::RuntimeStopped)?
            .clone();
        let fast = self.config_store.snapshot().fast();
        let supports_fast =
            self.selected_model_fast_support.load(Ordering::Relaxed) == FAST_SUPPORT_SUPPORTED;
        let fast_effective = fast && supports_fast;
        Ok(crate::SessionSnapshot {
            conversation_id: self.id,
            project_dir: workspace_state.project_dir,
            cwd: workspace_state.cwd,
            worktree: workspace_state.worktree,
            access: *self.access.borrow(),
            cursor,
            title: hydration.title,
            transcript: crate::TranscriptWindow::new(transcript, tail_page.older),
            turn,
            active_plan,
            last_activity_at,
            model_selection: hydration
                .model_selection
                .map(|(provider, model, effort, _)| (provider, model, effort)),
            fast,
            fast_effective,
            active_agent: hydration.active_profiles.0,
            active_mode: hydration.active_profiles.1,
            queue,
            composer_history: self.composer_history_entries(),
            usage: hydration.usage,
            context: if has_live_turn {
                self.live_context
                    .read()
                    .ok()
                    .and_then(|context| context.clone())
                    .or(hydration.context)
            } else {
                hydration.context
            },
            provider_usage: self
                .provider_usage
                .read()
                .ok()
                .and_then(|usage| usage.report.clone()),
            pending_interaction,
            // Both work projections are metadata-only. Logs are loaded separately.
            agent_runs: agent_run_summaries,
            delegated_live,
            terminals,
            supervised_work,
            enabled_providers,
            startup_resources: self.startup.snapshot(),
        })
    }

    /// Requests cancellation of the current turn without waiting behind normal commands.
    pub fn cancel(&self) {
        self.suppress_interrupt_notice
            .store(false, Ordering::Release);
        self.cancellation_requested.store(true, Ordering::Release);
        let _ = self
            .transient
            .send(crate::TransientEvent::TurnCancellationRequested);
        self.cancel_requests.notify_one();
    }

    /// Requests cancellation for an internal transition without adding an
    /// interruption notice to the conversation transcript.
    pub fn cancel_silently(&self) {
        self.suppress_interrupt_notice
            .store(true, Ordering::Release);
        self.cancellation_requested.store(true, Ordering::Release);
        self.cancel_requests.notify_one();
    }

    /// Returns the recall entries prepared when this handle was opened.
    /// New sessions receive the workspace seed; resumed sessions receive only
    /// entries created in that conversation.
    #[must_use]
    pub fn composer_history_entries(&self) -> Vec<crate::ComposerHistoryEntry> {
        self.composer_history
            .read()
            .map_or_else(|_| Vec::new(), |entries| entries.clone())
    }

    /// Reloads this conversation's durable recall entries.
    ///
    /// This intentionally excludes the cross-session seed used for a newly
    /// created handle.
    pub async fn reload_composer_history_entries(
        &self,
    ) -> Result<Vec<crate::ComposerHistoryEntry>, RuntimeError> {
        let local = self.store.load_composer_history(self.id, false).await?;
        let mut entries = self.composer_history_entries();
        for entry in local {
            entries.retain(|candidate| candidate != &entry);
            entries.push(entry);
        }
        if let Ok(mut cached) = self.composer_history.write() {
            *cached = entries.clone();
        }
        Ok(entries)
    }

    async fn run_detached_bash(&self, command: String) -> Result<(), RuntimeError> {
        let command = command.trim().to_owned();
        if command.is_empty() {
            return Err(RuntimeError::InvalidOption(
                "Bash command cannot be empty".into(),
            ));
        }
        let anchor = self
            .store
            .record_bash_command(self.id, command.clone())
            .await?;
        let request = crate::BashRequest {
            command,
            cwd: None,
            env: std::collections::BTreeMap::new(),
            forward_env: Vec::new(),
            timeout: None,
            wait: false,
        };
        let started = self
            .terminals
            .start_detached(self.id, anchor, &request)
            .map_err(|error| RuntimeError::InvalidOption(error.to_string()))?;
        let snapshot = self
            .terminals
            .snapshot(started.id)
            .map_err(|error| RuntimeError::InvalidOption(error.to_string()))?;
        self.store.upsert_terminal(snapshot).await?;
        self.reload_composer_history_entries().await?;
        Ok(())
    }

    /// Records a slash command after a frontend executes it, making it
    /// available when this conversation is resumed.
    ///
    /// # Errors
    /// Returns an error if the command cannot be durably recorded.
    pub async fn record_executed_slash_command(
        &self,
        text: impl Into<String>,
    ) -> Result<(), RuntimeError> {
        self.ensure_owner()?;
        let entry = crate::ComposerHistoryEntry {
            kind: crate::ComposerInputKind::Prompt,
            text: text.into(),
            attachment_specs: Vec::new(),
            images: Vec::new(),
            image_chips: Vec::new(),
        };
        self.store
            .record_slash_command(self.id, entry.text.clone(), None, true)
            .await?;
        if let Ok(mut entries) = self.composer_history.write()
            && entries.last() != Some(&entry)
        {
            entries.push(entry);
        }
        Ok(())
    }

    /// Records an invalid slash command in this conversation's composer
    /// history without publishing it to the cross-conversation global cache.
    ///
    /// # Errors
    /// Returns an error if the command cannot be durably recorded.
    pub async fn record_invalid_slash_command(
        &self,
        text: impl Into<String>,
    ) -> Result<(), RuntimeError> {
        self.ensure_owner()?;
        let entry = crate::ComposerHistoryEntry {
            kind: crate::ComposerInputKind::Prompt,
            text: text.into(),
            attachment_specs: Vec::new(),
            images: Vec::new(),
            image_chips: Vec::new(),
        };
        self.store
            .record_slash_command(self.id, entry.text.clone(), None, false)
            .await?;
        if let Ok(mut entries) = self.composer_history.write()
            && entries.last() != Some(&entry)
        {
            entries.push(entry);
        }
        Ok(())
    }

    /// Records a slash command that also submits `user_text`. The command is
    /// kept as the single composer-history entry instead of being followed by
    /// a duplicate generic user-message entry.
    pub async fn record_executed_slash_command_with_message(
        &self,
        text: impl Into<String>,
        user_text: impl Into<String>,
    ) -> Result<(), RuntimeError> {
        self.ensure_owner()?;
        let entry = crate::ComposerHistoryEntry {
            kind: crate::ComposerInputKind::Prompt,
            text: text.into(),
            attachment_specs: Vec::new(),
            images: Vec::new(),
            image_chips: Vec::new(),
        };
        self.store
            .record_slash_command(self.id, entry.text.clone(), Some(user_text.into()), true)
            .await?;
        if let Ok(mut entries) = self.composer_history.write()
            && entries.last() != Some(&entry)
        {
            entries.push(entry);
        }
        Ok(())
    }

    /// Appends a frontend-generated message to the durable transcript.
    ///
    /// # Errors
    /// Returns an error if the message cannot be persisted.
    pub async fn append_system_message(
        &self,
        message: impl Into<String>,
    ) -> Result<(), RuntimeError> {
        self.store
            .append_transcript_notice(self.id, message.into())
            .await
    }

    /// Returns effective MCP servers for the active agent, enriched with live status and tools.
    ///
    /// # Errors
    ///
    /// Returns an error if profiles or the MCP catalog cannot be loaded.
    pub async fn mcp_servers(&self) -> Result<Vec<crate::McpEffectiveServer>, RuntimeError> {
        let (agent, _) = self.active_profiles().await?;
        let servers = self
            .mcp_supervisor
            .describe(&self.mcp_config, &agent)
            .await?;
        for server in &servers {
            let _ = self
                .transient
                .send(crate::TransientEvent::McpStatusUpdated {
                    server: server.name.clone(),
                    status: server.status.clone(),
                });
        }
        Ok(servers)
    }

    /// Returns one effective MCP server by ID.
    ///
    /// # Errors
    ///
    /// Returns an error if profiles or the MCP catalog cannot be loaded.
    pub async fn mcp_server(
        &self,
        name: &str,
    ) -> Result<Option<crate::McpEffectiveServer>, RuntimeError> {
        Ok(self
            .mcp_servers()
            .await?
            .into_iter()
            .find(|server| server.name == name))
    }

    /// Starts discovery on demand for a server selected in the MCP browser.
    pub async fn inspect_mcp_server(
        &self,
        name: &str,
    ) -> Result<crate::McpEffectiveServer, RuntimeError> {
        let server = self
            .mcp_server(name)
            .await?
            .ok_or_else(|| RuntimeError::InvalidOption(format!("unknown MCP server: {name}")))?;
        let _ = self.mcp_supervisor.inspect(&self.mcp_config, &server).await;
        self.mcp_server(name)
            .await?
            .ok_or_else(|| RuntimeError::InvalidOption(format!("unknown MCP server: {name}")))
    }

    /// Returns whether managed OAuth credentials exist for an HTTP MCP server.
    pub async fn mcp_oauth_status(
        &self,
        name: &str,
    ) -> Result<crate::McpSecretStatus, RuntimeError> {
        let server = self
            .mcp_server(name)
            .await?
            .ok_or_else(|| RuntimeError::InvalidOption(format!("unknown MCP server: {name}")))?;
        self.mcp_supervisor.oauth_status(&server)
    }

    /// Starts browser or Device Flow OAuth and completes it in the background.
    pub async fn connect_mcp_oauth(
        &self,
        name: &str,
    ) -> Result<crate::McpOAuthAttempt, RuntimeError> {
        let server = self
            .mcp_server(name)
            .await?
            .ok_or_else(|| RuntimeError::InvalidOption(format!("unknown MCP server: {name}")))?;
        let flow = self.mcp_supervisor.begin_oauth(&server).await?;
        let prompt = flow.prompt.clone();
        let (completion_tx, completion) =
            tokio::sync::watch::channel(crate::McpOAuthCompletion::Waiting);
        let session = self.clone();
        let name = name.to_owned();
        tokio::spawn(
            async move {
                match flow.complete().await {
                    Ok(()) => {
                        session
                            .mcp_supervisor
                            .forget_server(session.mcp_config.workspace(), &name)
                            .await;
                        let restart = async {
                            let (agent, _) = session.active_profiles().await?;
                            session.publish_mcp_catalog(&agent).await?;
                            session.inspect_mcp_server(&name).await?;
                            Ok::<(), RuntimeError>(())
                        }
                        .await;
                        match restart {
                            Ok(()) => {
                                let _ = completion_tx.send(crate::McpOAuthCompletion::Connected);
                            }
                            Err(error) => {
                                tracing::warn!(%error, server = %name, "failed to restart MCP after OAuth");
                                let _ = completion_tx.send(crate::McpOAuthCompletion::Failed(
                                    format!("OAuth was saved, but the MCP did not restart: {error}"),
                                ));
                            }
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%error, server = %name, "MCP OAuth failed");
                        let _ = completion_tx
                            .send(crate::McpOAuthCompletion::Failed(error.to_string()));
                    }
                }
            }
            .in_current_span(),
        );
        Ok(crate::McpOAuthAttempt { prompt, completion })
    }

    /// Clears managed OAuth credentials and restarts the selected MCP server.
    pub async fn disconnect_mcp_oauth(&self, name: &str) -> Result<(), RuntimeError> {
        let server = self
            .mcp_server(name)
            .await?
            .ok_or_else(|| RuntimeError::InvalidOption(format!("unknown MCP server: {name}")))?;
        self.mcp_supervisor.clear_oauth(&server)?;
        self.mcp_supervisor
            .forget_server(self.mcp_config.workspace(), name)
            .await;
        let (agent, _) = self.active_profiles().await?;
        let _ = self.publish_mcp_catalog(&agent).await?;
        Ok(())
    }

    #[must_use]
    pub fn mcp_project_control(&self) -> crate::ProjectControlStatus {
        self.mcp_config.project_control_status()
    }

    /// Parses a portable JSON import without applying it.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid input, scope, or trust.
    #[allow(clippy::unused_async)]
    pub async fn preview_mcp_import(
        &self,
        location: crate::McpLocation,
        source: &str,
        separate_name: Option<&str>,
    ) -> Result<crate::McpImportPreview, RuntimeError> {
        self.mcp_config
            .preview_import_json(location, source, separate_name)
    }

    /// Validates an MCP mutation batch and returns its consequences.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid mutations, profiles, scope, or trust.
    pub async fn preview_mcp_mutations(
        &self,
        mutations: Vec<crate::McpMutation>,
    ) -> Result<crate::McpMutationPreview, RuntimeError> {
        let (agent, _) = self.active_profiles().await?;
        self.mcp_config.preview_mutations(&agent, mutations)
    }

    /// Applies a previewed import and publishes the new catalog.
    ///
    /// # Errors
    ///
    /// Returns an error for missing confirmation or failed persistence/catalog refresh.
    pub async fn apply_mcp_import(
        &self,
        preview: crate::McpImportPreview,
        confirmed: bool,
    ) -> Result<Vec<crate::McpEffectiveServer>, RuntimeError> {
        let (agent, _) = self.active_profiles().await?;
        self.mcp_config.apply_import(&agent, preview, confirmed)?;
        self.publish_mcp_catalog(&agent).await
    }

    /// Applies a previewed mutation batch and publishes the new catalog.
    ///
    /// # Errors
    ///
    /// Returns an error for missing confirmation or failed persistence/catalog refresh.
    pub async fn apply_mcp_mutations(
        &self,
        preview: crate::McpMutationPreview,
        confirmed: bool,
    ) -> Result<Vec<crate::McpEffectiveServer>, RuntimeError> {
        let (agent, _) = self.active_profiles().await?;
        self.mcp_config
            .apply_mutations(&agent, preview, confirmed)?;
        self.publish_mcp_catalog(&agent).await
    }

    /// Enables or disables one exact definition and publishes the new catalog.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown definition or failed persistence/catalog refresh.
    pub async fn set_mcp_enabled(
        &self,
        location: crate::McpLocation,
        name: &str,
        enabled: bool,
    ) -> Result<Vec<crate::McpEffectiveServer>, RuntimeError> {
        let (agent, _) = self.active_profiles().await?;
        self.mcp_config
            .set_enabled(&agent, location, name, enabled)?;
        self.publish_mcp_catalog(&agent).await
    }

    /// Returns setup declarations and secret statuses for an installed bundled package.
    pub async fn mcp_package_setup(
        &self,
        name: &str,
    ) -> Result<Option<crate::McpPackageSetup>, RuntimeError> {
        let Some(server) = self.mcp_server(name).await? else {
            return Ok(None);
        };
        let Some(source) = server.definition.package.as_deref() else {
            return Ok(None);
        };
        let Some(package_id) = source.strip_prefix("builtin:") else {
            return Ok(None);
        };
        let Some(package) = crate::mcp::builtin_packages()
            .into_iter()
            .find(|package| package.id == package_id)
        else {
            return Ok(None);
        };
        let secret_statuses = package
            .secrets
            .keys()
            .map(|id| {
                self.mcp_supervisor
                    .package_secret_status(name, id)
                    .map(|status| (id.clone(), status))
            })
            .collect::<Result<_, _>>()?;
        Ok(Some(crate::McpPackageSetup {
            package,
            server_name: server.name,
            location: server.location,
            parameters: server.definition.parameters,
            secret_statuses,
        }))
    }

    /// Installs a bundled MCP package with typed parameters and managed secrets.
    pub async fn install_builtin_mcp(
        &self,
        package_id: &str,
        location: crate::McpLocation,
    ) -> Result<Vec<crate::McpEffectiveServer>, RuntimeError> {
        let package = crate::mcp::builtin_packages()
            .into_iter()
            .find(|package| package.id == package_id)
            .ok_or_else(|| {
                RuntimeError::InvalidOption(format!("unknown built-in MCP package: {package_id}"))
            })?;
        let parameters = package
            .parameters
            .iter()
            .filter_map(|(id, parameter)| {
                parameter.default.clone().map(|value| (id.clone(), value))
            })
            .collect();
        self.install_builtin_mcp_configured(package_id, location, parameters, BTreeMap::new())
            .await
    }

    pub async fn install_builtin_mcp_configured(
        &self,
        package_id: &str,
        location: crate::McpLocation,
        parameters: BTreeMap<String, crate::McpParameterValue>,
        secrets: BTreeMap<String, String>,
    ) -> Result<Vec<crate::McpEffectiveServer>, RuntimeError> {
        let package = crate::mcp::builtin_packages()
            .into_iter()
            .find(|package| package.id == package_id)
            .ok_or_else(|| {
                RuntimeError::InvalidOption(format!("unknown built-in MCP package: {package_id}"))
            })?;
        if let Some(unknown) = secrets.keys().find(|id| !package.secrets.contains_key(*id)) {
            return Err(RuntimeError::InvalidOption(format!(
                "unknown MCP package secret: {unknown}"
            )));
        }
        let server_name = package.id.clone();
        let definition = package.resolve(
            &format!("builtin:{}", package.id),
            &server_name,
            None,
            parameters,
        )?;
        let preview = self
            .preview_mcp_mutations(vec![crate::McpMutation::Put {
                location,
                name: server_name.clone(),
                definition,
            }])
            .await?;
        if preview.requires_confirmation {
            return Err(RuntimeError::InvalidOption(
                "an MCP with this name already exists; remove it before catalog installation"
                    .into(),
            ));
        }
        for (id, value) in &secrets {
            self.mcp_supervisor
                .update_package_secret(&server_name, id, Some(value))?;
        }
        self.apply_mcp_mutations(preview, true).await
    }

    /// Updates typed parameters and managed secrets for an installed bundled package.
    pub async fn configure_mcp_package(
        &self,
        name: &str,
        parameters: BTreeMap<String, crate::McpParameterValue>,
        secret_updates: BTreeMap<String, Option<String>>,
    ) -> Result<Vec<crate::McpEffectiveServer>, RuntimeError> {
        let server = self
            .mcp_server(name)
            .await?
            .ok_or_else(|| RuntimeError::InvalidOption(format!("unknown MCP server: {name}")))?;
        let source = server.definition.package.as_deref().ok_or_else(|| {
            RuntimeError::InvalidOption("MCP server is not package-backed".into())
        })?;
        let package_id = source.strip_prefix("builtin:").ok_or_else(|| {
            RuntimeError::InvalidOption("only bundled MCP packages can be configured here".into())
        })?;
        let package = crate::mcp::builtin_packages()
            .into_iter()
            .find(|package| package.id == package_id)
            .ok_or_else(|| RuntimeError::InvalidOption("unknown bundled MCP package".into()))?;
        if let Some(unknown) = secret_updates
            .keys()
            .find(|id| !package.secrets.contains_key(*id))
        {
            return Err(RuntimeError::InvalidOption(format!(
                "unknown MCP package secret: {unknown}"
            )));
        }
        let mut definition = package.resolve(source, name, None, parameters)?;
        definition.enabled = server.definition.enabled;
        definition.agents = server.definition.agents;
        let preview = self
            .preview_mcp_mutations(vec![crate::McpMutation::Put {
                location: server.location,
                name: name.into(),
                definition,
            }])
            .await?;
        for (id, value) in &secret_updates {
            self.mcp_supervisor
                .update_package_secret(name, id, value.as_deref())?;
        }
        self.apply_mcp_mutations(preview, true).await?;
        // Parameter fingerprints normally replace the runtime during catalog
        // reconciliation, but secret-only updates are intentionally absent
        // from the persisted definition. Explicitly forget the old process so
        // either kind of package setup change takes effect immediately.
        self.mcp_supervisor
            .forget_server(self.mcp_config.workspace(), name)
            .await;
        let (agent, _) = self.active_profiles().await?;
        self.publish_mcp_catalog(&agent).await
    }

    /// Removes one exact definition after explicit confirmation.
    ///
    /// # Errors
    ///
    /// Returns an error without confirmation or when preview/persistence/catalog refresh fails.
    pub async fn remove_mcp_server(
        &self,
        location: crate::McpLocation,
        name: &str,
        confirmed: bool,
    ) -> Result<Vec<crate::McpEffectiveServer>, RuntimeError> {
        let preview = self
            .preview_mcp_mutations(vec![crate::McpMutation::Remove {
                location,
                name: name.into(),
            }])
            .await?;
        if !confirmed {
            return Err(RuntimeError::InvalidOption(
                "MCP removal requires explicit confirmation".into(),
            ));
        }
        self.apply_mcp_mutations(preview, true).await
    }

    async fn publish_mcp_catalog(
        &self,
        agent: &str,
    ) -> Result<Vec<crate::McpEffectiveServer>, RuntimeError> {
        self.mcp_supervisor
            .activate_agent(&self.mcp_config, agent)
            .await?;
        let servers = self
            .mcp_supervisor
            .describe(&self.mcp_config, agent)
            .await?;
        let _ = self
            .transient
            .send(crate::TransientEvent::McpCatalogUpdated {
                servers: servers.clone(),
            });
        for server in &servers {
            let _ = self
                .transient
                .send(crate::TransientEvent::McpStatusUpdated {
                    server: server.name.clone(),
                    status: server.status.clone(),
                });
        }
        Ok(servers)
    }

    /// Returns the conversation's stored title, or `None` for a new untitled session.
    ///
    /// # Errors
    /// Returns an error when the durable session state cannot be loaded.
    pub async fn title(&self) -> Result<Option<String>, RuntimeError> {
        self.store.load_conversation_title(self.id).await
    }

    #[must_use]
    pub fn terminal_counts(&self) -> (usize, usize) {
        self.terminals.counts()
    }

    pub async fn terminate_terminals(&self, force: bool) {
        self.terminals.terminate_all(force).await;
    }

    /// Gracefully stops one active item from the supervised-work browser.
    ///
    /// The target is an ID captured by the frontend, rather than a row index,
    /// so a concurrent snapshot refresh cannot redirect the stop request.
    pub async fn stop_supervised_work(
        &self,
        target: crate::runtime::SupervisedWorkTarget,
    ) -> Result<(), RuntimeError> {
        match target {
            crate::runtime::SupervisedWorkTarget::Terminal(id) => {
                let terminal = self
                    .terminals
                    .snapshot(id)
                    .map_err(|error| RuntimeError::InvalidOption(error.to_string()))?;
                if terminal.owner != self.id {
                    return Err(RuntimeError::InvalidOption(
                        "terminal belongs to another conversation".into(),
                    ));
                }
                if terminal.status.is_final() {
                    return Err(RuntimeError::InvalidOption(
                        "terminal is no longer active".into(),
                    ));
                }
                self.terminals
                    .kill(&crate::TerminalKillRequest { id, force: false })
                    .await
                    .map_err(|error| RuntimeError::InvalidOption(error.to_string()))?;
                Ok(())
            }
            crate::runtime::SupervisedWorkTarget::Agent(id) => {
                self.delegation.cancel_agent_run(self.id, id).await
            }
        }
    }

    /// Stops active work and supervised terminals before the frontend leaves
    /// this conversation. Unlike `Cancel`, this does not advance queued work.
    pub async fn end(&self) -> Result<(), RuntimeError> {
        let result = self
            .submit(crate::SessionCommand::new(crate::SessionAction::End))
            .await;
        if result.is_ok() {
            while self.live_turn.read().is_ok_and(|turn| turn.is_some()) {
                tokio::task::yield_now().await;
            }
        }
        self.delegation.shutdown(self.id).await;
        self.terminate_terminals(false).await;
        self.session_cancellation.cancel();
        self.global_store
            .stop_projection_watcher(&self.takeover_runtime.conversation_path(self.id));
        if let Some(sessions) = self.sessions.upgrade() {
            sessions.lock().await.remove(&self.id);
        }
        result.map(|_| ())
    }

    /// Returns the interaction currently waiting for a frontend response.
    ///
    /// Frontends may use this to reconcile their local presentation state in
    /// addition to consuming [`RuntimeEvent::Interaction`] notifications.
    #[must_use]
    pub fn pending_interaction(&self) -> Option<crate::InteractionRequest> {
        self.interactions.borrow().clone()
    }

    /// Returns message, token, and cost totals for the active history branch.
    ///
    /// # Errors
    /// Returns an error when the durable session state cannot be loaded.
    pub async fn stats(&self) -> Result<crate::SessionStats, RuntimeError> {
        let stored = self.store.load_session_stats(self.id).await?;
        let cost = stored.usage.total_cost();
        Ok(crate::SessionStats {
            id: self.id,
            title: stored.title,
            message_count: stored.message_count,
            total_tokens: stored.usage.display_total_tokens().unwrap_or_default(),
            total_cost: cost.and_then(|cost| cost.total_cost.clone()),
            currency: cost.map(|cost| cost.currency.clone()),
            cost_source: cost.map(|_| stored.usage.cost_source.unwrap_or(crate::CostSource::Mixed)),
            usage: stored.usage,
        })
    }

    #[cfg(test)]
    pub async fn transcript_events(&self) -> Result<Vec<crate::DurableEvent>, RuntimeError> {
        let history = self.store.load_history(self.id).await?;
        let branch = history.iter().map(|node| node.id).collect::<HashSet<_>>();
        let events = history
            .into_iter()
            .enumerate()
            .map(|(index, node)| {
                node.into_appended_event(
                    self.id,
                    crate::EventCursor(u64::try_from(index + 1).unwrap_or(u64::MAX)),
                )
            })
            .collect();
        Ok(self.transcript_events_from_branch(&branch, events))
    }

    /// Loads the page immediately preceding `cursor` on the current visible
    /// branch.
    pub async fn load_older_transcript(
        &self,
        cursor: crate::TranscriptCursor,
    ) -> Result<crate::TranscriptPage, RuntimeError> {
        let hydration = self
            .store
            .load_transcript_page(self.id, Some(cursor))
            .instrument(tracing::trace_span!("agent.transcript.page_query"))
            .await?;
        Ok(self.project_transcript_page(hydration, TranscriptPageContext::HistoricalPage))
    }

    /// Applies the same node/event/block pipeline to both the live tail and
    /// older immutable pages. The context makes the one intentional
    /// difference explicit: only the tail may carry live lifecycle state and
    /// a held permission interaction.
    fn project_transcript_page(
        &self,
        hydration: crate::store::TranscriptPageHydration,
        context: TranscriptPageContext<'_>,
    ) -> crate::TranscriptPage {
        let visible_recap = matches!(&context, TranscriptPageContext::LiveTail { .. })
            .then_some(crate::TranscriptBlockId::node(hydration.active_node_id));
        let events = self.transcript_events_from_page(hydration.history, hydration.events);
        let web_search_provider = self.config_store.snapshot().web_search().provider();
        let mut blocks = match context {
            TranscriptPageContext::LiveTail {
                turn,
                terminals,
                agent_runs,
                held_permission,
            } => transcript_blocks_with_web_search_provider(
                &events,
                &hydration.workspace,
                turn,
                terminals,
                agent_runs,
                held_permission,
                web_search_provider,
            ),
            TranscriptPageContext::HistoricalPage => transcript_blocks_with_web_search_provider(
                &events,
                &hydration.workspace,
                &crate::TurnState::Idle,
                &hydration.terminals,
                &hydration.agent_runs,
                None,
                web_search_provider,
            ),
        };
        retain_only_trailing_recap(&mut blocks, visible_recap.as_ref());
        crate::TranscriptPage {
            blocks,
            older: hydration.older,
        }
    }

    #[allow(clippy::needless_borrow)] // The branch set is borrowed for projection without transferring ownership.
    fn transcript_events_from_page(
        &self,
        history: Vec<crate::HistoryNode>,
        source_events: Vec<crate::store::TranscriptPageEvent>,
    ) -> Vec<crate::DurableEvent> {
        let branch = history.iter().map(|node| node.id).collect::<HashSet<_>>();
        let mut nodes = history
            .into_iter()
            .map(|node| (node.id, node))
            .collect::<HashMap<_, _>>();
        let events = source_events
            .into_iter()
            .filter_map(|event| match event {
                crate::store::TranscriptPageEvent::NodeAppended { cursor, node_id } => {
                    let node = nodes.remove(&node_id)?;
                    Some(node.into_appended_event(self.id, cursor))
                }
            })
            .collect();
        self.transcript_events_from_branch(&branch, events)
    }

    fn transcript_events_from_branch(
        &self,
        branch: &HashSet<crate::NodeId>,
        source_events: Vec<crate::DurableEvent>,
    ) -> Vec<crate::DurableEvent> {
        let mut projection = MarkdownProjection::default();
        let mut events = Vec::new();
        for mut event in source_events {
            let visible = transcript_event_visible_on_branch(branch, &event.kind);
            if visible {
                projection.project(&mut event.kind);
                events.push(event);
            }
        }
        events
    }

    /// Returns the optional model and effort currently selected for this conversation.
    ///
    /// # Errors
    ///
    /// Returns an error when the durable session selection cannot be loaded.
    pub async fn model_selection(
        &self,
    ) -> Result<Option<(String, String, Option<String>)>, RuntimeError> {
        let (_, mode) = self.store.load_active_profiles(self.id).await?;
        let selection = self
            .store
            .load_mode_selections(self.id)
            .await?
            .remove(&mode)
            .flatten()
            .or(if mode == "plan" {
                self.store.load_plan_model_selection(self.id).await?
            } else {
                self.store.load_model_selection(self.id).await?
            });
        Ok(selection.map(|(provider, model, effort, _source)| (provider, model, effort)))
    }

    /// Returns true only when the selected model explicitly advertises image input.
    pub async fn selected_model_supports_image_input(&self) -> Result<bool, RuntimeError> {
        let Some((provider, model, _)) = self.model_selection().await? else {
            return Ok(false);
        };
        let catalog = self.models(&provider).await?;
        Ok(catalog
            .catalog
            .models
            .iter()
            .find(|candidate| candidate.id == model)
            .is_some_and(|candidate| candidate.capabilities.supports_image_input == Some(true)))
    }

    /// Returns true only when the selected model advertises the Fast service tier.
    pub async fn selected_model_supports_fast_mode(&self) -> Result<bool, RuntimeError> {
        let Some((provider, model, _)) = self.model_selection().await? else {
            self.selected_model_fast_support
                .store(FAST_SUPPORT_UNSUPPORTED, Ordering::Relaxed);
            return Ok(false);
        };
        let catalog = self.models(&provider).await?;
        let supported = catalog
            .catalog
            .models
            .iter()
            .find(|candidate| candidate.id == model)
            .is_some_and(|candidate| candidate.capabilities.supports_fast_mode == Some(true));
        self.selected_model_fast_support.store(
            if supported {
                FAST_SUPPORT_SUPPORTED
            } else {
                FAST_SUPPORT_UNSUPPORTED
            },
            Ordering::Relaxed,
        );
        Ok(supported)
    }

    /// Returns the persisted global Fast preference.
    #[must_use]
    pub fn fast(&self) -> bool {
        self.config_store.snapshot().fast()
    }

    /// Persists Fast mode and returns whether it is effective for the selected model.
    pub async fn set_fast(&self, enabled: bool) -> Result<bool, RuntimeError> {
        self.ensure_owner()?;
        self.config_store.persist_fast(enabled)?;
        if !enabled {
            return Ok(false);
        }
        match self.selected_model_fast_support.load(Ordering::Relaxed) {
            FAST_SUPPORT_UNSUPPORTED => Ok(false),
            FAST_SUPPORT_SUPPORTED => Ok(true),
            _ => self.selected_model_supports_fast_mode().await,
        }
    }

    /// Returns the effective model selection for a configured mode.
    pub async fn mode_model_selection(
        &self,
        mode: &str,
    ) -> Result<Option<(String, String, Option<String>)>, RuntimeError> {
        let selection = self
            .store
            .load_mode_selections(self.id)
            .await?
            .remove(mode)
            .flatten()
            .or(if mode == "plan" {
                self.store.load_plan_model_selection(self.id).await?
            } else {
                self.store.load_model_selection(self.id).await?
            });
        Ok(selection.map(|(provider, model, effort, _source)| (provider, model, effort)))
    }

    /// Selects a model for a configured mode without changing the active mode
    /// or global defaults.
    pub async fn set_mode_model(
        &self,
        mode: impl Into<String>,
        provider: impl Into<String>,
        model: impl Into<String>,
        effort: Option<String>,
    ) -> Result<(), RuntimeError> {
        self.selected_model_fast_support
            .store(FAST_SUPPORT_UNKNOWN, Ordering::Relaxed);
        self.submit(crate::SessionCommand::new(
            crate::SessionAction::SetModeModel {
                mode: mode.into(),
                provider: provider.into(),
                model: model.into(),
                effort,
            },
        ))
        .await
        .map(|_| ())
    }

    /// Selects a model for this session without persisting a global default.
    pub async fn set_session_model(
        &self,
        provider: impl Into<String>,
        model: impl Into<String>,
        effort: Option<String>,
    ) -> Result<(), RuntimeError> {
        self.selected_model_fast_support
            .store(FAST_SUPPORT_UNKNOWN, Ordering::Relaxed);
        self.submit(crate::SessionCommand::new(
            crate::SessionAction::SetSessionModel {
                provider: provider.into(),
                model: model.into(),
                effort,
            },
        ))
        .await
        .map(|_| ())
    }

    /// Selects a model for a headless exec run after the frontend has verified
    /// that the provider has environment credentials.
    pub async fn set_session_exec_model(
        &self,
        provider: impl Into<String>,
        model: impl Into<String>,
        effort: Option<String>,
    ) -> Result<(), RuntimeError> {
        self.submit(crate::SessionCommand::new(
            crate::SessionAction::SetSessionExecModel {
                provider: provider.into(),
                model: model.into(),
                effort,
            },
        ))
        .await
        .map(|_| ())
    }

    /// Selects an agent for this session.
    pub async fn set_session_agent(&self, agent: impl Into<String>) -> Result<(), RuntimeError> {
        self.selected_model_fast_support
            .store(FAST_SUPPORT_UNKNOWN, Ordering::Relaxed);
        self.submit(crate::SessionCommand::new(
            crate::SessionAction::ChangeAgent {
                agent: agent.into(),
            },
        ))
        .await
        .map(|_| ())
    }

    /// Selects a mode for this session.
    pub async fn set_session_mode(&self, mode: impl Into<String>) -> Result<(), RuntimeError> {
        self.selected_model_fast_support
            .store(FAST_SUPPORT_UNKNOWN, Ordering::Relaxed);
        self.submit(crate::SessionCommand::new(
            crate::SessionAction::ChangeMode { mode: mode.into() },
        ))
        .await
        .map(|_| ())
    }

    /// Returns the model references currently marked as favourites.
    ///
    /// # Errors
    /// Returns an error if the shared runtime preference state is unavailable.
    pub fn model_favourites(&self) -> Result<BTreeSet<ModelRef>, RuntimeError> {
        Ok(self.config_store.snapshot().favourite_models().clone())
    }

    /// Returns the resolved global statusline preference shared by frontend sessions.
    ///
    /// # Errors
    /// Returns an error if the shared runtime preference state is unavailable.
    pub fn status_line_config(&self) -> Result<crate::StatusLineConfig, RuntimeError> {
        Ok(self.config_store.snapshot().status_line().clone())
    }

    /// Atomically persists and applies a global statusline preference.
    ///
    /// Persistence happens before shared runtime state changes, so a failed save is non-mutating.
    ///
    /// # Errors
    /// Returns an error if shared state is unavailable or the configuration cannot be persisted.
    pub fn save_status_line_config(
        &self,
        status_line: &crate::StatusLineConfig,
    ) -> Result<(), RuntimeError> {
        self.config_store.persist_status_line(status_line)?;
        Ok(())
    }

    /// Toggles a model favourite and atomically persists the updated set in `config.toml`.
    /// Returns whether the model is now a favourite.
    ///
    /// # Errors
    /// Returns an error if shared state is unavailable or the configuration cannot be persisted.
    pub fn toggle_model_favourite(
        &self,
        provider: impl Into<String>,
        model: impl Into<String>,
    ) -> Result<bool, RuntimeError> {
        let model = ModelRef {
            provider: provider.into(),
            model: model.into(),
        };
        let mut updated = self.config_store.snapshot().favourite_models().clone();
        let favourite = if updated.remove(&model) {
            false
        } else {
            updated.insert(model);
            true
        };
        self.config_store.persist_model_favourites(&updated)?;
        Ok(favourite)
    }

    /// Returns the conversation's active agent and mode.
    ///
    /// # Errors
    /// Returns an error when the durable session state cannot be loaded.
    pub async fn active_profiles(&self) -> Result<(String, String), RuntimeError> {
        self.store.load_active_profiles(self.id).await
    }

    /// Returns all resolved agent profiles.
    ///
    /// # Errors
    /// Returns an error when profile configuration is invalid.
    pub fn agent_profiles(&self) -> Result<Vec<crate::AgentProfile>, RuntimeError> {
        Ok(self
            .config_store
            .snapshot()
            .agent_catalog()?
            .user_profiles()
            .map(|(_, profile)| profile.clone())
            .collect())
    }

    /// Returns every profile for configuration management, including profiles
    /// that may only be used by delegated subagents.
    pub fn manageable_agent_profiles(&self) -> Result<Vec<crate::AgentProfile>, RuntimeError> {
        Ok(self
            .config_store
            .snapshot()
            .agent_catalog()?
            .iter()
            .map(|(_, profile)| profile.clone())
            .collect())
    }

    /// Reads the raw editable agent definitions, preserving unset fields.
    pub fn agent_drafts(
        &self,
    ) -> Result<std::collections::BTreeMap<String, crate::AgentProfileDraft>, RuntimeError> {
        self.config_store.agent_drafts()
    }

    /// Atomically persists an agent definition after validating the complete
    /// candidate configuration.
    pub fn save_agent_draft(
        &self,
        name: &str,
        draft: &crate::AgentProfileDraft,
    ) -> Result<(), RuntimeError> {
        self.config_store.save_agent_draft(name, draft).map(|_| ())
    }

    /// Renames a configured profile and its known configuration references.
    pub fn rename_agent_profile(&self, old: &str, new: &str) -> Result<(), RuntimeError> {
        self.config_store.rename_agent_profile(old, new).map(|_| ())
    }

    /// Loads durable delegated runs for this conversation, including tool activity.
    ///
    /// # Errors
    /// Returns an error when the durable run records cannot be loaded.
    pub async fn agent_runs(&self) -> Result<Vec<crate::AgentRun>, RuntimeError> {
        self.store.list_agent_runs(self.id).await
    }

    /// Loads only the next page of one delegated log, without hydrating other runs.
    pub async fn agent_run_log_page(
        &self,
        id: crate::AgentRunId,
        after: u64,
    ) -> Result<crate::AgentRunLogPage, RuntimeError> {
        self.store.load_agent_run_log_page(self.id, id, after).await
    }

    /// Loads durable supervised background terminals for this conversation.
    ///
    /// # Errors
    /// Returns an error if durable terminal records cannot be loaded.
    pub async fn background_terminals(&self) -> Result<Vec<crate::TerminalSnapshot>, RuntimeError> {
        self.store.list_terminals(self.id).await
    }

    /// Reads retained terminal output, including after the live process exits.
    ///
    /// # Errors
    /// Returns an error for an unknown terminal or unavailable durable state.
    pub async fn terminal_output(
        &self,
        request: crate::TerminalOutputRequest,
    ) -> Result<crate::TerminalOutput, RuntimeError> {
        if let Ok(output) = self.terminals.output(&request) {
            if output.status.is_final() {
                let _ = self
                    .store
                    .claim_completion(
                        self.id,
                        "terminal",
                        request.id.to_string(),
                        "terminal_output",
                    )
                    .await?;
            }
            return Ok(output);
        }
        let terminal = self.store.load_terminal(self.id, request.id).await?;
        if terminal.status.is_final() {
            let _ = self
                .store
                .claim_completion(
                    self.id,
                    "terminal",
                    request.id.to_string(),
                    "terminal_output",
                )
                .await?;
        }
        let requested = request.cursor.unwrap_or(terminal.output_base);
        let offset = usize::try_from(requested.saturating_sub(terminal.output_base))
            .unwrap_or(usize::MAX)
            .min(terminal.output.len());
        let offset = floor_char_boundary(&terminal.output, offset);
        Ok(crate::TerminalOutput {
            id: terminal.id,
            owner: terminal.owner,
            output: terminal.output[offset..].to_owned(),
            ansi_output: terminal.ansi_output,
            cursor: terminal.output_cursor,
            lost_output: requested < terminal.output_base,
            status: terminal.status,
            exit_code: terminal.exit_code,
            started_at: terminal.started_at,
            completed_at: terminal.completed_at,
        })
    }

    /// Returns the agent-owned projection for the background-work browser.
    ///
    /// # Errors
    /// Returns an error if either durable work collection cannot be loaded.
    pub async fn supervised_work(
        &self,
    ) -> Result<Vec<crate::presentation::SupervisedWork>, RuntimeError> {
        Ok(self.session_snapshot().await?.supervised_work)
    }

    /// Returns all resolved mode profiles.
    ///
    /// # Errors
    /// Returns an error when mode configuration is invalid.
    pub fn mode_profiles(&self) -> Result<Vec<crate::ModeProfile>, RuntimeError> {
        self.config_store.snapshot().enabled_modes()
    }

    /// Returns picker-facing provider states without mutating enablement.
    pub async fn providers(&self) -> Vec<crate::ProviderAvailability> {
        let set = self.providers.snapshot();
        let mut providers = Vec::with_capacity(set.len());
        for (_id, entry) in set.iter() {
            let provider = &entry.adapter;
            let auth =
                provider
                    .auth_state()
                    .await
                    .unwrap_or_else(|error| crate::AuthState::Error {
                        message: error.message,
                    });
            providers.push(crate::ProviderAvailability {
                descriptor: provider.descriptor().clone(),
                enabled: entry.settings.enabled,
                auth,
                has_managed_api_key: provider.has_managed_api_key().await,
            });
        }
        providers.sort_by(|left, right| {
            left.descriptor
                .display_name
                .cmp(&right.descriptor.display_name)
        });
        providers
    }

    /// Starts a managed provider authentication flow. The frontend owns presentation and browser
    /// launching; provider adapters only return a serializable challenge.
    ///
    /// # Errors
    /// Returns an error for an unknown provider or when the provider rejects the flow.
    pub async fn begin_provider_auth(
        &self,
        provider_id: &str,
        flow: crate::AuthFlow,
    ) -> Result<crate::AuthChallenge, RuntimeError> {
        let provider = self.providers.get(provider_id).ok_or_else(|| {
            RuntimeError::InvalidOption(format!("unknown provider adapter: {provider_id}"))
        })?;
        if flow == crate::AuthFlow::BrowserPkce
            && let Some(pending) = self.pending_oauth.lock().await.get(provider_id)
        {
            return Ok(pending.challenge.clone());
        }
        if flow == crate::AuthFlow::DeviceCode {
            self.cancel_pending_oauth(provider_id, provider.clone())
                .await;
        }
        let mut challenge = provider
            .begin_auth(flow)
            .await
            .map_err(|error| RuntimeError::InvalidOption(error.to_string()))?;
        if let crate::AuthChallenge::Browser { callback_url, .. } = &challenge {
            let mut listener = bind_oauth_callback(callback_url).await;
            if listener.is_err() && callback_url == "http://localhost:1455/auth/callback" {
                let _ = provider.complete_auth(crate::AuthResponse::Cancel).await;
                let fallback_url = "http://localhost:1457/auth/callback".to_owned();
                challenge = provider
                    .begin_auth_with_callback(flow, Some(fallback_url))
                    .await
                    .map_err(|error| RuntimeError::InvalidOption(error.to_string()))?;
                if let crate::AuthChallenge::Browser { callback_url, .. } = &challenge {
                    listener = bind_oauth_callback(callback_url).await;
                }
            }
            let listener = match listener {
                Ok(listener) => listener,
                Err(error) => {
                    let _ = provider.complete_auth(crate::AuthResponse::Cancel).await;
                    return Err(error);
                }
            };
            let provider_id = provider_id.to_owned();
            let transient = self.transient.clone();
            let session = self.clone();
            let session_id = self.id;
            let cancellation = CancellationToken::new();
            self.pending_oauth.lock().await.insert(
                provider_id.clone(),
                PendingOAuth {
                    challenge: challenge.clone(),
                    cancellation: cancellation.clone(),
                },
            );
            tokio::spawn(
                async move {
                    let mut result = tokio::select! {
                        biased;
                        () = cancellation.cancelled() => return,
                        result = receive_oauth_callback(listener, provider.clone()) => result,
                    };
                    session.pending_oauth.lock().await.remove(&provider_id);
                    if result.is_ok()
                        && let Err(error) =
                            session.activate_authenticated_provider(&provider_id, provider.clone())
                    {
                        result = Err(error);
                    }
                    if result.is_err() {
                        let _ = provider.complete_auth(crate::AuthResponse::Cancel).await;
                    }
                    let auth = match result {
                        Ok(()) => provider.auth_state().await.unwrap_or_else(|error| {
                            crate::AuthState::Error {
                                message: error.to_string(),
                            }
                        }),
                        Err(error) => crate::AuthState::Error {
                            message: error.to_string(),
                        },
                    };
                    let _ = transient.send(crate::TransientEvent::ProviderAuthUpdated {
                        provider: provider_id,
                        auth,
                    });
                }
                .instrument(tracing::info_span!(
                    "agent.session.oauth_callback",
                    %session_id
                )),
            );
        }
        Ok(challenge)
    }

    async fn cancel_pending_oauth(&self, provider_id: &str, provider: Arc<dyn Provider>) {
        if let Some(pending) = self.pending_oauth.lock().await.remove(provider_id) {
            pending.cancellation.cancel();
        }
        let _ = provider.complete_auth(crate::AuthResponse::Cancel).await;
    }

    /// Cancels an in-progress managed browser flow without publishing an authentication error.
    ///
    /// # Errors
    /// Returns an error when the provider is unknown.
    pub async fn cancel_provider_auth(&self, provider_id: &str) -> Result<(), RuntimeError> {
        let provider = self.providers.get(provider_id).ok_or_else(|| {
            RuntimeError::InvalidOption(format!("unknown provider adapter: {provider_id}"))
        })?;
        self.cancel_pending_oauth(provider_id, provider).await;
        Ok(())
    }

    /// Completes a managed provider authentication flow using frontend-collected input.
    ///
    /// # Errors
    /// Returns an error for invalid flow state, credentials, or an unknown provider.
    pub async fn complete_provider_auth(
        &self,
        provider_id: &str,
        response: crate::AuthResponse,
    ) -> Result<crate::AuthState, RuntimeError> {
        let provider = self.providers.get(provider_id).ok_or_else(|| {
            RuntimeError::InvalidOption(format!("unknown provider adapter: {provider_id}"))
        })?;
        let completion = match provider.complete_auth(response).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind == crate::ProviderErrorKind::Cancelled => {
                return Err(RuntimeError::InvalidOption(error.to_string()));
            }
            Err(error) => Err(RuntimeError::InvalidOption(error.to_string())),
        };
        let result = match completion {
            Ok(()) => self.activate_authenticated_provider(provider_id, provider.clone()),
            Err(error) => Err(error),
        };
        let auth = match &result {
            Ok(()) => provider
                .auth_state()
                .await
                .unwrap_or_else(|error| crate::AuthState::Error {
                    message: error.to_string(),
                }),
            Err(error) => crate::AuthState::Error {
                message: error.to_string(),
            },
        };
        let _ = self
            .transient
            .send(crate::TransientEvent::ProviderAuthUpdated {
                provider: provider_id.to_owned(),
                auth: auth.clone(),
            });
        result?;
        Ok(auth)
    }

    fn activate_authenticated_provider(
        &self,
        provider_id: &str,
        provider: Arc<dyn Provider>,
    ) -> Result<(), RuntimeError> {
        if !self.providers.is_enabled(provider_id) {
            let snapshot = self
                .config_store
                .persist_provider_enabled(provider_id, true)?;
            self.providers.rebuild(&snapshot);
        }
        let mut settings = self
            .config_store
            .snapshot()
            .provider(provider_id)
            .cloned()
            .ok_or_else(|| {
                RuntimeError::InvalidOption(format!("provider is not configured: {provider_id}"))
            })?;
        settings.enabled = true;
        let catalog = self.catalog.clone();
        let transient = self.transient.clone();
        let provider_id = provider_id.to_owned();
        let session_id = self.id;
        tokio::spawn(
            async move {
                match catalog.refresh_if_stale(provider, &settings).await {
                    Ok(catalog) => {
                        let _ = transient.send(crate::TransientEvent::ModelCatalogUpdated {
                            provider: provider_id.clone(),
                            catalog,
                        });
                    }
                    Err(error) => {
                        tracing::warn!(provider = provider_id, %error, "provider model catalog refresh failed after authentication");
                    }
                }
            }
            .instrument(tracing::info_span!(
                "agent.session.catalog_refresh",
                %session_id
            )),
        );
        Ok(())
    }

    /// Removes a provider-managed credential without changing unrelated provider configuration.
    ///
    /// # Errors
    /// Returns an error when the provider is unknown or credential removal fails.
    pub async fn disconnect_provider(&self, provider_id: &str) -> Result<(), RuntimeError> {
        let provider = self.providers.get(provider_id).ok_or_else(|| {
            RuntimeError::InvalidOption(format!("unknown provider adapter: {provider_id}"))
        })?;
        provider
            .disconnect()
            .await
            .map_err(|error| RuntimeError::InvalidOption(error.to_string()))?;
        if self.providers.is_enabled(provider_id) {
            let snapshot = self
                .config_store
                .persist_provider_enabled(provider_id, false)?;
            self.providers.rebuild(&snapshot);
        }
        Ok(())
    }

    /// Stores an API key in the credential backend and enables the provider immediately.
    pub async fn set_provider_api_key(
        &self,
        provider_id: &str,
        key: String,
    ) -> Result<(), RuntimeError> {
        if key.trim().is_empty() {
            return self.remove_provider_api_key(provider_id).await;
        }
        let provider = self.providers.get(provider_id).ok_or_else(|| {
            RuntimeError::InvalidOption(format!("unknown provider adapter: {provider_id}"))
        })?;
        provider
            .set_api_key(key)
            .await
            .map_err(|error| RuntimeError::InvalidOption(error.to_string()))?;
        self.activate_authenticated_provider(provider_id, provider)
    }

    /// Removes a UI-managed API key. Environment credentials remain available as a fallback.
    pub async fn remove_provider_api_key(&self, provider_id: &str) -> Result<(), RuntimeError> {
        let provider = self.providers.get(provider_id).ok_or_else(|| {
            RuntimeError::InvalidOption(format!("unknown provider adapter: {provider_id}"))
        })?;
        provider
            .remove_api_key()
            .await
            .map_err(|error| RuntimeError::InvalidOption(error.to_string()))?;
        if self.providers.is_enabled(provider_id) {
            let snapshot = self
                .config_store
                .persist_provider_enabled(provider_id, false)?;
            self.providers.rebuild(&snapshot);
        }
        Ok(())
    }

    /// Forces provider model discovery, retaining the catalog manager's stale-cache fallback.
    ///
    /// # Errors
    /// Returns an error when the provider is unknown or no usable catalog is available.
    pub async fn retry_model_discovery(
        &self,
        provider_id: &str,
    ) -> Result<crate::ResolvedModelCatalog, RuntimeError> {
        let provider = self.providers.get(provider_id).ok_or_else(|| {
            RuntimeError::InvalidOption(format!("unknown provider adapter: {provider_id}"))
        })?;
        let config = self.config_store.snapshot();
        let settings = config.provider(provider_id).ok_or_else(|| {
            RuntimeError::InvalidOption(format!("provider is not configured: {provider_id}"))
        })?;
        self.catalog.refresh(provider, settings).await
    }

    /// Toggles a configured provider and persists the new state before applying it live.
    /// Providers may only be enabled when their adapter reports usable credentials.
    ///
    /// # Errors
    ///
    /// Returns an error when the provider is not configured, has no adapter, is not ready to be
    /// enabled, or the configuration cannot be persisted.
    pub async fn toggle_provider(
        &self,
        provider_id: &str,
    ) -> Result<crate::ProviderAvailability, RuntimeError> {
        let provider = self.providers.get(provider_id).ok_or_else(|| {
            RuntimeError::InvalidOption(format!("provider adapter is unavailable: {provider_id}"))
        })?;
        let mut settings = self
            .config_store
            .snapshot()
            .provider(provider_id)
            .cloned()
            .ok_or_else(|| {
                RuntimeError::InvalidOption(format!("provider is not configured: {provider_id}"))
            })?;
        let enabled = !self.providers.is_enabled(provider_id);
        let auth = provider
            .auth_state()
            .await
            .map_err(|error| RuntimeError::InvalidOption(error.to_string()))?;
        if enabled
            && !matches!(
                &auth,
                crate::AuthState::Available { .. } | crate::AuthState::Connected { .. }
            )
        {
            return Err(RuntimeError::InvalidOption(format!(
                "provider cannot be enabled until it is set up: {provider_id} ({auth:?})"
            )));
        }

        let snapshot = self
            .config_store
            .persist_provider_enabled(provider_id, enabled)?;
        self.providers.rebuild(&snapshot);

        if enabled {
            settings.enabled = true;
            let catalog = self.catalog.clone();
            let refresh_provider = provider.clone();
            let transient = self.transient.clone();
            let provider_id = provider_id.to_owned();
            let session_id = self.id;
            tokio::spawn(
                async move {
                    match catalog.refresh_if_stale(refresh_provider, &settings).await {
                        Ok(catalog) => {
                            let _ = transient.send(crate::TransientEvent::ModelCatalogUpdated {
                                provider: provider_id,
                                catalog,
                            });
                        }
                        Err(error) => {
                            tracing::warn!(%error, "provider model catalog refresh failed");
                        }
                    }
                }
                .instrument(tracing::info_span!(
                    "agent.session.catalog_refresh",
                    %session_id
                )),
            );
        }

        Ok(crate::ProviderAvailability {
            descriptor: provider.descriptor().clone(),
            enabled,
            auth,
            has_managed_api_key: provider.has_managed_api_key().await,
        })
    }

    /// Loads the immediate cache/seed model catalog for an enabled picker row.
    ///
    /// # Errors
    ///
    /// Returns an error when the provider is unknown or its catalog cannot be read.
    pub async fn models(
        &self,
        provider_id: &str,
    ) -> Result<crate::ResolvedModelCatalog, RuntimeError> {
        self.startup.wait_for_models().await;
        let mut resolved = if provider_id == "mock" {
            let provider = self.providers.get(provider_id).ok_or_else(|| {
                RuntimeError::InvalidOption("mock provider is unavailable".into())
            })?;
            let catalog = provider
                .discover_models()
                .ok_or_else(|| {
                    RuntimeError::InvalidOption(format!(
                        "provider does not own model discovery: {provider_id}"
                    ))
                })?
                .await
                .map_err(|error| RuntimeError::InvalidOption(error.to_string()))?;
            crate::ResolvedModelCatalog {
                catalog,
                aliases: std::collections::BTreeMap::new(),
                source: crate::ModelCatalogSource::BundledSeed,
                age_millis: None,
                refresh_error: None,
            }
        } else {
            let config = self.config_store.snapshot();
            let settings = config.provider(provider_id).ok_or_else(|| {
                RuntimeError::InvalidOption(format!("provider is not configured: {provider_id}"))
            })?;
            let provider = self.providers.get(provider_id).ok_or_else(|| {
                RuntimeError::InvalidOption(format!("unknown provider adapter: {provider_id}"))
            })?;
            self.catalog
                .current(
                    provider.descriptor(),
                    settings,
                    provider.subscription_plan().await.as_deref(),
                )
                .await?
        };
        if let Some(path) = self.store.database_path() {
            self.global_store.project_conversation(path).await?;
        }
        let recent = self.global_store.load_recent_models(provider_id, 8).await?;
        rank_models_by_recency(&mut resolved.catalog.models, &recent);
        Ok(resolved)
    }

    /// Returns the enabled-provider model rows used by the model picker.
    ///
    /// # Errors
    ///
    /// Returns an error when a provider model catalog cannot be loaded.
    pub async fn model_picker_rows(
        &self,
    ) -> Result<Vec<crate::presentation::ModelPickerRow>, RuntimeError> {
        let providers = self
            .providers()
            .await
            .into_iter()
            .filter(|provider| provider.enabled)
            .map(|provider| provider.descriptor.id)
            .collect::<Vec<_>>();
        let current = self
            .model_selection()
            .await?
            .and_then(|(provider, model, _)| {
                crate::provider::ModelRef::parse(&format!("{provider}/{model}")).ok()
            });
        let favourites = self.model_favourites()?;
        let mut rows = Vec::new();
        for provider in providers {
            let provider_rows = crate::presentation::project_model_picker(
                &provider,
                current.as_ref(),
                &favourites,
                self.models(&provider).await?,
            );
            rows.extend(provider_rows);
        }
        crate::presentation::prioritize_current_model(&mut rows);
        Ok(rows)
    }

    /// Resolves a model argument using the same precedence and filtering as the model picker.
    ///
    /// # Errors
    ///
    /// Returns an error when the enabled-provider model catalogs cannot be loaded.
    pub async fn resolve_model_picker_row(
        &self,
        input: &str,
    ) -> Result<Option<crate::presentation::ModelPickerRow>, RuntimeError> {
        let rows = self.model_picker_rows().await?;
        Ok(crate::presentation::resolve_model_picker_row(&rows, input).cloned())
    }

    /// Resolves a model using one provider's catalog, including a disabled
    /// provider used by an explicitly requested headless exec run.
    pub async fn resolve_model_picker_row_for_provider(
        &self,
        provider: &str,
        input: &str,
    ) -> Result<Option<crate::presentation::ModelPickerRow>, RuntimeError> {
        let current = self
            .model_selection()
            .await?
            .and_then(|(provider, model, _)| {
                crate::provider::ModelRef::parse(&format!("{provider}/{model}")).ok()
            });
        let favourites = self.model_favourites()?;
        let rows = crate::presentation::project_model_picker(
            provider,
            current.as_ref(),
            &favourites,
            self.models(provider).await?,
        );
        Ok(crate::presentation::resolve_model_picker_row(&rows, input).cloned())
    }

    /// Fuzzily completes workspace-relative, home-relative, or absolute paths.
    ///
    /// Home and absolute queries list one explicitly requested directory at a
    /// time. Reading a selected path outside the workspace remains subject to
    /// the normal external-path permission boundary.
    ///
    /// # Errors
    ///
    /// Returns a structured runtime error when the directory cannot be listed safely.
    pub async fn complete_paths(
        &self,
        query: &str,
    ) -> Result<Vec<crate::WorkspaceEntry>, RuntimeError> {
        let completion = self
            .workspace_state
            .read()
            .expect("workspace state lock poisoned")
            .path_completion
            .clone();
        completion.complete(query).await
    }

    /// Returns warm path-completion results without waiting for the session
    /// corpus to finish building.
    #[must_use]
    pub fn complete_paths_cached(
        &self,
        query: &str,
    ) -> Option<Result<Vec<crate::WorkspaceEntry>, RuntimeError>> {
        self.workspace_state
            .read()
            .expect("workspace state lock poisoned")
            .path_completion
            .complete_cached(query)
    }

    /// Starts a fresh reusable corpus for one frontend completion token.
    #[must_use]
    pub fn start_path_completion(&self) -> PathCompletionSession {
        PathCompletionSession::new(
            self.workspace_state
                .read()
                .expect("workspace state lock poisoned")
                .tools
                .clone(),
            self.config_store.clone(),
        )
    }

    /// Returns the direct visible workspace entries used by `@` completion.
    ///
    /// Unlike the generic list tool, hidden entries are omitted because this
    /// is the initial, lightweight page shown before a recursive query.
    ///
    /// # Errors
    ///
    /// Returns a structured runtime error when the workspace cannot be listed safely.
    pub async fn directory_completion_listing(
        &self,
    ) -> Result<Vec<crate::WorkspaceEntry>, RuntimeError> {
        let config = self.config_store.snapshot();
        self.workspace_state
            .read()
            .expect("workspace state lock poisoned")
            .tools
            .clone()
            .with_file_picker_options(
                config.file_picker_respect_gitignore(),
                config.file_picker_hide_hidden_files(),
            )
            .visible_workspace_directory()
            .map_err(|error| RuntimeError::InvalidOption(error.to_string()))
    }

    /// Durably submits a command to the session orchestrator.
    ///
    /// # Errors
    ///
    /// Returns an error when the runtime has stopped or the command is not implemented by the
    /// current implementation phase.
    pub async fn submit(&self, command: SessionCommand) -> Result<CommandId, RuntimeError> {
        if matches!(*self.access.borrow(), crate::SessionAccess::Observer { .. }) {
            return Err(RuntimeError::ReadOnlyObserver(self.id));
        }
        if command.version != crate::API_VERSION {
            return Err(RuntimeError::UnsupportedVersion {
                actual: command.version,
                supported: crate::API_VERSION,
            });
        }
        if matches!(&command.action, crate::SessionAction::Cancel) {
            self.cancel();
            if let Ok(mut latest) = self.latest_command.write() {
                *latest = Some(command.id);
            }
            return Ok(command.id);
        }
        if let crate::SessionAction::RunBash { command: bash } = &command.action {
            self.run_detached_bash(bash.clone()).await?;
            if let Ok(mut latest) = self.latest_command.write() {
                *latest = Some(command.id);
            }
            return Ok(command.id);
        }
        if matches!(
            &command.action,
            crate::SessionAction::ReloadStartupResources
        ) {
            self.reload_startup_resources().await;
            if let Ok(mut latest) = self.latest_command.write() {
                *latest = Some(command.id);
            }
            return Ok(command.id);
        }
        let composer_entry = composer_entry_for_command(&command);
        let command_id = command.id;
        let (accepted, response) = oneshot::channel();
        self.commands
            .send(CommandRequest { command, accepted })
            .await
            .map_err(|_| RuntimeError::RuntimeStopped)?;
        response.await.map_err(|_| RuntimeError::RuntimeStopped)??;
        if let Some(entry) = composer_entry
            && let Ok(mut entries) = self.composer_history.write()
        {
            entries.retain(|candidate| candidate != &entry);
            entries.push(entry);
        }
        if let Ok(mut latest) = self.latest_command.write() {
            *latest = Some(command_id);
        }
        Ok(command_id)
    }

    async fn reload_startup_resources(&self) {
        self.startup.reload().await;
        let config = self.config_store.snapshot();
        let (agent, _) = self
            .active_profiles()
            .await
            .unwrap_or_else(|_| (config.default_agent().into(), config.default_mode().into()));
        if let Err(error) = self
            .mcp_supervisor
            .start_eager(&self.mcp_config, &agent)
            .await
        {
            tracing::warn!(%error, "MCP reconciliation failed during reload");
        }
        self.startup.refresh_remote_in_background(true);
    }

    #[must_use]
    pub fn subscribe(&self, after: Option<EventCursor>) -> RuntimeEventStream {
        self.subscribe_inner(after, true)
    }

    fn subscribe_inner(
        &self,
        after: Option<EventCursor>,
        project_markdown: bool,
    ) -> RuntimeEventStream {
        let store = self.store.clone();
        let mut transient = self.transient.subscribe();
        let mut interactions = self.interactions.subscribe();
        let conversation_id = self.id;
        let mut events = store.subscribe_live();
        let (sender, receiver) = mpsc::channel(self.event_capacity);
        tokio::spawn(
            async move {
                let mut markdown_projection = MarkdownProjection::default();
                let initial_interaction = interactions.borrow_and_update().clone();
                if let Some(request) = initial_interaction
                    && sender
                        .send(Ok(RuntimeEvent::Interaction {
                            version: crate::API_VERSION,
                            request: Box::new(request),
                        }))
                        .await
                        .is_err()
                {
                    return;
                }
                // This stream is process-local and live-only. Historical state is
                // recovered through snapshots and node-backed transcript pages.
                loop {
                    tokio::select! {
                        () = sender.closed() => break,
                        event = events.recv() => {
                            let mut event = match event {
                                Ok(event) => event,
                                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                                Err(broadcast::error::RecvError::Closed) => break,
                            };
                            if project_markdown {
                                markdown_projection.project(&mut event.kind);
                            }
                            if after.is_some_and(|cursor| event.cursor.0 <= cursor.0) {
                                continue;
                            }
                            if sender.send(Ok(RuntimeEvent::Durable(event))).await.is_err() {
                                break;
                            }
                        }
                        event = transient.recv() => {
                            match event {
                                Ok(event) => {
                                    if sender.send(Ok(RuntimeEvent::Transient {
                                        version: crate::API_VERSION,
                                        event,
                                    })).await.is_err() {
                                        break;
                                    }
                                }
                                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                                Err(broadcast::error::RecvError::Closed) => break,
                            }
                        }
                        changed = interactions.changed() => {
                            if changed.is_err() {
                                break;
                            }
                            let interaction = interactions.borrow_and_update().clone();
                            match interaction {
                                Some(request) => {
                                    if sender
                                        .send(Ok(RuntimeEvent::Interaction {
                                            version: crate::API_VERSION,
                                            request: Box::new(request),
                                        }))
                                        .await
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                                None => {
                                    if sender
                                        .send(Ok(RuntimeEvent::InteractionCleared {
                                            version: crate::API_VERSION,
                                        }))
                                        .await
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            .instrument(tracing::info_span!(
                "agent.session.subscription",
                session_id = %conversation_id
            )),
        );
        RuntimeEventStream {
            inner: ReceiverStream::new(receiver),
        }
    }

    /// Loads a lazily retained large tool payload by its durable blob ID.
    ///
    /// # Errors
    ///
    /// Returns a database error when the blob is missing or cannot be read.
    pub async fn load_blob(&self, id: crate::BlobId) -> Result<Vec<u8>, RuntimeError> {
        self.store.load_blob(id).await
    }

    /// Loads an MCP call's complete details for explicit expanded-output inspection.
    /// Returns `None` for an unknown node, a node outside this conversation, or
    /// a non-MCP tool call. Retained output is hydrated only by this request;
    /// ordinary history and transcript projections remain compact.
    ///
    /// # Errors
    /// Returns an error if history, retained output, or workspace state cannot
    /// be read, or the retained blob reference or JSON is invalid.
    pub async fn mcp_call_detail(
        &self,
        node_id: crate::NodeId,
    ) -> Result<Option<crate::presentation::McpCall>, RuntimeError> {
        use crate::presentation::{ToolActivityGroup, ToolActivityRef, ToolActivityStatus};

        // Scope the lookup before following any result or blob reference.
        let history = self.history().await?;
        let Some(node) = history
            .iter()
            .find(|node| node.id == node_id && node.kind == crate::NodeKind::ToolCall)
        else {
            return Ok(None);
        };
        let Some(name) = node.content.get("name").and_then(serde_json::Value::as_str) else {
            return Ok(None);
        };
        if !name.starts_with("mcp__") {
            return Ok(None);
        }
        let result = history.iter().rev().find(|result| {
            result.kind == crate::NodeKind::ToolResult && result.owner_id == Some(node_id)
        });
        let output = if let Some(result) = result {
            if let Some(blob_id) = result
                .content
                .get("output_blob_id")
                .filter(|value| !value.is_null())
            {
                let blob_id = serde_json::from_value::<crate::BlobId>(blob_id.clone())?;
                Some(serde_json::from_slice(&self.load_blob(blob_id).await?)?)
            } else {
                result.content.get("output").cloned()
            }
        } else {
            None
        };
        let status = match result {
            Some(result) if result.content["is_error"].as_bool().unwrap_or(false) => {
                ToolActivityStatus::Failed
            }
            Some(_) => ToolActivityStatus::Succeeded,
            None => ToolActivityStatus::Pending,
        };
        let groups = crate::presentation::project_tool_activities(
            [ToolActivityRef {
                node_id: Some(node_id),
                name,
                arguments: &node.content["arguments"],
                result: output.as_ref(),
                status,
            }],
            &self.working_directory()?,
        );
        Ok(groups.into_iter().find_map(|group| match group {
            ToolActivityGroup::Mcp { mut call } => {
                // MCP's timer starts at dispatch, unlike legacy outer durations
                // that included permission approval. Prefer it on old records too.
                call.duration_millis = call.duration_millis.or_else(|| {
                    result.and_then(|result| result.content["duration_millis"].as_u64())
                });
                Some(call)
            }
            _ => None,
        }))
    }

    /// Normalizes raster clipboard data to PNG and retains it in this
    /// conversation without exposing the bytes through transcript events.
    pub async fn store_clipboard_image(
        &self,
        bytes: &[u8],
        number: u64,
    ) -> Result<crate::ImageAttachment, RuntimeError> {
        const MAX_CLIPBOARD_IMAGE_BYTES: usize = 32 * 1024 * 1024;
        if bytes.len() > MAX_CLIPBOARD_IMAGE_BYTES {
            return Err(RuntimeError::InvalidOption(
                "clipboard image exceeds the 32 MiB limit".into(),
            ));
        }
        let (png, width, height) = crate::images::normalize_to_png(bytes)?;
        if png.len() > MAX_CLIPBOARD_IMAGE_BYTES {
            return Err(RuntimeError::InvalidOption(
                "normalized clipboard image exceeds the 32 MiB limit".into(),
            ));
        }
        let sha256 = crate::images::sha256(&png);
        let blob_id = self
            .store
            .store_image_blob(crate::BlobId::new(), sha256, png.clone())
            .await?;
        let metadata = crate::images::metadata(number, blob_id, &png, width, height);
        Ok(metadata)
    }

    /// Returns the complete append-only conversation tree in durable order.
    ///
    /// # Errors
    ///
    /// Returns a database error when the conversation history cannot be loaded.
    pub async fn history(&self) -> Result<Vec<crate::HistoryNode>, RuntimeError> {
        self.store.load_history(self.id).await
    }

    /// Returns the user/assistant message tree used by conversation navigation.
    /// Non-message ancestry, including typed system nodes, is transparent.
    ///
    /// # Errors
    /// Returns a database error when the conversation history cannot be loaded.
    pub async fn message_history(&self) -> Result<Vec<crate::HistoryNode>, RuntimeError> {
        let history = self.store.load_history(self.id).await?;
        Ok(project_message_history(&history))
    }

    /// Returns the messages on the currently active branch in chronological order.
    ///
    /// # Errors
    /// Returns a database error when the conversation history cannot be loaded.
    pub async fn active_message_history(&self) -> Result<Vec<crate::HistoryNode>, RuntimeError> {
        let mut history = self.message_history().await?;
        let parents = history
            .iter()
            .map(|node| (node.id, node.parent_id))
            .collect::<HashMap<_, _>>();
        let mut active_branch = std::collections::HashSet::new();
        let mut current = history.iter().find(|node| node.active).map(|node| node.id);
        while let Some(id) = current {
            active_branch.insert(id);
            current = parents.get(&id).copied().flatten();
        }
        history.retain(|node| active_branch.contains(&node.id));
        Ok(history)
    }

    /// Returns the active-branch navigation rows with their durable revision.
    pub async fn active_branch_history_snapshot(
        &self,
    ) -> Result<crate::presentation::HistoryRowsSnapshot, RuntimeError> {
        self.history_snapshot(crate::presentation::HistoryScope::ActiveBranch)
            .await
    }

    /// Returns the single frontend-neutral history projection API.
    pub async fn history_snapshot(
        &self,
        scope: crate::presentation::HistoryScope,
    ) -> Result<crate::presentation::HistoryRowsSnapshot, RuntimeError> {
        let mut snapshot = self.project_full_history_snapshot().await?;
        if scope == crate::presentation::HistoryScope::FullTree {
            return Ok(snapshot);
        }
        let history = &mut snapshot.rows;
        let parents = history
            .iter()
            .map(|row| (row.id, row.parent_id))
            .collect::<HashMap<_, _>>();
        let mut active_branch = std::collections::HashSet::new();
        let mut current = history.iter().find(|row| row.active).map(|row| row.id);
        while let Some(id) = current {
            active_branch.insert(id);
            current = parents.get(&id).copied().flatten();
        }
        history.retain(|row| active_branch.contains(&row.id));
        Ok(snapshot)
    }

    /// Returns the complete navigable history projection and the durable
    /// revision read atomically with it.
    pub async fn tree_history_snapshot(
        &self,
    ) -> Result<crate::presentation::HistoryRowsSnapshot, RuntimeError> {
        self.history_snapshot(crate::presentation::HistoryScope::FullTree)
            .await
    }

    /// Projects the durable ancestor branch ending at `target` without
    /// changing the active branch or any runtime state.
    pub async fn history_preview(
        &self,
        target: crate::NodeId,
    ) -> Result<crate::HistoryPreviewSnapshot, RuntimeError> {
        let hydration = self.store.load_history_hydration(self.id).await?;
        let Some(target_index) = hydration.history.iter().position(|node| node.id == target) else {
            return Err(RuntimeError::NodeNotFoundOrInvalidState(target, self.id));
        };
        let parents = hydration
            .history
            .iter()
            .map(|node| (node.id, node.parent_id))
            .collect::<HashMap<_, _>>();
        let mut branch = HashSet::new();
        let mut cursor = Some(target);
        while let Some(id) = cursor {
            branch.insert(id);
            cursor = parents.get(&id).copied().flatten();
        }
        // A title sidecar is recorded after its anchor without advancing the
        // branch. Include titles anchored at the selected tip too, matching
        // paged replay and fork retention; sequence alone would hide them.
        let history = hydration
            .history
            .into_iter()
            .enumerate()
            .filter(|(index, node)| {
                (*index <= target_index && branch.contains(&node.id))
                    || (node.kind == crate::NodeKind::System
                        && node
                            .parent_id
                            .is_some_and(|parent| branch.contains(&parent))
                        && (node.content["system_type"] == "title_change"
                            || (*index <= target_index
                                && node
                                    .content
                                    .get("transcript")
                                    .and_then(serde_json::Value::as_str)
                                    .is_some_and(|kind| matches!(kind, "notice" | "interrupt")))))
            })
            .map(|(_, node)| node)
            .collect::<Vec<_>>();
        let source_events = history
            .iter()
            .enumerate()
            .map(
                |(index, node)| crate::store::TranscriptPageEvent::NodeAppended {
                    cursor: crate::EventCursor(u64::try_from(index).unwrap_or(u64::MAX)),
                    node_id: node.id,
                },
            )
            .collect();
        let events = self.transcript_events_from_page(history, source_events);
        let blocks = transcript_blocks_with_web_search_provider(
            &events,
            &hydration.workspace,
            &crate::TurnState::Idle,
            &hydration.terminals,
            &hydration.agent_runs,
            None,
            self.config_store.snapshot().web_search().provider(),
        );
        Ok(crate::HistoryPreviewSnapshot {
            target,
            transcript: crate::TranscriptWindow::new(blocks, None),
        })
    }

    async fn project_full_history_snapshot(
        &self,
    ) -> Result<crate::presentation::HistoryRowsSnapshot, RuntimeError> {
        let hydration = self.store.load_history_hydration(self.id).await?;
        let revision = hydration.revision;
        let rows = tokio::task::spawn_blocking(move || {
            let _span = tracing::trace_span!(
                "agent.history.projection",
                nodes = hydration.history.len(),
                terminals = hydration.terminals.len()
            )
            .entered();
            crate::presentation::project_history_rows_with_context(
                &hydration.history,
                &hydration.workspace,
                &hydration.terminals,
            )
        })
        .await
        .map_err(|_| RuntimeError::RuntimeStopped)?;
        Ok(crate::presentation::HistoryRowsSnapshot { revision, rows })
    }

    /// Lists resumable conversations for this session's canonical workspace.
    ///
    /// # Errors
    ///
    /// Returns a database error when the workspace or conversation list cannot be loaded.
    pub async fn conversations(&self) -> Result<Vec<crate::ConversationSummary>, RuntimeError> {
        self.query_conversations(None).await
    }

    /// Queries resumable conversations in this session's canonical workspace.
    /// Search matches normalized titles and every committed user message.
    pub async fn query_conversations(
        &self,
        search: Option<String>,
    ) -> Result<Vec<crate::ConversationSummary>, RuntimeError> {
        self.query_conversations_with_archived(search, false).await
    }

    pub async fn query_conversations_with_archived(
        &self,
        search: Option<String>,
        include_archived: bool,
    ) -> Result<Vec<crate::ConversationSummary>, RuntimeError> {
        let workspace = self.store.load_workspace(self.id).await?;
        if self.takeover_runtime.publish_conversations
            && let Err(error) = self
                .global_store
                .project_conversation(self.takeover_runtime.conversation_path(self.id))
                .await
        {
            tracing::debug!(session_id = %self.id, %error, "session projection deferred");
        }
        self.global_store
            .conversations(crate::ConversationQuery {
                workspace: Some(workspace),
                search,
                include_archived,
            })
            .await
    }
}

fn transcript_event_visible_on_branch(
    branch: &HashSet<crate::NodeId>,
    kind: &crate::DurableEventKind,
) -> bool {
    match kind {
        crate::DurableEventKind::NodeAppended {
            node_id,
            parent_id,
            node_kind: crate::NodeKind::System,
            content,
            ..
        } => match content
            .get("transcript")
            .and_then(serde_json::Value::as_str)
        {
            Some("recap") => branch.contains(node_id),
            Some("interrupt" | "notice") => {
                parent_id.is_some_and(|parent_id| branch.contains(&parent_id))
            }
            _ => false,
        },
        crate::DurableEventKind::NodeAppended { node_id, .. }
        | crate::DurableEventKind::AssistantDelta { node_id, .. }
        | crate::DurableEventKind::AssistantFailed { node_id, .. }
        | crate::DurableEventKind::ModelUsageRecorded { node_id, .. }
        | crate::DurableEventKind::NodeStatusChanged { node_id, .. } => branch.contains(node_id),
        kind if kind.is_branch_system_log() => kind
            .system_log_node()
            .is_none_or(|node_id| branch.contains(&node_id)),
        // Metadata and queue events are not branch nodes. Retain them to reconstruct frontend
        // state and carry the durable high-water mark without admitting abandoned-branch content.
        _ => true,
    }
}

fn retain_only_trailing_recap(
    blocks: &mut Vec<crate::TranscriptBlock>,
    active_id: Option<&crate::TranscriptBlockId>,
) {
    blocks.retain(|block| {
        !matches!(block.kind, crate::TranscriptBlockKind::Recap { .. })
            || active_id == Some(&block.id)
    });
}

fn merge_composer_seed(
    history: &Arc<std::sync::RwLock<Vec<crate::ComposerHistoryEntry>>>,
    transient: &broadcast::Sender<crate::TransientEvent>,
    records: Vec<crate::store::ComposerHistoryRecord>,
) {
    let mut changed = false;
    if let Ok(mut entries) = history.write() {
        for entry in records
            .into_iter()
            .map(crate::store::ComposerHistoryRecord::into_entry)
        {
            if !entries.contains(&entry) {
                entries.push(entry);
                changed = true;
            }
        }
    }
    if changed {
        let _ = transient.send(crate::TransientEvent::ComposerHistoryUpdated);
    }
}

enum TranscriptPageContext<'a> {
    LiveTail {
        turn: &'a crate::TurnState,
        terminals: &'a [crate::TerminalSnapshot],
        agent_runs: &'a [crate::AgentRun],
        held_permission: Option<&'a crate::PermissionResource>,
    },
    HistoricalPage,
}

fn transcript_projection(
    events: &[crate::DurableEvent],
    workspace: &std::path::Path,
) -> crate::TranscriptProjection {
    let mut projection = crate::TranscriptProjection::default();
    let mut assistants = HashMap::<crate::NodeId, usize>::new();
    let mut tools = HashMap::<crate::NodeId, usize>::new();
    // Permission decisions are durable audit records and can precede the held
    // tool node. Decide visibility before creating any presentation activity.
    let denied_tools = events
        .iter()
        .filter_map(|event| match &event.kind {
            crate::DurableEventKind::NodeAppended {
                node_kind: crate::NodeKind::PermissionDecision,
                owner_id: Some(owner_id),
                content,
                ..
            } => serde_json::from_value::<crate::PermissionAudit>(content.clone())
                .ok()
                .filter(|audit| audit.outcome == crate::PermissionEffect::Deny)
                .map(|_| *owner_id),
            _ => None,
        })
        .collect::<HashSet<_>>();
    for event in events {
        match &event.kind {
            crate::DurableEventKind::NodeAppended {
                node_id,
                node_kind,
                content,
                status,
                owner_id,
                ..
            } => match node_kind {
                crate::NodeKind::UserMessage => {
                    if let Some(text) = content.get("text").and_then(serde_json::Value::as_str) {
                        projection.entries.push(crate::TranscriptEntry::User {
                            node_id: *node_id,
                            label: content
                                .get("display_label")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_owned),
                            text: content
                                .get("display_text")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or(text)
                                .to_owned(),
                            images: content.get("images").cloned().and_then(|value| serde_json::from_value(value).ok()).unwrap_or_default(),
                            image_chips: content.get("image_chips").cloned().and_then(|value| serde_json::from_value(value).ok()).unwrap_or_default(),
                        });
                    }
                }
                crate::NodeKind::AssistantMessage => {
                    let source = content
                        .get("text")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    if content.get("flavor").and_then(serde_json::Value::as_str) == Some("plan") {
                        projection.entries.push(crate::TranscriptEntry::Plan {
                            node_id: *node_id,
                            document: crate::presentation::parse_markdown(&source),
                            source,
                            status: status.clone(),
                        });
                    } else {
                        assistants.insert(*node_id, projection.entries.len());
                        projection.entries.push(crate::TranscriptEntry::Assistant {
                            node_id: *node_id,
                            document: crate::presentation::parse_markdown(&source),
                            source,
                            status: status.clone(),
                        });
                    }
                }
                crate::NodeKind::AcceptedPlan => {
                    if let Some(source) = content
                        .get("plan_markdown")
                        .and_then(serde_json::Value::as_str)
                    {
                        let clear_context = content
                            .get("reset_context")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false);
                        let compact_context = content
                            .get("compact_context")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false);
                        if clear_context {
                            // An accepted plan can be the reset boundary itself;
                            // it replaces the separate clear marker used by older
                            // sessions.
                            projection.entries.clear();
                            projection.live_assistant = None;
                            projection.live_plan = None;
                            assistants.clear();
                            tools.clear();
                        }
                        projection.entries.push(crate::TranscriptEntry::AcceptedPlan {
                            node_id: *node_id,
                            source: source.to_owned(),
                            document: crate::presentation::parse_markdown(source),
                            clear_context,
                            compact_context,
                            compaction_summary: None,
                        });
                    }
                }
                crate::NodeKind::CompactionSummary => {
                    let summary = content
                        .get("summary")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    if let Some(reapplied_node_id) = content
                        .get("reapplied_node_id")
                        .and_then(serde_json::Value::as_str)
                        .and_then(|id| id.parse::<crate::NodeId>().ok())
                        && let Some(crate::TranscriptEntry::AcceptedPlan {
                            compaction_summary,
                            ..
                        }) =
                            projection.entries.iter_mut().find(|entry| matches!(
                                entry,
                                crate::TranscriptEntry::AcceptedPlan { node_id, .. }
                                    if *node_id == reapplied_node_id
                            ))
                    {
                        *compaction_summary = Some(summary);
                        continue;
                    }
                    projection.entries.push(crate::TranscriptEntry::Compacted {
                        node_id: *node_id,
                        summary,
                    });
                }
                crate::NodeKind::ToolCall => {
                    if denied_tools.contains(node_id) {
                        continue;
                    }
                    let name = content
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("tool");
                    tools.insert(*node_id, projection.entries.len());
                    projection.entries.push(crate::TranscriptEntry::Tool {
                        node_id: *node_id,
                        name: name.to_owned(),
                        arguments: content
                            .get("arguments")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null),
                        result: None,
                        failed: false,
                    });
                }
                crate::NodeKind::ToolResult => {
                    if let Some(owner_id) = owner_id
                        && let Some(index) = tools.get(owner_id).copied()
                        && let Some(crate::TranscriptEntry::Tool { result, failed, .. }) =
                            projection.entries.get_mut(index)
                    {
                        *result = content.get("output").cloned();
                        *failed = content
                            .get("is_error")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false);
                    }
                }
                crate::NodeKind::PermissionDecision => {
                    if let Ok(audit) =
                        serde_json::from_value::<crate::PermissionAudit>(content.clone())
                        && audit.outcome == crate::PermissionEffect::Deny
                        && let Some(summary) =
                            crate::presentation::permission_denial_summary(&audit, workspace)
                    {
                            projection
                                .entries
                                .push(crate::TranscriptEntry::PermissionDenied {
                                    node_id: *node_id,
                                    resource: summary.resource,
                                    reason: summary.reason,
                                });
                    }
                }
                crate::NodeKind::System
                    if content
                        .get("transcript")
                        .and_then(serde_json::Value::as_str)
                        == Some("recap") =>
                {
                    if let Some(text) = content.get("text").and_then(serde_json::Value::as_str) {
                        projection.entries.push(crate::TranscriptEntry::Recap {
                            node_id: *node_id,
                            text: text.to_owned(),
                        });
                    }
                }
                crate::NodeKind::System
                    if content
                        .get("transcript")
                        .and_then(serde_json::Value::as_str)
                        == Some("hidden") =>
                {
                    let warnings = content
                        .pointer("/local_context/warnings")
                        .and_then(serde_json::Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(|warning| {
                            Some(format!(
                                "{}: {}",
                                warning.get("path")?.as_str()?,
                                warning.get("message")?.as_str()?
                            ))
                        })
                        .collect::<Vec<_>>();
                    if !warnings.is_empty() {
                        projection.entries.push(crate::TranscriptEntry::Notice {
                            node_id: *node_id,
                            message: format!(
                                "Local context reload warning:\n{}",
                                warnings
                                    .into_iter()
                                    .map(|warning| format!("- {warning}"))
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            ),
                        });
                    }
                }
                crate::NodeKind::System
                    if content
                        .get("transcript")
                        .and_then(serde_json::Value::as_str)
                        == Some("interrupt") =>
                {
                    projection.entries.push(crate::TranscriptEntry::Interrupt {
                        node_id: *node_id,
                        queued_steering: content
                            .get("queued_steering")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false),
                    });
                }
                crate::NodeKind::System => {
                    if let Some(transition) = content
                        .get("workspace_transition")
                        .cloned()
                        .and_then(|transition| serde_json::from_value::<crate::runtime::worktrees::WorkspaceTransition>(transition).ok())
                        .filter(|transition| transition.reason != "missing_cwd_recovered")
                    {
                        let completed_tool = projection.entries.last().and_then(|entry| {
                            let crate::TranscriptEntry::Tool {
                                name,
                                result: Some(result),
                                failed: false,
                                ..
                            } = entry
                            else {
                                return None;
                            };
                            (matches!(name.as_str(), "enter_worktree" | "change_working_directory")
                                && result.get("cwd").and_then(serde_json::Value::as_str)
                                    == Some(transition.cwd.to_string_lossy().as_ref()))
                            .then(|| name.clone())
                        });
                        if completed_tool.is_some() {
                            projection.entries.pop();
                        }
                        let entering_worktree = completed_tool.as_deref()
                            == Some("enter_worktree")
                            || transition.worktree.is_some();
                        let target = if entering_worktree {
                            crate::runtime::worktrees::display_worktree_target(
                                &transition.project_dir,
                                &transition.previous_cwd,
                                &transition.cwd,
                            )
                        } else {
                            crate::runtime::worktrees::display_directory_target(
                                &transition.project_dir,
                                &transition.previous_cwd,
                                &transition.cwd,
                            )
                        };
                        projection.entries.push(crate::TranscriptEntry::WorkspaceTransition {
                            node_id: *node_id,
                            label: if entering_worktree {
                                "Enter Worktree"
                            } else {
                                "Change Working Directory"
                            }
                            .into(),
                            target,
                            base: transition.base.filter(|base| {
                                !matches!(base.as_str(), "head" | "fresh")
                            }),
                            path: Some(transition.cwd),
                        });
                    } else if let Some(message) =
                        content.get("message").and_then(serde_json::Value::as_str)
                    {
                        projection.entries.push(crate::TranscriptEntry::Notice {
                            node_id: *node_id,
                            message: message.to_owned(),
                        });
                    }
                }
                _ => {}
            },
            crate::DurableEventKind::ConversationTitleChanged {
                title,
                source: crate::ConversationTitleSource::Manual,
                ..
            } => {
                projection.entries.push(crate::TranscriptEntry::SystemLog {
                    cursor: event.cursor,
                    message: format!("Session renamed to {title}"),
                });
            }
            crate::DurableEventKind::ModelSelectionChanged {
                provider,
                model,
                pending: false,
                node_id: Some(_),
                ..
            } => {
                let message = format!("Changed model to {provider}/{model}");
                if let Some(crate::TranscriptEntry::SystemLog { cursor, message: existing }) =
                    projection.entries.last_mut().filter(|entry| {
                        matches!(entry, crate::TranscriptEntry::SystemLog { message, .. } if message.starts_with("Changed model to "))
                    })
                {
                    *cursor = event.cursor;
                    *existing = message;
                } else {
                    projection.entries.push(crate::TranscriptEntry::SystemLog {
                        cursor: event.cursor,
                        message,
                    });
                }
            }
            crate::DurableEventKind::AgentChanged {
                agent,
                pending: false,
                node_id: Some(_),
            } => {
                let message = format!("Changed agent to {agent}");
                if let Some(crate::TranscriptEntry::SystemLog { cursor, message: existing }) =
                    projection.entries.last_mut().filter(|entry| {
                        matches!(entry, crate::TranscriptEntry::SystemLog { message, .. } if message.starts_with("Changed agent to "))
                    })
                {
                    *cursor = event.cursor;
                    *existing = message;
                } else {
                    projection.entries.push(crate::TranscriptEntry::SystemLog {
                        cursor: event.cursor,
                        message,
                    });
                }
            }
            crate::DurableEventKind::AssistantDelta {
                node_id,
                delta,
                document,
            } => {
                if let Some(index) = assistants.get(node_id).copied()
                    && let Some(crate::TranscriptEntry::Assistant {
                        source,
                        document: stored,
                        ..
                    }) = projection.entries.get_mut(index)
                {
                    source.push_str(delta);
                    *stored = document.clone();
                }
            }
            crate::DurableEventKind::PlanStarted { node_id } => {
                if let Some(index) = assistants.remove(node_id)
                    && let Some(entry) = projection.entries.get_mut(index)
                {
                    *entry = crate::TranscriptEntry::Plan {
                        node_id: *node_id,
                        source: String::new(),
                        document: crate::MarkdownDocument::default(),
                        status: "streaming".into(),
                    };
                }
            }
            crate::DurableEventKind::PlanDelta { node_id, delta, document } => {
                if let Some(crate::TranscriptEntry::Plan { source, document: stored, .. }) =
                    projection.entries.iter_mut().find(|entry| matches!(entry, crate::TranscriptEntry::Plan { node_id: id, .. } if id == node_id))
                {
                    source.push_str(delta);
                    *stored = document.clone();
                }
            }
            crate::DurableEventKind::AssistantFailed {
                node_id, message, ..
            } => {
                if let Some(index) = assistants.get(node_id).copied()
                    && let Some(crate::TranscriptEntry::Assistant {
                        source,
                        document,
                        status,
                        ..
                    }) = projection.entries.get_mut(index)
                {
                    source.push_str(&format!("\n[error: {message}]"));
                    *document = crate::presentation::parse_markdown(source);
                    *status = "failed".into();
                }
            }
            crate::DurableEventKind::NodeStatusChanged { node_id, status } => {
                if let Some(index) = assistants.get(node_id).copied()
                    && let Some(crate::TranscriptEntry::Assistant {
                        status: stored_status,
                        ..
                    }) = projection.entries.get_mut(index)
                {
                    stored_status.clone_from(status);
                }
                if let Some(crate::TranscriptEntry::Plan {
                    source,
                    document,
                    status: stored_status,
                    ..
                }) =
                    projection.entries.iter_mut().find(|entry| matches!(entry, crate::TranscriptEntry::Plan { node_id: id, .. } if id == node_id))
                {
                    stored_status.clone_from(status);
                    if status == "completed" {
                        *source = source.trim().to_owned();
                        *document = crate::presentation::parse_markdown(source);
                    }
                }
            }
            _ => {}
        }
    }

    // An unfinished response node is the mutable response. Completed nodes
    // remain durable transcript entries even while a later turn is working.
    if let Some(index) = projection.entries.iter().rposition(|entry| {
        matches!(entry, crate::TranscriptEntry::Assistant { status, .. } if status == "streaming")
    }) {
        let crate::TranscriptEntry::Assistant {
            source, document, ..
        } = projection.entries.remove(index)
        else {
            unreachable!("the entry was matched as an assistant message");
        };
        projection.live_assistant = Some(crate::LiveMarkdown { source, document });
    }
    if let Some(index) = projection.entries.iter().rposition(|entry| {
        matches!(entry, crate::TranscriptEntry::Plan { status, .. } if status == "streaming")
    }) {
        let crate::TranscriptEntry::Plan { source, document, .. } = projection.entries.remove(index) else { unreachable!("the entry was matched as a plan") };
        projection.live_plan = Some(crate::LiveMarkdown { source, document });
    }
    projection
}

/// Reduces durable conversation events into the complete frontend-neutral
/// transcript. This is intentionally the only place that decides transcript
/// grouping and ordering; consumers receive stable semantic blocks, not raw
/// event fragments.
#[cfg(test)]
fn transcript_blocks(
    events: &[crate::DurableEvent],
    workspace: &std::path::Path,
    turn: &crate::TurnState,
    terminals: &[crate::TerminalSnapshot],
    agent_runs: &[crate::AgentRun],
    held_permission: Option<&crate::PermissionResource>,
) -> Vec<crate::TranscriptBlock> {
    transcript_blocks_with_web_search_provider(
        events,
        workspace,
        turn,
        terminals,
        agent_runs,
        held_permission,
        None,
    )
}

fn transcript_blocks_with_web_search_provider(
    events: &[crate::DurableEvent],
    workspace: &std::path::Path,
    turn: &crate::TurnState,
    terminals: &[crate::TerminalSnapshot],
    agent_runs: &[crate::AgentRun],
    held_permission: Option<&crate::PermissionResource>,
    web_search_provider: Option<crate::WebSearchProvider>,
) -> Vec<crate::TranscriptBlock> {
    let projection = {
        let _span = tracing::trace_span!(
            "agent.transcript.semantic_projection",
            events = events.len()
        )
        .entered();
        transcript_projection(events, workspace)
    };
    let _block_span = tracing::trace_span!(
        "agent.transcript.block_projection",
        entries = projection.entries.len()
    )
    .entered();
    let mut attachments = events
        .iter()
        .filter_map(|event| match &event.kind {
            crate::DurableEventKind::NodeAppended {
                node_id,
                node_kind: crate::NodeKind::UserMessage,
                content,
                ..
            } => Some((
                *node_id,
                (
                    content
                        .get("attachments")
                        .and_then(|value| {
                            serde_json::from_value::<Vec<crate::CapturedAttachment>>(value.clone())
                                .ok()
                        })
                        .unwrap_or_default(),
                    content
                        .get("attachment_specs")
                        .or_else(|| content.get("attachments"))
                        .and_then(|value| {
                            serde_json::from_value::<Vec<crate::AttachmentSpec>>(value.clone()).ok()
                        })
                        .unwrap_or_default(),
                ),
            )),
            _ => None,
        })
        .collect::<HashMap<_, _>>();
    let mut blocks = Vec::new();
    let mut tool_batch = Vec::<crate::ToolActivity>::new();

    let flush_tools = |blocks: &mut Vec<crate::TranscriptBlock>,
                       tools: &mut Vec<crate::ToolActivity>| {
        if tools.is_empty() {
            return;
        }
        let id = crate::TranscriptBlockId::derived("tools", tools[0].node_id);
        let mut groups = crate::presentation::project_tool_activities_with_context(
            tools.iter().map(crate::ToolActivity::as_ref),
            workspace,
            agent_runs,
            terminals,
        );
        if let Some(provider) = web_search_provider {
            for group in &mut groups {
                if let crate::ToolActivityGroup::WebSearch {
                    provider: displayed_provider,
                    ..
                } = group
                    && displayed_provider == "configured"
                {
                    *displayed_provider = provider.label().into();
                }
            }
        }
        let status = if tools
            .iter()
            .any(|tool| tool.status == crate::ToolActivityStatus::Pending)
        {
            crate::TranscriptBlockStatus::Pending
        } else {
            crate::TranscriptBlockStatus::Completed
        };
        let joins_previous_exploration = groups
            .iter()
            .all(|group| matches!(group, crate::ToolActivityGroup::Exploration { .. }))
            && blocks.last().is_some_and(|block| {
                matches!(
                    &block.kind,
                    crate::TranscriptBlockKind::ToolGroups { groups }
                        if groups.iter().all(|group| matches!(group, crate::ToolActivityGroup::Exploration { .. }))
                )
            });
        if joins_previous_exploration {
            let previous = blocks
                .last_mut()
                .expect("the preceding exploration block was just checked");
            let crate::TranscriptBlockKind::ToolGroups {
                groups: previous_groups,
            } = &mut previous.kind
            else {
                unreachable!("the preceding block was just checked as tool groups");
            };
            crate::presentation::append_tool_activity_groups(previous_groups, groups);
            if status == crate::TranscriptBlockStatus::Pending {
                previous.status = status;
            }
            tools.clear();
            return;
        }
        blocks.push(crate::TranscriptBlock {
            id,
            status,
            kind: crate::TranscriptBlockKind::ToolGroups { groups },
        });
        tools.clear();
    };

    for entry in projection.entries {
        match entry {
            crate::TranscriptEntry::Tool {
                node_id,
                name,
                arguments,
                result,
                failed,
            } => {
                if name == crate::runtime::request::UPDATE_PLAN_TOOL {
                    continue;
                }
                if result.is_none()
                    && held_permission.is_some_and(|permission| permission.tool == name)
                {
                    continue;
                }
                // A durable replay may contain several adjacent tool calls. Keep
                // only read-only exploration together; every other tool starts
                // its own transcript card, matching the live activity tracker.
                // Otherwise an exploration card and the following mutation are
                // incorrectly rendered as one collapsed transcript block.
                let starts_new_batch = !tool_batch.is_empty()
                    && (crate::presentation::exploration_activity_kind_for(&name, &arguments)
                        .is_none()
                        || tool_batch.iter().any(|tool| {
                            crate::presentation::exploration_activity_kind_for(
                                &tool.name,
                                &tool.arguments,
                            )
                            .is_none()
                        }));
                if starts_new_batch {
                    flush_tools(&mut blocks, &mut tool_batch);
                }
                tool_batch.push(crate::ToolActivity {
                    node_id,
                    name,
                    arguments,
                    status: if result.is_some() {
                        if failed {
                            crate::ToolActivityStatus::Failed
                        } else {
                            crate::ToolActivityStatus::Succeeded
                        }
                    } else {
                        crate::ToolActivityStatus::Pending
                    },
                    result,
                });
            }
            crate::TranscriptEntry::User {
                node_id,
                label,
                text,
                images,
                image_chips,
            } => {
                flush_tools(&mut blocks, &mut tool_batch);
                let (captured, specs) = attachments.remove(&node_id).unwrap_or_default();
                let mut projected =
                    crate::presentation::project_submitted_attachments(&text, &captured);
                for attachment in
                    crate::presentation::project_submitted_attachment_specs(&text, &specs)
                {
                    if projected
                        .iter()
                        .all(|existing| existing.start != attachment.start)
                    {
                        projected.push(attachment);
                    }
                }
                projected.sort_by_key(|attachment| attachment.start);
                blocks.push(crate::TranscriptBlock {
                    id: crate::TranscriptBlockId::node(node_id),
                    status: crate::TranscriptBlockStatus::Completed,
                    kind: crate::TranscriptBlockKind::User {
                        label,
                        text,
                        attachments: projected,
                        images,
                        image_chips,
                    },
                });
            }
            crate::TranscriptEntry::Assistant {
                node_id,
                source,
                document,
                status,
            } => {
                // Provider continuations often create an empty assistant
                // placeholder between adjacent read-only tool batches. It has
                // no transcript meaning and must not split exploration.
                if source.is_empty() && document.is_empty() && status != "streaming" {
                    continue;
                }
                flush_tools(&mut blocks, &mut tool_batch);
                blocks.push(crate::TranscriptBlock {
                    id: crate::TranscriptBlockId::node(node_id),
                    status: block_status(&status),
                    kind: crate::TranscriptBlockKind::Assistant {
                        source,
                        document,
                        message: None,
                    },
                });
            }
            crate::TranscriptEntry::Plan {
                node_id,
                source,
                document,
                status,
            } => {
                flush_tools(&mut blocks, &mut tool_batch);
                blocks.push(crate::TranscriptBlock {
                    id: crate::TranscriptBlockId::node(node_id),
                    status: block_status(&status),
                    kind: crate::TranscriptBlockKind::Plan { source, document },
                });
            }
            crate::TranscriptEntry::AcceptedPlan {
                node_id,
                source,
                document,
                clear_context,
                compact_context,
                compaction_summary,
            } => {
                flush_tools(&mut blocks, &mut tool_batch);
                blocks.push(crate::TranscriptBlock {
                    id: crate::TranscriptBlockId::node(node_id),
                    status: crate::TranscriptBlockStatus::Completed,
                    kind: crate::TranscriptBlockKind::AcceptedPlan {
                        source,
                        document,
                        clear_context,
                        compact_context,
                        compaction_summary,
                    },
                });
            }
            crate::TranscriptEntry::Compacted { node_id, summary } => {
                flush_tools(&mut blocks, &mut tool_batch);
                blocks.push(crate::TranscriptBlock {
                    id: crate::TranscriptBlockId::node(node_id),
                    status: crate::TranscriptBlockStatus::Completed,
                    kind: crate::TranscriptBlockKind::Compacted { summary },
                });
            }
            crate::TranscriptEntry::Notice { node_id, message } => {
                flush_tools(&mut blocks, &mut tool_batch);
                blocks.push(crate::TranscriptBlock {
                    id: crate::TranscriptBlockId::node(node_id),
                    status: crate::TranscriptBlockStatus::Completed,
                    kind: crate::TranscriptBlockKind::Notice { message },
                });
            }
            crate::TranscriptEntry::Recap { node_id, text } => {
                flush_tools(&mut blocks, &mut tool_batch);
                blocks.push(crate::TranscriptBlock {
                    id: crate::TranscriptBlockId::node(node_id),
                    status: crate::TranscriptBlockStatus::Completed,
                    kind: crate::TranscriptBlockKind::Recap { text },
                });
            }
            crate::TranscriptEntry::WorkspaceTransition {
                node_id,
                label,
                target,
                base,
                path,
            } => {
                flush_tools(&mut blocks, &mut tool_batch);
                blocks.push(crate::TranscriptBlock {
                    id: crate::TranscriptBlockId::node(node_id),
                    status: crate::TranscriptBlockStatus::Completed,
                    kind: crate::TranscriptBlockKind::WorkspaceTransition {
                        label,
                        target,
                        base,
                        path,
                    },
                });
            }
            crate::TranscriptEntry::SystemLog { cursor, message } => {
                flush_tools(&mut blocks, &mut tool_batch);
                blocks.push(crate::TranscriptBlock {
                    id: crate::TranscriptBlockId::event(cursor),
                    status: crate::TranscriptBlockStatus::Completed,
                    kind: crate::TranscriptBlockKind::Notice { message },
                });
            }
            crate::TranscriptEntry::PermissionDenied {
                node_id,
                resource,
                reason,
            } => {
                flush_tools(&mut blocks, &mut tool_batch);
                blocks.push(crate::TranscriptBlock {
                    id: crate::TranscriptBlockId::node(node_id),
                    status: crate::TranscriptBlockStatus::Completed,
                    kind: crate::TranscriptBlockKind::PermissionDenied { resource, reason },
                });
            }
            crate::TranscriptEntry::Interrupt {
                node_id,
                queued_steering,
            } => {
                flush_tools(&mut blocks, &mut tool_batch);
                blocks.push(crate::TranscriptBlock {
                    id: crate::TranscriptBlockId::node(node_id),
                    status: crate::TranscriptBlockStatus::Completed,
                    kind: crate::TranscriptBlockKind::Interrupt { queued_steering },
                });
            }
        }
    }
    flush_tools(&mut blocks, &mut tool_batch);

    // A provider continuation starts with an empty streaming assistant node.
    // It has no visible transcript meaning yet and must not temporarily close
    // the trailing Explore block between successive tool rounds.
    if let Some(live) = projection
        .live_assistant
        .filter(|live| !live.source.is_empty() || !live.document.is_empty())
    {
        blocks.push(crate::TranscriptBlock {
            id: crate::TranscriptBlockId("live-assistant".into()),
            status: crate::TranscriptBlockStatus::Streaming,
            kind: crate::TranscriptBlockKind::Assistant {
                source: live.source,
                document: live.document,
                message: None,
            },
        });
    }
    if let Some(live) = projection.live_plan {
        blocks.push(crate::TranscriptBlock {
            id: crate::TranscriptBlockId("live-plan".into()),
            status: crate::TranscriptBlockStatus::Streaming,
            kind: crate::TranscriptBlockKind::Plan {
                source: live.source,
                document: live.document,
            },
        });
    }
    if !matches!(turn, crate::TurnState::Idle)
        && let Some(crate::TranscriptBlock {
            kind: crate::TranscriptBlockKind::ToolGroups { groups },
            ..
        }) = blocks.last_mut()
        && groups
            .iter()
            .all(|group| matches!(group, crate::ToolActivityGroup::Exploration { .. }))
    {
        for group in groups {
            let crate::ToolActivityGroup::Exploration { active, .. } = group else {
                unreachable!("all trailing groups were checked as exploration");
            };
            *active = true;
        }
    }
    if !matches!(turn, crate::TurnState::Idle) {
        blocks.push(crate::TranscriptBlock {
            id: crate::TranscriptBlockId("work".into()),
            status: crate::TranscriptBlockStatus::Pending,
            kind: crate::TranscriptBlockKind::Work {
                state: turn.clone(),
            },
        });
    }
    for terminal in terminals {
        if terminal.read_safe.is_none()
            && terminal.owner_agent_run_id.is_none()
            && terminal.tool_call_node_id.is_some()
        {
            blocks.push(crate::presentation::detached_bash_transcript_block(
                terminal,
            ));
            continue;
        }
        let result = crate::presentation::terminal_activity_result(terminal);
        let status = crate::presentation::terminal_activity_status(terminal);
        for block in &mut blocks {
            let crate::TranscriptBlockKind::ToolGroups { groups } = &mut block.kind else {
                continue;
            };
            for group in groups {
                if let Some(node_id) = terminal.tool_call_node_id {
                    crate::presentation::update_projected_bash_activity(
                        group, node_id, &result, status,
                    );
                }
            }
        }
    }
    blocks
}

/// Builds the background-work projection from the same semantic transcript
/// blocks sent to frontends. Every Bash invocation is represented by its
/// durable supervised terminal.
fn supervised_work_from_transcript(
    transcript: &[crate::TranscriptBlock],
    agents: Vec<crate::AgentRun>,
    terminals: Vec<crate::TerminalSnapshot>,
) -> Vec<crate::presentation::SupervisedWork> {
    let _ = transcript;
    crate::presentation::project_supervised_work(crate::presentation::SupervisedWorkSources {
        agents,
        terminals,
    })
}

/// Inserts the temporary agent-owned interruption marker before trailing work
/// state. The durable marker replaces this block after cancellation settles.
fn insert_pending_interrupt(transcript: &mut Vec<crate::TranscriptBlock>, queued_steering: bool) {
    let insertion = transcript
        .iter()
        .position(|block| matches!(block.kind, crate::TranscriptBlockKind::Work { .. }))
        .unwrap_or(transcript.len());
    transcript.insert(
        insertion,
        crate::TranscriptBlock {
            id: crate::TranscriptBlockId("pending-interrupt".into()),
            status: crate::TranscriptBlockStatus::Pending,
            kind: crate::TranscriptBlockKind::Interrupt { queued_steering },
        },
    );
}

/// A previous interruption remains in visible history. Only a trailing one
/// belongs to the cancellation currently settling.
fn trailing_interrupt_is_durable(transcript: &[crate::TranscriptBlock]) -> bool {
    transcript
        .iter()
        .rev()
        .find(|block| !matches!(block.kind, crate::TranscriptBlockKind::Work { .. }))
        .is_some_and(|block| matches!(block.kind, crate::TranscriptBlockKind::Interrupt { .. }))
}

/// Projects the durable history and runtime-owned turn lifecycle into one
/// frontend-neutral turn state. Durable nodes may finish while tools or other
/// runtime work continues, so the live lifecycle is authoritative while set.
fn projected_turn_state(
    durable_turn: crate::TurnState,
    live_turn: Option<LiveTurn>,
    waiting_for_interaction: bool,
    cancellation_requested: bool,
) -> crate::TurnState {
    let turn = live_turn
        .and_then(|live| {
            live.turn_id.map(|turn_id| {
                if live.compacting {
                    crate::TurnState::Compacting {
                        started_at: live.started_at,
                        turn_id,
                    }
                } else if waiting_for_interaction || live.waiting_on_work > 0 {
                    crate::TurnState::Waiting {
                        started_at: live.started_at,
                        turn_id,
                    }
                } else if live.reasoning {
                    crate::TurnState::Thinking {
                        started_at: live.started_at,
                        turn_id,
                    }
                } else {
                    crate::TurnState::Working {
                        started_at: live.started_at,
                        turn_id,
                    }
                }
            })
        })
        .unwrap_or(durable_turn);
    if cancellation_requested {
        match turn {
            crate::TurnState::Working {
                started_at,
                turn_id,
            }
            | crate::TurnState::Thinking {
                started_at,
                turn_id,
            }
            | crate::TurnState::Waiting {
                started_at,
                turn_id,
            }
            | crate::TurnState::Cancelling {
                started_at,
                turn_id,
            }
            | crate::TurnState::Compacting {
                started_at,
                turn_id,
            } => crate::TurnState::Cancelling {
                started_at,
                turn_id,
            },
            crate::TurnState::Idle => crate::TurnState::Idle,
        }
    } else {
        turn
    }
}

fn block_status(status: &str) -> crate::TranscriptBlockStatus {
    match status {
        "pending" => crate::TranscriptBlockStatus::Pending,
        "streaming" => crate::TranscriptBlockStatus::Streaming,
        "failed" => crate::TranscriptBlockStatus::Failed,
        "cancelled" => crate::TranscriptBlockStatus::Cancelled,
        _ => crate::TranscriptBlockStatus::Completed,
    }
}

#[cfg(test)]
mod transcript_projection_tests {
    use super::*;

    fn event(kind: crate::DurableEventKind) -> crate::DurableEvent {
        crate::DurableEvent {
            version: crate::API_VERSION,
            cursor: crate::EventCursor(1),
            conversation_id: crate::ConversationId::new(),
            kind,
        }
    }

    #[test]
    fn finalized_blocks_match_between_live_tail_and_historical_projection() {
        let user = crate::NodeId::new();
        let assistant = crate::NodeId::new();
        let events = vec![
            event(crate::DurableEventKind::NodeAppended {
                node_id: user,
                parent_id: None,
                turn_id: None,
                owner_id: None,
                request_index: None,
                node_kind: crate::NodeKind::UserMessage,
                status: "completed".into(),
                content: serde_json::json!({"text":"hello"}),
            }),
            event(crate::DurableEventKind::NodeAppended {
                node_id: assistant,
                parent_id: Some(user),
                turn_id: None,
                owner_id: None,
                request_index: None,
                node_kind: crate::NodeKind::AssistantMessage,
                status: "completed".into(),
                content: serde_json::json!({"text":"done"}),
            }),
        ];
        let historical = transcript_blocks(
            &events,
            std::path::Path::new("/workspace"),
            &crate::TurnState::Idle,
            &[],
            &[],
            None,
        );
        let live = transcript_blocks(
            &events,
            std::path::Path::new("/workspace"),
            &crate::TurnState::Working {
                started_at: "1".into(),
                turn_id: crate::TurnId::new(),
            },
            &[],
            &[],
            None,
        );
        let finalized_live = live
            .into_iter()
            .filter(|block| !matches!(block.kind, crate::TranscriptBlockKind::Work { .. }))
            .collect::<Vec<_>>();
        assert_eq!(finalized_live, historical);
    }

    #[test]
    fn pending_web_search_uses_the_selected_provider() {
        let tool = crate::NodeId::new();
        let events = vec![event(crate::DurableEventKind::NodeAppended {
            node_id: tool,
            parent_id: None,
            turn_id: None,
            owner_id: None,
            request_index: None,
            node_kind: crate::NodeKind::ToolCall,
            status: "completed".into(),
            content: serde_json::json!({
                "name": "web_search",
                "arguments": {"query": "rust tui"},
            }),
        })];

        let blocks = transcript_blocks_with_web_search_provider(
            &events,
            std::path::Path::new("/workspace"),
            &crate::TurnState::Idle,
            &[],
            &[],
            None,
            Some(crate::WebSearchProvider::Exa),
        );

        assert!(matches!(
            &blocks[0].kind,
            crate::TranscriptBlockKind::ToolGroups { groups }
                if matches!(groups.as_slice(), [crate::ToolActivityGroup::WebSearch { provider, .. }]
                    if provider == "exa")
        ));
    }

    #[test]
    fn denied_permission_projection_preserves_resource_and_reason() {
        let node_id = crate::NodeId::new();
        let events = vec![event(crate::DurableEventKind::NodeAppended {
            node_id,
            parent_id: None,
            turn_id: None,
            owner_id: None,
            request_index: None,
            node_kind: crate::NodeKind::PermissionDecision,
            status: "completed".into(),
            content: serde_json::json!({
                "resource": {
                    "tool": "apply_patch",
                    "path": "/workspace/src/main.rs",
                    "access": "write",
                    "mode": "ask",
                    "agent": "general",
                    "command": [],
                    "raw_command": null,
                    "cwd": null
                },
                "decision": {
                    "effect": "deny",
                    "operation": {
                        "effect": "deny",
                        "layer": "default",
                        "rule_id": null,
                        "reason": "workspace is read-only"
                    },
                    "external": null
                },
                "outcome": "deny"
            }),
        })];

        let projection = transcript_projection(&events, std::path::Path::new("/workspace"));

        assert!(matches!(
            projection.entries.as_slice(),
            [crate::TranscriptEntry::PermissionDenied {
                node_id: projected,
                resource,
                reason,
            }] if *projected == node_id
                && resource == "edit src/main.rs"
                && reason == "workspace is read-only"
        ));
    }

    #[test]
    fn successful_worktree_transition_replaces_generic_tool_activity() {
        let call_id = crate::NodeId::new();
        let result_id = crate::NodeId::new();
        let notice_id = crate::NodeId::new();
        let cwd = "/workspace/.cagent/worktrees/red-button";
        let events = vec![
            event(crate::DurableEventKind::NodeAppended {
                node_id: call_id,
                parent_id: None,
                turn_id: None,
                owner_id: None,
                request_index: Some(0),
                node_kind: crate::NodeKind::ToolCall,
                status: "completed".into(),
                content: serde_json::json!({
                    "name": "enter_worktree",
                    "call_id": "call",
                    "arguments": { "name": "red-button", "path": null, "base": "head" }
                }),
            }),
            event(crate::DurableEventKind::NodeAppended {
                node_id: result_id,
                parent_id: Some(call_id),
                turn_id: None,
                owner_id: Some(call_id),
                request_index: None,
                node_kind: crate::NodeKind::ToolResult,
                status: "completed".into(),
                content: serde_json::json!({
                    "output": { "cwd": cwd },
                    "is_error": false
                }),
            }),
            event(crate::DurableEventKind::NodeAppended {
                node_id: notice_id,
                parent_id: Some(result_id),
                turn_id: None,
                owner_id: None,
                request_index: None,
                node_kind: crate::NodeKind::System,
                status: "completed".into(),
                content: serde_json::json!({
                    "transcript": "notice",
                    "workspace_transition": {
                        "project_dir": "/workspace",
                        "previous_cwd": "/workspace",
                        "cwd": cwd,
                        "reason": "worktree_created",
                        "vcs": "git",
                        "name": "red-button",
                        "branch_or_workspace": "worktree-red-button",
                        "base": "release/123",
                        "created": true,
                        "warning": null,
                        "worktree": {
                            "vcs": "git",
                            "name": "red-button",
                            "branch_or_workspace": "worktree-red-button",
                            "path": cwd
                        }
                    }
                }),
            }),
        ];

        let projection = transcript_projection(&events, std::path::Path::new("/workspace"));

        assert!(matches!(
            projection.entries.as_slice(),
            [crate::TranscriptEntry::WorkspaceTransition {
                node_id: projected,
                label,
                target,
                base,
                ..
            }] if *projected == notice_id
                && label == "Enter Worktree"
                && target == ".cagent/worktrees/red-button"
                && base.as_deref() == Some("release/123")
        ));
    }

    #[test]
    fn successful_directory_transition_is_one_relative_structured_item() {
        let call_id = crate::NodeId::new();
        let result_id = crate::NodeId::new();
        let notice_id = crate::NodeId::new();
        let cwd = "/workspace/test2";
        let events = vec![
            event(crate::DurableEventKind::NodeAppended {
                node_id: call_id,
                parent_id: None,
                turn_id: None,
                owner_id: None,
                request_index: Some(0),
                node_kind: crate::NodeKind::ToolCall,
                status: "completed".into(),
                content: serde_json::json!({
                    "name": "change_working_directory",
                    "call_id": "call",
                    "arguments": { "path": "../test2" }
                }),
            }),
            event(crate::DurableEventKind::NodeAppended {
                node_id: result_id,
                parent_id: Some(call_id),
                turn_id: None,
                owner_id: Some(call_id),
                request_index: None,
                node_kind: crate::NodeKind::ToolResult,
                status: "completed".into(),
                content: serde_json::json!({
                    "output": { "cwd": cwd },
                    "is_error": false
                }),
            }),
            event(crate::DurableEventKind::NodeAppended {
                node_id: notice_id,
                parent_id: Some(result_id),
                turn_id: None,
                owner_id: None,
                request_index: None,
                node_kind: crate::NodeKind::System,
                status: "completed".into(),
                content: serde_json::json!({
                    "transcript": "notice",
                    "workspace_transition": {
                        "project_dir": "/workspace",
                        "previous_cwd": "/workspace/test",
                        "cwd": cwd,
                        "reason": "working_directory_changed",
                        "vcs": "git",
                        "name": null,
                        "branch_or_workspace": null,
                        "base": null,
                        "created": false,
                        "warning": null,
                        "worktree": null
                    }
                }),
            }),
        ];

        let projection = transcript_projection(&events, std::path::Path::new("/workspace"));

        assert!(matches!(
            projection.entries.as_slice(),
            [crate::TranscriptEntry::WorkspaceTransition {
                node_id: projected,
                label,
                target,
                base: None,
                ..
            }] if *projected == notice_id
                && label == "Change Working Directory"
                && target == "../test2"
        ));
    }

    #[test]
    fn manual_rename_is_projected_as_a_system_log_but_automatic_titles_are_not() {
        let mut second_manual_rename = event(crate::DurableEventKind::ConversationTitleChanged {
            title: "Second manual title".into(),
            source: crate::ConversationTitleSource::Manual,
            node_id: None,
        });
        second_manual_rename.cursor = crate::EventCursor(2);
        let mut generated_title = event(crate::DurableEventKind::ConversationTitleChanged {
            title: "Generated title".into(),
            source: crate::ConversationTitleSource::Generated,
            node_id: None,
        });
        generated_title.cursor = crate::EventCursor(3);
        let mut fallback_title = event(crate::DurableEventKind::ConversationTitleChanged {
            title: "Fallback title".into(),
            source: crate::ConversationTitleSource::Fallback,
            node_id: None,
        });
        fallback_title.cursor = crate::EventCursor(4);
        let events = vec![
            event(crate::DurableEventKind::ConversationTitleChanged {
                title: "Manual title".into(),
                source: crate::ConversationTitleSource::Manual,
                node_id: None,
            }),
            second_manual_rename,
            generated_title,
            fallback_title,
        ];

        let projection = transcript_projection(&events, std::path::Path::new("/workspace"));
        assert!(matches!(
            projection.entries.as_slice(),
            [
                crate::TranscriptEntry::SystemLog {
                    cursor: first_cursor,
                    message: first_message,
                },
                crate::TranscriptEntry::SystemLog {
                    cursor: second_cursor,
                    message: second_message,
                },
            ] if *first_cursor == crate::EventCursor(1)
                && first_message == "Session renamed to Manual title"
                && *second_cursor == crate::EventCursor(2)
                && second_message == "Session renamed to Second manual title"
        ));

        let blocks = transcript_blocks(
            &events,
            std::path::Path::new("/workspace"),
            &crate::TurnState::Idle,
            &[],
            &[],
            None,
        );
        assert!(matches!(
            blocks.as_slice(),
            [
                crate::TranscriptBlock {
                    id: first_id,
                    kind: crate::TranscriptBlockKind::Notice {
                        message: first_message,
                    },
                    ..
                },
                crate::TranscriptBlock {
                    id: second_id,
                    kind: crate::TranscriptBlockKind::Notice {
                        message: second_message,
                    },
                    ..
                },
            ] if *first_id == crate::TranscriptBlockId::event(crate::EventCursor(1))
                && first_message == "Session renamed to Manual title"
                && *second_id == crate::TranscriptBlockId::event(crate::EventCursor(2))
                && second_message == "Session renamed to Second manual title"
        ));
    }

    #[test]
    fn hidden_local_context_exposes_only_reload_warnings() {
        let node_id = crate::NodeId::new();
        let events = vec![event(crate::DurableEventKind::NodeAppended {
            node_id,
            parent_id: None,
            turn_id: None,
            owner_id: None,
            request_index: None,
            node_kind: crate::NodeKind::System,
            status: "completed".into(),
            content: serde_json::json!({
                "transcript": "hidden",
                "message": "secret instructions and diffs",
                "local_context": {
                    "revision": "revision",
                    "instructions": [],
                    "skills": [],
                    "warnings": [{
                        "path": "/workspace/skills/demo/SKILL.md",
                        "message": "description is required"
                    }]
                }
            }),
        })];

        let projection = transcript_projection(&events, std::path::Path::new("/workspace"));

        assert!(matches!(
            projection.entries.as_slice(),
            [crate::TranscriptEntry::Notice { node_id: projected, message }]
                if *projected == node_id
                    && message.contains("SKILL.md: description is required")
                    && !message.contains("secret instructions")
        ));
    }

    #[test]
    fn recap_system_nodes_project_as_dedicated_blocks() {
        let node_id = crate::NodeId::new();
        let events = vec![event(crate::DurableEventKind::NodeAppended {
            node_id,
            parent_id: None,
            turn_id: None,
            owner_id: None,
            request_index: None,
            node_kind: crate::NodeKind::System,
            status: "completed".into(),
            content: serde_json::json!({
                "system_type": "recap",
                "transcript": "recap",
                "text": "The implementation is complete. Run tests next."
            }),
        })];
        let blocks = transcript_blocks(
            &events,
            std::path::Path::new("/workspace"),
            &crate::TurnState::Idle,
            &[],
            &[],
            None,
        );
        assert!(matches!(
            blocks.as_slice(),
            [crate::TranscriptBlock {
                id,
                kind: crate::TranscriptBlockKind::Recap { text },
                ..
            }] if *id == crate::TranscriptBlockId::node(node_id)
                && text == "The implementation is complete. Run tests next."
        ));
    }

    #[test]
    fn recap_system_nodes_survive_active_branch_filtering() {
        let parent_id = crate::NodeId::new();
        let recap_id = crate::NodeId::new();
        let branch = [parent_id, recap_id].into_iter().collect();
        let recap = crate::DurableEventKind::NodeAppended {
            node_id: recap_id,
            parent_id: Some(parent_id),
            turn_id: None,
            owner_id: None,
            request_index: None,
            node_kind: crate::NodeKind::System,
            status: "completed".into(),
            content: serde_json::json!({
                "system_type": "recap",
                "transcript": "recap",
                "text": "The implementation is complete."
            }),
        };

        assert!(super::transcript_event_visible_on_branch(&branch, &recap));
        assert!(!super::transcript_event_visible_on_branch(
            &[parent_id].into_iter().collect(),
            &recap
        ));
    }

    #[test]
    fn only_the_active_trailing_recap_remains_visible() {
        let old_id = crate::TranscriptBlockId::node(crate::NodeId::new());
        let active_id = crate::TranscriptBlockId::node(crate::NodeId::new());
        let recap = |id| crate::TranscriptBlock {
            id,
            status: crate::TranscriptBlockStatus::Completed,
            kind: crate::TranscriptBlockKind::Recap {
                text: "summary".into(),
            },
        };
        let mut blocks = vec![recap(old_id), recap(active_id.clone())];

        super::retain_only_trailing_recap(&mut blocks, Some(&active_id));

        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].id, active_id);
        super::retain_only_trailing_recap(&mut blocks, None);
        assert!(blocks.is_empty());
    }

    #[test]
    fn live_turn_keeps_work_visible_after_durable_work_has_settled() {
        let turn_id = crate::TurnId::new();
        let live_turn = || LiveTurn {
            started_at: "1234".into(),
            last_activity_at: "1234".into(),
            last_activity_published_at: std::time::Instant::now(),
            turn_id: Some(turn_id),
            reasoning: false,
            waiting_on_work: 0,
            compacting: false,
            active_plan: None,
            steering: None,
        };

        let working = projected_turn_state(crate::TurnState::Idle, Some(live_turn()), false, false);
        assert!(matches!(
            working,
            crate::TurnState::Working { started_at, turn_id: id }
                if started_at == "1234" && id == turn_id
        ));

        let waiting = projected_turn_state(crate::TurnState::Idle, Some(live_turn()), true, false);
        assert!(matches!(
            waiting,
            crate::TurnState::Waiting { started_at, turn_id: id }
                if started_at == "1234" && id == turn_id
        ));

        let waiting_for_work = projected_turn_state(
            crate::TurnState::Idle,
            Some(LiveTurn {
                waiting_on_work: 1,
                ..live_turn()
            }),
            false,
            false,
        );
        assert!(matches!(
            waiting_for_work,
            crate::TurnState::Waiting { started_at, turn_id: id }
                if started_at == "1234" && id == turn_id
        ));

        let thinking = projected_turn_state(
            crate::TurnState::Idle,
            Some(LiveTurn {
                reasoning: true,
                ..live_turn()
            }),
            false,
            false,
        );
        assert!(matches!(
            thinking,
            crate::TurnState::Thinking { started_at, turn_id: id }
                if started_at == "1234" && id == turn_id
        ));

        let cancelling =
            projected_turn_state(crate::TurnState::Idle, Some(live_turn()), false, true);
        assert!(matches!(
            cancelling,
            crate::TurnState::Cancelling { started_at, turn_id: id }
                if started_at == "1234" && id == turn_id
        ));
    }

    #[test]
    fn keeps_completed_assistant_messages_in_history_while_exposing_only_the_live_reply() {
        let completed = crate::NodeId::new();
        let live = crate::NodeId::new();
        let events = vec![
            event(crate::DurableEventKind::NodeAppended {
                node_id: completed,
                parent_id: None,
                turn_id: None,
                owner_id: None,
                request_index: None,
                node_kind: crate::NodeKind::AssistantMessage,
                status: "streaming".into(),
                content: serde_json::json!({ "text": "" }),
            }),
            event(crate::DurableEventKind::AssistantDelta {
                node_id: completed,
                delta: "Earlier reply".into(),
                document: crate::parse_markdown("Earlier reply"),
            }),
            event(crate::DurableEventKind::NodeStatusChanged {
                node_id: completed,
                status: "completed".into(),
            }),
            event(crate::DurableEventKind::NodeAppended {
                node_id: live,
                parent_id: Some(completed),
                turn_id: None,
                owner_id: None,
                request_index: None,
                node_kind: crate::NodeKind::AssistantMessage,
                status: "streaming".into(),
                content: serde_json::json!({ "text": "" }),
            }),
            event(crate::DurableEventKind::AssistantDelta {
                node_id: live,
                delta: "Current reply".into(),
                document: crate::parse_markdown("Current reply"),
            }),
        ];

        let projection = transcript_projection(&events, std::path::Path::new("/workspace"));

        assert!(matches!(
            projection.entries.as_slice(),
            [crate::TranscriptEntry::Assistant { node_id, source, status, .. }]
                if *node_id == completed && source == "Earlier reply" && status == "completed"
        ));
        assert_eq!(
            projection
                .live_assistant
                .as_ref()
                .map(|live| live.source.as_str()),
            Some("Current reply")
        );
    }

    #[test]
    fn reset_accepted_plan_discards_pre_plan_transcript_entries_without_a_clear_node() {
        let before = crate::NodeId::new();
        let accepted = crate::NodeId::new();
        let events = vec![
            event(crate::DurableEventKind::NodeAppended {
                node_id: before,
                parent_id: None,
                turn_id: None,
                owner_id: None,
                request_index: None,
                node_kind: crate::NodeKind::UserMessage,
                status: "completed".into(),
                content: serde_json::json!({ "text": "Plan the work" }),
            }),
            event(crate::DurableEventKind::NodeAppended {
                node_id: accepted,
                parent_id: Some(before),
                turn_id: None,
                owner_id: None,
                request_index: None,
                node_kind: crate::NodeKind::AcceptedPlan,
                status: "completed".into(),
                content: serde_json::json!({
                    "plan_markdown": "# Implement the plan",
                    "reset_context": true,
                }),
            }),
        ];

        let projection = transcript_projection(&events, std::path::Path::new("/workspace"));

        assert!(matches!(
            projection.entries.as_slice(),
            [crate::TranscriptEntry::AcceptedPlan { node_id, source, clear_context, .. }]
                if *node_id == accepted
                    && source == "# Implement the plan"
                    && *clear_context
        ));
    }

    #[test]
    fn compacted_accepted_plan_carries_its_checkpoint_summary() {
        let accepted = crate::NodeId::new();
        let checkpoint = crate::NodeId::new();
        let events = vec![
            event(crate::DurableEventKind::NodeAppended {
                node_id: accepted,
                parent_id: None,
                turn_id: None,
                owner_id: None,
                request_index: None,
                node_kind: crate::NodeKind::AcceptedPlan,
                status: "completed".into(),
                content: serde_json::json!({
                    "plan_markdown": "# Implement the plan",
                    "compact_context": true,
                }),
            }),
            event(crate::DurableEventKind::NodeAppended {
                node_id: checkpoint,
                parent_id: Some(accepted),
                turn_id: None,
                owner_id: None,
                request_index: None,
                node_kind: crate::NodeKind::CompactionSummary,
                status: "completed".into(),
                content: serde_json::json!({
                    "summary": "Keep the migration constraints.",
                    "reapplied_node_id": accepted,
                }),
            }),
        ];

        let projection = transcript_projection(&events, std::path::Path::new("/workspace"));

        assert!(matches!(
            projection.entries.as_slice(),
            [crate::TranscriptEntry::AcceptedPlan {
                node_id,
                compaction_summary: Some(summary),
                ..
            }] if *node_id == accepted && summary == "Keep the migration constraints."
        ));
    }

    #[test]
    fn standalone_compaction_carries_its_checkpoint_summary() {
        let checkpoint = crate::NodeId::new();
        let events = vec![event(crate::DurableEventKind::NodeAppended {
            node_id: checkpoint,
            parent_id: None,
            turn_id: None,
            owner_id: None,
            request_index: None,
            node_kind: crate::NodeKind::CompactionSummary,
            status: "completed".into(),
            content: serde_json::json!({ "summary": "Keep the API stable." }),
        })];

        let projection = transcript_projection(&events, std::path::Path::new("/workspace"));

        assert!(matches!(
            projection.entries.as_slice(),
            [crate::TranscriptEntry::Compacted { node_id, summary }]
                if *node_id == checkpoint && summary == "Keep the API stable."
        ));
    }

    #[test]
    fn session_blocks_preserve_live_content_grouping_attachments_and_work_state() {
        let user = crate::NodeId::new();
        let read = crate::NodeId::new();
        let list = crate::NodeId::new();
        let assistant = crate::NodeId::new();
        let events = vec![
            event(crate::DurableEventKind::NodeAppended {
                node_id: user,
                parent_id: None,
                turn_id: None,
                owner_id: None,
                request_index: None,
                node_kind: crate::NodeKind::UserMessage,
                status: "completed".into(),
                content: serde_json::json!({
                    "text": "Review @src/lib.rs:1-2",
                    "attachment_specs": [{"path":"src/lib.rs","start_line":1,"end_line":2}],
                }),
            }),
            event(crate::DurableEventKind::NodeAppended {
                node_id: read,
                parent_id: Some(user),
                turn_id: None,
                owner_id: None,
                request_index: None,
                node_kind: crate::NodeKind::ToolCall,
                status: "streaming".into(),
                content: serde_json::json!({"name":"read","arguments":{"path":"src/lib.rs"}}),
            }),
            event(crate::DurableEventKind::NodeAppended {
                node_id: list,
                parent_id: Some(read),
                turn_id: None,
                owner_id: None,
                request_index: None,
                node_kind: crate::NodeKind::ToolCall,
                status: "streaming".into(),
                content: serde_json::json!({"name":"list","arguments":{"path":"src"}}),
            }),
            event(crate::DurableEventKind::NodeAppended {
                node_id: assistant,
                parent_id: Some(list),
                turn_id: None,
                owner_id: None,
                request_index: None,
                node_kind: crate::NodeKind::AssistantMessage,
                status: "streaming".into(),
                content: serde_json::json!({"text":""}),
            }),
            event(crate::DurableEventKind::AssistantDelta {
                node_id: assistant,
                delta: "Working answer".into(),
                document: crate::parse_markdown("Working answer"),
            }),
        ];
        let turn_id = crate::TurnId::new();
        let blocks = transcript_blocks(
            &events,
            std::path::Path::new("/workspace"),
            &crate::TurnState::Working {
                started_at: "2026-01-01T00:00:00Z".into(),
                turn_id,
            },
            &[],
            &[],
            None,
        );

        assert!(matches!(
            &blocks[0].kind,
            crate::TranscriptBlockKind::User { attachments, .. }
                if matches!(attachments.as_slice(), [attachment]
                    if attachment.start == "Review ".len()
                        && attachment.end == "Review @src/lib.rs:1-2".len()
                        && attachment.spec.path == std::path::Path::new("src/lib.rs")
                        && attachment.spec.start_line == Some(1)
                        && attachment.spec.end_line == Some(2)
                        && attachment.kind
                            == crate::presentation::SubmittedAttachmentKind::File)
        ));
        assert!(matches!(
            &blocks[1].kind,
            crate::TranscriptBlockKind::ToolGroups { groups }
                if matches!(groups.as_slice(), [crate::ToolActivityGroup::Exploration { .. }])
        ));
        assert!(matches!(
            blocks[2],
            crate::TranscriptBlock {
                status: crate::TranscriptBlockStatus::Streaming,
                kind: crate::TranscriptBlockKind::Assistant { .. },
                ..
            }
        ));
        assert!(matches!(
            blocks.last().map(|block| &block.kind),
            Some(crate::TranscriptBlockKind::Work { .. })
        ));
    }

    #[test]
    fn session_blocks_include_a_partial_streaming_plan() {
        let plan = crate::NodeId::new();
        let events = vec![
            event(crate::DurableEventKind::NodeAppended {
                node_id: plan,
                parent_id: None,
                turn_id: None,
                owner_id: None,
                request_index: None,
                node_kind: crate::NodeKind::AssistantMessage,
                status: "streaming".into(),
                content: serde_json::json!({ "text": "" }),
            }),
            event(crate::DurableEventKind::PlanStarted { node_id: plan }),
            event(crate::DurableEventKind::PlanDelta {
                node_id: plan,
                delta: "# Build\n\n- Persist first".into(),
                document: crate::parse_markdown("# Build\n\n- Persist first"),
            }),
        ];
        let blocks = transcript_blocks(
            &events,
            std::path::Path::new("/workspace"),
            &crate::TurnState::Working {
                started_at: "2026-01-01T00:00:00Z".into(),
                turn_id: crate::TurnId::new(),
            },
            &[],
            &[],
            None,
        );
        assert!(matches!(
            blocks.first(),
            Some(crate::TranscriptBlock {
                status: crate::TranscriptBlockStatus::Streaming,
                kind: crate::TranscriptBlockKind::Plan { source, .. },
                ..
            }) if source == "# Build\n\n- Persist first"
        ));
    }

    #[test]
    fn session_blocks_replay_a_single_durable_queued_steering_interrupt() {
        let notice = crate::NodeId::new();
        let blocks = transcript_blocks(
            &[event(crate::DurableEventKind::NodeAppended {
                node_id: notice,
                parent_id: None,
                turn_id: None,
                owner_id: None,
                request_index: None,
                node_kind: crate::NodeKind::System,
                status: "completed".into(),
                content: serde_json::json!({"transcript":"interrupt", "queued_steering":true}),
            })],
            std::path::Path::new("/workspace"),
            &crate::TurnState::Idle,
            &[],
            &[],
            None,
        );
        assert!(matches!(
            blocks.as_slice(),
            [crate::TranscriptBlock {
                kind: crate::TranscriptBlockKind::Interrupt {
                    queued_steering: true
                },
                ..
            }]
        ));
    }

    #[test]
    fn session_blocks_keep_background_delegation_active_until_its_run_is_terminal() {
        let tool = crate::NodeId::new();
        let run_id = crate::AgentRunId::new();
        let events = [
            event(crate::DurableEventKind::NodeAppended {
                node_id: tool,
                parent_id: None,
                turn_id: None,
                owner_id: None,
                request_index: None,
                node_kind: crate::NodeKind::ToolCall,
                status: "completed".into(),
                content: serde_json::json!({
                    "name": "delegate_agent",
                    "arguments": {"agent": "explore", "task": "Inspect", "wait": false},
                }),
            }),
            event(crate::DurableEventKind::NodeAppended {
                node_id: crate::NodeId::new(),
                parent_id: Some(tool),
                turn_id: None,
                owner_id: Some(tool),
                request_index: None,
                node_kind: crate::NodeKind::ToolResult,
                status: "completed".into(),
                content: serde_json::json!({"output": {"id": run_id}, "is_error": false}),
            }),
        ];
        let mut run = crate::AgentRun {
            id: run_id,
            conversation_id: crate::ConversationId::new(),
            parent_turn_id: crate::TurnId::new(),
            sequence: 0,
            profile: "explore".into(),
            model: crate::ModelRef::parse("mock/echo").unwrap(),
            effort: None,
            task: "Inspect".into(),
            status: crate::AgentRunStatus::Running,
            result: None,
            error: None,
            usage: None,
            created_at: "1".into(),
            started_at: Some("1".into()),
            completed_at: None,
            timeline: Vec::new(),
            activity: Vec::new(),
        };
        let groups = |run: &crate::AgentRun| {
            transcript_blocks(
                &events,
                std::path::Path::new("/workspace"),
                &crate::TurnState::Idle,
                &[],
                std::slice::from_ref(run),
                None,
            )
        };
        let blocks = groups(&run);
        assert!(matches!(
            &blocks[0].kind,
            crate::TranscriptBlockKind::ToolGroups { groups }
                if matches!(groups.as_slice(), [crate::ToolActivityGroup::Delegate {
                    action, status: crate::ToolActivityStatus::Pending, ..
                }] if action == "Running")
        ));

        run.status = crate::AgentRunStatus::Completed;
        let blocks = groups(&run);
        assert!(matches!(
            &blocks[0].kind,
            crate::TranscriptBlockKind::ToolGroups { groups }
                if matches!(groups.as_slice(), [crate::ToolActivityGroup::Delegate {
                    action, status: crate::ToolActivityStatus::Succeeded, ..
                }] if action == "Finished")
        ));
    }

    #[test]
    fn permission_held_tool_is_hidden_and_live_bash_output_is_retained() {
        let bash = crate::NodeId::new();
        let events = [event(crate::DurableEventKind::NodeAppended {
            node_id: bash,
            parent_id: None,
            turn_id: None,
            owner_id: None,
            request_index: None,
            node_kind: crate::NodeKind::ToolCall,
            status: "streaming".into(),
            content: serde_json::json!({
                "name": "bash",
                "arguments": {"command": "printf '\\e[31mred\\e[0m'"},
            }),
        })];
        let permission = crate::PermissionResource {
            tool: "bash".into(),
            server: None,
            operation: None,
            path: None,
            access: None,
            mode: "ask".into(),
            agent: "general".into(),
            command: Vec::new(),
            raw_command: None,
            cwd: None,
        };
        let hidden = transcript_blocks(
            &events,
            std::path::Path::new("/workspace"),
            &crate::TurnState::Idle,
            &[],
            &[],
            Some(&permission),
        );
        assert!(hidden.is_empty());

        let visible = transcript_blocks(
            &events,
            std::path::Path::new("/workspace"),
            &crate::TurnState::Idle,
            &[],
            &[],
            None,
        );
        assert!(matches!(
            visible.as_slice(),
            [crate::TranscriptBlock {
                kind: crate::TranscriptBlockKind::ToolGroups { groups },
                ..
            }] if matches!(
                groups.as_slice(),
                [crate::ToolActivityGroup::Bash {
                    node_id: Some(node_id),
                    status: crate::ToolActivityStatus::Pending,
                    output: None,
                    ansi_output: None,
                    ..
                }] if *node_id == bash
            )
        ));
    }

    #[test]
    fn pending_queued_steering_interrupt_is_visible_before_work_state() {
        let mut blocks = vec![crate::TranscriptBlock {
            id: crate::TranscriptBlockId("work".into()),
            status: crate::TranscriptBlockStatus::Pending,
            kind: crate::TranscriptBlockKind::Work {
                state: crate::TurnState::Working {
                    started_at: "1".into(),
                    turn_id: crate::TurnId::new(),
                },
            },
        }];

        insert_pending_interrupt(&mut blocks, true);

        assert!(matches!(
            blocks.as_slice(),
            [
                crate::TranscriptBlock {
                    id: crate::TranscriptBlockId(id),
                    kind: crate::TranscriptBlockKind::Interrupt {
                        queued_steering: true
                    },
                    ..
                },
                crate::TranscriptBlock {
                    kind: crate::TranscriptBlockKind::Work { .. },
                    ..
                }
            ] if id == "pending-interrupt"
        ));
    }

    #[test]
    fn prior_interrupt_does_not_hide_a_later_pending_interrupt() {
        let interrupt = crate::TranscriptBlock {
            id: crate::TranscriptBlockId("old-interrupt".into()),
            status: crate::TranscriptBlockStatus::Completed,
            kind: crate::TranscriptBlockKind::Interrupt {
                queued_steering: true,
            },
        };
        let user = crate::TranscriptBlock {
            id: crate::TranscriptBlockId("next-user".into()),
            status: crate::TranscriptBlockStatus::Completed,
            kind: crate::TranscriptBlockKind::User {
                label: None,
                text: "next turn".into(),
                attachments: Vec::new(),
                images: Vec::new(),
                image_chips: Vec::new(),
            },
        };
        let work = crate::TranscriptBlock {
            id: crate::TranscriptBlockId("work".into()),
            status: crate::TranscriptBlockStatus::Pending,
            kind: crate::TranscriptBlockKind::Work {
                state: crate::TurnState::Working {
                    started_at: "1".into(),
                    turn_id: crate::TurnId::new(),
                },
            },
        };

        assert!(!trailing_interrupt_is_durable(&[
            interrupt.clone(),
            user.clone(),
            work.clone(),
        ]));
        assert!(trailing_interrupt_is_durable(&[user, interrupt, work]));
    }

    #[test]
    fn empty_assistant_placeholders_do_not_split_adjacent_exploration() {
        let first = crate::NodeId::new();
        let placeholder = crate::NodeId::new();
        let second = crate::NodeId::new();
        let blocks = transcript_blocks(
            &[
                event(crate::DurableEventKind::NodeAppended {
                    node_id: first,
                    parent_id: None,
                    turn_id: None,
                    owner_id: None,
                    request_index: None,
                    node_kind: crate::NodeKind::ToolCall,
                    status: "completed".into(),
                    content: serde_json::json!({"name":"read", "arguments":{"path":"SPEC.md"}}),
                }),
                event(crate::DurableEventKind::NodeAppended {
                    node_id: placeholder,
                    parent_id: Some(first),
                    turn_id: None,
                    owner_id: None,
                    request_index: None,
                    node_kind: crate::NodeKind::AssistantMessage,
                    status: "completed".into(),
                    content: serde_json::json!({"text":""}),
                }),
                event(crate::DurableEventKind::NodeAppended {
                    node_id: second,
                    parent_id: Some(placeholder),
                    turn_id: None,
                    owner_id: None,
                    request_index: None,
                    node_kind: crate::NodeKind::ToolCall,
                    status: "completed".into(),
                    content: serde_json::json!({"name":"list", "arguments":{"path":"docs"}}),
                }),
            ],
            std::path::Path::new("/workspace"),
            &crate::TurnState::Idle,
            &[],
            &[],
            None,
        );

        assert!(matches!(
            blocks.as_slice(),
            [crate::TranscriptBlock {
                kind: crate::TranscriptBlockKind::ToolGroups { groups },
                ..
            }] if matches!(groups.as_slice(), [crate::ToolActivityGroup::Exploration { activities, .. }] if activities.len() == 2)
        ));
    }

    #[test]
    fn adjacent_safe_bash_calls_share_the_trailing_active_exploration_block() {
        let first = crate::NodeId::new();
        let second = crate::NodeId::new();
        let blocks = transcript_blocks(
            &[
                event(crate::DurableEventKind::NodeAppended {
                    node_id: first,
                    parent_id: None,
                    turn_id: None,
                    owner_id: None,
                    request_index: None,
                    node_kind: crate::NodeKind::ToolCall,
                    status: "streaming".into(),
                    content: serde_json::json!({
                        "name":"bash",
                        "arguments":{"command":"cat TEST.md && ls"}
                    }),
                }),
                event(crate::DurableEventKind::NodeAppended {
                    node_id: second,
                    parent_id: Some(first),
                    turn_id: None,
                    owner_id: None,
                    request_index: None,
                    node_kind: crate::NodeKind::ToolCall,
                    status: "streaming".into(),
                    content: serde_json::json!({
                        "name":"bash",
                        "arguments":{"command":"ls docs"}
                    }),
                }),
            ],
            std::path::Path::new("/workspace"),
            &crate::TurnState::Working {
                started_at: "2026-01-01T00:00:00Z".into(),
                turn_id: crate::TurnId::new(),
            },
            &[],
            &[],
            None,
        );

        assert!(matches!(
            blocks.as_slice(),
            [
                crate::TranscriptBlock {
                    kind: crate::TranscriptBlockKind::ToolGroups { groups },
                    ..
                },
                crate::TranscriptBlock {
                    kind: crate::TranscriptBlockKind::Work { .. },
                    ..
                }
            ] if matches!(
                groups.as_slice(),
                [crate::ToolActivityGroup::Exploration { active: true, activities }]
                    if matches!(
                        activities.as_slice(),
                        [
                            crate::ExplorationActivity { kind: crate::ExplorationActivityKind::Read, targets, .. },
                            crate::ExplorationActivity { kind: crate::ExplorationActivityKind::List, targets: listed, .. },
                        ] if targets == &["TEST.md"] && listed == &[".", "docs"]
                    )
            )
        ));
    }

    #[test]
    fn later_safe_bash_round_extends_an_existing_exploration_block() {
        let first = crate::NodeId::new();
        let placeholder = crate::NodeId::new();
        let second = crate::NodeId::new();
        let blocks = transcript_blocks(
            &[
                event(crate::DurableEventKind::NodeAppended {
                    node_id: first,
                    parent_id: None,
                    turn_id: None,
                    owner_id: None,
                    request_index: None,
                    node_kind: crate::NodeKind::ToolCall,
                    status: "completed".into(),
                    content: serde_json::json!({
                        "name":"bash",
                        "arguments":{"command":"ls; cat TEST.md", "wait": true}
                    }),
                }),
                event(crate::DurableEventKind::NodeAppended {
                    node_id: placeholder,
                    parent_id: Some(first),
                    turn_id: None,
                    owner_id: None,
                    request_index: None,
                    node_kind: crate::NodeKind::AssistantMessage,
                    status: "completed".into(),
                    content: serde_json::json!({"text":""}),
                }),
                event(crate::DurableEventKind::NodeAppended {
                    node_id: second,
                    parent_id: Some(placeholder),
                    turn_id: None,
                    owner_id: None,
                    request_index: None,
                    node_kind: crate::NodeKind::ToolCall,
                    status: "streaming".into(),
                    content: serde_json::json!({
                        "name":"bash",
                        "arguments":{"command":"cat SPEC.md", "wait": true}
                    }),
                }),
            ],
            std::path::Path::new("/workspace"),
            &crate::TurnState::Working {
                started_at: "2026-01-01T00:00:00Z".into(),
                turn_id: crate::TurnId::new(),
            },
            &[],
            &[],
            None,
        );

        assert!(matches!(
            blocks.as_slice(),
            [
                crate::TranscriptBlock {
                    kind: crate::TranscriptBlockKind::ToolGroups { groups },
                    ..
                },
                crate::TranscriptBlock {
                    kind: crate::TranscriptBlockKind::Work { .. },
                    ..
                }
            ] if matches!(
                groups.as_slice(),
                [crate::ToolActivityGroup::Exploration { active: true, activities }]
                    if matches!(
                        activities.as_slice(),
                        [
                            crate::ExplorationActivity { kind: crate::ExplorationActivityKind::List, targets: listed, .. },
                            crate::ExplorationActivity { kind: crate::ExplorationActivityKind::Read, targets, .. },
                        ] if listed == &["."] && targets == &["TEST.md", "SPEC.md"]
                    )
            )
        ));
    }

    #[test]
    fn empty_streaming_continuation_keeps_trailing_exploration_active() {
        let tool = crate::NodeId::new();
        let continuation = crate::NodeId::new();
        let blocks = transcript_blocks(
            &[
                event(crate::DurableEventKind::NodeAppended {
                    node_id: tool,
                    parent_id: None,
                    turn_id: None,
                    owner_id: None,
                    request_index: None,
                    node_kind: crate::NodeKind::ToolCall,
                    status: "completed".into(),
                    content: serde_json::json!({
                        "name":"bash",
                        "arguments":{"command":"ls .", "wait": true}
                    }),
                }),
                event(crate::DurableEventKind::NodeAppended {
                    node_id: continuation,
                    parent_id: Some(tool),
                    turn_id: None,
                    owner_id: None,
                    request_index: None,
                    node_kind: crate::NodeKind::AssistantMessage,
                    status: "streaming".into(),
                    content: serde_json::json!({"text":""}),
                }),
            ],
            std::path::Path::new("/workspace"),
            &crate::TurnState::Working {
                started_at: "2026-01-01T00:00:00Z".into(),
                turn_id: crate::TurnId::new(),
            },
            &[],
            &[],
            None,
        );

        assert!(matches!(
            blocks.as_slice(),
            [
                crate::TranscriptBlock {
                    kind: crate::TranscriptBlockKind::ToolGroups { groups },
                    ..
                },
                crate::TranscriptBlock {
                    kind: crate::TranscriptBlockKind::Work { .. },
                    ..
                }
            ] if matches!(
                groups.as_slice(),
                [crate::ToolActivityGroup::Exploration { active: true, activities }]
                    if matches!(
                        activities.as_slice(),
                        [crate::ExplorationActivity {
                            kind: crate::ExplorationActivityKind::List,
                            targets,
                            ..
                        }] if targets == &["."]
                    )
            )
        ));
    }

    #[test]
    fn different_tool_activity_groups_use_separate_transcript_blocks() {
        let read = crate::NodeId::new();
        let bash = crate::NodeId::new();
        let blocks = transcript_blocks(
            &[
                event(crate::DurableEventKind::NodeAppended {
                    node_id: read,
                    parent_id: None,
                    turn_id: None,
                    owner_id: None,
                    request_index: None,
                    node_kind: crate::NodeKind::ToolCall,
                    status: "completed".into(),
                    content: serde_json::json!({"name":"read", "arguments":{"path":"SPEC.md"}}),
                }),
                event(crate::DurableEventKind::NodeAppended {
                    node_id: bash,
                    parent_id: Some(read),
                    turn_id: None,
                    owner_id: None,
                    request_index: None,
                    node_kind: crate::NodeKind::ToolCall,
                    status: "completed".into(),
                    content: serde_json::json!({"name":"bash", "arguments":{"command":"cargo test"}}),
                }),
            ],
            std::path::Path::new("/workspace"),
            &crate::TurnState::Idle,
            &[],
            &[],
            None,
        );

        assert!(matches!(
            blocks.as_slice(),
            [
                crate::TranscriptBlock {
                    kind: crate::TranscriptBlockKind::ToolGroups { groups: exploration },
                    ..
                },
                crate::TranscriptBlock {
                    kind: crate::TranscriptBlockKind::ToolGroups { groups: command },
                    ..
                },
            ] if matches!(exploration.as_slice(), [crate::ToolActivityGroup::Exploration { .. }])
                && matches!(command.as_slice(), [crate::ToolActivityGroup::Bash { .. }])
        ));
    }
}
