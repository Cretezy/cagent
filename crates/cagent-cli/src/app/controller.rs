#[allow(clippy::wildcard_imports)]
use super::*;

const DIRECTORY_LOAD_MAX_CONCURRENT: usize = 4;
const UI_RESULT_CHANNEL_CAPACITY: usize = 32;

struct TranscriptPageResult {
    cursor: cagent_agent::protocol::TranscriptCursor,
    result: Result<cagent_agent::protocol::TranscriptPage, cagent_agent::protocol::RuntimeError>,
}

struct HistoryLoadResult {
    generation: u64,
    conversation_id: cagent_agent::protocol::ConversationId,
    purpose: TreePurpose,
    result: Result<
        cagent_agent::presentation::HistoryRowsSnapshot,
        cagent_agent::protocol::RuntimeError,
    >,
}

struct SubmissionResult {
    draft: cagent_agent::protocol::UserDraft,
    optimistic_queue_id: Option<cagent_agent::protocol::QueuedMessageId>,
    result: Result<cagent_agent::protocol::CommandId, cagent_agent::protocol::RuntimeError>,
}

struct SubmissionRequest {
    session: SessionHandle,
    submission: DeferredSubmission,
}

fn schedule_history_load(
    session: &SessionHandle,
    purpose: TreePurpose,
    generation: u64,
    sender: &tokio::sync::mpsc::Sender<HistoryLoadResult>,
) {
    let history_session = session.clone();
    let conversation_id = session.id();
    let sender = sender.clone();
    tokio::spawn(
        async move {
            let result = if purpose == TreePurpose::Browse {
                history_session.tree_history_snapshot().await
            } else {
                history_session.active_branch_history_snapshot().await
            };
            let _ = sender
                .send(HistoryLoadResult {
                    generation,
                    conversation_id,
                    purpose,
                    result,
                })
                .await;
        }
        .instrument(tracing::trace_span!("frontend.history.load")),
    );
}

fn pending_interaction_id(
    snapshot: &cagent_agent::protocol::SessionSnapshot,
) -> Option<cagent_agent::protocol::InteractionRequestId> {
    snapshot
        .pending_interaction
        .as_ref()
        .map(|request| request.id)
}

pub(super) async fn end_session_before_transition(
    app: &App,
    session: &SessionHandle,
) -> Result<(), cagent_agent::protocol::RuntimeError> {
    if !app.is_observer() {
        session.end().await?;
    }
    Ok(())
}

fn pending_interaction_changed(
    previous: Option<cagent_agent::protocol::InteractionRequestId>,
    current: Option<cagent_agent::protocol::InteractionRequestId>,
) -> bool {
    match (previous, current) {
        (None, Some(_)) => true,
        (Some(previous), Some(current)) => previous != current,
        _ => false,
    }
}

fn should_emit_notification(
    access: cagent_agent::protocol::SessionAccess,
    previous_interaction: Option<cagent_agent::protocol::InteractionRequestId>,
    current_interaction: Option<cagent_agent::protocol::InteractionRequestId>,
    notice: Option<cagent_agent::protocol::SessionUpdateNotice>,
    bell: cagent_agent::config::BellConfig,
    was_compacting: bool,
) -> bool {
    if !matches!(access, cagent_agent::protocol::SessionAccess::Owner) {
        return false;
    }

    let interaction_changed =
        pending_interaction_changed(previous_interaction, current_interaction);
    (bell.completed_turn()
        && !was_compacting
        && notice == Some(cagent_agent::protocol::SessionUpdateNotice::TurnCompleted))
        || (bell.requested_input() && interaction_changed)
}

fn notification_description(
    snapshot: &cagent_agent::protocol::SessionSnapshot,
    interaction_changed: bool,
    turn_completed: bool,
) -> String {
    let interaction = interaction_changed.then(|| {
        snapshot
            .pending_interaction
            .as_ref()
            .map_or("User input required", |request| match &request.kind {
                cagent_agent::protocol::InteractionRequestKind::Question { .. } => {
                    "Question requires an answer"
                }
                cagent_agent::protocol::InteractionRequestKind::PermissionApproval { .. } => {
                    "Approval required"
                }
                cagent_agent::protocol::InteractionRequestKind::PlanCompletion { .. } => {
                    "Plan decision required"
                }
            })
            .to_owned()
    });
    match (turn_completed, interaction) {
        (true, Some(interaction)) => format!("Turn completed · {interaction}"),
        (true, None) => "Turn completed".into(),
        (false, Some(interaction)) => interaction,
        (false, None) => "Cagent update".into(),
    }
}

trait FreshSessionSource {
    fn is_fresh(&self) -> bool;
}

impl FreshSessionSource for cagent_agent::protocol::SessionSnapshot {
    fn is_fresh(&self) -> bool {
        !self.transcript.iter().any(|block| {
            matches!(
                block.kind,
                cagent_agent::protocol::TranscriptBlockKind::User { .. }
            )
        })
    }
}

impl<const N: usize> FreshSessionSource for [cagent_agent::protocol::DurableEvent; N] {
    fn is_fresh(&self) -> bool {
        !self.iter().any(|event| {
            matches!(
                event.kind,
                cagent_agent::protocol::DurableEventKind::NodeAppended {
                    node_kind: cagent_agent::protocol::NodeKind::UserMessage,
                    ..
                }
            )
        })
    }
}

fn is_fresh_session<T: FreshSessionSource>(source: &T) -> bool {
    source.is_fresh()
}

/// A composer surface to display as soon as the interactive frontend starts.
pub(crate) enum StartupSurface {
    ResumePicker(Vec<cagent_agent::protocol::ConversationSummary>),
    Onboarding,
}

async fn submit_initial_prompt(
    app: &mut App,
    session: &SessionHandle,
    prompt: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if app.is_observer() || prompt.trim().is_empty() {
        return Ok(());
    }
    app.replace_draft(prompt);
    app.submit_draft(session, QueueTarget::NextBoundary).await
}

