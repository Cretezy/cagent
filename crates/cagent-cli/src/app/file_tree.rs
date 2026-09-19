use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr as _;

use super::{DIM_STYLE, ERROR_STYLE, SELECTED_STYLE};

pub(crate) const MIN_FILES_WIDTH: u16 = 20;
pub(crate) const MIN_MAIN_WIDTH: u16 = 40;

#[derive(Debug)]
pub(crate) struct FilesSidebar {
    pub(crate) visible: bool,
    pub(crate) focused: bool,
    pub(crate) default_width: u16,
    pub(crate) dragged_width: Option<u16>,
    pub(crate) tree: Option<FileTreeState>,
    pub(crate) area: Option<Rect>,
    pub(crate) divider_column: Option<u16>,
    pub(crate) dragging: bool,
    pub(crate) redraw_after_image_update: bool,
    pub(crate) row_hits: Vec<(u16, usize)>,
    pub(crate) view_opened_from_sidebar: bool,
}

impl Default for FilesSidebar {
    fn default() -> Self {
        Self {
            visible: false,
            focused: false,
            default_width: u16::try_from(cagent_agent::config::DEFAULT_FILES_WIDTH)
                .unwrap_or(u16::MAX),
            dragged_width: None,
            tree: None,
            area: None,
            divider_column: None,
            dragging: false,
            redraw_after_image_update: false,
            row_hits: Vec::new(),
            view_opened_from_sidebar: false,
        }
    }
}

