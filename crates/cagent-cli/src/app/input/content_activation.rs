//! Activation of content represented by transcript and expanded surfaces.
//!
//! Content activation owns the hit-testing decisions used by transcript
//! links.  Keeping these predicates here means the event handlers only need
//! to perform the resulting activation.

use super::super::*;
use super::super::{DiffHit, ImageHit, PathHit};

impl App {
    pub(crate) fn link_at(&self, row: u16, column: u16) -> Option<&str> {
        if !self.open_links {
            return None;
        }
        self.link_hits
            .iter()
            .find_map(|hit| hit.destination_at(row, column))
    }

    pub(crate) fn open_link_at(&mut self, row: u16, column: u16) -> bool {
        let Some(destination) = self.link_at(row, column) else {
            return false;
        };
        if !open_browser(destination) {
            self.set_notice("browser unavailable");
        }
        true
    }

    pub(crate) fn open_diff_at(&mut self, row: u16) -> bool {
        if !self.surfaces.is_empty() {
            return false;
        }
        let Some(hit) = diff_hit_at(&self.diff_hits, row) else {
            return false;
        };
        let Some(file) = hit.target.diff.files.get(hit.target.file_index) else {
            return false;
        };
        let view = match cagent_agent::presentation::inspect_full_file_diff(file, &self.workspace) {
            Ok(view) => view,
            Err(error) => {
                self.set_notice(format!("could not open diff · {error}"));
                return true;
            }
        };
        let viewport_rows = usize::from(self.render_height.saturating_sub(4).max(1));
        let target = crate::app::surfaces::full_file_diff_line_offset(
            &view,
            self.render_width,
            hit.target.old_line,
            hit.target.new_line,
        );
        let (visible_rows, maximum) = crate::app::surfaces::full_file_diff_scroll_metrics(
            &view,
            self.render_width,
            viewport_rows,
        );
        self.expanded_text_render_cache.get_mut().take();
        self.image_preview = None;
        self.surfaces.push(Surface::Expanded {
            view: ExpandedView::Diff { view },
            scroll: target.saturating_sub(visible_rows / 2).min(maximum),
            viewport_rows,
        });
        true
    }

    pub(super) async fn open_image_at(
        &mut self,
        session: &SessionHandle,
        row: u16,
        column: u16,
    ) -> bool {
        if !self.surfaces.is_empty() {
            return false;
        }
        let Some(hit) = image_hit_at(&self.image_hits, row, column) else {
            return false;
        };
        match session.load_blob(hit.image.blob_id).await {
            Ok(png) => {
                self.expanded_text_render_cache.get_mut().take();
                self.image_preview = None;
                self.surfaces.push(Surface::Expanded {
                    view: ExpandedView::Image {
                        metadata: hit.image,
                        png,
                    },
                    scroll: 0,
                    viewport_rows: usize::from(self.render_height.saturating_sub(4).max(1)),
                });
            }
            Err(error) => self.set_notice(format!("could not load image · {error}")),
        }
        true
    }

    pub(crate) fn open_path_at(&mut self, row: u16, column: u16) -> bool {
        if !self.surfaces.is_empty() {
            return false;
        }
        let Some(hit) = path_hit_at(&self.path_hits, row, column) else {
            return false;
        };
        if hit.directory {
            self.open_builtin_path(&hit.path, false);
            return true;
        }
        if self.activate_directory_path(&hit.path) {
            return true;
        }
        self.open_path_at_line(hit.path, hit.line);
        true
    }
}

pub(super) fn path_hit_at(hits: &[PathHit], row: u16, column: u16) -> Option<PathHit> {
    hits.iter()
        .find(|hit| hit.row == row && hit.columns.contains(&column))
        .cloned()
}

pub(super) fn image_hit_at(hits: &[ImageHit], row: u16, column: u16) -> Option<ImageHit> {
    hits.iter()
        .find(|hit| hit.row == row && hit.columns.contains(&column))
        .cloned()
}

pub(super) fn diff_hit_at(hits: &[DiffHit], row: u16) -> Option<DiffHit> {
    hits.iter().find(|hit| hit.row == row).cloned()
}

/// Returns the keybinding action represented by a clickable status hint.
///
/// `Enter/Esc close` is one combined hint, but expanded output intentionally
/// ignores Enter. Clicking it must therefore use its close behavior.
pub(crate) fn status_hint_action(
    hint: &str,
) -> Option<cagent_agent::presentation::KeyBindingAction> {
    use cagent_agent::presentation::KeyBindingAction;

    if hint.starts_with("Enter/Esc") && hint.ends_with(" close") {
        Some(KeyBindingAction::CloseSurface)
    } else if hint.starts_with("Enter") {
        Some(KeyBindingAction::Submit)
    } else if hint.starts_with("Esc") {
        Some(KeyBindingAction::CloseSurface)
    } else {
        None
    }
}
