use super::super::list::ListHit;
use super::super::surface_control::{apply_history_list_action, list_action};
use super::super::surfaces::{
    SurfaceHit, SurfaceTab, cached_expanded_scroll_metrics, cached_permission_diff_max_scroll,
    cached_permission_diff_scroll_metrics, change_help_tab, change_settings_section,
    conversation_picker_item_capacity, expanded_scroll_metrics, history_picker_item_capacity,
    mcp_form_item_count, surface_layout_with_viewport, surface_status, surface_tab_at,
};
#[allow(clippy::wildcard_imports)]
use super::super::*;
use cagent_agent::presentation::{conversation_picker_indexes, history_picker_indexes};

use super::content_activation::status_hint_action;
use super::{keyboard, mouse};

fn agent_web_search_result_candidates(
    run: &cagent_agent::protocol::AgentRun,
    workspace: &std::path::Path,
) -> Vec<(
    String,
    String,
    Vec<cagent_agent::web_search::WebSearchResult>,
)> {
    let log = cagent_agent::presentation::project_agent_run_log(run, workspace);
    log.entries
        .into_iter()
        .filter_map(|entry| match entry {
            cagent_agent::presentation::AgentRunLogEntry::ToolGroups(groups) => Some(groups),
            cagent_agent::presentation::AgentRunLogEntry::Assistant(_) => None,
        })
        .flatten()
        .filter_map(|group| match group {
            cagent_agent::presentation::ToolActivityGroup::WebSearch {
                provider,
                query,
                status: cagent_agent::presentation::ToolActivityStatus::Succeeded,
                results,
                ..
            } => Some((provider, query, results)),
            _ => None,
        })
        .collect()
}

impl App {
    pub(super) fn activate_directory_path(&mut self, path: &std::path::Path) -> bool {
        if !matches!(self.ui_editor, cagent_agent::config::UiEditor::BuiltIn) {
            return false;
        }
        {
            let Some(Surface::Expanded {
                view: ExpandedView::Directory { browser },
                scroll,
                viewport_rows,
            }) = self.surfaces.last_mut()
            else {
                return false;
            };
            let rows = browser.tree.rows();
            let Some((index, row)) = rows.iter().enumerate().find(|(_, row)| row.path == path)
            else {
                return false;
            };
            if !row.is_directory() {
                return false;
            }
            browser.select_index(index);
            browser.tree.toggle(path);
            let capacity = (*viewport_rows).max(1);
            let selected = browser.selected_index();
            if selected < *scroll {
                *scroll = selected;
            } else if selected >= (*scroll).saturating_add(capacity) {
                *scroll = selected.saturating_add(1).saturating_sub(capacity);
            }
        }
        self.expanded_text_render_cache.get_mut().take();
        true
    }

    pub(crate) fn open_path(&mut self, path: std::path::PathBuf) {
        self.open_path_at_line(path, None);
    }

    pub(crate) fn open_path_at_line(&mut self, path: std::path::PathBuf, line: Option<usize>) {
        self.files_sidebar.view_opened_from_sidebar = false;
        match &self.ui_editor {
            cagent_agent::config::UiEditor::Disabled => {}
            cagent_agent::config::UiEditor::Command { mode, .. } => {
                let argv_candidates = self.ui_editor.argv_candidates(&path, line);
                if !argv_candidates.is_empty() {
                    self.pending_action = Some(AppAction::LaunchPath {
                        argv_candidates,
                        mode: *mode,
                        reload_startup_resources: false,
                    });
                }
            }
            cagent_agent::config::UiEditor::BuiltIn => {
                if self.open_builtin_path(&path, false)
                    && let Some(line) = line
                    && let Some(Surface::Expanded {
                        view: ExpandedView::File { view },
                        scroll,
                        ..
                    }) = self.surfaces.last_mut()
                {
                    *scroll = crate::app::surfaces::file_logical_line_offset(
                        view,
                        self.render_width,
                        line,
                    );
                }
            }
        }
    }

    pub(crate) fn open_builtin_path(&mut self, path: &std::path::Path, replace_path: bool) -> bool {
        let view = match cagent_agent::presentation::inspect_path(path) {
            Ok(cagent_agent::presentation::PathView::File(view)) => ExpandedView::File { view },
            Ok(cagent_agent::presentation::PathView::Directory(tree)) => ExpandedView::Directory {
                browser: super::file_tree::DirectoryBrowserState::from_tree(tree, 0),
            },
            Err(error) => {
                self.set_notice(format!(
                    "could not open {} · {error}",
                    path.to_string_lossy()
                ));
                return false;
            }
        };
        self.expanded_text_render_cache.get_mut().take();
        self.image_preview = None;
        let surface = Surface::Expanded {
            view,
            scroll: 0,
            viewport_rows: usize::from(self.render_height.saturating_sub(4).max(1)),
        };
        if replace_path
            && self.surfaces.last().is_some_and(|surface| {
                matches!(
                    surface,
                    Surface::Expanded {
                        view: ExpandedView::File { .. } | ExpandedView::Directory { .. },
                        ..
                    }
                )
            })
        {
            let _ = self.surfaces.pop();
        }
        self.surfaces.push(surface);
        true
    }

    pub(super) async fn activate_slash_command(
        &mut self,
        session: &SessionHandle,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Some(command) = self.accept_slash_command() else {
            return Ok(());
        };
        if self.is_read_only_view() {
            self.dispatch_observer_command(session).await?;
        } else if command
            .argument_hint
            .is_some_and(|hint| hint.starts_with('<'))
        {
            self.replace_draft(&format!("{} ", command.name));
        } else if !command.insert_argument_hint {
            self.submit_draft_deferred(session, QueueTarget::NextBoundary)
                .await?;
        }
        Ok(())
    }

    pub(crate) fn queued_display_indexes(&self) -> Vec<usize> {
        [QueueTarget::NextBoundary, QueueTarget::EndOfTurn]
            .into_iter()
            .flat_map(|target| {
                self.queued
                    .iter()
                    .enumerate()
                    .filter_map(move |(index, message)| (message.target == target).then_some(index))
            })
            .collect()
    }

    fn queue_index_at_display_row(&self, row: usize) -> Option<usize> {
        let mut row = row;
        for target in [QueueTarget::NextBoundary, QueueTarget::EndOfTurn] {
            let indexes = self
                .queued_display_indexes()
                .into_iter()
                .filter(|index| self.queued[*index].target == target)
                .collect::<Vec<_>>();
            if indexes.is_empty() {
                continue;
            }
            if row == 0 {
                return None;
            }
            row -= 1;
            if row < indexes.len() {
                return Some(indexes[row]);
            }
            row -= indexes.len();
        }
        None
    }