impl FilesSidebar {
    pub(crate) fn requested_width(&self) -> u16 {
        self.dragged_width.unwrap_or(self.default_width)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FilesPaneLayout {
    pub(crate) main: Rect,
    pub(crate) sidebar: Option<Rect>,
}

impl super::App {
    fn directory_click_is_double(&mut self, tree_id: u64, path: &std::path::Path) -> bool {
        let now = Instant::now();
        let double = self.last_directory_click.as_ref().is_some_and(
            |(previous_at, previous_tree, previous_path)| {
                *previous_tree == tree_id
                    && previous_path == path
                    && now.duration_since(*previous_at) <= Duration::from_millis(500)
            },
        );
        self.last_directory_click = if double {
            None
        } else {
            Some((now, tree_id, path.to_path_buf()))
        };
        double
    }

    pub(crate) fn click_expanded_directory_index(&mut self, index: usize) -> bool {
        let target = {
            let Some(super::Surface::Expanded {
                view: super::ExpandedView::Directory { browser },
                scroll,
                viewport_rows,
            }) = self.surfaces.last_mut()
            else {
                return false;
            };
            browser.select_index(index);
            reconcile_directory_scroll(browser, scroll, *viewport_rows);
            browser
                .tree
                .rows()
                .get(browser.selected_index())
                .filter(|row| row.is_selectable())
                .map(|row| (browser.tree.id(), row.path.clone()))
        };
        let Some((tree_id, path)) = target else {
            self.last_directory_click = None;
            return true;
        };
        let activate = self.directory_click_is_double(tree_id, &path);
        let open = if activate {
            let Some(super::Surface::Expanded {
                view: super::ExpandedView::Directory { browser },
                scroll,
                viewport_rows,
            }) = self.surfaces.last_mut()
            else {
                return true;
            };
            let open = browser.activate_index(index);
            reconcile_directory_scroll(browser, scroll, *viewport_rows);
            open
        } else {
            None
        };
        self.expanded_text_render_cache.get_mut().take();
        if let Some(path) = open {
            self.open_path(path);
        }
        true
    }

    #[cfg(test)]
    pub(crate) fn complete_directory_loads_for_test(&mut self) {
        loop {
            let requests = self.take_directory_load_requests();
            if requests.is_empty() {
                break;
            }
            for request in requests {
                self.apply_directory_load_result(request.load_blocking());
            }
        }
    }

    pub(crate) fn take_directory_load_requests(
        &mut self,
    ) -> Vec<cagent_agent::presentation::DirectoryLoadRequest> {
        let mut requests = Vec::new();
        if self.files_sidebar.visible
            && let Some(state) = &mut self.files_sidebar.tree
        {
            requests.extend(state.browser.tree.take_load_requests());
        }
        if let Some(super::Surface::Expanded {
            view: super::ExpandedView::Directory { browser },
            ..
        }) = self.surfaces.last_mut()
        {
            requests.extend(browser.tree.take_load_requests());
        }
        requests
    }

    pub(crate) fn apply_directory_load_result(
        &mut self,
        result: cagent_agent::presentation::DirectoryLoadResult,
    ) {
        let tree_id = result.tree_id();
        if let Some(state) = &mut self.files_sidebar.tree
            && state.browser.tree.id() == tree_id
        {
            if state.browser.apply_load_result(result) {
                state.unavailable = state.browser.tree.unavailable().map(str::to_owned);
                state.reconcile();
            }
            return;
        }
        let surface_index = self.surfaces.iter().rposition(|surface| {
            matches!(
                surface,
                super::Surface::Expanded {
                    view: super::ExpandedView::Directory { browser },
                    ..
                } if browser.tree.id() == tree_id
            )
        });
        let mut changed = false;
        if let Some(super::Surface::Expanded {
            view: super::ExpandedView::Directory { browser },
            scroll,
            viewport_rows,
        }) = surface_index.and_then(|index| self.surfaces.get_mut(index))
        {
            if browser.apply_load_result(result) {
                let rows = browser.tree.rows();
                let capacity = (*viewport_rows).max(1);
                *scroll = (*scroll).min(rows.len().saturating_sub(capacity));
                changed = true;
            }
        }
        if changed {
            self.expanded_text_render_cache.get_mut().take();
        }
    }

    pub(crate) fn toggle_files_sidebar(&mut self) {
        if self.files_sidebar.visible {
            self.files_sidebar.visible = false;
            self.files_sidebar.focused = false;
            self.files_sidebar.dragging = false;
            // The image viewport changes when the sidebar disappears. Drop
            // the encoded protocol so the render loop first clears the old
            // placement, then rebuilds it for the full-width viewport.
            if let Some(preview) = &mut self.image_preview {
                preview.protocol = None;
            }
            self.files_sidebar.view_opened_from_sidebar = false;
            if let Some(state) = &mut self.files_sidebar.tree {
                state.browser.tree.suspend();
            }
            return;
        }
        if self.files_sidebar.tree.is_none() {
            match FileTreeState::open(self.workspace.clone()) {
                Ok(tree) => self.files_sidebar.tree = Some(tree),
                Err(error) => {
                    self.set_notice(format!(
                        "could not list {} · {error}",
                        self.workspace.to_string_lossy()
                    ));
                    return;
                }
            }
        } else if let Some(tree) = &mut self.files_sidebar.tree {
            tree.refresh();
        }
        self.files_sidebar.visible = true;
        self.files_sidebar.focused = true;
    }

    pub(crate) fn filesystem_expanded(&self) -> bool {
        matches!(
            self.surfaces.last(),
            Some(super::Surface::Expanded {
                view: super::ExpandedView::File { .. } | super::ExpandedView::Directory { .. },
                ..
            })
        )
    }

    pub(crate) fn sidebar_file_is_open(&self) -> bool {
        self.files_sidebar.visible
            && self.files_sidebar.view_opened_from_sidebar
            && self.surfaces.len() == 1
            && matches!(
                self.surfaces.last(),
                Some(super::Surface::Expanded {
                    view: super::ExpandedView::File { .. },
                    ..
                })
            )
    }

    pub(crate) fn blocking_files_overlay(&self) -> bool {
        self.onboarding
            || self.pending_interaction.is_some()
            || matches!(
                self.surfaces.last(),
                Some(
                    super::Surface::Question { .. }
                        | super::Surface::Permission { .. }
                        | super::Surface::PermissionRuleEdit { .. }
                        | super::Surface::PlanCompletion { .. }
                )
            )
    }

    pub(crate) fn files_sidebar_locked(&self) -> bool {
        self.onboarding
            || self.pending_interaction.is_some()
            || self
                .surfaces
                .last()
                .is_some_and(|surface| !matches!(surface, super::Surface::Expanded { .. }))
            || self.attachment_completion.is_some()
            || !self.slash_suggestions().is_empty()
    }

    pub(crate) fn files_pane_layout(&self, full: Rect) -> FilesPaneLayout {
        if !self.files_sidebar.visible {
            return FilesPaneLayout {
                main: full,
                sidebar: None,
            };
        }
        if full.width < MIN_FILES_WIDTH.saturating_add(MIN_MAIN_WIDTH) {
            if self.blocking_files_overlay()
                || (self.filesystem_expanded() && !self.files_sidebar.focused)
            {
                return FilesPaneLayout {
                    main: full,
                    sidebar: None,
                };
            }
            return FilesPaneLayout {
                main: Rect::new(full.x, full.y, 0, full.height),
                sidebar: Some(full),
            };
        }
        let sidebar_width = self
            .files_sidebar
            .requested_width()
            .clamp(MIN_FILES_WIDTH, full.width.saturating_sub(MIN_MAIN_WIDTH));
        FilesPaneLayout {
            main: Rect::new(
                full.x.saturating_add(sidebar_width),
                full.y,
                full.width.saturating_sub(sidebar_width),
                full.height,
            ),
            sidebar: Some(Rect::new(full.x, full.y, sidebar_width, full.height)),
        }
    }

    pub(crate) fn filesystem_watch_targets(
        &self,
    ) -> Vec<cagent_agent::presentation::FilesystemWatchTarget> {
        use cagent_agent::presentation::FilesystemWatchTarget;

        let mut targets = Vec::new();
        if self.files_sidebar.visible
            && let Some(state) = &self.files_sidebar.tree
        {
            targets.extend(
                state
                    .browser
                    .tree
                    .visible_directories()
                    .into_iter()
                    .map(FilesystemWatchTarget::directory_children),
            );
        }
        if let Some(super::Surface::Expanded { view, .. }) = self.surfaces.last() {
            match view {
                super::ExpandedView::File { view } => {
                    targets.push(FilesystemWatchTarget::file(view.path.clone()));
                }
                super::ExpandedView::Directory { browser } => {
                    targets.extend(
                        browser
                            .tree
                            .visible_directories()
                            .into_iter()
                            .map(FilesystemWatchTarget::directory_children),
                    );
                }
                _ => {}
            }
        }
        targets
    }

    pub(crate) fn refresh_filesystem_views(&mut self, changed_paths: &[PathBuf]) {
        let mut notices = Vec::new();
        let mut expanded_changed = false;
        let mut image_changed = false;
        if self.files_sidebar.visible
            && changed_paths
                .iter()
                .any(|path| path.starts_with(&self.workspace) || path == &self.workspace)
            && let Some(state) = &mut self.files_sidebar.tree
        {
            state.browser.tree.refresh_changed(changed_paths);
        }

        let file_path = self.surfaces.last().and_then(|surface| match surface {
            super::Surface::Expanded {
                view: super::ExpandedView::File { view },
                ..
            } => Some(view.path.clone()),
            _ => None,
        });
        if let Some(path) = file_path
            && changed_paths.iter().any(|changed| changed == &path)
        {
            expanded_changed = true;
            image_changed = true;
            match cagent_agent::presentation::inspect_path(&path) {
                Ok(cagent_agent::presentation::PathView::File(view)) => {
                    if let Some(super::Surface::Expanded {
                        view: super::ExpandedView::File { view: current },
                        ..
                    }) = self.surfaces.last_mut()
                    {
                        *current = view;
                    }
                }
                Ok(cagent_agent::presentation::PathView::Directory(tree)) => {
                    if let Some(super::Surface::Expanded { view, .. }) = self.surfaces.last_mut() {
                        *view = super::ExpandedView::Directory {
                            browser: DirectoryBrowserState::from_tree(tree, 0),
                        };
                    }
                }
                Err(error) => {
                    let message = error.to_string();
                    if let Some(super::Surface::Expanded {
                        view: super::ExpandedView::File { view: current },
                        ..
                    }) = self.surfaces.last_mut()
                    {
                        *current = cagent_agent::presentation::unavailable_file_view(
                            path.clone(),
                            message.clone(),
                        );
                    }
                    notices.push(format!(
                        "could not refresh {} · {message}",
                        path.to_string_lossy()
                    ));
                }
            }
        } else if let Some(super::Surface::Expanded {
            view: super::ExpandedView::Directory { browser },
            scroll,
            viewport_rows,
        }) = self.surfaces.last_mut()
            && changed_paths.iter().any(|path| {
                path.starts_with(browser.tree.root())
                    || path
                        .parent()
                        .is_some_and(|parent| parent == browser.tree.root())
                    || browser
                        .tree
                        .root()
                        .parent()
                        .is_some_and(|parent| parent == path)
            })
        {
            expanded_changed = true;
            browser.tree.refresh_changed(changed_paths);
            let rows = browser.tree.rows();
            browser.select_index(browser.selected_index().min(rows.len().saturating_sub(1)));
            let capacity = (*viewport_rows).max(1);
            *scroll = (*scroll).min(rows.len().saturating_sub(capacity));
        }
        if expanded_changed {
            self.expanded_text_render_cache.get_mut().take();
        }
        if image_changed {
            self.image_preview = None;
        }
        if let Some(notice) = notices.into_iter().next() {
            self.set_notice(notice);
        }
    }

    pub(crate) fn handle_files_key(&mut self, key: KeyEvent) -> bool {
        if !self.files_sidebar.visible {
            return false;
        }
        let code = self.menu_navigation_code(key);
        if !self.files_sidebar.focused {
            if code == KeyCode::Esc && self.filesystem_expanded() && !self.files_sidebar_locked() {
                if self.files_sidebar.view_opened_from_sidebar {
                    let _ = self.surfaces.pop();
                    self.expanded_text_render_cache.get_mut().take();
                    self.image_preview = None;
                    self.files_sidebar.view_opened_from_sidebar = false;
                }
                self.files_sidebar.focused = true;
                return true;
            }
            return false;
        }
        if self.files_sidebar_locked() {
            return false;
        }
        if code == KeyCode::Esc {
            self.toggle_files_sidebar();
            return true;
        }
        if !matches!(
            code,
            KeyCode::Up
                | KeyCode::Down
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::PageUp
                | KeyCode::PageDown
                | KeyCode::Home
                | KeyCode::End
                | KeyCode::Enter
        ) {
            return true;
        }
        let result = self
            .files_sidebar
            .tree
            .as_mut()
            .map(|tree| tree.apply_key(code));
        if let Some(Some(path)) = result {
            self.files_sidebar.focused = false;
            self.files_sidebar.view_opened_from_sidebar = self.open_builtin_path(&path, true);
        }
        true
    }

    pub(crate) fn handle_files_mouse(&mut self, mouse: MouseEvent) -> bool {
        if self.files_sidebar.dragging {
            match mouse.kind {
                MouseEventKind::Drag(MouseButton::Left) => {
                    if let Some(area) = self.files_sidebar.area {
                        let width = mouse.column.saturating_sub(area.x).saturating_add(1);
                        self.files_sidebar.dragged_width = Some(width.max(MIN_FILES_WIDTH));
                    }
                    return true;
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    self.files_sidebar.dragging = false;
                    self.files_sidebar.redraw_after_image_update = self.image_preview.is_some();
                    // Keep the decoded pixels, but force the terminal image
                    // protocol to be rebuilt once after the drag settles.
                    if let Some(preview) = &mut self.image_preview {
                        preview.protocol = None;
                    }
                    return true;
                }
                _ => return true,
            }
        }
        if mouse.kind == MouseEventKind::Down(MouseButton::Left)
            && self.files_sidebar.divider_column == Some(mouse.column)
        {
            self.files_sidebar.dragging = true;
            return true;
        }
        let Some(area) = self.files_sidebar.area else {
            return false;
        };
        let inside = mouse.column >= area.x
            && mouse.column < area.right()
            && mouse.row >= area.y
            && mouse.row < area.bottom();
        if !inside {
            if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                self.files_sidebar.focused = false;
            }
            return false;
        }
        if self.files_sidebar_locked() {
            return true;
        }
        match mouse.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                self.files_sidebar.focused = true;
                if let Some(tree) = &mut self.files_sidebar.tree {
                    tree.wheel(mouse.kind == MouseEventKind::ScrollDown);
                }
                true
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.files_sidebar.focused = true;
                let index = self
                    .files_sidebar
                    .row_hits
                    .iter()
                    .find_map(|(row, index)| (*row == mouse.row).then_some(*index));
                let target = index.and_then(|index| {
                    let state = self.files_sidebar.tree.as_mut()?;
                    state.select_index(index);
                    state
                        .browser
                        .tree
                        .rows()
                        .get(state.selected_index())
                        .filter(|row| row.is_selectable())
                        .map(|row| (state.browser.tree.id(), row.path.clone(), index))
                });
                let result = if let Some((tree_id, path, index)) = target {
                    if self.directory_click_is_double(tree_id, &path) {
                        self.files_sidebar
                            .tree
                            .as_mut()
                            .and_then(|tree| tree.activate_index(index))
                    } else {
                        None
                    }
                } else {
                    self.last_directory_click = None;
                    None
                };
                if let Some(path) = result {
                    self.files_sidebar.focused = false;
                    self.files_sidebar.view_opened_from_sidebar =
                        self.open_builtin_path(&path, true);
                }
                true
            }
            _ => true,
        }
    }
}

