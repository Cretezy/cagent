mod activity;
mod diff;
mod status;
mod terminal;
mod text;
pub(crate) mod transcript;
mod welcome;

#[cfg(test)]
pub(crate) use activity::render_tool_groups;
pub(crate) use activity::{
    active_plan_lines, bash_command_spans, render_tool_groups_with_workspace, working_line,
};
pub(crate) use diff::{
    DIFF_ADDITION_STYLE, DIFF_DELETION_STYLE, DiffPreviewLayout, diff_preview_line_count,
    edit_history_line_targets, render_diff_preview_window, render_edit_history,
    render_expanded_diff,
};
pub(crate) use status::{
    status_line_background_hint_at, status_line_color, status_line_content, status_line_module_at,
};
pub(crate) use terminal::TerminalRenderCache;
pub(crate) use terminal::scrollback_max_with_completion as terminal_scrollback_max_with_completion;
pub(crate) use text::{
    byte_offset_at_display_column, compact_text, padded_status_line, rendered_cursor_line,
    visual_text_rows, wrap_ranges,
};
pub(crate) use welcome::{
    tip_for_session, truncate_with_ellipsis, welcome_card_rows, welcome_lines,
};

use transcript::WrappedLayout;

#[cfg(test)]
use crate::app::AgentWizardStep;
use crate::app::list::{ListHit, ListLayout, ListMode, ListWidget, truncate_line};
use crate::app::scroll::{ScrollViewHit, ScrollViewLayout, ScrollViewState, ScrollViewWidget};
use crate::app::surfaces::{
    cached_expanded_surface_lines, cached_permission_surface_line_count,
    cached_permission_surface_lines_with_viewport, list_entry_kind, menu_choice_line,
    surface_lines, surface_lines_with_viewport, surface_status,
};
use crate::app::{
    ACCENT_STYLE, App, AttachmentChip, BASH_STYLE, CHIP_STYLE, COMMAND_STYLE, COMPOSER_PLACEHOLDER,
    COMPOSER_STYLE, DIM_STYLE, ERROR_STYLE, ExpandedView, MENU_TITLE_STYLE, NOTICE_DURATION,
    READ_ONLY_COMPOSER_PLACEHOLDER, StatusLineEditorMode, Surface, TranscriptBlock, USER_STYLE,
    VISIBLE_MENU_ITEMS, WORKING_SHIMMER_PERIOD,
};
use cagent_agent::WorkspaceEntryKind as ListEntryKind;
use cagent_agent::presentation::StatusLineModule;
use cagent_agent::protocol::QueueTarget;
use ratatui::DefaultTerminal;
use ratatui::buffer::CellDiffOption;
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};
use std::io;
use std::path::Path;
use std::time::Instant;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const SCROLL_INDICATOR_HEIGHT: u16 = 1;

