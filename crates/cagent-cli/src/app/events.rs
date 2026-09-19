#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum SupervisedWorkKey {
    Agent(cagent_agent::protocol::AgentRunId),
    Terminal(cagent_agent::tools::TerminalId),
}

fn supervised_work_key(work: &cagent_agent::presentation::SupervisedWork) -> SupervisedWorkKey {
    match work {
        cagent_agent::presentation::SupervisedWork::Agent { run } => {
            SupervisedWorkKey::Agent(run.id)
        }
        cagent_agent::presentation::SupervisedWork::Terminal { terminal } => {
            SupervisedWorkKey::Terminal(terminal.id)
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct TerminalDetail {
    pub(super) terminal_id: Option<cagent_agent::tools::TerminalId>,
    pub(super) command: String,
    pub(super) output: String,
    pub(super) ansi_output: String,
    pub(super) completion: Option<String>,
    pub(super) started_at: Option<String>,
    pub(super) completed_at: Option<String>,
}

impl TerminalDetail {
    pub(super) fn into_surface(self, width: u16, height: u16) -> Surface {
        // Expanded Bash reserves the status row, two shared sticky rows, its
        // heading, upper indicator, and lower indicator/padding row.
        let viewport_rows = usize::from(height.saturating_sub(6).max(1));
        let scroll = crate::render::terminal_scrollback_max_with_completion(
            &self.ansi_output,
            self.completion.as_deref(),
            width.saturating_sub(4),
            u16::try_from(viewport_rows).unwrap_or(u16::MAX),
        );
        Surface::Expanded {
            view: ExpandedView::Terminal {
                terminal_id: self.terminal_id,
                command: self.command,
                output: self.output,
                ansi_output: self.ansi_output,
                completion: self.completion,
                started_at: self.started_at,
                completed_at: self.completed_at,
            },
            scroll,
            viewport_rows,
        }
    }
}

impl App {
    pub(super) fn apply_retry_status(
        &mut self,
        update: &cagent_agent::protocol::RetryStatusUpdate,
    ) {
        self.reconnect_status = Some(ReconnectStatus {
            next_attempt: update.next_attempt,
            max_attempts: update.max_attempts,
            reason: update.reason.clone(),
            deadline: Instant::now() + Duration::from_millis(update.delay_millis),
        });
    }

    /// Builds a terminal detail from either the complete retained terminal or
    /// the bounded legacy Bash-card cache when no terminal ID is available.
    pub(super) async fn terminal_detail_with_session(
        &self,
        session: &cagent_agent::runtime::SessionHandle,
        terminal_id: Option<cagent_agent::tools::TerminalId>,
        command: String,
        cached_output: String,
        cached_ansi_output: String,
        cached_completion: Option<String>,
    ) -> Result<TerminalDetail, cagent_agent::runtime::RuntimeError> {
        let (output, ansi_output, completion, started_at, completed_at) = match terminal_id {
            Some(id) => {
                let retained = session
                    .terminal_output(cagent_agent::tools::TerminalOutputRequest {
                        id,
                        cursor: None,
                    })
                    .await?;
                let completion = retained.status.is_final().then(|| {
                    retained.exit_code.map_or_else(
                        || "[process exited]".into(),
                        |code| format!("[exited with status {code}]"),
                    )
                });
                (
                    retained.output,
                    retained.ansi_output,
                    completion,
                    Some(retained.started_at),
                    retained.completed_at,
                )
            }
            None => (
                cached_output,
                cached_ansi_output,
                cached_completion,
                None,
                None,
            ),
        };
        Ok(Self::terminal_detail_with_timing(
            terminal_id,
            command,
            output,
            ansi_output,
            completion,
            started_at,
            completed_at,
        ))
    }

    pub(super) fn terminal_detail(
        terminal_id: Option<cagent_agent::tools::TerminalId>,
        command: String,
        output: String,
        ansi_output: String,
        completion: Option<String>,
    ) -> TerminalDetail {
        Self::terminal_detail_with_timing(
            terminal_id,
            command,
            output,
            ansi_output,
            completion,
            None,
            None,
        )
    }

    fn terminal_detail_with_timing(
        terminal_id: Option<cagent_agent::tools::TerminalId>,
        command: String,
        output: String,
        ansi_output: String,
        completion: Option<String>,
        started_at: Option<String>,
        completed_at: Option<String>,
    ) -> TerminalDetail {
        let ansi_output = if ansi_output.is_empty() {
            output.clone()
        } else {
            ansi_output
        };
        TerminalDetail {
            terminal_id,
            command,
            output,
            ansi_output,
            completion,
            started_at,
            completed_at,
        }
    }

    /// Rehydrates an open Bash detail from the supervisor's retained ring
    /// buffer. Incremental transcript updates intentionally carry previews,
    /// so the detail surface must fetch the complete plain and ANSI streams.
    pub(super) async fn refresh_open_terminal_surface(
        &mut self,
        session: &cagent_agent::runtime::SessionHandle,
    ) -> Result<(), cagent_agent::runtime::RuntimeError> {
        let Some((terminal_id, command, previous_max, previous_scroll, viewport_rows)) =
            self.surfaces.iter().find_map(|surface| match surface {
                Surface::Expanded {
                    view:
                        ExpandedView::Terminal {
                            terminal_id: Some(terminal_id),
                            command,
                            ansi_output,
                            completion,
                            ..
                        },
                    scroll,
                    viewport_rows,
                } => Some((
                    *terminal_id,
                    command.clone(),
                    self.cached_terminal_scrollback_max(
                        ansi_output,
                        completion.as_deref(),
                        self.render_width.saturating_sub(4),
                        u16::try_from(*viewport_rows).unwrap_or(u16::MAX),
                    )
                    .unwrap_or_else(|| {
                        crate::render::terminal_scrollback_max_with_completion(
                            ansi_output,
                            completion.as_deref(),
                            self.render_width.saturating_sub(4),
                            u16::try_from(*viewport_rows).unwrap_or(u16::MAX),
                        )
                    }),
                    *scroll,
                    *viewport_rows,
                )),
                _ => None,
            })
        else {
            return Ok(());
        };
        let detail = self
            .terminal_detail_with_session(
                session,
                Some(terminal_id),
                command,
                String::new(),
                String::new(),
                None,
            )
            .await?;
        // The refreshed detail replaces the complete retained stream, so its
        // cached VT100 state can no longer be reused.
        self.terminal_render_cache = None;
        let next_max = crate::render::terminal_scrollback_max_with_completion(
            &detail.ansi_output,
            detail.completion.as_deref(),
            self.render_width.saturating_sub(4),
            u16::try_from(viewport_rows).unwrap_or(u16::MAX),
        );
        let next_scroll =
            follow_expanded_scroll(previous_scroll.min(previous_max), previous_max, next_max);
        if let Some(Surface::Expanded {
            view:
                ExpandedView::Terminal {
                    output: surface_output,
                    ansi_output,
                    completion: surface_completion,
                    started_at: surface_started_at,
                    completed_at: surface_completed_at,
                    ..
                },
            scroll,
            ..
        }) = self.surfaces.iter_mut().find(|surface| {
            matches!(
                surface,
                Surface::Expanded {
                    view: ExpandedView::Terminal {
                        terminal_id: Some(id),
                        ..
                    },
                    ..
                } if *id == terminal_id
            )
        }) {
            *surface_output = detail.output;
            *ansi_output = detail.ansi_output;
            *surface_completion = detail.completion;
            *surface_started_at = detail.started_at;
            *surface_completed_at = detail.completed_at;
            *scroll = next_scroll;
        }
        Ok(())
    }

    /// Applies a bounded terminal update without rehydrating the transcript.
    /// The agent has already reconciled its snapshot; this mirrors only the
    /// affected activity card and supervised-work row while keeping scroll and
    /// frontend state intact.
    pub(super) fn apply_terminal_update(
        &mut self,
        update: &cagent_agent::protocol::TerminalTranscriptUpdate,
    ) {
        let terminal = &update.terminal;
        // Terminal updates can be the first notification for a foreground
        // wait:true Bash call. Keep the Background view complete between
        // full snapshots while still avoiding a full session rehydration.
        self.supervised_work.upsert_terminal(terminal.clone());
        if update.id.0.starts_with("detached-bash:") {
            let block = cagent_agent::presentation::detached_bash_transcript_block(terminal);
            if let Some(existing) = self
                .history
                .iter_mut()
                .find(|existing| existing.id == update.id)
            {
                *existing = block;
            } else {
                self.history.push(block);
            }
            self.history_block_layouts
                .retain(|(id, _), _| *id != update.id);
            self.history_layout.rendered = None;
            self.refresh_open_supervised_work_surfaces();
            self.refresh_open_agent_log_surfaces();
            return;
        }
        if !self.supervised_work.is_delegated_terminal(terminal) {
            let Some(node_id) = terminal.tool_call_node_id else {
                self.refresh_open_supervised_work_surfaces();
                self.refresh_open_agent_log_surfaces();
                return;
            };
            let result = cagent_agent::presentation::terminal_activity_result(terminal);
            let status = cagent_agent::presentation::terminal_activity_status(terminal);
            let mut changed = Vec::new();
            for (index, block) in self.history.iter_mut().enumerate() {
                let cagent_agent::protocol::TranscriptBlockKind::ToolGroups { groups } =
                    &mut block.kind
                else {
                    continue;
                };
                for group in groups {
                    if cagent_agent::presentation::update_projected_bash_activity(
                        group, node_id, &result, status,
                    ) {
                        changed.push(index);
                    }
                }
            }
            let changed_ids = changed
                .into_iter()
                .filter_map(|index| self.history.get(index).map(|block| block.id.clone()))
                .collect::<std::collections::HashSet<_>>();
            self.history_block_layouts
                .retain(|(id, _), _| !changed_ids.contains(id));
            self.history_layout.rendered = None;
        }
        self.refresh_open_supervised_work_surfaces();
        self.refresh_open_agent_log_surfaces();
    }

    /// Applies a delegated-terminal update to the supervised-work projections.
    /// Delegated terminals have no conversation node, so they must not enter
    /// the main transcript update path.
    pub(super) fn apply_delegated_terminal_update(
        &mut self,
        terminal: &cagent_agent::tools::TerminalSnapshot,
    ) {
        self.supervised_work.upsert_terminal(terminal.clone());
        self.refresh_open_supervised_work_surfaces();
        self.refresh_open_agent_log_surfaces();
    }

    pub(super) fn apply_delegated_run_update(
        &mut self,
        update: &cagent_agent::protocol::DelegatedRunUpdate,
    ) {
        self.invalidate_open_agent_log(Some(update.run.id));
        self.supervised_work.upsert_agent(update.run.clone());
        if update.run.status.is_terminal() {
            self.supervised_work.remove_live(update.run.id);
        } else {
            let live = self
                .supervised_work
                .agent_run_live
                .get(&update.run.id)
                .cloned()
                .unwrap_or(cagent_agent::presentation::DelegatedRunLive {
                    id: update.run.id,
                    text: String::new(),
                    usage: None,
                });
            self.supervised_work
                .upsert_live(cagent_agent::presentation::DelegatedRunLive {
                    usage: update.run.usage.clone(),
                    ..live
                });
        }
        self.refresh_open_supervised_work_surfaces();
        self.refresh_open_agent_log_surfaces();
    }

    pub(super) fn apply_delegated_text_update(
        &mut self,
        update: &cagent_agent::protocol::DelegatedTextUpdate,
    ) {
        let usage = self
            .supervised_work
            .agent_run_live
            .get(&update.id)
            .and_then(|live| live.usage.clone());
        self.supervised_work
            .upsert_live(cagent_agent::presentation::DelegatedRunLive {
                id: update.id,
                text: update.text.clone(),
                usage,
            });
        self.refresh_open_agent_log_surfaces();
    }

    pub(super) fn apply_startup_resources(
        &mut self,
        status: &cagent_agent::protocol::StartupResourceStatus,
    ) {
        if let cagent_agent::protocol::StartupInstructionsStatus::Failed { message } =
            &status.instructions
        {
            self.set_notice(format!(
                "instructions unavailable · {message} · fix them, then run /reload"
            ));
        }
    }

    /// Rehydrates agent-owned session semantics. Terminal layout, draft editing,
    /// focused surfaces, and scroll state deliberately remain local.
    pub(super) fn apply_session_snapshot(
        &mut self,
        snapshot: &cagent_agent::protocol::SessionSnapshot,
    ) {
        if let Some(preview) = self.history_preview.as_mut() {
            preview.latest_live = snapshot.clone();
            return;
        }
        let _span = tracing::trace_span!(
            "frontend.session.snapshot_apply",
            session_id = %snapshot.conversation_id,
            transcript_blocks = snapshot.transcript.len()
        )
        .entered();
        if self.workspace != snapshot.cwd {
            self.workspace.clone_from(&snapshot.cwd);
            self.files_sidebar.tree = None;
            self.files_sidebar.focused = false;
            self.history_layout.rendered = None;
        }
        let composer_history = snapshot
            .composer_history
            .iter()
            .map(|entry| HistoryEntry {
                kind: entry.kind,
                text: entry.text.clone(),
                attachment_specs: entry.attachment_specs.clone(),
                images: entry.images.clone(),
                image_chips: entry.image_chips.clone(),
            })
            .collect::<Vec<_>>();
        // Submission is acknowledged before its durable user node is written.
        // During that gap the composer has already added the accepted prompt,
        // while a transient snapshot can still contain only the older prefix.
        // Composer history is append-only, so retain that local tail until an
        // authoritative snapshot catches up.
        if self.history_entries != composer_history
            && !composer_history
                .iter()
                .all(|entry| self.history_entries.contains(entry))
        {
            self.hydrate_composer_history(snapshot.composer_history.clone());
        }
        let was_observer = self.is_observer();
        if self.session_id != snapshot.conversation_id {
            self.supervised_work.clear();
        }
        self.session_id = snapshot.conversation_id;
        self.access = snapshot.access;
        if was_observer && !self.is_observer() {
            self.clear_observer_command();
        }
        if self.is_observer() {
            // The observer command palette is frontend-local. Snapshot
            // rehydration must not erase it while the owner is working.
            self.draft.clear();
            self.cursor = 0;
            self.attachments.clear();
            self.pastes.clear();
            self.images.clear();
            self.selected_queue = None;
            self.editing_queue = None;
            self.attachment_completion = None;
            self.pending_interaction = None;
            self.surfaces.retain(Self::observer_surface_allowed);
        }

        // The agent owns the conversation title and publishes it with every
        // snapshot. The terminal is frontend-owned, so defer its update until
        // the controller has applied this snapshot.
        self.conversation_title = snapshot.title.clone();
        let prepended = self.history.first().is_some_and(|first| {
            snapshot
                .transcript
                .iter()
                .position(|block| block.id == first.id)
                .is_some_and(|position| position > 0)
        });
        let previous_history_rows = if prepended && !self.follow_history_tail {
            self.ensure_history_layout(self.render_width);
            self.history_layout
                .rendered
                .as_ref()
                .map_or(0, |layout| layout.rows.len())
        } else {
            0
        };
        if self.transcript_older.is_some() != snapshot.transcript.older.is_some() {
            // Measure the old anchor before changing the visible beginning.
            self.history_layout.rendered = None;
        }
        self.transcript_older = snapshot.transcript.older.clone();
        let previous_history = std::mem::take(&mut self.history);
        let previous_block_layouts = std::mem::take(&mut self.history_block_layouts);
        let previous_materialized_id = previous_history
            .get(self.history_layout.start_block)
            .map(|block| block.id.clone());
        let previous_streaming = std::mem::take(&mut self.streaming);
        let previous_streaming_source = std::mem::take(&mut self.streaming_source);
        let previous_streaming_plan = std::mem::take(&mut self.streaming_plan);
        let previous_streaming_plan_source = std::mem::take(&mut self.streaming_plan_source);
        // Queue snapshots replace the frontend's vector. Preserve a selected
        // message by its durable ID when it is still present, rather than by
        // its old index; deleting or dispatching an earlier item can shift
        // every later index.
        let selected_queue_id = self
            .selected_queue
            .and_then(|index| self.queued.get(index))
            .map(|message| message.id);
        self.queued.clone_from(&snapshot.queue);
        self.selected_queue = selected_queue_id.and_then(|id| {
            self.queued_display_indexes()
                .into_iter()
                .find(|index| self.queued[*index].id == id)
        });
        self.session_usage.clone_from(&snapshot.usage);
        self.provider_usage.clone_from(&snapshot.provider_usage);
        if let Some(context) = &snapshot.context {
            self.context_percent = context.percent_used();
            self.context_used_tokens = Some(context.used_tokens);
            self.context_window = Some(context.context_window);
        } else {
            self.context_percent = None;
            self.context_used_tokens = None;
            self.context_window = None;
        }
        for surface in &mut self.surfaces {
            if let Surface::PlanCompletion {
                context_percent, ..
            } = surface
            {
                *context_percent = self.context_percent;
            }
        }
        if !self.is_observer() {
            self.pending_interaction
                .clone_from(&snapshot.pending_interaction);
        }
        self.pending_terminal_title = Some(self.current_terminal_title());
        self.reconcile_interaction_surfaces();
        self.agent.clone_from(&snapshot.active_agent);
        self.mode.clone_from(&snapshot.active_mode);
        self.fast = snapshot.fast;
        if self.fast_effective != snapshot.fast_effective {
            self.token_rate_tracker = cagent_agent::presentation::TokenRateTracker::default();
        }
        self.fast_effective = snapshot.fast_effective;
        self.enabled_providers = snapshot.enabled_providers.iter().cloned().collect();
        if let cagent_agent::protocol::StartupInstructionsStatus::Failed { message } =
            &snapshot.startup_resources.instructions
        {
            self.set_notice(format!(
                "instructions unavailable · {message} · fix them, then run /reload"
            ));
        }
        self.set_selection(snapshot.model_selection.clone());
        if !snapshot.transcript.iter().any(|block| {
            matches!(
                block.kind,
                cagent_agent::protocol::TranscriptBlockKind::User { .. }
            )
        }) {
            self.welcome = welcome_lines(
                &self.provider_model,
                self.effort.as_deref(),
                !self.enabled_providers.is_empty(),
                self.render_width,
            );
        }
        let pending_interaction = !self.is_observer() && snapshot.pending_interaction.is_some();
        self.active = !matches!(snapshot.turn, cagent_agent::protocol::TurnState::Idle)
            || pending_interaction;
        self.waiting_for_work = matches!(
            snapshot.turn,
            cagent_agent::protocol::TurnState::Waiting { .. }
        ) && !pending_interaction;
        self.thinking = matches!(
            snapshot.turn,
            cagent_agent::protocol::TurnState::Thinking { .. }
        );
        self.token_rate_tracker.observe(
            &self.session_usage,
            (!self.provider_model.is_empty()).then_some(self.provider_model.as_str()),
            self.active && !pending_interaction,
        );
        self.compacting = matches!(
            snapshot.turn,
            cagent_agent::protocol::TurnState::Compacting { .. }
        );
        self.active_plan.clone_from(&snapshot.active_plan);
        self.working_started_at = turn_started_at(&snapshot.turn);
        self.last_activity_at = snapshot
            .last_activity_at
            .as_deref()
            .and_then(timestamp_to_instant)
            .or(self.working_started_at);
        self.hide_working_indicator = matches!(
            &snapshot.turn,
            cagent_agent::protocol::TurnState::Cancelling { .. }
        );
        if matches!(
            snapshot.turn,
            cagent_agent::protocol::TurnState::Idle
                | cagent_agent::protocol::TurnState::Cancelling { .. }
        ) {
            self.reconnect_status = None;
        }
        // `active` and the working-state fields are now current, so rebuild
        // the title after snapshot hydration rather than using the previous
        // turn's decoration.
        self.pending_terminal_title = Some(self.current_terminal_title());
        self.invalidate_open_agent_log(None);
        self.supervised_work
            .hydrate(snapshot.supervised_work.clone(), snapshot.terminals.clone());
        self.supervised_work
            .hydrate_live(snapshot.delegated_live.clone());
        self.refresh_open_supervised_work_surfaces();
        for block in &snapshot.transcript {
            match &block.kind {
                cagent_agent::protocol::TranscriptBlockKind::Assistant {
                    document, source, ..
                } => {
                    if block.status == cagent_agent::protocol::TranscriptBlockStatus::Streaming {
                        self.streaming.clone_from(document);
                        self.streaming_source.clone_from(source);
                    } else {
                        self.history.push(block.clone());
                    }
                }
                cagent_agent::protocol::TranscriptBlockKind::Plan { document, source } => {
                    if block.status == cagent_agent::protocol::TranscriptBlockStatus::Streaming {
                        self.streaming_plan.clone_from(document);
                        self.streaming_plan_source.clone_from(source);
                    } else {
                        self.history.push(block.clone());
                    }
                }
                cagent_agent::protocol::TranscriptBlockKind::Work { .. } => {}
                _ => {
                    self.history.push(block.clone());
                }
            }
        }
        let previous_blocks = previous_history
            .iter()
            .map(|block| (&block.id, block))
            .collect::<std::collections::HashMap<_, _>>();
        let unchanged_ids = self
            .history
            .iter()
            .filter_map(|block| {
                previous_blocks
                    .get(&block.id)
                    .is_some_and(|previous| *previous == block)
                    .then_some(&block.id)
            })
            .collect::<std::collections::HashSet<_>>();
        self.history_block_layouts = previous_block_layouts
            .into_iter()
            .filter(|((id, _), _)| unchanged_ids.contains(id))
            .collect();
        let history_unchanged = previous_history == self.history;
        if !history_unchanged {
            self.history_layout.rendered = None;
            self.history_layout.start_block = previous_materialized_id
                .and_then(|id| self.history.iter().position(|block| block.id == id))
                .unwrap_or_else(|| self.history_layout.start_block.min(self.history.len()));
        }
        if previous_streaming != self.streaming
            || previous_streaming_source != self.streaming_source
        {
            self.streaming_layout = None;
        }
        if previous_streaming_plan != self.streaming_plan
            || previous_streaming_plan_source != self.streaming_plan_source
        {
            self.streaming_plan_layout = None;
        }
        if prepended && !self.follow_history_tail {
            self.ensure_history_layout(self.render_width);
            let next_rows = self
                .history_layout
                .rendered
                .as_ref()
                .map_or(0, |layout| layout.rows.len());
            self.history_scroll = self
                .history_scroll
                .saturating_add(next_rows.saturating_sub(previous_history_rows));
        }
        if self.transcript_home_drain && self.transcript_older.is_none() {
            self.history_scroll = 0;
            self.transcript_home_drain = false;
        }
        self.finish_pending_transcript_scroll();
        self.refresh_open_agent_log_surfaces();
        self.refresh_open_mcp_surfaces();
        // Attach updates deliver a complete snapshot rather than individual
        // interaction events. Reconcile after hydrating the transcript so a
        // completed plan is present before its implementation menu opens.
        self.open_pending_interaction();
    }

    fn reconcile_interaction_surfaces(&mut self) {
        let pending_id = self.pending_interaction.as_ref().map(|request| request.id);
        let mut removed = false;
        self.surfaces.retain(|surface| {
            let surface_id = match surface {
                Surface::Question { request, .. }
                | Surface::Permission { request, .. }
                | Surface::PermissionRuleEdit { request, .. }
                | Surface::PlanCompletion { request, .. } => Some(request.id),
                _ => None,
            };
            let keep = surface_id.is_none() || surface_id == pending_id;
            removed |= !keep;
            keep
        });
        if removed && pending_id.is_none() {
            self.interaction_clears_draft = false;
        }
    }

    pub(super) fn refresh_open_supervised_work_surfaces(&mut self) {
        if !self.supervised_work_surface_open() {
            return;
        }
        let rows = self.supervised_work.rows();
        let mut changed_projection = false;
        for surface in &mut self.surfaces {
            let Surface::SupervisedWork {
                rows: surface_rows,
                list,
                show_past,
            } = surface
            else {
                continue;
            };
            let selected = list.selected.and_then(|selected| {
                surface_rows
                    .iter()
                    .filter(|item| item.active() != *show_past)
                    .nth(selected)
                    .map(supervised_work_key)
            });
            changed_projection |= *surface_rows != rows;
            surface_rows.clone_from(&rows);
            let visible_count = rows
                .iter()
                .filter(|item| item.active() != *show_past)
                .count();
            list.reconcile(ListMode::Selectable, visible_count, VISIBLE_MENU_ITEMS);
            if let Some(selected) = selected
                && let Some(index) = rows
                    .iter()
                    .filter(|item| item.active() != *show_past)
                    .position(|item| supervised_work_key(item) == selected)
            {
                list.select(index, VISIBLE_MENU_ITEMS);
            }
        }
        if changed_projection {
            self.last_picker_click = None;
        }
    }

    pub(super) fn refresh_open_agent_log_surfaces(&mut self) {
        if !self.surfaces.iter().any(|surface| {
            matches!(
                surface,
                Surface::Expanded {
                    view: ExpandedView::AgentLog { .. },
                    ..
                }
            )
        }) {
            return;
        }
        let (_, _, composer_height, _) =
            self.control_heights_within(self.render_width, self.render_height);
        let viewport_rows = usize::from(composer_height);
        for surface in &mut self.surfaces {
            let Surface::Expanded {
                view,
                scroll,
                viewport_rows: stored_viewport_rows,
            } = surface
            else {
                continue;
            };
            let cached_max_scroll = match view {
                ExpandedView::AgentLog { max_scroll, .. } => max_scroll.get(),
                _ => continue,
            };
            let previous_max_scroll = cached_max_scroll.unwrap_or_else(|| {
                super::surfaces::expanded_scroll_metrics(
                    view,
                    *stored_viewport_rows,
                    &self.workspace,
                    self.render_width,
                    viewport_rows,
                )
                .1
            });
            let ExpandedView::AgentLog {
                run,
                streaming,
                terminals,
                ..
            } = view
            else {
                continue;
            };
            let Some(next) = self
                .supervised_work
                .rows
                .iter()
                .find_map(|work| match work {
                    cagent_agent::presentation::SupervisedWork::Agent { run: next }
                        if next.id == run.id =>
                    {
                        Some(next.as_ref())
                    }
                    _ => None,
                })
            else {
                continue;
            };
            let mut next_run = next.clone();
            let live = self.supervised_work.agent_run_live.get(&run.id);
            if let Some(usage) = live.and_then(|live| live.usage.clone()) {
                next_run.usage = Some(usage);
            }
            let next_streaming = live.map(|state| state.text.clone()).unwrap_or_default();
            let next_terminals = self.supervised_work.terminals_for_agent(run);
            let terminals_changed = *terminals != next_terminals;
            // Log bodies belong to the detail view, not lifecycle snapshots.
            // Move them through metadata refreshes instead of copying/replacing them.
            next_run.timeline = std::mem::take(&mut run.timeline);
            next_run.activity = std::mem::take(&mut run.activity);
            let metadata_changed = run.status != next_run.status
                || run.usage != next_run.usage
                || run.result != next_run.result
                || run.error != next_run.error;
            if !metadata_changed && *streaming == next_streaming && !terminals_changed {
                run.timeline = next_run.timeline;
                run.activity = next_run.activity;
                continue;
            }
            **run = next_run;
            *streaming = next_streaming;
            if terminals_changed {
                *terminals = next_terminals;
                self.expanded_text_render_cache.get_mut().take();
            } else {
                super::surfaces::invalidate_expanded_text(&self.expanded_text_render_cache);
            }
            let next_max_scroll = super::surfaces::cached_expanded_scroll_metrics(
                &self.expanded_text_render_cache,
                view,
                *stored_viewport_rows,
                &self.workspace,
                self.render_width,
                viewport_rows,
            )
            .1;
            *scroll = follow_expanded_scroll(*scroll, previous_max_scroll, next_max_scroll);
        }
    }

    fn refresh_open_mcp_surfaces(&mut self) {
        for surface in &mut self.surfaces {
            let Surface::Expanded {
                view: ExpandedView::Mcp { call },
                ..
            } = surface
            else {
                continue;
            };
            let Some(node_id) = call.node_id else {
                continue;
            };
            if call.status != cagent_agent::presentation::ToolActivityStatus::Pending {
                continue;
            }
            // Use stable tool-call identity, not tool name or arguments: identical
            // calls may run concurrently and finish in either order.
            let next = self
                .history
                .iter()
                .filter_map(|block| match &block.kind {
                    cagent_agent::protocol::TranscriptBlockKind::ToolGroups { groups } => {
                        Some(groups)
                    }
                    _ => None,
                })
                .flatten()
                .find_map(|group| match group {
                    cagent_agent::presentation::ToolActivityGroup::Mcp { call: next }
                        if next.node_id == Some(node_id) =>
                    {
                        Some(next)
                    }
                    _ => None,
                });
            if let Some(next) = next
                && call.as_ref() != next
            {
                *call = std::sync::Arc::new(next.clone());
                self.pending_mcp_detail_load = Some(node_id);
                self.expanded_text_render_cache.get_mut().take();
            }
        }
    }

    pub(super) fn apply_mcp_detail(&mut self, detail: cagent_agent::presentation::McpCall) {
        for surface in &mut self.surfaces {
            if let Surface::Expanded { view: ExpandedView::Mcp { call }, .. } = surface
                && detail.node_id.is_some()
                && call.node_id == detail.node_id
                // A pending load can race with the completion snapshot.
                && (call.status == cagent_agent::presentation::ToolActivityStatus::Pending
                    || detail.status != cagent_agent::presentation::ToolActivityStatus::Pending)
            {
                *call = std::sync::Arc::new(detail.clone());
                self.expanded_text_render_cache.get_mut().take();
            }
        }
    }
}

pub(super) fn follow_expanded_scroll(scroll: usize, previous_max: usize, next_max: usize) -> usize {
    if scroll >= previous_max {
        next_max
    } else {
        scroll.min(next_max)
    }
}

fn turn_started_at(turn: &cagent_agent::protocol::TurnState) -> Option<Instant> {
    let started_at = match turn {
        cagent_agent::protocol::TurnState::Working { started_at, .. }
        | cagent_agent::protocol::TurnState::Thinking { started_at, .. }
        | cagent_agent::protocol::TurnState::Waiting { started_at, .. }
        | cagent_agent::protocol::TurnState::Cancelling { started_at, .. }
        | cagent_agent::protocol::TurnState::Compacting { started_at, .. } => started_at,
        cagent_agent::protocol::TurnState::Idle => return None,
    };
    timestamp_to_instant(started_at)
}

fn timestamp_to_instant(timestamp: &str) -> Option<Instant> {
    let timestamp = timestamp.parse::<u128>().ok()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis();
    let elapsed = std::time::Duration::from_millis(
        u64::try_from(now.saturating_sub(timestamp)).unwrap_or(u64::MAX),
    );
    Instant::now()
        .checked_sub(elapsed)
        .or_else(|| Some(Instant::now()))
}