#[derive(Debug)]
pub(crate) struct DirectoryBrowserState {
    pub(crate) tree: cagent_agent::presentation::DirectoryTree,
    selected_path: Option<PathBuf>,
}

impl DirectoryBrowserState {
    pub(crate) fn open(root: PathBuf) -> io::Result<Self> {
        Ok(Self::from_tree(
            cagent_agent::presentation::DirectoryTree::open(root)?,
            0,
        ))
    }

    pub(crate) fn from_tree(
        tree: cagent_agent::presentation::DirectoryTree,
        selected: usize,
    ) -> Self {
        let selected_path = tree
            .rows()
            .get(selected)
            .filter(|row| row.is_selectable())
            .map(|row| row.path.clone());
        Self {
            tree,
            selected_path,
        }
    }

    pub(crate) fn selected_index(&self) -> usize {
        let rows = self.tree.rows();
        self.selected_path
            .as_ref()
            .and_then(|path| {
                rows.iter()
                    .position(|row| row.is_selectable() && &row.path == path)
            })
            .or_else(|| {
                rows.iter()
                    .position(cagent_agent::presentation::DirectoryTreeRow::is_selectable)
            })
            .unwrap_or(0)
            .min(rows.len().saturating_sub(1))
    }

    pub(crate) fn select_index(&mut self, index: usize) {
        let rows = self.tree.rows();
        self.selected_path = rows
            .iter()
            .enumerate()
            .skip(index.min(rows.len().saturating_sub(1)))
            .find(|(_, row)| row.is_selectable())
            .or_else(|| {
                rows.iter()
                    .enumerate()
                    .take(index.saturating_add(1))
                    .rev()
                    .find(|(_, row)| row.is_selectable())
            })
            .map(|(_, row)| row.path.clone());
    }