fn workspace_display_path(workspace: &Path) -> String {
    let project = workspace
        .ancestors()
        .find(|path| path.join(".git").exists() || path.join(".jj").exists())
        .unwrap_or(workspace);
    let project_name = project.file_name().map_or_else(
        || project.to_string_lossy().into_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    let relative = workspace
        .strip_prefix(project)
        .ok()
        .filter(|path| !path.as_os_str().is_empty());
    match relative {
        Some(relative) => format!("{project_name}/{}", relative.to_string_lossy()),
        None => project_name,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ChipSubstitution {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) label: String,
}

/// The complete visual geometry of the composer for one terminal width and
/// viewport capacity. Rendering and input consume this prepared value instead
/// of independently substituting chips, wrapping text, and locating the caret.
pub(crate) struct ComposerViewportLayout {
    pub(crate) rendered: String,
    pub(crate) ranges: Vec<(usize, usize)>,
    pub(crate) caret: Option<(usize, usize)>,
    pub(crate) state: ScrollViewState,
    pub(crate) scroll: ScrollViewLayout,
    substitutions: Vec<ChipSubstitution>,
    source_len: usize,
}

impl ComposerViewportLayout {
    pub(crate) fn rendered_offset_at(&self, row: usize, column: usize) -> Option<usize> {
        let (start, end) = *self.ranges.get(row)?;
        Some(start + byte_offset_at_display_column(&self.rendered[start..end], column))
    }

    pub(crate) fn source_offset_at(&self, offset: usize) -> usize {
        let mut source = 0;
        let mut rendered = 0;
        for substitution in &self.substitutions {
            let plain_end = rendered + substitution.start - source;
            if offset <= plain_end {
                return (source + offset.saturating_sub(rendered)).min(self.source_len);
            }
            let label_end = plain_end + substitution.label.len();
            if offset < label_end {
                return substitution.start.min(self.source_len);
            }
            rendered = label_end;
            source = substitution.end;
        }
        (source + offset.saturating_sub(rendered)).min(self.source_len)
    }

    pub(crate) fn collapsed_chip_at(&self, offset: usize) -> Option<usize> {
        let mut source = 0;
        let mut rendered = 0;
        for substitution in &self.substitutions {
            rendered += substitution.start.saturating_sub(source);
            let label_end = rendered.saturating_add(substitution.label.len());
            if (rendered..=label_end).contains(&offset) {
                return Some(substitution.start);
            }
            rendered = label_end;
            source = substitution.end;
        }
        None
    }

    pub(crate) fn cursor_position(&self, area: Rect) -> Option<Position> {
        let (row, column) = self.caret?;
        let relative_x = u16::try_from(column.saturating_add(2))
            .unwrap_or(u16::MAX)
            .min(area.width.saturating_sub(1));
        Some(Position::new(
            area.x
                .saturating_add(relative_x)
                .min(area.right().saturating_sub(1)),
            area.y
                .saturating_add(
                    u16::try_from(row.saturating_sub(self.state.offset)).unwrap_or(u16::MAX),
                )
                .min(area.bottom().saturating_sub(1)),
        ))
    }

    pub(crate) fn hit(&self, layout_row: usize) -> Option<ScrollViewHit> {
        self.scroll.hits.get(layout_row).copied()
    }
}

fn sticky_rows_height(terminal_height: u16, status_height: u16) -> u16 {
    terminal_height.saturating_sub(status_height).min(2)
}

fn scroll_indicator_height(terminal_height: u16, status_height: u16) -> u16 {
    sticky_rows_height(terminal_height, status_height).min(SCROLL_INDICATOR_HEIGHT)
}

fn scroll_padding_height(terminal_height: u16, status_height: u16) -> u16 {
    sticky_rows_height(terminal_height, status_height)
        .saturating_sub(scroll_indicator_height(terminal_height, status_height))
}

/// Keeps an oversized permission prompt from replacing the transcript.
///
/// The prompt still gets enough rows to show its fixed chrome, choices, and a
/// preview window on short terminals. Beyond that minimum, reserve roughly a
/// quarter of the available main pane for transcript context.
fn permission_history_reserve(available_rows: u16) -> u16 {
    const MIN_PERMISSION_PANEL_ROWS: u16 = 13;

    available_rows
        .saturating_add(3)
        .checked_div(4)
        .unwrap_or_default()
        .min(available_rows.saturating_sub(MIN_PERMISSION_PANEL_ROWS))
}

fn scroll_indicator_line(
    scrolled: bool,
    width: u16,
    has_queue: bool,
    expanded_view_open: bool,
) -> Line<'static> {
    if expanded_view_open || (!scrolled && !has_queue) {
        return Line::default();
    }

    let prefix_width = if scrolled { 4 } else { 0 };
    let separator = "─".repeat(usize::from(width.saturating_sub(prefix_width)));
    if scrolled {
        Line::from(vec![
            Span::styled("─ ", DIM_STYLE),
            Span::styled("↓", DIM_STYLE),
            Span::styled(" ", DIM_STYLE),
            Span::styled(separator, DIM_STYLE),
        ])
    } else {
        Line::from(Span::styled(separator, DIM_STYLE))
    }
}

fn trim_trailing_log_blank_rows(
    rows: &[crate::markdown::DisplayRow],
) -> &[crate::markdown::DisplayRow] {
    let mut end = rows.len();
    // User messages deliberately carry styled blank rows above and below their
    // text. Keep those rows when they are the last transcript content (for
    // example, while a permission surface replaces the composer).
    while end > 0
        && rows[end - 1].line.style != USER_STYLE
        && rows[end - 1].line.to_string().trim().is_empty()
    {
        end -= 1;
    }
    &rows[..end]
}

/// Returns the rendered height of committed and live transcript sections,
/// including the visual boundaries owned by this view.
fn transcript_sections_len(
    sections: &[&[crate::markdown::DisplayRow]],
    separator_before_streaming_assistant: bool,
) -> usize {
    let mut total = 0_usize;
    let mut has_content = false;
    for (index, section) in sections.iter().enumerate() {
        let section = trim_trailing_log_blank_rows(section);
        if section.is_empty() {
            continue;
        }
        if has_content {
            total = total.saturating_add(if index == 1 && separator_before_streaming_assistant {
                3
            } else {
                1
            });
        }
        total = total.saturating_add(section.len());
        has_content = true;
    }
    total
}

/// Appends only the visible portion of the virtual transcript concatenation.
/// Loaded history can be arbitrarily large without being cloned each frame.
fn append_visible_transcript_sections(
    sections: &[&[crate::markdown::DisplayRow]],
    width: u16,
    separator_before_streaming_assistant: bool,
    mut offset: usize,
    limit: usize,
    output: &mut Vec<crate::markdown::DisplayRow>,
) {
    let mut remaining = limit;
    let mut has_content = false;
    for (index, section) in sections.iter().enumerate() {
        let section = trim_trailing_log_blank_rows(section);
        if section.is_empty() {
            continue;
        }
        if has_content {
            if index == 1 && separator_before_streaming_assistant {
                let separator = dim_separator_rows(width);
                transcript::append_visible_rows(&separator, &mut offset, &mut remaining, output);
            } else {
                let separator = [crate::markdown::DisplayRow::plain(Line::default())];
                transcript::append_visible_rows(&separator, &mut offset, &mut remaining, output);
            }
        }
        transcript::append_visible_rows(section, &mut offset, &mut remaining, output);
        has_content = true;
        if remaining == 0 {
            break;
        }
    }
}

#[cfg(test)]
fn join_transcript_sections(
    sections: &[&[crate::markdown::DisplayRow]],
    width: u16,
    separator_before_streaming_assistant: bool,
) -> Vec<crate::markdown::DisplayRow> {
    let len = transcript_sections_len(sections, separator_before_streaming_assistant);
    let mut joined = Vec::with_capacity(len);
    append_visible_transcript_sections(
        sections,
        width,
        separator_before_streaming_assistant,
        0,
        len,
        &mut joined,
    );
    joined
}

impl App {
    pub(super) fn cached_terminal_scrollback_max(
        &self,
        output: &str,
        completion: Option<&str>,
        width: u16,
        height: u16,
    ) -> Option<usize> {
        self.terminal_render_cache.as_ref().and_then(|cache| {
            cache
                .matches_by_identity(
                    output.as_ptr() as usize,
                    output.len(),
                    completion.map(|value| value.as_ptr() as usize),
                    completion.map_or(0, str::len),
                    width,
                    height,
                )
                .then(|| cache.maximum_scrollback())
        })
    }

    fn prepare_expanded_terminal_cache(&mut self, area: Rect) -> Option<usize> {
        let (output_address, output_len, completion_address, completion_len, scroll) =
            self.surfaces.last().and_then(|surface| {
                let Surface::Expanded {
                    view:
                        crate::app::ExpandedView::Terminal {
                            ansi_output,
                            completion,
                            ..
                        },
                    scroll,
                    ..
                } = surface
                else {
                    return None;
                };
                Some((
                    ansi_output.as_ptr() as usize,
                    ansi_output.len(),
                    completion.as_ref().map(|value| value.as_ptr() as usize),
                    completion.as_ref().map_or(0, String::len),
                    *scroll,
                ))
            })?;
        let cache_matches = self.terminal_render_cache.as_ref().is_some_and(|cache| {
            cache.matches_by_identity(
                output_address,
                output_len,
                completion_address,
                completion_len,
                area.width,
                area.height,
            )
        });
        if !cache_matches {
            let Surface::Expanded {
                view:
                    crate::app::ExpandedView::Terminal {
                        ansi_output,
                        completion,
                        ..
                    },
                ..
            } = self
                .surfaces
                .last()
                .expect("expanded terminal was just matched")
            else {
                unreachable!("expanded terminal changed during one render");
            };
            self.terminal_render_cache = Some(terminal::TerminalRenderCache::new(
                ansi_output,
                completion.as_deref(),
                area.width,
                area.height,
            ));
        }
        let cache = self
            .terminal_render_cache
            .as_mut()
            .expect("expanded terminal cache was initialized");
        cache.set_top_scroll_offset(scroll);
        Some(cache.maximum_scrollback())
    }

    fn prepare_expanded_image_cache(&mut self, area: Rect) {
        let source = self.surfaces.last().and_then(|surface| match surface {
            Surface::Expanded {
                view:
                    ExpandedView::File {
                        view:
                            cagent_agent::presentation::FileView {
                                path,
                                content: cagent_agent::presentation::FileViewContent::Image { .. },
                                ..
                            },
                    },
                ..
            } => Some((
                path.to_string_lossy().into_owned(),
                image::ImageReader::open(path)
                    .map_err(|error| error.to_string())
                    .and_then(|reader| {
                        reader
                            .with_guessed_format()
                            .map_err(|error| error.to_string())
                    })
                    .and_then(|reader| reader.decode().map_err(|error| error.to_string())),
            )),
            Surface::Expanded {
                view: ExpandedView::Image { metadata, png },
                ..
            } => Some((
                metadata.id.to_string(),
                image::load_from_memory_with_format(png, image::ImageFormat::Png)
                    .map_err(|error| error.to_string()),
            )),
            _ => None,
        });
        let Some((key, decoded)) = source else {
            self.image_preview = None;
            return;
        };
        if self
            .image_preview
            .as_ref()
            .is_none_or(|preview| preview.key != key)
        {
            match decoded {
                Ok(image) => {
                    self.image_preview = Some(crate::app::ImagePreview {
                        key: key.clone(),
                        image: Some(image),
                        protocol: None,
                        error: None,
                    });
                }
                Err(error) => {
                    self.set_notice(format!("could not decode image · {error}",));
                    self.image_preview = Some(crate::app::ImagePreview {
                        key,
                        image: None,
                        protocol: None,
                        error: Some(error),
                    });
                    return;
                }
            }
        }
        // Sidebar drags can produce many frames per second. Keep the last
        // encoded protocol while the divider is moving and re-encode once the
        // mouse is released; image encoding is synchronous and can otherwise
        // make the drag feel sticky, especially for large images.
        if self.files_sidebar.dragging
            && self
                .image_preview
                .as_ref()
                .is_some_and(|preview| preview.key == key && preview.protocol.is_some())
        {
            return;
        }
        let size = ratatui::layout::Size::new(area.width.max(1), area.height.max(1));
        let should_prepare = self.image_preview.as_ref().is_some_and(|preview| {
            preview.error.is_none()
                && preview
                    .protocol
                    .as_ref()
                    .is_none_or(|(prepared, _)| *prepared != size)
        });
        if !should_prepare {
            return;
        }
        let image = self
            .image_preview
            .as_ref()
            .and_then(|preview| preview.image.as_ref())
            .cloned()
            .expect("a decodable image was just checked");
        match self
            .image_picker
            .new_protocol(image, size, ratatui_image::Resize::Fit(None))
        {
            Ok(protocol) => {
                if let Some(preview) = &mut self.image_preview {
                    preview.protocol = Some((size, protocol));
                }
                // Kitty's first placement can disturb Crossterm's assumed
                // cursor position. Re-emit the complete sidebar in the same
                // frame whenever an image is opened or re-encoded.
                self.files_sidebar.redraw_after_image_update = true;
            }
            Err(error) => {
                self.set_notice(format!("could not prepare terminal image · {error}"));
                if let Some(preview) = &mut self.image_preview {
                    preview.error = Some(error.to_string());
                }
            }
        }
    }

    fn expanded_image_open(&self) -> bool {
        match self.surfaces.last() {
            Some(Surface::Expanded {
                view:
                    ExpandedView::File {
                        view:
                            cagent_agent::presentation::FileView {
                                path: _,
                                content: cagent_agent::presentation::FileViewContent::Image { .. },
                                ..
                            },
                    },
                ..
            }) => true,
            Some(Surface::Expanded {
                view: ExpandedView::Image { .. },
                ..
            }) => true,
            _ => false,
        }
    }

    fn image_protocol(protocol: &ratatui_image::protocol::Protocol) -> crate::app::ImageProtocol {
        match protocol {
            ratatui_image::protocol::Protocol::Halfblocks(_) => {
                crate::app::ImageProtocol::Halfblocks
            }
            ratatui_image::protocol::Protocol::Sixel(_) => crate::app::ImageProtocol::Sixel,
            ratatui_image::protocol::Protocol::Kitty(_) => crate::app::ImageProtocol::Kitty,
            ratatui_image::protocol::Protocol::ITerm2(_) => crate::app::ImageProtocol::Iterm2,
        }
    }

    fn clear_rendered_image<W: io::Write>(
        writer: &mut W,
        image: &crate::app::RenderedImage,
    ) -> io::Result<()> {
        // Kitty graphics are independent terminal objects. Erasing the cells
        // below them does not remove the old placement, so delete the graphics
        // before repainting the area. The image ID is private to ratatui-image,
        // making delete-all the only public-API-safe option here.
        if image.protocol == crate::app::ImageProtocol::Kitty {
            writer.write_all(b"\x1b_Ga=d,d=A\x1b\\")?;
        }

        let area = image.area;
        if area.width > 0 && area.height > 0 {
            write!(
                writer,
                "\x1b7\x1b[{};{}H",
                area.y.saturating_add(1),
                area.x.saturating_add(1)
            )?;
            for row in 0..area.height {
                write!(writer, "\x1b[{}X", area.width)?;
                if row + 1 < area.height {
                    writer.write_all(b"\x1b[1B")?;
                }
            }
            writer.write_all(b"\x1b8")?;
        }
        writer.flush()
    }

    fn clear_stale_image_before_draw(&mut self) -> io::Result<bool> {
        let Some(previous) = self.rendered_image.clone() else {
            return Ok(false);
        };
        let image_open = self.expanded_image_open();
        let current_protocol = self
            .image_preview
            .as_ref()
            .and_then(|preview| preview.protocol.as_ref())
            .map(|(_, protocol)| Self::image_protocol(protocol));
        if image_open
            && self
                .image_preview
                .as_ref()
                .is_some_and(|preview| preview.key == previous.key)
            && current_protocol == Some(previous.protocol)
        {
            return Ok(false);
        }

        let mut stdout = io::stdout();
        Self::clear_rendered_image(&mut stdout, &previous)?;
        self.image_redraw_area = Some(previous.area);
        self.rendered_image = None;
        Ok(true)
    }

    fn force_regular_cell_redraw(frame: &mut ratatui::Frame<'_>, area: Rect) {
        let area = frame.area().intersection(area);
        let buffer = frame.buffer_mut();
        for position in area.positions() {
            let Some(cell) = buffer.cell_mut(position) else {
                continue;
            };
            if cell.diff_option == CellDiffOption::None {
                cell.set_diff_option(CellDiffOption::AlwaysUpdate);
            }
        }
    }

    fn force_image_cleanup_redraw(&mut self, frame: &mut ratatui::Frame<'_>) {
        let Some(area) = self.image_redraw_area.take() else {
            return;
        };
        Self::force_regular_cell_redraw(frame, area);
    }

    pub(super) fn rendered_draft(&self) -> String {
        let substitutions = self.display_substitutions();
        self.rendered_draft_with_substitutions(&substitutions)
    }

    fn rendered_draft_with_substitutions(&self, substitutions: &[ChipSubstitution]) -> String {
        if self.observer_command_active() {
            return self.observer_command_text().unwrap_or_default().to_owned();
        }
        let mut output = String::new();
        let mut source = 0;
        for substitution in substitutions {
            output.push_str(&self.draft[source..substitution.start]);
            output.push_str(&substitution.label);
            source = substitution.end;
        }
        output.push_str(&self.draft[source..]);
        output
    }

    pub(super) fn display_substitutions(&self) -> Vec<ChipSubstitution> {
        if self.observer_command_active() {
            return Vec::new();
        }
        let mut substitutions = self
            .pastes
            .iter()
            .filter(|chip| !chip.range.expanded)
            .map(|chip| ChipSubstitution {
                start: chip.range.start,
                end: chip.range.end,
                label: format!(
                    "[Pasted text · {} lines · {}]",
                    chip.lines,
                    format_bytes(chip.bytes)
                ),
            })
            .collect::<Vec<_>>();
        substitutions.extend(
            self.attachments
                .iter()
                .filter(|chip| !chip.range.expanded && !self.attachment_is_active(chip))
                .map(|chip| ChipSubstitution {
                    start: chip.range.start,
                    end: chip.range.end,
                    label: attachment_chip_label(chip),
                }),
        );
        substitutions.extend(self.images.iter().map(|chip| ChipSubstitution {
            start: chip.range.start,
            end: chip.range.end,
            label: format!("[Image #{}]", chip.image.number),
        }));
        substitutions
            .sort_by_key(|substitution| (substitution.start, std::cmp::Reverse(substitution.end)));
        substitutions
    }

    fn rendered_cursor_with_substitutions(&self, substitutions: &[ChipSubstitution]) -> usize {
        if self.observer_command_active() {
            return self.observer_command_cursor();
        }
        let mut output = 0;
        let mut source = 0;
        for substitution in substitutions {
            if substitution.start >= self.cursor {
                break;
            }
            output += substitution.start - source;
            if self.cursor <= substitution.end {
                return output + substitution.label.len();
            }
            output += substitution.label.len();
            source = substitution.end;
        }
        output + self.cursor.saturating_sub(source)
    }

    #[cfg(test)]
    pub(super) fn source_offset_at_rendered_offset(&self, offset: usize) -> usize {
        if self.observer_command_active() {
            return offset.min(self.observer_command_text().map_or(0, str::len));
        }
        let mut source = 0;
        let mut rendered = 0;
        for substitution in self.display_substitutions() {
            let plain_end = rendered + substitution.start - source;
            if offset <= plain_end {
                return source + offset.saturating_sub(rendered);
            }
            let label_end = plain_end + substitution.label.len();
            if offset < label_end {
                return substitution.start;
            }
            rendered = label_end;
            source = substitution.end;
        }
        source + offset.saturating_sub(rendered)
    }

    pub(super) fn ensure_history_layout(&mut self, width: u16) {
        if self
            .history_layout
            .rendered
            .as_ref()
            .is_some_and(|layout| layout.width == width)
            && self.history_layout.start_block == 0
        {
            return;
        }
        let _span = tracing::trace_span!(
            "frontend.transcript.layout",
            blocks = self.history.len(),
            width
        )
        .entered();
        let rows = {
            let cache_misses = (0..self.history.len())
                .filter(|index| self.ensure_history_block_layout(*index, width))
                .count();
            tracing::trace!(
                cache_hits = self.history.len().saturating_sub(cache_misses),
                cache_misses,
                "transcript block layout cache summary"
            );
            self.history_rows_from_cached_range(0, width)
        };
        self.history_layout.start_block = 0;
        self.history_layout.width = Some(width);
        self.history_layout.rendered = Some(WrappedLayout { width, rows });
    }

    fn ensure_history_tail_layout(&mut self, width: u16, viewport_rows: usize) {
        if self.collapse_tool_activity {
            self.ensure_history_layout(width);
            return;
        }
        let target_rows = viewport_rows.saturating_mul(2).max(viewport_rows);
        let same_width = self.history_layout.width == Some(width);
        if self.history_layout.rendered.as_ref().is_some_and(|layout| {
            layout.width == width
                && (self.history_layout.start_block == 0 || layout.rows.len() >= target_rows)
        }) {
            return;
        }
        let previous_start = self.history_layout.start_block;
        let previous_rows = same_width
            .then(|| {
                self.history_layout
                    .rendered
                    .as_ref()
                    .map(|layout| layout.rows.len())
            })
            .flatten();
        let _span = tracing::trace_span!(
            "frontend.transcript.layout",
            blocks = self.history.len(),
            width,
            tail_first = true
        )
        .entered();
        if self.history_layout.width != Some(width)
            || self.history_layout.start_block > self.history.len()
        {
            self.history_layout.start_block = self.history.len();
            self.history_layout.width = Some(width);
        }

        let mut start = self.history_layout.start_block;
        let mut rows = 0_usize;
        for index in start..self.history.len() {
            self.ensure_history_block_layout(index, width);
            rows = rows.saturating_add(self.cached_history_block_rows(index, width));
        }
        while start > 0 && rows < target_rows {
            start -= 1;
            self.ensure_history_block_layout(start, width);
            rows = rows
                .saturating_add(self.cached_history_block_rows(start, width))
                .saturating_add(1);
        }
        self.history_layout.start_block = start;
        let rows = self.history_rows_from_cached_range(start, width);
        if let Some(previous_rows) = previous_rows
            && start < previous_start
            && !self.follow_history_tail
        {
            self.history_scroll = self
                .history_scroll
                .saturating_add(rows.len().saturating_sub(previous_rows));
        }
        tracing::trace!(
            materialized_blocks = self.history.len().saturating_sub(start),
            deferred_blocks = start,
            rows = rows.len(),
            "tail-first transcript layout ready"
        );
        self.history_layout.rendered = Some(WrappedLayout { width, rows });
    }

    pub(crate) fn history_layout_incomplete(&self) -> bool {
        self.history_layout.start_block > 0
    }

    /// Materializes one older loaded block for explicit upward navigation. If the viewport is
    /// above the tail, add the new prefix height to its top-origin offset so
    /// the visible content remains anchored.
    pub(crate) fn materialize_older_history_layout(&mut self) {
        if !self.history_layout_incomplete() {
            return;
        }
        let width = self.history_layout.width.unwrap_or(self.render_width);
        let mut existing_rows = self
            .history_layout
            .rendered
            .take()
            .expect("tail layout is initialized before older rows are materialized")
            .rows;
        let previous_rows = existing_rows.len();
        let start = self.history_layout.start_block - 1;
        self.ensure_history_block_layout(start, width);
        self.history_layout.start_block = start;
        let id = &self.history[start].id;
        let item_rows = self
            .history_block_layouts
            .get(&(id.clone(), width))
            .expect("newly materialized transcript block is cached");
        let next_visible_item = (start + 1..self.history.len()).find_map(|index| {
            let next_id = &self.history[index].id;
            self.history_block_layouts
                .get(&(next_id.clone(), width))
                .is_some_and(|rows| !rows.is_empty())
                .then_some(&self.history[index])
        });
        let mut rows = transcript::history_prefix_rows(
            if start == 0 && self.transcript_older.is_none() {
                &self.welcome
            } else {
                &[]
            },
            &self.history[start],
            item_rows,
            next_visible_item,
            &self.workspace,
            self.session_id,
            width,
        );
        rows.append(&mut existing_rows);
        if !self.follow_history_tail {
            self.history_scroll = self
                .history_scroll
                .saturating_add(rows.len().saturating_sub(previous_rows));
        }
        if start == 0 {
            tracing::trace!(
                blocks = self.history.len(),
                rows = rows.len(),
                "on-demand transcript layout complete"
            );
        }
        self.history_layout.rendered = Some(WrappedLayout { width, rows });
    }

    /// Maintains an overscanned loaded prefix ahead of explicit upward
    /// navigation. Deferred blocks otherwise remain unformatted indefinitely.
    pub(crate) fn prepare_history_scroll_up(&mut self, amount: usize) {
        self.follow_history_tail = false;
        let threshold = self
            .transcript_viewport_height
            .saturating_mul(2)
            .saturating_add(amount);
        while self.history_layout_incomplete() && self.history_scroll <= threshold {
            self.materialize_older_history_layout();
        }
    }

    fn ensure_history_block_layout(&mut self, index: usize, width: u16) -> bool {
        let id = &self.history[index].id;
        if self
            .history_block_layouts
            .contains_key(&(id.clone(), width))
        {
            return false;
        }
        let item = &self.history[index];
        let started = Instant::now();
        let rows = transcript::history_item_rows_with_explorations(
            item,
            &self.workspace,
            width,
            self.ui_editor.enabled(),
            Some(id),
            &self.expanded_explorations,
        );
        let elapsed = started.elapsed();
        if elapsed.as_millis() >= 2 {
            tracing::trace!(
                block_id = ?id,
                block_kind = transcript_block_kind(item),
                rows = rows.len(),
                elapsed = ?elapsed,
                "slow transcript block layout"
            );
        }
        self.history_block_layouts.insert((id.clone(), width), rows);
        true
    }

    fn cached_history_block_rows(&self, index: usize, width: u16) -> usize {
        let id = &self.history[index].id;
        self.history_block_layouts
            .get(&(id.clone(), width))
            .expect("transcript block layout is cached")
            .len()
    }

    fn history_rows_from_cached_range(
        &self,
        start: usize,
        width: u16,
    ) -> Vec<crate::markdown::DisplayRow> {
        let item_rows = self.history[start..]
            .iter()
            .map(|block| {
                self.history_block_layouts
                    .get(&(block.id.clone(), width))
                    .expect("every materialized transcript block has a width-specific layout")
                    .as_slice()
            })
            .collect::<Vec<_>>();
        let at_beginning = start == 0 && self.transcript_older.is_none();
        let welcome = if at_beginning {
            self.welcome.as_slice()
        } else {
            &[]
        };
        transcript::history_rows_from_items_with_collapsed_activity(
            welcome,
            at_beginning.then_some(self.welcome_tip).flatten(),
            &self.history[start..],
            &item_rows,
            &self.workspace,
            self.session_id,
            width,
            self.collapse_tool_activity,
            &self.expanded_activity_runs,
        )
    }

    pub(super) fn transcript_block_offset(
        &mut self,
        target: &cagent_agent::protocol::TranscriptBlockId,
    ) -> Option<usize> {
        let target_index = self.history.iter().position(|block| &block.id == target)?;
        let width = self.render_width;
        self.ensure_history_layout(width);
        let item_rows = self.history[..=target_index]
            .iter()
            .map(|block| {
                self.history_block_layouts
                    .get(&(block.id.clone(), width))
                    .expect("materialized transcript block has a cached layout")
                    .as_slice()
            })
            .collect::<Vec<_>>();
        let target_rows = item_rows.last().map_or(0, |rows| rows.len());
        let rows = transcript::history_rows_from_items_with_collapsed_activity(
            if self.transcript_older.is_none() {
                &self.welcome
            } else {
                &[]
            },
            self.welcome_tip,
            &self.history[..=target_index],
            &item_rows,
            &self.workspace,
            self.session_id,
            width,
            self.collapse_tool_activity,
            &self.expanded_activity_runs,
        );
        Some(rows.len().saturating_sub(target_rows))
    }

    fn ensure_streaming_layout(&mut self, width: u16) {
        if self
            .streaming_layout
            .as_ref()
            .is_some_and(|layout| layout.width == width)
        {
            return;
        }
        let rows = transcript::streaming_rows(
            &self.streaming,
            self.streaming_status.as_ref(),
            width,
            &self.workspace,
            self.ui_editor.enabled(),
        );
        self.streaming_layout = Some(WrappedLayout { width, rows });
    }

    fn ensure_streaming_plan_layout(&mut self, width: u16) {
        if self
            .streaming_plan_layout
            .as_ref()
            .is_some_and(|layout| layout.width == width)
        {
            return;
        }
        let rows = if self.streaming_plan.is_empty() {
            Vec::new()
        } else {
            transcript::plan_markdown_rows_with_paths(
                &self.streaming_plan,
                &self.workspace,
                width,
                self.ui_editor.enabled(),
            )
        };
        self.streaming_plan_layout = Some(WrappedLayout { width, rows });
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn render(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        let cleared_image = self.clear_stale_image_before_draw()?;
        if cleared_image {
            // Commit a complete background frame before placing the new image.
            // A single Ratatui frame can only contain the final cell state, so
            // otherwise overlapping image cells skip the background repaint.
            terminal.draw(|frame| {
                drop(self.render_frame_with_image(frame, false));
            })?;
        }
        let mut hyperlink_overlays = Vec::new();
        terminal.draw(|frame| {
            hyperlink_overlays = self.render_frame(frame);
        })?;
        crate::markdown::render_hyperlink_overlays(&hyperlink_overlays)?;
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn render_frame(
        &mut self,
        frame: &mut ratatui::Frame<'_>,
    ) -> Vec<crate::markdown::HyperlinkOverlay> {
        self.render_frame_with_image(frame, true)
    }

    fn render_frame_with_image(
        &mut self,
        frame: &mut ratatui::Frame<'_>,
        render_image: bool,
    ) -> Vec<crate::markdown::HyperlinkOverlay> {
        let full_area = frame.area();
        self.transcript_fill_needed = false;
        let files_layout = self.files_pane_layout(full_area);
        self.render_height = full_area.height;
        self.working_tasks_hit = None;
        if files_layout.main.width == 0 {
            self.files_sidebar.focused = true;
            self.render_width = full_area.width.max(1);
            self.bash_output_hits.clear();
            self.exploration_toggle_hits.clear();
            self.web_fetch_output_hits.clear();
            self.mcp_call_hits.clear();
            self.compaction_hits.clear();
            self.web_search_result_hits.clear();
            self.agent_log_hits.clear();
            self.path_hits.clear();
            self.link_hits.clear();
            self.image_hits.clear();
            frame.render_widget(Clear, full_area);
            if let Some(sidebar) = files_layout.sidebar {
                self.render_files_sidebar(frame, sidebar);
            }
            self.theme.apply(frame.buffer_mut());
            return Vec::new();
        }
        let area = files_layout.main;
        let width = area.width.max(1);
        self.render_width = width;
        let (queue_height, popup_height, composer_height, status_height) =
            self.control_heights_within(width, area.height);
        let controls_height = queue_height
            .saturating_add(popup_height)
            .saturating_add(composer_height)
            .saturating_add(status_height)
            .saturating_add(sticky_rows_height(area.height, status_height));
        let [history_area, controls_area] = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(controls_height)])
            .areas(area);
        self.ensure_history_tail_layout(width, usize::from(history_area.height));
        self.ensure_streaming_layout(width);
        self.ensure_streaming_plan_layout(width);
        let history_rows = &self
            .history_layout
            .rendered
            .as_ref()
            .expect("history layout is initialized")
            .rows;
        let streaming_rows = &self
            .streaming_layout
            .as_ref()
            .expect("streaming layout is initialized")
            .rows;
        let streaming_plan_rows = &self
            .streaming_plan_layout
            .as_ref()
            .expect("streaming plan layout is initialized")
            .rows;
        let live_tool_rows = Vec::new();
        // Supervised work is represented by its originating tool card in the
        // primary transcript. The unified list and details live in the browser.
        let agent_run_rows = Vec::new();
        let task_count = self
            .supervised_work
            .running_agent_count()
            .saturating_add(self.supervised_work.running_terminal_count());
        let working_rows = if self.working_indicator_visible() {
            let waiting_for_user = self.pending_interaction.is_some();
            let waiting = self.waiting_for_work;
            let mut rows = wrap_log_line(
                &working_line(
                    self.working_started_at,
                    self.last_activity_at,
                    self.thinking,
                    waiting,
                    waiting_for_user,
                    self.compacting,
                    task_count,
                    self.reconnect_status.as_ref(),
                ),
                width,
            )
            .into_iter()
            .map(crate::markdown::DisplayRow::plain)
            .collect::<Vec<_>>();
            rows.push(crate::markdown::DisplayRow::plain(Line::default()));
            rows
        } else {
            Vec::new()
        };
        let active_plan_rows = if self.working_indicator_visible() {
            self.active_plan.as_ref().map_or_else(Vec::new, |plan| {
                active_plan_lines(plan, width)
                    .into_iter()
                    .map(crate::markdown::DisplayRow::plain)
                    .collect::<Vec<_>>()
            })
        } else {
            Vec::new()
        };
        let transcript_sections = [
            history_rows.as_slice(),
            streaming_rows.as_slice(),
            live_tool_rows.as_slice(),
            streaming_plan_rows.as_slice(),
            agent_run_rows.as_slice(),
            working_rows.as_slice(),
            active_plan_rows.as_slice(),
        ];
        let separator_before_streaming_assistant = !streaming_rows.is_empty()
            && self
                .history
                .last()
                .is_some_and(transcript::is_non_message_block);
        let transcript_height =
            transcript_sections_len(&transcript_sections, separator_before_streaming_assistant);
        let working_start = transcript_height.saturating_sub(working_rows.len());
        let content_height = transcript_height;
        // The welcome card is scrollable history, but enlarging the viewport
        // must not pull it back into view at the live tail. Once it has left
        // the screen, cap the viewport to the non-intro content and anchor
        // that viewport above the composer. Use this same area for scrolling,
        // drawing, and hit testing so upward navigation still reveals the card.
        let intro_rows = if self.history_layout.start_block == 0 && self.transcript_older.is_none()
        {
            let card_rows = wrap_log_lines(
                &welcome_card_rows(&self.welcome, &self.workspace, self.session_id, width),
                width,
            )
            .len();
            card_rows
                + usize::from(
                    card_rows > 0
                        && history_rows
                            .get(card_rows)
                            .is_some_and(|row| row.line.to_string().trim().is_empty()),
                )
        } else {
            0
        };
        let natural_max_scroll = content_height.saturating_sub(usize::from(history_area.height));
        let requested_scroll = if self.follow_history_tail || self.history_scroll == usize::MAX {
            natural_max_scroll
        } else {
            self.history_scroll.min(natural_max_scroll)
        };
        if history_area.height > 0
            && !self.welcome.is_empty()
            && (self.history_layout.start_block > 0
                || (intro_rows > 0 && requested_scroll >= intro_rows))
        {
            self.welcome_scrolled_past = true;
        }
        let mut log_area = history_area;
        let tail_rows = content_height.saturating_sub(intro_rows);
        self.transcript_fill_needed = history_area.height > 0
            && self.transcript_older.is_some()
            && tail_rows < usize::from(history_area.height);
        if self.welcome_scrolled_past
            // Even if all live content disappeared, Home/up must have a
            // nonempty viewport in which to reveal the retained welcome card.
            && (tail_rows > 0 || self.follow_history_tail || self.history_scroll == usize::MAX)
        {
            let height = usize::from(log_area.height).min(tail_rows) as u16;
            log_area.y = log_area.bottom().saturating_sub(height);
            log_area.height = height;
        }
        self.transcript_viewport_height = usize::from(log_area.height);
        let max_scroll = content_height.saturating_sub(self.transcript_viewport_height);
        if self.follow_history_tail {
            self.history_scroll = max_scroll;
        } else {
            let end_requested = self.history_scroll == usize::MAX;
            self.history_scroll = self.history_scroll.min(max_scroll);
            if end_requested {
                self.follow_history_tail = true;
            }
        }
        let transcript_offset = self.history_scroll;
        let mut visible_history = Vec::with_capacity(usize::from(log_area.height));
        if log_area.height > 0 {
            append_visible_transcript_sections(
                &transcript_sections,
                width,
                separator_before_streaming_assistant,
                transcript_offset,
                usize::from(log_area.height),
                &mut visible_history,
            );
        }
        self.bash_output_hits.clear();
        self.exploration_toggle_hits.clear();
        self.activity_collapse_hits.clear();
        self.web_fetch_output_hits.clear();
        self.mcp_call_hits.clear();
        self.compaction_hits.clear();
        self.web_search_result_hits.clear();
        self.agent_log_hits.clear();
        self.path_hits.clear();
        self.link_hits.clear();
        self.diff_hits.clear();
        self.image_hits.clear();
        if self.surfaces.is_empty() {
            if !self.compacting && task_count != 0 {
                let task_label = format!(
                    "{task_count} task{}",
                    if task_count == 1 { "" } else { "s" }
                );
                for (index, row) in visible_history.iter().enumerate() {
                    if transcript_offset.saturating_add(index) < working_start {
                        continue;
                    }
                    let rendered = row.line.to_string();
                    let Some(byte) = rendered.find(&task_label) else {
                        continue;
                    };
                    let start = log_area.x.saturating_add(
                        u16::try_from(rendered[..byte].width()).unwrap_or(u16::MAX),
                    );
                    let end = start
                        .saturating_add(u16::try_from(task_label.width()).unwrap_or(u16::MAX))
                        .min(log_area.right());
                    if start < end {
                        self.working_tasks_hit = Some(crate::app::WorkingTasksHit {
                            row: log_area
                                .y
                                .saturating_add(u16::try_from(index).unwrap_or(u16::MAX)),
                            columns: start..end,
                        });
                    }
                }
            }
            for (index, row) in visible_history.iter().enumerate() {
                if let Some(target) = row.activity_collapse() {
                    self.activity_collapse_hits
                        .push(crate::app::ActivityCollapseHit {
                            row: log_area
                                .y
                                .saturating_add(u16::try_from(index).unwrap_or(u16::MAX)),
                            target: target.clone(),
                        });
                }
                if let Some(target) = row.exploration_toggle() {
                    self.exploration_toggle_hits
                        .push(crate::app::ExplorationToggleHit {
                            row: log_area
                                .y
                                .saturating_add(u16::try_from(index).unwrap_or(u16::MAX)),
                            target: target.clone(),
                        });
                }
            }
            for (index, row) in visible_history.iter().enumerate() {
                if let Some(target) = row.diff_line() {
                    self.diff_hits.push(crate::app::DiffHit {
                        row: log_area
                            .y
                            .saturating_add(u16::try_from(index).unwrap_or(u16::MAX)),
                        target: target.clone(),
                    });
                }
            }
            if self.ui_editor.enabled()
                || visible_history
                    .iter()
                    .any(|row| row.paths().iter().any(|segment| segment.target.directory))
            {
                for (index, row) in visible_history.iter().enumerate() {
                    append_display_row_path_hits(
                        &mut self.path_hits,
                        row,
                        log_area
                            .y
                            .saturating_add(u16::try_from(index).unwrap_or(u16::MAX)),
                        log_area.x,
                        log_area.width,
                    );
                    let screen_row = log_area
                        .y
                        .saturating_add(u16::try_from(index).unwrap_or(u16::MAX));
                    for segment in row.images() {
                        let start = log_area
                            .x
                            .saturating_add(u16::try_from(segment.column).unwrap_or(u16::MAX));
                        let end = start
                            .saturating_add(u16::try_from(segment.width).unwrap_or(u16::MAX))
                            .min(log_area.x.saturating_add(log_area.width));
                        if start < end {
                            self.image_hits.push(crate::app::ImageHit {
                                row: screen_row,
                                columns: start..end,
                                image: segment.image.clone(),
                            });
                        }
                    }
                }
            }
            let candidates = bash_output_candidates(self);
            for candidate in &candidates {
                let command_marker = format!(
                    "{} {}",
                    if candidate.status == cagent_agent::presentation::ToolActivityStatus::Pending {
                        "Running"
                    } else {
                        "Ran"
                    },
                    candidate.command,
                );
                let Some(command_row) = visible_history
                    .iter()
                    .position(|row| row.line.to_string().contains(&command_marker))
                else {
                    continue;
                };
                let preview_rows = crate::render::activity::output_preview_row_count(
                    &candidate.output,
                    width.saturating_sub(5),
                );
                for index in bash_hit_rows(command_row, preview_rows, visible_history.len()) {
                    self.bash_output_hits.push(crate::app::BashOutputHit {
                        row: log_area
                            .y
                            .saturating_add(u16::try_from(index).unwrap_or(u16::MAX)),
                        terminal_id: candidate.terminal_id,
                        command: candidate.command.clone(),
                        output: candidate.output.clone(),
                        ansi_output: candidate.ansi_output.clone(),
                        completion: candidate.completion.clone(),
                    });
                }
            }
            for (index, row) in visible_history.iter().enumerate() {
                let Some(terminal_id) = row.terminal_output() else {
                    continue;
                };
                let Some(candidate) = candidates
                    .iter()
                    .find(|candidate| candidate.terminal_id == Some(terminal_id))
                else {
                    continue;
                };
                self.bash_output_hits.push(crate::app::BashOutputHit {
                    row: log_area
                        .y
                        .saturating_add(u16::try_from(index).unwrap_or(u16::MAX)),
                    terminal_id: candidate.terminal_id,
                    command: candidate.command.clone(),
                    output: candidate.output.clone(),
                    ansi_output: candidate.ansi_output.clone(),
                    completion: candidate.completion.clone(),
                });
            }
            for (index, row) in visible_history.iter().enumerate() {
                let Some(target) = row.compaction() else {
                    continue;
                };
                self.compaction_hits.push(crate::app::CompactionHit {
                    row: log_area
                        .y
                        .saturating_add(u16::try_from(index).unwrap_or(u16::MAX)),
                    summary: target.summary.to_string(),
                });
            }
            for (index, row) in visible_history.iter().enumerate() {
                let Some(target) = row.web_fetch_output() else {
                    continue;
                };
                self.web_fetch_output_hits
                    .push(crate::app::WebFetchOutputHit {
                        row: log_area
                            .y
                            .saturating_add(u16::try_from(index).unwrap_or(u16::MAX)),
                        url: target.url.clone(),
                        redirected_url: target.redirected_url.clone(),
                        format: target.format,
                        output: target.output.to_string(),
                    });
            }
            for (index, row) in visible_history.iter().enumerate() {
                if let Some(call) = row.mcp_call() {
                    self.mcp_call_hits.push((
                        log_area
                            .y
                            .saturating_add(u16::try_from(index).unwrap_or(u16::MAX)),
                        std::sync::Arc::clone(call),
                    ));
                }
            }
            let mut search_start = 0;
            for (provider, query, results) in web_search_result_candidates(self) {
                let marker = format!("Web search {provider}");
                let Some(heading_row) = visible_history
                    .iter()
                    .enumerate()
                    .skip(search_start)
                    .find_map(|(index, row)| {
                        row.line.to_string().contains(&marker).then_some(index)
                    })
                else {
                    continue;
                };
                search_start = heading_row.saturating_add(1);
                let query_rows = crate::render::wrap_ranges(
                    &crate::render::terminal_safe(&query),
                    width.saturating_sub(6),
                )
                .len();
                for index in heading_row.saturating_add(1)
                    ..heading_row.saturating_add(1).saturating_add(query_rows)
                {
                    if index >= visible_history.len() {
                        break;
                    }
                    self.web_search_result_hits
                        .push(crate::app::WebSearchResultHit {
                            row: log_area
                                .y
                                .saturating_add(u16::try_from(index).unwrap_or(u16::MAX)),
                            provider: provider.clone(),
                            query: query.clone(),
                            results: results.clone(),
                        });
                }
            }
            let visible_rows = visible_history.iter().enumerate().fold(
                std::collections::HashMap::<String, Vec<usize>>::new(),
                |mut rows, (index, row)| {
                    rows.entry(row.line.to_string()).or_default().push(index);
                    rows
                },
            );
            for work in &self.supervised_work.rows {
                let cagent_agent::presentation::SupervisedWork::Agent { run } = work else {
                    continue;
                };
                let task_rows = wrap_log_line(
                    &Line::from(format!("  └ {}", terminal_safe(&run.task))),
                    width,
                )
                .into_iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>();
                for task_row in task_rows {
                    let Some(indexes) = visible_rows.get(&task_row) else {
                        continue;
                    };
                    self.agent_log_hits.extend(indexes.iter().map(|index| {
                        crate::app::AgentLogHit {
                            row: log_area
                                .y
                                .saturating_add(u16::try_from(*index).unwrap_or(u16::MAX)),
                            run_id: run.id,
                        }
                    }));
                }
            }
        }
        let draft_limit = match self.surfaces.last() {
            // Permission panels reserve the final composer row as a blank
            // buffer below the approval choices.  Their scroll metrics use
            // the same reduced viewport; without this, a long preview fills
            // that row and leaves Deny directly above the status line.
            Some(crate::app::Surface::Permission {
                editing_note: false,
                ..
            }) => composer_height.saturating_sub(1),
            Some(_) => composer_height,
            None => composer_height.saturating_sub(1),
        };
        let composer_viewport = self
            .surfaces
            .is_empty()
            .then(|| self.composer_viewport_layout(width, usize::from(draft_limit)));
        if let Some(viewport) = &composer_viewport {
            self.composer_scroll = viewport.state;
        }
        let (queue, popup, composer, status) = if let Some(viewport) = &composer_viewport {
            (
                self.queue_lines(width),
                self.completion_lines(width),
                viewport.scroll.lines.iter().skip(1).cloned().collect(),
                self.status_line(width),
            )
        } else {
            self.control_lines_with_draft_limit(width, usize::from(draft_limit))
        };
        let composer_top = composer_viewport
            .as_ref()
            .map(|viewport| viewport.scroll.lines.first().cloned().unwrap_or_default());
        let has_queue = !queue.is_empty();
        let mut hyperlink_overlays =
            crate::markdown::hyperlink_overlays(&visible_history, log_area);
        let [
            scroll_indicator_area,
            queue_area,
            popup_area,
            scroll_padding_area,
            composer_area,
            status_area,
        ] = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(scroll_indicator_height(area.height, status_height)),
                Constraint::Length(queue_height),
                Constraint::Length(popup_height),
                Constraint::Length(scroll_padding_height(area.height, status_height)),
                Constraint::Length(composer_height),
                Constraint::Length(status_height),
            ])
            .areas(controls_area);
        if self.surfaces.is_empty() && self.ui_editor.enabled() {
            for (line_index, line) in composer.iter().enumerate() {
                let rendered = line.to_string();
                for chip in &self.attachments {
                    let label = attachment_chip_label(chip);
                    let mut start = 0;
                    while let Some(relative) = rendered[start..].find(&label) {
                        let byte = start + relative;
                        let column = rendered[..byte].width();
                        let screen_column = composer_area
                            .x
                            .saturating_add(u16::try_from(column).unwrap_or(u16::MAX));
                        let end = screen_column
                            .saturating_add(u16::try_from(label.width()).unwrap_or(u16::MAX))
                            .min(composer_area.right());
                        if screen_column < end {
                            self.path_hits.push(crate::app::PathHit {
                                row: composer_area
                                    .y
                                    .saturating_add(u16::try_from(line_index).unwrap_or(u16::MAX)),
                                columns: screen_column..end,
                                path: cagent_agent::presentation::resolve_display_path(
                                    &chip.spec.path.to_string_lossy(),
                                    &self.workspace,
                                ),
                                line: None,
                                directory: false,
                            });
                        }
                        start = byte + label.len();
                    }
                }
                for chip in &self.images {
                    append_image_hit(
                        &mut self.image_hits,
                        &rendered,
                        composer_area
                            .y
                            .saturating_add(u16::try_from(line_index).unwrap_or(u16::MAX)),
                        composer_area.x,
                        composer_area.width,
                        &chip.image,
                    );
                }
            }
        }
        if let Some(Surface::Expanded {
            view,
            scroll,
            viewport_rows,
        }) = self.surfaces.last()
            && !matches!(view, ExpandedView::Terminal { .. })
        {
            for (relative, row) in crate::app::surfaces::cached_expanded_visible_path_rows(
                &self.expanded_text_render_cache,
                view,
                *scroll,
                *viewport_rows,
                &self.workspace,
                width,
                usize::from(composer_height),
            ) {
                let screen_row = composer_area
                    .y
                    .saturating_add(u16::try_from(relative).unwrap_or(u16::MAX));
                if screen_row >= composer_area.bottom() {
                    continue;
                }
                if self.ui_editor.enabled() {
                    append_display_row_path_hits(
                        &mut self.path_hits,
                        &row,
                        screen_row,
                        composer_area.x,
                        composer_area.width,
                    );
                }
                hyperlink_overlays.extend(crate::markdown::hyperlink_overlays(
                    std::slice::from_ref(&row),
                    Rect::new(composer_area.x, screen_row, composer_area.width, 1),
                ));
            }
        }
        if let Some(Surface::Expanded {
            view,
            scroll,
            viewport_rows,
        }) = self.surfaces.last()
            && matches!(view, ExpandedView::AgentLog { .. })
        {
            for (relative, row) in crate::app::surfaces::cached_expanded_visible_path_rows(
                &self.expanded_text_render_cache,
                view,
                *scroll,
                *viewport_rows,
                &self.workspace,
                width,
                usize::from(composer_height),
            ) {
                if let Some(target) = row.exploration_toggle() {
                    self.exploration_toggle_hits
                        .push(crate::app::ExplorationToggleHit {
                            row: composer_area
                                .y
                                .saturating_add(u16::try_from(relative).unwrap_or(u16::MAX)),
                            target: target.clone(),
                        });
                }
                if let Some(call) = row.mcp_call() {
                    self.mcp_call_hits.push((
                        composer_area
                            .y
                            .saturating_add(u16::try_from(relative).unwrap_or(u16::MAX)),
                        std::sync::Arc::clone(call),
                    ));
                }
                if let Some(target) = row.activity_collapse() {
                    self.activity_collapse_hits
                        .push(crate::app::ActivityCollapseHit {
                            row: composer_area
                                .y
                                .saturating_add(u16::try_from(relative).unwrap_or(u16::MAX)),
                            target: target.clone(),
                        });
                }
            }
        }
        let cursor = composer_viewport
            .as_ref()
            .and_then(|viewport| viewport.cursor_position(composer_area));
        let terminal_area = expanded_output_area(composer_area);
        let expanded_terminal_max = self.prepare_expanded_terminal_cache(terminal_area);
        if render_image {
            self.prepare_expanded_image_cache(terminal_area);
        }
        let expanded_terminal = expanded_terminal_max.and_then(|maximum| {
            self.terminal_render_cache
                .as_ref()
                .map(|cache| (cache.screen(), maximum, cache.screen().scrollback() > 0))
        });
        let expanded_image = if render_image {
            self.image_preview
                .as_ref()
                .and_then(|preview| preview.protocol.as_ref().map(|(_, protocol)| protocol))
        } else {
            None
        };
        if let Some(crate::app::Surface::ProviderSetup {
            auth_challenge:
                Some(cagent_agent::provider::AuthChallenge::Device {
                    verification_url, ..
                }),
            authenticated: false,
            ..
        }) = self.surfaces.last()
        {
            let x = composer_area.x.saturating_add(7);
            let y = composer_area.y.saturating_add(2);
            if let Some(link) = crate::markdown::external_hyperlink_overlay(
                x,
                y,
                verification_url,
                verification_url,
                composer_area.width.saturating_sub(7),
                Some(236),
            ) {
                hyperlink_overlays.push(link);
            }
        }
        if let Some(crate::app::Surface::McpOAuth { attempt, .. }) = self.surfaces.last()
            && !matches!(
                &*attempt.completion.borrow(),
                cagent_agent::mcp::McpOAuthCompletion::Connected
            )
        {
            let (prefix_width, url) = match &attempt.prompt {
                cagent_agent::mcp::McpOAuthPrompt::Device {
                    verification_url, ..
                } => (7, verification_url.as_str()),
                cagent_agent::mcp::McpOAuthPrompt::Browser {
                    authorization_url, ..
                } => (29, authorization_url.as_str()),
            };
            if let Some(link) = crate::markdown::external_hyperlink_overlay(
                composer_area.x.saturating_add(prefix_width),
                composer_area.y.saturating_add(2),
                url,
                url,
                composer_area.width.saturating_sub(prefix_width),
                Some(236),
            ) {
                hyperlink_overlays.push(link);
            }
        }
        if let Some(crate::app::Surface::McpPackageValueEdit { draft, field, .. }) =
            self.surfaces.last()
        {
            let description = match field {
                crate::app::McpPackageField::Parameter(id) => draft
                    .package
                    .parameters
                    .get(id)
                    .and_then(|parameter| parameter.description.as_deref()),
                crate::app::McpPackageField::Secret(id) => draft
                    .package
                    .secrets
                    .get(id)
                    .and_then(|secret| secret.description.as_deref()),
            };
            if let Some((description, (start, url))) = description.and_then(|description| {
                crate::app::surfaces::external_url_in_text(description)
                    .map(|url| (description, url))
            }) {
                let prefix_width = 2u16.saturating_add(
                    u16::try_from(description[..start].width()).unwrap_or(u16::MAX),
                );
                if let Some(link) = crate::markdown::external_hyperlink_overlay(
                    composer_area.x.saturating_add(prefix_width),
                    composer_area.y.saturating_add(3),
                    url,
                    url,
                    composer_area.width.saturating_sub(prefix_width),
                    Some(236),
                ) {
                    hyperlink_overlays.push(link);
                }
            }
        }
        frame.render_widget(Clear, frame.area());
        if log_area.height > 0 {
            frame.render_widget(
                Paragraph::new(Text::from(
                    visible_history
                        .iter()
                        .map(|row| row.line.clone())
                        .collect::<Vec<_>>(),
                )),
                log_area,
            );
            // Overlay the transcript scrollbar so markdown keeps the same
            // wrapping width. Its range deliberately describes only the
            // loaded window: older pages adjust both the content length and
            // the anchor-preserving top-origin offset when they are prepended.
            self.transcript_scrollbar = (!self.follow_history_tail)
                .then(|| {
                    crate::app::TranscriptScrollbarLayout::new(
                        log_area,
                        content_height,
                        transcript_offset,
                    )
                })
                .flatten();
            if let Some(scrollbar) = self.transcript_scrollbar {
                let x = scrollbar.area.right().saturating_sub(1);
                for row in 0..usize::from(scrollbar.area.height) {
                    let thumb = row >= scrollbar.thumb_start
                        && row < scrollbar.thumb_start.saturating_add(scrollbar.thumb_len);
                    let line = if row == 0
                        && (self.transcript_older.is_some() || self.history_layout_incomplete())
                    {
                        Line::styled("↑", DIM_STYLE.fg(Color::Reset))
                    } else if thumb {
                        Line::styled("█", Style::default().fg(Color::Reset))
                    } else {
                        Line::styled("│", DIM_STYLE.fg(Color::Reset))
                    };
                    frame.render_widget(
                        Paragraph::new(line),
                        Rect::new(
                            x,
                            scrollbar
                                .area
                                .y
                                .saturating_add(u16::try_from(row).unwrap_or(u16::MAX)),
                            1,
                            1,
                        ),
                    );
                }
            }
        } else {
            self.transcript_scrollbar = None;
        }
        if !queue.is_empty() {
            frame.render_widget(Paragraph::new(Text::from(queue)), queue_area);
        }
        if !popup.is_empty() {
            frame.render_widget(
                Paragraph::new(Text::from(popup)).wrap(Wrap { trim: false }),
                popup_area,
            );
        }
        frame.render_widget(
            Paragraph::new(scroll_indicator_line(
                self.history_scroll < max_scroll,
                width,
                has_queue,
                matches!(self.surfaces.last(), Some(Surface::Expanded { .. })),
            )),
            scroll_indicator_area,
        );
        frame.render_widget(
            Paragraph::new(composer_top.unwrap_or_default()).style(COMPOSER_STYLE),
            scroll_padding_area,
        );
        frame.render_widget(Block::default().style(COMPOSER_STYLE), composer_area);
        if let Some((screen, _maximum, has_content_below)) = expanded_terminal {
            frame.render_widget(
                Paragraph::new(Text::from(composer.into_iter().take(2).collect::<Vec<_>>()))
                    .style(COMPOSER_STYLE),
                composer_area,
            );
            frame.render_widget(
                terminal::TerminalView::new(screen, COMPOSER_STYLE.bg.unwrap_or(Color::Reset)),
                terminal_area,
            );
            if has_content_below {
                let arrow_area = Rect::new(
                    composer_area.x,
                    composer_area.bottom().saturating_sub(1),
                    composer_area.width,
                    1,
                );
                frame.render_widget(
                    Paragraph::new(Line::from("  ↓").style(DIM_STYLE)).style(COMPOSER_STYLE),
                    arrow_area,
                );
            }
        } else {
            frame.render_widget(
                Paragraph::new(Text::from(composer))
                    .style(COMPOSER_STYLE)
                    .wrap(Wrap { trim: false }),
                composer_area,
            );
            if let Some(image) = expanded_image {
                let image_area = image_render_area(terminal_area, image);
                frame.render_widget(
                    ratatui_image::Image::new(image).allow_clipping(true),
                    image_area,
                );
                if let Some(preview) = self.image_preview.as_ref()
                    && let Some((_, protocol)) = preview.protocol.as_ref()
                {
                    let protocol_kind = Self::image_protocol(protocol);
                    let image_area = self
                        .rendered_image
                        .as_ref()
                        .filter(|previous| {
                            self.files_sidebar.dragging
                                && previous.key == preview.key
                                && previous.protocol == protocol_kind
                        })
                        .map_or(image_area, |previous| {
                            let left = previous.area.x.min(image_area.x);
                            let top = previous.area.y.min(image_area.y);
                            let right = previous.area.right().max(image_area.right());
                            let bottom = previous.area.bottom().max(image_area.bottom());
                            Rect::new(left, top, right - left, bottom - top)
                        });
                    self.rendered_image = Some(crate::app::RenderedImage {
                        key: preview.key.clone(),
                        area: image_area,
                        protocol: protocol_kind,
                    });
                }
            }
        }
        frame.render_widget(Paragraph::new(status), status_area);
        if let Some(position) =
            cursor.filter(|_| !self.files_sidebar.focused || self.files_sidebar_locked())
        {
            frame.set_cursor_position(position);
        }
        if let Some(sidebar) = files_layout.sidebar {
            self.render_files_sidebar(frame, sidebar);
            if std::mem::take(&mut self.files_sidebar.redraw_after_image_update) {
                // Kitty placeholders from the cached drag frame can cross the
                // new divider, and their first placement can disturb the
                // terminal cursor. Emit every sidebar cell so the physical
                // terminal is restored even when Ratatui sees no logical cell
                // changes.
                Self::force_regular_cell_redraw(frame, sidebar);
            }
        } else {
            self.files_sidebar.area = None;
            self.files_sidebar.divider_column = None;
            self.files_sidebar.redraw_after_image_update = false;
            self.files_sidebar.row_hits.clear();
        }
        self.force_image_cleanup_redraw(frame);
        self.theme.apply(frame.buffer_mut());
        if self.open_links {
            self.link_hits = hyperlink_overlays.clone();
        }
        hyperlink_overlays
    }

    fn render_files_sidebar(&mut self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        // Keep the sidebar visually separate from the terminal edges. The
        // cleared rows remain the terminal's default background rather than
        // part of the composer-colored sidebar surface.
        frame.render_widget(Clear, area);
        let surface_area = Rect::new(
            area.x,
            area.y.saturating_add(1),
            area.width,
            area.height.saturating_sub(2),
        );
        self.files_sidebar.area = Some(surface_area);
        self.files_sidebar.row_hits.clear();
        if area.width == 0 || area.height == 0 {
            self.files_sidebar.divider_column = None;
            return;
        }
        let divider = area.right().saturating_sub(1);
        self.files_sidebar.divider_column = Some(divider);
        if surface_area.width > 0 && surface_area.height > 0 {
            self.render_files_sidebar_surface(frame, surface_area);
        }
        let divider_style = if self.files_sidebar.dragging || self.files_sidebar.focused {
            Style::new().fg(Color::Cyan)
        } else {
            DIM_STYLE
        };
        let divider_area = Rect::new(divider, area.y, 1, area.height);
        frame.render_widget(Clear, divider_area);
        frame.render_widget(
            Paragraph::new(Text::from(
                (0..area.height)
                    .map(|_| Line::from("│").style(divider_style))
                    .collect::<Vec<_>>(),
            )),
            divider_area,
        );
    }

    fn render_files_sidebar_surface(&mut self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let content_area = Rect::new(area.x, area.y, area.width.saturating_sub(1), area.height);
        let content_width = content_area.width;
        let mut lines = Vec::new();
        lines.push(Line::default());
        lines.push(truncate_line(
            Line::from(vec![
                Span::raw("  "),
                Span::styled("Files", MENU_TITLE_STYLE),
                Span::raw("  "),
                Span::styled(
                    terminal_safe(&workspace_display_path(&self.workspace)),
                    DIM_STYLE,
                ),
            ]),
            usize::from(content_width),
        ));

        if let Some(tree) = &mut self.files_sidebar.tree {
            let capacity = usize::from(area.height.saturating_sub(4)).max(1);
            tree.set_viewport_rows(capacity);
            let rows = crate::app::file_tree::styled_rows(
                &tree.browser.tree,
                tree.selected_index(),
                content_width,
            );
            let tree_rows = tree.browser.tree.rows();
            lines.push(if tree.scroll > 0 {
                Line::from("  ↑").style(DIM_STYLE)
            } else {
                Line::default()
            });
            if let Some(error) = tree
                .browser
                .tree
                .unavailable()
                .or(tree.unavailable.as_deref())
            {
                lines.push(truncate_line(
                    Line::from(Span::styled(
                        format!("  Directory unavailable · {error}"),
                        ERROR_STYLE,
                    )),
                    usize::from(content_width),
                ));
            } else {
                for (visible, row) in rows.iter().skip(tree.scroll).take(capacity).enumerate() {
                    let index = tree.scroll + visible;
                    if tree_rows.get(index).is_some_and(|row| row.is_selectable()) {
                        self.files_sidebar.row_hits.push((
                            area.y
                                .saturating_add(3)
                                .saturating_add(u16::try_from(visible).unwrap_or(u16::MAX)),
                            index,
                        ));
                    }
                    lines.push(row.line.clone());
                }
            }
            while lines.len() < usize::from(area.height.saturating_sub(1)) {
                lines.push(Line::default());
            }
            lines.push(if tree.scroll.saturating_add(capacity) < rows.len() {
                Line::from("  ↓").style(DIM_STYLE)
            } else {
                Line::default()
            });
        }
        frame.render_widget(Block::default().style(COMPOSER_STYLE), area);
        frame.render_widget(
            Paragraph::new(Text::from(lines)).style(COMPOSER_STYLE),
            content_area,
        );
    }

    pub(super) fn working_indicator_visible(&self) -> bool {
        self.active && !self.hide_working_indicator
    }

    pub(super) fn bash_activity_animation_visible(&self) -> bool {
        self.supervised_work.rows.iter().any(|work| {
            matches!(
                work,
                cagent_agent::presentation::SupervisedWork::Terminal { terminal }
                    if terminal.status.is_active()
            )
        }) || self
            .history
            .iter()
            .any(|block| transcript_block_has_pending_activity(block))
            || self.surfaces.iter().any(|surface| {
                matches!(
                    surface,
                    Surface::Expanded {
                        view: ExpandedView::AgentLog { run, terminals, .. },
                        ..
                    } if !run.status.is_terminal()
                        || terminals.iter().any(|terminal| terminal.status.is_active())
                )
            })
    }

    pub(super) fn invalidate_activity_layout(&mut self) {
        // A clock tick must not invalidate a delegated log's static body.
        // The header clock is refreshed separately by the expanded renderer.
        let pending_activity_ids = self
            .history
            .iter()
            .filter(|block| transcript_block_has_pending_activity(block))
            .map(|block| &block.id)
            .collect::<std::collections::HashSet<_>>();
        if pending_activity_ids.is_empty() || !self.pending_activity_intersects_viewport() {
            return;
        }
        self.history_block_layouts
            .retain(|(id, _), _| !pending_activity_ids.contains(id));
        // Preserve the tail-first materialized prefix. The next render
        // recomposes only that range from the surviving block caches.
        self.history_layout.rendered = None;
    }

    fn pending_activity_intersects_viewport(&self) -> bool {
        // Pending transcript activity is appended at the live tail. Once the
        // user leaves tail-follow mode its animation must not churn the static
        // viewport (or the increasingly materialized prefix above it).
        self.follow_history_tail
    }

    pub(super) fn supervised_work_surface_open(&self) -> bool {
        self.surfaces
            .iter()
            .any(|surface| matches!(surface, Surface::SupervisedWork { .. }))
    }

    pub(super) fn controls_height(&self, width: u16, terminal_height: u16) -> u16 {
        let (queue, popup, composer, status) = self.control_heights_within(width, terminal_height);
        queue
            .saturating_add(popup)
            .saturating_add(composer)
            .saturating_add(status)
            .saturating_add(sticky_rows_height(terminal_height, status))
    }

    pub(super) fn control_heights_within(
        &self,
        width: u16,
        terminal_height: u16,
    ) -> (u16, u16, u16, u16) {
        let (queue, popup, composer, status) = self.control_heights(width);
        if terminal_height <= status {
            return (0, 0, 0, terminal_height);
        }

        // Reserve the status row, the scroll indicator row, and, when
        // possible, the composer's blank rows above and below its input.
        // Completion and queue content is cropped before it can push the
        // composer past the screen edge.
        let sticky_rows = sticky_rows_height(terminal_height, status);
        let minimum_composer = terminal_height.saturating_sub(status + sticky_rows).min(2);
        let remaining = terminal_height.saturating_sub(status + sticky_rows + minimum_composer);
        let queue = queue.min(remaining);
        let popup = popup.min(remaining.saturating_sub(queue));
        let available_composer = terminal_height
            .saturating_sub(status)
            .saturating_sub(sticky_rows)
            .saturating_sub(queue)
            .saturating_sub(popup);
        let available_composer = if matches!(self.surfaces.last(), Some(Surface::Permission { .. }))
        {
            available_composer.saturating_sub(permission_history_reserve(available_composer))
        } else {
            available_composer
        };
        let composer = composer.min(available_composer);
        (queue, popup, composer, status)
    }

    pub(super) fn control_heights(&self, width: u16) -> (u16, u16, u16, u16) {
        if !self.history_preview_showing()
            && let Some(surface) = self.surfaces.last()
        {
            if matches!(
                surface,
                crate::app::Surface::HistoryTree { .. }
                    | crate::app::Surface::Conversations { .. }
                    | crate::app::Surface::Expanded { .. }
            ) {
                return (0, 0, u16::MAX, 1);
            }
            if matches!(surface, crate::app::Surface::Permission { .. }) {
                let mut panel = u16::try_from(cached_permission_surface_line_count(
                    &self.permission_diff_render_cache,
                    surface,
                    &self.workspace,
                    width,
                ))
                .unwrap_or(u16::MAX);
                if !matches!(
                    surface,
                    crate::app::Surface::Permission {
                        editing_note: true,
                        ..
                    }
                ) {
                    panel = panel.saturating_add(1);
                }
                let panel = panel.max(1);
                return (0, 0, panel, 1);
            }
            let panel = u16::try_from(surface_lines(surface, &self.workspace, width).len())
                .unwrap_or(u16::MAX);
            return (0, 0, panel.max(1), 1);
        }

        let queue = u16::try_from(self.queue_lines(width).len()).unwrap_or(u16::MAX);
        let popup = u16::try_from(self.completion_lines(width).len()).unwrap_or(u16::MAX);
        let draft_rows = if self.is_read_only_view() && !self.observer_command_active() {
            1
        } else {
            visual_text_rows(&self.rendered_draft(), width.saturating_sub(4))
        };
        let draft_rows = self.composer_max_rows.map_or(draft_rows, |maximum| {
            draft_rows.min(u16::try_from(maximum).unwrap_or(u16::MAX))
        });
        // The scroll indicator owns the row above the composer. Keep one
        // empty row below the input, and give the status line its own row.
        (queue, popup, draft_rows.saturating_add(1), 1)
    }

    pub(super) fn control_lines_with_draft_limit(
        &self,
        width: u16,
        draft_limit: usize,
    ) -> (
        Vec<Line<'static>>,
        Vec<Line<'static>>,
        Vec<Line<'static>>,
        Line<'static>,
    ) {
        if !self.history_preview_showing()
            && let Some(surface) = self.surfaces.last()
        {
            let mut panel = Vec::new();
            let mut surface_rows = match surface {
                Surface::Expanded {
                    view,
                    scroll,
                    viewport_rows,
                } if !matches!(view, crate::app::ExpandedView::Terminal { .. }) => {
                    cached_expanded_surface_lines(
                        &self.expanded_text_render_cache,
                        view,
                        *scroll,
                        *viewport_rows,
                        &self.workspace,
                        width,
                        draft_limit,
                        self.files_sidebar.dragging,
                    )
                }
                Surface::Permission { .. } => cached_permission_surface_lines_with_viewport(
                    &self.permission_diff_render_cache,
                    surface,
                    &self.workspace,
                    width,
                    draft_limit,
                ),
                _ => surface_lines_with_viewport(surface, &self.workspace, width, draft_limit),
            };
            if matches!(surface, crate::app::Surface::Conversations { .. }) {
                surface_rows.resize_with(draft_limit, Line::default);
            }
            panel.extend(surface_rows);
            let hints = self.contextual_key_hints(&surface_status(surface));
            let status = self.active_notice().map_or_else(
                || {
                    if matches!(surface, Surface::Onboarding) {
                        keybinding_status_line_with_disabled_key(&hints, "Esc")
                    } else if let Surface::HistoryTree {
                        rows, list, query, ..
                    } = surface
                        && list
                            .selected
                            .and_then(|position| {
                                cagent_agent::presentation::history_picker_indexes(rows, query)
                                    .get(position)
                                    .copied()
                            })
                            .and_then(|source| rows.get(source))
                            .is_none_or(|row| row.transcript_target.is_none())
                    {
                        let key = self
                            .key_bindings
                            .label(cagent_agent::presentation::KeyBindingAction::ScrollToMessage);
                        keybinding_status_line_with_disabled_key(&hints, &key)
                    } else if let Some(disabled_keys) = statusline_disabled_keys(surface) {
                        keybinding_status_line_with_disabled_keys(&hints, disabled_keys)
                    } else {
                        keybinding_status_line(&hints)
                    }
                },
                |notice| padded_status_line(Line::from(notice.to_owned()).style(DIM_STYLE)),
            );
            return (Vec::new(), Vec::new(), panel, status);
        }

        let queue = self.queue_lines(width);
        let popup = self.completion_lines(width);
        let composer = self
            .composer_viewport_layout(width, draft_limit)
            .scroll
            .lines
            .into_iter()
            .skip(1)
            .collect();
        (queue, popup, composer, self.status_line(width))
    }

    pub(crate) fn composer_viewport_layout(
        &self,
        width: u16,
        capacity: usize,
    ) -> ComposerViewportLayout {
        let substitutions = self.display_substitutions();
        let rendered = self.rendered_draft_with_substitutions(&substitutions);
        let ranges = wrap_ranges(&rendered, width.saturating_sub(4));
        let passive_observer = self.is_read_only_view() && !self.observer_command_active();
        let content_rows = if passive_observer { 1 } else { ranges.len() };
        let caret = if passive_observer {
            None
        } else {
            rendered_cursor_line(
                &rendered,
                &ranges,
                self.rendered_cursor_with_substitutions(&substitutions),
            )
        };
        let cursor_row = caret.map_or(0, |(row, _)| row);
        let mut state = self.composer_scroll;
        state.reconcile_focus(content_rows, cursor_row, capacity);
        let command_end = self.valid_slash_command_end();
        let scroll = ScrollViewWidget::new(capacity).render(&state, |row| {
            if passive_observer {
                return Line::from(vec![
                    Span::styled("› ", DIM_STYLE),
                    Span::styled(READ_ONLY_COMPOSER_PLACEHOLDER, DIM_STYLE),
                ]);
            }
            if rendered.is_empty() {
                return Line::from(vec![
                    Span::styled(
                        if self.composer_mode == crate::app::ComposerMode::Bash {
                            "! "
                        } else {
                            "› "
                        },
                        if self.composer_mode == crate::app::ComposerMode::Bash {
                            BASH_STYLE
                        } else {
                            ACCENT_STYLE
                        },
                    ),
                    Span::styled(COMPOSER_PLACEHOLDER, DIM_STYLE),
                ]);
            }
            let (start, end) = ranges[row];
            let mut line = Line::from(vec![Span::styled(
                if row == state.offset {
                    if self.composer_mode == crate::app::ComposerMode::Bash {
                        "! "
                    } else {
                        "› "
                    }
                } else {
                    "  "
                },
                if self.composer_mode == crate::app::ComposerMode::Bash {
                    BASH_STYLE
                } else {
                    ACCENT_STYLE
                },
            )])
            .style(COMPOSER_STYLE);
            line.spans.extend(styled_draft_slice(
                &rendered[start..end],
                &substitutions,
                start,
                command_end,
            ));
            line
        });
        let source_len = if self.observer_command_active() {
            self.observer_command_text().map_or(0, str::len)
        } else {
            self.draft.len()
        };
        ComposerViewportLayout {
            rendered,
            ranges,
            caret,
            state,
            scroll,
            substitutions,
            source_len,
        }
    }

    #[cfg(test)]
    pub(super) fn composer_scroll_layout(&self, width: u16, capacity: usize) -> ScrollViewLayout {
        self.composer_viewport_layout(width, capacity).scroll
    }

    pub(super) fn queue_lines(&self, width: u16) -> Vec<Line<'static>> {
        if self.is_read_only_view() || self.queued.is_empty() {
            return Vec::new();
        }

        let mut lines = Vec::new();
        for (target, title) in [
            (
                QueueTarget::NextBoundary,
                "• Queued steering inputs (Enter)",
            ),
            (QueueTarget::EndOfTurn, "• Queued follow-up inputs (Tab)"),
        ] {
            let indexes = self
                .queued_display_indexes()
                .into_iter()
                .filter(|index| self.queued[*index].target == target)
                .collect::<Vec<_>>();
            if indexes.is_empty() {
                continue;
            }
            lines.push(Line::from(Span::styled(title, DIM_STYLE)));
            lines.extend(indexes.into_iter().map(|index| {
                let message = &self.queued[index];
                let marker = if self.selected_queue == Some(index) {
                    "› "
                } else {
                    "  "
                };
                let label = "↳ ";
                let prefix_width = marker.width().saturating_add(label.width());
                let text_width =
                    width.saturating_sub(u16::try_from(prefix_width).unwrap_or(u16::MAX));
                Line::from(vec![
                    Span::styled(marker, ACCENT_STYLE),
                    Span::raw(label),
                    Span::styled(
                        match message.kind {
                            cagent_agent::protocol::QueuedItemKind::Compact => {
                                if message.text.is_empty() {
                                    "/compact".into()
                                } else {
                                    compact_text(&format!("/compact {}", message.text), text_width)
                                }
                            }
                            cagent_agent::protocol::QueuedItemKind::ModePrompt => compact_text(
                                message.command_text.as_deref().unwrap_or(&message.text),
                                text_width,
                            ),
                            cagent_agent::protocol::QueuedItemKind::Prompt => {
                                compact_text(&message.text, text_width)
                            }
                        },
                        DIM_STYLE,
                    ),
                ])
            }));
        }
        lines
    }

    pub(super) fn completion_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.completion_layout(width).lines
    }

    pub(super) fn completion_layout(&self, width: u16) -> ListLayout {
        if let Some(completion) = &self.attachment_completion {
            let mut state = completion.list;
            state.reconcile(
                ListMode::Selectable,
                completion.rows.len(),
                VISIBLE_MENU_ITEMS,
            );
            return composer_completion_list_layout(width, &state, |index, _, selected| {
                let entry = &completion.rows[index];
                vec![menu_choice_line(
                    selected,
                    entry.path.to_string_lossy().into_owned(),
                    list_entry_kind(entry.kind),
                )]
            });
        }
        let mut state = if self.is_read_only_view() {
            self.observer_command_list
        } else {
            self.slash_list
        };
        let suggestions = self.slash_suggestions();
        if suggestions.is_empty() {
            return ListLayout {
                lines: Vec::new(),
                hits: Vec::new(),
            };
        }
        state.reconcile(ListMode::Selectable, suggestions.len(), VISIBLE_MENU_ITEMS);
        composer_completion_list_layout(width, &state, |index, _, selected| {
            let command = &suggestions[index];
            let alias = command
                .alias
                .map_or_else(String::new, |alias| format!(" ({alias})"));
            let argument = command
                .argument_hint
                .map_or_else(String::new, |hint| format!(" {hint}"));
            vec![menu_choice_line(
                selected,
                format!("{}{argument}{alias}", command.name),
                &command.description,
            )]
        })
    }

    pub(super) fn status_line(&self, width: u16) -> Line<'static> {
        let content_width = width.saturating_sub(4).max(1);
        if let Some(notice) = self.active_notice() {
            return padded_status_line(Line::from(notice.to_owned()).style(DIM_STYLE));
        }
        if self.history_preview.is_some() && !self.observer_command_active() {
            return keybinding_status_line(&self.contextual_key_hints("Esc return to history"));
        }
        if !self.slash_suggestions().is_empty() {
            return keybinding_status_line(
                &self.contextual_key_hints("↑/↓ navigate · Tab complete · Enter run · Esc close"),
            );
        }
        padded_status_line(status_line_content(
            &self.status_line_config,
            &self.status_line_values(),
            usize::from(content_width),
        ))
    }

    pub(crate) fn status_line_values(&self) -> cagent_agent::presentation::StatusLineValues {
        let selection = cagent_agent::provider::ModelRef::parse(&self.provider_model).ok();
        let sub_count = self.supervised_work.running_agent_count();
        let bg_count = self.supervised_work.running_terminal_count();
        cagent_agent::presentation::StatusLineValues {
            mode: self.mode.clone(),
            mode_color: self.mode_colors.get(&self.mode).copied(),
            agent: self.agent.clone(),
            model: selection.as_ref().map(|selection| selection.model.clone()),
            provider: selection.map(|selection| selection.provider),
            fast: self.fast_effective,
            provider_usage: self.provider_usage.clone(),
            effort: self.effort.clone(),
            context_percent: self.context_percent,
            context_used_tokens: self.context_used_tokens,
            context_window: self.context_window,
            // The agent-layer snapshot is already the complete active-branch
            // aggregate, including completed delegated runs.
            usage: self.session_usage.clone(),
            token_rate: self.token_rate_tracker.current(),
            subagents: sub_count,
            background: bg_count,
            composer_has_text: (!self.is_observer() && !self.draft.is_empty())
                || self.observer_command_active(),
            conversation_is_working: self.active,
            primary_hint: if self.is_observer() {
                None
            } else if !self.follow_history_tail {
                Some("Ctrl+↑/↓ navigate messages".into())
            } else if self.editing_queue.is_some() {
                Some("Enter to save, Ctrl+C to unqueue".into())
            } else if let Some(message) =
                self.selected_queue.and_then(|index| self.queued.get(index))
            {
                (matches!(
                    message.kind,
                    cagent_agent::protocol::QueuedItemKind::Prompt
                        | cagent_agent::protocol::QueuedItemKind::ModePrompt
                ))
                .then(|| "Enter to edit".into())
            } else {
                None
            },
            hints: (!self.is_observer() && self.editing_queue.is_none() && !self.queued.is_empty())
                .then(|| "Alt+↑/↓ queued".into())
                .into_iter()
                .collect(),
        }
    }

    #[cfg(test)]
    pub(super) fn cursor_position(
        &self,
        area: Rect,
        width: u16,
        draft_limit: u16,
    ) -> Option<Position> {
        if (self.is_read_only_view() && !self.observer_command_active())
            || (!self.surfaces.is_empty() && !self.history_preview_showing())
        {
            return None;
        }
        self.composer_viewport_layout(width, usize::from(draft_limit))
            .cursor_position(area)
    }

    pub(super) fn status_notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }
    pub(super) fn active_notice(&self) -> Option<&str> {
        self.notice_deadline
            .filter(|deadline| *deadline > Instant::now())
            .and(self.status_notice())
    }
    pub(super) fn set_notice(&mut self, notice: impl Into<String>) {
        self.notice = Some(notice.into());
        self.notice_deadline = Some(Instant::now() + NOTICE_DURATION);
        self.notice_muted = false;
    }
    pub(super) fn clear_notice(&mut self) {
        self.notice = None;
        self.notice_deadline = None;
        self.notice_muted = false;
    }

    #[cfg(test)]
    pub(super) fn push_user(
        &mut self,
        text: &str,
        attachments: &[cagent_agent::presentation::SubmittedAttachment],
    ) {
        self.history.push(crate::app::local_transcript_block(
            cagent_agent::protocol::TranscriptBlockKind::User {
                label: None,
                text: text.to_owned(),
                attachments: attachments.to_vec(),
                images: Vec::new(),
                image_chips: Vec::new(),
            },
        ));
    }

    #[cfg(test)]
    pub(super) fn push_user_with_label(
        &mut self,
        label: Option<&str>,
        text: &str,
        attachments: &[cagent_agent::presentation::SubmittedAttachment],
    ) {
        self.history.push(crate::app::local_transcript_block(
            cagent_agent::protocol::TranscriptBlockKind::User {
                label: label.map(str::to_owned),
                text: text.to_owned(),
                attachments: attachments.to_vec(),
                images: Vec::new(),
                image_chips: Vec::new(),
            },
        ));
        self.history_layout.rendered = None;
    }
    #[cfg(test)]
    pub(super) fn push_assistant(
        &mut self,
        document: cagent_agent::presentation::MarkdownDocument,
        source: String,
        status: Option<Line<'static>>,
    ) {
        if !document.is_empty() {
            self.history.push(crate::app::local_transcript_block(
                cagent_agent::protocol::TranscriptBlockKind::Assistant {
                    document,
                    source,
                    message: status.map(|line| line.to_string()),
                },
            ));
        }
        self.history_layout.rendered = None;
    }
    pub(super) fn push_system_message(&mut self, message: &str) {
        self.history.push(crate::app::local_transcript_block(
            cagent_agent::protocol::TranscriptBlockKind::Notice {
                message: message.to_owned(),
            },
        ));
        self.history_layout.rendered = None;
    }
    #[cfg(test)]
    pub(super) fn push_permission_denied(&mut self, resource: &str, reason: &str) {
        self.history.push(crate::app::local_transcript_block(
            cagent_agent::protocol::TranscriptBlockKind::PermissionDenied {
                resource: resource.to_owned(),
                reason: reason.to_owned(),
            },
        ));
        self.history_layout.rendered = None;
    }
}

