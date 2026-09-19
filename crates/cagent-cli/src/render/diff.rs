//! Semantic diff projection for history, previews, and approval surfaces.

use std::path::Path;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::{DIM_STYLE, terminal_safe};

fn display_path(path: &Path, workspace: Option<&Path>) -> String {
    terminal_safe(&cagent_agent::presentation::display_activity_path(
        path,
        workspace.unwrap_or_else(|| Path::new("")),
    ))
}

/// Renders a complete diff for the fullscreen expanded view.
///
/// Unlike compact transcript rendering, every source row wraps within the
/// viewport and addition/deletion backgrounds are padded through its right
/// edge, matching the built-in file viewer.
pub(crate) fn render_expanded_diff(
    diff: &cagent_agent::tools::SemanticDiff,
    width: u16,
) -> Vec<Line<'static>> {
    const MAX_ROWS: usize = 50_000;
    // Count wrapping before allocating/highlighting rows. This also applies on
    // resize, when an otherwise modest diff could expand into millions of rows.
    let mut rows = 0_usize;
    for file in &diff.files {
        rows = rows.saturating_add(3 + file.hunks.len());
        let gutter = line_number_width(file);
        for line in file.hunks.iter().flat_map(|hunk| &hunk.lines) {
            rows = rows.saturating_add(diff_preview_wrapped_row_count(line, gutter, "  ", width));
            if rows > MAX_ROWS {
                return vec![crate::app::list::truncate_line(
                    Line::from("  Diff is too large to display at this width (50,000 row limit)")
                        .style(DIM_STYLE),
                    usize::from(width),
                )];
            }
        }
        if rows > MAX_ROWS {
            return vec![crate::app::list::truncate_line(
                Line::from("  Diff is too large to display (50,000 row limit)").style(DIM_STYLE),
                usize::from(width),
            )];
        }
    }
    let mut lines = Vec::new();
    for (file_index, file) in diff.files.iter().enumerate() {
        if file_index > 0 {
            lines.push(Line::default());
        }
        let path = diff_display_path(file, Path::new(""));
        let kind = match file.kind {
            cagent_agent::tools::DiffFileKind::Modified => "",
            cagent_agent::tools::DiffFileKind::Added => " · new file",
            cagent_agent::tools::DiffFileKind::Deleted => " · deleted",
            cagent_agent::tools::DiffFileKind::Renamed => " · renamed",
        };
        lines.push(crate::app::list::truncate_line(
            Line::from(vec![
                Span::raw("  "),
                Span::styled(path, Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(kind, DIM_STYLE),
                Span::styled("  ", DIM_STYLE),
                Span::styled(format!("+{}", file.added_lines), DIFF_ADDITION_FOREGROUND),
                Span::raw(" "),
                Span::styled(format!("-{}", file.removed_lines), DIFF_DELETION_FOREGROUND),
            ]),
            usize::from(width),
        ));
        let line_number_width = line_number_width(file);
        for hunk in &file.hunks {
            lines.push(crate::app::list::truncate_line(
                Line::from(format!("  {}", terminal_safe(&hunk.header))).style(DIM_STYLE),
                usize::from(width),
            ));
            for line in &hunk.lines {
                lines.extend(render_diff_preview_line(
                    file,
                    line,
                    line_number_width,
                    "  ",
                    width,
                ));
            }
        }
        if file.old_no_final_newline || file.new_no_final_newline {
            lines.push(crate::app::list::truncate_line(
                Line::from("  \\ No newline at end of file").style(DIM_STYLE),
                usize::from(width),
            ));
        }
    }
    lines
}