    pub(crate) fn activate_index(&mut self, index: usize) -> Option<PathBuf> {
        self.select_index(index);
        let rows = self.tree.rows();
        let row = rows.get(self.selected_index())?;
        if !row.is_selectable() {
            return None;
        }
        let path = row.path.clone();
        if row.is_directory() {
            self.tree.toggle(&path);
            None
        } else {
            Some(path)
        }
    }

    pub(crate) fn apply_key(&mut self, code: KeyCode, viewport_rows: usize) -> Option<PathBuf> {
        let rows = self.tree.rows();
        let selected = self.selected_index();
        let selectable = rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| row.is_selectable().then_some(index))
            .collect::<Vec<_>>();
        let previous = || {
            selectable
                .iter()
                .copied()
                .rev()
                .find(|index| *index < selected)
        };
        let next = || selectable.iter().copied().find(|index| *index > selected);
        match code {
            KeyCode::Up => self.select_index(previous().unwrap_or(selected)),
            KeyCode::Down => self.select_index(next().unwrap_or(selected)),
            KeyCode::PageUp => self.select_index(selected.saturating_sub(viewport_rows.max(1))),
            KeyCode::PageDown => {
                self.select_index(selected.saturating_add(viewport_rows.max(1)));
            }
            KeyCode::Home => self.select_index(selectable.first().copied().unwrap_or(0)),
            KeyCode::End => self.select_index(selectable.last().copied().unwrap_or(0)),
            KeyCode::Right => {
                if let Some(row) = rows.get(selected)
                    && row.is_directory()
                    && !row.expanded
                {
                    self.tree.toggle(&row.path);
                }
            }
            KeyCode::Left => {
                if let Some(row) = rows.get(selected) {
                    if row.is_directory() && row.expanded {
                        self.tree.toggle(&row.path);
                    } else if row.depth > 0
                        && let Some(parent) = rows[..selected].iter().rposition(|candidate| {
                            candidate.is_directory() && candidate.depth + 1 == row.depth
                        })
                    {
                        self.select_index(parent);
                    }
                }
            }
            KeyCode::Enter => return self.activate_index(selected),
            _ => {}
        }
        None
    }

    pub(crate) fn apply_load_result(
        &mut self,
        result: cagent_agent::presentation::DirectoryLoadResult,
    ) -> bool {
        let old_index = self.selected_index();
        if !self.tree.apply_load_result(result) {
            return false;
        }
        if self.selected_path.as_ref().is_none_or(|selected| {
            !self
                .tree
                .rows()
                .iter()
                .any(|row| row.is_selectable() && &row.path == selected)
        }) {
            self.select_index(old_index);
        }
        true
    }

    pub(crate) fn refresh(&mut self) {
        let old_index = self.selected_index();
        self.tree.refresh();
        if self.selected_path.as_ref().is_none_or(|selected| {
            !self
                .tree
                .rows()
                .iter()
                .any(|row| row.is_selectable() && &row.path == selected)
        }) {
            self.select_index(old_index);
        }
    }
}

