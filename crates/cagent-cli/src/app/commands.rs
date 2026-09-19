#[allow(clippy::wildcard_imports)]
use super::*;

impl App {
    pub(super) async fn apply_fast_setting(
        &mut self,
        session: &SessionHandle,
        enabled: bool,
    ) -> Result<(), cagent_agent::runtime::RuntimeError> {
        let effective = session.set_fast(enabled).await?;
        if self.fast_effective != effective {
            self.token_rate_tracker = cagent_agent::presentation::TokenRateTracker::default();
        }
        self.fast = enabled;
        self.fast_effective = effective;
        Ok(())
    }

    pub(super) async fn submit_bash(
        &mut self,
        session: &SessionHandle,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let command = self.draft.trim().to_owned();
        self.composer_mode = super::ComposerMode::Prompt;
        if command.is_empty() {
            self.clear_draft();
            return Ok(());
        }
        session
            .submit(SessionCommand::new(SessionAction::RunBash { command }))
            .await?;
        self.clear_draft();
        self.discard_cleared_draft();
        Ok(())
    }
    pub(super) fn open_permissions_surface(
        &mut self,
        session: &SessionHandle,
    ) -> Result<(), cagent_agent::runtime::RuntimeError> {
        let policy = session.persistent_permissions()?;
        let rows = policy
            .session
            .into_iter()
            .map(|rule| PermissionMenuRow {
                scope: cagent_agent::permissions::PermissionScope::Conversation,
                rule,
            })
            .chain(policy.project.into_iter().map(|rule| PermissionMenuRow {
                scope: cagent_agent::permissions::PermissionScope::Project,
                rule,
            }))
            .chain(policy.global.into_iter().map(|rule| PermissionMenuRow {
                scope: cagent_agent::permissions::PermissionScope::Global,
                rule,
            }))
            .collect::<Vec<_>>();
        let count = rows.len() + 1;
        self.surfaces.push(Surface::Permissions {
            rows,
            list: ListState::selectable(count),
        });
        Ok(())
    }

    pub(super) async fn dispatch_observer_command(
        &mut self,
        session: &SessionHandle,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Some(submitted) = self.observer_command_text().map(str::to_owned) else {
            return Ok(());
        };
        let (command, argument) = split_command(&submitted);
        let Some(metadata) = SLASH_COMMANDS
            .iter()
            .find(|candidate| candidate.name == command && candidate.observer_safe)
        else {
            self.clear_observer_command();
            return Ok(());
        };
        let valid = match metadata.name {
            "/diff" => true, // Shared handler validates arguments and rejects clear for observers.
            "/copy" => copy_dialect(argument).is_ok(),
            "/resume" => argument
                .is_none_or(|id| id.parse::<cagent_agent::protocol::ConversationId>().is_ok()),
            _ => argument.is_none(),
        };
        if !valid {
            self.clear_observer_command();
            return Ok(());
        }

        self.clear_observer_command();
        match command {
            "/providers" => self.open_provider_surface(session).await,
            "/search" => {
                let rows = session.web_search_providers();
                let item_count = rows.len() + 1;
                self.surfaces.push(Surface::WebSearchPicker {
                    rows,
                    list: ListState::selectable(item_count),
                    query: String::new(),
                    query_cursor: 0,
                });
            }
            "/statusline" => {
                let config = self.status_line_config.clone();
                let preview = self.status_line_values();
                let rows = status_line_rows(&config);
                let item_count = rows.len();
                self.surfaces.push(Surface::StatusLine {
                    rows,
                    config,
                    list: ListState::selectable(item_count),
                    mode: StatusLineEditorMode::Modules,
                    preview,
                });
            }
            "/mcp" => match session.mcp_servers().await {
                Ok(rows) => {
                    let item_count = rows.len() + 2;
                    self.surfaces.push(Surface::McpServers {
                        rows,
                        list: ListState::selectable(item_count),
                    });
                }
                Err(error) => self.set_notice(format!("MCP catalog unavailable · {error}")),
            },
            "/help" => self.surfaces.push(Surface::Help {
                tab: HelpTab::default(),
                command_rows: observer_help_command_rows(),
                key_rows: self.key_bindings.help_rows(),
                list: ScrollViewState::new(observer_help_command_rows().len()),
            }),
            "/background" => self.open_background_surface(),
            "/files" => self.toggle_files_sidebar(),
            "/copy" => {
                let dialect =
                    copy_dialect(argument).expect("observer copy arguments were validated");
                if let Some(response) = self.latest_assistant_source() {
                    let text = cagent_agent::presentation::copy_markdown(response, dialect);
                    if copy_to_clipboard(&text) {
                        self.set_notice(match dialect {
                            Some(cagent_agent::presentation::MarkdownDialect::Slack) => {
                                "copied last assistant response for Slack"
                            }
                            Some(cagent_agent::presentation::MarkdownDialect::Discord) => {
                                "copied last assistant response for Discord"
                            }
                            None => "copied last assistant response",
                        });
                    } else {
                        self.set_notice("clipboard unavailable");
                    }
                } else {
                    self.set_notice("no completed assistant response");
                }
            }
            "/resume" => {
                if let Some(id) = argument {
                    self.pending_action = Some(AppAction::Resume(Some(
                        id.parse().expect("observer resume ID was validated"),
                    )));
                } else {
                    let rows = session.conversations().await?;
                    if rows.is_empty() {
                        self.set_notice("no conversations found for this workspace");
                    } else {
                        let visible =
                            cagent_agent::presentation::conversation_picker_indexes(&rows, "");
                        let selected =
                            cagent_agent::presentation::conversation_picker_initial_selection(
                                &rows, "",
                            );
                        self.surfaces.push(Surface::Conversations {
                            rows,
                            list: ListState::selectable_at(
                                visible.len(),
                                selected,
                                VISIBLE_MENU_ITEMS,
                            ),
                            query: String::new(),
                            query_cursor: 0,
                            opened_at_millis: picker_opened_at_millis(),
                            include_archived: false,
                        });
                    }
                }
            }
            "/diff" => {
                self.open_diff(session, argument).await;
            }
            "/new" => self.pending_action = Some(AppAction::NewSession(None)),
            "/quit" => self.pending_action = Some(AppAction::Quit),
            _ => unreachable!("observer command metadata and dispatcher are out of sync"),
        }
        Ok(())
    }