fn transcript_block_kind(block: &TranscriptBlock) -> &'static str {
    match block.kind {
        cagent_agent::protocol::TranscriptBlockKind::WorkspaceTransition { .. } => {
            "workspace_transition"
        }
        cagent_agent::protocol::TranscriptBlockKind::User { .. } => "user",
        cagent_agent::protocol::TranscriptBlockKind::Compacted { .. } => "compacted",
        cagent_agent::protocol::TranscriptBlockKind::Assistant { .. } => "assistant",
        cagent_agent::protocol::TranscriptBlockKind::Plan { .. } => "plan",
        cagent_agent::protocol::TranscriptBlockKind::AcceptedPlan { .. } => "accepted_plan",
        cagent_agent::protocol::TranscriptBlockKind::ToolGroups { .. } => "tool_groups",
        cagent_agent::protocol::TranscriptBlockKind::Edits { .. } => "edits",
        cagent_agent::protocol::TranscriptBlockKind::Notice { .. } => "notice",
        cagent_agent::protocol::TranscriptBlockKind::Recap { .. } => "recap",
        cagent_agent::protocol::TranscriptBlockKind::PermissionDenied { .. } => "permission_denied",
        cagent_agent::protocol::TranscriptBlockKind::Interrupt { .. } => "interrupt",
        cagent_agent::protocol::TranscriptBlockKind::Work { .. } => "work",
    }
}