#[allow(clippy::too_many_lines)]
#[tracing::instrument(
    level = "info",
    name = "frontend.run",
    skip_all,
    fields(session_id = %session.id())
)]
pub(crate) async fn run(
    runtime: &AgentRuntime,
    mut session: SessionHandle,
    mut workspace: PathBuf,
    initial: Option<String>,
    startup_surface: Option<StartupSurface>,
    terminal: &mut DefaultTerminal,
    startup_started_at: Instant,
) -> Result<(cagent_agent::protocol::SessionStats, PathBuf), Box<dyn std::error::Error>> {
    let mut attachment = session.attach().await?;
    tracing::trace!(
        phase = "session_attached",
        elapsed = ?startup_started_at.elapsed(),
        "startup milestone reached"
    );
    let snapshot = &attachment.snapshot;
    let config = runtime.config_snapshot();
    let title = session_terminal_title_with_progress(
        snapshot.title.as_deref(),
        !matches!(
            snapshot.access,
            cagent_agent::protocol::SessionAccess::Observer { .. }
        ) && snapshot.pending_interaction.is_some(),
        config.title().requested_input(),
        config.title().progress(),
        !matches!(snapshot.turn, cagent_agent::protocol::TurnState::Idle),
        0,
    );
    set_terminal_title(&title)?;
    let mode_profiles = session.mode_profiles()?;
    let mut app = App::new(
        &workspace,
        snapshot.model_selection.clone(),
        snapshot.enabled_providers.iter().cloned().collect(),
        &snapshot.active_mode,
        config.composer_max_rows(),
    );
    app.image_picker = ratatui_image::picker::Picker::from_query_stdio()
        .unwrap_or_else(|_| ratatui_image::picker::Picker::halfblocks());
    app.set_requested_input_title(config.title().requested_input());
    app.collapse_tool_activity = config.collapse_tool_activity();
    app.open_links = config.open_links();
    app.set_progress_osc(config.progress_osc());
    app.set_progress_mode(config.title().progress());
    app.ui_editor = config.editor().clone();
    app.manual_cleanup_available = !config.conversation_cleanup().automatic();
    app.theme = crate::theme::Theme::new(config.ui_theme(), config.ui_colors());
    app.files_sidebar.default_width = u16::try_from(config.files_width()).unwrap_or(u16::MAX);
    app.session_id = snapshot.conversation_id;
    app.status_line_config = session.status_line_config()?;
    app.mode_colors = mode_profiles
        .iter()
        .map(|mode| (mode.name.clone(), mode.color))
        .collect();
    let (key_bindings, key_binding_warnings) =
        cagent_agent::presentation::ResolvedKeyBindings::from_config(&config);
    app.key_bindings = key_bindings;
    app.mode_commands = mode_profiles.into_iter().map(|mode| mode.name).collect();
    app.skill_commands = session
        .skills()
        .into_iter()
        .filter(|skill| skill.enabled)
        .filter(|skill| {
            !SLASH_COMMANDS
                .iter()
                .any(|command| command.name == format!("/{}", skill.name))
        })
        .map(|skill| (skill.name, skill.description))
        .collect();
    app.hydrate_composer_history(snapshot.composer_history.clone());
    app.welcome_tip = (config.show_tips() && is_fresh_session(snapshot))
        .then(|| crate::render::tip_for_session(session.id()));
    app.apply_session_snapshot(&attachment.snapshot);
    let mut conversation_maintenance = runtime.subscribe_conversation_maintenance();
    // The initial snapshot is baseline state. An interaction already pending
    // when the frontend attaches must not notify the terminal immediately.
    match startup_surface {
        Some(StartupSurface::ResumePicker(rows)) => {
            let visible = cagent_agent::presentation::conversation_picker_indexes(&rows, "");
            let selected =
                cagent_agent::presentation::conversation_picker_initial_selection(&rows, "");
            app.surfaces.push(Surface::Conversations {
                rows,
                list: ListState::selectable_at(visible.len(), selected, VISIBLE_MENU_ITEMS),
                query: String::new(),
                query_cursor: 0,
                opened_at_millis: picker_opened_at_millis(),
                include_archived: false,
            });
        }
        Some(StartupSurface::Onboarding) => app.begin_onboarding(),
        _ => {}
    }
    app.refresh_conversation_surface(&session).await?;
    if !key_binding_warnings.is_empty() {
        app.set_notice(key_binding_warnings.join(" · "));
    }
    let mut config_updates = runtime.subscribe_config();
    let mut input = EventStream::new();
    let mut filesystem_watcher = match cagent_agent::presentation::FilesystemWatcher::new() {
        Ok(watcher) => Some(watcher),
        Err(error) => {
            app.set_notice(format!("filesystem watching unavailable · {error}"));
            None
        }
    };
    let mut progress = ProgressGuard::new();
    // Keep this ticker alive across runtime updates. Recreating a sleep in the
    // select loop lets a busy event stream continually cancel the animation
    // before it reaches its deadline, freezing both the shimmer and elapsed
    // time in the working row.
    let mut working_animation = working_animation_ticker();
    let mut bell = config.bell();
    let mut deferred_initial_prompt = None;
    if app.onboarding {
        deferred_initial_prompt = initial.filter(|prompt| !prompt.trim().is_empty());
    } else if let Some(prompt) = initial.as_deref() {
        submit_initial_prompt(&mut app, &session, prompt).await?;
    }

    let (completion_sender, mut completion_updates) =
        tokio::sync::mpsc::channel::<CompletionResult>(UI_RESULT_CHANNEL_CAPACITY);
    let (directory_load_sender, mut directory_load_updates) =
        tokio::sync::mpsc::channel(UI_RESULT_CHANNEL_CAPACITY);
    let (transcript_page_sender, mut transcript_page_updates) =
        tokio::sync::mpsc::channel::<TranscriptPageResult>(UI_RESULT_CHANNEL_CAPACITY);
    let (history_load_sender, mut history_load_updates) =
        tokio::sync::mpsc::channel::<HistoryLoadResult>(UI_RESULT_CHANNEL_CAPACITY);
    let (submission_sender, mut submission_updates) =
        tokio::sync::mpsc::channel::<SubmissionResult>(UI_RESULT_CHANNEL_CAPACITY);
    let (submission_request_sender, mut submission_requests) =
        tokio::sync::mpsc::channel::<SubmissionRequest>(UI_RESULT_CHANNEL_CAPACITY);
    tokio::spawn(async move {
        while let Some(request) = submission_requests.recv().await {
            let result = request.session.submit(request.submission.command).await;
            if submission_sender
                .send(SubmissionResult {
                    draft: request.submission.draft,
                    optimistic_queue_id: request.submission.optimistic_queue_id,
                    result,
                })
                .await
                .is_err()
            {
                break;
            }
        }
    });
    let mut history_loading = false;
    let mut history_dirty = false;
    let mut history_generation = 0_u64;
    let mut history_cache = std::collections::HashMap::<
        (cagent_agent::protocol::ConversationId, TreePurpose),
        cagent_agent::presentation::HistoryRowsSnapshot,
    >::new();
    let directory_load_limit =
        std::sync::Arc::new(tokio::sync::Semaphore::new(DIRECTORY_LOAD_MAX_CONCURRENT));
    let mut scheduled_completion = None::<CompletionRequest>;
    let mut completion_task = None::<tokio::task::JoinHandle<()>>;
    let mut path_completion = None::<PathCompletionState>;
    let mut first_frame = true;
    let (mcp_detail_sender, mut mcp_detail_updates) = tokio::sync::mpsc::channel(8);
    let (agent_log_sender, mut agent_log_updates) = tokio::sync::mpsc::channel(2);
    let mut pending_resume_frame = None::<std::time::Instant>;

    loop {
        schedule_directory_loads(&mut app, &directory_load_sender, &directory_load_limit);
        if let Some(watcher) = &mut filesystem_watcher
            && let Err(error) = watcher.reconcile(&app.filesystem_watch_targets())
        {
            app.set_notice(format!("could not update filesystem watches · {error}"));
        }
        app.render(terminal)?;
        if let Some((id, after)) = app.take_agent_log_request() {
            let detail_session = session.clone();
            let sender = agent_log_sender.clone();
            tokio::spawn(async move {
                let result = detail_session.agent_run_log_page(id, after).await;
                let _ = sender.send((detail_session.id(), id, after, result)).await;
            });
        }
        if let Some(node_id) = app.pending_mcp_detail_load.take() {
            let detail_session = session.clone();
            let sender = mcp_detail_sender.clone();
            tokio::spawn(async move {
                let result = detail_session.mcp_call_detail(node_id).await;
                let _ = sender.send((detail_session.id(), result)).await;
            });
        }
        if let Some(started_at) = pending_resume_frame.take() {
            tracing::trace!(
                phase = "resume_first_ui_frame_rendered",
                elapsed = ?started_at.elapsed(),
                "session transition milestone reached"
            );
        }
        if let Some(cursor) = app.take_transcript_page_request() {
            let sender = transcript_page_sender.clone();
            let page_session = session.clone();
            tokio::spawn(
                async move {
                    let result = page_session.load_older_transcript(cursor.clone()).await;
                    let _ = sender.send(TranscriptPageResult { cursor, result }).await;
                }
                .instrument(tracing::trace_span!("frontend.transcript.prefetch")),
            );
        }
        if first_frame {
            first_frame = false;
            tracing::trace!(
                phase = "first_ui_frame_rendered",
                elapsed = ?startup_started_at.elapsed(),
                "startup milestone reached"
            );
        }
        let notice_deadline = app.notice_deadline;
        let interaction_deadline = app.pending_interaction.as_ref().and_then(|_| {
            (!app.draft.is_empty() && app.surfaces.is_empty())
                .then(|| {
                    app.last_composer_input_at
                        .map(|input| input + INTERACTION_INPUT_SETTLE_DELAY)
                })
                .flatten()
        });
        let animate_working = app.working_indicator_visible()
            || app.bash_activity_animation_visible()
            || app.supervised_work.has_delayed_read()
            || (!app.is_observer()
                && app.pending_interaction.is_some()
                && app.requested_input_title.frames().len() > 1);
        let keep_progress_alive = app.osc_progress_active();
        let mcp_tools_loading = matches!(
            app.surfaces.last(),
            Some(Surface::McpTools { loading: true, .. })
        );
        let mcp_oauth_waiting = matches!(
            app.surfaces.last(),
            Some(Surface::McpOAuth { attempt, .. })
                if matches!(
                    &*attempt.completion.borrow(),
                    cagent_agent::mcp::McpOAuthCompletion::Waiting
                )
        );
        let mcp_server_refreshing = matches!(
            app.surfaces.last(),
            Some(Surface::McpServer { server, .. })
                if matches!(
                    server.status,
                    cagent_agent::mcp::McpRuntimeStatus::Starting
                        | cagent_agent::mcp::McpRuntimeStatus::Restarting
                        | cagent_agent::mcp::McpRuntimeStatus::AuthenticationRequired
                )
        );
        tokio::select! {
            loaded = agent_log_updates.recv() => {
                if let Some((conversation_id, id, after, result)) = loaded
                    && conversation_id == session.id()
                {
                    app.apply_agent_log_page(id, after, result);
                }
            }
            loaded = mcp_detail_updates.recv() => {
                if let Some((conversation_id, result)) = loaded
                    && conversation_id == session.id()
                {
                    match result {
                        Ok(Some(detail)) => app.apply_mcp_detail(detail),
                        Ok(None) => {},
                        Err(error) => app.set_notice(format!("could not load MCP details · {error}")),
                    }
                }
            }
            submitted = submission_updates.recv() => {
                let Some(submitted) = submitted else { break; };
                if let Err(error) = submitted.result {
                    if let Some(id) = submitted.optimistic_queue_id {
                        app.queued.retain(|message| message.id != id);
                    }
                    app.restore_failed_submission(submitted.draft);
                    app.set_notice(format!("could not send message · {error}"));
                }
            }
            loaded = history_load_updates.recv() => {
                if let Some(loaded) = loaded {
                    if loaded.generation != history_generation {
                        continue;
                    }
                    history_loading = false;
                    if loaded.conversation_id == session.id()
                        && app.open_history_purpose() == Some(loaded.purpose)
                    {
                        match loaded.result {
                            Ok(snapshot) => {
                                history_cache.insert(
                                    (loaded.conversation_id, loaded.purpose),
                                    snapshot.clone(),
                                );
                                app.apply_history_snapshot(loaded.purpose, snapshot);
                            }
                            Err(error) => {
                                app.close_history_surface();
                                app.set_notice(format!("could not load history · {error}"));
                            }
                        }
                    }
                    if history_dirty
                        && let Some(purpose) = app.open_history_purpose()
                    {
                        history_dirty = false;
                        history_loading = true;
                        history_generation = history_generation.wrapping_add(1);
                        schedule_history_load(
                            &session,
                            purpose,
                            history_generation,
                            &history_load_sender,
                        );
                    }
                }
            }
            page = transcript_page_updates.recv() => {
                if let Some(page) = page {
                    if !app.finish_transcript_page_request(&page.cursor) {
                        continue;
                    }
                    match page.result {
                        Ok(older) => {
                            if attachment.snapshot.transcript.prepend(&page.cursor, older) {
                                let _span = tracing::trace_span!("frontend.transcript.prepend").entered();
                                app.apply_session_snapshot(&attachment.snapshot);
                            } else {
                                attachment = session.attach().await?;
                                app.apply_session_snapshot(&attachment.snapshot);
                            }
                        }
                        Err(cagent_agent::protocol::RuntimeError::StaleTranscriptCursor) => {
                            app.transcript_fill_failed = Some(page.cursor.clone());
                            attachment = session.attach().await?;
                            app.apply_session_snapshot(&attachment.snapshot);
                        }
                        Err(error) => {
                            app.transcript_fill_failed = Some(page.cursor.clone());
                            app.set_notice(format!("could not load older history · {error}"));
                            app.transcript_home_drain = false;
                        }
                    }
                }
            }
            changed = conversation_maintenance.changed() => {
                if changed.is_ok() {
                    app.refresh_conversation_surface(&session).await?;
                }
            }
            changed = config_updates.changed() => {
                if changed.is_err() {
                    break;
                }
                let config = config_updates.borrow_and_update().clone();
                bell = config.bell();
                app.set_progress_osc(config.progress_osc());
                app.set_progress_mode(config.title().progress());
                progress.refresh(app.osc_progress_active(), app.progress_osc)?;
                app.composer_max_rows = config.composer_max_rows();
                app.manual_cleanup_available = !config.conversation_cleanup().automatic();
                app.open_links = config.open_links();
                app.theme = crate::theme::Theme::new(config.ui_theme(), config.ui_colors());
                if app.collapse_tool_activity != config.collapse_tool_activity() {
                    app.collapse_tool_activity = config.collapse_tool_activity();
                    for surface in &mut app.surfaces {
                        if let Surface::Expanded {
                            view:
                                ExpandedView::AgentLog {
                                    collapse_tool_activity,
                                    ..
                                },
                            ..
                        } = surface
                        {
                            *collapse_tool_activity = config.collapse_tool_activity();
                        }
                    }
                    app.history_layout.rendered = None;
                    app.expanded_text_render_cache.get_mut().take();
                }
                app.files_sidebar.default_width =
                    u16::try_from(config.files_width()).unwrap_or(u16::MAX);
                if &app.ui_editor != config.editor() {
                    app.ui_editor = config.editor().clone();
                    app.history_layout.rendered = None;
                    app.history_block_layouts.clear();
                    app.streaming_layout = None;
                    app.streaming_plan_layout = None;
                    app.expanded_text_render_cache.get_mut().take();
                }
                app.set_requested_input_title(config.title().requested_input());
                if let Some(title) = app.pending_terminal_title.take() {
                    set_terminal_title(&title)?;
                }
                if !config.show_tips() {
                    app.welcome_tip = None;
                } else if is_fresh_session(&attachment.snapshot) {
                    app.welcome_tip = Some(crate::render::tip_for_session(session.id()));
                }
                let (bindings, warnings) =
                    cagent_agent::presentation::ResolvedKeyBindings::from_config(&config);
                app.key_bindings = bindings;
                app.status_line_config = config.status_line().clone();
                let mode_profiles = config.enabled_modes()?;
                app.mode_colors = mode_profiles
                    .iter()
                    .map(|mode| (mode.name.clone(), mode.color))
                    .collect();
                app.mode_commands = mode_profiles
                    .into_iter()
                    .map(|mode| mode.name)
                    .collect();
                app.skill_commands = session
                    .skills()
                    .into_iter()
                    .filter(|skill| skill.enabled)
                    .filter(|skill| {
                        !SLASH_COMMANDS
                            .iter()
                            .any(|command| command.name == format!("/{}", skill.name))
                    })
                    .map(|skill| (skill.name, skill.description))
                    .collect();
                if !warnings.is_empty() {
                    app.set_notice(warnings.join(" · "));
                }
            }
            () = wait_for_deadline(notice_deadline) => {
                if app.notice_deadline == notice_deadline {
                    app.clear_notice();
                }
            }
            () = wait_for_interaction_delay(interaction_deadline) => {
                app.open_pending_interaction();
            }
            _ = working_animation.tick(), if animate_working => {
                if app.reconnect_status.as_ref().is_some_and(|status| {
                    status.deadline <= std::time::Instant::now()
                }) {
                    app.reconnect_status = None;
                }
                if app.supervised_work_surface_open() {
                    app.refresh_open_supervised_work_surfaces();
                }
                app.invalidate_activity_layout();
                app.advance_progress_frame();
                progress.refresh(app.osc_progress_active(), app.progress_osc)?;
                if let Some(title) = app.pending_terminal_title.take() {
                    set_terminal_title(&title)?;
                }
            }
            () = wait_for_keepalive(keep_progress_alive) => {
                progress.refresh(app.osc_progress_active(), app.progress_osc)?;
            }
            () = wait_for_mcp_tools(mcp_tools_loading || mcp_oauth_waiting || mcp_server_refreshing), if mcp_tools_loading || mcp_oauth_waiting || mcp_server_refreshing => {
                if mcp_tools_loading {
                    app.refresh_loading_mcp_tools(&session).await;
                }
                if mcp_oauth_waiting || mcp_server_refreshing {
                    app.refresh_open_mcp_server(&session).await;
                }
            }
            update = attachment.updates.next() => {
                let Some(update) = update else { break; };
                let update = update?;
                let history_changed = update.history_changed;
                let previous_interaction = pending_interaction_id(&attachment.snapshot);
                let was_compacting = matches!(
                    attachment.snapshot.turn,
                    cagent_agent::protocol::TurnState::Compacting { .. }
                );
                let notice = update.notice;
                let provider_usage_updated = matches!(
                    &update.kind,
                    cagent_agent::protocol::SessionUpdateKind::ProviderUsage(_)
                );
                let retry_status = match &update.kind {
                    cagent_agent::protocol::SessionUpdateKind::RetryScheduled(status) => {
                        Some(status.clone())
                    }
                    _ => None,
                };
                let terminal_update = match &update.kind {
                    cagent_agent::protocol::SessionUpdateKind::Terminal(update) => Some(update.clone()),
                    cagent_agent::protocol::SessionUpdateKind::Snapshot(_)
                    | cagent_agent::protocol::SessionUpdateKind::RetryScheduled(_)
                    | cagent_agent::protocol::SessionUpdateKind::ProviderUsage(_)
                    | cagent_agent::protocol::SessionUpdateKind::StartupResources(_)
                    | cagent_agent::protocol::SessionUpdateKind::DelegatedTerminal(_)
                    | cagent_agent::protocol::SessionUpdateKind::DelegatedRun(_)
                    | cagent_agent::protocol::SessionUpdateKind::DelegatedText(_)
                    | cagent_agent::protocol::SessionUpdateKind::ResyncRequired => None,
                };
                let delegated_terminal_update = match &update.kind {
                    cagent_agent::protocol::SessionUpdateKind::DelegatedTerminal(terminal) => {
                        Some(terminal.clone())
                    }
                    cagent_agent::protocol::SessionUpdateKind::Snapshot(_)
                    | cagent_agent::protocol::SessionUpdateKind::RetryScheduled(_)
                    | cagent_agent::protocol::SessionUpdateKind::ProviderUsage(_)
                    | cagent_agent::protocol::SessionUpdateKind::StartupResources(_)
                    | cagent_agent::protocol::SessionUpdateKind::Terminal(_)
                    | cagent_agent::protocol::SessionUpdateKind::DelegatedRun(_)
                    | cagent_agent::protocol::SessionUpdateKind::DelegatedText(_)
                    | cagent_agent::protocol::SessionUpdateKind::ResyncRequired => None,
                };
                let startup_resources = match &update.kind {
                    cagent_agent::protocol::SessionUpdateKind::StartupResources(status) => {
                        Some(status.clone())
                    }
                    cagent_agent::protocol::SessionUpdateKind::Snapshot(_)
                    | cagent_agent::protocol::SessionUpdateKind::RetryScheduled(_)
                    | cagent_agent::protocol::SessionUpdateKind::ProviderUsage(_)
                    | cagent_agent::protocol::SessionUpdateKind::Terminal(_)
                    | cagent_agent::protocol::SessionUpdateKind::DelegatedTerminal(_)
                    | cagent_agent::protocol::SessionUpdateKind::DelegatedRun(_)
                    | cagent_agent::protocol::SessionUpdateKind::DelegatedText(_)
                    | cagent_agent::protocol::SessionUpdateKind::ResyncRequired => None,
                };
                let delegated_run_update = match &update.kind {
                    cagent_agent::protocol::SessionUpdateKind::DelegatedRun(update) => {
                        Some(update.clone())
                    }
                    _ => None,
                };
                let delegated_text_update = match &update.kind {
                    cagent_agent::protocol::SessionUpdateKind::DelegatedText(update) => {
                        Some(update.clone())
                    }
                    _ => None,
                };
                let applied = attachment.snapshot.apply(update);
                if !applied {
                    attachment = session.attach().await?;
                    app.apply_session_snapshot(&attachment.snapshot);
                } else if app.history_preview.is_some() {
                    app.apply_session_snapshot(&attachment.snapshot);
                } else if let Some(update) = terminal_update {
                    if attachment
                        .snapshot
                        .transcript
                        .iter()
                        .any(|block| block.id == update.id)
                        && !app.history.iter().any(|block| block.id == update.id)
                    {
                        app.apply_session_snapshot(&attachment.snapshot);
                    } else {
                        app.apply_terminal_update(&update);
                    }
                } else if let Some(terminal) = delegated_terminal_update {
                    app.apply_delegated_terminal_update(&terminal);
                } else if let Some(update) = delegated_run_update {
                    app.apply_delegated_run_update(&update);
                } else if let Some(update) = delegated_text_update {
                    app.apply_delegated_text_update(&update);
                } else if let Some(status) = startup_resources {
                    app.apply_startup_resources(&status);
                } else if provider_usage_updated {
                    app.provider_usage
                        .clone_from(&attachment.snapshot.provider_usage);
                } else if let Some(status) = retry_status {
                    app.apply_retry_status(&status);
                } else {
                    app.apply_session_snapshot(&attachment.snapshot);
                }
                // Local-context watcher updates are surfaced through the existing startup-resource
                // update stream. Refresh the open catalog from the agent-owned snapshot so edits
                // made by `$EDITOR` or any external process appear without reopening `/skills`.
                let skills = session.skills();
                app.skill_commands = skills.iter().filter(|skill| skill.enabled)
                    .filter(|skill| !SLASH_COMMANDS.iter().any(|command| command.name == format!("/{}", skill.name)))
                    .map(|skill| (skill.name.clone(), skill.description.clone())).collect();
                if let Some(Surface::Skills { rows, list }) = app.surfaces.last_mut()
                    && *rows != skills
                {
                    *rows = skills;
                    list.reconcile(ListMode::Selectable, rows.len() + 2, VISIBLE_MENU_ITEMS);
                }
                let current_interaction = pending_interaction_id(&attachment.snapshot);
                let interaction_changed =
                    pending_interaction_changed(previous_interaction, current_interaction);
                let turn_completed =
                    notice == Some(cagent_agent::protocol::SessionUpdateNotice::TurnCompleted);
                if applied
                    && should_emit_notification(
                        attachment.snapshot.access,
                        previous_interaction,
                        current_interaction,
                        notice,
                        bell,
                        was_compacting,
                    )
                {
                    let title = attachment
                        .snapshot
                        .title
                        .as_deref()
                        .filter(|title| !title.trim().is_empty())
                        .unwrap_or("Cagent");
                    let description = notification_description(
                        &attachment.snapshot,
                        interaction_changed,
                        turn_completed,
                    );
                    progress::write_notification(bell.method(), title, &description)?;
                }
                app.refresh_open_terminal_surface(&session).await?;
                if history_changed
                    && let Some(purpose) = app.open_history_purpose()
                {
                    history_cache.remove(&(session.id(), purpose));
                    if history_loading {
                        history_dirty = true;
                    } else {
                        history_loading = true;
                        history_generation = history_generation.wrapping_add(1);
                        schedule_history_load(
                            &session,
                            purpose,
                            history_generation,
                            &history_load_sender,
                        );
                    }
                }
                if notice
                    == Some(cagent_agent::protocol::SessionUpdateNotice::ProviderAuthUpdated)
                {
                    app.refresh_provider_auth_surfaces(&session).await;
                }
                if let Some(title) = app.pending_terminal_title.take() {
                    set_terminal_title(&title)?;
                }
                progress.refresh(app.osc_progress_active(), app.progress_osc)?;
                if let Some(title) = app.pending_terminal_title.take() {
                    set_terminal_title(&title)?;
                }
            }
            completion = completion_updates.recv() => {
                let Some(completion) = completion else { break; };
                app.apply_completion(completion);
                progress.refresh(app.osc_progress_active(), app.progress_osc)?;
            }
            loaded = directory_load_updates.recv() => {
                if let Some(loaded) = loaded {
                    app.apply_directory_load_result(loaded);
                }
            }
            change = wait_for_filesystem_change(&mut filesystem_watcher) => {
                if let Some(change) = change {
                    app.refresh_filesystem_views(&change.paths);
                    if let Some(error) = change.errors.first() {
                        app.set_notice(format!("filesystem watch error · {error}"));
                    }
                }
            }
            terminal_event = input.next() => {
                let Some(terminal_event) = terminal_event else { break; };
                if app.handle_terminal_event(&session, terminal_event?).await? {
                    break;
                }
                if let Some(outcome) = app.take_onboarding_outcome() {
                    match outcome {
                        OnboardingOutcome::Completed => {
                            if let Some(prompt) = deferred_initial_prompt.take()
                            {
                                submit_initial_prompt(&mut app, &session, &prompt).await?;
                            }
                        }
                        OnboardingOutcome::Dismissed => {
                            if let Some(prompt) = deferred_initial_prompt.take() {
                                app.replace_draft(&prompt);
                            }
                        }
                    }
                }
                match app.pending_action.take() {
                    Some(AppAction::Quit) => {
                        if app.is_observer() {
                            break;
                        }
                        let (running, _) = session.terminal_counts();
                        if running > 0
                            && app
                                .exit_armed
                                .is_none_or(|armed| armed.elapsed() > Duration::from_secs(2))
                        {
                            app.exit_armed = Some(Instant::now());
                            app.set_notice(format!("{running} background terminal(s) running · quit again to terminate"));
                        } else {
                            session.end().await?;
                            break;
                        }
                    }
                    Some(AppAction::Archive) => {
                        session.end().await?;
                        runtime.set_conversation_archived(session.id(), true).await?;
                        break;
                    }
                    Some(AppAction::Delete) => {
                        let id = session.id();
                        runtime.delete_conversation(id).await?;
                        break;
                    }
                    Some(AppAction::EditDraft) => {
                        let edited = edit_draft_with_terminal(terminal, &app.draft)?;
                        match edited {
                            Ok(draft) => app.replace_draft_preserving_images(&draft),
                            Err(message) => app.set_notice(message),
                        }
                    }
                    Some(AppAction::LaunchPath {
                        argv_candidates,
                        mode,
                        reload_startup_resources,
                    }) => {
                        let launched = match mode {
                            cagent_agent::config::UiEditorMode::Foreground => {
                                launch_path_in_foreground(terminal, &argv_candidates)?
                            }
                            cagent_agent::config::UiEditorMode::Background => {
                                launch_path_in_background(&argv_candidates)
                            }
                        };
                        if let Err(message) = launched {
                            app.set_notice(message);
                        } else if reload_startup_resources {
                            session
                                .submit(cagent_agent::protocol::SessionCommand::new(
                                    cagent_agent::protocol::SessionAction::ReloadStartupResources,
                                ))
                                .await?;
                            let skills = session.skills();
                            app.skill_commands = skills
                                .iter()
                                .filter(|skill| skill.enabled)
                                .filter(|skill| {
                                    !SLASH_COMMANDS.iter().any(|command| {
                                        command.name == format!("/{}", skill.name)
                                    })
                                })
                                .map(|skill| (skill.name.clone(), skill.description.clone()))
                                .collect();
                            if let Some(Surface::Skills { rows, list }) =
                                app.surfaces.last_mut()
                            {
                                *rows = skills;
                                list.reconcile(
                                    ListMode::Selectable,
                                    rows.len() + 2,
                                    VISIBLE_MENU_ITEMS,
                                );
                            }
                        }
                    }
                    Some(AppAction::NewSession(name)) => {
                        history_generation = history_generation.wrapping_add(1);
                        history_loading = false;
                        history_dirty = false;
                        history_cache.clear();
                        if !app.is_observer() {
                            session.end().await?;
                        }
                        session = runtime
                            .create_session_named(
                                cagent_agent::runtime::NewSession {
                                    workspace: workspace.clone(),
                                },
                                name,
                            )
                            .await?;
                        attachment = session.attach().await?;
                        let mode_profiles = session.mode_profiles()?;
                        app.reset_for_session_snapshot(
                            &attachment.snapshot,
                            runtime.config_snapshot().show_tips()
                                .then(|| crate::render::tip_for_session(session.id())),
                            terminal.size()?.width,
                        );
                        app.mode_colors = mode_profiles
                            .into_iter()
                            .map(|mode| (mode.name, mode.color))
                            .collect();
                        let title = app.current_terminal_title();
                        set_terminal_title(&title)?;
                    }
                    Some(AppAction::SwitchWorkspace(path)) => {
                        if !app.is_observer() {
                            session.end().await?;
                        }
                        workspace = path;
                        session = runtime.create_session(cagent_agent::runtime::NewSession {
                            workspace: workspace.clone(),
                        }).await?;
                        attachment = session.attach().await?;
                        let mode_profiles = session.mode_profiles()?;
                        app.workspace = workspace.clone();
                        app.reset_for_session_snapshot(
                            &attachment.snapshot,
                            None,
                            terminal.size()?.width,
                        );
                        app.mode_colors = mode_profiles
                            .into_iter()
                            .map(|mode| (mode.name, mode.color))
                            .collect();
                        app.push_system_message(&format!(
                            "Switched workspace to {}",
                            workspace.display()
                        ));
                        let title = app.current_terminal_title();
                        set_terminal_title(&title)?;
                    }
                    Some(AppAction::Resume(id)) => {
                        let resume_started_at = std::time::Instant::now();
                        pending_resume_frame = Some(resume_started_at);
                        tracing::trace!(
                            phase = "resume_selected",
                            session_id = ?id,
                            "session transition milestone reached"
                        );
                        history_generation = history_generation.wrapping_add(1);
                        history_loading = false;
                        history_dirty = false;
                        history_cache.clear();
                        end_session_before_transition(&app, &session).await?;
                        session = match id {
                            Some(id) => runtime.resume_session(id).await?,
                            None => runtime.continue_session(&workspace).await?,
                        };
                        tracing::trace!(
                            phase = "resume_handle_ready",
                            session_id = %session.id(),
                            elapsed = ?resume_started_at.elapsed(),
                            "session transition milestone reached"
                        );
                        attachment = session.attach().await?;
                        tracing::trace!(
                            phase = "resume_snapshot_attached",
                            session_id = %session.id(),
                            transcript_blocks = attachment.snapshot.transcript.len(),
                            elapsed = ?resume_started_at.elapsed(),
                            "session transition milestone reached"
                        );
                        let mode_profiles = session.mode_profiles()?;
                        app.reset_for_session_snapshot(
                            &attachment.snapshot,
                            (runtime.config_snapshot().show_tips()
                                && is_fresh_session(&attachment.snapshot))
                                .then(|| crate::render::tip_for_session(session.id())),
                            terminal.size()?.width,
                        );
                        tracing::trace!(
                            phase = "resume_snapshot_applied",
                            session_id = %session.id(),
                            elapsed = ?resume_started_at.elapsed(),
                            "session transition milestone reached"
                        );
                        app.mode_colors = mode_profiles
                            .into_iter()
                            .map(|mode| (mode.name, mode.color))
                            .collect();
                        let title = app.current_terminal_title();
                        set_terminal_title(&title)?;
                    }
                    Some(AppAction::RefreshFork) => {
                        let draft = app.pending_fork_draft.take();
                        attachment = session.attach().await?;
                        app.reset_for_session_snapshot(
                            &attachment.snapshot,
                            None,
                            terminal.size()?.width,
                        );
                        let title = app.current_terminal_title();
                        set_terminal_title(&title)?;
                        app.pending_terminal_title = None;
                        if let Some((draft, specs)) = draft {
                            app.replace_draft_with_attachment_specs(&draft, &specs);
                        }
                    }
                    Some(AppAction::HardFork(at)) => {
                        let draft = app.pending_fork_draft.take();
                        history_generation = history_generation.wrapping_add(1);
                        history_loading = false;
                        history_dirty = false;
                        history_cache.clear();
                        let previous = session.clone();
                        session = runtime.hard_fork_session(&previous, at).await?;
                        if !app.is_observer() {
                            previous.end().await?;
                        }
                        attachment = session.attach().await?;
                        let mode_profiles = session.mode_profiles()?;
                        app.reset_for_session_snapshot(
                            &attachment.snapshot,
                            None,
                            terminal.size()?.width,
                        );
                        app.mode_colors = mode_profiles
                            .into_iter()
                            .map(|mode| (mode.name, mode.color))
                            .collect();
                        if let Some((draft, specs)) = draft {
                            app.replace_draft_with_attachment_specs(&draft, &specs);
                        }
                        let title = app.current_terminal_title();
                        set_terminal_title(&title)?;
                    }
                    Some(AppAction::Submit(submission)) => {
                        submission_request_sender
                            .send(SubmissionRequest {
                                session: session.clone(),
                                submission,
                            })
                            .await
                            .map_err(|_| "submission worker stopped")?;
                    }
                    Some(AppAction::LoadHistory(purpose)) => {
                        if !history_loading {
                            let cached = history_cache
                                .get(&(session.id(), purpose))
                                .filter(|snapshot| {
                                    snapshot.revision == attachment.snapshot.cursor
                                })
                                .cloned();
                            if let Some(snapshot) = cached {
                                app.apply_history_snapshot(purpose, snapshot);
                            } else {
                                history_loading = true;
                                history_dirty = false;
                                history_generation = history_generation.wrapping_add(1);
                                schedule_history_load(
                                    &session,
                                    purpose,
                                    history_generation,
                                    &history_load_sender,
                                );
                            }
                        }
                    }
                    None => {}
                }
                progress.refresh(app.osc_progress_active(), app.progress_osc)?;
                schedule_completion(
                    &session,
                    &app,
                    &completion_sender,
                    &mut scheduled_completion,
                    &mut completion_task,
                    &mut path_completion,
                );
            }
        }
    }
    if let Some(completion_task) = completion_task {
        completion_task.abort();
    }
    Ok((session.stats().await?, session.working_directory()?))
}