    pub(super) async fn open_rename_surface(
        &mut self,
        session: &SessionHandle,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if self.is_observer() {
            return Ok(());
        }
        let title = session.title().await?.unwrap_or_default();
        let title = title
            .chars()
            .filter(|character| !character.is_control())
            .collect::<String>();
        let title = title.trim().to_owned();
        let cursor = cursor_at_end(&title);
        self.surfaces.push(Surface::Rename { title, cursor });
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    pub(super) async fn submit_draft(
        &mut self,
        session: &SessionHandle,
        target: QueueTarget,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.submit_draft_impl(session, target, false).await
    }

    pub(super) async fn submit_draft_deferred(
        &mut self,
        session: &SessionHandle,
        target: QueueTarget,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.submit_draft_impl(session, target, true).await
    }

    #[allow(clippy::too_many_lines)]
    async fn submit_draft_impl(
        &mut self,
        session: &SessionHandle,
        target: QueueTarget,
        defer_submission: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if self.is_observer() {
            return Ok(());
        }
        if self.draft.trim().is_empty() {
            return Ok(());
        }
        let submitted = self.draft.trim().to_owned();
        let (command, argument) = if self.draft.starts_with('/') {
            split_command(&submitted)
        } else {
            ("", None)
        };
        let direct_mode = session
            .mode_profiles()?
            .into_iter()
            .find(|mode| format!("/{}", mode.name) == command)
            .map(|mode| mode.name);
        let slash_command_user_text = if command == "/mode" {
            argument
                .and_then(|argument| split_command(argument).1)
                .map(str::to_owned)
        } else if direct_mode.is_some() {
            argument.map(str::to_owned)
        } else if command == "/spawn" {
            argument.map(str::to_owned)
        } else if command == "/search"
            && argument.is_some()
            && session
                .web_search_providers()
                .into_iter()
                .any(|provider| provider.active && provider.ready)
        {
            argument.map(|query| format!("Search {query}"))
        } else {
            None
        };
        let mut mode_command_with_message = false;
        let mut spawn_command_with_message = false;
        let mut web_search_command_with_message = false;
        let mut queued_mode_command = None::<(String, String)>;
        if let Some(command) = SLASH_COMMANDS
            .iter()
            .find(|candidate| candidate.name == command)
            .copied()
        {
            self.record_executed_command(
                SlashSuggestion {
                    name: command.name.into(),
                    alias: command.alias,
                    argument_hint: command.argument_hint,
                    insert_argument_hint: command.insert_argument_hint,
                    description: command.description.into(),
                },
                &submitted,
            );
            if let Some(user_text) = slash_command_user_text {
                session
                    .record_executed_slash_command_with_message(&submitted, user_text.as_str())
                    .await?;
            } else {
                session.record_executed_slash_command(&submitted).await?;
            }
        } else if let Some(mode) = &direct_mode {
            self.record_executed_command(
                SlashSuggestion {
                    name: format!("/{mode}"),
                    alias: None,
                    argument_hint: Some("[message]"),
                    insert_argument_hint: false,
                    description: format!("switch to {mode} mode and optionally send a message"),
                },
                &submitted,
            );
            if let Some(user_text) = slash_command_user_text {
                session
                    .record_executed_slash_command_with_message(&submitted, user_text.as_str())
                    .await?;
            } else {
                session.record_executed_slash_command(&submitted).await?;
            }
        }
        match command {
            "/dir" => {
                self.clear_draft();
                if let Some(path) = argument {
                    match session.change_working_directory(path).await {
                        Ok(transition) => {
                            self.workspace = transition.cwd;
                            self.files_sidebar.tree = None;
                            self.files_sidebar.focused = false;
                            self.history_layout.rendered = None;
                        }
                        Err(cagent_agent::protocol::RuntimeError::InvalidOption(message)) => {
                            self.set_notice(message);
                        }
                        Err(error) => return Err(error.into()),
                    }
                } else {
                    let path =
                        cagent_agent::runtime::worktrees::display_absolute_path(&self.workspace);
                    let message = format!("Current directory: {path}");
                    session.append_system_message(&message).await?;
                    self.push_system_message(&message);
                }
                return Ok(());
            }
            "/providers" => {
                self.clear_draft();
                self.open_provider_surface(session).await;
                return Ok(());
            }
            "/fast" => {
                self.clear_draft();
                let enabled = match argument {
                    None => !self.fast,
                    Some("on") => true,
                    Some("off") => false,
                    Some(_) => {
                        self.set_notice("usage: /fast [on|off]");
                        return Ok(());
                    }
                };
                match self.apply_fast_setting(session, enabled).await {
                    Ok(()) => {}
                    Err(cagent_agent::protocol::RuntimeError::InvalidOption(message)) => {
                        self.set_notice(message);
                    }
                    Err(error) => return Err(error.into()),
                }
                return Ok(());
            }
            "/permissions" => {
                self.clear_draft();
                if let Some(argument) = argument {
                    let (subcommand, command) = split_command(argument);
                    if subcommand != "simulate" {
                        self.set_notice("usage: /permissions simulate <bash command>");
                        return Ok(());
                    }
                    match session
                        .simulate_bash_permissions(command.unwrap_or(""))
                        .await
                    {
                        Ok(message) => {
                            session.append_system_message(&message).await?;
                            self.push_system_message(&message);
                        }
                        Err(cagent_agent::protocol::RuntimeError::InvalidOption(message)) => {
                            self.set_notice(message);
                        }
                        Err(error) => return Err(error.into()),
                    }
                } else {
                    self.open_permissions_surface(session)?;
                }
                return Ok(());
            }
            "/model" => {
                self.clear_draft();
                if let Some(argument) = argument {
                    if argument == "default" {
                        match session
                            .submit(SessionCommand::new(SessionAction::UseAgentDefault))
                            .await
                        {
                            Ok(_) => self.set_notice("using agent model default"),
                            Err(cagent_agent::protocol::RuntimeError::InvalidOption(message)) => {
                                self.set_notice(message);
                            }
                            Err(error) => return Err(error.into()),
                        }
                        return Ok(());
                    }
                    let rows = self.load_model_rows(session).await?;
                    if let Some(row) =
                        cagent_agent::presentation::resolve_model_picker_row(&rows, argument)
                            .cloned()
                    {
                        self.select_model_row(session, row, ModelSelectionTarget::Conversation)
                            .await?;
                    } else {
                        self.set_notice(format!("model not found · {argument}"));
                    }
                } else {
                    self.open_model_surface(session).await?;
                }
                return Ok(());
            }
            "/agent" => {
                self.clear_draft();
                if let Some(agent) = argument {
                    let agent = agent.to_owned();
                    match session
                        .submit(SessionCommand::new(SessionAction::ChangeAgent {
                            agent: agent.clone(),
                        }))
                        .await
                    {
                        Ok(_) => {}
                        Err(cagent_agent::protocol::RuntimeError::InvalidOption(message))
                            if message == format!("unknown agent profile: {agent}") =>
                        {
                            self.set_notice(format!("unknown agent: {agent}"));
                        }
                        Err(cagent_agent::protocol::RuntimeError::InvalidOption(message)) => {
                            self.set_notice(message);
                        }
                        Err(error) => {
                            self.set_notice(format!("agent change failed · {error}"));
                        }
                    }
                } else {
                    self.open_agent_surface(session)?;
                }
                return Ok(());
            }
            "/skills" => {
                self.clear_draft();
                let rows = session.skills();
                self.surfaces.push(Surface::Skills {
                    list: ListState::selectable(rows.len() + 2),
                    rows,
                });
                return Ok(());
            }
            "/mode" => {
                if let Some(argument) = argument {
                    let (mode, message) = split_command(argument);
                    let Some(message) = message else {
                        match session
                            .submit(SessionCommand::new(SessionAction::ChangeMode {
                                mode: mode.into(),
                            }))
                            .await
                        {
                            Ok(_) => {}
                            Err(cagent_agent::protocol::RuntimeError::InvalidOption(message)) => {
                                self.set_notice(message);
                            }
                            Err(error) => return Err(error.into()),
                        }
                        self.clear_draft();
                        return Ok(());
                    };
                    if self.active || self.editing_queue.is_some() {
                        queued_mode_command = Some((mode.into(), submitted.clone()));
                    } else {
                        match session
                            .submit(SessionCommand::new(SessionAction::ChangeMode {
                                mode: mode.into(),
                            }))
                            .await
                        {
                            Ok(_) => {}
                            Err(cagent_agent::protocol::RuntimeError::InvalidOption(message)) => {
                                self.set_notice(message);
                                self.clear_draft();
                                return Ok(());
                            }
                            Err(error) => return Err(error.into()),
                        }
                    }
                    let attachments = self.confirmed_attachments();
                    self.replace_draft_with_attachment_specs(message, &attachments);
                    mode_command_with_message = true;
                } else {
                    self.clear_draft();
                    self.open_mode_surface(session)?;
                    return Ok(());
                }
            }
            "/spawn" => {
                let Some(message) = argument else {
                    self.clear_draft();
                    self.set_notice("usage: /spawn <message>");
                    return Ok(());
                };
                let attachments = self.confirmed_attachments();
                self.replace_draft_with_attachment_specs(message, &attachments);
                spawn_command_with_message = true;
            }
            "/search" => {
                let Some(query) = argument else {
                    self.clear_draft();
                    let rows = session.web_search_providers();
                    let item_count = rows.len() + 1;
                    self.surfaces.push(Surface::WebSearchPicker {
                        rows,
                        list: ListState::selectable(item_count),
                        query: String::new(),
                        query_cursor: 0,
                    });
                    return Ok(());
                };
                if !session
                    .web_search_providers()
                    .into_iter()
                    .any(|row| row.active && row.ready)
                {
                    self.replace_draft(query);
                    self.set_notice("configure a web-search provider to search");
                    let rows = session.web_search_providers();
                    let item_count = rows.len() + 1;
                    self.surfaces.push(Surface::WebSearchPicker {
                        rows,
                        list: ListState::selectable(item_count),
                        query: String::new(),
                        query_cursor: 0,
                    });
                    return Ok(());
                }
                let search_prompt = format!("Search {query}");
                let attachments = self.confirmed_attachments();
                self.replace_draft_with_attachment_specs(&search_prompt, &attachments);
                web_search_command_with_message = true;
            }
            "/statusline" => {
                self.clear_draft();
                let config = self.status_line_config.clone();
                let preview = self.status_line_values();
                let rows = status_line_rows(&config);
                let item_count = rows.len();
                self.surfaces.push(Surface::StatusLine {
                    rows,
                    config,
                    list: ListState::selectable(item_count),
                    mode: StatusLineEditorMode::Modules,
                    preview,
                });
                return Ok(());
            }
            "/settings" => {
                self.clear_draft();
                match session.settings() {
                    Ok(rows) => self.surfaces.push(Surface::Settings {
                        list: ListState::selectable(
                            rows.iter()
                                .filter(|row| {
                                    row.definition.section
                                        == cagent_agent::config::SettingSection::General
                                })
                                .count(),
                        ),
                        rows,
                        section: cagent_agent::config::SettingSection::General,
                        query: String::new(),
                        query_cursor: 0,
                    }),
                    Err(error) => self.set_notice(format!("settings unavailable · {error}")),
                }
                return Ok(());
            }
            "/usage" => {
                self.clear_draft();
                match session.usage_overview().await {
                    Ok(overview) => self.surfaces.push(Surface::Usage {
                        overview,
                        list: ListState::selectable(4),
                    }),
                    Err(error) => self.set_notice(format!("usage unavailable · {error}")),
                }
                return Ok(());
            }
            "/mcp" => {
                self.clear_draft();
                match session.mcp_servers().await {
                    Ok(rows) => {
                        let item_count = rows.len() + 2;
                        self.surfaces.push(Surface::McpServers {
                            rows,
                            list: ListState::selectable(item_count),
                        });
                    }
                    Err(error) => self.set_notice(format!("MCP catalog unavailable · {error}")),
                }
                return Ok(());
            }
            _ if direct_mode.is_some() => {
                let mode = direct_mode.expect("guarded mode exists");
                let Some(message) = argument else {
                    session
                        .submit(SessionCommand::new(SessionAction::ChangeMode { mode }))
                        .await?;
                    self.clear_draft();
                    return Ok(());
                };
                if self.active || self.editing_queue.is_some() {
                    queued_mode_command = Some((mode, submitted.clone()));
                } else {
                    session
                        .submit(SessionCommand::new(SessionAction::ChangeMode { mode }))
                        .await?;
                }
                let attachments = self.confirmed_attachments();
                self.replace_draft_with_attachment_specs(message, &attachments);
                mode_command_with_message = true;
            }
            "/help" => {
                self.clear_draft();
                let command_rows = configured_help_command_rows(self.manual_cleanup_available);
                self.surfaces.push(Surface::Help {
                    tab: HelpTab::default(),
                    list: ScrollViewState::new(command_rows.len()),
                    command_rows,
                    key_rows: self.key_bindings.help_rows(),
                });
                return Ok(());
            }
            "/background" | "/bg" => {
                self.clear_draft();
                self.open_background_surface();
                return Ok(());
            }
            "/copy" => {
                let dialect = match copy_dialect(argument) {
                    Ok(dialect) => dialect,
                    Err(usage) => {
                        self.clear_draft();
                        self.set_notice(usage);
                        return Ok(());
                    }
                };
                if let Some(response) = self.latest_assistant_source() {
                    let text = cagent_agent::presentation::copy_markdown(response, dialect);
                    if copy_to_clipboard(&text) {
                        self.set_notice(match dialect {
                            Some(cagent_agent::presentation::MarkdownDialect::Slack) => {
                                "copied last assistant response for Slack"
                            }
                            Some(cagent_agent::presentation::MarkdownDialect::Discord) => {
                                "copied last assistant response for Discord"
                            }
                            None => "copied last assistant response",
                        });
                    } else {
                        self.set_notice("clipboard unavailable");
                    }
                } else {
                    self.set_notice("no completed assistant response");
                }
                self.clear_draft();
                return Ok(());
            }
            "/new" => {
                self.clear_draft();
                self.pending_action = Some(AppAction::NewSession(argument.map(str::to_owned)));
                return Ok(());
            }
            "/tree" => {
                self.clear_draft();
                self.open_history_surface(session, TreePurpose::Browse)?;
                return Ok(());
            }
            "/files" => {
                self.clear_draft();
                self.toggle_files_sidebar();
                return Ok(());
            }
            "/worktree" => {
                self.clear_draft();
                if let Some(argument) = argument {
                    let (name, base) = split_command(argument);
                    match session.resolve_or_create_worktree(name, base) {
                        Ok(path) => self.pending_action = Some(AppAction::SwitchWorkspace(path)),
                        Err(error) => self.set_notice(format!("worktree failed · {error}")),
                    }
                } else {
                    match session.worktrees() {
                        Ok(rows) => {
                            let selected = rows
                                .iter()
                                .position(|row| row.current)
                                .map_or(0, |index| index + 1);
                            self.surfaces.push(Surface::Worktrees {
                                list: ListState::selectable_at(
                                    rows.len() + 1,
                                    selected,
                                    VISIBLE_MENU_ITEMS,
                                ),
                                rows,
                            });
                        }
                        Err(error) => self.set_notice(format!("worktree list failed · {error}")),
                    }
                }
                return Ok(());
            }
            "/fork" => {
                self.clear_draft();
                self.open_history_surface(session, TreePurpose::Fork)?;
                return Ok(());
            }
            "/resume" => {
                self.clear_draft();
                let id = if let Some(argument) = argument {
                    if let Ok(id) = argument.parse() {
                        Some(id)
                    } else {
                        self.set_notice("invalid conversation ID");
                        return Ok(());
                    }
                } else {
                    let rows = session.conversations().await?;
                    if rows.is_empty() {
                        self.set_notice("no conversations found for this workspace");
                    } else {
                        let visible =
                            cagent_agent::presentation::conversation_picker_indexes(&rows, "");
                        let selected =
                            cagent_agent::presentation::conversation_picker_initial_selection(
                                &rows, "",
                            );
                        self.surfaces.push(Surface::Conversations {
                            rows,
                            list: ListState::selectable_at(
                                visible.len(),
                                selected,
                                VISIBLE_MENU_ITEMS,
                            ),
                            query: String::new(),
                            query_cursor: 0,
                            opened_at_millis: picker_opened_at_millis(),
                            include_archived: false,
                        });
                    }
                    return Ok(());
                };
                self.pending_action = Some(AppAction::Resume(id));
                return Ok(());
            }
            "/continue" => {
                self.clear_draft();
                self.pending_action = Some(AppAction::Resume(None));
                return Ok(());
            }
            "/rename" => {
                self.clear_draft();
                if let Some(title) = argument {
                    match session
                        .submit(SessionCommand::new(SessionAction::RenameConversation {
                            title: title.into(),
                        }))
                        .await
                    {
                        Ok(_) => {}
                        Err(error) => self.set_notice(error.to_string()),
                    }
                } else {
                    self.open_rename_surface(session).await?;
                }
                return Ok(());
            }
            "/archive" => {
                self.clear_draft();
                self.pending_action = Some(AppAction::Archive);
                return Ok(());
            }
            "/delete" => {
                self.clear_draft();
                self.pending_action = Some(AppAction::Delete);
                return Ok(());
            }
            "/retry" => {
                session
                    .submit(SessionCommand::new(SessionAction::Retry))
                    .await?;
                self.clear_draft();
                return Ok(());
            }
            "/compact" => {
                session
                    .submit(SessionCommand::new(SessionAction::Compact {
                        target,
                        instructions: argument.map(str::to_owned),
                    }))
                    .await?;
                self.clear_draft();
                self.set_notice(if self.active {
                    "queued context compaction"
                } else {
                    "compacting active branch context"
                });
                return Ok(());
            }
            "/recap" => {
                self.clear_draft();
                match session
                    .submit(SessionCommand::new(SessionAction::Recap))
                    .await
                {
                    Ok(_) => self.set_notice("generating recap"),
                    Err(cagent_agent::protocol::RuntimeError::InvalidOption(message)) => {
                        self.set_notice(message);
                    }
                    Err(error) => return Err(error.into()),
                }
                return Ok(());
            }
            "/context" => {
                self.clear_draft();
                if argument != Some("save") {
                    self.set_notice("usage: /context save");
                    return Ok(());
                }
                match session.save_context().await {
                    Ok(path) => {
                        let message = format!("Context saved to {}", path.display());
                        session.append_system_message(&message).await?;
                        self.push_system_message(&message);
                    }
                    Err(cagent_agent::protocol::RuntimeError::InvalidOption(message)) => {
                        self.set_notice(message);
                    }
                    Err(error) => return Err(error.into()),
                }
                return Ok(());
            }
            "/cleanup" if self.manual_cleanup_available => {
                self.clear_draft();
                match session.cleanup_conversations().await {
                    Ok(report) => {
                        let removed = report.removed.len();
                        let skipped = report.skipped.len();
                        let failures = report.failures.len();
                        let mut notice = format!("cleanup removed {removed}");
                        if skipped > 0 {
                            notice.push_str(&format!(" · skipped {skipped}"));
                        }
                        if failures > 0 {
                            notice.push_str(&format!(" · failed {failures}"));
                        }
                        if report.remaining.bytes > 0 || report.remaining.conversations > 0 {
                            notice.push_str(" · limits remain exceeded");
                        }
                        self.set_notice(notice);
                    }
                    Err(error) => self.set_notice(format!("cleanup failed · {error}")),
                }
                return Ok(());
            }
            "/reload" => {
                session
                    .submit(SessionCommand::new(SessionAction::ReloadStartupResources))
                    .await?;
                self.clear_draft();
                self.set_notice("reloading configuration and refreshing model catalogs");
                return Ok(());
            }
            "/quit" => {
                self.clear_draft();
                self.pending_action = Some(AppAction::Quit);
                return Ok(());
            }
            "/diff" => {
                self.clear_draft();
                self.open_diff(session, argument).await;
                return Ok(());
            }
            _ => {}
        }
        if self.draft.starts_with('/') {
            let (command, arguments) = split_command(&submitted);
            if let Some(prompt) = session.resolve_skill_command(command, arguments.unwrap_or(""))? {
                self.replace_draft(&prompt);
            } else {
                let message = format!("Unknown command: {submitted}");
                session.record_invalid_slash_command(&submitted).await?;
                session.append_system_message(&message).await?;
                self.clear_draft();
                self.push_system_message(&message);
                return Ok(());
            }
        }
        if self.provider_model.is_empty() {
            self.surfaces.push(if self.enabled_providers.is_empty() {
                Surface::ProvidersRequired
            } else {
                Surface::ModelRequired
            });
            return Ok(());
        }
        let mut text = self.normalized_submission_text();
        if self.draft.starts_with(" /") && text.starts_with(' ') {
            text.remove(0);
        }
        let attachments = self
            .attachments
            .iter()
            .map(|chip| chip.spec.clone())
            .collect::<Vec<_>>();
        if !self.images.is_empty() {
            if !session.selected_model_supports_image_input().await? {
                self.set_notice("selected model does not support image input");
                return Ok(());
            }
            let draft = self.user_draft();
            let editing_queued = self.editing_queue.is_some();
            let action = if let Some(message) = self.editing_queue.take() {
                SessionAction::ReplaceQueuedDraft {
                    id: message.id,
                    draft: draft.clone(),
                    target: (target == QueueTarget::EndOfTurn).then_some(target),
                }
            } else if self.active {
                SessionAction::QueueDraft {
                    draft: draft.clone(),
                    target,
                }
            } else {
                SessionAction::SubmitDraft {
                    draft: draft.clone(),
                }
            };
            let command = SessionCommand::new(action);
            if defer_submission && !editing_queued {
                let optimistic_queue_id = self.optimistic_queue_message(&draft, target);
                self.pending_action = Some(AppAction::Submit(DeferredSubmission {
                    command,
                    draft: draft.clone(),
                    optimistic_queue_id,
                }));
            } else {
                session.submit(command).await?;
            }
            let entry = HistoryEntry {
                kind: cagent_agent::protocol::ComposerInputKind::Prompt,
                text: draft.text,
                attachment_specs: draft.attachment_specs,
                images: draft.images,
                image_chips: draft.image_chips,
            };
            self.history_entries.retain(|candidate| candidate != &entry);
            self.history_entries.push(entry);
            self.discard_cleared_draft();
            self.follow_history_tail = true;
            self.clear_draft();
            return Ok(());
        }
        let queued_mode_submission = queued_mode_command.is_some();
        if let Some(message) = self.editing_queue.take() {
            let action = if let Some((mode, command_text)) = queued_mode_command {
                SessionAction::ReplaceQueuedModeInput {
                    id: message.id,
                    mode,
                    command_text,
                    text,
                    target: (target == QueueTarget::EndOfTurn).then_some(target),
                    attachments,
                }
            } else {
                SessionAction::ReplaceQueued {
                    id: message.id,
                    text,
                    target: (target == QueueTarget::EndOfTurn).then_some(target),
                    attachments,
                }
            };
            match session.submit(SessionCommand::new(action)).await {
                Ok(_) => {}
                Err(cagent_agent::protocol::RuntimeError::InvalidOption(message))
                    if queued_mode_submission =>
                {
                    self.set_notice(message);
                    self.clear_draft();
                    return Ok(());
                }
                Err(error) => return Err(error.into()),
            }
            self.clear_draft();
            return Ok(());
        }
        let command = if let Some((mode, command_text)) = queued_mode_command {
            SessionCommand::new(SessionAction::QueueModeInputWithAttachments {
                mode,
                command_text,
                text: text.clone(),
                target,
                attachments: attachments.clone(),
            })
        } else if web_search_command_with_message && attachments.is_empty() {
            SessionCommand::new(SessionAction::SubmitWebSearchInput { text: text.clone() })
        } else if web_search_command_with_message {
            SessionCommand::new(SessionAction::SubmitWebSearchWithAttachments {
                text: text.clone(),
                attachments: attachments.clone(),
            })
        } else if spawn_command_with_message && attachments.is_empty() {
            SessionCommand::new(SessionAction::SubmitSpawnInput { text: text.clone() })
        } else if spawn_command_with_message {
            SessionCommand::new(SessionAction::SubmitSpawnWithAttachments {
                text: text.clone(),
                attachments: attachments.clone(),
            })
        } else if self.active {
            SessionCommand::new(SessionAction::QueueInputWithAttachments {
                text: text.clone(),
                target,
                attachments: attachments.clone(),
            })
        } else if attachments.is_empty() {
            SessionCommand::submit_input(text.clone())
        } else {
            SessionCommand::new(SessionAction::SubmitWithAttachments {
                text: text.clone(),
                attachments: attachments.clone(),
            })
        };
        let submission = if defer_submission {
            let draft = self.user_draft();
            let optimistic_queue_id = self.optimistic_queue_message(&draft, target);
            self.pending_action = Some(AppAction::Submit(DeferredSubmission {
                command,
                draft,
                optimistic_queue_id,
            }));
            Ok(())
        } else {
            session.submit(command).await.map(|_| ())
        };
        match submission {
            Ok(_) => {}
            Err(cagent_agent::protocol::RuntimeError::InvalidOption(message))
                if queued_mode_submission =>
            {
                self.set_notice(message);
                self.clear_draft();
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        }
        self.discard_cleared_draft();
        // A submitted prompt is an explicit request to resume the live end of
        // the conversation, even when the user was inspecting older output.
        self.follow_history_tail = true;
        if !mode_command_with_message
            && !spawn_command_with_message
            && !web_search_command_with_message
        {
            self.record_history_entry(text, self.confirmed_attachments());
        }
        self.clear_draft();
        Ok(())
    }

    fn optimistic_queue_message(
        &mut self,
        draft: &cagent_agent::protocol::UserDraft,
        target: QueueTarget,
    ) -> Option<cagent_agent::protocol::QueuedMessageId> {
        if !self.active {
            return None;
        }
        let id = cagent_agent::protocol::QueuedMessageId::new();
        self.queued.push(cagent_agent::protocol::QueuedMessage {
            id,
            position: self
                .queued
                .iter()
                .map(|message| message.position)
                .max()
                .map_or(0, |position| position.saturating_add(1)),
            target,
            kind: cagent_agent::protocol::QueuedItemKind::Prompt,
            mode: None,
            command_text: None,
            text: draft.text.clone(),
            attachments: draft.attachment_specs.clone(),
            images: draft.images.clone(),
            image_chips: draft.image_chips.clone(),
            blocked_by_startup: false,
            require_subagent: false,
        });
        Some(id)
    }

    pub(super) async fn open_diff(&mut self, session: &SessionHandle, argument: Option<&str>) {
        use cagent_agent::{config::UiDiffMode, tools::DiffCommand};
        match DiffCommand::parse(argument, session.diff_mode()) {
            Err(usage) => self.set_notice(usage),
            Ok(DiffCommand::Clear) if self.is_read_only_view() => {
                self.set_notice("observers cannot clear conversation diff tracking");
            }
            Ok(DiffCommand::Clear) => match session.clear_conversation_diff().await {
                Ok(()) => self
                    .set_notice("conversation diff tracking cleared; files and history unchanged"),
                Err(error) => self.set_notice(error.to_string()),
            },
            Ok(DiffCommand::View(UiDiffMode::Git)) => self.open_repository_diff().await,
            Ok(DiffCommand::View(UiDiffMode::Conversation)) => {
                match session.load_conversation_diff().await {
                    Ok(recorded) => {
                        let empty = recorded.diff.files.is_empty();
                        if !empty {
                            self.show_recorded_diff(recorded.diff, "Conversation diff");
                        }
                        if recorded.incomplete_history {
                            self.set_notice(if empty {
                                "no recorded conversation changes; historical coverage is incomplete because some successful patch output was unavailable"
                            } else {
                                "historical diff coverage is incomplete because some successful patch output was unavailable"
                            });
                        } else if empty {
                            self.set_notice("no recorded conversation changes");
                        }
                    }
                    Err(error) => self.set_notice(error.to_string()),
                }
            }
        }
    }

    pub(super) async fn open_repository_diff(&mut self) {
        match cagent_agent::tools::load_repository_diff(&self.workspace).await {
            Ok(diff) if diff.files.is_empty() => self.set_notice("repository diff is clean"),
            Ok(diff) => self.show_repository_diff(diff),
            Err(error) => self.set_notice(error.to_string()),
        }
    }

    pub(super) fn show_repository_diff(&mut self, diff: cagent_agent::tools::SemanticDiff) {
        self.show_recorded_diff(diff, "Repository diff");
    }

    fn show_recorded_diff(&mut self, diff: cagent_agent::tools::SemanticDiff, title: &'static str) {
        self.surfaces.push(Surface::Expanded {
            view: ExpandedView::RepositoryDiff { diff, title },
            scroll: 0,
            viewport_rows: usize::from(self.render_height.saturating_sub(4).max(1)),
        });
    }
}