fn transcript_block_has_pending_activity(block: &TranscriptBlock) -> bool {
    matches!(
        block,
        TranscriptBlock { kind: cagent_agent::protocol::TranscriptBlockKind::ToolGroups { groups }, .. }
            if groups.iter().any(pending_activity_group)
    )
}

fn pending_activity_group(group: &cagent_agent::presentation::ToolActivityGroup) -> bool {
    transcript::activity_group_is_active(group)
}

/// Renders a composer-attached completion popup. Its separator replaces the
/// list's normal upper indicator row, keeping the popup attached to the
/// composer without sacrificing the page-up control when the list is scrolled.
fn composer_completion_list_layout<F>(
    width: u16,
    state: &crate::app::list::ListState,
    render_item: F,
) -> ListLayout
where
    F: FnMut(usize, u16, bool) -> Vec<Line<'static>>,
{
    let mut layout = ListWidget::new(width, VISIBLE_MENU_ITEMS)
        .policy(crate::app::list::LinePolicy::Truncate)
        .render(state, render_item);
    layout.lines[0] = if layout.hit(0) == ListHit::Up {
        merged_completion_up_indicator(width)
    } else {
        dim_separator_line(width)
    };
    layout
}

fn merged_completion_up_indicator(width: u16) -> Line<'static> {
    let prefix = "─ ↑ ".chars().take(usize::from(width)).collect::<String>();
    let tail = "─".repeat(usize::from(
        width.saturating_sub(u16::try_from(prefix.width()).unwrap_or(u16::MAX)),
    ));
    Line::from(vec![
        Span::styled(prefix, DIM_STYLE),
        Span::styled(tail, DIM_STYLE),
    ])
}