fn schedule_directory_loads(
    app: &mut App,
    sender: &tokio::sync::mpsc::Sender<cagent_agent::presentation::DirectoryLoadResult>,
    limit: &std::sync::Arc<tokio::sync::Semaphore>,
) {
    for request in app.take_directory_load_requests() {
        let sender = sender.clone();
        let limit = std::sync::Arc::clone(limit);
        tokio::spawn(
            async move {
                let Ok(permit) = limit.acquire_owned().await else {
                    return;
                };
                let result = request.load().await;
                drop(permit);
                let _ = sender.send(result).await;
            }
            .in_current_span(),
        );
    }
}

async fn wait_for_filesystem_change(
    watcher: &mut Option<cagent_agent::presentation::FilesystemWatcher>,
) -> Option<cagent_agent::presentation::FilesystemChange> {
    match watcher {
        Some(watcher) => watcher.changed().await,
        None => std::future::pending().await,
    }
}

fn edit_draft_with_terminal(
    terminal: &mut DefaultTerminal,
    draft: &str,
) -> Result<Result<String, String>, Box<dyn std::error::Error>> {
    crossterm::execute!(
        std::io::stdout(),
        crossterm::event::DisableBracketedPaste,
        crossterm::event::DisableMouseCapture,
    )?;
    ratatui::try_restore()?;
    let edited = edit_draft_in_system_editor(draft);
    *terminal = ratatui::init();
    crossterm::execute!(
        std::io::stdout(),
        crossterm::event::EnableBracketedPaste,
        crossterm::event::EnableMouseCapture,
    )?;
    Ok(edited)
}