pub(crate) fn render_edit_history(
    diff: &cagent_agent::tools::SemanticDiff,
    workspace: &Path,
    width: u16,
) -> Vec<Line<'static>> {
    let added = diff.files.iter().map(|file| file.added_lines).sum::<u64>();
    let removed = diff
        .files
        .iter()
        .map(|file| file.removed_lines)
        .sum::<u64>();
    let (verb, subject) = if let [file] = diff.files.as_slice() {
        let verb = match file.kind {
            cagent_agent::tools::DiffFileKind::Added => "Added",
            cagent_agent::tools::DiffFileKind::Deleted => "Deleted",
            cagent_agent::tools::DiffFileKind::Modified
            | cagent_agent::tools::DiffFileKind::Renamed => "Edited",
        };
        (verb, format!("{} ", diff_display_path(file, workspace)))
    } else {
        ("Edited", format!("{} files ", diff.files.len()))
    };
    let mut lines = vec![Line::from(vec![
        Span::styled("• ", DIM_STYLE),
        Span::styled(verb, Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(format!(" {subject}(")),
        Span::styled(format!("+{added}"), DIFF_ADDITION_FOREGROUND),
        Span::raw(" "),
        Span::styled(format!("-{removed}"), DIFF_DELETION_FOREGROUND),
        Span::raw(")"),
    ])];
    lines.extend(render_diff_lines(
        diff,
        Some(workspace),
        diff.files.len() > 1,
        false,
        "    ",
        Some(width),
    ));
    lines
}

/// Semantic source locations parallel to [`render_edit_history`]. Wrapped
/// visual rows inherit the source location of their logical diff row.
pub(crate) fn edit_history_line_targets(
    diff: &cagent_agent::tools::SemanticDiff,
) -> Vec<Option<(usize, Option<u64>, Option<u64>)>> {
    let mut targets = vec![None];
    for (file_index, file) in diff.files.iter().enumerate() {
        if diff.files.len() > 1 {
            targets.push(None);
        }
        for (hunk_index, hunk) in file.hunks.iter().enumerate() {
            if hunk_index > 0 {
                targets.push(None);
            }
            targets.extend(
                hunk.lines
                    .iter()
                    .map(|line| Some((file_index, line.old_line, line.new_line))),
            );
        }
        if file.old_no_final_newline || file.new_no_final_newline {
            targets.push(None);
        }
    }
    targets
}

#[derive(Debug)]
pub(crate) struct DiffPreviewLayout {
    width: u16,
    entries: Vec<DiffPreviewEntry>,
    file_line_number_widths: Vec<usize>,
    content_rows: usize,
}

#[derive(Debug)]
struct DiffPreviewEntry {
    start: usize,
    rows: usize,
    kind: DiffPreviewEntryKind,
}

#[derive(Debug)]
enum DiffPreviewEntryKind {
    Line {
        file: usize,
        hunk: usize,
        line: usize,
    },
    HunkSeparator,
    NoFinalNewline,
}

impl DiffPreviewLayout {
    pub(crate) fn new(diff: &cagent_agent::tools::SemanticDiff, width: u16) -> Self {
        let mut entries = Vec::new();
        let mut file_line_number_widths = Vec::with_capacity(diff.files.len());
        let mut content_rows = 0_usize;
        for (file_index, file) in diff.files.iter().enumerate() {
            let line_number_width = line_number_width(file);
            file_line_number_widths.push(line_number_width);
            for (hunk_index, hunk) in file.hunks.iter().enumerate() {
                if hunk_index > 0 {
                    entries.push(DiffPreviewEntry {
                        start: content_rows,
                        rows: 1,
                        kind: DiffPreviewEntryKind::HunkSeparator,
                    });
                    content_rows = content_rows.saturating_add(1);
                }
                for (line_index, line) in hunk.lines.iter().enumerate() {
                    let rows = diff_preview_wrapped_row_count(line, line_number_width, "  ", width);
                    entries.push(DiffPreviewEntry {
                        start: content_rows,
                        rows,
                        kind: DiffPreviewEntryKind::Line {
                            file: file_index,
                            hunk: hunk_index,
                            line: line_index,
                        },
                    });
                    content_rows = content_rows.saturating_add(rows);
                }
            }
            if file.old_no_final_newline || file.new_no_final_newline {
                entries.push(DiffPreviewEntry {
                    start: content_rows,
                    rows: 1,
                    kind: DiffPreviewEntryKind::NoFinalNewline,
                });
                content_rows = content_rows.saturating_add(1);
            }
        }
        Self {
            width,
            entries,
            file_line_number_widths,
            content_rows,
        }
    }

