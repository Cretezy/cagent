use super::surfaces::{
    cached_expanded_scroll_metrics, cached_permission_diff_max_scroll_for_request, change_help_tab,
    change_settings_section, expanded_scroll_metrics,
};
#[allow(clippy::wildcard_imports)]
use super::*;
use cagent_agent::presentation::{
    conversation_picker_indexes, conversation_picker_initial_selection, history_picker_indexes,
};

#[path = "surface_control_mcp.rs"]
mod surface_control_mcp;
#[path = "surface_control_settings.rs"]
mod surface_control_settings;
use surface_control_mcp::*;
#[cfg(test)]
pub(crate) use surface_control_settings::concise_setting_error;
use surface_control_settings::*;

fn paste_single_line(value: &mut String, cursor: &mut usize, pasted: &str) {
    let mut input = SingleLineInput::new(std::mem::take(value), *cursor);
    input.insert_text(pasted);
    (*value, *cursor) = input.into_parts();
}

pub(super) fn background_work_remains_after_kill(
    rows: &[cagent_agent::presentation::SupervisedWork],
    target: cagent_agent::runtime::SupervisedWorkTarget,
) -> bool {
    let target_run = match target {
        cagent_agent::runtime::SupervisedWorkTarget::Agent(id) => rows.iter().find_map(|work| {
            let cagent_agent::presentation::SupervisedWork::Agent { run } = work else {
                return None;
            };
            (run.id == id).then_some(run.as_ref())
        }),
        cagent_agent::runtime::SupervisedWorkTarget::Terminal(_) => None,
    };

    rows.iter().any(|work| {
        if !work.active() {
            return false;
        }
        match (target, work) {
            (
                cagent_agent::runtime::SupervisedWorkTarget::Agent(id),
                cagent_agent::presentation::SupervisedWork::Agent { run },
            ) => run.id != id,
            (
                cagent_agent::runtime::SupervisedWorkTarget::Agent(id),
                cagent_agent::presentation::SupervisedWork::Terminal { terminal },
            ) => {
                terminal.owner_agent_run_id != Some(id)
                    && !target_run.is_some_and(|run| {
                        cagent_agent::presentation::terminal_belongs_to_agent_run(terminal, run)
                    })
            }
            (
                cagent_agent::runtime::SupervisedWorkTarget::Terminal(id),
                cagent_agent::presentation::SupervisedWork::Terminal { terminal },
            ) => terminal.id != id,
            (
                cagent_agent::runtime::SupervisedWorkTarget::Terminal(_),
                cagent_agent::presentation::SupervisedWork::Agent { .. },
            ) => true,
        }
    })
}

fn question_option_index(
    question_index: usize,
    questions: &[cagent_agent::QuestionPrompt],
    answers: &[QuestionAnswer],
    answered: &[bool],
) -> usize {
    let Some(question) = questions.get(question_index) else {
        return 0;
    };
    if !answered[question_index] {
        return 0;
    }
    answers[question_index]
        .selection
        .as_ref()
        .and_then(|selection| {
            question
                .options
                .iter()
                .position(|option| &option.label == selection)
        })
        .unwrap_or(question.options.len())
}

#[allow(dead_code)]
pub(super) fn agent_menu_row(profile: cagent_agent::config::AgentProfile) -> (String, String) {
    (profile.name, profile.description)
}

fn agent_edit_list(selected: usize) -> ListState {
    ListState::selectable_at(5, selected, VISIBLE_MENU_ITEMS)
}

fn history_list(
    rows: &[cagent_agent::presentation::HistoryRow],
    query: &str,
    selected_source: usize,
) -> ListState {
    let visible = history_picker_indexes(rows, query);
    let selected = visible
        .iter()
        .position(|source| *source == selected_source)
        .or_else(|| visible.iter().position(|source| rows[*source].selectable))
        .unwrap_or(0);
    ListState::selectable_at(visible.len(), selected, VISIBLE_MENU_ITEMS)
}

pub(super) fn apply_history_list_action(
    list: &mut ListState,
    action: ListAction,
    rows: &[cagent_agent::presentation::HistoryRow],
    visible: &[usize],
    capacity: usize,
) {
    list.apply(action, ListMode::Selectable, capacity);
    let Some(target) = list.selected else {
        return;
    };
    if visible
        .get(target)
        .is_some_and(|source| rows[*source].selectable)
    {
        return;
    }
    let toward_start = matches!(
        action,
        ListAction::Previous
            | ListAction::PagePrevious
            | ListAction::WheelPrevious
            | ListAction::Home
    );
    let next = if toward_start {
        (0..=target)
            .rev()
            .find(|position| rows[visible[*position]].selectable)
            .or_else(|| {
                (target + 1..visible.len()).find(|position| rows[visible[*position]].selectable)
            })
    } else {
        (target..visible.len())
            .find(|position| rows[visible[*position]].selectable)
            .or_else(|| {
                (0..target)
                    .rev()
                    .find(|position| rows[visible[*position]].selectable)
            })
    };
    if let Some(next) = next {
        list.select(next, capacity);
    }
}

pub(super) fn list_action(code: KeyCode) -> Option<ListAction> {
    match code {
        KeyCode::Up => Some(ListAction::Previous),
        KeyCode::Down => Some(ListAction::Next),
        KeyCode::PageUp => Some(ListAction::PagePrevious),
        KeyCode::PageDown => Some(ListAction::PageNext),
        KeyCode::Home => Some(ListAction::Home),
        KeyCode::End => Some(ListAction::End),
        _ => None,
    }
}

fn package_draft(package: cagent_agent::mcp::McpPackage) -> McpPackageDraft {
    let parameters = package
        .parameters
        .iter()
        .filter_map(|(id, parameter)| parameter.default.clone().map(|value| (id.clone(), value)))
        .collect();
    let secret_statuses = package
        .secrets
        .keys()
        .map(|id| (id.clone(), cagent_agent::mcp::McpSecretStatus::Missing))
        .collect();
    McpPackageDraft {
        server_name: package.id.clone(),
        package,
        location: cagent_agent::mcp::McpLocation::global(),
        parameters,
        secret_statuses,
        secret_updates: std::collections::BTreeMap::new(),
        installed: false,
    }
}

fn package_setup_count(draft: &McpPackageDraft) -> usize {
    1 + draft.package.parameters.len() + draft.package.secrets.len()
}

fn package_setup_field(draft: &McpPackageDraft, selected: usize) -> Option<McpPackageField> {
    let mut index = selected.checked_sub(1)?;
    if index < draft.package.secrets.len() {
        return draft
            .package
            .secrets
            .keys()
            .nth(index)
            .cloned()
            .map(McpPackageField::Secret);
    }
    index -= draft.package.secrets.len();
    draft
        .package
        .parameters
        .keys()
        .nth(index)
        .cloned()
        .map(McpPackageField::Parameter)
}

fn package_field_value(draft: &McpPackageDraft, field: &McpPackageField) -> String {
    match field {
        McpPackageField::Parameter(id) => draft
            .parameters
            .get(id)
            .map(ToString::to_string)
            .unwrap_or_default(),
        McpPackageField::Secret(id) => draft
            .secret_updates
            .get(id)
            .and_then(Clone::clone)
            .unwrap_or_default(),
    }
}

fn apply_package_field(
    draft: &mut McpPackageDraft,
    field: &McpPackageField,
    value: String,
) -> Result<(), String> {
    match field {
        McpPackageField::Secret(id) => {
            draft.secret_updates.insert(id.clone(), Some(value));
        }
        McpPackageField::Parameter(id) => {
            let parameter = draft
                .package
                .parameters
                .get(id)
                .ok_or_else(|| "unknown package parameter".to_owned())?;
            let value = match parameter.kind {
                cagent_agent::mcp::McpPackageParameterType::String => {
                    cagent_agent::mcp::McpParameterValue::String(value)
                }
                cagent_agent::mcp::McpPackageParameterType::StringList => {
                    cagent_agent::mcp::McpParameterValue::Strings(
                        value
                            .split(',')
                            .map(str::trim)
                            .filter(|value| !value.is_empty())
                            .map(str::to_owned)
                            .collect(),
                    )
                }
                cagent_agent::mcp::McpPackageParameterType::Integer => {
                    cagent_agent::mcp::McpParameterValue::Integer(
                        value
                            .parse()
                            .map_err(|_| "expected an integer".to_owned())?,
                    )
                }
                cagent_agent::mcp::McpPackageParameterType::Boolean => {
                    return Err("boolean parameters are toggled from the setup menu".into());
                }
            };
            draft.parameters.insert(id.clone(), value);
        }
    }
    Ok(())
}

impl App {
    pub(crate) async fn refresh_open_mcp_server(
        &mut self,
        session: &cagent_agent::runtime::SessionHandle,
    ) {
        let Some(name) = self
            .surfaces
            .iter()
            .rev()
            .find_map(|surface| match surface {
                Surface::McpOAuth { server, .. } => Some(server.clone()),
                Surface::McpServer { server, .. } => Some(server.name.clone()),
                _ => None,
            })
        else {
            return;
        };
        let oauth_connected = session
            .mcp_oauth_status(&name)
            .await
            .is_ok_and(|status| status == cagent_agent::mcp::McpSecretStatus::Configured);
        let Ok(Some(refreshed)) = session.mcp_server(&name).await else {
            return;
        };
        for surface in &mut self.surfaces {
            match surface {
                Surface::McpServer {
                    server,
                    oauth_connected: connected,
                    ..
                } if server.name == name => {
                    server.clone_from(&refreshed);
                    *connected = oauth_connected;
                }
                Surface::McpServers { rows, .. } => {
                    if let Some(server) = rows.iter_mut().find(|server| server.name == name) {
                        server.clone_from(&refreshed);
                    }
                }
                _ => {}
            }
        }
    }

    pub(crate) async fn refresh_loading_mcp_tools(
        &mut self,
        session: &cagent_agent::runtime::SessionHandle,
    ) {
        let Some(Surface::McpTools {
            server,
            loading: true,
            ..
        }) = self.surfaces.last()
        else {
            return;
        };
        let name = server.clone();
        let Ok(server) = session.mcp_server(&name).await else {
            return;
        };
        let Some(server) = server else {
            return;
        };
        if matches!(
            server.status,
            cagent_agent::mcp::McpRuntimeStatus::Starting
                | cagent_agent::mcp::McpRuntimeStatus::NotStarted
        ) {
            return;
        }
        if let Some(Surface::McpTools {
            tools,
            list,
            loading,
            failure,
            ..
        }) = self.surfaces.last_mut()
        {
            *tools = server.tools;
            list.reconcile(ListMode::Selectable, tools.len(), VISIBLE_MENU_ITEMS);
            *loading = false;
            *failure = match server.status {
                cagent_agent::mcp::McpRuntimeStatus::Failed { message } => Some(message),
                cagent_agent::mcp::McpRuntimeStatus::AuthenticationRequired => {
                    Some("Login required · choose Login with OAuth in the server menu".into())
                }
                _ => None,
            };
        }
    }

    pub(super) fn open_background_surface(&mut self) {
        let rows = self.supervised_work.rows();
        let count = rows.iter().filter(|item| item.active()).count();
        self.surfaces.push(Surface::SupervisedWork {
            rows,
            list: ListState::selectable(count),
            show_past: false,
        });
    }

    pub(super) fn insert_surface_paste(&mut self, value: &str) -> bool {
        let width = self.render_width;
        let (_, _, composer_height, _) = self.control_heights_within(width, self.render_height);
        let multiline_capacity =
            super::surfaces::multiline_content_capacity(usize::from(composer_height));
        let Some(surface) = self.surfaces.last_mut() else {
            return false;
        };
        if surface.is_editing_interaction_note() {
            let Some(mut note) = surface.interaction_note_mut() else {
                return false;
            };
            note.insert_text(value, width);
            return true;
        }
        match surface {
            Surface::PermissionRuleEdit {
                pattern, cursor, ..
            }
            | Surface::Rename {
                title: pattern,
                cursor,
            }
            | Surface::WebSearchPicker {
                query: pattern,
                query_cursor: cursor,
                ..
            }
            | Surface::WebSearchSetup {
                value: pattern,
                cursor,
                ..
            }
            | Surface::McpFieldEdit {
                value: pattern,
                cursor,
                ..
            }
            | Surface::McpPackageValueEdit {
                value: pattern,
                cursor,
                ..
            }
            | Surface::Providers {
                query: pattern,
                query_cursor: cursor,
                ..
            }
            | Surface::Models {
                query: pattern,
                query_cursor: cursor,
                ..
            }
            | Surface::HistoryTree {
                query: pattern,
                query_cursor: cursor,
                ..
            }
            | Surface::Conversations {
                query: pattern,
                query_cursor: cursor,
                ..
            } => {
                paste_single_line(pattern, cursor, value);
                true
            }
            Surface::Settings {
                rows,
                section,
                list,
                query,
                query_cursor,
            } => {
                paste_single_line(query, query_cursor, value);
                list.reset(
                    ListMode::Selectable,
                    filter_settings_rows(rows, *section, query).len(),
                );
                true
            }
            Surface::WorktreeNew {
                name,
                base,
                cursor,
                editing_base,
            } => {
                paste_single_line(if *editing_base { base } else { name }, cursor, value);
                true
            }
            Surface::McpJsonEdit { editor, .. } => {
                editor.insert_text_in_view(value, width, multiline_capacity);
                true
            }
            Surface::ProviderSetup {
                api_key_auth: true,
                api_key,
                api_key_cursor,
                ..
            } => {
                paste_single_line(api_key, api_key_cursor, value);
                true
            }
            Surface::AgentWizard {
                name,
                description,
                prompt,
                step,
                cursor,
                ..
            } => match step {
                AgentWizardStep::Name => {
                    paste_single_line(name, cursor, value);
                    true
                }
                AgentWizardStep::Description => {
                    paste_single_line(description, cursor, value);
                    true
                }
                AgentWizardStep::Prompt => {
                    prompt.insert_text_in_view(value, width, multiline_capacity);
                    true
                }
                AgentWizardStep::Parent | AgentWizardStep::Availability => false,
            },
            Surface::SettingInput {
                setting,
                value: source,
                cursor,
            } => {
                let pasted = if matches!(
                    &setting.kind,
                    cagent_agent::config::SettingKind::PositiveInteger
                        | cagent_agent::config::SettingKind::NonNegativeInteger
                ) {
                    value.chars().filter(char::is_ascii_digit).collect()
                } else {
                    value.to_owned()
                };
                paste_single_line(source, cursor, &pasted);
                true
            }
            Surface::StatusLine {
                mode: StatusLineEditorMode::Hex { input, cursor, .. },
                ..
            } => {
                paste_single_line(input, cursor, value);
                true
            }
            _ => false,
        }
    }

    pub(super) fn open_agent_surface(
        &mut self,
        session: &SessionHandle,
    ) -> Result<(), cagent_agent::runtime::RuntimeError> {
        if self.is_observer() {
            return Ok(());
        }
        let mut rows: Vec<_> = session
            .manageable_agent_profiles()?
            .into_iter()
            .map(|profile| {
                let availability = match profile.availability {
                    cagent_agent::config::AgentAvailability::User => "user",
                    cagent_agent::config::AgentAvailability::Subagent => "delegate",
                    cagent_agent::config::AgentAvailability::Both => "both",
                };
                let detail = if profile.description.is_empty() {
                    availability.into()
                } else {
                    format!("{availability} · {}", profile.description)
                };
                let detail = if profile.enabled {
                    detail
                } else {
                    format!("disabled · {detail}")
                };
                (
                    profile.name,
                    detail,
                    profile.enabled && profile.availability.user_selectable(),
                )
            })
            .collect();
        rows.sort_by_key(|(_, _, selectable)| !selectable);
        rows.push(("Add".into(), "create an agent profile".into(), true));
        let item_count = rows.len();
        self.surfaces.push(Surface::Profiles {
            kind: ProfileKind::Agent,
            rows,
            list: ListState::selectable(item_count),
        });
        Ok(())
    }