fn launch_path_in_foreground(
    terminal: &mut DefaultTerminal,
    argv_candidates: &[Vec<String>],
) -> Result<Result<(), String>, Box<dyn std::error::Error>> {
    crossterm::execute!(
        std::io::stdout(),
        crossterm::event::DisableBracketedPaste,
        crossterm::event::DisableMouseCapture,
    )?;
    ratatui::try_restore()?;
    let launched = launch_path_foreground_candidates(argv_candidates);
    *terminal = ratatui::init();
    crossterm::execute!(
        std::io::stdout(),
        crossterm::event::EnableBracketedPaste,
        crossterm::event::EnableMouseCapture,
    )?;
    Ok(launched)
}

fn launch_path_foreground_candidates(argv_candidates: &[Vec<String>]) -> Result<(), String> {
    let mut missing = Vec::new();
    for argv in argv_candidates {
        let (executable, arguments) = argv
            .split_first()
            .ok_or_else(|| "ui.editor command is empty".to_owned())?;
        match Command::new(executable).args(arguments).status() {
            Ok(status) => {
                return status
                    .success()
                    .then_some(())
                    .ok_or_else(|| format!("{executable} exited with {status}"));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(executable.clone());
            }
            Err(error) => return Err(format!("could not launch {executable} · {error}")),
        }
    }
    Err(missing_editor_message(&missing))
}