    pub(crate) const fn content_rows(&self) -> usize {
        self.content_rows
    }

    pub(crate) fn render_window(
        &self,
        diff: &cagent_agent::tools::SemanticDiff,
        start: usize,
        rows: usize,
    ) -> Vec<Line<'static>> {
        self.render_window_with(diff, start, rows, render_diff_preview_line)
    }

    fn render_window_with<F>(
        &self,
        diff: &cagent_agent::tools::SemanticDiff,
        start: usize,
        rows: usize,
        mut render_line: F,
    ) -> Vec<Line<'static>>
    where
        F: FnMut(
            &cagent_agent::tools::DiffFile,
            &cagent_agent::tools::DiffLine,
            usize,
            &str,
            u16,
        ) -> Vec<Line<'static>>,
    {
        let end = start.saturating_add(rows).min(self.content_rows);
        let mut rendered = Vec::with_capacity(end.saturating_sub(start));
        let first = self
            .entries
            .partition_point(|entry| entry.start.saturating_add(entry.rows) <= start);
        for entry in self.entries.iter().skip(first) {
            if entry.start >= end {
                break;
            }
            match entry.kind {
                DiffPreviewEntryKind::Line {
                    file: file_index,
                    hunk,
                    line,
                } => {
                    let file = &diff.files[file_index];
                    let line = &file.hunks[hunk].lines[line];
                    let line_rows = render_line(
                        file,
                        line,
                        self.file_line_number_widths[file_index],
                        "  ",
                        self.width,
                    );
                    debug_assert_eq!(line_rows.len(), entry.rows);
                    let first = start.saturating_sub(entry.start).min(entry.rows);
                    let last = end.saturating_sub(entry.start).min(entry.rows);
                    rendered.extend(line_rows.into_iter().skip(first).take(last - first));
                }
                DiffPreviewEntryKind::HunkSeparator => {
                    rendered.push(Line::from("  ⋮").style(DIM_STYLE));
                }
                DiffPreviewEntryKind::NoFinalNewline => {
                    rendered.push(Line::from("  \\ No newline at end of file").style(DIM_STYLE));
                }
            }
        }
        rendered
    }
}

/// Returns the number of rows in the compact preview used by permission prompts.
///
/// This deliberately counts structural diff rows without syntax highlighting them,
/// so scroll bounds stay cheap even for very large proposed changes.
pub(crate) fn diff_preview_line_count(
    diff: &cagent_agent::tools::SemanticDiff,
    width: u16,
) -> usize {
    DiffPreviewLayout::new(diff, width).content_rows()
}

/// Renders only a viewport of a compact diff preview.
///
/// Permission prompts can contain whole-file diffs. Rendering and highlighting
/// every row on each scroll event made those prompts increasingly slow as the
/// diff grew. Locate the requested rows structurally, then highlight just the
/// rows the terminal can display.
pub(crate) fn render_diff_preview_window(
    diff: &cagent_agent::tools::SemanticDiff,
    width: u16,
    start: usize,
    rows: usize,
) -> Vec<Line<'static>> {
    DiffPreviewLayout::new(diff, width).render_window(diff, start, rows)
}

#[cfg(test)]
pub(super) fn render_diff_preview_window_with<F>(
    diff: &cagent_agent::tools::SemanticDiff,
    width: u16,
    start: usize,
    rows: usize,
    mut render_line: F,
) -> Vec<Line<'static>>
where
    F: FnMut(
        &cagent_agent::tools::DiffFile,
        &cagent_agent::tools::DiffLine,
        usize,
        &str,
        u16,
    ) -> Vec<Line<'static>>,
{
    DiffPreviewLayout::new(diff, width).render_window_with(diff, start, rows, &mut render_line)
}

fn diff_preview_wrapped_row_count(
    line: &cagent_agent::tools::DiffLine,
    line_number_width: usize,
    line_indent: &str,
    width: u16,
) -> usize {
    let prefix_width = format!("{line_indent}{:>line_number_width$} ", "").width() + 1;
    wrapped_row_count(&line.text, width.saturating_sub(prefix_width as u16))
}