    pub(super) async fn complete_input(
        &mut self,
        session: &SessionHandle,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        if self.is_observer() {
            return Ok(true);
        }
        if let Some(completion) = &self.attachment_completion
            && !completion.rows.is_empty()
        {
            self.accept_attachment_completion();
            return Ok(true);
        }
        if let Some(command) = self.selected_slash_command() {
            self.accept_slash_command();
            let _ = command;
            return Ok(true);
        }
        let token_start = self.draft[..self.cursor]
            .rfind(char::is_whitespace)
            .map_or(0, |index| index + 1);
        let token = &self.draft[token_start..self.cursor];
        if let Some(prefix) = token
            .strip_prefix('@')
            .filter(|prefix| !prefix.starts_with('@') && !prefix.starts_with('{'))
        {
            let rows = session.complete_paths(prefix).await?;
            if let [entry] = rows.as_slice() {
                self.apply_path_completion(token_start, entry);
                return Ok(true);
            } else if !rows.is_empty() {
                self.surfaces.push(Surface::Paths {
                    list: ListState::selectable(rows.len()),
                    rows,
                    token_start,
                });
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(super) async fn open_provider_surface(&mut self, session: &SessionHandle) {
        self.open_provider_surface_with_model_configuration_follows(
            session,
            self.provider_model.is_empty(),
        )
        .await;
    }

    async fn open_provider_surface_with_model_configuration_follows(
        &mut self,
        session: &SessionHandle,
        model_configuration_follows: bool,
    ) {
        let rows = project_provider_picker(session.providers().await);
        let item_count = rows.len() + 1;
        self.surfaces.push(Surface::Providers {
            rows,
            list: ListState::selectable(item_count),
            query: String::new(),
            query_cursor: 0,
            model_configuration_follows,
        });
    }

    pub(super) async fn refresh_provider_auth_surfaces(&mut self, session: &SessionHandle) {
        let providers = session.providers().await;
        self.apply_provider_auth_update(providers);
    }

    pub(super) fn apply_provider_auth_update(
        &mut self,
        providers: Vec<cagent_agent::provider::ProviderAvailability>,
    ) {
        let rows = project_provider_picker(providers.clone());
        let mut notice = None;
        for surface in &mut self.surfaces {
            match surface {
                Surface::Providers {
                    rows: current_rows, ..
                } => current_rows.clone_from(&rows),
                Surface::ProviderSetup {
                    provider_id,
                    provider,
                    instructions,
                    auth_challenge,
                    managed_auth: true,
                    authenticated,
                    ..
                } => {
                    let Some(availability) = providers
                        .iter()
                        .find(|availability| availability.descriptor.id == *provider_id)
                    else {
                        continue;
                    };
                    *auth_challenge = None;
                    match &availability.auth {
                        cagent_agent::provider::AuthState::Connected { .. }
                        | cagent_agent::provider::AuthState::Available { .. } => {
                            *authenticated = true;
                            *instructions = format!("{provider} connected.");
                            notice = Some(format!("{provider} connected"));
                        }
                        cagent_agent::provider::AuthState::Error { message } => {
                            *authenticated = false;
                            notice = Some(message.clone());
                        }
                        cagent_agent::provider::AuthState::Expired { detail } => {
                            *authenticated = false;
                            notice = Some(format!("{provider} authentication expired · {detail}"));
                        }
                        cagent_agent::provider::AuthState::Missing => {
                            *authenticated = false;
                        }
                    }
                }
                _ => {}
            }
        }
        if let Some(notice) = notice {
            self.set_notice(notice);
        }
    }

    pub(super) fn open_mode_surface(
        &mut self,
        session: &SessionHandle,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if self.is_observer() {
            return Ok(());
        }
        let rows = session
            .mode_profiles()?
            .into_iter()
            .map(|profile| (profile.name, profile.description, true))
            .collect::<Vec<_>>();
        let item_count = rows.len();
        self.surfaces.push(Surface::Profiles {
            kind: ProfileKind::Mode,
            rows,
            list: ListState::selectable(item_count),
        });
        Ok(())
    }

    pub(super) async fn open_model_surface(
        &mut self,
        session: &SessionHandle,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.open_model_surface_for_target(session, ModelSelectionTarget::Conversation)
            .await
    }

    pub(super) async fn open_model_surface_for_mode(
        &mut self,
        session: &SessionHandle,
        selection_mode: Option<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.open_model_surface_for_target(
            session,
            selection_mode.map_or(
                ModelSelectionTarget::Conversation,
                ModelSelectionTarget::Mode,
            ),
        )
        .await
    }

    async fn open_model_surface_for_target(
        &mut self,
        session: &SessionHandle,
        selection_target: ModelSelectionTarget,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if self.is_observer() {
            return Ok(());
        }
        let rows = self.load_model_rows(session).await?;
        if rows.is_empty() {
            if matches!(&selection_target, ModelSelectionTarget::Setting(_)) {
                self.set_notice("no enabled provider models available");
            } else {
                self.surfaces.push(Surface::ModelsUnavailable);
            }
        } else {
            let selected = match &selection_target {
                ModelSelectionTarget::Setting(setting) => session
                    .settings()?
                    .into_iter()
                    .find(|row| row.definition.key == setting.key)
                    .filter(|row| row.explicit_value.is_some())
                    .map(|row| row.effective_value)
                    .and_then(|selected| {
                        rows.iter()
                            .position(|row| format!("{}/{}", row.provider, row.id) == selected)
                    })
                    .unwrap_or(0),
                ModelSelectionTarget::Conversation | ModelSelectionTarget::Mode(_) => 0,
            };
            self.surfaces.push(Surface::Models {
                list: ListState::selectable_at(rows.len(), selected, VISIBLE_MENU_ITEMS),
                rows,
                query: String::new(),
                query_cursor: 0,
                selection_target,
            });
        }
        Ok(())
    }

    pub(super) fn open_history_surface(
        &mut self,
        _session: &SessionHandle,
        purpose: TreePurpose,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if self.is_observer() {
            return Ok(());
        }
        let rows = Vec::new();
        self.surfaces.push(Surface::HistoryTree {
            list: ListState::selectable(0),
            rows,
            purpose,
            loading: true,
            revision: None,
            query: String::new(),
            query_cursor: 0,
            opened_at_millis: picker_opened_at_millis(),
        });
        self.pending_action = Some(AppAction::LoadHistory(purpose));
        Ok(())
    }

    pub(super) fn open_history_purpose(&self) -> Option<TreePurpose> {
        self.surfaces
            .iter()
            .rev()
            .find_map(|surface| match surface {
                Surface::HistoryTree { purpose, .. } => Some(*purpose),
                _ => None,
            })
    }

    pub(super) fn close_history_surface(&mut self) {
        self.surfaces
            .retain(|surface| !matches!(surface, Surface::HistoryTree { .. }));
    }

    async fn open_history_preview(
        &mut self,
        session: &SessionHandle,
        target: cagent_agent::protocol::NodeId,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let live = session.attach().await?.snapshot;
        let preview = session.history_preview(target).await?;
        let mut display = live.clone();
        display.transcript = preview.transcript;
        display.turn = cagent_agent::protocol::TurnState::Idle;
        display.last_activity_at = None;
        display.queue.clear();
        display.pending_interaction = None;
        display.delegated_live.clear();
        display.supervised_work.clear();
        self.apply_session_snapshot(&display);
        self.history_preview = Some(HistoryPreviewState { latest_live: live });
        self.clear_observer_command();
        Ok(())
    }

    pub(super) fn close_history_preview(&mut self) {
        let Some(preview) = self.history_preview.take() else {
            return;
        };
        self.clear_observer_command();
        self.apply_session_snapshot(&preview.latest_live);
    }

    pub(super) fn apply_history_snapshot(
        &mut self,
        purpose: TreePurpose,
        snapshot: cagent_agent::presentation::HistoryRowsSnapshot,
    ) {
        let Some(index) = self.surfaces.iter().rposition(|surface| {
            matches!(surface, Surface::HistoryTree { purpose: current, .. } if *current == purpose)
        }) else {
            return;
        };
        let selected_id = match &self.surfaces[index] {
            Surface::HistoryTree {
                rows, list, query, ..
            } => {
                let visible = history_picker_indexes(rows, query);
                list.selected
                    .and_then(|position| visible.get(position))
                    .and_then(|source| rows.get(*source))
                    .map(|row| row.id)
            }
            _ => None,
        };
        if let Surface::HistoryTree {
            rows,
            list,
            query,
            loading,
            revision,
            ..
        } = &mut self.surfaces[index]
        {
            let selected = selected_id
                .and_then(|id| snapshot.rows.iter().position(|row| row.id == id))
                .unwrap_or_else(|| tree_initial_selection(&snapshot.rows));
            *list = history_list(&snapshot.rows, query, selected);
            *rows = snapshot.rows;
            *revision = snapshot.revision;
            *loading = false;
        }
    }

    pub(super) async fn refresh_conversation_surface(
        &mut self,
        session: &SessionHandle,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Some((query, selected_id)) = self.surfaces.last().and_then(|surface| {
            let Surface::Conversations {
                rows, list, query, ..
            } = surface
            else {
                return None;
            };
            let visible = conversation_picker_indexes(rows, query);
            let selected_id = list
                .selected
                .and_then(|position| visible.get(position))
                .and_then(|source| rows.get(*source))
                .map(|row| row.id);
            Some((query.clone(), selected_id))
        }) else {
            return Ok(());
        };
        let rows = session
            .query_conversations((!query.is_empty()).then_some(query.clone()))
            .await?;
        let visible = conversation_picker_indexes(&rows, &query);
        let selected = selected_id
            .and_then(|id| visible.iter().position(|source| rows[*source].id == id))
            .unwrap_or_else(|| conversation_picker_initial_selection(&rows, &query));
        if let Some(Surface::Conversations {
            rows: current_rows,
            list,
            ..
        }) = self.surfaces.last_mut()
        {
            *list = ListState::selectable_at(visible.len(), selected, VISIBLE_MENU_ITEMS);
            *current_rows = rows;
        }
        Ok(())
    }

    pub(super) async fn load_model_rows(
        &self,
        session: &SessionHandle,
    ) -> Result<Vec<ModelPickerRow>, Box<dyn std::error::Error>> {
        Ok(session.model_picker_rows().await?)
    }

    async fn reconcile_after_provider_close(
        &mut self,
        session: &SessionHandle,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if self.is_observer() {
            return Ok(());
        }
        let current_provider = cagent_agent::provider::ModelRef::parse(&self.provider_model)
            .ok()
            .map(|model| model.provider);
        if current_provider
            .as_ref()
            .is_some_and(|provider| self.enabled_providers.contains(provider))
        {
            return Ok(());
        }

        if current_provider.is_some() {
            let rows = self.load_model_rows(session).await?;
            if let Some(row) = rows.into_iter().next() {
                let effort = fallback_model_effort(&row.efforts, self.effort.as_deref());
                session
                    .submit(SessionCommand::new(SessionAction::ChangeModelAndEffort {
                        provider: row.provider,
                        model: row.id,
                        effort,
                    }))
                    .await?;
            } else {
                self.provider_model.clear();
                self.effort = None;
            }
        } else if !self.enabled_providers.is_empty() {
            self.open_model_surface(session).await?;
        }
        Ok(())
    }

    async fn start_managed_provider_auth(
        &mut self,
        session: &SessionHandle,
        provider_id: &str,
        provider: &str,
        flow: cagent_agent::provider::AuthFlow,
        launch_browser: bool,
    ) -> Option<cagent_agent::provider::AuthChallenge> {
        match session.begin_provider_auth(provider_id, flow).await {
            Ok(challenge @ cagent_agent::provider::AuthChallenge::Browser { .. }) => {
                let cagent_agent::provider::AuthChallenge::Browser {
                    authorization_url, ..
                } = &challenge
                else {
                    unreachable!()
                };
                if !launch_browser {
                    if copy_to_clipboard(authorization_url) {
                        self.set_notice(format!("copied {provider} authentication URL"));
                    } else {
                        self.set_notice("clipboard unavailable");
                    }
                } else if open_browser(authorization_url) {
                    self.set_notice(format!(
                        "complete {provider} authentication in your browser"
                    ));
                } else {
                    self.set_notice(format!(
                        "open this URL to connect {provider}: {authorization_url}"
                    ));
                }
                Some(challenge)
            }
            Ok(challenge @ cagent_agent::provider::AuthChallenge::Device { .. }) => {
                let cagent_agent::provider::AuthChallenge::Device {
                    verification_url,
                    user_code,
                    device_code,
                    ..
                } = &challenge
                else {
                    unreachable!()
                };
                if launch_browser {
                    let opened = open_browser(verification_url);
                    let copied = copy_to_clipboard(user_code);
                    match (opened, copied) {
                        (true, true) => self.set_notice(format!(
                            "opened {provider} device login · copied code {user_code}"
                        )),
                        (true, false) => self.set_notice(format!(
                            "opened {provider} device login · copy code {user_code}"
                        )),
                        (false, true) => self.set_notice(format!(
                            "copied code {user_code} · open {verification_url}"
                        )),
                        (false, false) => self.set_notice(format!(
                            "open {verification_url} and enter code {user_code}"
                        )),
                    }
                }
                let device_code = device_code.clone();
                let session = session.clone();
                let provider_id = provider_id.to_owned();
                tokio::spawn(
                    async move {
                        let _ = session
                            .complete_provider_auth(
                                &provider_id,
                                cagent_agent::provider::AuthResponse::DeviceCode { device_code },
                            )
                            .await;
                    }
                    .in_current_span(),
                );
                Some(challenge)
            }
            Err(error) => {
                self.set_notice(error.to_string());
                None
            }
        }
    }

    pub(super) async fn select_model_row(
        &mut self,
        session: &SessionHandle,
        row: ModelPickerRow,
        selection_target: ModelSelectionTarget,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let ModelPickerRow {
            provider,
            id: model,
            efforts,
            reasoning_control,
            ..
        } = row;
        if matches!(selection_target, ModelSelectionTarget::Setting(_)) {
            self.apply_model_selection(session, selection_target, provider, model, None)
                .await?;
            return Ok(());
        }
        match effort_picker_flow(&efforts) {
            EffortPickerFlow::Choose => {
                let selected = self
                    .effort
                    .as_deref()
                    .and_then(|current| efforts.iter().position(|effort| effort == current))
                    .unwrap_or(0);
                self.surfaces.push(Surface::Effort {
                    provider,
                    model,
                    list: ListState::selectable_at(efforts.len(), selected, VISIBLE_MENU_ITEMS),
                    rows: efforts,
                    reasoning_control,
                    selection_target,
                });
            }
            EffortPickerFlow::NoneSupported => {
                self.apply_model_selection(session, selection_target, provider, model, None)
                    .await?;
            }
            EffortPickerFlow::Automatic(effort) => {
                self.apply_model_selection(
                    session,
                    selection_target,
                    provider,
                    model,
                    Some(effort),
                )
                .await?;
            }
        }
        Ok(())
    }

    async fn apply_model_selection(
        &mut self,
        session: &SessionHandle,
        selection_target: ModelSelectionTarget,
        provider: String,
        model: String,
        effort: Option<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if self.is_observer() {
            return Ok(());
        }
        match selection_target {
            ModelSelectionTarget::Mode(mode) => {
                session
                    .set_mode_model(mode, provider.clone(), model.clone(), effort)
                    .await?;
                self.provider_model = format!("{provider}/{model}");
                if matches!(self.surfaces.last(), Some(Surface::Models { .. })) {
                    self.surfaces.pop();
                }
                if let Some(Surface::PlanCompletion {
                    implementation_model,
                    ..
                }) = self.surfaces.last_mut()
                {
                    *implementation_model = self.provider_model.clone();
                }
                return Ok(());
            }
            ModelSelectionTarget::Setting(setting) => {
                let value = format!("{provider}/{model}");
                session.save_setting(&setting.key, &value)?;
                refresh_settings_surface(&mut self.surfaces, session)?;
                return Ok(());
            }
            ModelSelectionTarget::Conversation => {}
        }
        session
            .submit(SessionCommand::new(SessionAction::ChangeModelAndEffort {
                provider,
                model,
                effort,
            }))
            .await?;
        self.surfaces.clear();
        self.complete_onboarding();
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    /// Routes a key to the focused surface and then gives any deferred
    /// interaction a chance to open after that surface closes.
    pub(super) async fn handle_surface_key(
        &mut self,
        session: &SessionHandle,
        key: KeyEvent,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        // Keyboard navigation and edits never arm mouse activation, and any
        // projection or tab change must invalidate the previous click target.
        self.last_picker_click = None;
        let result = self.handle_surface_key_inner(session, key).await;
        self.open_pending_interaction();
        result
    }

    async fn handle_surface_key_inner(
        &mut self,
        session: &SessionHandle,
        key: KeyEvent,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        let direct_navigation = key
            .modifiers
            .is_empty()
            .then(|| list_action(key.code))
            .flatten();
        let directory_navigation = matches!(
            self.surfaces.last(),
            Some(Surface::Expanded {
                view: ExpandedView::Directory { .. },
                ..
            })
        );
        if let Some(
            action @ (ListAction::PagePrevious
            | ListAction::PageNext
            | ListAction::Home
            | ListAction::End),
        ) = direct_navigation
            && !directory_navigation
            && (self.apply_current_surface_scroll_action(
                action.into(),
                self.render_width,
                self.render_height,
            ) || self.apply_current_surface_list_action(action).is_some())
        {
            return Ok(false);
        }
        let multiline_width = self.render_width;
        let (_, _, composer_height, _) =
            self.control_heights_within(self.render_width, self.render_height);
        let multiline_capacity =
            super::surfaces::multiline_content_capacity(usize::from(composer_height));
        let history_capacity =
            super::surfaces::history_picker_item_capacity(usize::from(composer_height));
        let Some(surface) = self.surfaces.pop() else {
            return Ok(false);
        };
        if self.is_observer() && !Self::observer_surface_allowed(&surface) {
            return Ok(false);
        }
        let submit_key =
            self.matches_action(cagent_agent::presentation::KeyBindingAction::Submit, key);
        let insert_newline_key = self.matches_action(
            cagent_agent::presentation::KeyBindingAction::InsertNewline,
            key,
        );
        let cancel_key =
            self.matches_action(cagent_agent::presentation::KeyBindingAction::Cancel, key);
        let editing_interaction_note = surface.is_editing_interaction_note();
        if self.matches_action(
            cagent_agent::presentation::KeyBindingAction::CloseSurface,
            key,
        ) && !editing_interaction_note
        {
            match surface {
                Surface::Question {
                    request,
                    answers,
                    answered,
                    ..
                } => {
                    self.pending_interaction = Some((*request).clone());
                    if let Err(error) = self
                        .respond_to_question(session, request.id, &answers, Some(&answered), true)
                        .await
                    {
                        self.surfaces.push(Surface::Question {
                            request,
                            question_index: 0,
                            option_index: 0,
                            answers,
                            answered,
                            editing_note: false,
                            note_cursor: 0,
                        });
                        self.set_notice(format!("question response failed · {error}"));
                    } else {
                        self.finish_interaction_response("cancel");
                    }
                }
                Surface::PermissionRuleEdit { .. } => {}
                Surface::Permission {
                    request,
                    selected,
                    scope,
                    diff_scroll,
                    denial_note,
                    editing_note,
                    note_cursor,
                } => {
                    let reason = denial_note.trim().to_owned();
                    if let Err(error) = self
                        .respond_to_permission(
                            session,
                            request.id,
                            "deny",
                            (!reason.is_empty()).then_some(reason.as_str()),
                        )
                        .await
                    {
                        self.surfaces.push(Surface::Permission {
                            request,
                            selected,
                            scope,
                            diff_scroll,
                            denial_note,
                            editing_note,
                            note_cursor,
                        });
                        self.set_notice(format!("approval response failed · {error}"));
                    } else {
                        self.finish_interaction_response("deny");
                    }
                }
                Surface::PlanCompletion {
                    request,
                    selected,
                    note,
                    editing_note,
                    note_cursor,
                    implementation_mode_index,
                    implementation_mode_colors,
                    implementation_model,
                    context_percent,
                } => {
                    if let Err(error) = self
                        .respond_to_plan(session, request.id, None, "keep", None)
                        .await
                    {
                        self.surfaces.push(Surface::PlanCompletion {
                            request,
                            selected,
                            note,
                            editing_note,
                            note_cursor,
                            implementation_mode_index,
                            implementation_mode_colors,
                            implementation_model,
                            context_percent,
                        });
                        self.set_notice(format!("plan response failed · {error}"));
                    } else {
                        self.pending_interaction = None;
                    }
                }
                Surface::Onboarding => self.dismiss_onboarding(),
                Surface::AgentEdit { .. } => self.open_agent_surface(session)?,
                Surface::AgentWizard {
                    editing: true,
                    name,
                    ..
                } => self.surfaces.push(Surface::AgentEdit {
                    name,
                    list: agent_edit_list(0),
                }),
                Surface::AgentWizard { .. } => self.open_agent_surface(session)?,
                Surface::SkillWizard {
                    project,
                    scope_list,
                    name,
                    description,
                    content,
                    step,
                } => {
                    let previous = match step {
                        SkillWizardStep::Content => Some(SkillWizardStep::Description),
                        SkillWizardStep::Description => Some(SkillWizardStep::Name),
                        SkillWizardStep::Name => Some(SkillWizardStep::Scope),
                        SkillWizardStep::Scope => None,
                    };
                    if let Some(step) = previous {
                        self.surfaces.push(Surface::SkillWizard {
                            project,
                            scope_list,
                            name,
                            description,
                            content,
                            step,
                        });
                    } else {
                        let rows = session.skills();
                        self.surfaces.push(Surface::Skills {
                            list: ListState::selectable(rows.len() + 2),
                            rows,
                        });
                    }
                }
                Surface::Skills { .. } => {}
                Surface::Providers {
                    model_configuration_follows: false,
                    ..
                } => self.reconcile_after_provider_close(session).await?,
                Surface::Providers {
                    model_configuration_follows: true,
                    ..
                } if self.onboarding => self.dismiss_onboarding(),
                Surface::Models { .. } | Surface::ModelsUnavailable | Surface::Effort { .. }
                    if self.onboarding =>
                {
                    self.dismiss_onboarding()
                }
                Surface::ProviderSetup {
                    provider_id,
                    managed_auth: true,
                    authenticated: false,
                    ..
                } => {
                    let _ = session.cancel_provider_auth(&provider_id).await;
                }
                Surface::McpOAuth { .. } => {
                    self.refresh_open_mcp_server(session).await;
                }
                Surface::StatusLine {
                    config,
                    rows,
                    list,
                    mode: StatusLineEditorMode::Colors { .. } | StatusLineEditorMode::Hex { .. },
                    preview,
                } => self.surfaces.push(Surface::StatusLine {
                    config,
                    rows,
                    list,
                    mode: StatusLineEditorMode::Modules,
                    preview,
                }),
                Surface::StatusLine {
                    config,
                    rows,
                    list,
                    mode: StatusLineEditorMode::Modules,
                    preview,
                } => match session.save_status_line_config(&config) {
                    Ok(()) => {
                        self.status_line_config = config;
                    }
                    Err(error) => {
                        self.surfaces.push(Surface::StatusLine {
                            config,
                            rows,
                            list,
                            mode: StatusLineEditorMode::Modules,
                            preview,
                        });
                        self.set_notice(format!("statusline could not be saved · {error}"));
                    }
                },
                _ => {}
            }
            return Ok(false);
        }
        if submit_key
            && matches!(&surface, Surface::Expanded { view, .. }
                if !matches!(view, ExpandedView::Directory { .. }))
        {
            return Ok(false);
        }
        match surface {
            Surface::Worktrees { rows, mut list } => {
                list.reconcile(ListMode::Selectable, rows.len() + 1, VISIBLE_MENU_ITEMS);
                let code = self.menu_navigation_code(key);
                if let Some(action) = list_action(code) {
                    list.apply(action, ListMode::Selectable, VISIBLE_MENU_ITEMS);
                    self.surfaces.push(Surface::Worktrees { rows, list });
                } else if code == KeyCode::Enter {
                    match list.selected.unwrap_or(0) {
                        0 => self.surfaces.push(Surface::WorktreeNew {
                            name: String::new(),
                            base: String::new(),
                            cursor: 0,
                            editing_base: false,
                        }),
                        selected => {
                            if let Some(row) = rows.get(selected - 1) {
                                self.pending_action =
                                    Some(AppAction::SwitchWorkspace(row.path.clone()));
                                self.surfaces.clear();
                            }
                        }
                    }
                } else {
                    self.surfaces.push(Surface::Worktrees { rows, list });
                }
            }
            Surface::WorktreeNew {
                mut name,
                mut base,
                mut cursor,
                mut editing_base,
            } => {
                if matches!(key.code, KeyCode::Tab | KeyCode::BackTab)
                    || (key.code == KeyCode::Enter && !editing_base)
                {
                    editing_base = !editing_base;
                    cursor = if editing_base { base.len() } else { name.len() };
                } else if key.code == KeyCode::Enter {
                    if name.trim().is_empty() {
                        self.set_notice("worktree name is required");
                    } else {
                        let base = (!base.trim().is_empty()).then_some(base.trim());
                        match session.resolve_or_create_worktree(name.trim(), base) {
                            Ok(path) => {
                                self.pending_action = Some(AppAction::SwitchWorkspace(path));
                                self.surfaces.clear();
                                return Ok(false);
                            }
                            Err(error) => self.set_notice(format!("worktree failed · {error}")),
                        }
                    }
                } else {
                    let mut input = SingleLineInput::new(
                        if editing_base {
                            base.clone()
                        } else {
                            name.clone()
                        },
                        cursor,
                    );
                    let _ = input.handle_key(key);
                    let (value, next_cursor) = input.into_parts();
                    if editing_base {
                        base = value;
                    } else {
                        name = value;
                    }
                    cursor = next_cursor;
                }
                self.surfaces.push(Surface::WorktreeNew {
                    name,
                    base,
                    cursor,
                    editing_base,
                });
            }
            Surface::Onboarding => {
                if submit_key {
                    self.open_provider_surface_with_model_configuration_follows(session, true)
                        .await;
                } else {
                    self.surfaces.push(Surface::Onboarding);
                }
            }
            Surface::Question {
                request,
                mut question_index,
                mut option_index,
                mut answers,
                mut answered,
                mut editing_note,
                mut note_cursor,
            } => {
                let InteractionRequestKind::Question { questions } = &request.kind else {
                    return Ok(false);
                };
                if editing_note {
                    let outcome = InteractionNote::optional(
                        &mut answers[question_index].note,
                        &mut editing_note,
                        &mut note_cursor,
                    )
                    .handle_key(
                        key,
                        multiline_width,
                        submit_key,
                        insert_newline_key,
                        cancel_key,
                        is_escape_key(key),
                    );
                    if outcome == InteractionNoteOutcome::Submit {
                        if let Some(question) = questions.get(question_index) {
                            answers[question_index].selection = question
                                .options
                                .get(option_index)
                                .map(|option| option.label.clone());
                            answered[question_index] = true;
                            if question_index + 1 == questions.len() {
                                editing_note = false;
                                self.pending_interaction = Some((*request).clone());
                                if let Err(error) = self
                                    .respond_to_question(
                                        session,
                                        request.id,
                                        &answers,
                                        Some(&answered),
                                        false,
                                    )
                                    .await
                                {
                                    self.surfaces.push(Surface::Question {
                                        request,
                                        question_index,
                                        option_index,
                                        answers,
                                        answered,
                                        editing_note,
                                        note_cursor,
                                    });
                                    self.set_notice(format!("question response failed · {error}"));
                                } else {
                                    self.finish_interaction_response("answered");
                                }
                                return Ok(false);
                            }
                            question_index += 1;
                            option_index = question_option_index(
                                question_index,
                                questions,
                                &answers,
                                &answered,
                            );
                        }
                        editing_note = false;
                    }
                    self.surfaces.push(Surface::Question {
                        request,
                        question_index,
                        option_index,
                        answers,
                        answered,
                        editing_note,
                        note_cursor,
                    });
                    return Ok(false);
                }
                match self.menu_navigation_code(key) {
                    KeyCode::Left => {
                        question_index = question_index.saturating_sub(1);
                        option_index =
                            question_option_index(question_index, questions, &answers, &answered);
                    }
                    KeyCode::Right => {
                        question_index = (question_index + 1).min(questions.len() - 1);
                        option_index =
                            question_option_index(question_index, questions, &answers, &answered);
                    }
                    KeyCode::Up => {
                        if let Some(question) = questions.get(question_index) {
                            option_index =
                                option_index.saturating_sub(1).min(question.options.len());
                        }
                    }
                    KeyCode::Down => {
                        if let Some(question) = questions.get(question_index) {
                            option_index = (option_index + 1).min(question.options.len());
                        }
                    }
                    KeyCode::Tab if question_index < questions.len() => {
                        InteractionNote::optional(
                            &mut answers[question_index].note,
                            &mut editing_note,
                            &mut note_cursor,
                        )
                        .open_at_end();
                    }
                    KeyCode::Enter => {
                        if let Some(question) = questions.get(question_index) {
                            answers[question_index].selection = question
                                .options
                                .get(option_index)
                                .map(|option| option.label.clone());
                            answered[question_index] = true;
                            if question_index + 1 == questions.len() {
                                self.pending_interaction = Some((*request).clone());
                                if let Err(error) = self
                                    .respond_to_question(
                                        session,
                                        request.id,
                                        &answers,
                                        Some(&answered),
                                        false,
                                    )
                                    .await
                                {
                                    self.surfaces.push(Surface::Question {
                                        request,
                                        question_index,
                                        option_index,
                                        answers,
                                        answered,
                                        editing_note,
                                        note_cursor,
                                    });
                                    self.set_notice(format!("question response failed · {error}"));
                                } else {
                                    self.finish_interaction_response("answered");
                                }
                                return Ok(false);
                            }
                            question_index += 1;
                            option_index = question_option_index(
                                question_index,
                                questions,
                                &answers,
                                &answered,
                            );
                        }
                    }
                    _ => {}
                }
                self.surfaces.push(Surface::Question {
                    request,
                    question_index,
                    option_index,
                    answers,
                    answered,
                    editing_note,
                    note_cursor,
                });
            }
            Surface::Permission {
                request,
                mut selected,
                mut scope,
                mut diff_scroll,
                mut denial_note,
                mut editing_note,
                mut note_cursor,
            } => {
                if editing_note {
                    let outcome = InteractionNote::required(
                        &mut denial_note,
                        &mut editing_note,
                        &mut note_cursor,
                    )
                    .handle_key(
                        key,
                        multiline_width,
                        submit_key,
                        insert_newline_key,
                        cancel_key,
                        is_escape_key(key),
                    );
                    if outcome == InteractionNoteOutcome::Submit {
                        let reason = denial_note.trim().to_owned();
                        if let Err(error) = self
                            .respond_to_permission(
                                session,
                                request.id,
                                "deny",
                                (!reason.is_empty()).then_some(reason.as_str()),
                            )
                            .await
                        {
                            self.surfaces.push(Surface::Permission {
                                request,
                                selected,
                                scope,
                                diff_scroll,
                                denial_note,
                                editing_note,
                                note_cursor,
                            });
                            self.set_notice(format!("approval response failed · {error}"));
                        } else {
                            self.finish_interaction_response("deny");
                        }
                        return Ok(false);
                    }
                    self.surfaces.push(Surface::Permission {
                        request,
                        selected,
                        scope,
                        diff_scroll,
                        denial_note,
                        editing_note,
                        note_cursor,
                    });
                    return Ok(false);
                }
                let (_, _, composer_height, _) =
                    self.control_heights_within(self.render_width, self.render_height);
                let viewport_rows = usize::from(composer_height.saturating_sub(1));
                let max_diff_scroll = cached_permission_diff_max_scroll_for_request(
                    &self.permission_diff_render_cache,
                    &request,
                    scope,
                    &denial_note,
                    editing_note,
                    note_cursor,
                    &self.workspace,
                    self.render_width,
                    viewport_rows,
                );
                match key.code {
                    KeyCode::PageUp => {
                        diff_scroll = diff_scroll.saturating_sub(viewport_rows.max(1));
                    }
                    KeyCode::PageDown => {
                        diff_scroll = diff_scroll
                            .saturating_add(viewport_rows.max(1))
                            .min(max_diff_scroll);
                    }
                    KeyCode::Home => diff_scroll = 0,
                    KeyCode::End => diff_scroll = max_diff_scroll,
                    _ => match self.menu_navigation_code(key) {
                        KeyCode::Up => {
                            selected = selected.saturating_sub(1);
                        }
                        KeyCode::Down => {
                            selected = (selected + 1).min(
                                permission_approval_choices(&request, scope)
                                    .len()
                                    .saturating_sub(1),
                            );
                        }
                        KeyCode::Enter => {
                            let choice = permission_submission_choice(
                                &request,
                                selected,
                                scope,
                                key.modifiers.contains(KeyModifiers::SHIFT),
                            )
                            .expect("selected permission choice");
                            let rule = if choice.decision == "allow_session" {
                                choice.session_rule.as_ref()
                            } else {
                                choice.prepared_rule.as_ref()
                            };
                            let response = if let Some(rule) = rule {
                                self.respond_to_permission_rule(
                                    session,
                                    request.id,
                                    choice.decision,
                                    rule,
                                )
                                .await
                            } else {
                                let reason = denial_note.trim().to_owned();
                                self.respond_to_permission(
                                    session,
                                    request.id,
                                    choice.decision,
                                    (choice.decision == "deny" && !reason.is_empty())
                                        .then_some(reason.as_str()),
                                )
                                .await
                            };
                            if let Err(error) = response {
                                self.surfaces.push(Surface::Permission {
                                    request,
                                    selected,
                                    scope,
                                    diff_scroll,
                                    denial_note,
                                    editing_note,
                                    note_cursor,
                                });
                                self.set_notice(format!("approval response failed · {error}"));
                            } else {
                                if choice.decision != "deny" {
                                    self.approve_permission_request(&request);
                                }
                                self.finish_interaction_response(choice.decision);
                            }
                            return Ok(false);
                        }
                        KeyCode::Tab => {
                            let choices = permission_approval_choices(&request, scope);
                            if choices
                                .get(selected)
                                .is_some_and(|choice| choice.decision == "deny")
                            {
                                InteractionNote::required(
                                    &mut denial_note,
                                    &mut editing_note,
                                    &mut note_cursor,
                                )
                                .open_at_end();
                            } else if choices.get(selected).is_some_and(
                                cagent_agent::presentation::PermissionApprovalChoice::is_persistent,
                            ) {
                                scope = match scope {
                                    cagent_agent::permissions::PermissionScope::Conversation => {
                                        cagent_agent::permissions::PermissionScope::ConversationGlobal
                                    }
                                    cagent_agent::permissions::PermissionScope::ConversationGlobal => {
                                        cagent_agent::permissions::PermissionScope::Conversation
                                    }
                                    cagent_agent::permissions::PermissionScope::Project => {
                                        cagent_agent::permissions::PermissionScope::Global
                                    }
                                    cagent_agent::permissions::PermissionScope::Global => {
                                        cagent_agent::permissions::PermissionScope::Project
                                    }
                                };
                            } else if choices.get(selected).is_some_and(
                                cagent_agent::presentation::PermissionApprovalChoice::supports_session,
                            ) {
                                scope = match scope {
                                    cagent_agent::permissions::PermissionScope::Conversation => {
                                        cagent_agent::permissions::PermissionScope::Project
                                    }
                                    cagent_agent::permissions::PermissionScope::ConversationGlobal => {
                                        cagent_agent::permissions::PermissionScope::Global
                                    }
                                    cagent_agent::permissions::PermissionScope::Global => {
                                        cagent_agent::permissions::PermissionScope::ConversationGlobal
                                    }
                                    cagent_agent::permissions::PermissionScope::Project => {
                                        cagent_agent::permissions::PermissionScope::Conversation
                                    }
                                };
                            }
                        }
                        _ => {}
                    },
                }
                if matches!(key.code, KeyCode::Char('e' | 'E'))
                    && let Some((rule, cursor, edit_scope)) = permission_rule_edit_details(
                        &request,
                        selected,
                        if key.modifiers.contains(KeyModifiers::SHIFT) {
                            cagent_agent::permissions::PermissionScope::Global
                        } else {
                            scope
                        },
                    )
                {
                    let pattern = rule
                        .editable_pattern()
                        .map_or_else(String::new, |(pattern, _)| pattern);
                    self.surfaces.push(Surface::Permission {
                        request: request.clone(),
                        selected,
                        scope,
                        diff_scroll,
                        denial_note,
                        editing_note,
                        note_cursor,
                    });
                    self.surfaces.push(Surface::PermissionRuleEdit {
                        request,
                        rule,
                        pattern,
                        cursor,
                        scope: edit_scope,
                    });
                    return Ok(false);
                }
                self.surfaces.push(Surface::Permission {
                    request,
                    selected,
                    scope,
                    diff_scroll,
                    denial_note,
                    editing_note,
                    note_cursor,
                });
            }
            Surface::PermissionRuleEdit {
                request,
                mut rule,
                mut pattern,
                mut cursor,
                scope,
            } => {
                if submit_key {
                    rule.set_editable_pattern(pattern.clone());
                    let decision = match scope {
                        cagent_agent::permissions::PermissionScope::Conversation => "allow_session",
                        cagent_agent::permissions::PermissionScope::ConversationGlobal => {
                            "allow_global"
                        }
                        cagent_agent::permissions::PermissionScope::Project => "allow_project",
                        cagent_agent::permissions::PermissionScope::Global => "allow_global",
                    };
                    if let Err(error) = self
                        .respond_to_permission_rule(session, request.id, decision, &rule)
                        .await
                    {
                        self.surfaces.push(Surface::PermissionRuleEdit {
                            request,
                            rule,
                            pattern,
                            cursor,
                            scope,
                        });
                        self.set_notice(format!("approval response failed · {error}"));
                    } else {
                        let _ = self.surfaces.pop();
                        self.approve_permission_request(&request);
                        self.finish_interaction_response(decision);
                        self.set_notice(match scope {
                            cagent_agent::permissions::PermissionScope::Conversation => {
                                "edited permission applied for this conversation"
                            }
                            cagent_agent::permissions::PermissionScope::ConversationGlobal => {
                                "edited permission saved globally"
                            }
                            cagent_agent::permissions::PermissionScope::Project => {
                                "edited permission saved for this project"
                            }
                            cagent_agent::permissions::PermissionScope::Global => {
                                "edited permission saved globally"
                            }
                        });
                    }
                    return Ok(false);
                }
                let mut input = SingleLineInput::new(pattern, cursor);
                let _ = input.handle_key(key);
                (pattern, cursor) = input.into_parts();
                self.surfaces.push(Surface::PermissionRuleEdit {
                    request,
                    rule,
                    pattern,
                    cursor,
                    scope,
                });
            }
            Surface::Permissions { rows, mut list } => {
                list.reconcile(ListMode::Selectable, rows.len() + 1, VISIBLE_MENU_ITEMS);
                if let Some(action) = list_action(self.menu_navigation_code(key)) {
                    list.apply(action, ListMode::Selectable, VISIBLE_MENU_ITEMS);
                    self.surfaces.push(Surface::Permissions { rows, list });
                    return Ok(false);
                }
                let selected = list.selected.unwrap_or(0);
                if submit_key && selected == 0 {
                    return Ok(false);
                }
                if matches!(key.code, KeyCode::Char('e')) && selected > 0 {
                    let row = rows[selected - 1].clone();
                    if let Some((pattern, _)) = row.rule.editable_pattern() {
                        let cursor = cursor_at_end(&pattern);
                        self.surfaces.push(Surface::Permissions { rows, list });
                        self.surfaces.push(Surface::PersistentPermissionEdit {
                            rule: row.rule,
                            pattern,
                            cursor,
                            scope: row.scope,
                            selected,
                        });
                    } else {
                        self.surfaces.push(Surface::Permissions { rows, list });
                        self.set_notice("this permission has no editable path or command matcher");
                    }
                    return Ok(false);
                }
                if matches!(key.code, KeyCode::Char('d')) && selected > 0 {
                    let row = &rows[selected - 1];
                    match session.delete_persistent_permission(row.scope, &row.rule.id) {
                        Ok(()) => {
                            self.open_permissions_surface(session)?;
                            if let Some(Surface::Permissions { list, rows }) =
                                self.surfaces.last_mut()
                            {
                                list.select(selected.min(rows.len()), VISIBLE_MENU_ITEMS);
                            }
                            self.set_notice("permission deleted");
                        }
                        Err(error) => {
                            self.surfaces.push(Surface::Permissions { rows, list });
                            self.set_notice(format!("permission delete failed · {error}"));
                        }
                    }
                    return Ok(false);
                }
                self.surfaces.push(Surface::Permissions { rows, list });
            }
            Surface::Usage { overview, mut list } => {
                list.reconcile(ListMode::Selectable, 4, VISIBLE_MENU_ITEMS);
                if let Some(action) = list_action(self.menu_navigation_code(key)) {
                    list.apply(action, ListMode::Selectable, VISIBLE_MENU_ITEMS);
                    self.surfaces.push(Surface::Usage { overview, list });
                    return Ok(false);
                }
                if submit_key {
                    match list.selected.unwrap_or(0) {
                        0 => return Ok(false),
                        1 => {
                            self.surfaces.push(Surface::Usage { overview, list });
                            self.surfaces
                                .push(Surface::UsageResetConfirm { selected: 0 });
                            return Ok(false);
                        }
                        selected => {
                            let kind = if selected == 2 {
                                UsageBreakdownKind::Project
                            } else {
                                UsageBreakdownKind::Model
                            };
                            let result = match kind {
                                UsageBreakdownKind::Project => session.usage_by_project().await,
                                UsageBreakdownKind::Model => session.usage_by_model().await,
                            };
                            self.surfaces.push(Surface::Usage { overview, list });
                            match result {
                                Ok(rows) => self.surfaces.push(Surface::UsageBreakdown {
                                    kind,
                                    list: ListState::selectable(rows.len()),
                                    rows,
                                }),
                                Err(error) => self
                                    .set_notice(format!("usage breakdown unavailable · {error}")),
                            }
                            return Ok(false);
                        }
                    }
                }
                self.surfaces.push(Surface::Usage { overview, list });
            }
            Surface::UsageBreakdown {
                kind,
                rows,
                mut list,
            } => {
                list.reconcile(ListMode::Selectable, rows.len(), VISIBLE_MENU_ITEMS);
                if let Some(action) = list_action(self.menu_navigation_code(key)) {
                    list.apply(action, ListMode::Selectable, VISIBLE_MENU_ITEMS);
                }
                self.surfaces
                    .push(Surface::UsageBreakdown { kind, rows, list });
            }
            Surface::UsageResetConfirm { mut selected } => {
                match self.menu_navigation_code(key) {
                    KeyCode::Up | KeyCode::PageUp | KeyCode::Home => selected = 0,
                    KeyCode::Down | KeyCode::PageDown | KeyCode::End => selected = 1,
                    KeyCode::Enter if selected == 0 => return Ok(false),
                    KeyCode::Enter => {
                        session.reset_global_usage().await?;
                        if matches!(self.surfaces.last(), Some(Surface::Usage { .. })) {
                            self.surfaces.pop();
                        }
                        let overview = session.usage_overview().await?;
                        self.surfaces.push(Surface::Usage {
                            overview,
                            list: ListState::selectable(4),
                        });
                        self.set_notice("global usage reset");
                        return Ok(false);
                    }
                    _ => {}
                }
                self.surfaces.push(Surface::UsageResetConfirm { selected });
            }
            Surface::PersistentPermissionEdit {
                mut rule,
                mut pattern,
                mut cursor,
                scope,
                selected,
            } => {
                if submit_key {
                    rule.set_editable_pattern(pattern.clone());
                    match session.update_persistent_permission(
                        scope,
                        &rule.id.clone(),
                        rule.clone(),
                    ) {
                        Ok(_) => {
                            let _ = self.surfaces.pop();
                            self.open_permissions_surface(session)?;
                            if let Some(Surface::Permissions { list, rows }) =
                                self.surfaces.last_mut()
                            {
                                list.select(selected.min(rows.len()), VISIBLE_MENU_ITEMS);
                            }
                            self.set_notice("permission updated");
                        }
                        Err(error) => {
                            self.surfaces.push(Surface::PersistentPermissionEdit {
                                rule,
                                pattern,
                                cursor,
                                scope,
                                selected,
                            });
                            self.set_notice(format!("permission update failed · {error}"));
                        }
                    }
                    return Ok(false);
                }
                let mut input = SingleLineInput::new(pattern, cursor);
                let _ = input.handle_key(key);
                (pattern, cursor) = input.into_parts();
                self.surfaces.push(Surface::PersistentPermissionEdit {
                    rule,
                    pattern,
                    cursor,
                    scope,
                    selected,
                });
            }
            Surface::PlanCompletion {
                request,
                mut selected,
                mut note,
                mut editing_note,
                mut note_cursor,
                mut implementation_mode_index,
                implementation_mode_colors,
                mut implementation_model,
                context_percent,
            } => {
                let InteractionRequestKind::PlanCompletion {
                    implementation_modes,
                    ..
                } = &request.kind
                else {
                    return Ok(false);
                };
                if editing_note {
                    let outcome =
                        InteractionNote::required(&mut note, &mut editing_note, &mut note_cursor)
                            .handle_key(
                                key,
                                multiline_width,
                                submit_key,
                                insert_newline_key,
                                cancel_key,
                                is_escape_key(key),
                            );
                    if outcome == InteractionNoteOutcome::Submit {
                        let mode = implementation_modes[implementation_mode_index].as_str();
                        let normalized = note.trim().to_owned();
                        if let Err(error) = self
                            .respond_to_plan(
                                session,
                                request.id,
                                Some(mode),
                                match selected {
                                    1 => "clear",
                                    2 => "compact",
                                    _ => "keep",
                                },
                                (!normalized.is_empty()).then_some(normalized.as_str()),
                            )
                            .await
                        {
                            self.surfaces.push(Surface::PlanCompletion {
                                request,
                                selected,
                                note,
                                editing_note,
                                note_cursor,
                                implementation_mode_index,
                                implementation_mode_colors,
                                implementation_model,
                                context_percent,
                            });
                            self.set_notice(format!("plan response failed · {error}"));
                        } else {
                            self.pending_interaction = None;
                            self.follow_history_tail = true;
                            self.plan_transition_pending = true;
                            self.active = true;
                            self.working_started_at.get_or_insert_with(Instant::now);
                        }
                        return Ok(false);
                    }
                    self.surfaces.push(Surface::PlanCompletion {
                        request,
                        selected,
                        note,
                        editing_note,
                        note_cursor,
                        implementation_mode_index,
                        implementation_mode_colors,
                        implementation_model,
                        context_percent,
                    });
                    return Ok(false);
                }
                let previous_mode = self.matches_action(
                    cagent_agent::presentation::KeyBindingAction::PreviousMode,
                    key,
                );
                let next_mode = self
                    .matches_action(cagent_agent::presentation::KeyBindingAction::NextMode, key);
                if previous_mode || next_mode {
                    if !implementation_modes.is_empty() {
                        implementation_mode_index = implementation_mode_index
                            .min(implementation_modes.len().saturating_sub(1));
                        implementation_mode_index = if previous_mode {
                            implementation_mode_index
                                .checked_sub(1)
                                .unwrap_or(implementation_modes.len().saturating_sub(1))
                        } else {
                            (implementation_mode_index + 1) % implementation_modes.len()
                        };
                    }
                    if !implementation_modes.is_empty()
                        && let Some((provider, model, _)) = session
                            .mode_model_selection(&implementation_modes[implementation_mode_index])
                            .await?
                    {
                        implementation_model = format!("{provider}/{model}");
                    }
                    self.surfaces.push(Surface::PlanCompletion {
                        request,
                        selected,
                        note,
                        editing_note,
                        note_cursor,
                        implementation_mode_index,
                        implementation_mode_colors: implementation_mode_colors.clone(),
                        implementation_model,
                        context_percent,
                    });
                    return Ok(false);
                }
                if self.matches_action(
                    cagent_agent::presentation::KeyBindingAction::ModelPicker,
                    key,
                ) || (key.modifiers.is_empty()
                    && matches!(key.code, KeyCode::Char('p') | KeyCode::Char('m')))
                {
                    let mode = implementation_modes[implementation_mode_index].clone();
                    self.surfaces.push(Surface::PlanCompletion {
                        request,
                        selected,
                        note,
                        editing_note,
                        note_cursor,
                        implementation_mode_index,
                        implementation_mode_colors,
                        implementation_model,
                        context_percent,
                    });
                    self.open_model_surface_for_mode(session, Some(mode))
                        .await?;
                    return Ok(false);
                }
                match self.menu_navigation_code(key) {
                    KeyCode::Up => selected = selected.saturating_sub(1),
                    KeyCode::Down => selected = (selected + 1).min(3),
                    KeyCode::Enter => {
                        let mode = (selected < 3)
                            .then(|| implementation_modes[implementation_mode_index].as_str());
                        let context_action = match selected {
                            1 => "clear",
                            2 => "compact",
                            _ => "keep",
                        };
                        let normalized = note.trim().to_owned();
                        if let Err(error) = self
                            .respond_to_plan(
                                session,
                                request.id,
                                mode,
                                context_action,
                                (mode.is_some() && !normalized.is_empty())
                                    .then_some(normalized.as_str()),
                            )
                            .await
                        {
                            self.surfaces.push(Surface::PlanCompletion {
                                request,
                                selected,
                                note,
                                editing_note,
                                note_cursor,
                                implementation_mode_index,
                                implementation_mode_colors: implementation_mode_colors.clone(),
                                implementation_model,
                                context_percent,
                            });
                            self.set_notice(format!("plan response failed · {error}"));
                        } else {
                            self.pending_interaction = None;
                            if mode.is_some() {
                                // Accepting a plan starts a new live turn, so leave any older
                                // transcript position and reveal the accepted-plan tail.
                                self.follow_history_tail = true;
                                // The old planning turn completes before the implementation turn
                                // starts. Keep the working indicator alive across that boundary;
                                // transient and durable event streams are not globally ordered.
                                self.plan_transition_pending = true;
                                self.active = true;
                                self.working_started_at.get_or_insert_with(Instant::now);
                            }
                        }
                        return Ok(false);
                    }
                    KeyCode::Tab if selected < 3 => {
                        InteractionNote::required(&mut note, &mut editing_note, &mut note_cursor)
                            .open_at_end();
                    }
                    _ => {}
                }
                self.surfaces.push(Surface::PlanCompletion {
                    request,
                    selected,
                    note,
                    editing_note,
                    note_cursor,
                    implementation_mode_index,
                    implementation_mode_colors,
                    implementation_model,
                    context_percent,
                });
            }
            Surface::WebSearchPicker {
                rows,
                mut list,
                query,
                query_cursor,
            } => {
                let mut input = SingleLineInput::new(query, query_cursor);
                let visible = rows
                    .iter()
                    .filter(|row| row.provider.label().contains(&input.text().to_lowercase()))
                    .cloned()
                    .collect::<Vec<_>>();
                list.reconcile(ListMode::Selectable, visible.len() + 1, VISIBLE_MENU_ITEMS);
                let code = self.menu_navigation_code(key);
                if let Some(action) = list_action(code) {
                    list.apply(action, ListMode::Selectable, VISIBLE_MENU_ITEMS);
                } else {
                    match code {
                        KeyCode::Enter if list.selected == Some(0) => return Ok(false),
                        KeyCode::Enter => {
                            if let Some(row) = list
                                .selected
                                .and_then(|selected| selected.checked_sub(1))
                                .and_then(|index| visible.get(index))
                            {
                                if row.ready {
                                    match session.select_web_search_provider(row.provider) {
                                        Ok(()) => {
                                            return Ok(false);
                                        }
                                        Err(error) => self.set_notice(error.to_string()),
                                    }
                                } else if row.provider
                                    == cagent_agent::web_search::WebSearchProvider::Chatgpt
                                {
                                    self.set_notice("connect a ChatGPT subscription in /providers");
                                    self.surfaces.push(Surface::WebSearchPicker {
                                        rows,
                                        list,
                                        query: input.text().into(),
                                        query_cursor: cursor_at_end(input.text()),
                                    });
                                    return Ok(false);
                                }
                                self.surfaces.push(Surface::WebSearchPicker {
                                    rows,
                                    list,
                                    query: input.text().into(),
                                    query_cursor: cursor_at_end(input.text()),
                                });
                                self.surfaces.push(Surface::WebSearchSetup {
                                    provider: row.provider,
                                    value: String::new(),
                                    cursor: 0,
                                });
                                return Ok(false);
                            }
                        }
                        KeyCode::Char('r') => {
                            if let Some(row) = list
                                .selected
                                .and_then(|selected| selected.checked_sub(1))
                                .and_then(|index| visible.get(index))
                            {
                                if row.provider
                                    == cagent_agent::web_search::WebSearchProvider::Chatgpt
                                {
                                    self.set_notice("connect a ChatGPT subscription in /providers");
                                    self.surfaces.push(Surface::WebSearchPicker {
                                        rows,
                                        list,
                                        query: input.text().into(),
                                        query_cursor: cursor_at_end(input.text()),
                                    });
                                    return Ok(false);
                                }
                                self.surfaces.push(Surface::WebSearchPicker {
                                    rows,
                                    list,
                                    query: input.text().into(),
                                    query_cursor: cursor_at_end(input.text()),
                                });
                                self.surfaces.push(Surface::WebSearchSetup {
                                    provider: row.provider,
                                    value: String::new(),
                                    cursor: 0,
                                });
                                return Ok(false);
                            }
                        }
                        KeyCode::Char('d') => {
                            if let Some(row) = list
                                .selected
                                .and_then(|selected| selected.checked_sub(1))
                                .and_then(|index| visible.get(index))
                            {
                                if !row.ready {
                                    self.surfaces.push(Surface::WebSearchPicker {
                                        rows,
                                        list,
                                        query: input.text().into(),
                                        query_cursor: cursor_at_end(input.text()),
                                    });
                                    return Ok(false);
                                }
                                let result = match row.provider {
                                    cagent_agent::web_search::WebSearchProvider::Searxng => {
                                        session.remove_searxng_url()
                                    }
                                    cagent_agent::web_search::WebSearchProvider::Exa => {
                                        session.remove_exa_api_key()
                                    }
                                    cagent_agent::web_search::WebSearchProvider::Chatgpt => Ok(()),
                                };
                                match result {
                                    Ok(()) => {}
                                    Err(error) => self.set_notice(error.to_string()),
                                }
                                let refreshed = session.web_search_providers();
                                let count = refreshed
                                    .iter()
                                    .filter(|row| {
                                        row.provider.label().contains(&input.text().to_lowercase())
                                    })
                                    .count()
                                    + 1;
                                list.reconcile(ListMode::Selectable, count, VISIBLE_MENU_ITEMS);
                                self.surfaces.push(Surface::WebSearchPicker {
                                    rows: refreshed,
                                    list,
                                    query: input.text().into(),
                                    query_cursor: cursor_at_end(input.text()),
                                });
                                return Ok(false);
                            }
                        }
                        _ => {
                            let _ = input.handle_key(key);
                            let count = rows
                                .iter()
                                .filter(|row| {
                                    row.provider.label().contains(&input.text().to_lowercase())
                                })
                                .count()
                                + 1;
                            list.reset(ListMode::Selectable, count);
                        }
                    }
                }
                let (query, query_cursor) = input.into_parts();
                self.surfaces.push(Surface::WebSearchPicker {
                    rows,
                    list,
                    query,
                    query_cursor,
                });
            }
            Surface::WebSearchSetup {
                provider,
                value,
                cursor,
            } => {
                let mut input = SingleLineInput::new(value, cursor);
                if submit_key {
                    let result = match provider {
                        cagent_agent::web_search::WebSearchProvider::Searxng => {
                            session.save_searxng_url(input.text())
                        }
                        cagent_agent::web_search::WebSearchProvider::Exa => {
                            session.save_exa_api_key(input.text())
                        }
                        cagent_agent::web_search::WebSearchProvider::Chatgpt => {
                            self.set_notice("connect ChatGPT from /providers");
                            return Ok(false);
                        }
                    };
                    match result {
                        Ok(()) => {
                            if let Some(Surface::WebSearchPicker { rows, .. }) =
                                self.surfaces.last_mut()
                            {
                                *rows = session.web_search_providers();
                            }
                            return Ok(false);
                        }
                        Err(error) => self.set_notice(error.to_string()),
                    }
                } else {
                    let _ = input.handle_key(key);
                }
                let (value, cursor) = input.into_parts();
                self.surfaces.push(Surface::WebSearchSetup {
                    provider,
                    value,
                    cursor,
                });
            }
            Surface::Rename { title, cursor } => {
                let mut input = SingleLineInput::new(title, cursor);
                if submit_key {
                    let submitted = input.text().to_owned();
                    match session
                        .submit(SessionCommand::new(SessionAction::RenameConversation {
                            title: submitted,
                        }))
                        .await
                    {
                        Ok(_) => {}
                        Err(error) => {
                            let (title, cursor) = input.into_parts();
                            self.surfaces.push(Surface::Rename { title, cursor });
                            self.set_notice(error.to_string());
                        }
                    }
                    return Ok(false);
                }
                let _ = input.handle_key(key);
                let (title, cursor) = input.into_parts();
                self.surfaces.push(Surface::Rename { title, cursor });
            }
            surface @ (Surface::ProvidersRequired | Surface::ModelsUnavailable) => {
                if submit_key {
                    self.open_provider_surface(session).await;
                    return Ok(false);
                }
                self.surfaces.push(surface);
            }
            Surface::ModelRequired => {
                if submit_key {
                    self.open_model_surface(session).await?;
                    return Ok(false);
                }
                self.surfaces.push(Surface::ModelRequired);
            }
            Surface::ProviderSetup {
                provider_id,
                provider,
                instructions,
                credential_environment_variable,
                mut auth_challenge,
                managed_auth,
                api_key_auth,
                mut api_key,
                mut api_key_cursor,
                auth_flows,
                authenticated,
            } => {
                if api_key_auth {
                    if submit_key {
                        match session
                            .set_provider_api_key(&provider_id, api_key.clone())
                            .await
                        {
                            Ok(()) => {
                                let removed = api_key.trim().is_empty();
                                let updated_rows =
                                    project_provider_picker(session.providers().await);
                                if let Some(Surface::Providers { rows, .. }) =
                                    self.surfaces.last_mut()
                                {
                                    *rows = updated_rows;
                                }
                                self.set_notice(if removed {
                                    format!("{provider} API key removed")
                                } else {
                                    format!("{provider} API key saved")
                                });
                            }
                            Err(error) => {
                                self.set_notice(error.to_string());
                                self.surfaces.push(Surface::ProviderSetup {
                                    provider_id,
                                    provider,
                                    instructions,
                                    credential_environment_variable,
                                    auth_challenge,
                                    managed_auth,
                                    api_key_auth,
                                    api_key,
                                    api_key_cursor,
                                    auth_flows,
                                    authenticated,
                                });
                            }
                        }
                    } else if key.code != KeyCode::Esc {
                        let mut input = SingleLineInput::new(api_key, api_key_cursor);
                        let _ = input.handle_key(key);
                        (api_key, api_key_cursor) = input.into_parts();
                        self.surfaces.push(Surface::ProviderSetup {
                            provider_id,
                            provider,
                            instructions,
                            credential_environment_variable,
                            auth_challenge,
                            managed_auth,
                            api_key_auth,
                            api_key,
                            api_key_cursor,
                            auth_flows,
                            authenticated,
                        });
                    }
                    return Ok(false);
                }
                if !managed_auth && submit_key {
                    return Ok(false);
                }
                if authenticated && submit_key {
                    return Ok(false);
                }
                if managed_auth
                    && key.code == KeyCode::Char('C')
                    && let Some(cagent_agent::provider::AuthChallenge::Device { user_code, .. }) =
                        &auth_challenge
                {
                    if copy_to_clipboard(user_code) {
                        self.set_notice(format!("copied {provider} device code"));
                    } else {
                        self.set_notice("clipboard unavailable");
                    }
                    self.surfaces.push(Surface::ProviderSetup {
                        provider_id,
                        provider,
                        instructions,
                        credential_environment_variable,
                        auth_challenge,
                        managed_auth,
                        api_key_auth,
                        api_key,
                        api_key_cursor,
                        auth_flows,
                        authenticated,
                    });
                    return Ok(false);
                }
                if managed_auth && key.code == KeyCode::Char('c') {
                    if auth_challenge.is_none()
                        && auth_flows == [cagent_agent::provider::AuthFlow::DeviceCode]
                    {
                        self.surfaces.push(Surface::ProviderSetup {
                            provider_id,
                            provider,
                            instructions,
                            credential_environment_variable,
                            auth_challenge,
                            managed_auth,
                            api_key_auth,
                            api_key,
                            api_key_cursor,
                            auth_flows,
                            authenticated,
                        });
                        return Ok(false);
                    }
                    let active_url = auth_challenge.as_ref().map(|challenge| match challenge {
                        cagent_agent::provider::AuthChallenge::Browser {
                            authorization_url,
                            ..
                        } => (authorization_url.as_str(), false),
                        cagent_agent::provider::AuthChallenge::Device {
                            verification_url, ..
                        } => (verification_url.as_str(), true),
                    });
                    if let Some((url, device)) = active_url {
                        if copy_to_clipboard(url) {
                            self.set_notice(if device {
                                format!("copied {provider} device login URL")
                            } else {
                                format!("copied {provider} authentication URL")
                            });
                        } else {
                            self.set_notice("clipboard unavailable");
                        }
                    } else {
                        let flow = cagent_agent::provider::preferred_auth_flow(&auth_flows)
                            .unwrap_or(cagent_agent::provider::AuthFlow::DeviceCode);
                        auth_challenge = self
                            .start_managed_provider_auth(
                                session,
                                &provider_id,
                                &provider,
                                flow,
                                false,
                            )
                            .await;
                    }
                    self.surfaces.push(Surface::ProviderSetup {
                        provider_id,
                        provider,
                        instructions,
                        credential_environment_variable,
                        auth_challenge,
                        managed_auth,
                        api_key_auth,
                        api_key,
                        api_key_cursor,
                        auth_flows,
                        authenticated,
                    });
                    return Ok(false);
                }
                if (submit_key || key.code == KeyCode::Char('d')) && managed_auth {
                    if key.code == KeyCode::Char('d')
                        && !auth_flows.contains(&cagent_agent::provider::AuthFlow::DeviceCode)
                    {
                        self.surfaces.push(Surface::ProviderSetup {
                            provider_id,
                            provider,
                            instructions,
                            credential_environment_variable,
                            auth_challenge,
                            managed_auth,
                            api_key_auth,
                            api_key,
                            api_key_cursor,
                            auth_flows,
                            authenticated,
                        });
                        return Ok(false);
                    }
                    if key.code == KeyCode::Char('d') {
                        self.clear_notice();
                    }
                    let flow = if submit_key {
                        cagent_agent::provider::preferred_auth_flow(&auth_flows)
                            .unwrap_or(cagent_agent::provider::AuthFlow::DeviceCode)
                    } else {
                        cagent_agent::provider::AuthFlow::DeviceCode
                    };
                    if submit_key
                        && let Some(cagent_agent::provider::AuthChallenge::Browser {
                            authorization_url,
                            ..
                        }) = &auth_challenge
                    {
                        if open_browser(authorization_url) {
                            self.set_notice(format!(
                                "complete {provider} authentication in your browser"
                            ));
                        }
                    } else if let Some(challenge) = self
                        .start_managed_provider_auth(session, &provider_id, &provider, flow, true)
                        .await
                    {
                        auth_challenge = Some(challenge);
                    }
                    self.surfaces.push(Surface::ProviderSetup {
                        provider_id,
                        provider,
                        instructions,
                        credential_environment_variable,
                        auth_challenge,
                        managed_auth,
                        api_key_auth,
                        api_key,
                        api_key_cursor,
                        auth_flows,
                        authenticated,
                    });
                } else {
                    self.surfaces.push(Surface::ProviderSetup {
                        provider_id,
                        provider,
                        instructions,
                        credential_environment_variable,
                        auth_challenge,
                        managed_auth,
                        api_key_auth,
                        api_key,
                        api_key_cursor,
                        auth_flows,
                        authenticated,
                    });
                }
                return Ok(false);
            }
            Surface::Providers {
                mut rows,
                mut list,
                query,
                query_cursor,
                model_configuration_follows,
            } => {
                let visible = filter_provider_picker_rows(&rows, &query);
                list.reconcile(ListMode::Selectable, visible.len() + 1, VISIBLE_MENU_ITEMS);
                let mut selected = list.selected.unwrap_or(0);
                let mut input = SingleLineInput::new(query, query_cursor);
                let provider_submit = submit_key;
                match self.menu_navigation_code(key) {
                    KeyCode::Up => selected = selected.saturating_sub(1),
                    KeyCode::Down => selected = (selected + 1).min(visible.len()),
                    KeyCode::Enter if selected == 0 => {
                        let (query, query_cursor) = input.into_parts();
                        if model_configuration_follows && self.enabled_providers.is_empty() {
                            self.surfaces.push(Surface::Providers {
                                rows,
                                list,
                                query,
                                query_cursor,
                                model_configuration_follows,
                            });
                        } else {
                            self.reconcile_after_provider_close(session).await?;
                        }
                        return Ok(false);
                    }
                    KeyCode::Char('d') if selected > 0 => {
                        if let Some(row) = visible.get(selected - 1).map(|row| (*row).clone())
                            && ((row.managed_auth && row.setup_instructions.is_none())
                                || (row.api_key_auth && row.has_managed_api_key))
                        {
                            let removal = if row.api_key_auth {
                                session.remove_provider_api_key(&row.id).await
                            } else {
                                session.disconnect_provider(&row.id).await
                            };
                            match removal {
                                Ok(()) => {
                                    if let Some(stored) =
                                        rows.iter_mut().find(|stored| stored.id == row.id)
                                    {
                                        stored.enabled = false;
                                        stored.status = "not setup".into();
                                        stored.setup_instructions = if row.api_key_auth {
                                            Some(stored.configuration_instructions.clone())
                                        } else {
                                            Some(format!("Connect your {} account.", row.label))
                                        };
                                        stored.has_managed_api_key = false;
                                    }
                                    self.enabled_providers.remove(&row.id);
                                    self.set_notice(format!("{} disconnected", row.label));
                                }
                                Err(error) => self.set_notice(error.to_string()),
                            }
                        }
                    }
                    KeyCode::Char('r') if selected > 0 => {
                        if let Some(row) = visible.get(selected - 1).map(|row| (*row).clone())
                            && row.managed_auth
                            && row.setup_instructions.is_none()
                        {
                            let (query, query_cursor) = input.into_parts();
                            self.surfaces.push(Surface::Providers {
                                rows,
                                list,
                                query,
                                query_cursor,
                                model_configuration_follows,
                            });
                            self.surfaces.push(Surface::ProviderSetup {
                                provider_id: row.id,
                                provider: row.label,
                                instructions: row.configuration_instructions,
                                credential_environment_variable: None,
                                auth_challenge: None,
                                managed_auth: true,
                                api_key_auth: false,
                                api_key: String::new(),
                                api_key_cursor: 0,
                                auth_flows: row.auth_flows,
                                authenticated: false,
                            });
                            return Ok(false);
                        } else if let Some(row) =
                            visible.get(selected - 1).map(|row| (*row).clone())
                            && row.api_key_auth
                        {
                            let (query, query_cursor) = input.into_parts();
                            self.surfaces.push(Surface::Providers {
                                rows,
                                list,
                                query,
                                query_cursor,
                                model_configuration_follows,
                            });
                            self.surfaces.push(Surface::ProviderSetup {
                                provider_id: row.id,
                                provider: row.label,
                                instructions: row.configuration_instructions,
                                credential_environment_variable: row
                                    .credential_environment_variable,
                                auth_challenge: None,
                                managed_auth: false,
                                api_key_auth: true,
                                api_key: String::new(),
                                api_key_cursor: 0,
                                auth_flows: row.auth_flows,
                                authenticated: false,
                            });
                            return Ok(false);
                        }
                    }
                    _ if provider_submit && selected > 0 => {
                        if let Some(row) = visible.get(selected - 1) {
                            if let Some(instructions) = row.setup_instructions.clone() {
                                let provider = row.label.clone();
                                let provider_id = row.id.clone();
                                let managed_auth = row.managed_auth;
                                let api_key_auth = row.api_key_auth;
                                let auth_flows = row.auth_flows.clone();
                                let credential_environment_variable =
                                    row.credential_environment_variable.clone();
                                let (query, query_cursor) = input.into_parts();
                                self.surfaces.push(Surface::Providers {
                                    rows,
                                    list,
                                    query,
                                    query_cursor,
                                    model_configuration_follows,
                                });
                                self.surfaces.push(Surface::ProviderSetup {
                                    provider_id,
                                    provider,
                                    instructions,
                                    credential_environment_variable,
                                    auth_challenge: None,
                                    managed_auth,
                                    api_key_auth,
                                    api_key: String::new(),
                                    api_key_cursor: 0,
                                    auth_flows,
                                    authenticated: false,
                                });
                                return Ok(false);
                            }
                            let id = row.id.clone();
                            match session.toggle_provider(&id).await {
                                Ok(availability) => {
                                    if let Some(stored) =
                                        rows.iter_mut().find(|stored| stored.id == id)
                                    {
                                        stored.enabled = availability.enabled;
                                        stored.status = availability.status_text().into();
                                    }
                                    if availability.enabled {
                                        self.enabled_providers.insert(id);
                                    } else {
                                        self.enabled_providers.remove(&id);
                                    }
                                }
                                Err(error) => self.set_notice(error.to_string()),
                            }
                        }
                    }
                    _ if !matches!(key.code, KeyCode::Char(character) if character.is_whitespace())
                        && input.handle_key(key) =>
                    {
                        selected = 0;
                        list.reset(ListMode::Selectable, visible.len() + 1);
                    }
                    _ => {}
                }
                let (query, query_cursor) = input.into_parts();
                list.select(selected, VISIBLE_MENU_ITEMS);
                self.surfaces.push(Surface::Providers {
                    rows,
                    list,
                    query,
                    query_cursor,
                    model_configuration_follows,
                });
            }
            Surface::Models {
                mut rows,
                mut list,
                query,
                query_cursor,
                selection_target,
            } => {
                let visible = filter_model_picker_rows(&rows, &query);
                list.reconcile(ListMode::Selectable, visible.len(), VISIBLE_MENU_ITEMS);
                let selected = list.selected.unwrap_or(0);
                let mut input = SingleLineInput::new(query, query_cursor);
                let code = self.menu_navigation_code(key);
                if let Some(action) = list_action(code) {
                    list.apply(action, ListMode::Selectable, VISIBLE_MENU_ITEMS);
                } else {
                    match code {
                        KeyCode::Enter => {
                            if let Some(row) = visible.get(selected).copied() {
                                let row = (*row).clone();
                                if !matches!(&selection_target, ModelSelectionTarget::Setting(_))
                                    && matches!(
                                        effort_picker_flow(&row.efforts),
                                        EffortPickerFlow::Choose
                                    )
                                {
                                    let (query, query_cursor) = input.into_parts();
                                    self.surfaces.push(Surface::Models {
                                        rows,
                                        list,
                                        query,
                                        query_cursor,
                                        selection_target: selection_target.clone(),
                                    });
                                }
                                self.select_model_row(session, row, selection_target)
                                    .await?;
                                return Ok(false);
                            }
                        }
                        KeyCode::Char('f')
                            if key.modifiers.is_empty()
                                && !matches!(
                                    &selection_target,
                                    ModelSelectionTarget::Setting(_)
                                ) =>
                        {
                            if let Some(row) = visible.get(selected).copied() {
                                let provider = row.provider.clone();
                                let model = row.id.clone();
                                let favourite =
                                    session.toggle_model_favourite(&provider, &model)?;
                                if let Some(row) = rows
                                    .iter_mut()
                                    .find(|row| row.provider == provider && row.id == model)
                                {
                                    row.favourite = favourite;
                                }
                                prioritize_current_model(&mut rows);
                                let selected = filter_model_picker_rows(&rows, input.text())
                                    .iter()
                                    .position(|row| row.provider == provider && row.id == model)
                                    .unwrap_or(0);
                                list.select(selected, VISIBLE_MENU_ITEMS);
                            }
                        }
                        _ if !matches!(&selection_target, ModelSelectionTarget::Setting(_))
                            && self.matches_action(
                                cagent_agent::presentation::KeyBindingAction::Exit,
                                key,
                            ) =>
                        {
                            session
                                .submit(SessionCommand::new(SessionAction::UseAgentDefault))
                                .await?;
                            self.surfaces.clear();
                            self.dismiss_onboarding();
                            self.set_notice("using agent model default");
                            return Ok(false);
                        }
                        _ if !matches!(key.code, KeyCode::Char(character) if character.is_whitespace())
                            && input.handle_key(key) =>
                        {
                            list.reset(
                                ListMode::Selectable,
                                filter_model_picker_rows(&rows, input.text()).len(),
                            );
                        }
                        _ => {}
                    }
                }
                let (query, query_cursor) = input.into_parts();
                self.surfaces.push(Surface::Models {
                    rows,
                    list,
                    query,
                    query_cursor,
                    selection_target,
                });
            }
            Surface::Effort {
                provider,
                model,
                rows,
                reasoning_control,
                mut list,
                selection_target,
            } => {
                list.reconcile(ListMode::Selectable, rows.len(), VISIBLE_MENU_ITEMS);
                let code = self.menu_navigation_code(key);
                if let Some(action) = list_action(code) {
                    list.apply(action, ListMode::Selectable, VISIBLE_MENU_ITEMS);
                } else if code == KeyCode::Enter {
                    if let Some(effort) = list.selected.and_then(|index| rows.get(index)).cloned() {
                        self.apply_model_selection(
                            session,
                            selection_target,
                            provider,
                            model,
                            Some(effort),
                        )
                        .await?;
                        return Ok(false);
                    }
                }
                self.surfaces.push(Surface::Effort {
                    provider,
                    model,
                    rows,
                    reasoning_control,
                    list,
                    selection_target,
                });
            }
            Surface::McpServers { rows, mut list } => {
                list.reconcile(ListMode::Selectable, rows.len() + 3, VISIBLE_MENU_ITEMS);
                let code = self.menu_navigation_code(key);
                if let Some(action) = list_action(code) {
                    list.apply(action, ListMode::Selectable, VISIBLE_MENU_ITEMS);
                } else {
                    match code {
                        KeyCode::Enter if list.selected == Some(0) => return Ok(false),
                        KeyCode::Enter if list.selected == Some(1) => {
                            self.surfaces.push(Surface::McpServers { rows, list });
                            self.surfaces.push(Surface::McpAddScope { selected: 0 });
                            return Ok(false);
                        }
                        KeyCode::Enter if list.selected == Some(2) => {
                            let packages = cagent_agent::mcp::builtin_packages();
                            self.surfaces.push(Surface::McpServers { rows, list });
                            self.surfaces.push(Surface::McpCatalog {
                                list: ListState::selectable(packages.len() + 1),
                                rows: packages,
                            });
                            return Ok(false);
                        }
                        KeyCode::Enter => {
                            if let Some(server) = list
                                .selected
                                .and_then(|selected| selected.checked_sub(3))
                                .and_then(|index| rows.get(index))
                                .cloned()
                            {
                                let oauth_connected = session
                                    .mcp_oauth_status(&server.name)
                                    .await
                                    .is_ok_and(|status| {
                                        status == cagent_agent::mcp::McpSecretStatus::Configured
                                    });
                                self.surfaces.push(Surface::McpServers { rows, list });
                                self.surfaces.push(Surface::McpServer {
                                    server,
                                    selected: 0,
                                    oauth_connected,
                                });
                                return Ok(false);
                            }
                        }
                        _ => {}
                    }
                }
                self.surfaces.push(Surface::McpServers { rows, list });
            }
            Surface::McpCatalog { rows, mut list } => {
                list.reconcile(ListMode::Selectable, rows.len() + 1, VISIBLE_MENU_ITEMS);
                let code = self.menu_navigation_code(key);
                if let Some(action) = list_action(code) {
                    list.apply(action, ListMode::Selectable, VISIBLE_MENU_ITEMS);
                } else if code == KeyCode::Enter {
                    let Some(selected) = list.selected else {
                        self.surfaces.push(Surface::McpCatalog { rows, list });
                        return Ok(false);
                    };
                    if selected == 0 {
                        return Ok(false);
                    }
                    let draft = package_draft(rows[selected - 1].clone());
                    self.surfaces.push(Surface::McpCatalog { rows, list });
                    self.surfaces.push(Surface::McpPackageSetup {
                        draft: Box::new(draft),
                        selected: 0,
                    });
                    return Ok(false);
                }
                self.surfaces.push(Surface::McpCatalog { rows, list });
            }
            Surface::McpServer {
                server,
                mut selected,
                mut oauth_connected,
            } => {
                let has_oauth = matches!(
                    &server.definition.transport,
                    cagent_agent::mcp::McpTransportConfig::StreamableHttp { .. }
                );
                let has_package = server.definition.package.is_some();
                let package_index = 5 + usize::from(has_oauth);
                let remove_index = package_index + usize::from(has_package);
                match self.menu_navigation_code(key) {
                    KeyCode::Up => selected = selected.saturating_sub(1),
                    KeyCode::Down => selected = (selected + 1).min(remove_index),
                    KeyCode::PageUp | KeyCode::Home => selected = 0,
                    KeyCode::PageDown | KeyCode::End => selected = remove_index,
                    KeyCode::Enter if selected == 0 => return Ok(false),
                    KeyCode::Enter if selected == 1 => {
                        match session
                            .set_mcp_enabled(
                                server.location.clone(),
                                &server.name,
                                !server.definition.enabled,
                            )
                            .await
                        {
                            Ok(rows) => {
                                self.surfaces.clear();
                                let item_count = rows.len() + 2;
                                self.surfaces.push(Surface::McpServers {
                                    rows,
                                    list: ListState::selectable(item_count),
                                });
                            }
                            Err(error) => {
                                self.set_notice(format!("MCP update failed · {error}"));
                                self.surfaces.push(Surface::McpServer {
                                    server,
                                    selected,
                                    oauth_connected,
                                });
                            }
                        }
                        return Ok(false);
                    }
                    KeyCode::Enter if selected == remove_index => {
                        self.surfaces.push(Surface::McpServer {
                            server: server.clone(),
                            selected,
                            oauth_connected,
                        });
                        self.surfaces.push(Surface::McpRemoveConfirm {
                            server,
                            selected: 0,
                        });
                        return Ok(false);
                    }
                    KeyCode::Enter if has_oauth && selected == 5 => {
                        if oauth_connected {
                            match session.disconnect_mcp_oauth(&server.name).await {
                                Ok(()) => {
                                    oauth_connected = false;
                                    self.set_notice("MCP OAuth credentials removed");
                                }
                                Err(error) => {
                                    self.set_notice(format!("MCP logout failed · {error}"));
                                }
                            }
                            let refreshed =
                                session.mcp_server(&server.name).await?.unwrap_or(server);
                            self.surfaces.push(Surface::McpServer {
                                server: refreshed,
                                selected,
                                oauth_connected,
                            });
                        } else {
                            match session.connect_mcp_oauth(&server.name).await {
                                Ok(attempt) => {
                                    let (url, copy_value, copy_description) = match &attempt.prompt
                                    {
                                        cagent_agent::mcp::McpOAuthPrompt::Browser {
                                            authorization_url,
                                            ..
                                        } => (authorization_url, authorization_url, "login URL"),
                                        cagent_agent::mcp::McpOAuthPrompt::Device {
                                            verification_url,
                                            user_code,
                                        } => (verification_url, user_code, "device code"),
                                    };
                                    // Match provider Device Flow: launch the browser first, then
                                    // place the short code on the clipboard as the auth surface opens.
                                    // Some desktop clipboard managers replace an earlier selection
                                    // while handling the browser launch.
                                    let opened = open_browser(url);
                                    let copied = copy_to_clipboard(copy_value);
                                    self.set_notice(match (opened, copied) {
                                        (true, true) => format!(
                                            "opened MCP authentication · copied {copy_description}"
                                        ),
                                        (true, false) => format!(
                                            "opened MCP authentication · could not copy {copy_description}"
                                        ),
                                        (false, true) => format!(
                                            "browser unavailable · copied MCP {copy_description}"
                                        ),
                                        (false, false) => format!(
                                            "browser and clipboard unavailable · use the value shown"
                                        ),
                                    });
                                    let label = server
                                        .definition
                                        .package
                                        .as_deref()
                                        .and_then(|source| source.strip_prefix("builtin:"))
                                        .and_then(|id| {
                                            cagent_agent::mcp::builtin_packages()
                                                .into_iter()
                                                .find(|package| package.id == id)
                                                .map(|package| package.name)
                                        })
                                        .unwrap_or_else(|| server.name.clone());
                                    let name = server.name.clone();
                                    self.surfaces.push(Surface::McpServer {
                                        server,
                                        selected,
                                        oauth_connected,
                                    });
                                    self.surfaces.push(Surface::McpOAuth {
                                        server: name,
                                        label,
                                        attempt,
                                    });
                                }
                                Err(error) => {
                                    self.set_notice(format!("MCP login failed · {error}"));
                                    self.surfaces.push(Surface::McpServer {
                                        server,
                                        selected,
                                        oauth_connected,
                                    });
                                }
                            }
                        }
                        return Ok(false);
                    }
                    KeyCode::Enter if has_package && selected == package_index => {
                        match session.mcp_package_setup(&server.name).await {
                            Ok(Some(setup)) => {
                                let draft = McpPackageDraft {
                                    package: setup.package,
                                    server_name: setup.server_name,
                                    location: setup.location,
                                    parameters: setup.parameters,
                                    secret_statuses: setup.secret_statuses,
                                    secret_updates: std::collections::BTreeMap::new(),
                                    installed: true,
                                };
                                self.surfaces.push(Surface::McpServer {
                                    server,
                                    selected,
                                    oauth_connected,
                                });
                                self.surfaces.push(Surface::McpPackageSetup {
                                    draft: Box::new(draft),
                                    selected: 0,
                                });
                            }
                            Ok(None) => {
                                self.set_notice("package setup is unavailable");
                                self.surfaces.push(Surface::McpServer {
                                    server,
                                    selected,
                                    oauth_connected,
                                });
                            }
                            Err(error) => {
                                self.set_notice(format!("MCP package setup failed · {error}"));
                                self.surfaces.push(Surface::McpServer {
                                    server,
                                    selected,
                                    oauth_connected,
                                });
                            }
                        }
                        return Ok(false);
                    }
                    KeyCode::Enter if selected == 2 => {
                        let draft = McpFormDraft {
                            original: Some((server.location.clone(), server.name.clone())),
                            location: server.location.clone(),
                            name: server.name.clone(),
                            definition: server.definition.clone(),
                        };
                        let list = ListState::selectable(mcp_form_maximum(&draft.definition) + 1);
                        self.surfaces.push(Surface::McpServer {
                            server,
                            selected,
                            oauth_connected,
                        });
                        self.surfaces.push(Surface::McpForm {
                            draft: Box::new(draft),
                            list,
                        });
                        return Ok(false);
                    }
                    KeyCode::Enter if selected == 3 => {
                        let source = serde_json::to_string_pretty(&server.definition)?;
                        let cursor = cursor_at_end(&source);
                        self.surfaces.push(Surface::McpServer {
                            server: server.clone(),
                            selected,
                            oauth_connected,
                        });
                        self.surfaces.push(Surface::McpJsonEdit {
                            location: server.location,
                            separate_name: Some(server.name),
                            editor: MultilineInput::new(source, cursor)
                                .with_syntax_language("json"),
                        });
                        return Ok(false);
                    }
                    KeyCode::Enter if selected == 4 => {
                        let name = server.name.clone();
                        let inspection_name = name.clone();
                        let inspection = session.clone();
                        tokio::spawn(
                            async move {
                                let _ = inspection.inspect_mcp_server(&inspection_name).await;
                            }
                            .in_current_span(),
                        );
                        self.surfaces.push(Surface::McpServer {
                            server,
                            selected,
                            oauth_connected,
                        });
                        self.surfaces.push(Surface::McpTools {
                            server: name,
                            tools: Vec::new(),
                            list: ListState::selectable(0),
                            loading: true,
                            failure: None,
                        });
                        return Ok(false);
                    }
                    _ => {}
                }
                self.surfaces.push(Surface::McpServer {
                    server,
                    selected,
                    oauth_connected,
                });
            }
            Surface::McpOAuth {
                server,
                label,
                attempt,
            } => {
                if matches!(
                    &*attempt.completion.borrow(),
                    cagent_agent::mcp::McpOAuthCompletion::Connected
                ) && submit_key
                {
                    self.refresh_open_mcp_server(session).await;
                    return Ok(false);
                }
                let code = self.menu_navigation_code(key);
                let target = match &attempt.prompt {
                    cagent_agent::mcp::McpOAuthPrompt::Browser {
                        authorization_url, ..
                    } => authorization_url,
                    cagent_agent::mcp::McpOAuthPrompt::Device {
                        verification_url, ..
                    } => verification_url,
                };
                match code {
                    KeyCode::Enter | KeyCode::Char('o') => {
                        if !open_browser(target) {
                            self.set_notice("browser unavailable");
                        }
                    }
                    KeyCode::Char('c') => {
                        let value = match &attempt.prompt {
                            cagent_agent::mcp::McpOAuthPrompt::Browser {
                                authorization_url,
                                ..
                            } => authorization_url,
                            cagent_agent::mcp::McpOAuthPrompt::Device {
                                verification_url, ..
                            } => verification_url,
                        };
                        self.set_notice(if copy_to_clipboard(value) {
                            "MCP OAuth URL copied"
                        } else {
                            "clipboard unavailable"
                        });
                    }
                    KeyCode::Char('C') => {
                        if let cagent_agent::mcp::McpOAuthPrompt::Device { user_code, .. } =
                            &attempt.prompt
                        {
                            self.set_notice(if copy_to_clipboard(user_code) {
                                "MCP OAuth device code copied"
                            } else {
                                "clipboard unavailable"
                            });
                        }
                    }
                    _ => {}
                }
                self.surfaces.push(Surface::McpOAuth {
                    server,
                    label,
                    attempt,
                });
            }
            Surface::McpPackageSetup {
                mut draft,
                mut selected,
            } => {
                let maximum = package_setup_count(&draft).saturating_sub(1);
                match self.menu_navigation_code(key) {
                    KeyCode::Up => selected = selected.saturating_sub(1),
                    KeyCode::Down => selected = (selected + 1).min(maximum),
                    KeyCode::PageUp | KeyCode::Home => selected = 0,
                    KeyCode::PageDown | KeyCode::End => selected = maximum,
                    KeyCode::Enter if selected == 0 => {
                        let result = if draft.installed {
                            session
                                .configure_mcp_package(
                                    &draft.server_name,
                                    draft.parameters.clone(),
                                    draft.secret_updates.clone(),
                                )
                                .await
                        } else {
                            let secrets = draft
                                .secret_updates
                                .iter()
                                .filter_map(|(id, value)| {
                                    value
                                        .as_ref()
                                        .filter(|value| !value.is_empty())
                                        .map(|value| (id.clone(), value.clone()))
                                })
                                .collect();
                            session
                                .install_builtin_mcp_configured(
                                    &draft.package.id,
                                    draft.location.clone(),
                                    draft.parameters.clone(),
                                    secrets,
                                )
                                .await
                        };
                        match result {
                            Ok(rows) => {
                                self.surfaces.clear();
                                self.surfaces.push(Surface::McpServers {
                                    list: ListState::selectable(rows.len() + 3),
                                    rows,
                                });
                            }
                            Err(error) => {
                                self.set_notice(format!("MCP package setup failed · {error}"));
                                self.surfaces
                                    .push(Surface::McpPackageSetup { draft, selected });
                            }
                        }
                        return Ok(false);
                    }
                    KeyCode::Enter => {
                        let Some(field) = package_setup_field(&draft, selected) else {
                            self.surfaces
                                .push(Surface::McpPackageSetup { draft, selected });
                            return Ok(false);
                        };
                        if let McpPackageField::Parameter(id) = &field
                            && draft.package.parameters.get(id).is_some_and(|parameter| {
                                parameter.kind
                                    == cagent_agent::mcp::McpPackageParameterType::Boolean
                            })
                        {
                            let value = draft.parameters.get(id).is_none_or(|value| {
                                !matches!(
                                    value,
                                    cagent_agent::mcp::McpParameterValue::Boolean(true)
                                )
                            });
                            draft.parameters.insert(
                                id.clone(),
                                cagent_agent::mcp::McpParameterValue::Boolean(value),
                            );
                        } else {
                            let value = package_field_value(&draft, &field);
                            let cursor = cursor_at_end(&value);
                            self.surfaces.push(Surface::McpPackageSetup {
                                draft: draft.clone(),
                                selected,
                            });
                            self.surfaces.push(Surface::McpPackageValueEdit {
                                draft,
                                field,
                                value,
                                cursor,
                            });
                            return Ok(false);
                        }
                    }
                    _ => {}
                }
                self.surfaces
                    .push(Surface::McpPackageSetup { draft, selected });
            }
            Surface::McpPackageValueEdit {
                mut draft,
                field,
                value,
                cursor,
            } => {
                if submit_key {
                    match apply_package_field(&mut draft, &field, value.clone()) {
                        Ok(()) => {
                            self.surfaces.pop();
                            let selected = match &field {
                                McpPackageField::Secret(id) => draft
                                    .package
                                    .secrets
                                    .keys()
                                    .position(|candidate| candidate == id)
                                    .map_or(0, |index| index + 1),
                                McpPackageField::Parameter(id) => draft
                                    .package
                                    .parameters
                                    .keys()
                                    .position(|candidate| candidate == id)
                                    .map_or(0, |index| 1 + draft.package.secrets.len() + index),
                            };
                            self.surfaces
                                .push(Surface::McpPackageSetup { draft, selected });
                        }
                        Err(error) => {
                            self.set_notice(format!("Invalid package value · {error}"));
                            self.surfaces.push(Surface::McpPackageValueEdit {
                                draft,
                                field,
                                value,
                                cursor,
                            });
                        }
                    }
                    return Ok(false);
                }
                let mut input = SingleLineInput::new(value, cursor);
                input.handle_key(key);
                let (value, cursor) = input.into_parts();
                self.surfaces.push(Surface::McpPackageValueEdit {
                    draft,
                    field,
                    value,
                    cursor,
                });
            }
            Surface::McpTools {
                server,
                tools,
                mut list,
                loading,
                failure,
            } => {
                list.reconcile(ListMode::Selectable, tools.len(), VISIBLE_MENU_ITEMS);
                let code = self.menu_navigation_code(key);
                if let Some(action) = list_action(code) {
                    list.apply(action, ListMode::Selectable, VISIBLE_MENU_ITEMS);
                } else if code == KeyCode::Enter {
                    return Ok(false);
                }
                self.surfaces.push(Surface::McpTools {
                    server,
                    tools,
                    list,
                    loading,
                    failure,
                });
            }
            Surface::McpRemoveConfirm {
                server,
                mut selected,
            } => {
                match self.menu_navigation_code(key) {
                    KeyCode::Up => selected = selected.saturating_sub(1),
                    KeyCode::Down => selected = (selected + 1).min(1),
                    KeyCode::PageUp | KeyCode::Home => selected = 0,
                    KeyCode::PageDown | KeyCode::End => selected = 1,
                    KeyCode::Enter if selected == 0 => return Ok(false),
                    KeyCode::Enter => {
                        match session
                            .remove_mcp_server(server.location.clone(), &server.name, true)
                            .await
                        {
                            Ok(rows) => {
                                self.surfaces.clear();
                                let item_count = rows.len() + 2;
                                self.surfaces.push(Surface::McpServers {
                                    rows,
                                    list: ListState::selectable(item_count),
                                });
                            }
                            Err(error) => {
                                self.set_notice(format!("MCP removal failed · {error}"));
                                self.surfaces
                                    .push(Surface::McpRemoveConfirm { server, selected });
                            }
                        }
                        return Ok(false);
                    }
                    _ => {}
                }
                self.surfaces
                    .push(Surface::McpRemoveConfirm { server, selected });
            }
            Surface::McpAddScope { mut selected } => {
                match self.menu_navigation_code(key) {
                    KeyCode::Up => selected = selected.saturating_sub(1),
                    KeyCode::Down => selected = (selected + 1).min(2),
                    KeyCode::PageUp | KeyCode::Home => selected = 0,
                    KeyCode::PageDown | KeyCode::End => selected = 2,
                    KeyCode::Enter if selected == 0 => return Ok(false),
                    KeyCode::Enter => {
                        self.surfaces.push(Surface::McpAddScope { selected });
                        let location = if selected == 1 {
                            cagent_agent::mcp::McpLocation::global()
                        } else {
                            cagent_agent::mcp::McpLocation::project()
                        };
                        self.surfaces.push(Surface::McpTransport {
                            location,
                            selected: 0,
                        });
                        return Ok(false);
                    }
                    _ => {}
                }
                self.surfaces.push(Surface::McpAddScope { selected });
            }
            Surface::McpTransport {
                location,
                mut selected,
            } => {
                match self.menu_navigation_code(key) {
                    KeyCode::Up => selected = selected.saturating_sub(1),
                    KeyCode::Down => selected = (selected + 1).min(3),
                    KeyCode::PageUp | KeyCode::Home => selected = 0,
                    KeyCode::PageDown | KeyCode::End => selected = 3,
                    KeyCode::Enter if selected == 0 => return Ok(false),
                    KeyCode::Enter if selected == 3 => {
                        self.surfaces.push(Surface::McpTransport {
                            location: location.clone(),
                            selected,
                        });
                        let source = String::from("{\n  \n}");
                        let cursor = cursor_at_end(&source);
                        self.surfaces.push(Surface::McpJsonEdit {
                            location,
                            separate_name: None,
                            editor: MultilineInput::new(source, cursor)
                                .with_syntax_language("json"),
                        });
                        return Ok(false);
                    }
                    KeyCode::Enter => {
                        let definition = mcp_definition_template(selected);
                        let list = ListState::selectable_at(
                            mcp_form_maximum(&definition) + 1,
                            2,
                            VISIBLE_MENU_ITEMS,
                        );
                        self.surfaces.push(Surface::McpTransport {
                            location: location.clone(),
                            selected,
                        });
                        self.surfaces.push(Surface::McpForm {
                            draft: Box::new(McpFormDraft {
                                original: None,
                                location,
                                name: String::new(),
                                definition,
                            }),
                            list,
                        });
                        return Ok(false);
                    }
                    _ => {}
                }
                self.surfaces
                    .push(Surface::McpTransport { location, selected });
            }
            Surface::McpForm {
                mut draft,
                mut list,
            } => {
                let maximum = mcp_form_maximum(&draft.definition);
                list.reconcile(ListMode::Selectable, maximum + 1, VISIBLE_MENU_ITEMS);
                let code = self.menu_navigation_code(key);
                if let Some(action) = list_action(code) {
                    list.apply(action, ListMode::Selectable, VISIBLE_MENU_ITEMS);
                } else {
                    let selected = list.selected.unwrap_or(0);
                    match code {
                        KeyCode::Enter if selected == 0 => return Ok(false),
                        KeyCode::Enter if selected == 1 => {
                            match session
                                .preview_mcp_mutations(mcp_form_mutations(&draft))
                                .await
                            {
                                Ok(preview) => {
                                    self.surfaces.push(Surface::McpForm { draft, list });
                                    self.surfaces.push(Surface::McpMutationPreview {
                                        preview,
                                        list: ListState::selectable(2),
                                        details: ScrollViewState::default(),
                                    });
                                }
                                Err(error) => {
                                    self.surfaces.push(Surface::McpForm { draft, list });
                                    self.set_notice(format!("MCP draft is invalid · {error}"));
                                }
                            }
                            return Ok(false);
                        }
                        KeyCode::Enter if selected == 3 => {
                            let (active_agent, _) = session.active_profiles().await?;
                            draft.location = next_mcp_location(&draft.location, active_agent);
                        }
                        KeyCode::Enter if selected == 4 => {
                            draft.definition.enabled = !draft.definition.enabled;
                        }
                        KeyCode::Enter if selected == 5 => {
                            draft.definition.eager = !draft.definition.eager;
                        }
                        KeyCode::Enter if is_mcp_form_boolean(&draft.definition, selected) => {
                            toggle_mcp_form_boolean(&mut draft.definition, selected);
                        }
                        KeyCode::Enter => {
                            if let Some(field) = mcp_form_field(&draft.definition, selected) {
                                let value = mcp_form_field_value(&draft, field);
                                let cursor = cursor_at_end(&value);
                                self.surfaces.push(Surface::McpForm {
                                    draft: draft.clone(),
                                    list,
                                });
                                self.surfaces.push(Surface::McpFieldEdit {
                                    draft,
                                    field,
                                    value,
                                    cursor,
                                });
                                return Ok(false);
                            }
                        }
                        _ => {}
                    }
                }
                self.surfaces.push(Surface::McpForm { draft, list });
            }
            Surface::McpFieldEdit {
                mut draft,
                field,
                value,
                cursor,
            } => {
                if submit_key {
                    match apply_mcp_form_field(&mut draft, field, &value) {
                        Ok(()) => {
                            self.surfaces.pop();
                            let selected = mcp_form_field_index(field);
                            self.surfaces.push(Surface::McpForm {
                                list: ListState::selectable_at(
                                    mcp_form_maximum(&draft.definition) + 1,
                                    selected,
                                    VISIBLE_MENU_ITEMS,
                                ),
                                draft,
                            });
                        }
                        Err(error) => {
                            self.surfaces.push(Surface::McpFieldEdit {
                                draft,
                                field,
                                value,
                                cursor,
                            });
                            self.set_notice(format!("Invalid MCP field · {error}"));
                        }
                    }
                    return Ok(false);
                }
                let mut input = SingleLineInput::new(value, cursor);
                if mcp_field_uses_json(field) {
                    input = input.with_syntax_language("json");
                }
                input.handle_key(key);
                let (value, cursor) = input.into_parts();
                self.surfaces.push(Surface::McpFieldEdit {
                    draft,
                    field,
                    value,
                    cursor,
                });
            }
            Surface::McpJsonEdit {
                location,
                separate_name,
                mut editor,
            } => {
                if is_control_enter(key) {
                    match session
                        .preview_mcp_import(
                            location.clone(),
                            editor.text(),
                            separate_name.as_deref(),
                        )
                        .await
                    {
                        Ok(preview) => match session.apply_mcp_import(preview, true).await {
                            Ok(rows) => {
                                self.surfaces.clear();
                                let item_count = rows.len() + 2;
                                self.surfaces.push(Surface::McpServers {
                                    rows,
                                    list: ListState::selectable(item_count),
                                });
                            }
                            Err(error) => {
                                self.surfaces.push(Surface::McpJsonEdit {
                                    location,
                                    separate_name,
                                    editor,
                                });
                                self.set_notice(format!("MCP import failed · {error}"));
                            }
                        },
                        Err(error) => {
                            self.surfaces.push(Surface::McpJsonEdit {
                                location,
                                separate_name,
                                editor,
                            });
                            self.set_notice(format!("Invalid MCP JSON · {error}"));
                        }
                    }
                    return Ok(false);
                }
                editor.handle_key_in_view(key, multiline_width, multiline_capacity);
                self.surfaces.push(Surface::McpJsonEdit {
                    location,
                    separate_name,
                    editor,
                });
            }
            Surface::McpMutationPreview {
                preview,
                mut list,
                mut details,
            } => {
                list.reconcile(ListMode::Selectable, 2, VISIBLE_MENU_ITEMS);
                let detail_rows = super::surfaces::mcp_mutation_detail_lines(&preview).len();
                details.reconcile(detail_rows, VISIBLE_HELP_ITEMS, false);
                let selected = list.selected.unwrap_or(0);
                match self.menu_navigation_code(key) {
                    KeyCode::Up => {
                        list.apply(
                            ListAction::Previous,
                            ListMode::Selectable,
                            VISIBLE_MENU_ITEMS,
                        );
                    }
                    KeyCode::Down => {
                        list.apply(ListAction::Next, ListMode::Selectable, VISIBLE_MENU_ITEMS);
                    }
                    KeyCode::PageUp => {
                        details.apply(ScrollViewAction::PagePrevious, VISIBLE_HELP_ITEMS);
                    }
                    KeyCode::PageDown => {
                        details.apply(ScrollViewAction::PageNext, VISIBLE_HELP_ITEMS);
                    }
                    KeyCode::Home => {
                        details.apply(ScrollViewAction::Home, VISIBLE_HELP_ITEMS);
                    }
                    KeyCode::End => {
                        details.apply(ScrollViewAction::End, VISIBLE_HELP_ITEMS);
                    }
                    KeyCode::Enter if selected == 0 => return Ok(false),
                    KeyCode::Enter => {
                        match session.apply_mcp_mutations(preview.clone(), true).await {
                            Ok(rows) => {
                                self.surfaces.clear();
                                let item_count = rows.len() + 2;
                                self.surfaces.push(Surface::McpServers {
                                    rows,
                                    list: ListState::selectable(item_count),
                                });
                            }
                            Err(error) => {
                                self.surfaces.push(Surface::McpMutationPreview {
                                    preview,
                                    list,
                                    details,
                                });
                                self.set_notice(format!("MCP update failed · {error}"));
                            }
                        }
                        return Ok(false);
                    }
                    _ => {}
                }
                self.surfaces.push(Surface::McpMutationPreview {
                    preview,
                    list,
                    details,
                });
            }
            Surface::Skills { mut rows, mut list } => {
                list.reconcile(ListMode::Selectable, rows.len() + 2, VISIBLE_MENU_ITEMS);
                if let Some(action) = list_action(self.menu_navigation_code(key)) {
                    list.apply(action, ListMode::Selectable, VISIBLE_MENU_ITEMS);
                } else if key.code == KeyCode::Enter {
                    match list.selected.unwrap_or(0) {
                        0 => return Ok(false),
                        1 => {
                            self.surfaces.push(Surface::SkillWizard {
                                project: true,
                                scope_list: ListState::selectable(2),
                                name: SingleLineInput::new(String::new(), 0),
                                description: SingleLineInput::new(String::new(), 0),
                                content: MultilineInput::new(String::new(), 0),
                                step: SkillWizardStep::Scope,
                            });
                            return Ok(false);
                        }
                        selected => {
                            let name = rows[selected - 2].name.clone();
                            let enabled = session.toggle_skill(&name)?;
                            for skill in &mut rows {
                                if skill.name == name {
                                    skill.enabled = enabled;
                                }
                            }
                        }
                    }
                } else if key.code == KeyCode::Char('e')
                    && key.modifiers.is_empty()
                    && let Some(skill) = list
                        .selected
                        .and_then(|selected| selected.checked_sub(2))
                        .and_then(|selected| rows.get(selected))
                        .cloned()
                {
                    if skill.source == cagent_agent::config::SkillSource::System {
                        self.surfaces.push(Surface::Skills { rows, list });
                        return Ok(false);
                    }
                    if let Some(editor) =
                        std::env::var_os("VISUAL").or_else(|| std::env::var_os("EDITOR"))
                    {
                        let mut argv = editor
                            .to_string_lossy()
                            .split_whitespace()
                            .map(str::to_owned)
                            .collect::<Vec<_>>();
                        argv.push(skill.path.to_string_lossy().into_owned());
                        self.pending_action = Some(AppAction::LaunchPath {
                            argv_candidates: vec![argv],
                            mode: cagent_agent::config::UiEditorMode::Foreground,
                            reload_startup_resources: true,
                        });
                    } else {
                        self.set_notice("set $VISUAL or $EDITOR to edit skills");
                    }
                } else if key.code == KeyCode::Char('d')
                    && key.modifiers.is_empty()
                    && let Some(skill) = list
                        .selected
                        .and_then(|selected| selected.checked_sub(2))
                        .and_then(|selected| rows.get(selected))
                        .cloned()
                {
                    if skill.source == cagent_agent::config::SkillSource::System {
                        self.surfaces.push(Surface::Skills { rows, list });
                        return Ok(false);
                    }
                    self.surfaces.push(Surface::Skills { rows, list });
                    self.surfaces
                        .push(Surface::SkillDeleteConfirm { skill, selected: 0 });
                    return Ok(false);
                }
                self.surfaces.push(Surface::Skills { rows, list });
            }
            Surface::SkillDeleteConfirm {
                skill,
                mut selected,
            } => {
                match self.menu_navigation_code(key) {
                    KeyCode::Up | KeyCode::PageUp | KeyCode::Home => selected = 0,
                    KeyCode::Down | KeyCode::PageDown | KeyCode::End => selected = 1,
                    KeyCode::Enter if selected == 0 => return Ok(false),
                    KeyCode::Enter => {
                        session.delete_skill(&skill.path)?;
                        session
                            .submit(SessionCommand::new(SessionAction::ReloadStartupResources))
                            .await?;
                        let rows = session.skills();
                        self.surfaces.clear();
                        self.surfaces.push(Surface::Skills {
                            list: ListState::selectable(rows.len() + 2),
                            rows,
                        });
                        return Ok(false);
                    }
                    _ => {}
                }
                self.surfaces
                    .push(Surface::SkillDeleteConfirm { skill, selected });
            }
            Surface::SkillWizard {
                mut project,
                mut scope_list,
                mut name,
                mut description,
                mut content,
                mut step,
            } => {
                match key.code {
                    KeyCode::Up | KeyCode::Down if step == SkillWizardStep::Scope => {
                        scope_list.apply(
                            if key.code == KeyCode::Up {
                                ListAction::Previous
                            } else {
                                ListAction::Next
                            },
                            ListMode::Selectable,
                            VISIBLE_MENU_ITEMS,
                        );
                        project = scope_list.selected.unwrap_or(0) == 0;
                    }
                    KeyCode::Enter
                        if step == SkillWizardStep::Content
                            && (is_control_enter(key)
                                || key.modifiers.contains(KeyModifiers::SHIFT)) =>
                    {
                        content.insert_text_in_view("\n", multiline_width, multiline_capacity);
                    }
                    KeyCode::Enter => match step {
                        SkillWizardStep::Scope => step = SkillWizardStep::Name,
                        SkillWizardStep::Name if !name.text().trim().is_empty() => {
                            step = SkillWizardStep::Description
                        }
                        SkillWizardStep::Description if !description.text().trim().is_empty() => {
                            step = SkillWizardStep::Content
                        }
                        SkillWizardStep::Content => {
                            let path = session.create_skill(
                                project,
                                name.text(),
                                description.text(),
                                content.text(),
                            )?;
                            session
                                .submit(SessionCommand::new(SessionAction::ReloadStartupResources))
                                .await?;
                            let mut rows = session.skills();
                            if !rows.iter().any(|skill| skill.path == path) {
                                rows.push(cagent_agent::config::SkillMetadata {
                                    path,
                                    name: name.text().to_owned(),
                                    description: description.text().to_owned(),
                                    source: if project {
                                        cagent_agent::config::SkillSource::Project
                                    } else {
                                        cagent_agent::config::SkillSource::Global
                                    },
                                    compatibility: None,
                                    compatibility_project: false,
                                    enabled: true,
                                    content_hash: String::new(),
                                });
                            }
                            rows.sort_by(|left, right| {
                                left.source
                                    .cmp(&right.source)
                                    .then_with(|| left.name.cmp(&right.name))
                                    .then_with(|| left.path.cmp(&right.path))
                            });
                            self.surfaces.push(Surface::Skills {
                                list: ListState::selectable(rows.len() + 2),
                                rows,
                            });
                            return Ok(false);
                        }
                        _ => {}
                    },
                    _ if matches!(step, SkillWizardStep::Name | SkillWizardStep::Description) => {
                        let input = if step == SkillWizardStep::Name {
                            &mut name
                        } else {
                            &mut description
                        };
                        input.handle_key(key);
                    }
                    _ if step == SkillWizardStep::Content => {
                        content.handle_key_in_view(key, multiline_width, multiline_capacity);
                    }
                    _ => {}
                }
                self.surfaces.push(Surface::SkillWizard {
                    project,
                    scope_list,
                    name,
                    description,
                    content,
                    step,
                });
            }
            Surface::Profiles {
                kind,
                rows,
                mut list,
            } => {
                list.reconcile(ListMode::Selectable, rows.len(), VISIBLE_MENU_ITEMS);
                let code = self.menu_navigation_code(key);
                if let Some(action) = list_action(code) {
                    list.apply(action, ListMode::Selectable, VISIBLE_MENU_ITEMS);
                } else {
                    match code {
                        KeyCode::Enter => {
                            if let Some((name, _, selectable)) =
                                list.selected.and_then(|selected| rows.get(selected))
                            {
                                if kind == ProfileKind::Agent && !selectable {
                                    self.surfaces.push(Surface::Profiles { kind, rows, list });
                                    return Ok(false);
                                }
                                if kind == ProfileKind::Agent && name == "Add" {
                                    let mut parents = vec!["None".into()];
                                    parents.extend(
                                        session
                                            .manageable_agent_profiles()?
                                            .into_iter()
                                            .map(|profile| profile.name),
                                    );
                                    let parent_list = ListState::selectable(parents.len());
                                    self.surfaces.push(Surface::AgentWizard {
                                        name: String::new(),
                                        description: String::new(),
                                        parent: String::new(),
                                        parents,
                                        parent_list,
                                        prompt: MultilineInput::new(String::new(), 0),
                                        availability: 0,
                                        step: AgentWizardStep::Name,
                                        cursor: 0,
                                        editing: false,
                                        original_name: None,
                                    });
                                    return Ok(false);
                                }
                                let action = match kind {
                                    ProfileKind::Agent => SessionAction::ChangeAgent {
                                        agent: name.clone(),
                                    },
                                    ProfileKind::Mode => {
                                        SessionAction::ChangeMode { mode: name.clone() }
                                    }
                                };
                                session.submit(SessionCommand::new(action)).await?;
                                self.surfaces.clear();
                                return Ok(false);
                            }
                        }
                        KeyCode::Char('e')
                            if key.modifiers.is_empty() && kind == ProfileKind::Agent =>
                        {
                            if let Some((name, _, _)) = list
                                .selected
                                .and_then(|selected| rows.get(selected))
                                .filter(|(name, _, _)| name != "Add")
                            {
                                self.surfaces.push(Surface::AgentEdit {
                                    name: name.clone(),
                                    list: agent_edit_list(0),
                                });
                                return Ok(false);
                            }
                        }
                        KeyCode::Char(' ') if key.modifiers.is_empty() => {
                            if let Some((name, _, selectable)) =
                                list.selected.and_then(|selected| rows.get(selected))
                            {
                                if kind == ProfileKind::Agent && !selectable {
                                    self.surfaces.push(Surface::Profiles { kind, rows, list });
                                    return Ok(false);
                                }
                                let action = match kind {
                                    ProfileKind::Agent => SessionAction::ChangeAgent {
                                        agent: name.clone(),
                                    },
                                    ProfileKind::Mode => {
                                        SessionAction::ChangeMode { mode: name.clone() }
                                    }
                                };
                                session.submit(SessionCommand::new(action)).await?;
                                self.surfaces.clear();
                                return Ok(false);
                            }
                        }
                        _ => {}
                    }
                }
                self.surfaces.push(Surface::Profiles { kind, rows, list });
            }
            Surface::AgentWizard {
                mut name,
                mut description,
                mut parent,
                parents,
                mut parent_list,
                mut prompt,
                mut availability,
                mut step,
                mut cursor,
                editing,
                original_name,
            } => {
                if step == AgentWizardStep::Parent {
                    parent_list.reconcile(ListMode::Selectable, parents.len(), VISIBLE_MENU_ITEMS);
                    let code = self.menu_navigation_code(key);
                    if let Some(action) = list_action(code) {
                        parent_list.apply(action, ListMode::Selectable, VISIBLE_MENU_ITEMS);
                    } else {
                        match code {
                            KeyCode::Enter => {
                                let selected = parent_list.selected.unwrap_or(0);
                                parent = if selected == 0 {
                                    String::new()
                                } else {
                                    parents[selected].clone()
                                };
                                if editing {
                                    let current = original_name.as_deref().unwrap_or(&name);
                                    let mut draft =
                                        session.agent_drafts()?.remove(current).unwrap_or_default();
                                    draft.extends =
                                        (!parent.trim().is_empty()).then(|| parent.trim().into());
                                    session.save_agent_draft(current, &draft)?;
                                    self.surfaces.push(Surface::AgentEdit {
                                        name,
                                        list: agent_edit_list(2),
                                    });
                                    return Ok(false);
                                }
                                step = AgentWizardStep::Prompt;
                                prompt.move_to_end();
                            }
                            _ => {}
                        }
                    }
                } else if step == AgentWizardStep::Prompt {
                    let prompt_newline = is_control_enter(key)
                        || matches!(key.code, KeyCode::Enter)
                            && key.modifiers.contains(KeyModifiers::SHIFT);
                    if prompt_newline {
                        prompt.insert_text_in_view("\n", multiline_width, multiline_capacity);
                    } else if submit_key {
                        if editing {
                            let current = original_name.as_deref().unwrap_or(&name);
                            let mut draft =
                                session.agent_drafts()?.remove(current).unwrap_or_default();
                            draft.prompt = (!prompt.is_empty()).then(|| prompt.text().to_owned());
                            draft.prompt_merge = Some(cagent_agent::config::PromptMerge::Append);
                            session.save_agent_draft(current, &draft)?;
                            self.surfaces.push(Surface::AgentEdit {
                                name,
                                list: agent_edit_list(3),
                            });
                            return Ok(false);
                        }
                        step = AgentWizardStep::Availability;
                        cursor = 0;
                    } else {
                        prompt.handle_key_in_view(key, multiline_width, multiline_capacity);
                    }
                } else if step == AgentWizardStep::Availability {
                    match self.menu_navigation_code(key) {
                        KeyCode::Up => availability = availability.saturating_sub(1),
                        KeyCode::Down => availability = (availability + 1).min(2),
                        KeyCode::PageUp | KeyCode::Home => availability = 0,
                        KeyCode::PageDown | KeyCode::End => availability = 2,
                        KeyCode::Enter => {
                            let mut draft =
                                session.agent_drafts()?.remove(&name).unwrap_or_default();
                            draft.extends =
                                (!parent.trim().is_empty()).then(|| parent.trim().into());
                            draft.description =
                                (!description.is_empty()).then_some(description.clone());
                            draft.prompt = (!prompt.is_empty()).then(|| prompt.text().to_owned());
                            draft.prompt_merge = Some(cagent_agent::config::PromptMerge::Append);
                            draft.availability = Some(match availability {
                                1 => cagent_agent::config::AgentAvailability::Subagent,
                                2 => cagent_agent::config::AgentAvailability::Both,
                                _ => cagent_agent::config::AgentAvailability::User,
                            });
                            match session.save_agent_draft(&name, &draft) {
                                Ok(()) => {
                                    if editing {
                                        self.surfaces.push(Surface::AgentEdit {
                                            name,
                                            list: agent_edit_list(4),
                                        });
                                    } else {
                                        self.surfaces.clear();
                                        self.open_agent_surface(session)?;
                                    }
                                }
                                Err(error) => {
                                    self.set_notice(format!("Agent update failed · {error}"));
                                    self.surfaces.push(Surface::AgentWizard {
                                        name,
                                        description,
                                        parent,
                                        parents,
                                        parent_list,
                                        prompt,
                                        availability,
                                        step,
                                        cursor,
                                        editing,
                                        original_name,
                                    });
                                }
                            }
                            return Ok(false);
                        }
                        _ => {}
                    }
                } else {
                    let editing_name = step == AgentWizardStep::Name;
                    let value = if editing_name {
                        name.clone()
                    } else {
                        description.clone()
                    };
                    let mut input = SingleLineInput::new(value, cursor);
                    if submit_key {
                        step = step.next();
                        cursor = step.input_cursor(&name, &description);
                    } else {
                        let _ = input.handle_key(key);
                    }
                    let (value, value_cursor) = input.into_parts();
                    if editing_name {
                        name = value;
                    } else {
                        description = value;
                    }
                    if submit_key && editing_name {
                        if name.trim().is_empty() {
                            self.set_notice("agent name required");
                            self.surfaces.push(Surface::AgentWizard {
                                name,
                                description,
                                parent,
                                parents,
                                parent_list,
                                prompt,
                                availability,
                                step: AgentWizardStep::Name,
                                cursor: value_cursor,
                                editing,
                                original_name,
                            });
                            return Ok(false);
                        }
                        let original = original_name.as_deref();
                        let taken = session
                            .manageable_agent_profiles()?
                            .into_iter()
                            .any(|profile| profile.name == name && original != Some(name.as_str()));
                        if taken {
                            self.set_notice("agent name taken");
                            self.surfaces.push(Surface::AgentWizard {
                                name,
                                description,
                                parent,
                                parents,
                                parent_list,
                                prompt,
                                availability,
                                step: AgentWizardStep::Name,
                                cursor: value_cursor,
                                editing,
                                original_name,
                            });
                            return Ok(false);
                        }
                    }
                    if submit_key && editing {
                        let current = original_name.as_deref().unwrap_or(&name);
                        if editing_name && current != name {
                            session.rename_agent_profile(current, &name)?;
                        } else {
                            let mut draft =
                                session.agent_drafts()?.remove(current).unwrap_or_default();
                            draft.description =
                                (!description.is_empty()).then_some(description.clone());
                            session.save_agent_draft(current, &draft)?;
                        }
                        self.surfaces.push(Surface::AgentEdit {
                            name: name.clone(),
                            list: agent_edit_list(step.index().saturating_sub(1)),
                        });
                        return Ok(false);
                    }
                    if !submit_key {
                        cursor = value_cursor;
                    }
                }
                self.surfaces.push(Surface::AgentWizard {
                    name,
                    description,
                    parent,
                    parents,
                    parent_list,
                    prompt,
                    availability,
                    step,
                    cursor,
                    editing,
                    original_name,
                });
            }
            Surface::AgentEdit { name, mut list } => {
                list.reconcile(ListMode::Selectable, 5, VISIBLE_MENU_ITEMS);
                let code = self.menu_navigation_code(key);
                if let Some(action) = list_action(code) {
                    list.apply(action, ListMode::Selectable, VISIBLE_MENU_ITEMS);
                } else {
                    match code {
                        KeyCode::Enter => {
                            let selected = list.selected.unwrap_or(0);
                            let draft = session.agent_drafts()?.remove(&name).unwrap_or_default();
                            let parent = draft.extends.unwrap_or_default();
                            let mut parents = vec!["None".into()];
                            parents.extend(
                                session
                                    .manageable_agent_profiles()?
                                    .into_iter()
                                    .filter(|profile| profile.name != name)
                                    .map(|profile| profile.name),
                            );
                            let parent_selected = parents
                                .iter()
                                .position(|entry| entry == &parent)
                                .unwrap_or(0);
                            let parent_list = ListState::selectable_at(
                                parents.len(),
                                parent_selected,
                                VISIBLE_MENU_ITEMS,
                            );
                            let original_name = name.clone();
                            let description = draft.description.unwrap_or_default();
                            let prompt = draft.prompt.unwrap_or_default();
                            let step = match selected {
                                0 => AgentWizardStep::Name,
                                1 => AgentWizardStep::Description,
                                2 => AgentWizardStep::Parent,
                                3 => AgentWizardStep::Prompt,
                                _ => AgentWizardStep::Availability,
                            };
                            let cursor = step.input_cursor(&name, &description);
                            let prompt_cursor = cursor_at_end(&prompt);
                            self.surfaces.push(Surface::AgentWizard {
                                name,
                                description,
                                parent,
                                parents,
                                parent_list,
                                prompt: MultilineInput::new(prompt, prompt_cursor),
                                availability: match draft.availability.unwrap_or_default() {
                                    cagent_agent::config::AgentAvailability::User => 0,
                                    cagent_agent::config::AgentAvailability::Subagent => 1,
                                    cagent_agent::config::AgentAvailability::Both => 2,
                                },
                                step,
                                cursor,
                                editing: true,
                                original_name: Some(original_name),
                            });
                            return Ok(false);
                        }
                        _ => {}
                    }
                }
                self.surfaces.push(Surface::AgentEdit { name, list });
            }
            Surface::Settings {
                mut rows,
                mut section,
                mut list,
                query,
                query_cursor,
            } => {
                let mut input = SingleLineInput::new(query, query_cursor);
                list.reconcile(
                    ListMode::Selectable,
                    filter_settings_rows(&rows, section, input.text()).len(),
                    settings_item_capacity(input.text()),
                );
                let code = self.menu_navigation_code(key);
                if let Some(action) = list_action(code) {
                    list.apply(
                        action,
                        ListMode::Selectable,
                        settings_item_capacity(input.text()),
                    );
                } else {
                    match code {
                        KeyCode::Left if input.text().is_empty() => {
                            let next = adjacent_setting_section(section, false);
                            change_settings_section(&rows, &mut section, &mut list, next);
                        }
                        KeyCode::Right if input.text().is_empty() => {
                            let next = adjacent_setting_section(section, true);
                            change_settings_section(&rows, &mut section, &mut list, next);
                        }
                        KeyCode::Char('r' | 'R')
                            if key.modifiers.contains(KeyModifiers::CONTROL) =>
                        {
                            if let Some(row) = filter_settings_rows(&rows, section, input.text())
                                .get(list.selected.unwrap_or(0))
                            {
                                match session.reset_setting(&row.definition.key) {
                                    Ok(()) => {
                                        rows = session.settings()?;
                                        let count =
                                            filter_settings_rows(&rows, section, input.text())
                                                .len();
                                        list.reconcile(
                                            ListMode::Selectable,
                                            count,
                                            settings_item_capacity(input.text()),
                                        );
                                    }
                                    Err(error) => self.set_notice(format!(
                                        "setting could not be reset · {}",
                                        concise_setting_error(&error)
                                    )),
                                }
                            }
                        }
                        KeyCode::Enter => {
                            let Some(row) = filter_settings_rows(&rows, section, input.text())
                                .get(list.selected.unwrap_or(0))
                                .copied()
                                .cloned()
                            else {
                                let (query, query_cursor) = input.into_parts();
                                self.surfaces.push(Surface::Settings {
                                    rows,
                                    section,
                                    list,
                                    query,
                                    query_cursor,
                                });
                                return Ok(false);
                            };
                            if row.definition.kind == cagent_agent::config::SettingKind::ModelPicker
                            {
                                let setting = row.definition;
                                let (query, query_cursor) = input.into_parts();
                                self.surfaces.push(Surface::Settings {
                                    rows,
                                    section,
                                    list,
                                    query,
                                    query_cursor,
                                });
                                self.open_model_surface_for_target(
                                    session,
                                    ModelSelectionTarget::Setting(setting),
                                )
                                .await?;
                                return Ok(false);
                            }
                            let editor = match &row.definition.kind {
                                cagent_agent::config::SettingKind::Boolean => {
                                    let description = row.definition.description.clone();
                                    let choices = vec![
                                        ("true".into(), description.clone()),
                                        ("false".into(), format!("do not {description}")),
                                    ];
                                    let choice_selected =
                                        usize::from(row.effective_value != "true");
                                    let list = ListState::selectable_at(
                                        choices.len(),
                                        choice_selected,
                                        VISIBLE_MENU_ITEMS,
                                    );
                                    Surface::SettingChoices {
                                        setting: row.definition,
                                        choices,
                                        list,
                                        custom_value: None,
                                    }
                                }
                                cagent_agent::config::SettingKind::List(choices) => {
                                    let choice_selected = choices
                                        .iter()
                                        .position(|(choice, _)| choice == &row.effective_value)
                                        .unwrap_or(0);
                                    let list = ListState::selectable_at(
                                        choices.len(),
                                        choice_selected,
                                        VISIBLE_MENU_ITEMS,
                                    );
                                    Surface::SettingChoices {
                                        setting: row.definition.clone(),
                                        choices: choices.clone(),
                                        list,
                                        custom_value: None,
                                    }
                                }
                                cagent_agent::config::SettingKind::TomlChoices(choices) => {
                                    let choice_selected = choices
                                        .iter()
                                        .position(|(choice, _)| choice == &row.effective_value)
                                        .unwrap_or(0);
                                    let list = ListState::selectable_at(
                                        choices.len(),
                                        choice_selected,
                                        VISIBLE_MENU_ITEMS,
                                    );
                                    Surface::SettingChoices {
                                        setting: row.definition.clone(),
                                        choices: choices.clone(),
                                        list,
                                        custom_value: Some(custom_setting_value(
                                            row.explicit_value,
                                        )),
                                    }
                                }
                                cagent_agent::config::SettingKind::DefaultAgentPicker(choices) => {
                                    let choice_selected = choices
                                        .iter()
                                        .position(|(choice, _)| choice == &row.effective_value)
                                        .unwrap_or(0);
                                    let list = ListState::selectable_at(
                                        choices.len(),
                                        choice_selected,
                                        VISIBLE_MENU_ITEMS,
                                    );
                                    Surface::SettingChoices {
                                        setting: row.definition.clone(),
                                        choices: choices.clone(),
                                        list,
                                        custom_value: None,
                                    }
                                }
                                _ => {
                                    let value = match &row.definition.kind {
                                        cagent_agent::config::SettingKind::TomlArray
                                        | cagent_agent::config::SettingKind::KeyBindings => {
                                            row.explicit_value.unwrap_or(row.effective_value)
                                        }
                                        _ if row.definition.key == "ui.composer_max_rows"
                                            && row.explicit_value.is_none() =>
                                        {
                                            String::new()
                                        }
                                        _ if row.definition.key == "ui.colors.background"
                                            && row.explicit_value.is_none() =>
                                        {
                                            String::new()
                                        }
                                        _ => row.effective_value,
                                    };
                                    let cursor = cursor_at_end(&value);
                                    Surface::SettingInput {
                                        setting: row.definition,
                                        value,
                                        cursor,
                                    }
                                }
                            };
                            let (query, query_cursor) = input.into_parts();
                            self.surfaces.push(Surface::Settings {
                                rows,
                                section,
                                list,
                                query,
                                query_cursor,
                            });
                            self.surfaces.push(editor);
                            return Ok(false);
                        }
                        _ if !matches!(key.code, KeyCode::Char(character) if character.is_whitespace())
                            && input.handle_key(key) =>
                        {
                            list.reset(
                                ListMode::Selectable,
                                filter_settings_rows(&rows, section, input.text()).len(),
                            );
                        }
                        _ => {}
                    }
                }
                let (query, query_cursor) = input.into_parts();
                self.surfaces.push(Surface::Settings {
                    rows,
                    section,
                    list,
                    query,
                    query_cursor,
                });
            }
            Surface::SettingChoices {
                setting,
                choices,
                mut list,
                custom_value,
            } => {
                list.reconcile(ListMode::Selectable, choices.len(), VISIBLE_MENU_ITEMS);
                let code = self.menu_navigation_code(key);
                if let Some(action) = list_action(code) {
                    list.apply(action, ListMode::Selectable, VISIBLE_MENU_ITEMS);
                } else {
                    match code {
                        KeyCode::Enter => {
                            let selected = list.selected.unwrap_or(0);
                            let Some((value, _)) = choices.get(selected) else {
                                return Ok(false);
                            };
                            if matches!(
                                &setting.kind,
                                cagent_agent::config::SettingKind::TomlChoices(_)
                            ) && value == "custom"
                            {
                                let value = custom_value.unwrap_or_else(|| "[]".into());
                                let cursor = cursor_at_end(&value);
                                self.surfaces.push(Surface::SettingInput {
                                    setting,
                                    value,
                                    cursor,
                                });
                                return Ok(false);
                            }
                            let raw = if matches!(
                                &setting.kind,
                                cagent_agent::config::SettingKind::List(_)
                            ) {
                                serde_json::to_string(value).expect("a string always serializes")
                            } else {
                                value.clone()
                            };
                            match session.save_setting(&setting.key, &raw) {
                                Ok(()) => {
                                    if let Some(Surface::Settings {
                                        rows,
                                        list,
                                        section,
                                        query,
                                        ..
                                    }) = self.surfaces.last_mut()
                                    {
                                        *rows = session.settings()?;
                                        let count =
                                            filter_settings_rows(rows, *section, query).len();
                                        list.reconcile(
                                            ListMode::Selectable,
                                            count,
                                            settings_item_capacity(query),
                                        );
                                    }
                                    return Ok(false);
                                }
                                Err(error) => self.set_notice(format!(
                                    "invalid setting · {}",
                                    concise_setting_error(&error)
                                )),
                            }
                        }
                        _ => {}
                    }
                }
                self.surfaces.push(Surface::SettingChoices {
                    setting,
                    choices,
                    list,
                    custom_value,
                });
            }
            Surface::SettingInput {
                setting,
                value,
                cursor,
            } => {
                let mut input = SingleLineInput::new(value, cursor);
                if submit_key {
                    if input.text().trim().is_empty() {
                        match session.reset_setting(&setting.key) {
                            Ok(()) => {
                                if let Some(Surface::Settings {
                                    rows,
                                    list,
                                    section,
                                    query,
                                    ..
                                }) = self.surfaces.last_mut()
                                {
                                    *rows = session.settings()?;
                                    let count = filter_settings_rows(rows, *section, query).len();
                                    list.reconcile(
                                        ListMode::Selectable,
                                        count,
                                        settings_item_capacity(query),
                                    );
                                }
                                return Ok(false);
                            }
                            Err(error) => {
                                self.set_notice(format!(
                                    "setting could not be reset · {}",
                                    concise_setting_error(&error)
                                ));
                                let (value, cursor) = input.into_parts();
                                self.surfaces.push(Surface::SettingInput {
                                    setting,
                                    value,
                                    cursor,
                                });
                                return Ok(false);
                            }
                        }
                    }
                    let raw = match &setting.kind {
                        cagent_agent::config::SettingKind::String => {
                            serde_json::to_string(input.text()).expect("a string always serializes")
                        }
                        _ => input.text().to_owned(),
                    };
                    match session.save_setting(&setting.key, &raw) {
                        Ok(()) => {
                            if let Some(Surface::Settings {
                                rows,
                                list,
                                section,
                                query,
                                ..
                            }) = self.surfaces.last_mut()
                            {
                                *rows = session.settings()?;
                                let count = filter_settings_rows(rows, *section, query).len();
                                list.reconcile(
                                    ListMode::Selectable,
                                    count,
                                    settings_item_capacity(query),
                                );
                            }
                            return Ok(false);
                        }
                        Err(error) => self.set_notice(format!(
                            "invalid setting · {}",
                            concise_setting_error(&error)
                        )),
                    }
                } else if !matches!(
                    &setting.kind,
                    cagent_agent::config::SettingKind::PositiveInteger
                        | cagent_agent::config::SettingKind::NonNegativeInteger
                ) || !(key.modifiers.is_empty()
                    && matches!(key.code, KeyCode::Char(character) if !character.is_ascii_digit()))
                {
                    let _ = input.handle_key(key);
                }
                let (value, cursor) = input.into_parts();
                self.surfaces.push(Surface::SettingInput {
                    setting,
                    value,
                    cursor,
                });
            }
            Surface::StatusLine {
                mut config,
                mut rows,
                mut list,
                mut mode,
                preview,
            } => {
                list.reconcile(ListMode::Selectable, rows.len(), VISIBLE_MENU_ITEMS);
                match &mut mode {
                    StatusLineEditorMode::Modules => {
                        let selected_module = list
                            .selected
                            .and_then(|selected| rows.get(selected))
                            .copied();
                        let code = self.menu_navigation_code(key);
                        if let Some(action) = list_action(code) {
                            list.apply(action, ListMode::Selectable, VISIBLE_MENU_ITEMS);
                        } else {
                            match code {
                                KeyCode::Left => {
                                    if let Some(module) = selected_module {
                                        move_status_line_module(
                                            &mut config,
                                            &mut rows,
                                            module,
                                            true,
                                        );
                                        let selected = rows
                                            .iter()
                                            .position(|candidate| *candidate == module)
                                            .unwrap_or(0);
                                        list.select(selected, VISIBLE_MENU_ITEMS);
                                    }
                                }
                                KeyCode::Right => {
                                    if let Some(module) = selected_module {
                                        move_status_line_module(
                                            &mut config,
                                            &mut rows,
                                            module,
                                            false,
                                        );
                                        let selected = rows
                                            .iter()
                                            .position(|candidate| *candidate == module)
                                            .unwrap_or(0);
                                        list.select(selected, VISIBLE_MENU_ITEMS);
                                    }
                                }
                                KeyCode::Enter if key.modifiers.is_empty() => {
                                    if let Some(module) = selected_module {
                                        let enabled = !config.modules.contains(&module);
                                        set_status_line_module_enabled(
                                            &mut config,
                                            &mut rows,
                                            module,
                                            enabled,
                                        );
                                        let selected = rows
                                            .iter()
                                            .position(|candidate| *candidate == module)
                                            .unwrap_or(0);
                                        list.select(selected, VISIBLE_MENU_ITEMS);
                                    }
                                }
                                KeyCode::Char('c') if key.modifiers.is_empty() => {
                                    if let Some(module) = selected_module
                                        .filter(|module| status_line_module_color_editable(*module))
                                    {
                                        mode = StatusLineEditorMode::Colors {
                                            module,
                                            list: ListState::selectable_at(
                                                status_line_color_choices().len(),
                                                status_line_color_choice_index(
                                                    module,
                                                    config.color(module),
                                                ),
                                                VISIBLE_MENU_ITEMS,
                                            ),
                                        };
                                    }
                                }
                                KeyCode::Char('r') if key.modifiers.is_empty() => {
                                    if let Some(module) = selected_module {
                                        reset_status_line_module(&mut config, &mut rows, module);
                                        let selected = rows
                                            .iter()
                                            .position(|candidate| *candidate == module)
                                            .unwrap_or(0);
                                        list.select(selected, VISIBLE_MENU_ITEMS);
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    StatusLineEditorMode::Colors {
                        module,
                        list: color_list,
                    } => {
                        let choices = status_line_color_choices();
                        color_list.reconcile(
                            ListMode::Selectable,
                            choices.len(),
                            VISIBLE_MENU_ITEMS,
                        );
                        let code = self.menu_navigation_code(key);
                        if let Some(action) = list_action(code) {
                            color_list.apply(action, ListMode::Selectable, VISIBLE_MENU_ITEMS);
                        } else {
                            match code {
                                KeyCode::Enter if key.modifiers.is_empty() => {
                                    match color_list
                                        .selected
                                        .and_then(|selected| choices.get(selected))
                                        .copied()
                                    {
                                        Some(StatusLineColorChoice::Default) => {
                                            config.colors.insert(*module, module.default_color());
                                            mode = StatusLineEditorMode::Modules;
                                        }
                                        Some(StatusLineColorChoice::Named(color)) => {
                                            config.colors.insert(*module, color);
                                            mode = StatusLineEditorMode::Modules;
                                        }
                                        Some(StatusLineColorChoice::Custom) => {
                                            let input = match config.color(*module) {
                                                StatusLineColor::Rgb(red, green, blue) => {
                                                    format!("#{red:02X}{green:02X}{blue:02X}")
                                                }
                                                _ => "#".into(),
                                            };
                                            let cursor = cursor_at_end(&input);
                                            mode = StatusLineEditorMode::Hex {
                                                module: *module,
                                                input,
                                                cursor,
                                            };
                                        }
                                        None => {}
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    StatusLineEditorMode::Hex {
                        module,
                        input,
                        cursor,
                    } => {
                        let mut editor = SingleLineInput::new(input.clone(), *cursor);
                        if key.code == KeyCode::Enter && key.modifiers.is_empty() {
                            match editor.text().parse::<StatusLineColor>() {
                                Ok(color @ StatusLineColor::Rgb(..)) => {
                                    config.colors.insert(*module, color);
                                    mode = StatusLineEditorMode::Modules;
                                }
                                Ok(_) | Err(_) => {
                                    self.set_notice("enter a color as #RRGGBB");
                                }
                            }
                        } else if editor.handle_key(key) {
                            (*input, *cursor) = editor.into_parts();
                        }
                    }
                }
                self.surfaces.push(Surface::StatusLine {
                    config,
                    rows,
                    list,
                    mode,
                    preview,
                });
            }
            Surface::SupervisedWork {
                rows,
                mut list,
                mut show_past,
            } => {
                if key.code == KeyCode::Up && key.modifiers == KeyModifiers::ALT {
                    return Ok(false);
                }
                let visible_count = rows
                    .iter()
                    .filter(|item| item.active() != show_past)
                    .count();
                list.reconcile(ListMode::Selectable, visible_count, VISIBLE_MENU_ITEMS);
                let code = self.menu_navigation_code(key);
                if code == KeyCode::Up && list.selected == Some(0) {
                    return Ok(false);
                }
                if matches!(key.code, KeyCode::Char('k'))
                    && key.modifiers.is_empty()
                    && !show_past
                    && !self.is_observer()
                {
                    let target = rows
                        .iter()
                        .filter(|item| item.active())
                        .nth(list.selected.unwrap_or(0))
                        .map(|item| match item {
                            cagent_agent::presentation::SupervisedWork::Agent { run } => {
                                cagent_agent::runtime::SupervisedWorkTarget::Agent(run.id)
                            }
                            cagent_agent::presentation::SupervisedWork::Terminal { terminal } => {
                                cagent_agent::runtime::SupervisedWorkTarget::Terminal(terminal.id)
                            }
                        });
                    if let Some(target) = target {
                        self.surfaces.push(Surface::SupervisedWork {
                            rows,
                            list,
                            show_past,
                        });
                        self.surfaces.push(Surface::KillSupervisedWork {
                            target,
                            selected: 0,
                        });
                        return Ok(false);
                    }
                }
                if let Some(action) = list_action(code) {
                    list.apply(action, ListMode::Selectable, VISIBLE_MENU_ITEMS);
                } else {
                    match code {
                        KeyCode::Char('p') => {
                            show_past = !show_past;
                            let count = rows
                                .iter()
                                .filter(|item| item.active() != show_past)
                                .count();
                            list.reset(ListMode::Selectable, count);
                        }
                        KeyCode::Enter => {
                            if let Some(item) = rows
                                .iter()
                                .filter(|item| item.active() != show_past)
                                .nth(list.selected.unwrap_or(0))
                            {
                                let detail = match item {
                                    cagent_agent::presentation::SupervisedWork::Terminal {
                                        terminal,
                                    } => self
                                        .terminal_detail_with_session(
                                            session,
                                            Some(terminal.id),
                                            terminal.command.clone(),
                                            String::new(),
                                            String::new(),
                                            None,
                                        )
                                        .await?
                                        .into_surface(self.render_width, self.render_height),
                                    cagent_agent::presentation::SupervisedWork::Agent { run } => {
                                        let live = self.supervised_work.agent_run_live.get(&run.id);
                                        let mut detail_run = run.clone();
                                        if let Some(usage) =
                                            live.and_then(|live| live.usage.clone())
                                        {
                                            detail_run.usage = Some(usage);
                                        }
                                        Surface::Expanded {
                                            view: ExpandedView::AgentLog {
                                                run: detail_run,
                                                streaming: live
                                                    .map(|live| live.text.clone())
                                                    .unwrap_or_default(),
                                                terminals: self
                                                    .supervised_work
                                                    .terminals_for_agent(run),
                                                expanded_explorations: Default::default(),
                                                expanded_activity_runs: Default::default(),
                                                collapse_tool_activity: self.collapse_tool_activity,
                                                max_scroll: Cell::new(None),
                                            },
                                            scroll: 0,
                                            viewport_rows: usize::from(
                                                self.render_height.saturating_sub(6).max(1),
                                            ),
                                        }
                                    }
                                };
                                self.surfaces.push(Surface::SupervisedWork {
                                    rows,
                                    list,
                                    show_past,
                                });
                                self.surfaces.push(detail);
                                return Ok(false);
                            }
                        }
                        _ => {}
                    }
                }
                self.surfaces.push(Surface::SupervisedWork {
                    rows,
                    list,
                    show_past,
                });
            }
            Surface::KillSupervisedWork {
                target,
                mut selected,
            } => {
                match self.menu_navigation_code(key) {
                    KeyCode::Up => selected = selected.saturating_sub(1),
                    KeyCode::Down => selected = (selected + 1).min(1),
                    KeyCode::PageUp | KeyCode::Home => selected = 0,
                    KeyCode::PageDown | KeyCode::End => selected = 1,
                    KeyCode::Enter if selected == 1 => return Ok(false),
                    KeyCode::Enter => {
                        if let Err(error) = session.stop_supervised_work(target).await {
                            self.set_notice(format!("could not kill background work · {error}"));
                            self.surfaces
                                .push(Surface::KillSupervisedWork { target, selected });
                        } else if self.surfaces.last().is_some_and(|surface| {
                            matches!(surface, Surface::SupervisedWork { rows, .. }
                                if !background_work_remains_after_kill(rows, target))
                        }) {
                            self.surfaces.pop();
                        }
                        return Ok(false);
                    }
                    _ => {}
                }
                self.surfaces
                    .push(Surface::KillSupervisedWork { target, selected });
            }
            Surface::Expanded {
                view,
                mut scroll,
                viewport_rows,
            } => {
                if let ExpandedView::Directory { mut browser } = view {
                    let code = self.menu_navigation_code(key);
                    let open = browser.apply_key(code, viewport_rows);
                    super::file_tree::reconcile_directory_scroll(
                        &browser,
                        &mut scroll,
                        viewport_rows,
                    );
                    self.expanded_text_render_cache.get_mut().take();
                    self.surfaces.push(Surface::Expanded {
                        view: ExpandedView::Directory { browser },
                        scroll,
                        viewport_rows,
                    });
                    if let Some(path) = open {
                        self.open_path(path);
                    }
                    return Ok(false);
                }
                let (_, _, composer_height, _) =
                    self.control_heights_within(self.render_width, self.render_height);
                let panel_rows = if view.fills_bottom_row() {
                    composer_height
                } else {
                    composer_height.saturating_sub(1)
                };
                let (capacity, last) = if let ExpandedView::Terminal {
                    ansi_output,
                    completion,
                    ..
                } = &view
                {
                    (
                        viewport_rows,
                        self.cached_terminal_scrollback_max(
                            ansi_output,
                            completion.as_deref(),
                            self.render_width.saturating_sub(4),
                            u16::try_from(viewport_rows).unwrap_or(u16::MAX),
                        )
                        .unwrap_or_else(|| {
                            expanded_scroll_metrics(
                                &view,
                                viewport_rows,
                                &self.workspace,
                                self.render_width,
                                usize::from(panel_rows),
                            )
                            .1
                        }),
                    )
                } else {
                    cached_expanded_scroll_metrics(
                        &self.expanded_text_render_cache,
                        &view,
                        viewport_rows,
                        &self.workspace,
                        self.render_width,
                        usize::from(panel_rows),
                    )
                };
                let mut state = ScrollViewState {
                    offset: scroll.min(last),
                    content_rows: last.saturating_add(capacity),
                };
                if let Some(action) = list_action(self.menu_navigation_code(key)) {
                    state.apply(action.into(), capacity);
                }
                scroll = state.offset;
                self.surfaces.push(Surface::Expanded {
                    view,
                    scroll,
                    viewport_rows,
                });
            }
            Surface::Paths {
                rows,
                mut list,
                token_start,
            } => {
                list.reconcile(ListMode::Selectable, rows.len(), VISIBLE_MENU_ITEMS);
                let code = self.menu_navigation_code(key);
                if let Some(action) = list_action(code) {
                    list.apply(action, ListMode::Selectable, VISIBLE_MENU_ITEMS);
                } else if code == KeyCode::Enter {
                    if let Some(entry) = list
                        .selected
                        .and_then(|selected| rows.get(selected))
                        .cloned()
                    {
                        self.apply_path_completion(token_start, &entry);
                    }
                    return Ok(false);
                }
                self.surfaces.push(Surface::Paths {
                    rows,
                    list,
                    token_start,
                });
            }
            Surface::HistoryTree {
                rows,
                mut list,
                purpose,
                loading,
                revision,
                query,
                query_cursor,
                opened_at_millis,
            } => {
                let mut input = SingleLineInput::new(query, query_cursor);
                let visible = history_picker_indexes(&rows, input.text());
                list.reconcile(ListMode::Selectable, visible.len(), history_capacity);
                let selected_source = list
                    .selected
                    .and_then(|position| visible.get(position))
                    .copied();
                if self.matches_action(
                    cagent_agent::presentation::KeyBindingAction::ScrollToMessage,
                    key,
                ) {
                    let target = selected_source
                        .and_then(|source| rows.get(source))
                        .and_then(|row| row.transcript_target.clone());
                    if let Some(target) = target {
                        self.begin_scroll_to_transcript_block(target);
                        return Ok(false);
                    }
                    let (query, query_cursor) = input.into_parts();
                    self.surfaces.push(Surface::HistoryTree {
                        rows,
                        list,
                        purpose,
                        loading,
                        revision,
                        query,
                        query_cursor,
                        opened_at_millis,
                    });
                    return Ok(false);
                }
                if key.modifiers.is_empty() && key.code == KeyCode::Char('p') {
                    if let Some(target) = selected_source
                        .and_then(|source| rows.get(source))
                        .filter(|row| row.selectable)
                        .map(cagent_agent::presentation::history_fork_target)
                    {
                        self.surfaces.push(Surface::HistoryTree {
                            rows,
                            list,
                            purpose,
                            loading,
                            revision,
                            query: input.text().to_owned(),
                            query_cursor: input.cursor(),
                            opened_at_millis,
                        });
                        self.open_history_preview(session, target).await?;
                        return Ok(false);
                    }
                }
                if key.modifiers == KeyModifiers::SHIFT
                    && matches!(key.code, KeyCode::Char('F') | KeyCode::Char('f'))
                {
                    if let Some(row) = selected_source
                        .and_then(|source| rows.get(source))
                        .filter(|row| row.selectable)
                    {
                        self.pending_fork_draft = row
                            .user_draft
                            .clone()
                            .map(|draft| (draft.text, draft.attachment_specs));
                        self.pending_action = Some(AppAction::HardFork(
                            cagent_agent::presentation::history_fork_target(row),
                        ));
                        return Ok(false);
                    }
                }
                let code = self.menu_navigation_code(key);
                if let Some(action) = list_action(code) {
                    apply_history_list_action(&mut list, action, &rows, &visible, history_capacity);
                } else {
                    match code {
                        KeyCode::Enter => {
                            if let Some(row) = list
                                .selected
                                .and_then(|position| visible.get(position))
                                .and_then(|source| rows.get(*source))
                                .filter(|row| row.selectable)
                            {
                                self.pending_fork_draft = row
                                    .user_draft
                                    .clone()
                                    .map(|draft| (draft.text, draft.attachment_specs));
                                // The runtime silently cancels any active work before committing
                                // the branch, so it cannot outlive the new branch point.
                                self.hide_working_indicator = true;
                                self.pending_interaction = None;
                                session
                                    .submit(SessionCommand::new(SessionAction::Fork {
                                        at: cagent_agent::presentation::history_fork_target(row),
                                    }))
                                    .await?;
                                self.pending_action = Some(AppAction::RefreshFork);
                                return Ok(false);
                            }
                        }
                        _ if input.handle_key(key) => {
                            let next_visible = history_picker_indexes(&rows, input.text());
                            let selected = selected_source
                                .and_then(|source| {
                                    next_visible
                                        .iter()
                                        .position(|candidate| *candidate == source)
                                })
                                .filter(|position| rows[next_visible[*position]].selectable)
                                .or_else(|| {
                                    next_visible
                                        .iter()
                                        .position(|source| rows[*source].selectable)
                                })
                                .unwrap_or(0);
                            list = ListState::selectable_at(
                                next_visible.len(),
                                selected,
                                history_capacity,
                            );
                        }
                        _ => {}
                    }
                }
                let (query, query_cursor) = input.into_parts();
                self.surfaces.push(Surface::HistoryTree {
                    rows,
                    list,
                    purpose,
                    loading,
                    revision,
                    query,
                    query_cursor,
                    opened_at_millis,
                });
            }
            Surface::Conversations {
                mut rows,
                mut list,
                query,
                query_cursor,
                opened_at_millis,
                mut include_archived,
            } => {
                let mut input = SingleLineInput::new(query, query_cursor);
                let visible = conversation_picker_indexes(&rows, input.text());
                let conversation_capacity = super::surfaces::conversation_picker_item_capacity(
                    usize::from(composer_height),
                    include_archived,
                    !input.text().is_empty() && visible.is_empty(),
                );
                list.reconcile(ListMode::Selectable, visible.len(), conversation_capacity);
                let code = self.menu_navigation_code(key);
                if let Some(action) = list_action(code) {
                    list.apply(action, ListMode::Selectable, conversation_capacity);
                } else {
                    match code {
                        KeyCode::Enter => {
                            if let Some(conversation) = list
                                .selected
                                .and_then(|position| visible.get(position))
                                .and_then(|source| rows.get(*source))
                            {
                                self.pending_action =
                                    Some(AppAction::Resume(Some(conversation.id)));
                                return Ok(false);
                            }
                        }
                        KeyCode::Char('a') => {
                            include_archived = !include_archived;
                            rows = session
                                .query_conversations_with_archived(
                                    Some(input.text().to_owned()),
                                    include_archived,
                                )
                                .await?;
                            list.reset(
                                ListMode::Selectable,
                                conversation_picker_indexes(&rows, "").len(),
                            );
                        }
                        KeyCode::Char('f') if key.modifiers.is_empty() => {
                            if let Some(conversation) = list
                                .selected
                                .and_then(|position| visible.get(position))
                                .and_then(|source| rows.get(*source))
                            {
                                let id = conversation.id;
                                let favourite = !conversation.favourite;
                                session.set_conversation_favourite(id, favourite).await?;
                                rows = session
                                    .query_conversations_with_archived(
                                        Some(input.text().to_owned()),
                                        include_archived,
                                    )
                                    .await?;
                                let selected = conversation_picker_indexes(&rows, "")
                                    .iter()
                                    .position(|source| rows[*source].id == id)
                                    .unwrap_or(0);
                                list.select(selected, conversation_capacity);
                            }
                        }
                        KeyCode::Char('A') => {
                            if let Some(conversation) = list
                                .selected
                                .and_then(|position| visible.get(position))
                                .and_then(|source| rows.get(*source))
                            {
                                session
                                    .set_conversation_archived(conversation.id, true)
                                    .await?;
                                rows = session
                                    .query_conversations_with_archived(
                                        Some(input.text().to_owned()),
                                        include_archived,
                                    )
                                    .await?;
                                list.reconcile(
                                    ListMode::Selectable,
                                    conversation_picker_indexes(&rows, "").len(),
                                    super::surfaces::conversation_picker_item_capacity(
                                        usize::from(composer_height),
                                        include_archived,
                                        false,
                                    ),
                                );
                            }
                        }
                        KeyCode::Char('D') => {
                            if let Some(conversation) = list
                                .selected
                                .and_then(|position| visible.get(position))
                                .and_then(|source| rows.get(*source))
                            {
                                let id = conversation.id;
                                session.delete_conversation(id).await?;
                                if id == session.id() {
                                    self.pending_action = Some(AppAction::Quit);
                                    return Ok(false);
                                }
                                rows = session
                                    .query_conversations_with_archived(
                                        Some(input.text().to_owned()),
                                        include_archived,
                                    )
                                    .await?;
                                list.reconcile(
                                    ListMode::Selectable,
                                    conversation_picker_indexes(&rows, "").len(),
                                    super::surfaces::conversation_picker_item_capacity(
                                        usize::from(composer_height),
                                        include_archived,
                                        false,
                                    ),
                                );
                            }
                        }
                        _ if input.handle_key(key) => {
                            rows = session
                                .query_conversations_with_archived(
                                    Some(input.text().to_owned()),
                                    include_archived,
                                )
                                .await?;
                            list.reset(
                                ListMode::Selectable,
                                conversation_picker_indexes(&rows, "").len(),
                            );
                        }
                        _ => {}
                    }
                }
                let (query, query_cursor) = input.into_parts();
                self.surfaces.push(Surface::Conversations {
                    rows,
                    list,
                    query,
                    query_cursor,
                    opened_at_millis,
                    include_archived,
                });
            }
            Surface::Help {
                mut tab,
                command_rows,
                key_rows,
                mut list,
            } => {
                let item_count = match tab {
                    HelpTab::Commands => command_rows.len(),
                    HelpTab::Keybindings => key_rows.len(),
                };
                list.reconcile(item_count, VISIBLE_HELP_ITEMS, false);
                let code = self.menu_navigation_code(key);
                if let Some(action) = list_action(code) {
                    list.apply(action.into(), VISIBLE_HELP_ITEMS);
                } else {
                    match code {
                        KeyCode::Left => {
                            let next = tab.previous();
                            change_help_tab(&mut tab, &command_rows, &key_rows, &mut list, next);
                        }
                        KeyCode::Right => {
                            let next = tab.next();
                            change_help_tab(&mut tab, &command_rows, &key_rows, &mut list, next);
                        }
                        _ => {}
                    }
                }
                self.surfaces.push(Surface::Help {
                    tab,
                    command_rows,
                    key_rows,
                    list,
                });
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod mcp_tests {
    use super::*;

    fn draft() -> McpFormDraft {
        McpFormDraft {
            original: None,
            location: cagent_agent::mcp::McpLocation::global(),
            name: "docs".into(),
            definition: mcp_definition_template(1),
        }
    }

    #[test]
    fn structured_fields_parse_nested_lists_and_maps() {
        let mut draft = draft();
        apply_mcp_form_field(
            &mut draft,
            McpFormField::Arguments,
            r#"["serve","--root", "${workspace}"]"#,
        )
        .unwrap();
        apply_mcp_form_field(
            &mut draft,
            McpFormField::Environment,
            r#"{"TOKEN":"${env:TOKEN}"}"#,
        )
        .unwrap();
        apply_mcp_form_field(
            &mut draft,
            McpFormField::ReadOnlyTools,
            r#"["search","read"]"#,
        )
        .unwrap();

        let cagent_agent::mcp::McpTransportConfig::Stdio { args, env, .. } =
            &draft.definition.transport
        else {
            panic!("expected stdio draft");
        };
        assert_eq!(args, &["serve", "--root", "${workspace}"]);
        assert_eq!(env["TOKEN"].value(), "${env:TOKEN}");
        assert_eq!(draft.definition.read_only_tools, ["search", "read"]);
    }

    #[test]
    fn rename_and_scope_edit_build_one_atomic_move_and_put_preview() {
        let mut draft = draft();
        draft.original = Some((cagent_agent::mcp::McpLocation::global(), "docs".into()));
        draft.location = cagent_agent::mcp::McpLocation::agent("review");
        draft.name = "review-docs".into();
        let mutations = mcp_form_mutations(&draft);
        assert!(matches!(
            &mutations[0],
            cagent_agent::mcp::McpMutation::Move { name, new_name, .. }
                if name == "docs" && new_name == "review-docs"
        ));
        assert!(matches!(
            &mutations[1],
            cagent_agent::mcp::McpMutation::Put { location, name, .. }
                if location == &cagent_agent::mcp::McpLocation::agent("review")
                    && name == "review-docs"
        ));
    }

    #[test]
    fn resume_picker_excludes_untitled_conversations() {
        let conversation = |title: &str| cagent_agent::protocol::ConversationSummary {
            id: cagent_agent::protocol::ConversationId::new(),
            workspace: std::path::PathBuf::from("/tmp/project"),
            title: title.into(),
            created_at: "0".into(),
            updated_at: "0".into(),
            active_node_id: cagent_agent::protocol::NodeId::new(),
            status: "idle".into(),
            agent: "default".into(),
            mode: "ask".into(),
            model: None,
            message_count: 0,
            archived: false,
            favourite: false,
            preview: String::new(),
        };
        let rows = vec![conversation(""), conversation("Useful title")];
        assert_eq!(conversation_picker_indexes(&rows, ""), vec![1]);
        assert_eq!(conversation_picker_indexes(&rows, "useful"), vec![1]);
        let visible = conversation_picker_indexes(&rows, "");
        let list = ListState::selectable(visible.len());
        let selected_source = list.selected.and_then(|position| visible.get(position));
        assert_eq!(
            selected_source
                .and_then(|source| rows.get(*source))
                .unwrap()
                .title,
            "Useful title"
        );
    }
}

#[cfg(test)]
mod settings_tests {
    use super::*;
    use crate::app::surfaces::setting_input_description;

    #[test]
    fn custom_editors_start_empty_for_presets_and_restore_commands() {
        assert_eq!(custom_setting_value(None), "[]");
        assert_eq!(custom_setting_value(Some("triangles".into())), "[]");
        assert_eq!(custom_setting_value(Some("braille".into())), "[]");
        assert_eq!(
            custom_setting_value(Some("[\"!\", \" \"]".into())),
            "[\"!\", \" \"]"
        );
        assert_eq!(
            custom_setting_value(Some(
                "{ command = [\"nvim\"], mode = \"foreground\" }".into()
            )),
            "{ command = [\"nvim\"], mode = \"foreground\" }"
        );
    }

    #[test]
    fn setting_input_descriptions_explain_type_and_default() {
        let requested_input = cagent_agent::config::SettingDefinition {
            key: "ui.title.requested_input".into(),
            label: String::new(),
            description: String::new(),
            kind: cagent_agent::config::SettingKind::String,
            section: cagent_agent::config::SettingSection::Ui,
            default_value: "warning".into(),
        };
        assert_eq!(
            setting_input_description(&requested_input),
            "JSON-compatible array of strings to show before the title, e.g. [\"!\", \" \"] (default: warning)"
        );

        let progress = cagent_agent::config::SettingDefinition {
            key: "ui.title.progress".into(),
            default_value: "false".into(),
            ..requested_input.clone()
        };
        assert_eq!(
            setting_input_description(&progress),
            "JSON-compatible array of strings to cycle in the title, e.g. [\".\", \"..\", \"...\"] (default: false)"
        );

        let number = cagent_agent::config::SettingDefinition {
            key: "subagents.max_concurrent".into(),
            description: "maximum active sub-agents".into(),
            kind: cagent_agent::config::SettingKind::NonNegativeInteger,
            default_value: "10".into(),
            ..requested_input
        };
        assert_eq!(
            setting_input_description(&number),
            "maximum active sub-agents (int, default: 10)"
        );

        let editor = cagent_agent::config::SettingDefinition {
            key: "ui.editor".into(),
            kind: cagent_agent::config::SettingKind::TomlChoices(vec![(
                "custom".into(),
                "custom command".into(),
            )]),
            ..progress
        };
        assert_eq!(
            setting_input_description(&editor),
            "TOML command array (foreground) or { command = [...], mode = \"foreground\" | \"background\" } (default: false)"
        );
    }
}