/// Shared frame geometry for expanded terminal output: a two-column inset on
/// both sides beneath the heading, with one fixed trailing padding row.
fn expanded_output_area(composer_area: Rect) -> Rect {
    Rect::new(
        composer_area.x.saturating_add(2),
        composer_area.y.saturating_add(2),
        composer_area.width.saturating_sub(4),
        composer_area.height.saturating_sub(3),
    )
}

fn image_render_area(viewport: Rect, protocol: &ratatui_image::protocol::Protocol) -> Rect {
    let size = protocol.size();
    Rect::new(
        viewport.x,
        viewport.y,
        viewport.width.min(size.width),
        viewport.height.min(size.height),
    )
}

fn preview_hit_rows(
    heading_row: usize,
    preview_rows: usize,
    history_len: usize,
) -> std::ops::Range<usize> {
    let start = heading_row.saturating_add(1);
    start..start.saturating_add(preview_rows).min(history_len)
}

fn bash_hit_rows(heading_row: usize, preview_rows: usize, history_len: usize) -> Vec<usize> {
    let mut rows = vec![heading_row];
    rows.extend(preview_hit_rows(heading_row, preview_rows, history_len));
    rows
}

fn question_lines(question: &cagent_agent::presentation::QuestionTranscript) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for entry in &question.entries {
        lines.push(Line::from(vec![
            Span::raw("  • "),
            Span::raw(terminal_safe(&entry.question)),
        ]));
        if let Some(answer) = &entry.answer {
            lines.push(Line::from(vec![
                Span::styled("    answer: ", DIM_STYLE),
                Span::styled(terminal_safe(answer), CHIP_STYLE),
            ]));
        }
        if let Some(note) = &entry.note {
            for (index, row) in note.split('\n').enumerate() {
                lines.push(Line::from(vec![
                    Span::styled(
                        if index == 0 {
                            "    note: "
                        } else {
                            "          "
                        },
                        DIM_STYLE,
                    ),
                    Span::raw(terminal_safe(row)),
                ]));
            }
        }
    }
    lines.push(Line::default());
    lines
}