fn launch_path_in_background(argv_candidates: &[Vec<String>]) -> Result<(), String> {
    let mut missing = Vec::new();
    for argv in argv_candidates {
        match spawn_path_in_background(argv) {
            Ok(()) => return Ok(()),
            Err((executable, error)) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(executable);
            }
            Err((executable, error)) => {
                return Err(format!("could not launch {executable} · {error}"));
            }
        }
    }
    Err(missing_editor_message(&missing))
}

fn spawn_path_in_background(argv: &[String]) -> Result<(), (String, std::io::Error)> {
    let (executable, arguments) = argv.split_first().ok_or_else(|| {
        (
            "ui.editor".to_owned(),
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "command is empty"),
        )
    })?;
    let mut child = Command::new(executable)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| (executable.clone(), error))?;
    std::thread::Builder::new()
        .name("cagent-path-editor".into())
        .spawn(move || {
            let _ = child.wait();
        })
        .map_err(|error| (executable.clone(), error))?;
    Ok(())
}

fn missing_editor_message(executables: &[String]) -> String {
    if executables.is_empty() {
        "ui.editor command is empty".to_owned()
    } else {
        format!(
            "could not find ui.editor executable(s): {}",
            executables.join(", ")
        )
    }
}

async fn wait_for_deadline(deadline: Option<Instant>) {
    if let Some(deadline) = deadline {
        tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
    } else {
        std::future::pending::<()>().await;
    }
}