#[derive(Debug)]
pub(crate) struct FileTreeState {
    pub(crate) browser: DirectoryBrowserState,
    pub(crate) scroll: usize,
    pub(crate) viewport_rows: usize,
    pub(crate) unavailable: Option<String>,
}

impl FileTreeState {
    pub(crate) fn open(root: PathBuf) -> io::Result<Self> {
        Ok(Self {
            browser: DirectoryBrowserState::open(root)?,
            scroll: 0,
            viewport_rows: 1,
            unavailable: None,
        })
    }

    pub(crate) fn selected_index(&self) -> usize {
        self.browser.selected_index()
    }

    pub(crate) fn set_viewport_rows(&mut self, viewport_rows: usize) {
        self.viewport_rows = viewport_rows.max(1);
        self.reconcile();
    }

    pub(crate) fn select_index(&mut self, index: usize) {
        self.browser.select_index(index);
        self.reconcile();
    }

    pub(crate) fn activate_index(&mut self, index: usize) -> Option<PathBuf> {
        let result = self.browser.activate_index(index);
        self.reconcile();
        result
    }

    pub(crate) fn apply_key(&mut self, code: KeyCode) -> Option<PathBuf> {
        let result = self.browser.apply_key(code, self.viewport_rows);
        self.reconcile();
        result
    }