pub(super) fn question_transcript_lines(
    question: &cagent_agent::presentation::QuestionTranscript,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::raw("• "),
        Span::styled("Questions", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" "),
        Span::styled(
            format!("{}/{}", question.answered, question.total),
            DIM_STYLE.add_modifier(Modifier::BOLD),
        ),
        Span::styled(" answered", DIM_STYLE),
    ])];
    lines.extend(question_lines(question));
    lines
}

struct BashOutputCandidate {
    terminal_id: Option<cagent_agent::tools::TerminalId>,
    command: String,
    output: String,
    ansi_output: String,
    completion: Option<String>,
    status: cagent_agent::presentation::ToolActivityStatus,
}

fn bash_output_candidates(app: &App) -> Vec<BashOutputCandidate> {
    let candidates = app
        .history
        .iter()
        .filter_map(|block| match block {
            TranscriptBlock {
                kind: cagent_agent::protocol::TranscriptBlockKind::ToolGroups { groups },
                ..
            } => Some(groups.as_slice()),
            _ => None,
        })
        .flatten()
        .filter_map(|group| match group {
            cagent_agent::presentation::ToolActivityGroup::Bash {
                terminal_id,
                command,
                output,
                ansi_output,
                status,
                exit_code,
                ..
            } => {
                let output = output.as_deref().unwrap_or_default();
                if terminal_id.is_none() && output.is_empty() {
                    return None;
                }
                Some(BashOutputCandidate {
                    terminal_id: *terminal_id,
                    command: command.clone(),
                    output: output.to_owned(),
                    ansi_output: ansi_output.clone().unwrap_or_else(|| output.to_owned()),
                    completion: bash_completion(*status, *exit_code),
                    status: *status,
                })
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    candidates
}

fn web_search_result_candidates(
    app: &App,
) -> Vec<(
    String,
    String,
    Vec<cagent_agent::web_search::WebSearchResult>,
)> {
    app.history
        .iter()
        .filter_map(|block| match block {
            TranscriptBlock {
                kind: cagent_agent::protocol::TranscriptBlockKind::ToolGroups { groups },
                ..
            } => Some(groups.as_slice()),
            _ => None,
        })
        .flatten()
        .filter_map(|group| match group {
            cagent_agent::presentation::ToolActivityGroup::WebSearch {
                provider,
                query,
                status: cagent_agent::presentation::ToolActivityStatus::Succeeded,
                results,
                ..
            } => Some((provider.clone(), query.clone(), results.clone())),
            _ => None,
        })
        .collect()
}

fn bash_completion(
    status: cagent_agent::presentation::ToolActivityStatus,
    exit_code: Option<i32>,
) -> Option<String> {
    (status != cagent_agent::presentation::ToolActivityStatus::Pending).then(|| {
        exit_code.map_or_else(
            || "[process exited]".into(),
            |code| format!("[exited with status {code}]"),
        )
    })
}

fn submitted_attachment_labels(
    text: &str,
    attachments: &[cagent_agent::presentation::SubmittedAttachment],
    image_chips: &[cagent_agent::protocol::ImageChipRange],
) -> Vec<ChipSubstitution> {
    let mut substitutions = attachments
        .iter()
        .map(|attachment| {
            let label = attachment_kind_label(
                attachment.kind == cagent_agent::presentation::SubmittedAttachmentKind::Directory,
            );
            ChipSubstitution {
                start: attachment.start,
                end: attachment.end,
                label: attachment_spec_label(&attachment.spec, label),
            }
        })
        .collect::<Vec<_>>();
    substitutions.extend(image_chips.iter().filter_map(|chip| {
        text.get(chip.start..chip.end)
            .map(|label| ChipSubstitution {
                start: chip.start,
                end: chip.end,
                label: label.to_owned(),
            })
    }));
    substitutions.sort_by_key(|substitution| substitution.start);
    substitutions
}

fn styled_user_slice(
    text: &str,
    start: usize,
    end: usize,
    substitutions: &[ChipSubstitution],
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut offset = start;
    for substitution in substitutions {
        if substitution.start < start || substitution.end > end {
            continue;
        }
        if offset < substitution.start {
            spans.push(Span::raw(terminal_safe(&text[offset..substitution.start])));
        }
        spans.push(Span::styled(substitution.label.clone(), CHIP_STYLE));
        offset = substitution.end;
    }
    if offset < end {
        spans.push(Span::raw(terminal_safe(&text[offset..end])));
    }
    spans
}

pub(super) fn keybinding_status_line(content: &str) -> Line<'static> {
    keybinding_status_line_with_disabled_key(content, "")
}

fn keybinding_status_line_with_disabled_key(content: &str, disabled_key: &str) -> Line<'static> {
    keybinding_status_line_with_disabled_keys(content, &[disabled_key])
}

fn keybinding_status_line_with_disabled_keys(
    content: &str,
    disabled_keys: &[&str],
) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, binding) in content.split(" · ").enumerate() {
        if index > 0 {
            spans.push(Span::styled(" · ", DIM_STYLE));
        }
        if let Some((key, description)) = binding.split_once(' ') {
            spans.push(Span::styled(
                key.to_owned(),
                if disabled_keys.contains(&key) {
                    DIM_STYLE
                } else {
                    ACCENT_STYLE
                },
            ));
            spans.push(Span::styled(format!(" {description}"), DIM_STYLE));
        } else {
            spans.push(Span::styled(binding.to_owned(), ACCENT_STYLE));
        }
    }
    padded_status_line(Line::from(spans))
}