pub(super) fn wrapped_row_count(text: &str, width: u16) -> usize {
    let width = usize::from(width.max(1));
    let mut rows = 0_usize;
    let mut start = 0;
    let mut used = 0;
    let mut last_break = None;
    for (offset, grapheme) in text.grapheme_indices(true) {
        if grapheme == "\n" {
            rows = rows.saturating_add(1);
            start = offset + grapheme.len();
            used = 0;
            last_break = None;
            continue;
        }
        let grapheme_width = grapheme.width();
        if used > 0 && used + grapheme_width > width {
            if let Some(break_at) = last_break.filter(|break_at| *break_at > start) {
                rows = rows.saturating_add(1);
                start = break_at;
                used = text[start..offset].width();
            } else {
                rows = rows.saturating_add(1);
                start = offset;
                used = 0;
            }
            last_break = None;
        }
        used += grapheme_width;
        if grapheme.chars().all(char::is_whitespace) {
            last_break = Some(offset + grapheme.len());
        }
    }
    rows.saturating_add(1)
}

fn render_diff_preview_line(
    file: &cagent_agent::tools::DiffFile,
    line: &cagent_agent::tools::DiffLine,
    line_number_width: usize,
    line_indent: &str,
    width: u16,
) -> Vec<Line<'static>> {
    let (number, marker, style) = match line.kind {
        cagent_agent::tools::DiffLineKind::Addition => (line.new_line, '+', DIFF_ADDITION_STYLE),
        cagent_agent::tools::DiffLineKind::Deletion => (line.old_line, '-', DIFF_DELETION_STYLE),
        cagent_agent::tools::DiffLineKind::Context => {
            (line.new_line.or(line.old_line), ' ', Style::default())
        }
    };
    let prefix = format!(
        "{line_indent}{:>line_number_width$} {marker}",
        number.unwrap_or_default()
    );
    let ranges = super::wrap_ranges(&line.text, width.saturating_sub(prefix.width() as u16));
    let tokens = cagent_agent::presentation::highlight_code(
        file.language.as_deref().unwrap_or(""),
        &line.text,
    );
    ranges
        .into_iter()
        .enumerate()
        .map(|(index, (start, end))| {
            let mut spans = vec![Span::styled(
                if index == 0 {
                    prefix.clone()
                } else {
                    format!("{line_indent}{:>line_number_width$}  ", "")
                },
                if index == 0 { style } else { DIM_STYLE },
            )];
            let mut token_start = 0;
            for token in &tokens {
                let token_end = token_start + token.text.len();
                let slice_start = start.max(token_start);
                let slice_end = end.min(token_end);
                if slice_start < slice_end {
                    spans.push(Span::styled(
                        terminal_safe(
                            &token.text[slice_start - token_start..slice_end - token_start],
                        ),
                        crate::markdown::code_style(token.kind),
                    ));
                }
                token_start = token_end;
            }
            let mut line = Line::from(spans).style(style);
            let padding = usize::from(width).saturating_sub(line.width());
            if padding > 0 {
                line.spans.push(Span::styled(" ".repeat(padding), style));
            }
            line
        })
        .collect()
}