    pub(crate) fn wheel(&mut self, down: bool) {
        let rows = self.browser.tree.rows();
        let selected = self.selected_index();
        let selectable = rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| row.is_selectable().then_some(index))
            .collect::<Vec<_>>();
        let position = selectable
            .iter()
            .position(|index| *index == selected)
            .unwrap_or(0);
        let target = if down {
            position
                .saturating_add(3)
                .min(selectable.len().saturating_sub(1))
        } else {
            position.saturating_sub(3)
        };
        self.select_index(selectable.get(target).copied().unwrap_or(0));
    }

    pub(crate) fn refresh(&mut self) {
        self.browser.refresh();
        self.reconcile();
    }

    fn reconcile(&mut self) {
        reconcile_directory_scroll(&self.browser, &mut self.scroll, self.viewport_rows);
    }
}

pub(crate) fn reconcile_directory_scroll(
    browser: &DirectoryBrowserState,
    scroll: &mut usize,
    viewport_rows: usize,
) {
    let row_count = browser.tree.rows().len();
    let selected = browser.selected_index();
    let capacity = viewport_rows.max(1);
    if selected < *scroll {
        *scroll = selected;
    } else if selected >= scroll.saturating_add(capacity) {
        *scroll = selected.saturating_add(1).saturating_sub(capacity);
    }
    *scroll = (*scroll).min(row_count.saturating_sub(capacity));
}