fn statusline_disabled_keys(surface: &Surface) -> Option<&'static [&'static str]> {
    const RESET: &[&str] = &["Ctrl+R"];
    const COLOR_AND_RESET: &[&str] = &["c", "r"];
    const EDIT_AND_DELETE: &[&str] = &["e", "d"];

    match surface {
        Surface::Skills { rows, list } => list
            .selected
            .and_then(|selected| selected.checked_sub(2))
            .and_then(|selected| rows.get(selected))
            .is_some_and(|skill| skill.source == cagent_agent::config::SkillSource::System)
            .then_some(EDIT_AND_DELETE),
        Surface::Settings {
            rows,
            section,
            list,
            query,
            ..
        } => cagent_agent::presentation::filter_settings_rows(rows, *section, query)
            .get(list.selected.unwrap_or(0))
            .is_some_and(|row| row.explicit_value.is_none())
            .then_some(RESET),
        Surface::StatusLine {
            config,
            rows,
            list,
            mode: StatusLineEditorMode::Modules,
            ..
        } => {
            let module = list.selected.and_then(|selected| rows.get(selected))?;
            if *module == StatusLineModule::Mode {
                Some(COLOR_AND_RESET)
            } else if config.color(*module) == module.default_color() {
                Some(RESET)
            } else {
                None
            }
        }
        _ => None,
    }
}
pub(super) fn worked_line(elapsed: &str, width: u16) -> Line<'static> {
    let prefix = format!("─ Worked for {elapsed} ");
    let width = usize::from(width.max(1));
    let prefix_width = prefix.width();
    if prefix_width >= width {
        Line::from(Span::styled(
            prefix.chars().take(width).collect::<String>(),
            DIM_STYLE,
        ))
    } else {
        Line::from(vec![
            Span::styled(prefix, DIM_STYLE),
            Span::styled("─".repeat(width - prefix_width), DIM_STYLE),
        ])
    }
}