#[allow(clippy::too_many_lines)]
fn render_diff_lines(
    diff: &cagent_agent::tools::SemanticDiff,
    workspace: Option<&Path>,
    include_file_headers: bool,
    include_hunk_headers: bool,
    line_indent: &str,
    width: Option<u16>,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for file in &diff.files {
        let path = match (&file.old_path, &file.new_path) {
            (Some(old), Some(new)) if old != new => format!(
                "{} → {}",
                display_path(old, workspace),
                display_path(new, workspace)
            ),
            (_, Some(path)) | (Some(path), None) => display_path(path, workspace),
            (None, None) => "file".into(),
        };
        let kind = match file.kind {
            cagent_agent::tools::DiffFileKind::Modified => "",
            cagent_agent::tools::DiffFileKind::Added => " (new file)",
            cagent_agent::tools::DiffFileKind::Deleted => " (deleted)",
            cagent_agent::tools::DiffFileKind::Renamed => " (renamed)",
        };
        if include_file_headers {
            lines.push(Line::from(vec![
                Span::styled("  └ ", DIM_STYLE),
                Span::raw(path),
                Span::styled(kind, DIM_STYLE),
                Span::raw(" ("),
                Span::styled(format!("+{}", file.added_lines), DIFF_ADDITION_FOREGROUND),
                Span::raw(" "),
                Span::styled(format!("-{}", file.removed_lines), DIFF_DELETION_FOREGROUND),
                Span::raw(")"),
            ]));
        }
        let line_number_width = line_number_width(file);
        for (hunk_index, hunk) in file.hunks.iter().enumerate() {
            if hunk_index > 0 {
                lines.push(Line::from(format!("{line_indent}⋮")).style(DIM_STYLE));
            }
            if include_hunk_headers {
                lines.push(
                    Line::from(format!("  {}", terminal_safe(&hunk.header))).style(DIM_STYLE),
                );
            }
            for line in &hunk.lines {
                lines.push(render_diff_line(
                    file,
                    line,
                    line_number_width,
                    line_indent,
                    width,
                ));
            }
        }
        if file.old_no_final_newline || file.new_no_final_newline {
            lines.push(Line::from("  \\ No newline at end of file").style(DIM_STYLE));
        }
    }
    lines
}

fn line_number_width(file: &cagent_agent::tools::DiffFile) -> usize {
    file.hunks
        .iter()
        .flat_map(|hunk| hunk.lines.iter())
        .flat_map(|line| [line.old_line, line.new_line])
        .flatten()
        .max()
        .map_or(1, |number| number.to_string().len())
}

fn render_diff_line(
    file: &cagent_agent::tools::DiffFile,
    line: &cagent_agent::tools::DiffLine,
    line_number_width: usize,
    line_indent: &str,
    width: Option<u16>,
) -> Line<'static> {
    let (number, marker, style) = match line.kind {
        cagent_agent::tools::DiffLineKind::Addition => (line.new_line, '+', DIFF_ADDITION_STYLE),
        cagent_agent::tools::DiffLineKind::Deletion => (line.old_line, '-', DIFF_DELETION_STYLE),
        cagent_agent::tools::DiffLineKind::Context => {
            (line.new_line.or(line.old_line), ' ', Style::default())
        }
    };
    let mut spans = vec![
        Span::styled(
            format!(
                "{line_indent}{:>line_number_width$} ",
                number.unwrap_or_default()
            ),
            DIM_STYLE,
        ),
        Span::styled(marker.to_string(), style),
    ];
    spans.extend(
        cagent_agent::presentation::highlight_code(
            file.language.as_deref().unwrap_or(""),
            &line.text,
        )
        .into_iter()
        .map(|token| {
            Span::styled(
                terminal_safe(&token.text),
                crate::markdown::code_style(token.kind),
            )
        }),
    );
    let mut rendered = Line::from(spans).style(style);
    if let Some(width) = width {
        rendered.spans.push(Span::raw(
            " ".repeat(usize::from(width).saturating_sub(rendered.width())),
        ));
    }
    rendered
}

fn diff_display_path(file: &cagent_agent::tools::DiffFile, workspace: &Path) -> String {
    match (&file.old_path, &file.new_path) {
        (Some(old), Some(new)) if old != new => format!(
            "{} → {}",
            display_path(old, Some(workspace)),
            display_path(new, Some(workspace))
        ),
        (_, Some(path)) | (Some(path), None) => display_path(path, Some(workspace)),
        (None, None) => "file".into(),
    }
}

pub(crate) const DIFF_ADDITION_STYLE: Style = Style::new()
    .fg(Color::Rgb(154, 205, 114))
    .bg(Color::Rgb(33, 58, 43));
pub(crate) const DIFF_DELETION_STYLE: Style = Style::new()
    .fg(Color::Rgb(255, 107, 107))
    .bg(Color::Rgb(74, 34, 29));
const DIFF_ADDITION_FOREGROUND: Style = Style::new().fg(Color::Rgb(154, 205, 114));
const DIFF_DELETION_FOREGROUND: Style = Style::new().fg(Color::Rgb(255, 107, 107));