pub(crate) fn styled_rows(
    tree: &cagent_agent::presentation::DirectoryTree,
    selected: usize,
    width: u16,
) -> Vec<crate::markdown::DisplayRow> {
    use cagent_agent::presentation::DirectoryEntryKind;

    tree.rows()
        .into_iter()
        .enumerate()
        .map(|(index, entry)| {
            let selectable = entry.is_selectable();
            let is_selected = selectable && index == selected;
            let background = if index.is_multiple_of(2) {
                Style::new().bg(Color::Indexed(235))
            } else {
                Style::new().bg(Color::Indexed(236))
            };
            let selected_style = background.patch(SELECTED_STYLE);
            let row_style = if is_selected {
                selected_style
            } else {
                background
            };
            let selection = if is_selected { "›" } else { " " };
            let disclosure = match entry.kind {
                DirectoryEntryKind::Directory if entry.expanded => "▾ ",
                DirectoryEntryKind::Directory => "▸ ",
                DirectoryEntryKind::File
                | DirectoryEntryKind::Symlink
                | DirectoryEntryKind::Other
                | DirectoryEntryKind::Loading
                | DirectoryEntryKind::Unavailable => "  ",
            };
            let selection_prefix = format!(" {selection} ");
            let icon_prefix = format!("{disclosure}{} ", entry.icon());
            let prefix_width = selection_prefix.width() + entry.guide.width() + icon_prefix.width();
            let available = usize::from(width).saturating_sub(prefix_width);
            let name = crate::render::truncate_with_ellipsis(
                &crate::render::terminal_safe(&entry.name),
                available,
            );
            let name_style = if entry.kind == DirectoryEntryKind::Loading {
                row_style.patch(DIM_STYLE)
            } else if entry.kind == DirectoryEntryKind::Unavailable {
                row_style.patch(ERROR_STYLE)
            } else if entry.name.starts_with('.') {
                row_style.patch(DIM_STYLE)
            } else {
                row_style
            };
            let icon_style = if is_selected {
                selected_style
            } else {
                match entry.kind {
                    DirectoryEntryKind::Directory => background.fg(Color::Blue),
                    DirectoryEntryKind::Symlink => background.fg(Color::Cyan),
                    DirectoryEntryKind::Loading => background.patch(DIM_STYLE),
                    DirectoryEntryKind::Unavailable => background.patch(ERROR_STYLE),
                    DirectoryEntryKind::File | DirectoryEntryKind::Other => background,
                }
            };
            let name_width = name.width();
            let used = prefix_width.saturating_add(name_width);
            let row = crate::markdown::DisplayRow::plain(
                Line::from(vec![
                    Span::styled(selection_prefix, row_style),
                    Span::styled(entry.guide, background.patch(DIM_STYLE)),
                    Span::styled(icon_prefix, icon_style),
                    Span::styled(name, name_style),
                    Span::styled(
                        " ".repeat(usize::from(width).saturating_sub(used)),
                        background,
                    ),
                ])
                .style(row_style),
            );
            row
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use ratatui::style::Modifier;

    use super::*;

    #[test]
    fn shared_tree_rows_dim_hidden_names() {
        let temporary = tempfile::tempdir().unwrap();
        std::fs::write(temporary.path().join(".hidden"), "hidden\n").unwrap();
        std::fs::write(temporary.path().join("visible"), "visible\n").unwrap();
        let mut tree =
            cagent_agent::presentation::DirectoryTree::open(temporary.path().to_path_buf())
                .unwrap();
        for request in tree.take_load_requests() {
            assert!(tree.apply_load_result(request.load_blocking()));
        }

        let rows = styled_rows(&tree, usize::MAX, 80);
        let style_for = |name: &str| {
            rows.iter()
                .flat_map(|row| &row.line.spans)
                .find(|span| span.content == name)
                .map(|span| span.style)
                .unwrap()
        };
        assert!(style_for(".hidden").add_modifier.contains(Modifier::DIM));
        assert!(!style_for("visible").add_modifier.contains(Modifier::DIM));
    }
}