pub(super) fn compacted_line(width: u16) -> Line<'static> {
    let prefix = "─ Compacted ";
    let width = usize::from(width.max(1));
    let prefix_width = prefix.width();
    if prefix_width >= width {
        Line::from(Span::styled(
            prefix.chars().take(width).collect::<String>(),
            DIM_STYLE,
        ))
    } else {
        Line::from(vec![
            Span::styled(prefix, DIM_STYLE),
            Span::styled("─".repeat(width - prefix_width), DIM_STYLE),
        ])
    }
}

pub(super) fn dim_separator_line(width: u16) -> Line<'static> {
    Line::from("─".repeat(usize::from(width.max(1)))).style(DIM_STYLE)
}

pub(super) fn dim_separator_rows(width: u16) -> [crate::markdown::DisplayRow; 3] {
    [
        crate::markdown::DisplayRow::plain(Line::default()),
        crate::markdown::DisplayRow::plain(dim_separator_line(width)),
        crate::markdown::DisplayRow::plain(Line::default()),
    ]
}

pub(super) fn pad_history_line(line: &Line<'static>, width: u16) -> Line<'static> {
    if line.style != USER_STYLE && line.style.bg.is_none() {
        return line.clone();
    }
    let padding = usize::from(width).saturating_sub(line.width());
    if padding == 0 {
        return line.clone();
    }
    let mut spans = line.spans.clone();
    spans.push(Span::styled(" ".repeat(padding), line.style));
    Line::from(spans).style(line.style)
}

pub(super) fn wrap_log_line(line: &Line<'static>, width: u16) -> Vec<Line<'static>> {
    let width = usize::from(width.max(1));
    if visible_line_width(line) <= width {
        return vec![line.clone()];
    }
    let content = line.to_string();
    let outer_gutter_width = if content.starts_with("  └ ") || content.starts_with("  │ ") {
        // Activity children have a four-column tree prefix. Keep the whole
        // prefix reserved so wrapped task text begins beyond the elbow.
        4
    } else if content.starts_with("› ")
        || content.starts_with("• ")
        // Assistant lines after the first line already carry the
        // continuation gutter, so they need the same reduced width.
        || content.starts_with("  ")
    {
        2
    } else {
        0
    };
    let outer_gutter_end = byte_index_after_display_width(&content, outer_gutter_width);
    let quote_gutter_width = quote_gutter_width(&content[outer_gutter_end..]);
    let gutter_width = outer_gutter_width + quote_gutter_width;
    let has_gutter = gutter_width > 0;
    let mut lines = Vec::new();
    // The bullet/chevron and following space occupy two columns on the first
    // row. Continuation rows reserve those same columns with two spaces.
    let content_width = width.saturating_sub(gutter_width).max(1);
    let (prefix_spans, body_spans) = split_prefix_spans(line, gutter_width);
    let body = body_spans
        .iter()
        .flat_map(|span| {
            span.content
                .graphemes(true)
                .map(move |grapheme| (grapheme.to_owned(), span.style))
        })
        .collect::<Vec<_>>();
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < body.len() {
        let chunk_start = start;
        let mut end = start;
        let mut used = 0;
        let mut last_break = None;
        while end < body.len() {
            let grapheme_width = display_grapheme_width(&body[end].0);
            if used > 0 && used + grapheme_width > content_width {
                if body[end].0.chars().all(char::is_whitespace) {
                    last_break = Some(end);
                }
                break;
            }
            used += grapheme_width;
            if body[end].0.chars().all(char::is_whitespace) {
                last_break = Some(end);
            }
            end += 1;
        }
        if end < body.len()
            && let Some(break_at) = last_break.filter(|break_at| *break_at > chunk_start)
        {
            end = break_at;
            start = break_at + 1;
            while start < body.len() && body[start].0.chars().all(char::is_whitespace) {
                start += 1;
            }
        } else {
            start = end;
        }
        chunks.push((chunk_start, end));
    }
    if body.is_empty() {
        chunks.push((0, 0));
    }

    let full_prefix = prefix_spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    let continuation = if full_prefix.starts_with("  │ ") {
        // Permission command rows keep the vertical bar on every wrapped
        // line so the command remains visibly inside the approval gutter.
        full_prefix.clone()
    } else if quote_gutter_width > 0 {
        if outer_gutter_width > 0 {
            let outer_end = byte_index_after_display_width(&full_prefix, outer_gutter_width);
            format!("  {}", &full_prefix[outer_end..])
        } else {
            full_prefix
        }
    } else {
        " ".repeat(outer_gutter_width)
    };
    let continuation_style = prefix_spans.last().map_or(line.style, |span| span.style);
    for (chunk_index, (chunk_start, chunk_end)) in chunks.iter().enumerate() {
        let mut output = if chunk_index == 0 && has_gutter {
            prefix_spans.clone()
        } else if has_gutter {
            vec![Span::styled(continuation.clone(), continuation_style)]
        } else {
            Vec::new()
        };
        output.extend(
            body[*chunk_start..*chunk_end]
                .iter()
                .map(|(grapheme, style)| Span::styled(grapheme.clone(), *style)),
        );
        lines.push(Line::from(output).style(line.style));
    }
    lines
}

fn quote_gutter_width(content: &str) -> usize {
    let mut remaining = content;
    let mut width = 0;
    loop {
        if let Some(rest) = remaining.strip_prefix("┃ ") {
            width += 2;
            remaining = rest;
        } else if let Some(rest) = remaining.strip_prefix("│ ") {
            width += 2;
            remaining = rest;
        } else {
            return width;
        }
    }
}

fn split_prefix_spans(
    line: &Line<'static>,
    width: usize,
) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
    let mut remaining_width = width;
    let mut prefix = Vec::new();
    let mut body = Vec::new();
    for span in &line.spans {
        if remaining_width == 0 {
            body.push(span.clone());
            continue;
        }
        let span_width = span.content.width();
        if span_width <= remaining_width {
            prefix.push(span.clone());
            remaining_width -= span_width;
            continue;
        }
        let split = byte_index_after_display_width(&span.content, remaining_width);
        prefix.push(Span::styled(span.content[..split].to_owned(), span.style));
        body.push(Span::styled(span.content[split..].to_owned(), span.style));
        remaining_width = 0;
    }
    (prefix, body)
}

pub(super) fn wrap_log_lines(lines: &[Line<'static>], width: u16) -> Vec<Line<'static>> {
    lines
        .iter()
        .flat_map(|line| wrap_log_line(line, width))
        .collect()
}

fn visible_line_width(line: &Line<'_>) -> usize {
    line.spans
        .iter()
        .map(|span| {
            span.content
                .graphemes(true)
                .map(display_grapheme_width)
                .sum::<usize>()
        })
        .sum()
}

fn byte_index_after_display_width(text: &str, width: usize) -> usize {
    let mut used = 0;
    for (offset, grapheme) in text.grapheme_indices(true) {
        let grapheme_width = display_grapheme_width(grapheme);
        if used + grapheme_width > width {
            break;
        }
        used += grapheme_width;
        if used >= width {
            return offset + grapheme.len();
        }
    }
    text.len()
}

fn display_grapheme_width(grapheme: &str) -> usize {
    if grapheme.chars().all(char::is_control) {
        0
    } else {
        grapheme.width()
    }
}

pub(super) fn styled_draft_slice(
    slice: &str,
    substitutions: &[ChipSubstitution],
    offset: usize,
    command_end: Option<usize>,
) -> Vec<Span<'static>> {
    let mut ranges = Vec::new();
    let mut source = 0;
    let mut rendered = 0;
    for substitution in substitutions {
        rendered += substitution.start.saturating_sub(source);
        ranges.push((rendered, rendered + substitution.label.len()));
        rendered += substitution.label.len();
        source = substitution.end;
    }
    let end = offset + slice.len();
    let mut boundaries = vec![offset, end];
    for (range_start, range_end) in &ranges {
        boundaries.push((*range_start).clamp(offset, end));
        boundaries.push((*range_end).clamp(offset, end));
    }
    if let Some(command_end) = command_end {
        boundaries.push(command_end.clamp(offset, end));
    }
    boundaries.sort_unstable();
    boundaries.dedup();

    let mut spans = Vec::new();
    for boundary in boundaries.windows(2) {
        let start = boundary[0];
        let end = boundary[1];
        if start == end {
            continue;
        }
        let text = slice[start - offset..end - offset].to_owned();
        if ranges
            .iter()
            .any(|(chip_start, chip_end)| start >= *chip_start && start < *chip_end)
        {
            spans.push(Span::styled(text, CHIP_STYLE));
        } else if command_end.is_some_and(|command_end| start < command_end) {
            spans.push(Span::styled(text, COMMAND_STYLE));
        } else {
            spans.push(Span::raw(text));
        }
    }
    spans
}

fn append_display_row_path_hits(
    hits: &mut Vec<crate::app::PathHit>,
    row: &crate::markdown::DisplayRow,
    screen_row: u16,
    area_x: u16,
    area_width: u16,
) {
    for segment in row.paths() {
        if !segment.target.path.exists()
            || segment
                .target
                .line
                .is_some_and(|_| !segment.target.path.is_file())
        {
            continue;
        }
        let start = area_x.saturating_add(u16::try_from(segment.column).unwrap_or(u16::MAX));
        if start >= area_x.saturating_add(area_width) {
            continue;
        }
        let end = start
            .saturating_add(u16::try_from(segment.width).unwrap_or(u16::MAX))
            .min(area_x.saturating_add(area_width));
        if start < end {
            hits.push(crate::app::PathHit {
                row: screen_row,
                columns: start..end,
                path: segment.target.path.clone(),
                line: segment.target.line,
                directory: segment.target.directory,
            });
        }
    }
}

fn append_image_hit(
    hits: &mut Vec<crate::app::ImageHit>,
    rendered: &str,
    row: u16,
    x: u16,
    width: u16,
    image: &cagent_agent::protocol::ImageAttachment,
) {
    let label = format!("[Image #{}]", image.number);
    let mut start = 0;
    while let Some(relative) = rendered[start..].find(&label) {
        let byte = start + relative;
        let column = rendered[..byte].width();
        let screen_column = x.saturating_add(u16::try_from(column).unwrap_or(u16::MAX));
        let end = screen_column
            .saturating_add(u16::try_from(label.width()).unwrap_or(u16::MAX))
            .min(x.saturating_add(width));
        if screen_column < end
            && !hits
                .iter()
                .any(|hit| hit.row == row && hit.columns == (screen_column..end))
        {
            hits.push(crate::app::ImageHit {
                row,
                columns: screen_column..end,
                image: image.clone(),
            });
        }
        start = byte + label.len();
    }
}

pub(crate) fn terminal_safe(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_control() && !matches!(character, '\n' | '\t') {
                format!("\\u{{{:x}}}", u32::from(character))
            } else {
                character.to_string()
            }
        })
        .collect()
}
pub(super) fn format_bytes(bytes: usize) -> String {
    if bytes >= 1024 {
        let tenths = bytes.saturating_mul(10) / 1024;
        format!("{}.{:01} KiB", tenths / 10, tenths % 10)
    } else {
        format!("{bytes} B")
    }
}
pub(super) fn attachment_chip_label(chip: &AttachmentChip) -> String {
    let label = attachment_kind_label(chip.kind == ListEntryKind::Directory);
    attachment_spec_label(&chip.spec, label)
}

fn attachment_kind_label(directory: bool) -> &'static str {
    if directory { "Dir" } else { "File" }
}

pub(super) fn attachment_spec_label(
    spec: &cagent_agent::protocol::AttachmentSpec,
    label: &str,
) -> String {
    let path = terminal_safe(&spec.path.to_string_lossy());
    match (spec.start_line, spec.end_line) {
        (Some(start), Some(end)) => format!("[{label} · {path}:{start}-{end}]"),
        _ => format!("[{label} · {path}]"),
    }
}

#[cfg(test)]
mod tests;