async fn wait_for_mcp_tools(loading: bool) {
    if loading {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn working_animation_ticker() -> tokio::time::Interval {
    let mut ticker = tokio::time::interval(WORKING_ANIMATION_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ticker
}

async fn wait_for_interaction_delay(deadline: Option<Instant>) {
    if let Some(deadline) = deadline {
        tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
    } else {
        std::future::pending::<()>().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn directory_load_scheduler_populates_a_tree_without_blocking_its_initial_render() {
        let temporary = tempfile::tempdir().unwrap();
        let file = temporary.path().join("main.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();
        let mut app = App::new(
            temporary.path(),
            ("mock".into(), "echo".into(), None),
            Default::default(),
            "ask",
            None,
        );
        app.toggle_files_sidebar();
        assert!(
            app.files_sidebar
                .tree
                .as_ref()
                .unwrap()
                .browser
                .tree
                .rows()
                .iter()
                .any(|row| row.kind == cagent_agent::presentation::DirectoryEntryKind::Loading)
        );

        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        schedule_directory_loads(
            &mut app,
            &sender,
            &std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
        );
        let result = tokio::time::timeout(Duration::from_secs(3), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        app.apply_directory_load_result(result);

        assert!(
            app.files_sidebar
                .tree
                .as_ref()
                .unwrap()
                .browser
                .tree
                .rows()
                .iter()
                .any(|row| row.path == file)
        );
    }

    #[cfg(unix)]
    #[test]
    fn background_path_launch_appends_the_path_and_returns_to_the_tui() {
        let temporary = tempfile::tempdir().unwrap();
        let selected = temporary.path().join("selected file.rs");
        let recorded = temporary.path().join("recorded");
        std::fs::write(&selected, "fn main() {}\n").unwrap();
        let command = vec![
            "sh".into(),
            "-c".into(),
            "printf '%s' \"$2\" > \"$1\"".into(),
            "sh".into(),
            recorded.to_string_lossy().into_owned(),
        ];

        let mut argv = command;
        argv.push(selected.to_string_lossy().into_owned());
        launch_path_in_background(&[argv]).unwrap();
        for _ in 0..100 {
            if recorded.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            std::fs::read_to_string(recorded).unwrap(),
            selected.to_string_lossy()
        );
    }

    #[cfg(unix)]
    #[test]
    fn background_path_launch_falls_back_when_the_primary_executable_is_missing() {
        let temporary = tempfile::tempdir().unwrap();
        let recorded = temporary.path().join("recorded");
        let candidates = vec![
            vec!["cagent-editor-that-does-not-exist".into()],
            vec![
                "sh".into(),
                "-c".into(),
                "printf fallback > \"$1\"".into(),
                "sh".into(),
                recorded.to_string_lossy().into_owned(),
            ],
        ];

        launch_path_in_background(&candidates).unwrap();
        for _ in 0..100 {
            if recorded.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(std::fs::read_to_string(recorded).unwrap(), "fallback");
    }

    #[tokio::test(start_paused = true)]
    async fn working_animation_ticker_is_not_postponed_by_runtime_updates() {
        let mut ticker = working_animation_ticker();
        ticker.tick().await;

        for _ in 0..8 {
            tokio::time::advance(Duration::from_millis(10)).await;
            // A runtime update causes the select loop to run again, but must
            // not replace the ticker or postpone its scheduled frame.
        }

        assert!(
            tokio::time::timeout(Duration::ZERO, ticker.tick())
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn initial_prompt_uses_composer_command_dispatch() {
        let temporary = tempfile::tempdir().unwrap();
        let config = cagent_agent::config::ConfigSnapshot::parse(
            &temporary.path().join("config.toml"),
            "version = 1\ndefault_model = { model = 'mock/echo' }\n[providers.mock]\ntype = 'mock'\nenabled = true\n",
        )
        .unwrap();
        let runtime = AgentRuntime::open(
            cagent_agent::runtime::RuntimeOptions::new(temporary.path().join("commands.db"))
                .with_config(config),
        )
        .await
        .unwrap();
        let session = runtime
            .create_session(cagent_agent::runtime::NewSession {
                workspace: temporary.path().to_path_buf(),
            })
            .await
            .unwrap();
        let mut app = App::new(
            temporary.path(),
            ("mock".into(), "echo".into(), None),
            ["mock".into()].into_iter().collect(),
            "ask",
            None,
        );

        submit_initial_prompt(&mut app, &session, "/plan make a plan that just says hi")
            .await
            .unwrap();

        assert_eq!(session.active_profiles().await.unwrap().1, "plan");
        assert_eq!(
            app.history_entries.last().map(|entry| entry.text.as_str()),
            Some("/plan make a plan that just says hi")
        );
        assert!(app.draft.is_empty());
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if session.history().await.unwrap().iter().any(|node| {
                    node.kind == NodeKind::UserMessage
                        && node.content["text"] == "make a plan that just says hi"
                }) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        let composer_history = session.reload_composer_history_entries().await.unwrap();
        assert!(
            composer_history
                .iter()
                .any(|entry| entry.text == "/plan make a plan that just says hi")
        );
        assert!(
            composer_history
                .iter()
                .any(|entry| entry.text == "make a plan that just says hi")
        );
    }

    #[tokio::test]
    async fn blank_initial_prompt_is_ignored() {
        let temporary = tempfile::tempdir().unwrap();
        let runtime = AgentRuntime::open(cagent_agent::runtime::RuntimeOptions::new(
            temporary.path().join("commands.db"),
        ))
        .await
        .unwrap();
        let session = runtime
            .create_session(cagent_agent::runtime::NewSession {
                workspace: temporary.path().to_path_buf(),
            })
            .await
            .unwrap();
        let mut app = App::new(
            temporary.path(),
            ("mock".into(), "echo".into(), None),
            ["mock".into()].into_iter().collect(),
            "ask",
            None,
        );

        submit_initial_prompt(&mut app, &session, " \n\t ")
            .await
            .unwrap();

        assert!(app.draft.is_empty());
        assert!(app.history_entries.is_empty());
        assert!(
            session
                .history()
                .await
                .unwrap()
                .iter()
                .all(|node| { node.kind != NodeKind::UserMessage })
        );
    }

    fn node_event(node_kind: NodeKind) -> cagent_agent::protocol::DurableEvent {
        cagent_agent::protocol::DurableEvent {
            version: cagent_agent::protocol::API_VERSION,
            cursor: cagent_agent::protocol::EventCursor(1),
            conversation_id: cagent_agent::protocol::ConversationId::new(),
            kind: DurableEventKind::NodeAppended {
                node_id: cagent_agent::protocol::NodeId::new(),
                parent_id: None,
                turn_id: None,
                owner_id: None,
                request_index: None,
                node_kind,
                status: "completed".into(),
                content: serde_json::json!({}),
            },
        }
    }

    #[test]
    fn root_only_replay_is_fresh_but_user_history_is_not() {
        assert!(is_fresh_session(&[node_event(NodeKind::ConversationRoot)]));
        assert!(!is_fresh_session(&[node_event(NodeKind::UserMessage)]));
    }

    #[test]
    fn pending_interaction_notification_only_marks_new_request_ids() {
        let first = cagent_agent::protocol::InteractionRequestId::new();
        let second = cagent_agent::protocol::InteractionRequestId::new();

        assert!(should_emit_notification(
            cagent_agent::protocol::SessionAccess::Owner,
            None,
            Some(first),
            None,
            cagent_agent::config::BellConfig::default(),
            false,
        ));
        assert!(!should_emit_notification(
            cagent_agent::protocol::SessionAccess::Owner,
            Some(first),
            Some(first),
            None,
            cagent_agent::config::BellConfig::default(),
            false,
        ));
        assert!(should_emit_notification(
            cagent_agent::protocol::SessionAccess::Owner,
            Some(first),
            Some(second),
            None,
            cagent_agent::config::BellConfig::default(),
            false,
        ));
        assert!(!should_emit_notification(
            cagent_agent::protocol::SessionAccess::Owner,
            Some(first),
            None,
            None,
            cagent_agent::config::BellConfig::default(),
            false,
        ));
    }

    #[test]
    fn completion_notification_is_silent_for_observers_and_without_a_notice() {
        assert!(should_emit_notification(
            cagent_agent::protocol::SessionAccess::Owner,
            None,
            None,
            Some(cagent_agent::protocol::SessionUpdateNotice::TurnCompleted),
            cagent_agent::config::BellConfig::default(),
            false,
        ));
        assert!(!should_emit_notification(
            cagent_agent::protocol::SessionAccess::Owner,
            None,
            None,
            None,
            cagent_agent::config::BellConfig::default(),
            false,
        ));
        assert!(!should_emit_notification(
            cagent_agent::protocol::SessionAccess::Observer {
                takeover_available: true,
            },
            None,
            None,
            Some(cagent_agent::protocol::SessionUpdateNotice::TurnCompleted),
            cagent_agent::config::BellConfig::default(),
            false,
        ));
    }

    #[test]
    fn completion_notification_is_silent_after_compaction() {
        assert!(!should_emit_notification(
            cagent_agent::protocol::SessionAccess::Owner,
            None,
            None,
            Some(cagent_agent::protocol::SessionUpdateNotice::TurnCompleted),
            cagent_agent::config::BellConfig::default(),
            true,
        ));
    }

    #[test]
    fn bell_notification_settings_gate_interactions_and_completions_independently() {
        let id = cagent_agent::protocol::InteractionRequestId::new();
        let disabled_input = cagent_agent::config::BellConfig::new(
            false,
            true,
            cagent_agent::config::BellMethod::Bell,
        );
        let disabled_completion = cagent_agent::config::BellConfig::new(
            true,
            false,
            cagent_agent::config::BellMethod::Bell,
        );
        let completion = Some(cagent_agent::protocol::SessionUpdateNotice::TurnCompleted);

        assert!(!should_emit_notification(
            cagent_agent::protocol::SessionAccess::Owner,
            None,
            Some(id),
            None,
            disabled_input,
            false,
        ));
        assert!(should_emit_notification(
            cagent_agent::protocol::SessionAccess::Owner,
            None,
            None,
            completion,
            disabled_input,
            false,
        ));
        assert!(should_emit_notification(
            cagent_agent::protocol::SessionAccess::Owner,
            None,
            Some(id),
            None,
            disabled_completion,
            false,
        ));
        assert!(!should_emit_notification(
            cagent_agent::protocol::SessionAccess::Owner,
            None,
            None,
            completion,
            disabled_completion,
            false,
        ));
    }
}