    pub(crate) async fn handle_terminal_event(
        &mut self,
        session: &SessionHandle,
        event: Event,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                self.handle_key(session, key).await
            }
            Event::Paste(text) => {
                if self.is_read_only_view()
                    && !self.observer_command_active()
                    && (self.surfaces.is_empty()
                        || !self
                            .surfaces
                            .last()
                            .is_some_and(Self::observer_surface_allowed))
                {
                    return Ok(false);
                }
                if self.surfaces.is_empty() || self.observer_command_active() {
                    self.insert_paste(&text);
                } else {
                    let _ = self.insert_surface_paste(&text);
                }
                Ok(false)
            }
            Event::Mouse(mouse) => {
                if self.handle_files_mouse(mouse) {
                    return Ok(false);
                }
                if self.handle_transcript_scrollbar_mouse(mouse) {
                    return Ok(false);
                }
                if self.handle_mouse_scroll(mouse) {
                    return Ok(false);
                }
                let main_mouse = MouseEvent {
                    column: mouse.column.saturating_sub(
                        self.files_sidebar
                            .area
                            .map_or(0, ratatui::layout::Rect::right),
                    ),
                    ..mouse
                };
                // Anchored surfaces are painted over the transcript. Route
                // their status row first so neither an actionable hint nor
                // inert status padding can click a stale target underneath.
                if main_mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                    let (width, height) = (self.render_width, self.render_height);
                    if self
                        .click_surface_status_action(
                            session,
                            height,
                            main_mouse.row,
                            main_mouse.column,
                        )
                        .await?
                    {
                        return Ok(false);
                    }
                    if self
                        .click_status_line_module(
                            session,
                            width,
                            height,
                            main_mouse.row,
                            main_mouse.column,
                        )
                        .await?
                    {
                        return Ok(false);
                    }
                }
                if mouse.kind == MouseEventKind::Down(MouseButton::Left)
                    && self.open_link_at(mouse.row, mouse.column)
                {
                    return Ok(false);
                }
                if mouse.kind == MouseEventKind::Down(MouseButton::Left)
                    && self.open_image_at(session, mouse.row, mouse.column).await
                {
                    return Ok(false);
                }
                if mouse.kind == MouseEventKind::Down(MouseButton::Left)
                    && self.open_diff_at(mouse.row)
                {
                    return Ok(false);
                }
                if mouse.kind == MouseEventKind::Down(MouseButton::Left)
                    && self.open_path_at(mouse.row, mouse.column)
                {
                    return Ok(false);
                }
                if mouse.kind == MouseEventKind::Down(MouseButton::Left)
                    && self
                        .open_bash_output_at_with_session(session, mouse.row, self.render_height)
                        .await?
                {
                    return Ok(false);
                }
                if mouse.kind == MouseEventKind::Down(MouseButton::Left)
                    && self
                        .open_agent_bash_output_at_with_session(
                            session,
                            mouse.row,
                            self.render_height,
                        )
                        .await?
                {
                    return Ok(false);
                }
                let mouse = main_mouse;
                if mouse.kind == MouseEventKind::Down(MouseButton::Right)
                    && self.close_expanded_view_at(mouse.row, mouse.column)
                {
                    return Ok(false);
                }
                if mouse.kind == MouseEventKind::Down(MouseButton::Right)
                    && self.close_activity_collapse_at(mouse.row)
                {
                    return Ok(false);
                }
                if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                    let (width, height) = (self.render_width, self.render_height);
                    if self.click_surface_tab(width, height, mouse.row, mouse.column) {
                        return Ok(false);
                    }
                    if self.click_expanded_output_indicator(width, height, mouse.row) {
                        return Ok(false);
                    }
                }
                if mouse.kind == MouseEventKind::Down(MouseButton::Left)
                    && self.scroll_indicator_clicked(
                        self.render_width,
                        self.render_height,
                        mouse.row,
                        mouse.column,
                    )
                {
                    return Ok(false);
                }
                if self.is_read_only_view()
                    && !self.observer_command_active()
                    && (self.surfaces.is_empty()
                        || !self
                            .surfaces
                            .last()
                            .is_some_and(Self::observer_surface_allowed))
                {
                    return Ok(false);
                }
                if let Some(completion) = &mut self.attachment_completion {
                    match mouse.kind {
                        MouseEventKind::ScrollUp => {
                            completion.list.apply(
                                ListAction::WheelPrevious,
                                ListMode::Selectable,
                                VISIBLE_MENU_ITEMS,
                            );
                        }
                        MouseEventKind::ScrollDown => {
                            completion.list.apply(
                                ListAction::WheelNext,
                                ListMode::Selectable,
                                VISIBLE_MENU_ITEMS,
                            );
                        }
                        MouseEventKind::Down(MouseButton::Left) => {
                            if self.select_picker_mouse(mouse, PickerClickKind::Attachment) {
                                return self
                                    .handle_surface_key(
                                        session,
                                        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                                    )
                                    .await;
                            }
                        }
                        _ => self.scroll_history_mouse(mouse.kind),
                    }
                } else if !self.slash_suggestions().is_empty() {
                    match mouse.kind {
                        MouseEventKind::ScrollUp => {
                            if self.is_read_only_view() {
                                self.observer_command_list.apply(
                                    ListAction::WheelPrevious,
                                    ListMode::Selectable,
                                    VISIBLE_MENU_ITEMS,
                                );
                            } else {
                                self.slash_list.apply(
                                    ListAction::WheelPrevious,
                                    ListMode::Selectable,
                                    VISIBLE_MENU_ITEMS,
                                );
                            }
                        }
                        MouseEventKind::ScrollDown => {
                            if self.is_read_only_view() {
                                self.observer_command_list.apply(
                                    ListAction::WheelNext,
                                    ListMode::Selectable,
                                    VISIBLE_MENU_ITEMS,
                                );
                            } else {
                                self.slash_list.apply(
                                    ListAction::WheelNext,
                                    ListMode::Selectable,
                                    VISIBLE_MENU_ITEMS,
                                );
                            }
                        }
                        MouseEventKind::Down(MouseButton::Left) => {
                            if self.select_picker_mouse(mouse, PickerClickKind::Slash) {
                                self.activate_slash_command(session).await?;
                                return Ok(false);
                            }
                        }
                        _ => self.scroll_history_mouse(mouse.kind),
                    }
                } else if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                    if self.place_surface_cursor(mouse) {
                        return Ok(false);
                    }
                    if self.select_picker_mouse(mouse, PickerClickKind::Surface) {
                        return self
                            .handle_surface_key(
                                session,
                                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                            )
                            .await;
                    }
                } else {
                    self.scroll_history_mouse(mouse.kind);
                }
                Ok(false)
            }
            _ => Ok(false),
        }
    }

    pub(crate) async fn click_status_line_module(
        &mut self,
        session: &SessionHandle,
        width: u16,
        height: u16,
        row: u16,
        column: u16,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        if !self.surfaces.is_empty()
            || self.active_notice().is_some()
            || !self.slash_suggestions().is_empty()
        {
            return Ok(false);
        }
        let status_height = self.control_heights_within(width, height).3;
        let status_top = height.saturating_sub(status_height);
        if row < status_top {
            return Ok(false);
        }
        let Some(module) = status_line_module_at(
            &self.status_line_config,
            &self.status_line_values(),
            width,
            column,
        ) else {
            return Ok(false);
        };
        if self.is_observer() && !matches!(module, StatusLineModule::Provider) {
            return Ok(false);
        }

        if module == StatusLineModule::Hint
            && status_line_background_hint_at(
                &self.status_line_config,
                &self.status_line_values(),
                width,
                column,
            )
        {
            self.clear_draft();
            self.open_background_surface();
            return Ok(true);
        }

        match module {
            StatusLineModule::Mode => {
                self.clear_draft();
                self.open_mode_surface(session)?;
            }
            StatusLineModule::Agent => {
                self.clear_draft();
                self.open_agent_surface(session)?;
            }
            StatusLineModule::Model => {
                self.clear_draft();
                self.open_model_surface(session).await?;
            }
            StatusLineModule::Provider => {
                self.clear_draft();
                self.open_provider_surface(session).await;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    async fn click_surface_status_action(
        &mut self,
        session: &SessionHandle,
        height: u16,
        row: u16,
        column: u16,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        let Some(surface) = self.surfaces.last() else {
            return Ok(false);
        };
        if row != height.saturating_sub(1) {
            return Ok(false);
        }
        // The whole rendered row belongs to the surface. Notices, padding,
        // separators, disabled bindings, and non-actionable hints are inert,
        // but must still stop dispatch to content behind the row.
        if self.active_notice().is_some() {
            return Ok(true);
        }
        let hints = self.contextual_key_hints(&surface_status(surface));
        // `padded_status_line` reserves two columns on each side.
        let mut start = 2usize;
        for hint in hints.split(" · ") {
            let end = start + hint.width();
            if (start..end).contains(&usize::from(column)) {
                if hint.starts_with("Ctrl+Enter") {
                    let _ = self
                        .handle_surface_key(
                            session,
                            KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL),
                        )
                        .await?;
                    return Ok(true);
                }
                if hint.starts_with("Ctrl+R") {
                    let _ = self
                        .handle_surface_key(
                            session,
                            KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
                        )
                        .await?;
                    return Ok(true);
                }
                if hint.starts_with("Shift+Left/Right/Tab") || hint.starts_with("Shift+Tab") {
                    if let Some(action) =
                        self.key_for_action(cagent_agent::presentation::KeyBindingAction::NextMode)
                    {
                        let _ = self.handle_surface_key(session, action).await?;
                        return Ok(true);
                    }
                }
                let action = status_hint_action(hint);
                if let Some(action) = action.and_then(|action| self.key_for_action(action)) {
                    let _ = self.handle_surface_key(session, action).await?;
                    return Ok(true);
                }
                // Surface-local letter shortcuts are intentionally literal: they
                // are not configurable keybinding actions.
                if let Some(letter) = hint
                    .chars()
                    .next()
                    .filter(|c| matches!(c, 'c' | 'C' | 'd' | 'e' | 'f' | 'k' | 'p' | 'r'))
                {
                    let _ = self
                        .handle_surface_key(
                            session,
                            KeyEvent::new(KeyCode::Char(letter), KeyModifiers::NONE),
                        )
                        .await?;
                    return Ok(true);
                }
                return Ok(true);
            }
            start = end + 3;
        }
        Ok(true)
    }

    /// Routes wheel input to the pane under the pointer. This keeps the
    /// transcript usable while an anchored interaction surface is open.
    pub(super) fn handle_mouse_scroll(&mut self, mouse: MouseEvent) -> bool {
        if !mouse::is_wheel(mouse.kind) {
            return false;
        }

        let (width, height) = (self.render_width, self.render_height);
        self.handle_mouse_scroll_within(mouse, width, height)
    }

    pub(crate) fn handle_transcript_scrollbar_mouse(&mut self, mouse: MouseEvent) -> bool {
        if let Some(mut drag) = self.transcript_scrollbar_drag {
            match mouse.kind {
                MouseEventKind::Drag(MouseButton::Left) => {
                    let mut arm_prefetch = false;
                    if let Some(layout) = self.transcript_scrollbar {
                        let row = usize::from(mouse.row.saturating_sub(layout.area.y));
                        let previous = self.history_scroll;
                        let next =
                            layout.offset_for_thumb_start(row.saturating_sub(drag.grab_offset));
                        self.history_scroll = next;
                        if next < previous && !drag.prefetch_seen {
                            self.prepare_history_scroll_up(previous.saturating_sub(next));
                            drag.prefetch_seen = true;
                            arm_prefetch = true;
                        }
                        self.follow_history_tail = next >= layout.maximum;
                        self.transcript_home_drain = false;
                    }
                    self.transcript_scrollbar_drag = Some(drag);
                    if arm_prefetch {
                        self.arm_transcript_prefetch_if_idle();
                    }
                    return true;
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    self.transcript_scrollbar_drag = None;
                    return true;
                }
                _ => return true,
            }
        }

        let Some(layout) = self.transcript_scrollbar else {
            return false;
        };
        if mouse.kind != MouseEventKind::Down(MouseButton::Left)
            || mouse.column != layout.area.right().saturating_sub(1)
            || mouse.row < layout.area.y
            || mouse.row >= layout.area.bottom()
        {
            return false;
        }

        let row = usize::from(mouse.row - layout.area.y);
        if row == 0 && (self.transcript_older.is_some() || self.history_layout_incomplete()) {
            self.scroll_history_to_top();
            return true;
        }
        let thumb_end = layout.thumb_start.saturating_add(layout.thumb_len);
        let mut prefetch_seen = false;
        let mut arm_prefetch = false;
        let grab_offset = if row >= layout.thumb_start && row < thumb_end {
            row - layout.thumb_start
        } else {
            let centered = row.saturating_sub(layout.thumb_len / 2);
            let previous = self.history_scroll;
            let next = layout.offset_for_thumb_start(centered);
            self.history_scroll = next;
            if next < previous {
                self.prepare_history_scroll_up(previous.saturating_sub(next));
                prefetch_seen = true;
                arm_prefetch = true;
            }
            layout.thumb_len / 2
        };
        self.follow_history_tail = self.history_scroll >= layout.maximum;
        self.transcript_home_drain = false;
        self.transcript_scrollbar_drag = Some(super::TranscriptScrollbarDrag {
            grab_offset,
            prefetch_seen,
        });
        if arm_prefetch {
            self.arm_transcript_prefetch_if_idle();
        }
        true
    }

    pub(crate) fn handle_mouse_scroll_within(
        &mut self,
        mouse: MouseEvent,
        width: u16,
        height: u16,
    ) -> bool {
        let history_bottom = height.saturating_sub(self.controls_height(width, height));
        if mouse.row < history_bottom {
            self.scroll_history_mouse(mouse.kind);
            return true;
        }

        let multiline = self
            .surfaces
            .last()
            .is_some_and(Surface::is_editing_interaction_note)
            || matches!(
                self.surfaces.last(),
                Some(
                    Surface::AgentWizard {
                        step: AgentWizardStep::Prompt,
                        ..
                    } | Surface::McpJsonEdit { .. }
                        | Surface::SkillWizard {
                            step: SkillWizardStep::Content,
                            ..
                        }
                )
            );
        if multiline {
            let controls_height = self.controls_height(width, height);
            let panel_top = height
                .saturating_sub(controls_height)
                .saturating_add(height.saturating_sub(1).min(2));
            let (_, _, composer_height, _) = self.control_heights_within(width, height);
            let relative = usize::from(mouse.row).checked_sub(usize::from(panel_top));
            let over_content = relative.is_some_and(|relative| {
                self.surfaces.last().is_some_and(|surface| {
                    matches!(
                        surface_layout_with_viewport(
                            surface,
                            &self.workspace,
                            width,
                            usize::from(composer_height),
                        )
                        .surface_hit(relative),
                        SurfaceHit::Scroll(super::scroll::ScrollViewHit::Content(_))
                    )
                })
            });
            if over_content {
                let action = if matches!(mouse.kind, MouseEventKind::ScrollUp) {
                    super::scroll::ScrollViewAction::WheelPrevious
                } else {
                    super::scroll::ScrollViewAction::WheelNext
                };
                self.apply_current_surface_scroll_action(action, width, height);
                return true;
            }
        }

        if self.surfaces.is_empty()
            && matches!(
                self.composer_scroll_hit_at(width, height, mouse.row),
                Some(super::scroll::ScrollViewHit::Content(_))
            )
        {
            if !self.draft.is_empty() {
                self.last_composer_input_at = Some(Instant::now());
            }
            let action = if matches!(mouse.kind, MouseEventKind::ScrollUp) {
                super::scroll::ScrollViewAction::WheelPrevious
            } else {
                super::scroll::ScrollViewAction::WheelNext
            };
            self.apply_composer_scroll_action(action, width, height);
            return true;
        }

        self.scroll_expanded_mouse(mouse.kind)
    }

    fn composer_scroll_hit_at(
        &self,
        width: u16,
        height: u16,
        row: u16,
    ) -> Option<super::scroll::ScrollViewHit> {
        if !self.surfaces.is_empty() {
            return None;
        }
        let (queue_rows, popup_rows, composer_rows, status_height) =
            self.control_heights_within(width, height);
        let controls_top = height.saturating_sub(self.controls_height(width, height));
        let sticky_rows = height.saturating_sub(status_height).min(2);
        let top = controls_top
            .saturating_add(sticky_rows.min(1))
            .saturating_add(queue_rows)
            .saturating_add(popup_rows);
        let relative = usize::from(row.checked_sub(top)?);
        self.composer_viewport_layout(width, usize::from(composer_rows.saturating_sub(1)))
            .hit(relative)
    }

    fn composer_is_scrollable(&self, width: u16, height: u16) -> bool {
        let (_, _, composer_rows, _) = self.control_heights_within(width, height);
        let capacity = usize::from(composer_rows.saturating_sub(1));
        self.composer_viewport_layout(width, capacity)
            .scroll
            .metrics
            .maximum_offset
            > 0
    }

    fn apply_composer_scroll_action(
        &mut self,
        action: super::scroll::ScrollViewAction,
        width: u16,
        height: u16,
    ) -> bool {
        if !self.surfaces.is_empty() || (self.is_observer() && !self.observer_command_active()) {
            return false;
        }
        let (_, _, composer_rows, _) = self.control_heights_within(width, height);
        let capacity = usize::from(composer_rows.saturating_sub(1)).max(1);
        let viewport = self.composer_viewport_layout(width, capacity);
        let Some((current, column)) = viewport.caret else {
            return false;
        };
        let page = capacity.max(1);
        let target = match action {
            super::scroll::ScrollViewAction::Previous => current.saturating_sub(1),
            super::scroll::ScrollViewAction::Next => current.saturating_add(1),
            super::scroll::ScrollViewAction::PagePrevious => current.saturating_sub(page),
            super::scroll::ScrollViewAction::PageNext => current.saturating_add(page),
            super::scroll::ScrollViewAction::WheelPrevious => current.saturating_sub(3),
            super::scroll::ScrollViewAction::WheelNext => current.saturating_add(3),
            super::scroll::ScrollViewAction::Home => 0,
            super::scroll::ScrollViewAction::End => viewport.ranges.len().saturating_sub(1),
        }
        .min(viewport.ranges.len().saturating_sub(1));
        if target == current {
            return false;
        }
        let desired = self.preferred_column.unwrap_or(column);
        let Some(rendered_offset) = viewport.rendered_offset_at(target, desired) else {
            return false;
        };
        self.cursor = viewport
            .source_offset_at(rendered_offset)
            .min(self.draft.len());
        self.preferred_column = Some(desired);
        self.composer_scroll = self.composer_viewport_layout(width, capacity).state;
        true
    }

    pub(crate) fn scroll_expanded_mouse(&mut self, kind: MouseEventKind) -> bool {
        if !mouse::is_wheel(kind) {
            return false;
        }
        let action = match mouse::wheel_action(kind) {
            Some(super::scroll::ScrollViewAction::PagePrevious) => ListAction::WheelPrevious,
            Some(super::scroll::ScrollViewAction::PageNext) => ListAction::WheelNext,
            Some(_) | None => return false,
        };
        if let Some(Surface::McpMutationPreview {
            preview, details, ..
        }) = self.surfaces.last_mut()
        {
            let content_rows = super::surfaces::mcp_mutation_detail_lines(preview).len();
            details.reconcile(content_rows, VISIBLE_HELP_ITEMS, false);
            details.apply(action.into(), VISIBLE_HELP_ITEMS);
            return true;
        }
        if !matches!(
            self.surfaces.last(),
            Some(Surface::Expanded { .. } | Surface::Permission { .. })
        ) && self.apply_current_surface_list_action(action).is_some()
        {
            return true;
        }
        let (_, _, composer_height, _) =
            self.control_heights_within(self.render_width, self.render_height);
        let permission_viewport_rows = usize::from(composer_height.saturating_sub(1));
        let Some(surface) = self.surfaces.last() else {
            return false;
        };
        let maximum = match surface {
            Surface::Expanded {
                view,
                viewport_rows,
                ..
            } => {
                let panel_rows = if view.fills_bottom_row() {
                    composer_height
                } else {
                    composer_height.saturating_sub(1)
                };
                if let ExpandedView::Terminal {
                    ansi_output,
                    completion,
                    ..
                } = view
                {
                    self.cached_terminal_scrollback_max(
                        ansi_output,
                        completion.as_deref(),
                        self.render_width.saturating_sub(4),
                        u16::try_from(*viewport_rows).unwrap_or(u16::MAX),
                    )
                    .unwrap_or_else(|| {
                        expanded_scroll_metrics(
                            view,
                            *viewport_rows,
                            &self.workspace,
                            self.render_width,
                            usize::from(panel_rows),
                        )
                        .1
                    })
                } else {
                    cached_expanded_scroll_metrics(
                        &self.expanded_text_render_cache,
                        view,
                        *viewport_rows,
                        &self.workspace,
                        self.render_width,
                        usize::from(panel_rows),
                    )
                    .1
                }
            }
            Surface::Permission { .. } => cached_permission_diff_max_scroll(
                &self.permission_diff_render_cache,
                surface,
                &self.workspace,
                self.render_width,
                permission_viewport_rows,
            ),
            _ => return false,
        };
        match self.surfaces.last_mut() {
            Some(Surface::Expanded { scroll, .. }) => {
                *scroll = (*scroll).min(maximum);
                if matches!(kind, MouseEventKind::ScrollUp) {
                    move_scroll_offset(scroll, maximum, -3);
                } else {
                    move_scroll_offset(scroll, maximum, 3);
                }
            }
            Some(Surface::Permission { diff_scroll, .. }) => match kind {
                MouseEventKind::ScrollUp => {
                    move_scroll_offset(diff_scroll, maximum, -3);
                }
                MouseEventKind::ScrollDown => {
                    move_scroll_offset(diff_scroll, maximum, 3);
                }
                _ => unreachable!("non-scroll event was rejected above"),
            },
            _ => unreachable!("expanded surface changed without yielding"),
        }
        true
    }

    /// Applies one navigation transition to whichever list-backed surface is
    /// active. `Some` means the surface owns list navigation, even when the
    /// state was already clamped at an edge.
    pub(crate) fn apply_current_surface_list_action(&mut self, action: ListAction) -> Option<bool> {
        let (_, _, composer_height, _) =
            self.control_heights_within(self.render_width, self.render_height);
        let viewport_rows = usize::from(composer_height);
        let history_capacity = history_picker_item_capacity(viewport_rows);
        let surface = self.surfaces.last_mut()?;
        let changed = match surface {
            Surface::Providers { list, .. }
            | Surface::Permissions { list, .. }
            | Surface::Usage { list, .. }
            | Surface::UsageBreakdown { list, .. }
            | Surface::Models { list, .. }
            | Surface::Effort { list, .. }
            | Surface::Profiles { list, .. }
            | Surface::Worktrees { list, .. }
            | Surface::AgentEdit { list, .. }
            | Surface::McpServers { list, .. }
            | Surface::McpTools { list, .. }
            | Surface::McpForm { list, .. }
            | Surface::McpMutationPreview { list, .. }
            | Surface::Paths { list, .. }
            | Surface::SettingChoices { list, .. }
            | Surface::StatusLine {
                list,
                mode: StatusLineEditorMode::Modules,
                ..
            }
            | Surface::SupervisedWork { list, .. } => {
                list.apply(action, ListMode::Selectable, VISIBLE_MENU_ITEMS)
            }
            Surface::Settings { list, query, .. } => {
                list.apply(action, ListMode::Selectable, settings_item_capacity(query))
            }
            Surface::WebSearchPicker {
                list, rows, query, ..
            } => {
                let changed = list.apply(action, ListMode::Selectable, VISIBLE_MENU_ITEMS);
                let visible = rows
                    .iter()
                    .filter(|row| row.provider.label().contains(&query.to_lowercase()))
                    .collect::<Vec<_>>();
                if list
                    .selected
                    .and_then(|selected| selected.checked_sub(1))
                    .and_then(|index| visible.get(index))
                    .is_some_and(|row| !row.ready)
                {
                    if let Some(index) = visible.iter().position(|row| row.ready) {
                        list.select(index + 1, VISIBLE_MENU_ITEMS);
                    } else {
                        list.select(0, VISIBLE_MENU_ITEMS);
                    }
                }
                changed
            }
            Surface::Conversations {
                rows,
                list,
                query,
                include_archived,
                ..
            } => {
                let visible = conversation_picker_indexes(rows, query);
                let capacity = conversation_picker_item_capacity(
                    viewport_rows,
                    *include_archived,
                    !query.is_empty() && visible.is_empty(),
                );
                list.apply(action, ListMode::Selectable, capacity)
            }
            Surface::HistoryTree {
                rows, list, query, ..
            } => {
                let visible = history_picker_indexes(rows, query);
                let before = *list;
                apply_history_list_action(list, action, rows, &visible, history_capacity);
                *list != before
            }
            Surface::Help { list, .. } => list.apply(action.into(), VISIBLE_HELP_ITEMS),
            Surface::AgentWizard {
                step: AgentWizardStep::Parent,
                parent_list,
                ..
            } => parent_list.apply(action, ListMode::Selectable, VISIBLE_MENU_ITEMS),
            Surface::StatusLine {
                mode: StatusLineEditorMode::Colors { list, .. },
                ..
            } => list.apply(action, ListMode::Selectable, VISIBLE_MENU_ITEMS),
            Surface::AgentWizard {
                step: AgentWizardStep::Availability,
                availability,
                ..
            } => {
                let mut list = ListState::selectable_at(3, *availability, VISIBLE_MENU_ITEMS);
                let changed = list.apply(action, ListMode::Selectable, VISIBLE_MENU_ITEMS);
                *availability = list.selected.unwrap_or(0);
                changed
            }
            Surface::Question {
                request,
                question_index,
                option_index,
                editing_note: false,
                ..
            } => {
                let InteractionRequestKind::Question { questions } = &request.kind else {
                    return None;
                };
                let item_count = questions
                    .get(*question_index)
                    .map_or(0, |question| question.options.len() + 1);
                let mut list =
                    ListState::selectable_at(item_count, *option_index, VISIBLE_MENU_ITEMS);
                let changed = list.apply(action, ListMode::Selectable, VISIBLE_MENU_ITEMS);
                *option_index = list.selected.unwrap_or(0);
                changed
            }
            Surface::PlanCompletion {
                selected,
                editing_note: false,
                ..
            } => {
                let mut list = ListState::selectable_at(4, *selected, VISIBLE_MENU_ITEMS);
                let changed = list.apply(action, ListMode::Selectable, VISIBLE_MENU_ITEMS);
                *selected = list.selected.unwrap_or(0);
                changed
            }
            Surface::McpServer {
                server, selected, ..
            } => {
                let mut list = ListState::selectable_at(
                    mcp_server_menu_item_count(server),
                    *selected,
                    VISIBLE_MENU_ITEMS,
                );
                let changed = list.apply(action, ListMode::Selectable, VISIBLE_MENU_ITEMS);
                *selected = list.selected.unwrap_or(0);
                changed
            }
            Surface::McpPackageSetup { draft, selected } => {
                let mut list = ListState::selectable_at(
                    1 + draft.package.parameters.len() + draft.package.secrets.len(),
                    *selected,
                    VISIBLE_MENU_ITEMS,
                );
                let changed = list.apply(action, ListMode::Selectable, VISIBLE_MENU_ITEMS);
                *selected = list.selected.unwrap_or(0);
                changed
            }
            Surface::McpRemoveConfirm { selected, .. } => {
                let mut list = ListState::selectable_at(2, *selected, VISIBLE_MENU_ITEMS);
                let changed = list.apply(action, ListMode::Selectable, VISIBLE_MENU_ITEMS);
                *selected = list.selected.unwrap_or(0);
                changed
            }
            Surface::KillSupervisedWork { selected, .. } => {
                let mut list = ListState::selectable_at(2, *selected, VISIBLE_MENU_ITEMS);
                let changed = list.apply(action, ListMode::Selectable, VISIBLE_MENU_ITEMS);
                *selected = list.selected.unwrap_or(0);
                changed
            }
            Surface::McpAddScope { selected } => {
                let mut list = ListState::selectable_at(3, *selected, VISIBLE_MENU_ITEMS);
                let changed = list.apply(action, ListMode::Selectable, VISIBLE_MENU_ITEMS);
                *selected = list.selected.unwrap_or(0);
                changed
            }
            Surface::McpTransport { selected, .. } => {
                let mut list = ListState::selectable_at(4, *selected, VISIBLE_MENU_ITEMS);
                let changed = list.apply(action, ListMode::Selectable, VISIBLE_MENU_ITEMS);
                *selected = list.selected.unwrap_or(0);
                changed
            }
            _ => return None,
        };
        Some(changed)
    }

    fn scroll_history_mouse(&mut self, kind: MouseEventKind) {
        match kind {
            MouseEventKind::ScrollUp => self.scroll_history_up(3),
            MouseEventKind::ScrollDown => self.scroll_history_down(3),
            _ => {}
        }
    }

    fn apply_current_multiline_scroll_action(
        &mut self,
        action: super::scroll::ScrollViewAction,
        width: u16,
        height: u16,
    ) -> Option<bool> {
        if matches!(
            action,
            super::scroll::ScrollViewAction::Home | super::scroll::ScrollViewAction::End
        ) {
            return None;
        }
        let (_, _, composer_height, _) = self.control_heights_within(width, height);
        let capacity = super::surfaces::multiline_content_capacity(usize::from(composer_height));
        let Some(surface) = self.surfaces.last_mut() else {
            return None;
        };
        if surface.is_editing_interaction_note() {
            surface
                .interaction_note_mut()?
                .apply_scroll_action(action, width);
            return Some(true);
        }
        match surface {
            Surface::AgentWizard {
                prompt,
                step: AgentWizardStep::Prompt,
                ..
            }
            | Surface::McpJsonEdit { editor: prompt, .. }
            | Surface::SkillWizard {
                content: prompt,
                step: SkillWizardStep::Content,
                ..
            } => {
                prompt.apply_scroll_action(action, width, capacity);
                Some(true)
            }
            _ => None,
        }
    }

    pub(crate) fn apply_current_surface_scroll_action(
        &mut self,
        action: super::scroll::ScrollViewAction,
        width: u16,
        height: u16,
    ) -> bool {
        if self
            .apply_current_multiline_scroll_action(action, width, height)
            .is_some()
        {
            return true;
        }
        let (_, _, composer_height, _) = self.control_heights_within(width, height);
        let permission_viewport = usize::from(composer_height.saturating_sub(1));
        let permission_metrics = self.surfaces.last().and_then(|surface| {
            matches!(surface, Surface::Permission { .. }).then(|| {
                cached_permission_diff_scroll_metrics(
                    &self.permission_diff_render_cache,
                    surface,
                    &self.workspace,
                    width,
                    permission_viewport,
                )
            })
        });
        let expanded_metrics = self.surfaces.last().and_then(|surface| match surface {
            Surface::Expanded {
                view,
                viewport_rows,
                ..
            } => match view {
                ExpandedView::Terminal {
                    ansi_output,
                    completion,
                    ..
                } => {
                    let capacity = usize::from(composer_height.saturating_sub(3).max(1));
                    let maximum = self
                        .cached_terminal_scrollback_max(
                            ansi_output,
                            completion.as_deref(),
                            width.saturating_sub(4),
                            u16::try_from(capacity).unwrap_or(u16::MAX),
                        )
                        .unwrap_or_else(|| {
                            crate::render::terminal_scrollback_max_with_completion(
                                ansi_output,
                                completion.as_deref(),
                                width.saturating_sub(4),
                                u16::try_from(capacity).unwrap_or(u16::MAX),
                            )
                        });
                    Some((capacity, maximum))
                }
                _ => Some(cached_expanded_scroll_metrics(
                    &self.expanded_text_render_cache,
                    view,
                    *viewport_rows,
                    &self.workspace,
                    width,
                    usize::from(composer_height),
                )),
            },
            _ => None,
        });
        match self.surfaces.last_mut() {
            Some(Surface::Help { list, .. }) => {
                list.apply(action, VISIBLE_HELP_ITEMS);
                true
            }
            Some(Surface::McpMutationPreview {
                preview, details, ..
            }) => {
                let content_rows = super::surfaces::mcp_mutation_detail_lines(preview).len();
                let capacity = usize::from(composer_height)
                    .saturating_sub(7)
                    .min(VISIBLE_HELP_ITEMS);
                details.reconcile(content_rows, capacity, false);
                details.apply(action, capacity);
                true
            }
            Some(Surface::Permission { diff_scroll, .. }) => {
                let Some((capacity, maximum)) = permission_metrics else {
                    return false;
                };
                let mut state = super::scroll::ScrollViewState {
                    offset: *diff_scroll,
                    content_rows: maximum.saturating_add(capacity),
                };
                state.apply(action, capacity);
                *diff_scroll = state.offset;
                true
            }
            Some(Surface::Expanded { scroll, .. }) => {
                let Some((capacity, maximum)) = expanded_metrics else {
                    return false;
                };
                let mut state = super::scroll::ScrollViewState {
                    offset: *scroll,
                    content_rows: maximum.saturating_add(capacity),
                };
                state.apply(action, capacity);
                *scroll = state.offset;
                true
            }
            _ => false,
        }
    }

    pub(crate) fn scroll_indicator_clicked(
        &mut self,
        width: u16,
        height: u16,
        row: u16,
        column: u16,
    ) -> bool {
        if self.follow_history_tail || !self.surfaces.is_empty() || column != 2 {
            return false;
        }
        let (_, _, _, status) = self.control_heights_within(width, height);
        let indicator_row = height.saturating_sub(self.controls_height(width, height));
        if height <= status || row != indicator_row {
            return false;
        }
        self.follow_history_tail = true;
        true
    }

    fn close_expanded_view_at(&mut self, row: u16, column: u16) -> bool {
        if !matches!(self.surfaces.last(), Some(Surface::Expanded { .. })) {
            return false;
        }
        let (_, _, composer_height, status_height) =
            self.control_heights_within(self.render_width, self.render_height);
        let bottom = self.render_height.saturating_sub(status_height);
        let top = bottom.saturating_sub(composer_height);
        if column >= self.render_width || row < top || row >= bottom {
            return false;
        }
        self.surfaces.pop();
        self.last_picker_click = None;
        self.open_pending_interaction();
        true
    }

    pub(crate) fn click_expanded_output_indicator(
        &mut self,
        width: u16,
        height: u16,
        row: u16,
    ) -> bool {
        let controls_height = self.controls_height(width, height);
        let (_, _, composer_height, status_height) = self.control_heights_within(width, height);
        // Expanded surfaces begin below the shared scroll-indicator and
        // padding rows, so their hit coordinates are relative to the composer
        // rather than the complete controls area.
        let surface_top = controls_height
            .saturating_sub(composer_height)
            .saturating_sub(status_height);
        let Some(relative) = usize::from(row).checked_sub(usize::from(
            height
                .saturating_sub(controls_height)
                .saturating_add(surface_top),
        )) else {
            return false;
        };
        let (action, capacity, maximum, current) = match self.surfaces.last() {
            Some(Surface::Expanded {
                view,
                scroll,
                viewport_rows,
            }) if matches!(view, ExpandedView::Terminal { .. }) => {
                let panel_rows = if view.fills_bottom_row() {
                    composer_height
                } else {
                    composer_height.saturating_sub(1)
                };
                let capacity = usize::from(composer_height.saturating_sub(3));
                let maximum = self
                    .cached_terminal_scrollback_max(
                        match view {
                            ExpandedView::Terminal { ansi_output, .. } => ansi_output,
                            _ => unreachable!("terminal expanded view was just matched"),
                        },
                        match view {
                            ExpandedView::Terminal { completion, .. } => completion.as_deref(),
                            _ => unreachable!("terminal expanded view was just matched"),
                        },
                        width.saturating_sub(4),
                        u16::try_from(*viewport_rows).unwrap_or(u16::MAX),
                    )
                    .unwrap_or_else(|| {
                        expanded_scroll_metrics(
                            view,
                            *viewport_rows,
                            &self.workspace,
                            width,
                            usize::from(panel_rows),
                        )
                        .1
                    });
                let state = super::scroll::ScrollViewState {
                    offset: (*scroll).min(maximum),
                    content_rows: maximum.saturating_add(capacity),
                };
                let Some(hit_row) = relative.checked_sub(1) else {
                    return false;
                };
                let super::scroll::ScrollViewHit::Indicator(action) =
                    super::scroll::ScrollViewWidget::new(capacity)
                        .indicators(
                            super::scroll::ScrollViewAction::Home,
                            super::scroll::ScrollViewAction::End,
                        )
                        .render(&state, |_| Line::default())
                        .hits
                        .get(hit_row)
                        .copied()
                        .unwrap_or(super::scroll::ScrollViewHit::None)
                else {
                    return false;
                };
                (action, capacity, maximum, *scroll)
            }
            Some(Surface::Expanded {
                view,
                scroll,
                viewport_rows,
            }) => {
                let Some((action, capacity, maximum)) =
                    super::surfaces::cached_expanded_indicator_action(
                        &self.expanded_text_render_cache,
                        view,
                        *scroll,
                        *viewport_rows,
                        &self.workspace,
                        width,
                        usize::from(composer_height),
                        relative,
                    )
                else {
                    return false;
                };
                (action, capacity, maximum, *scroll)
            }
            _ => return false,
        };
        let state = super::scroll::ScrollViewState {
            offset: current.min(maximum),
            content_rows: maximum.saturating_add(capacity),
        };
        let Some(Surface::Expanded { scroll, .. }) = self.surfaces.last_mut() else {
            return false;
        };
        let mut state = state;
        if !state.apply(action, capacity) {
            return false;
        }
        *scroll = state.offset;
        true
    }

    fn click_surface_tab(&mut self, width: u16, height: u16, row: u16, column: u16) -> bool {
        let controls_height = self.controls_height(width, height);
        let panel_top = height
            .saturating_sub(controls_height)
            .saturating_add(height.saturating_sub(1).min(2));
        let Some(relative) = usize::from(row).checked_sub(usize::from(panel_top)) else {
            return false;
        };
        let Some(tab) = self
            .surfaces
            .last()
            .and_then(|surface| surface_tab_at(surface, relative, usize::from(column)))
        else {
            return false;
        };

        match (self.surfaces.last_mut(), tab) {
            (
                Some(Surface::Help {
                    tab,
                    command_rows,
                    key_rows,
                    list,
                }),
                SurfaceTab::Help(next),
            ) => {
                change_help_tab(tab, command_rows, key_rows, list, next);
            }
            (
                Some(Surface::Settings {
                    rows,
                    section,
                    list,
                    query,
                    ..
                }),
                SurfaceTab::Settings(next),
            ) if query.is_empty() => {
                change_settings_section(rows, section, list, next);
            }
            _ => return false,
        }
        self.last_picker_click = None;
        true
    }

    fn select_picker_mouse(&mut self, mouse: MouseEvent, kind: PickerClickKind) -> bool {
        let (width, height) = (self.render_width, self.render_height);
        let controls_height = self.controls_height(width, height);
        let popup_index = usize::from(mouse.row)
            .checked_sub(usize::from(height.saturating_sub(controls_height)))
            .and_then(|relative| relative.checked_sub(1 + self.queue_lines(width).len()))
            .filter(|index| *index < self.completion_lines(width).len());
        let selectable = match kind {
            PickerClickKind::Attachment | PickerClickKind::Slash => popup_index
                .is_some_and(|index| matches!(self.completion_hit(index), ListHit::Item(_))),
            PickerClickKind::Surface => match self.surfaces.last() {
                Some(Surface::Profiles { rows, .. }) => self
                    .surface_option_at(width, height, mouse.row)
                    .and_then(|option| rows.get(option))
                    .is_some_and(|row| row.2),
                Some(Surface::HistoryTree { rows, query, .. }) => self
                    .surface_option_at(width, height, mouse.row)
                    .and_then(|position| history_picker_indexes(rows, query).get(position).copied())
                    .is_some_and(|source| rows[source].selectable),
                _ => self.surface_option_at(width, height, mouse.row).is_some(),
            },
        };
        if !selectable {
            self.last_picker_click = None;
            self.select_mouse_at(width, height, mouse.row, mouse.column);
            return false;
        }
        self.select_mouse_at(width, height, mouse.row, mouse.column);
        let now = Instant::now();
        let target = if kind == PickerClickKind::Surface {
            self.surfaces.last().and_then(|surface| {
                self.surface_selected_option()
                    .map(|item| PickerClickTarget::Surface {
                        kind: std::mem::discriminant(surface),
                        item,
                    })
            })
        } else if kind == PickerClickKind::Attachment {
            self.attachment_completion
                .as_ref()
                .and_then(|completion| completion.list.selected)
                .map(PickerClickTarget::Attachment)
        } else {
            let selected = if self.is_read_only_view() {
                self.observer_command_list.selected
            } else {
                self.slash_list.selected
            };
            selected.map(PickerClickTarget::Slash)
        };
        let double_click = target.is_some_and(|target| {
            self.last_picker_click.is_some_and(|(then, previous)| {
                previous == target && now.duration_since(then) <= Duration::from_millis(500)
            })
        });
        self.last_picker_click = target.map(|target| (now, target));
        double_click
    }

    /// Places a cursor only when the click lands in the active editor recorded
    /// by the surface's rendered layout. It deliberately does not focus a new
    /// field or affect the composer.
    fn place_surface_cursor(&mut self, mouse: MouseEvent) -> bool {
        let (width, height) = (self.render_width, self.render_height);
        let controls_height = self.controls_height(width, height);
        let panel_top = height
            .saturating_sub(controls_height)
            .saturating_add(height.saturating_sub(1).min(2));
        let Some(relative) = usize::from(mouse.row).checked_sub(usize::from(panel_top)) else {
            return false;
        };
        let (_, _, composer_height, _) = self.control_heights_within(width, height);
        let viewport_rows = if matches!(self.surfaces.last(), Some(Surface::Permission { .. })) {
            usize::from(composer_height.saturating_sub(1))
        } else {
            usize::from(composer_height)
        };
        let cursor = self.surfaces.last().and_then(|surface| {
            surface_layout_with_viewport(surface, &self.workspace, width, viewport_rows)
                .input_cursor_at(relative, usize::from(mouse.column))
        });
        let Some(cursor) = cursor else {
            return false;
        };
        if self.set_multiline_surface_cursor(cursor, width, viewport_rows) {
            self.last_picker_click = None;
            return true;
        }
        let Some(surface) = self.surfaces.last_mut() else {
            return false;
        };
        if surface.is_editing_interaction_note() {
            let Some(mut note) = surface.interaction_note_mut() else {
                return false;
            };
            note.set_cursor(cursor, width);
            return true;
        }
        match surface {
            Surface::PermissionRuleEdit { cursor: target, .. }
            | Surface::Rename { cursor: target, .. }
            | Surface::WebSearchPicker {
                query_cursor: target,
                ..
            }
            | Surface::WebSearchSetup { cursor: target, .. }
            | Surface::McpFieldEdit { cursor: target, .. }
            | Surface::Providers {
                query_cursor: target,
                ..
            }
            | Surface::Models {
                query_cursor: target,
                ..
            }
            | Surface::Settings {
                query_cursor: target,
                ..
            }
            | Surface::HistoryTree {
                query_cursor: target,
                ..
            }
            | Surface::Conversations {
                query_cursor: target,
                ..
            }
            | Surface::SettingInput { cursor: target, .. }
            | Surface::StatusLine {
                mode: StatusLineEditorMode::Hex { cursor: target, .. },
                ..
            } => *target = cursor,
            Surface::AgentWizard {
                cursor: target,
                step: AgentWizardStep::Name | AgentWizardStep::Description,
                ..
            } => {
                *target = cursor;
            }
            Surface::SkillWizard {
                name,
                step: SkillWizardStep::Name,
                ..
            } => name.set_cursor(cursor),
            Surface::SkillWizard {
                description,
                step: SkillWizardStep::Description,
                ..
            } => description.set_cursor(cursor),
            Surface::ProviderSetup {
                api_key_auth: true,
                api_key_cursor,
                api_key,
                ..
            } => *api_key_cursor = cursor.min(api_key.len()),
            _ => return false,
        }
        self.last_picker_click = None;
        true
    }

    fn set_multiline_surface_cursor(
        &mut self,
        cursor: usize,
        width: u16,
        viewport_rows: usize,
    ) -> bool {
        let Some(surface) = self.surfaces.last_mut() else {
            return false;
        };
        match surface {
            Surface::AgentWizard {
                prompt,
                step: AgentWizardStep::Prompt,
                ..
            }
            | Surface::McpJsonEdit { editor: prompt, .. }
            | Surface::SkillWizard {
                content: prompt,
                step: SkillWizardStep::Content,
                ..
            } => {
                prompt.set_cursor_in_view(
                    cursor,
                    width,
                    super::surfaces::multiline_content_capacity(viewport_rows),
                );
                true
            }
            _ => false,
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn select_mouse_at(&mut self, width: u16, height: u16, row: u16, column: u16) {
        if self.open_mcp_call_at(row, height) {
            return;
        }
        if self.open_agent_log_at(row, height) {
            return;
        }
        if self.open_agent_web_search_results_at(row, height) {
            return;
        }
        if self.open_agent_bash_output_at(row, height) {
            return;
        }
        if self.toggle_exploration_at(row, width, height) {
            return;
        }
        if self.surfaces.is_empty()
            && self
                .working_tasks_hit
                .as_ref()
                .is_some_and(|hit| hit.row == row && hit.columns.contains(&column))
        {
            self.open_background_surface();
            return;
        }
        if self.is_observer() && !self.observer_command_active() {
            return;
        }
        if self.open_bash_output_at(row, height) {
            return;
        }
        if self.open_web_search_results_at(row, height) {
            return;
        }
        if self.open_web_fetch_output_at(row, height) {
            return;
        }
        if self.open_compaction_at(row, height) {
            return;
        }
        // Expanded activity runs keep the outer collapse target on their
        // detail rows so an otherwise inert row can still hide the run. Give
        // controls inside that run priority over the outer target.
        if self.toggle_activity_collapse_at(row) {
            return;
        }
        if self.is_observer()
            && (self.surfaces.is_empty()
                || !self
                    .surfaces
                    .last()
                    .is_some_and(Self::observer_surface_allowed))
        {
            return;
        }
        if self.click_expanded_output_indicator(width, height, row) {
            return;
        }
        if self.click_surface_tab(width, height, row, column) {
            return;
        }
        let controls_height = self.controls_height(width, height);
        // The renderer reserves the scroll-indicator and padding rows above
        // every anchored surface. Mouse coordinates must start at the actual
        // composer panel, not at the enclosing controls area.
        let panel_top =
            height
                .saturating_sub(controls_height)
                .saturating_add(if self.surfaces.is_empty() {
                    0
                } else {
                    height.saturating_sub(1).min(2)
                });
        let Some(relative) = usize::from(row).checked_sub(usize::from(panel_top)) else {
            return;
        };
        let (_, _, composer_height, _) = self.control_heights_within(width, height);
        let viewport_rows = if matches!(self.surfaces.last(), Some(Surface::Permission { .. })) {
            usize::from(composer_height.saturating_sub(1))
        } else {
            usize::from(composer_height)
        };
        let layout = self.surfaces.last().map(|surface| {
            surface_layout_with_viewport(surface, &self.workspace, width, viewport_rows)
        });
        let surface_hit = layout
            .as_ref()
            .and_then(|layout| layout.hits.get(relative).copied())
            .unwrap_or(ListHit::None);
        if let Some(SurfaceHit::Scroll(super::scroll::ScrollViewHit::Indicator(action))) =
            layout.as_ref().map(|layout| layout.surface_hit(relative))
            && self.apply_current_surface_scroll_action(action, width, height)
        {
            return;
        }
        if let Some(SurfaceHit::DirectoryItem(index)) =
            layout.as_ref().map(|layout| layout.surface_hit(relative))
            && self.click_expanded_directory_index(index)
        {
            return;
        }
        match surface_hit {
            ListHit::Up | ListHit::Down => {
                self.apply_current_surface_list_action(if surface_hit == ListHit::Up {
                    ListAction::PagePrevious
                } else {
                    ListAction::PageNext
                });
                return;
            }
            ListHit::Item(option)
                if !matches!(
                    self.surfaces.last(),
                    Some(Surface::StatusLine { .. } | Surface::Help { .. })
                ) =>
            {
                if self.set_surface_selected_option(option) {
                    return;
                }
            }
            _ => {}
        }
        if let Some(surface) = self.surfaces.last_mut() {
            match surface {
                Surface::PlanCompletion { selected, .. } => {
                    // Plan menus include a question and a spacer after their
                    // title, so their choices start on panel row four.
                    if let Some(index) = relative.checked_sub(4).filter(|index| *index < 4) {
                        *selected = index;
                    }
                }
                Surface::StatusLine {
                    config,
                    rows,
                    list,
                    mode,
                    ..
                } => match mode {
                    StatusLineEditorMode::Modules => {
                        let ListHit::Item(index) = surface_hit else {
                            return;
                        };
                        list.select(index, VISIBLE_MENU_ITEMS);
                        let module = rows[index];
                        match column {
                            4..=5 => {
                                let enabled = !config.modules.contains(&module);
                                set_status_line_module_enabled(config, rows, module, enabled);
                                let selected = rows
                                    .iter()
                                    .position(|candidate| *candidate == module)
                                    .unwrap_or(0);
                                list.select(selected, VISIBLE_MENU_ITEMS);
                            }
                            8.. if status_line_module_color_editable(module) => {
                                *mode = StatusLineEditorMode::Colors {
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
                                return;
                            }
                            _ => return,
                        }
                    }
                    StatusLineEditorMode::Colors { module, list } => {
                        let choices = status_line_color_choices();
                        let ListHit::Item(index) = surface_hit else {
                            return;
                        };
                        list.select(index, VISIBLE_MENU_ITEMS);
                        match choices[index] {
                            StatusLineColorChoice::Default => {
                                config.colors.insert(*module, module.default_color());
                                *mode = StatusLineEditorMode::Modules;
                            }
                            StatusLineColorChoice::Named(color) => {
                                config.colors.insert(*module, color);
                                *mode = StatusLineEditorMode::Modules;
                            }
                            StatusLineColorChoice::Custom => {
                                let input = match config.color(*module) {
                                    StatusLineColor::Rgb(red, green, blue) => {
                                        format!("#{red:02X}{green:02X}{blue:02X}")
                                    }
                                    _ => "#".into(),
                                };
                                let cursor = cursor_at_end(&input);
                                *mode = StatusLineEditorMode::Hex {
                                    module: *module,
                                    input,
                                    cursor,
                                };
                            }
                        }
                    }
                    StatusLineEditorMode::Hex { .. } => {}
                },
                _ => {}
            }
            return;
        }
        let (queue_rows, popup_rows, composer_rows, status_height) =
            self.control_heights_within(width, height);
        let sticky_rows = height.saturating_sub(status_height).min(2);
        let indicator_rows = sticky_rows.min(1);
        let queue_start = usize::from(indicator_rows);
        let popup_start = queue_start + usize::from(queue_rows);
        let draft_start = popup_start
            + usize::from(popup_rows)
            + usize::from(sticky_rows.saturating_sub(indicator_rows));
        if (popup_start..popup_start + usize::from(popup_rows)).contains(&relative) {
            let hit = self.completion_hit(relative - popup_start);
            let attachment = self.attachment_completion.is_some();
            let observer = self.is_read_only_view();
            let list = if attachment {
                &mut self.attachment_completion.as_mut().unwrap().list
            } else if observer {
                &mut self.observer_command_list
            } else {
                &mut self.slash_list
            };
            match hit {
                ListHit::Item(index) => {
                    list.select(index, VISIBLE_MENU_ITEMS);
                }
                ListHit::Up => {
                    list.apply(
                        ListAction::PagePrevious,
                        ListMode::Selectable,
                        VISIBLE_MENU_ITEMS,
                    );
                }
                ListHit::Down => {
                    list.apply(
                        ListAction::PageNext,
                        ListMode::Selectable,
                        VISIBLE_MENU_ITEMS,
                    );
                }
                ListHit::None => {}
            }
            return;
        }
        let capacity = usize::from(composer_rows.saturating_sub(1));
        let composer_layout = self.composer_viewport_layout(width, capacity);
        let composer_hit = relative
            .checked_sub(draft_start.saturating_sub(1))
            .and_then(|row| composer_layout.hit(row));
        if let Some(super::scroll::ScrollViewHit::Indicator(action)) = composer_hit {
            if !self.draft.is_empty() {
                self.last_composer_input_at = Some(Instant::now());
            }
            self.apply_composer_scroll_action(action, width, height);
            return;
        }
        if let Some(super::scroll::ScrollViewHit::Content(source_row)) = composer_hit {
            if !self.draft.is_empty() {
                self.last_composer_input_at = Some(Instant::now());
            }
            let Some(rendered_offset) = composer_layout
                .rendered_offset_at(source_row, usize::from(column.saturating_sub(2)))
            else {
                return;
            };
            if let Some(source) = composer_layout.collapsed_chip_at(rendered_offset) {
                self.cursor = source;
                if self.activate_chip_at_cursor() {
                    return;
                }
            }
            self.cursor = composer_layout
                .source_offset_at(rendered_offset)
                .min(self.draft.len());
            if self.activate_chip_at_cursor() {
                return;
            }
            self.preferred_column = None;
        } else if let Some(row) = relative.checked_sub(queue_start)
            && let Some(index) = self.queue_index_at_display_row(row)
        {
            self.selected_queue = Some(index);
        }
    }

    fn completion_hit(&self, row: usize) -> ListHit {
        self.completion_layout(self.render_width).hit(row)
    }

    /// Finds the logical list item at a terminal row from the same hit map the
    /// renderer exposes. Multi-line items map every visual row to that item.
    fn surface_option_at(&self, width: u16, height: u16, row: u16) -> Option<usize> {
        let surface = self.surfaces.last()?;
        if matches!(surface, Surface::StatusLine { .. } | Surface::Help { .. }) {
            return None;
        }
        let controls_height = self.controls_height(width, height);
        let panel_top = height
            .saturating_sub(controls_height)
            .saturating_add(height.saturating_sub(1).min(2));
        let relative = usize::from(row).checked_sub(usize::from(panel_top))?;
        let (_, _, composer_height, _) = self.control_heights_within(width, height);
        let viewport_rows = if matches!(surface, Surface::Permission { .. }) {
            usize::from(composer_height.saturating_sub(1))
        } else {
            usize::from(composer_height)
        };
        match surface_layout_with_viewport(surface, &self.workspace, width, viewport_rows)
            .hits
            .get(relative)
            .copied()
            .unwrap_or(ListHit::None)
        {
            ListHit::Item(index) => Some(index),
            _ => None,
        }
    }

    fn surface_selected_option(&self) -> Option<usize> {
        match self.surfaces.last()? {
            Surface::Question { option_index, .. }
            | Surface::Permission {
                selected: option_index,
                ..
            }
            | Surface::PlanCompletion {
                selected: option_index,
                ..
            }
            | Surface::McpServer {
                selected: option_index,
                ..
            }
            | Surface::McpPackageSetup {
                selected: option_index,
                ..
            }
            | Surface::McpRemoveConfirm {
                selected: option_index,
                ..
            }
            | Surface::McpAddScope {
                selected: option_index,
            }
            | Surface::McpTransport {
                selected: option_index,
                ..
            } => Some(*option_index),
            Surface::KillSupervisedWork {
                selected: option_index,
                ..
            } => Some(*option_index),
            Surface::Providers { list, .. }
            | Surface::Models { list, .. }
            | Surface::Effort { list, .. }
            | Surface::Profiles { list, .. }
            | Surface::Worktrees { list, .. }
            | Surface::AgentEdit { list, .. }
            | Surface::WebSearchPicker { list, .. }
            | Surface::McpServers { list, .. }
            | Surface::Permissions { list, .. }
            | Surface::Usage { list, .. }
            | Surface::UsageBreakdown { list, .. }
            | Surface::McpTools { list, .. }
            | Surface::McpForm { list, .. }
            | Surface::McpMutationPreview { list, .. }
            | Surface::Paths { list, .. }
            | Surface::Conversations { list, .. }
            | Surface::HistoryTree { list, .. }
            | Surface::SupervisedWork { list, .. }
            | Surface::Settings { list, .. }
            | Surface::SettingChoices { list, .. } => list.selected,
            Surface::AgentWizard {
                step: AgentWizardStep::Parent,
                parent_list,
                ..
            } => parent_list.selected,
            Surface::AgentWizard {
                step: AgentWizardStep::Availability,
                availability,
                ..
            } => Some(*availability),
            _ => None,
        }
    }

    fn set_surface_selected_option(&mut self, option: usize) -> bool {
        let (_, _, composer_height, _) =
            self.control_heights_within(self.render_width, self.render_height);
        let viewport_rows = usize::from(composer_height);
        match self.surfaces.last_mut() {
            Some(Surface::Question {
                option_index,
                request,
                question_index,
                ..
            }) => {
                let InteractionRequestKind::Question { questions } = &request.kind else {
                    return false;
                };
                if option <= questions[*question_index].options.len() {
                    *option_index = option;
                    true
                } else {
                    false
                }
            }
            Some(Surface::Permission {
                selected,
                scope,
                request,
                ..
            }) => {
                if option < permission_approval_choices(request, *scope).len() {
                    *selected = option;
                    true
                } else {
                    false
                }
            }
            Some(Surface::PlanCompletion { selected, .. }) => {
                if option < 4 {
                    *selected = option;
                    true
                } else {
                    false
                }
            }
            Some(Surface::McpServers { list, rows }) => {
                if option < rows.len() + 2 {
                    list.select(option, VISIBLE_MENU_ITEMS);
                    true
                } else {
                    false
                }
            }
            Some(Surface::Permissions { list, rows }) => {
                if option <= rows.len() {
                    list.select(option, VISIBLE_MENU_ITEMS);
                    true
                } else {
                    false
                }
            }
            Some(Surface::Usage { list, .. }) => {
                if option < 4 {
                    list.select(option, VISIBLE_MENU_ITEMS);
                    true
                } else {
                    false
                }
            }
            Some(Surface::UsageBreakdown { list, rows, .. }) => {
                if option < rows.len() {
                    list.select(option, VISIBLE_MENU_ITEMS);
                    true
                } else {
                    false
                }
            }
            Some(Surface::UsageResetConfirm { selected }) => {
                if option < 2 {
                    *selected = option;
                    true
                } else {
                    false
                }
            }
            Some(Surface::WebSearchPicker {
                list, rows, query, ..
            }) => {
                let visible = rows
                    .iter()
                    .filter(|row| row.provider.label().contains(&query.to_lowercase()))
                    .collect::<Vec<_>>();
                if option <= visible.len() {
                    if option > 0 && !visible[option - 1].ready {
                        return true;
                    }
                    list.select(option, VISIBLE_MENU_ITEMS);
                    true
                } else {
                    false
                }
            }
            Some(Surface::McpServer {
                server, selected, ..
            }) => {
                if option < mcp_server_menu_item_count(server) {
                    *selected = option;
                    true
                } else {
                    false
                }
            }
            Some(Surface::McpPackageSetup { draft, selected }) => {
                let count = 1 + draft.package.parameters.len() + draft.package.secrets.len();
                if option < count {
                    *selected = option;
                    true
                } else {
                    false
                }
            }
            Some(Surface::McpTools { list, tools, .. }) => {
                if option < tools.len() {
                    list.select(option, VISIBLE_MENU_ITEMS);
                    true
                } else {
                    false
                }
            }
            Some(Surface::McpRemoveConfirm { selected, .. }) => {
                if option < 2 {
                    *selected = option;
                    true
                } else {
                    false
                }
            }
            Some(Surface::McpMutationPreview { list, .. }) => {
                if option < 2 {
                    list.select(option, VISIBLE_MENU_ITEMS);
                    true
                } else {
                    false
                }
            }
            Some(Surface::KillSupervisedWork { selected, .. }) => {
                if option < 2 {
                    *selected = option;
                    true
                } else {
                    false
                }
            }
            Some(Surface::McpAddScope { selected }) => {
                if option < 3 {
                    *selected = option;
                    true
                } else {
                    false
                }
            }
            Some(Surface::McpTransport { selected, .. }) => {
                if option < 4 {
                    *selected = option;
                    true
                } else {
                    false
                }
            }
            Some(Surface::McpForm { list, draft }) => {
                let count = mcp_form_item_count(draft);
                if option < count {
                    list.select(option, VISIBLE_MENU_ITEMS);
                    true
                } else {
                    false
                }
            }
            Some(Surface::Providers {
                list, rows, query, ..
            }) => {
                let count = filter_provider_picker_rows(rows, query).len();
                if option <= count {
                    list.select(option, VISIBLE_MENU_ITEMS);
                    true
                } else {
                    false
                }
            }
            Some(Surface::Models {
                list, rows, query, ..
            }) => {
                let count = filter_model_picker_rows(rows, query).len();
                if option < count {
                    list.select(option, VISIBLE_MENU_ITEMS);
                    true
                } else {
                    false
                }
            }
            Some(Surface::Effort { list, rows, .. }) => {
                if option < rows.len() {
                    list.select(option, VISIBLE_MENU_ITEMS);
                    true
                } else {
                    false
                }
            }
            Some(Surface::Profiles { list, rows, .. }) => {
                if rows.get(option).is_some_and(|row| row.2) {
                    list.select(option, VISIBLE_MENU_ITEMS);
                    true
                } else {
                    false
                }
            }
            Some(Surface::Worktrees { list, rows }) => {
                if option <= rows.len() {
                    list.select(option, VISIBLE_MENU_ITEMS);
                    true
                } else {
                    false
                }
            }
            Some(Surface::AgentWizard {
                step: AgentWizardStep::Parent,
                parent_list,
                parents,
                ..
            }) => {
                if option < parents.len() {
                    parent_list.select(option, VISIBLE_MENU_ITEMS);
                    true
                } else {
                    false
                }
            }
            Some(Surface::AgentWizard {
                step: AgentWizardStep::Availability,
                availability,
                ..
            }) => {
                if option < 3 {
                    *availability = option;
                    true
                } else {
                    false
                }
            }
            Some(Surface::AgentEdit { list, .. }) => {
                if option < 5 {
                    list.select(option, VISIBLE_MENU_ITEMS);
                    true
                } else {
                    false
                }
            }
            Some(Surface::SupervisedWork {
                list,
                rows,
                show_past,
            }) => {
                let count = rows
                    .iter()
                    .filter(|item| item.active() != *show_past)
                    .count();
                if option < count {
                    list.select(option, VISIBLE_MENU_ITEMS);
                    true
                } else {
                    false
                }
            }
            Some(Surface::Paths { list, rows, .. }) => {
                if option < rows.len() {
                    list.select(option, VISIBLE_MENU_ITEMS);
                    true
                } else {
                    false
                }
            }
            Some(Surface::HistoryTree {
                list, rows, query, ..
            }) => {
                let visible = history_picker_indexes(rows, query);
                if visible
                    .get(option)
                    .is_some_and(|source| rows[*source].selectable)
                {
                    list.select(option, VISIBLE_MENU_ITEMS);
                    true
                } else {
                    false
                }
            }
            Some(Surface::Conversations {
                list,
                rows,
                query,
                include_archived,
                ..
            }) => {
                let visible = conversation_picker_indexes(rows, query);
                if option < visible.len() {
                    let capacity = conversation_picker_item_capacity(
                        viewport_rows,
                        *include_archived,
                        !query.is_empty() && visible.is_empty(),
                    );
                    list.select(option, capacity);
                    true
                } else {
                    false
                }
            }
            Some(Surface::Settings {
                rows,
                section,
                list,
                query,
                ..
            }) => {
                let count = filter_settings_rows(rows, *section, query).len();
                if option < count {
                    list.select(option, settings_item_capacity(query));
                    true
                } else {
                    false
                }
            }
            Some(Surface::SettingChoices { choices, list, .. }) => {
                if option < choices.len() {
                    list.select(option, VISIBLE_MENU_ITEMS);
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    fn open_bash_output_at(&mut self, row: u16, height: u16) -> bool {
        if !self.surfaces.is_empty() {
            return false;
        }
        let Some(hit) = self
            .bash_output_hits
            .iter()
            .find(|hit| hit.row == row)
            .cloned()
        else {
            return false;
        };
        let detail = Self::terminal_detail(
            hit.terminal_id,
            hit.command,
            hit.output,
            hit.ansi_output,
            hit.completion,
        );
        self.surfaces
            .push(detail.into_surface(self.render_width, height));
        true
    }

    fn toggle_exploration_at(&mut self, row: u16, width: u16, height: u16) -> bool {
        let Some(target) = self
            .exploration_toggle_hits
            .iter()
            .find(|hit| hit.row == row)
            .map(|hit| hit.target.clone())
        else {
            return false;
        };
        match target {
            crate::markdown::ExplorationToggleTarget::Transcript {
                block_id,
                group_index,
            } => {
                let target = crate::markdown::ExplorationToggleTarget::Transcript {
                    block_id: block_id.clone(),
                    group_index,
                };
                if !self.expanded_explorations.remove(&target) {
                    self.expanded_explorations.insert(target);
                }
                self.history_block_layouts
                    .retain(|(id, _), _| *id != block_id);
                self.history_layout.rendered = None;
            }
            crate::markdown::ExplorationToggleTarget::AgentLog {
                run_id,
                group_index,
            } => {
                let (_, _, composer_height, _) = self.control_heights_within(width, height);
                let followed_tail = {
                    let Some(Surface::Expanded {
                        view:
                            ExpandedView::AgentLog {
                                run,
                                expanded_explorations,
                                max_scroll,
                                ..
                            },
                        scroll,
                        ..
                    }) = self.surfaces.last_mut()
                    else {
                        return false;
                    };
                    if run.id != run_id {
                        return false;
                    }
                    let previous_max = max_scroll.get().unwrap_or(*scroll);
                    let followed_tail = *scroll >= previous_max;
                    if !expanded_explorations.remove(&group_index) {
                        expanded_explorations.insert(group_index);
                    }
                    max_scroll.set(None);
                    followed_tail
                };
                super::surfaces::invalidate_expanded_text(&self.expanded_text_render_cache);
                let next_max = super::surfaces::cached_expanded_scroll_metrics(
                    &self.expanded_text_render_cache,
                    match self.surfaces.last() {
                        Some(Surface::Expanded {
                            view: view @ ExpandedView::AgentLog { .. },
                            ..
                        }) => view,
                        _ => return false,
                    },
                    match self.surfaces.last() {
                        Some(Surface::Expanded { viewport_rows, .. }) => *viewport_rows,
                        _ => return false,
                    },
                    &self.workspace,
                    width,
                    usize::from(composer_height),
                )
                .1;
                if let Some(Surface::Expanded { scroll, .. }) = self.surfaces.last_mut() {
                    *scroll = if followed_tail {
                        next_max
                    } else {
                        (*scroll).min(next_max)
                    };
                }
            }
        }
        true
    }

    fn toggle_activity_collapse_at(&mut self, row: u16) -> bool {
        let Some(target) = self
            .activity_collapse_hits
            .iter()
            .find(|hit| hit.row == row)
            .map(|hit| hit.target.clone())
        else {
            return false;
        };
        match &target {
            crate::markdown::ActivityCollapseTarget::Transcript(_) => {
                if !self.expanded_activity_runs.remove(&target) {
                    self.expanded_activity_runs.insert(target);
                }
            }
            crate::markdown::ActivityCollapseTarget::AgentLog {
                run_id,
                group_index,
            } => {
                let Some(Surface::Expanded {
                    view:
                        ExpandedView::AgentLog {
                            run,
                            expanded_activity_runs,
                            max_scroll,
                            ..
                        },
                    ..
                }) = self.surfaces.last_mut()
                else {
                    return false;
                };
                if run.id != *run_id {
                    return false;
                }
                if !expanded_activity_runs.remove(group_index) {
                    expanded_activity_runs.insert(*group_index);
                }
                max_scroll.set(None);
            }
        }
        self.history_layout.rendered = None;
        super::surfaces::invalidate_expanded_text(&self.expanded_text_render_cache);
        true
    }

    pub(crate) fn close_activity_collapse_at(&mut self, row: u16) -> bool {
        let Some(target) = self
            .activity_collapse_hits
            .iter()
            .find(|hit| hit.row == row)
            .map(|hit| hit.target.clone())
        else {
            return false;
        };
        let closed = match &target {
            crate::markdown::ActivityCollapseTarget::Transcript(_) => {
                self.expanded_activity_runs.remove(&target)
            }
            crate::markdown::ActivityCollapseTarget::AgentLog {
                run_id,
                group_index,
            } => {
                let Some(Surface::Expanded {
                    view:
                        ExpandedView::AgentLog {
                            run,
                            expanded_activity_runs,
                            max_scroll,
                            ..
                        },
                    ..
                }) = self.surfaces.last_mut()
                else {
                    return false;
                };
                if run.id != *run_id {
                    return false;
                }
                let closed = expanded_activity_runs.remove(group_index);
                if closed {
                    max_scroll.set(None);
                }
                closed
            }
        };
        if closed {
            self.history_layout.rendered = None;
            super::surfaces::invalidate_expanded_text(&self.expanded_text_render_cache);
        }
        closed
    }

    async fn open_bash_output_at_with_session(
        &mut self,
        session: &SessionHandle,
        row: u16,
        height: u16,
    ) -> Result<bool, cagent_agent::runtime::RuntimeError> {
        if !self.surfaces.is_empty() {
            return Ok(false);
        }
        let Some(hit) = self
            .bash_output_hits
            .iter()
            .find(|hit| hit.row == row)
            .cloned()
        else {
            return Ok(false);
        };
        let detail = self
            .terminal_detail_with_session(
                session,
                hit.terminal_id,
                hit.command,
                hit.output,
                hit.ansi_output,
                hit.completion,
            )
            .await?;
        self.surfaces
            .push(detail.into_surface(self.render_width, height));
        Ok(true)
    }

    fn open_agent_log_at(&mut self, row: u16, height: u16) -> bool {
        if !self.surfaces.is_empty() {
            return false;
        }
        let Some(run_id) = self
            .agent_log_hits
            .iter()
            .find(|hit| hit.row == row)
            .map(|hit| hit.run_id)
        else {
            return false;
        };
        let Some(run) = self
            .supervised_work
            .rows
            .iter()
            .find_map(|work| match work {
                cagent_agent::presentation::SupervisedWork::Agent { run } if run.id == run_id => {
                    Some(run.clone())
                }
                _ => None,
            })
        else {
            return false;
        };
        let live = self.supervised_work.agent_run_live.get(&run_id);
        let streaming = live.map(|state| state.text.clone()).unwrap_or_default();
        let terminals = self.supervised_work.terminals_for_agent(&run);
        let mut run = run;
        if let Some(usage) = live.and_then(|state| state.usage.clone()) {
            run.usage = Some(usage);
        }
        self.expanded_text_render_cache.get_mut().take();
        self.surfaces.push(Surface::Expanded {
            view: ExpandedView::AgentLog {
                run,
                streaming,
                terminals,
                expanded_explorations: Default::default(),
                expanded_activity_runs: Default::default(),
                collapse_tool_activity: self.collapse_tool_activity,
                max_scroll: Cell::new(None),
            },
            scroll: 0,
            viewport_rows: usize::from(height.saturating_sub(6).max(1)),
        });
        true
    }

    fn open_web_fetch_output_at(&mut self, row: u16, height: u16) -> bool {
        if !self.surfaces.is_empty() {
            return false;
        }
        let Some(hit) = self
            .web_fetch_output_hits
            .iter()
            .find(|hit| hit.row == row)
            .cloned()
        else {
            return false;
        };
        self.expanded_text_render_cache.get_mut().take();
        self.surfaces.push(Surface::Expanded {
            view: ExpandedView::WebFetch {
                url: hit.url,
                redirected_url: hit.redirected_url,
                format: hit.format,
                output: hit.output,
            },
            scroll: 0,
            viewport_rows: usize::from(height.saturating_sub(4).max(1)),
        });
        true
    }

    fn open_mcp_call_at(&mut self, row: u16, height: u16) -> bool {
        if !self.surfaces.is_empty()
            && !matches!(
                self.surfaces.last(),
                Some(Surface::Expanded {
                    view: ExpandedView::AgentLog { .. },
                    ..
                })
            )
        {
            return false;
        }
        let Some((_, call)) = self
            .mcp_call_hits
            .iter()
            .find(|(hit_row, _)| *hit_row == row)
        else {
            return false;
        };
        let call = std::sync::Arc::clone(call);
        self.pending_mcp_detail_load = call.node_id;
        self.expanded_text_render_cache.get_mut().take();
        self.surfaces.push(Surface::Expanded {
            view: ExpandedView::Mcp { call },
            scroll: 0,
            viewport_rows: usize::from(height.saturating_sub(4).max(1)),
        });
        true
    }

    fn open_compaction_at(&mut self, row: u16, height: u16) -> bool {
        if !self.surfaces.is_empty() {
            return false;
        }
        let Some(hit) = self
            .compaction_hits
            .iter()
            .find(|hit| hit.row == row)
            .cloned()
        else {
            return false;
        };
        self.expanded_text_render_cache.get_mut().take();
        self.surfaces.push(Surface::Expanded {
            view: ExpandedView::Compaction {
                summary: hit.summary,
            },
            scroll: 0,
            viewport_rows: usize::from(height.saturating_sub(4).max(1)),
        });
        true
    }

    fn open_web_search_results_at(&mut self, row: u16, height: u16) -> bool {
        if !self.surfaces.is_empty() {
            return false;
        }
        let Some(hit) = self
            .web_search_result_hits
            .iter()
            .find(|hit| hit.row == row)
            .cloned()
        else {
            return false;
        };
        self.expanded_text_render_cache.get_mut().take();
        self.surfaces.push(Surface::Expanded {
            view: ExpandedView::WebSearch {
                provider: hit.provider,
                query: hit.query,
                results: hit.results,
            },
            scroll: 0,
            viewport_rows: usize::from(height.saturating_sub(4).max(1)),
        });
        true
    }

    /// Opens a sub-agent's result view from exactly the same projected tool
    /// groups used to render its log.
    pub(crate) fn open_agent_web_search_results_at(&mut self, row: u16, height: u16) -> bool {
        let Some(relative) = self.expanded_surface_content_row(row, height) else {
            return false;
        };
        let Some(Surface::Expanded {
            view:
                ExpandedView::AgentLog {
                    run,
                    streaming,
                    terminals,
                    expanded_explorations,
                    ..
                },
            scroll,
            viewport_rows,
        }) = self.surfaces.last()
        else {
            return false;
        };
        let surface = Surface::Expanded {
            view: ExpandedView::AgentLog {
                run: run.clone(),
                streaming: streaming.clone(),
                terminals: terminals.clone(),
                expanded_explorations: expanded_explorations.clone(),
                expanded_activity_runs: Default::default(),
                collapse_tool_activity: self.collapse_tool_activity,
                max_scroll: Cell::new(None),
            },
            scroll: *scroll,
            viewport_rows: *viewport_rows,
        };
        let lines = super::surfaces::surface_lines(&surface, &self.workspace, self.render_width);
        let mut start = 0;
        for (provider, query, results) in agent_web_search_result_candidates(run, &self.workspace) {
            let marker = format!("Web search {provider}");
            let Some(heading) = lines
                .iter()
                .enumerate()
                .skip(start)
                .find_map(|(index, line)| line.to_string().contains(&marker).then_some(index))
            else {
                continue;
            };
            start = heading.saturating_add(1);
            let query_rows = crate::render::wrap_ranges(
                &crate::render::terminal_safe(&query),
                self.render_width.saturating_sub(8),
            )
            .len();
            if (heading.saturating_add(1)..heading.saturating_add(1 + query_rows))
                .any(|index| index == relative)
            {
                self.expanded_text_render_cache.get_mut().take();
                self.surfaces.push(Surface::Expanded {
                    view: ExpandedView::WebSearch {
                        provider,
                        query,
                        results,
                    },
                    scroll: 0,
                    viewport_rows: usize::from(height.saturating_sub(4).max(1)),
                });
                return true;
            }
        }
        false
    }

    pub(crate) fn delegated_bash_hit(
        &self,
        row: u16,
        height: u16,
    ) -> Option<(
        Option<cagent_agent::tools::TerminalId>,
        String,
        String,
        String,
        Option<String>,
    )> {
        let Some(relative) = self.expanded_surface_content_row(row, height) else {
            return None;
        };
        let Some(Surface::Expanded {
            view:
                ExpandedView::AgentLog {
                    run,
                    streaming,
                    terminals,
                    expanded_explorations,
                    ..
                },
            scroll,
            viewport_rows,
        }) = self.surfaces.last()
        else {
            return None;
        };
        let surface = Surface::Expanded {
            view: ExpandedView::AgentLog {
                run: run.clone(),
                streaming: streaming.clone(),
                terminals: terminals.clone(),
                expanded_explorations: expanded_explorations.clone(),
                expanded_activity_runs: Default::default(),
                collapse_tool_activity: self.collapse_tool_activity,
                max_scroll: Cell::new(None),
            },
            scroll: *scroll,
            viewport_rows: *viewport_rows,
        };
        let lines = super::surfaces::surface_lines(&surface, &self.workspace, self.render_width);
        // `surface_lines` already contains the currently visible, scrolled
        // rows. Adding the scroll offset again shifts every hit below its
        // rendered Bash card once the log has been scrolled.
        let clicked = relative;
        let log = cagent_agent::presentation::project_agent_run_log_with_terminals(
            run,
            &self.workspace,
            terminals,
        );
        let mut search_start = 0;
        for entry in log.entries {
            let cagent_agent::presentation::AgentRunLogEntry::ToolGroups(groups) = entry else {
                continue;
            };
            for group in groups {
                let rendered = crate::render::render_tool_groups_with_workspace(
                    std::slice::from_ref(&group),
                    &self.workspace,
                    self.render_width,
                );
                let cagent_agent::presentation::ToolActivityGroup::Bash {
                    terminal_id,
                    command,
                    output,
                    ansi_output,
                    status,
                    exit_code,
                    ..
                } = group
                else {
                    continue;
                };
                let Some(heading) =
                    lines
                        .iter()
                        .enumerate()
                        .skip(search_start)
                        .find_map(|(index, line)| {
                            rendered
                                .first()
                                .is_some_and(|first| line.to_string() == first.to_string())
                                .then_some(index)
                        })
                else {
                    continue;
                };
                search_start = heading.saturating_add(rendered.len());
                if (heading..heading.saturating_add(rendered.len())).contains(&clicked) {
                    let output = output.unwrap_or_default();
                    let ansi_output = ansi_output.unwrap_or_else(|| output.clone());
                    let completion = (status
                        != cagent_agent::presentation::ToolActivityStatus::Pending)
                        .then(|| {
                            exit_code.map_or_else(
                                || "[process exited]".into(),
                                |code| format!("[exited with status {code}]"),
                            )
                        });
                    return Some((terminal_id, command, output, ansi_output, completion));
                }
            }
        }
        None
    }

    /// Maps a screen row to the visible expanded-surface body, excluding the
    /// sticky spacer/indicator rows and the status row below it.
    fn expanded_surface_content_row(&self, row: u16, height: u16) -> Option<usize> {
        let controls_height = self.controls_height(self.render_width, height);
        let (_, _, composer_height, status_height) =
            self.control_heights_within(self.render_width, height);
        let surface_top = controls_height
            .saturating_sub(composer_height)
            .saturating_sub(status_height);
        let top = height
            .saturating_sub(controls_height)
            .saturating_add(surface_top);
        let end = top
            .saturating_add(composer_height)
            .min(height.saturating_sub(status_height));
        (top..end)
            .contains(&row)
            .then(|| usize::from(row.saturating_sub(top)))
    }

    /// Opens the Bash group under the pointer, preserving the delegated log's
    /// actual group boundaries instead of assuming the latest Bash call.
    fn open_agent_bash_output_at(&mut self, row: u16, height: u16) -> bool {
        let Some((terminal_id, command, output, ansi_output, completion)) =
            self.delegated_bash_hit(row, height)
        else {
            return false;
        };
        let detail = Self::terminal_detail(terminal_id, command, output, ansi_output, completion);
        self.surfaces
            .push(detail.into_surface(self.render_width, height));
        true
    }

    async fn open_agent_bash_output_at_with_session(
        &mut self,
        session: &SessionHandle,
        row: u16,
        height: u16,
    ) -> Result<bool, cagent_agent::runtime::RuntimeError> {
        let Some((terminal_id, command, fallback_output, fallback_ansi, completion)) =
            self.delegated_bash_hit(row, height)
        else {
            return Ok(false);
        };
        let detail = self
            .terminal_detail_with_session(
                session,
                terminal_id,
                command,
                fallback_output,
                fallback_ansi,
                completion,
            )
            .await?;
        self.surfaces
            .push(detail.into_surface(self.render_width, height));
        Ok(true)
    }

    async fn handle_observer_key(
        &mut self,
        session: &SessionHandle,
        key: KeyEvent,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        if self.history_preview_showing() && !self.observer_command_active() {
            if self.matches_action(
                cagent_agent::presentation::KeyBindingAction::CloseSurface,
                key,
            ) || self.matches_action(cagent_agent::presentation::KeyBindingAction::Cancel, key)
            {
                self.close_history_preview();
            } else {
                match key.code {
                    KeyCode::PageUp => self.scroll_history_up(10),
                    KeyCode::PageDown => self.scroll_history_down(10),
                    KeyCode::Home => self.scroll_history_to_top(),
                    KeyCode::End => self.scroll_history_to_end(),
                    KeyCode::Char('/') if key.modifiers.is_empty() => {
                        self.activate_observer_command();
                    }
                    _ => {}
                }
            }
            return Ok(false);
        }
        if !self.history_preview_showing()
            && self
                .surfaces
                .last()
                .is_some_and(Self::observer_surface_allowed)
        {
            return self.handle_surface_key(session, key).await;
        }
        if self.history_preview.is_none() && !self.surfaces.is_empty() {
            return Ok(false);
        }
        let cancel = key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c' | 'C'));
        if self.observer_command_active() {
            if cancel
                || self.matches_action(cagent_agent::presentation::KeyBindingAction::Cancel, key)
            {
                self.clear_observer_command();
                return Ok(false);
            }
            let navigation_code = self.menu_navigation_code(key);
            if !self.slash_suggestions().is_empty() {
                if let Some(action) = list_action(navigation_code) {
                    self.observer_command_list.apply(
                        action,
                        ListMode::Selectable,
                        VISIBLE_MENU_ITEMS,
                    );
                } else {
                    match navigation_code {
                        KeyCode::Esc => self.observer_command_dismissed = true,
                        KeyCode::Tab => {
                            self.accept_slash_command();
                        }
                        KeyCode::Enter => {
                            self.activate_slash_command(session).await?;
                        }
                        _ => {}
                    }
                }
                if matches!(
                    navigation_code,
                    KeyCode::Esc | KeyCode::Up | KeyCode::Down | KeyCode::Tab | KeyCode::Enter
                ) {
                    return Ok(false);
                }
            } else if navigation_code == KeyCode::Enter {
                self.dispatch_observer_command(session).await?;
                return Ok(false);
            }
            self.handle_composer_editing_key(key);
            if self
                .observer_command_text()
                .is_some_and(|text| text.is_empty() || !text.starts_with('/'))
            {
                self.clear_observer_command();
            } else {
                self.normalize_slash();
            }
            return Ok(false);
        }
        if self.matches_action(cagent_agent::presentation::KeyBindingAction::Exit, key) {
            return Ok(true);
        }
        if self.matches_action(cagent_agent::presentation::KeyBindingAction::Cancel, key) {
            if self
                .exit_armed
                .is_some_and(|armed| armed.elapsed() <= Duration::from_secs(2))
            {
                return Ok(true);
            }
            self.exit_armed = Some(Instant::now());
            self.set_notice(EXIT_NOTICE);
            return Ok(false);
        }
        match key.code {
            KeyCode::PageUp => self.scroll_history_up(10),
            KeyCode::PageDown => self.scroll_history_down(10),
            KeyCode::Home => self.scroll_history_to_top(),
            KeyCode::End => self.scroll_history_to_end(),
            KeyCode::Char('/') if key.modifiers.is_empty() => {
                self.activate_observer_command();
            }
            _ => {}
        }
        Ok(false)
    }

    /// Handles only the plain-text portion of composer input. Contextual
    /// actions (completion, paste, history, submission, and observer command
    /// dispatch) are intentionally handled by the callers around this method.
    fn handle_composer_editing_key(&mut self, key: KeyEvent) -> bool {
        let observer = self.observer_command_active();
        let line_start =
            self.matches_action(cagent_agent::presentation::KeyBindingAction::LineStart, key);
        let line_end =
            self.matches_action(cagent_agent::presentation::KeyBindingAction::LineEnd, key);
        let delete_word = self.matches_action(
            cagent_agent::presentation::KeyBindingAction::DeleteWord,
            key,
        ) || is_word_backspace(key);
        let word_left = is_word_left(key);
        let word_right = is_word_right(key);
        let undo = self.matches_action(cagent_agent::presentation::KeyBindingAction::Undo, key);
        let navigate_left = self.matches_action(
            cagent_agent::presentation::KeyBindingAction::NavigateLeft,
            key,
        );
        let navigate_right = self.matches_action(
            cagent_agent::presentation::KeyBindingAction::NavigateRight,
            key,
        );

        // Repeating Ctrl+A/Ctrl+E at a line boundary continues vertically,
        // matching the behavior of the corresponding arrow key. At any
        // other cursor position these remain ordinary line-boundary actions.
        if line_start && !observer {
            let at_line_start = self.cursor == 0
                || self.draft[..self.cursor]
                    .chars()
                    .next_back()
                    .is_some_and(|character| character == '\n');
            if at_line_start {
                self.move_vertical(false);
                return true;
            }
        }
        if line_end && !observer {
            let at_line_end = self.draft[self.cursor..]
                .chars()
                .next()
                .is_none_or(|character| character == '\n');
            if at_line_end {
                self.move_vertical(true);
                return true;
            }
        }

        let editor = self.observer_command_editor.as_mut();

        if line_start {
            if let Some(editor) = editor {
                editor.move_to_line_start();
            } else if !observer {
                self.move_to_line_start();
            }
            return true;
        }
        if line_end {
            if let Some(editor) = editor {
                editor.move_to_line_end();
            } else if !observer {
                self.move_to_line_end();
            }
            return true;
        }
        if delete_word {
            if let Some(editor) = editor {
                editor.delete_previous_word();
            } else if !observer {
                self.delete_previous_word();
            }
            return true;
        }
        if word_left {
            if let Some(editor) = editor {
                editor.move_word_left();
            } else if !observer {
                self.move_word_left();
            }
            return true;
        }
        if word_right {
            if let Some(editor) = editor {
                editor.move_word_right();
            } else if !observer {
                self.move_word_right();
            }
            return true;
        }
        if undo {
            if let Some(editor) = editor {
                editor.undo();
            } else if !observer {
                self.undo_last_edit();
            }
            return true;
        }
        if navigate_left {
            if let Some(editor) = editor {
                editor.move_left();
            } else if !observer {
                self.move_left();
            }
            return true;
        }
        if navigate_right {
            if let Some(editor) = editor {
                editor.move_right();
            } else if !observer {
                self.move_right();
            }
            return true;
        }

        match key.code {
            KeyCode::Backspace => {
                if let Some(editor) = editor {
                    editor.backspace();
                } else if !observer {
                    self.backspace();
                }
                true
            }
            KeyCode::Delete => {
                if let Some(editor) = editor {
                    editor.delete_forward();
                } else if !observer {
                    self.delete_forward();
                }
                true
            }
            KeyCode::Home => {
                if let Some(editor) = editor {
                    editor.move_to_line_start();
                } else if !observer {
                    self.move_to_line_start();
                }
                true
            }
            KeyCode::End => {
                if let Some(editor) = editor {
                    editor.move_to_line_end();
                } else if !observer {
                    self.move_to_line_end();
                }
                true
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                if let Some(editor) = editor {
                    editor.insert(&character.to_string());
                } else if !observer {
                    self.insert_text(&character.to_string());
                }
                true
            }
            _ => false,
        }
    }

    pub(super) async fn cycle_mode(
        &mut self,
        session: &SessionHandle,
        previous: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let modes = session.mode_profiles()?;
        let Some(current) = modes.iter().position(|mode| mode.name == self.mode) else {
            return Ok(());
        };
        let Some(mode) = (1..=modes.len()).find_map(|distance| {
            let index = if previous {
                (current + modes.len() - distance % modes.len()) % modes.len()
            } else {
                (current + distance) % modes.len()
            };
            modes[index].cycleable.then(|| modes[index].name.clone())
        }) else {
            return Ok(());
        };
        self.mode = mode.clone();
        session
            .submit(SessionCommand::new(SessionAction::ChangeMode { mode }))
            .await?;
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) async fn handle_key(
        &mut self,
        session: &SessionHandle,
        key: KeyEvent,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        // Keyboard input never contributes to the two-click activation gesture.
        self.last_picker_click = None;
        if self.handle_files_key(key) {
            return Ok(false);
        }
        if self.history_preview.is_some() || self.is_observer() {
            return self.handle_observer_key(session, key).await;
        }
        if self.editing_queue.is_some()
            && (self.matches_action(cagent_agent::presentation::KeyBindingAction::Cancel, key)
                || self.matches_action(
                    cagent_agent::presentation::KeyBindingAction::CloseSurface,
                    key,
                ))
        {
            let message = self
                .editing_queue
                .as_ref()
                .expect("editing queue was checked above");
            session
                .submit(SessionCommand::new(SessionAction::DeleteQueued {
                    id: message.id,
                }))
                .await?;
            self.editing_queue = None;
            self.clear_draft();
            return Ok(false);
        }
        let closing_surface_note = self
            .matches_action(cagent_agent::presentation::KeyBindingAction::Cancel, key)
            && self
                .surfaces
                .last()
                .is_some_and(Surface::is_editing_interaction_note);
        if closing_surface_note {
            return self.handle_surface_key(session, key).await;
        }
        if self.matches_action(cagent_agent::presentation::KeyBindingAction::Cancel, key) {
            if self.composer_mode == super::ComposerMode::Bash && self.draft.is_empty() {
                self.composer_mode = super::ComposerMode::Prompt;
                return Ok(false);
            }
            if !self.draft.is_empty() {
                self.clear_draft_for_recall();
                return Ok(false);
            }
            if self
                .exit_armed
                .is_some_and(|armed| armed.elapsed() <= Duration::from_secs(2))
            {
                session.end().await?;
                return Ok(true);
            }
            let (running, _) = session.terminal_counts();
            self.exit_armed = Some(Instant::now());
            if running > 0 {
                self.set_notice(format!("{running} background terminal(s) running · press Ctrl+C again to terminate and exit"));
            } else {
                self.set_notice(EXIT_NOTICE);
            }
            return Ok(false);
        }
        if !self.surfaces.is_empty() {
            if is_paste_key(key) {
                match read_from_clipboard() {
                    Some(text) if self.insert_surface_paste(&text) => return Ok(false),
                    Some(_) => self.set_notice("paste is unavailable in this menu"),
                    None => self.set_notice("clipboard unavailable"),
                }
                return Ok(false);
            }
            let exit = self.handle_surface_key(session, key).await?;
            return Ok(exit);
        }
        if self.composer_mode == super::ComposerMode::Bash {
            if key.code == KeyCode::Backspace && self.draft.is_empty() {
                self.composer_mode = super::ComposerMode::Prompt;
                return Ok(false);
            }
            if self.matches_action(cagent_agent::presentation::KeyBindingAction::Submit, key) {
                self.submit_bash(session).await?;
                return Ok(false);
            }
        } else if self.draft.is_empty()
            && key.code == KeyCode::Char('!')
            && key.modifiers.is_empty()
        {
            self.composer_mode = super::ComposerMode::Bash;
            return Ok(false);
        }
        if self.matches_action(cagent_agent::presentation::KeyBindingAction::Exit, key) {
            if self.draft.is_empty() {
                self.hide_working_indicator = true;
                session.cancel();
                return Ok(true);
            }
            return Ok(false);
        }
        // The composer owns keyboard input whenever no focused surface does. Count
        // navigation and command keys as activity as well as text mutations so an
        // interaction cannot appear while the user is still working in the draft.
        if !self.draft.is_empty() {
            self.last_composer_input_at = Some(Instant::now());
        }
        if self.pending_interaction.is_some() && !self.interaction_presentation_is_deferred() {
            return self.handle_pending_interaction_key(session, key).await;
        }
        if self.matches_action(
            cagent_agent::presentation::KeyBindingAction::PreviousMode,
            key,
        ) {
            self.cycle_mode(session, true).await?;
            return Ok(false);
        }
        if self.matches_action(cagent_agent::presentation::KeyBindingAction::NextMode, key) {
            self.cycle_mode(session, false).await?;
            return Ok(false);
        }
        let navigation_code = self.menu_navigation_code(key);
        if let Some(completion) = &mut self.attachment_completion {
            if let Some(action) = list_action(navigation_code) {
                completion
                    .list
                    .apply(action, ListMode::Selectable, VISIBLE_MENU_ITEMS);
            } else {
                match navigation_code {
                    KeyCode::Esc => self.attachment_completion = None,
                    KeyCode::Enter | KeyCode::Tab => self.accept_attachment_completion(),
                    KeyCode::Right if self.cursor >= completion.end => {
                        self.attachment_completion = None;
                    }
                    _ => {}
                }
            }
            if matches!(
                self.menu_navigation_code(key),
                KeyCode::Esc
                    | KeyCode::Up
                    | KeyCode::Down
                    | KeyCode::PageUp
                    | KeyCode::PageDown
                    | KeyCode::Home
                    | KeyCode::End
                    | KeyCode::Enter
                    | KeyCode::Tab
            ) {
                return Ok(false);
            }
        }
        if !self.slash_suggestions().is_empty() {
            let code = self.menu_navigation_code(key);
            if let Some(action) = list_action(code) {
                self.slash_list
                    .apply(action, ListMode::Selectable, VISIBLE_MENU_ITEMS);
            } else {
                match code {
                    KeyCode::Esc => self.slash_dismissed = true,
                    KeyCode::Tab => {
                        self.accept_slash_command();
                    }
                    KeyCode::Enter if self.has_exact_runnable_slash_command() => {
                        self.submit_draft_deferred(session, QueueTarget::NextBoundary)
                            .await?;
                    }
                    KeyCode::Enter => {
                        self.activate_slash_command(session).await?;
                    }
                    _ => {}
                }
            }
            if matches!(
                self.menu_navigation_code(key),
                KeyCode::Esc | KeyCode::Up | KeyCode::Down | KeyCode::Tab | KeyCode::Enter
            ) {
                return Ok(false);
            }
        }
        if is_paste_key(key) {
            if let Some(bytes) = read_image_from_clipboard() {
                if !session.selected_model_supports_image_input().await? {
                    self.set_notice("selected model does not support image input");
                    return Ok(false);
                }
                let number = self.next_image_number();
                match session.store_clipboard_image(&bytes, number).await {
                    Ok(image) => self.insert_image(image),
                    Err(error) => self.set_notice(error.to_string()),
                }
            } else {
                match read_from_clipboard() {
                    Some(text) => self.insert_paste(&text),
                    None => self.set_notice("clipboard unavailable"),
                }
            }
            return Ok(false);
        }
        if self.matches_action(
            cagent_agent::presentation::KeyBindingAction::ExternalEditor,
            key,
        ) {
            self.pending_action = Some(AppAction::EditDraft);
            return Ok(false);
        }
        if self.matches_action(
            cagent_agent::presentation::KeyBindingAction::ModelPicker,
            key,
        ) {
            self.open_model_surface(session).await?;
            return Ok(false);
        }
        if self.matches_action(
            cagent_agent::presentation::KeyBindingAction::ToggleFast,
            key,
        ) {
            self.apply_fast_setting(session, !self.fast).await?;
            return Ok(false);
        }
        if self.matches_action(cagent_agent::presentation::KeyBindingAction::Tree, key) {
            self.open_history_surface(session, TreePurpose::Browse)?;
            return Ok(false);
        }
        if self.matches_action(cagent_agent::presentation::KeyBindingAction::Fork, key) {
            self.open_history_surface(session, TreePurpose::Fork)?;
            return Ok(false);
        }
        if self.matches_action(cagent_agent::presentation::KeyBindingAction::Rename, key) {
            self.open_rename_surface(session).await?;
            return Ok(false);
        }
        if self.matches_action(cagent_agent::presentation::KeyBindingAction::RetryTurn, key) {
            session
                .submit(SessionCommand::new(SessionAction::Retry))
                .await?;
            return Ok(false);
        }
        if self.matches_action(
            cagent_agent::presentation::KeyBindingAction::ToggleChip,
            key,
        ) {
            let _ = self.activate_chip_at_cursor();
            return Ok(false);
        }
        let message_navigation = key.modifiers == KeyModifiers::CONTROL
            && matches!(key.code, KeyCode::Up | KeyCode::Down);
        if message_navigation && self.navigate_user_message(matches!(key.code, KeyCode::Down)) {
            return Ok(false);
        }
        let queue_navigation =
            key.modifiers == KeyModifiers::ALT && matches!(key.code, KeyCode::Up | KeyCode::Down);
        if let Some(selected) = self.selected_queue {
            if queue_navigation {
                let indexes = self.queued_display_indexes();
                let Some(position) = indexes.iter().position(|index| *index == selected) else {
                    // A queue snapshot may have removed the selected item
                    // between terminal events. Selection is transient UI
                    // state, so discard it instead of crashing the TUI.
                    self.selected_queue = None;
                    return Ok(false);
                };
                match key.code {
                    KeyCode::Up => self.selected_queue = Some(indexes[position.saturating_sub(1)]),
                    KeyCode::Down => {
                        self.selected_queue = indexes.get(position + 1).copied();
                    }
                    _ => unreachable!("queue navigation only accepts vertical arrows"),
                }
                return Ok(false);
            }
            if self.matches_action(
                cagent_agent::presentation::KeyBindingAction::NavigateUp,
                key,
            ) {
                self.selected_queue = None;
                self.recall_previous_history();
                return Ok(false);
            }
            if self.matches_action(
                cagent_agent::presentation::KeyBindingAction::NavigateDown,
                key,
            ) {
                self.selected_queue = None;
                self.recall_next_history();
                return Ok(false);
            }
            match self.menu_navigation_code(key) {
                KeyCode::Esc => self.selected_queue = None,
                KeyCode::Enter => {
                    if let Some(message) = self.queued.get(selected).cloned()
                        && matches!(
                            message.kind,
                            cagent_agent::protocol::QueuedItemKind::Prompt
                                | cagent_agent::protocol::QueuedItemKind::ModePrompt
                        )
                    {
                        session
                            .submit(SessionCommand::new(SessionAction::BeginEditingQueued {
                                id: message.id,
                            }))
                            .await?;
                        let editor_text = message
                            .command_text
                            .as_ref()
                            .unwrap_or(&message.text)
                            .clone();
                        self.replace_draft_with_attachment_specs(
                            &editor_text,
                            &message.attachments,
                        );
                        if message.command_text.is_none() {
                            let metadata = message
                                .images
                                .iter()
                                .map(|image| (image.id, image))
                                .collect::<std::collections::HashMap<_, _>>();
                            self.images = message
                                .image_chips
                                .iter()
                                .filter_map(|range| {
                                    metadata.get(&range.image_id).map(|image| ImageChip {
                                        image: (*image).clone(),
                                        range: ChipRange::new(range.start, range.end),
                                    })
                                })
                                .collect();
                        }
                        self.editing_queue = Some(message);
                        self.selected_queue = None;
                    }
                }
                _ if self.matches_action(
                    cagent_agent::presentation::KeyBindingAction::DeleteQueued,
                    key,
                ) =>
                {
                    if let Some(message) = self.queued.get(selected) {
                        session
                            .submit(SessionCommand::new(SessionAction::DeleteQueued {
                                id: message.id,
                            }))
                            .await?;
                    }
                    self.selected_queue = None;
                }
                _ if self.matches_action(
                    cagent_agent::presentation::KeyBindingAction::PromoteQueued,
                    key,
                ) =>
                {
                    if let Some(message) = self.queued.get(selected) {
                        let result = session
                            .submit(SessionCommand::new(SessionAction::PromoteQueued {
                                id: message.id,
                            }))
                            .await;
                        if let Err(error) = result {
                            self.set_notice(format!("could not promote queued message · {error}"));
                        }
                    }
                    self.selected_queue = None;
                }
                _ => {}
            }
            return Ok(false);
        }
        if self.draft.is_empty()
            && self.history_index.is_none()
            && self.matches_action(
                cagent_agent::presentation::KeyBindingAction::Background,
                key,
            )
        {
            self.open_background_surface();
            return Ok(false);
        }
        if queue_navigation {
            let indexes = self.queued_display_indexes();
            if !indexes.is_empty() {
                self.selected_queue = Some(match key.code {
                    KeyCode::Up => *indexes.last().expect("queue is not empty"),
                    KeyCode::Down => indexes[0],
                    _ => unreachable!("queue navigation only accepts vertical arrows"),
                });
                return Ok(false);
            }
        }
        if self.draft.is_empty() {
            match key.code {
                KeyCode::Home => {
                    self.scroll_history_to_top();
                    return Ok(false);
                }
                KeyCode::End => {
                    self.scroll_history_to_end();
                    return Ok(false);
                }
                _ => {}
            }
        }
        if self.handle_composer_editing_key(key) {
            return Ok(false);
        }
        if self.matches_action(
            cagent_agent::presentation::KeyBindingAction::CloseSurface,
            key,
        ) {
            self.slash_dismissed = true;
            self.hide_working_indicator = true;
            session.cancel();
            return Ok(false);
        }
        if self.matches_action(
            cagent_agent::presentation::KeyBindingAction::NavigateUp,
            key,
        ) {
            if (self.history_index.is_some() || self.recalling_cleared_draft)
                && self.cursor == self.draft.len()
            {
                self.recall_previous_history();
            } else if !self.move_vertical(false) {
                let at_line_start = self.cursor == 0
                    || self.draft[..self.cursor]
                        .chars()
                        .next_back()
                        .is_some_and(|character| character == '\n');
                if at_line_start {
                    self.recall_previous_history();
                } else {
                    self.move_to_line_start();
                }
            }
            return Ok(false);
        }
        if self.matches_action(
            cagent_agent::presentation::KeyBindingAction::NavigateDown,
            key,
        ) {
            if !self.move_vertical(true) {
                let at_line_end = self.draft[self.cursor..]
                    .chars()
                    .next()
                    .is_none_or(|character| character == '\n');
                if at_line_end {
                    self.recall_next_history();
                } else {
                    self.move_to_line_end();
                }
            }
            return Ok(false);
        }
        if self.matches_action(
            cagent_agent::presentation::KeyBindingAction::InsertNewline,
            key,
        ) {
            self.insert_text("\n");
            return Ok(false);
        }
        if self.matches_action(cagent_agent::presentation::KeyBindingAction::Submit, key) {
            self.submit_draft_deferred(session, QueueTarget::NextBoundary)
                .await?;
            return Ok(false);
        }
        if self.matches_action(cagent_agent::presentation::KeyBindingAction::Complete, key) {
            if self.active {
                self.submit_draft_deferred(session, QueueTarget::EndOfTurn)
                    .await?;
            } else if !self.complete_input(session).await? && !self.draft.trim().is_empty() {
                self.submit_draft_deferred(session, QueueTarget::NextBoundary)
                    .await?;
            }
            return Ok(false);
        }
        if keyboard::is_page_navigation(key)
            && self.composer_is_scrollable(self.render_width, self.render_height)
        {
            if let Some(action) = keyboard::page_navigation_action(key) {
                self.apply_composer_scroll_action(action, self.render_width, self.render_height);
            }
        } else {
            match key.code {
                KeyCode::PageUp => self.scroll_history_up(10),
                KeyCode::PageDown => self.scroll_history_down(10),
                _ => {}
            }
        }
        Ok(false)
    }
}
