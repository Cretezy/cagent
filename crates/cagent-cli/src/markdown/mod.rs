use cagent_agent::presentation::{
    CodeTokenKind, MarkdownAlignment, MarkdownBlock, MarkdownDocument, MarkdownInline,
    MarkdownListItem, MarkdownTable, MarkdownTableCell, MarkdownTableRow,
};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use std::io::{self, Write};
use std::sync::Arc;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const DIM: Style = Style::new().add_modifier(Modifier::DIM);
const LIST_MARKER: Style = Style::new().add_modifier(Modifier::DIM);
const CODE_BAR: Style = Style::new().add_modifier(Modifier::DIM);
const QUOTE_TEXT: Style = Style::new().fg(Color::LightGreen);
const QUOTE_BAR: Style = Style::new().fg(Color::LightGreen);
const TABLE_HEADER: Style = Style::new()
    .fg(Color::LightYellow)
    .add_modifier(Modifier::BOLD);
const TABLE_RULE: Style = DIM;
const TABLE_CELL_PADDING: usize = 1;
const TABLE_COLUMN_GAP: usize = 2;
const INLINE_CODE: Style = Style::new().fg(Color::Cyan);
const LINK: Style = Style::new()
    .fg(Color::Cyan)
    .add_modifier(Modifier::UNDERLINED);
const PATH_REFERENCE: Style = LINK;
const CODE_PLAIN: Style = Style::new().fg(Color::Indexed(252));
const CODE_COMMENT: Style = Style::new().fg(Color::Indexed(103));
const CODE_STRING: Style = Style::new().fg(Color::Indexed(114));
const CODE_NUMBER: Style = Style::new().fg(Color::Indexed(215));
const CODE_KEYWORD: Style = Style::new().fg(Color::Indexed(141));
const CODE_TYPE: Style = Style::new().fg(Color::Indexed(117));
const CODE_FUNCTION: Style = Style::new().fg(Color::Indexed(111));
const CODE_MACRO: Style = Style::new().fg(Color::Indexed(204));
const CODE_ATTRIBUTE: Style = Style::new().fg(Color::Indexed(216));
const CODE_OPERATOR: Style = Style::new().fg(Color::Indexed(81));
const CODE_MARKUP_HEADING: Style = Style::new()
    .fg(Color::Indexed(117))
    .add_modifier(Modifier::BOLD);
const CODE_MARKUP_STRONG: Style = Style::new()
    .fg(Color::Indexed(252))
    .add_modifier(Modifier::BOLD);
const CODE_MARKUP_EMPHASIS: Style = Style::new()
    .fg(Color::Indexed(252))
    .add_modifier(Modifier::ITALIC);
const CODE_MARKUP_LINK: Style = Style::new()
    .fg(Color::Cyan)
    .add_modifier(Modifier::UNDERLINED);
const CODE_MARKUP_RAW: Style = Style::new().fg(Color::Cyan);

/// Frontend-local payload for a rendered web-fetch card that can open detail.
#[derive(Clone, Debug)]
pub(crate) struct WebFetchOutputTarget {
    pub(crate) url: String,
    pub(crate) redirected_url: Option<String>,
    pub(crate) format: cagent_agent::WebFetchFormat,
    pub(crate) output: Arc<str>,
}

/// Frontend-local payload for a compaction divider that can open its summary.
#[derive(Clone, Debug)]
pub(crate) struct CompactionTarget {
    pub(crate) summary: Arc<str>,
}

#[derive(Clone, Debug)]
pub(crate) struct DiffLineTarget {
    pub(crate) diff: Arc<cagent_agent::tools::SemanticDiff>,
    pub(crate) file_index: usize,
    pub(crate) old_line: Option<u64>,
    pub(crate) new_line: Option<u64>,
}

/// Frontend-local identity for an inline Explore expansion control.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum ExplorationToggleTarget {
    Transcript {
        block_id: cagent_agent::protocol::TranscriptBlockId,
        group_index: usize,
    },
    AgentLog {
        run_id: cagent_agent::protocol::AgentRunId,
        group_index: usize,
    },
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum ActivityCollapseTarget {
    Transcript(cagent_agent::protocol::TranscriptBlockId),
    AgentLog {
        run_id: cagent_agent::protocol::AgentRunId,
        group_index: usize,
    },
}

#[derive(Clone, Debug)]
pub(super) struct DisplayRow {
    pub(crate) line: Line<'static>,
    links: Vec<LinkSegment>,
    paths: Vec<PathSegment>,
    images: Vec<ImageSegment>,
    web_fetch_output: Option<WebFetchOutputTarget>,
    mcp_call: Option<Arc<cagent_agent::presentation::McpCall>>,
    compaction: Option<CompactionTarget>,
    terminal_output: Option<cagent_agent::tools::TerminalId>,
    diff_line: Option<DiffLineTarget>,
    exploration_toggle: Option<ExplorationToggleTarget>,
    activity_collapse: Option<ActivityCollapseTarget>,
}

impl DisplayRow {
    pub(crate) fn plain(line: Line<'static>) -> Self {
        Self {
            line,
            links: Vec::new(),
            paths: Vec::new(),
            images: Vec::new(),
            web_fetch_output: None,
            mcp_call: None,
            compaction: None,
            terminal_output: None,
            diff_line: None,
            exploration_toggle: None,
            activity_collapse: None,
        }
    }

    pub(crate) fn with_web_fetch_output(mut self, target: WebFetchOutputTarget) -> Self {
        self.web_fetch_output = Some(target);
        self
    }

    pub(crate) fn with_mcp_call(mut self, call: Arc<cagent_agent::presentation::McpCall>) -> Self {
        self.mcp_call = Some(call);
        self
    }

    pub(crate) fn mcp_call(&self) -> Option<&Arc<cagent_agent::presentation::McpCall>> {
        self.mcp_call.as_ref()
    }

    pub(crate) fn web_fetch_output(&self) -> Option<&WebFetchOutputTarget> {
        self.web_fetch_output.as_ref()
    }

    pub(crate) fn with_compaction(mut self, summary: impl Into<Arc<str>>) -> Self {
        self.compaction = Some(CompactionTarget {
            summary: summary.into(),
        });
        self
    }

    pub(crate) fn compaction(&self) -> Option<&CompactionTarget> {
        self.compaction.as_ref()
    }

    pub(crate) fn with_terminal_output(
        mut self,
        terminal_id: cagent_agent::tools::TerminalId,
    ) -> Self {
        self.terminal_output = Some(terminal_id);
        self
    }

    pub(crate) fn terminal_output(&self) -> Option<cagent_agent::tools::TerminalId> {
        self.terminal_output
    }

    pub(crate) fn with_diff_line(mut self, target: DiffLineTarget) -> Self {
        self.diff_line = Some(target);
        self
    }

    pub(crate) fn diff_line(&self) -> Option<&DiffLineTarget> {
        self.diff_line.as_ref()
    }

    pub(crate) fn with_exploration_toggle(mut self, target: ExplorationToggleTarget) -> Self {
        self.exploration_toggle = Some(target);
        self
    }

    pub(crate) fn exploration_toggle(&self) -> Option<&ExplorationToggleTarget> {
        self.exploration_toggle.as_ref()
    }

    pub(crate) fn with_activity_collapse(mut self, target: ActivityCollapseTarget) -> Self {
        self.activity_collapse = Some(target);
        self
    }

    pub(crate) fn activity_collapse(&self) -> Option<&ActivityCollapseTarget> {
        self.activity_collapse.as_ref()
    }

    pub(crate) fn with_path(
        mut self,
        column: usize,
        width: usize,
        path: std::path::PathBuf,
    ) -> Self {
        if width > 0 {
            self.paths.push(PathSegment {
                column,
                width,
                target: ResolvedPathTarget {
                    path,
                    line: None,
                    directory: false,
                },
            });
        }
        self
    }

    pub(crate) fn with_directory_path(
        mut self,
        column: usize,
        width: usize,
        path: std::path::PathBuf,
    ) -> Self {
        if width > 0 {
            self.paths.push(PathSegment {
                column,
                width,
                target: ResolvedPathTarget {
                    path,
                    line: None,
                    directory: true,
                },
            });
        }
        self
    }

    pub(crate) fn paths(&self) -> &[PathSegment] {
        &self.paths
    }
    pub(crate) fn with_image(
        mut self,
        column: usize,
        width: usize,
        image: cagent_agent::protocol::ImageAttachment,
    ) -> Self {
        if width > 0 {
            self.images.push(ImageSegment {
                column,
                width,
                image,
            });
        }
        self
    }
    pub(crate) fn images(&self) -> &[ImageSegment] {
        &self.images
    }
    pub(super) fn prefixed(mut self, prefix: Span<'static>) -> Self {
        let shift = prefix.content.width();
        for link in &mut self.links {
            link.column = link.column.saturating_add(shift);
        }
        for path in &mut self.paths {
            path.column = path.column.saturating_add(shift);
        }
        for image in &mut self.images {
            image.column = image.column.saturating_add(shift);
        }
        let mut spans = vec![prefix];
        spans.extend(self.line.spans);
        self.line = Line::from(spans).style(self.line.style);
        self
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ImageSegment {
    pub(crate) column: usize,
    pub(crate) width: usize,
    pub(crate) image: cagent_agent::protocol::ImageAttachment,
}

#[derive(Clone, Debug)]
pub(crate) struct PathSegment {
    pub(crate) column: usize,
    pub(crate) width: usize,
    pub(crate) target: ResolvedPathTarget,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedPathTarget {
    pub(crate) path: std::path::PathBuf,
    pub(crate) line: Option<usize>,
    pub(crate) directory: bool,
}

impl std::fmt::Display for DisplayRow {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.line.fmt(formatter)
    }
}

#[derive(Clone, Debug)]
struct LinkSegment {
    column: usize,
    text: String,
    destination: String,
}

#[derive(Clone, Debug)]
pub(super) struct HyperlinkOverlay {
    x: u16,
    y: u16,
    text: String,
    destination: String,
    background: Option<u8>,
}

impl HyperlinkOverlay {
    pub(crate) fn destination_at(&self, row: u16, column: u16) -> Option<&str> {
        let width = u16::try_from(self.text.width()).unwrap_or(u16::MAX);
        (row == self.y && (self.x..self.x.saturating_add(width)).contains(&column))
            .then_some(self.destination.as_str())
    }
}

pub(super) fn external_hyperlink_overlay(
    x: u16,
    y: u16,
    text: &str,
    destination: &str,
    available_width: u16,
    background: Option<u8>,
) -> Option<HyperlinkOverlay> {
    if (!destination.starts_with("https://") && !destination.starts_with("http://"))
        || destination.chars().any(char::is_control)
    {
        return None;
    }
    let text = truncate_display_width(text, usize::from(available_width));
    (!text.is_empty()).then(|| HyperlinkOverlay {
        x,
        y,
        text,
        destination: destination.to_owned(),
        background,
    })
}

/// Renders an unbounded document for tests and callers that do their own layout.
#[cfg(test)]
pub(super) fn render_document(document: &MarkdownDocument) -> Vec<Line<'static>> {
    layout_document(document, u16::MAX)
        .into_iter()
        .map(|row| row.line)
        .collect()
}

/// Converts the agent's semantic Markdown document into width-aware Ratatui rows.
pub(super) fn layout_document(document: &MarkdownDocument, width: u16) -> Vec<DisplayRow> {
    Renderer::new(width, None).render(document)
}

pub(super) fn layout_document_with_paths(
    document: &MarkdownDocument,
    width: u16,
    workspace: &std::path::Path,
    enabled: bool,
) -> Vec<DisplayRow> {
    Renderer::new(width, enabled.then_some(workspace)).render(document)
}

pub(super) fn hyperlink_overlays(rows: &[DisplayRow], area: Rect) -> Vec<HyperlinkOverlay> {
    let mut overlays = Vec::new();
    for (row, line) in rows.iter().enumerate() {
        let Ok(row) = u16::try_from(row) else {
            break;
        };
        if row >= area.height {
            break;
        }
        for link in &line.links {
            if link.column >= usize::from(area.width) {
                continue;
            }
            let available = usize::from(area.width).saturating_sub(link.column);
            let text = truncate_display_width(&link.text, available);
            if text.is_empty() {
                continue;
            }
            overlays.push(HyperlinkOverlay {
                x: area
                    .x
                    .saturating_add(u16::try_from(link.column).unwrap_or(u16::MAX)),
                y: area.y.saturating_add(row),
                text,
                destination: link.destination.clone(),
                background: None,
            });
        }
    }
    overlays
}

fn truncate_display_width(text: &str, width: usize) -> String {
    let mut used = 0;
    text.graphemes(true)
        .take_while(|grapheme| {
            let next = used + grapheme.width();
            if next > width {
                false
            } else {
                used = next;
                true
            }
        })
        .collect()
}

pub(super) fn render_hyperlink_overlays(overlays: &[HyperlinkOverlay]) -> io::Result<()> {
    use crossterm::{
        cursor::{MoveTo, RestorePosition, SavePosition},
        queue,
        style::Print,
    };

    let mut stdout = io::stdout();
    queue!(stdout, SavePosition)?;
    for link in overlays {
        let style = link.background.map_or_else(
            || "\x1b[36;4m".to_owned(),
            |background| format!("\x1b[36;4;48;5;{background}m"),
        );
        let text = format!(
            "{style}\x1b]8;;{}\x07{}\x1b]8;;\x07\x1b[0m",
            link.destination, link.text,
        );
        queue!(stdout, MoveTo(link.x, link.y), Print(text))?;
    }
    queue!(stdout, RestorePosition)?;
    stdout.flush()
}

#[derive(Clone)]
struct RichSpan {
    text: String,
    style: Style,
    link: Option<String>,
    path: Option<ResolvedPathTarget>,
}

impl RichSpan {
    fn styled(text: impl Into<String>, style: Style) -> Self {
        Self {
            text: text.into(),
            style,
            link: None,
            path: None,
        }
    }

    fn width(&self) -> usize {
        self.text.width()
    }
}

struct LogicalLine {
    prefix: Vec<RichSpan>,
    continuation: Vec<RichSpan>,
    body: Vec<RichSpan>,
    wrap: bool,
}

struct Renderer {
    width: usize,
    lines: Vec<LogicalLine>,
    current: Vec<RichSpan>,
    prefix: Vec<RichSpan>,
    continuation: Vec<RichSpan>,
    quote_depth: usize,
    indent: usize,
    in_code_block: bool,
    line_start: bool,
    skip_paragraph_spacing: bool,
    workspace: Option<std::path::PathBuf>,
}

impl Renderer {
    fn new(width: u16, workspace: Option<&std::path::Path>) -> Self {
        Self {
            width: usize::from(width.max(1)),
            lines: Vec::new(),
            current: Vec::new(),
            prefix: Vec::new(),
            continuation: Vec::new(),
            quote_depth: 0,
            indent: 0,
            in_code_block: false,
            line_start: true,
            skip_paragraph_spacing: false,
            workspace: workspace.map(std::path::Path::to_path_buf),
        }
    }

    fn render(mut self, document: &MarkdownDocument) -> Vec<DisplayRow> {
        self.blocks(&document.blocks);
        if !self.line_start || !self.current.is_empty() {
            self.newline();
        }
        while self.lines.last().is_some_and(|line| {
            line.body.is_empty() && line.prefix.is_empty() && line.continuation.is_empty()
        }) {
            self.lines.pop();
        }
        self.lines
            .into_iter()
            .flat_map(|line| wrap_logical_line(line, self.width))
            .collect()
    }

    fn blocks(&mut self, blocks: &[MarkdownBlock]) {
        for block in blocks {
            self.block(block);
        }
    }

    fn block(&mut self, block: &MarkdownBlock) {
        match block {
            MarkdownBlock::Paragraph(content) => {
                if self.skip_paragraph_spacing {
                    self.skip_paragraph_spacing = false;
                } else {
                    self.block_spacing();
                }
                self.inlines(content, self.content_style(), None);
                self.newline();
            }
            MarkdownBlock::Heading { level, content } => {
                self.ensure_blank_line();
                let style = heading_style(*level).patch(self.content_style());
                self.raw(
                    &format!("{} ", "#".repeat(usize::from(*level))),
                    style,
                    None,
                );
                self.inlines(content, style, None);
                self.newline();
                self.ensure_blank_line();
            }
            MarkdownBlock::BlockQuote(blocks) => {
                if self.quote_depth == 0 {
                    self.ensure_blank_line();
                } else {
                    self.ensure_quote_blank_line();
                }
                self.quote_depth += 1;
                self.skip_paragraph_spacing = true;
                self.blocks(blocks);
                self.quote_depth = self.quote_depth.saturating_sub(1);
                self.ensure_newline();
            }
            MarkdownBlock::List { start, items } => self.list(*start, items),
            MarkdownBlock::CodeBlock { tokens, .. } => {
                self.ensure_blank_line();
                self.in_code_block = true;
                for token in tokens {
                    self.text(&token.text, code_style(token.kind), None);
                }
                self.ensure_newline();
                self.in_code_block = false;
                self.ensure_blank_line();
            }
            MarkdownBlock::Table(table) => {
                self.ensure_blank_line();
                self.table(table);
                self.ensure_blank_line();
            }
            MarkdownBlock::Rule => {
                self.ensure_blank_line();
                self.raw("────────", DIM, None);
                self.newline();
                self.ensure_blank_line();
            }
        }
    }

    fn list(&mut self, start: Option<u64>, items: &[MarkdownListItem]) {
        let depth = self.indent;
        self.ensure_newline();
        for (index, item) in items.iter().enumerate() {
            if index > 0 {
                self.ensure_newline();
            }
            let marker = start.map_or_else(
                || "• ".to_owned(),
                |number| format!("{}. ", number.saturating_add(index as u64)),
            );
            let mut first = self.base_prefix(depth);
            first.push(RichSpan::styled(marker.clone(), LIST_MARKER));
            if let Some(checked) = item.checked {
                first.push(RichSpan::styled(
                    if checked { "[x] " } else { "[ ] " },
                    self.content_style(),
                ));
            }
            let mut continuation = self.base_prefix(depth);
            let marker_width = first.iter().map(RichSpan::width).sum::<usize>()
                - continuation.iter().map(RichSpan::width).sum::<usize>();
            continuation.push(RichSpan::styled(" ".repeat(marker_width), Style::default()));
            self.set_prefix(first, continuation);

            if let Some((first_block, rest)) = item.blocks.split_first() {
                if let MarkdownBlock::Paragraph(content) = first_block {
                    self.inlines(content, self.content_style(), None);
                    self.newline();
                } else {
                    self.block(first_block);
                }
                self.indent = depth + 1;
                self.blocks(rest);
                self.indent = depth;
            } else {
                self.newline();
            }
        }
        self.ensure_newline();
    }

    fn table(&mut self, table: &MarkdownTable) {
        let mut rows = Vec::with_capacity(table.rows.len() + 1);
        rows.push((true, self.collect_table_row(&table.header)));
        rows.extend(
            table
                .rows
                .iter()
                .map(|row| (false, self.collect_table_row(row))),
        );
        let columns = rows.iter().map(|(_, row)| row.len()).max().unwrap_or(0);
        if columns == 0 {
            return;
        }
        let prefix = self.base_prefix(self.indent);
        let prefix_width = rich_width(&prefix);
        let available = self.width.saturating_sub(prefix_width).max(1);
        let minimum_grid = table_width(&vec![2; columns]);
        if available < minimum_grid {
            self.stacked_table(table, &rows, prefix);
            return;
        }
        let desired = (0..columns)
            .map(|column| {
                rows.iter()
                    .filter_map(|(_, row)| row.get(column))
                    .map(|cell| rich_width(cell).max(1))
                    .max()
                    .unwrap_or(1)
            })
            .collect::<Vec<_>>();
        let widths = fit_table_widths(desired, available);
        for (header, cells) in &rows {
            self.table_grid_row(&prefix, cells, &widths, *header, &table.alignments);
            self.table_separator(&prefix, &widths);
        }
    }

    fn stacked_table(
        &mut self,
        table: &MarkdownTable,
        rows: &[(bool, Vec<Vec<RichSpan>>)],
        prefix: Vec<RichSpan>,
    ) {
        let headers = rows
            .first()
            .map(|(_, row)| row)
            .cloned()
            .unwrap_or_default();
        for (row_index, (_, cells)) in rows.iter().skip(1).enumerate() {
            if row_index > 0 {
                self.lines.push(LogicalLine {
                    prefix: prefix.clone(),
                    continuation: prefix.clone(),
                    body: Vec::new(),
                    wrap: false,
                });
            }
            for (column, cell) in cells.iter().enumerate() {
                let mut body = headers.get(column).cloned().unwrap_or_default();
                if !body.is_empty() {
                    for span in &mut body {
                        span.style = table_header_style(span.style, span.link.is_some());
                    }
                    body.push(RichSpan::styled(": ", DIM));
                }
                body.extend(cell.clone());
                self.lines.push(LogicalLine {
                    prefix: prefix.clone(),
                    continuation: prefix.clone(),
                    body,
                    wrap: true,
                });
            }
        }
        if table.rows.is_empty() {
            self.lines.push(LogicalLine {
                prefix: prefix.clone(),
                continuation: prefix,
                body: headers.into_iter().flatten().collect(),
                wrap: true,
            });
        }
        self.line_start = true;
    }

    fn table_grid_row(
        &mut self,
        prefix: &[RichSpan],
        cells: &[Vec<RichSpan>],
        widths: &[usize],
        header: bool,
        alignments: &[MarkdownAlignment],
    ) {
        let wrapped = widths
            .iter()
            .enumerate()
            .map(|(column, width)| {
                let mut cell = cells.get(column).cloned().unwrap_or_default();
                if header {
                    for span in &mut cell {
                        span.style = table_header_style(span.style, span.link.is_some());
                    }
                }
                wrap_rich_spans(cell, *width)
            })
            .collect::<Vec<_>>();
        let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
        for row in 0..height {
            let mut body = Vec::new();
            for (column, width) in widths.iter().enumerate() {
                let cell = wrapped[column].get(row).cloned().unwrap_or_default();
                let alignment = alignments
                    .get(column)
                    .copied()
                    .unwrap_or(MarkdownAlignment::None);
                body.push(RichSpan::styled(" ", TABLE_RULE));
                body.extend(aligned_cell(cell, *width, alignment));
                body.push(RichSpan::styled(" ", TABLE_RULE));
                if column + 1 < widths.len() {
                    body.push(RichSpan::styled("  ", TABLE_RULE));
                }
            }
            self.lines.push(LogicalLine {
                prefix: prefix.to_vec(),
                continuation: prefix.to_vec(),
                body,
                wrap: false,
            });
        }
        self.line_start = true;
    }

    fn table_separator(&mut self, prefix: &[RichSpan], widths: &[usize]) {
        let mut body = Vec::new();
        for (column, width) in widths.iter().enumerate() {
            body.push(RichSpan::styled(
                "─".repeat(width.saturating_add(TABLE_CELL_PADDING * 2)),
                TABLE_RULE,
            ));
            if column + 1 < widths.len() {
                body.push(RichSpan::styled("  ", TABLE_RULE));
            }
        }
        self.lines.push(LogicalLine {
            prefix: prefix.to_vec(),
            continuation: prefix.to_vec(),
            body,
            wrap: false,
        });
        self.line_start = true;
    }

    fn inlines(&mut self, inlines: &[MarkdownInline], style: Style, link: Option<&str>) {
        for inline in inlines {
            match inline {
                MarkdownInline::Text(text) => self.text(text, style, link),
                MarkdownInline::Code(code) => self.text(code, style.patch(INLINE_CODE), link),
                MarkdownInline::PathReference {
                    display,
                    path,
                    line,
                    code,
                } => {
                    let target = self.resolve_path(path, *line);
                    let style = if target.is_some() {
                        style.patch(PATH_REFERENCE)
                    } else if *code {
                        style.patch(INLINE_CODE)
                    } else {
                        style
                    };
                    self.path_text(display, style, link, target);
                }
                MarkdownInline::Emphasis(content) => self.inlines(
                    content,
                    style.patch(Style::new().add_modifier(Modifier::ITALIC)),
                    link,
                ),
                MarkdownInline::Strong(content) => self.inlines(
                    content,
                    style.patch(Style::new().add_modifier(Modifier::BOLD)),
                    link,
                ),
                MarkdownInline::Strikethrough(content) => self.inlines(
                    content,
                    style.patch(Style::new().add_modifier(Modifier::CROSSED_OUT)),
                    link,
                ),
                MarkdownInline::Link {
                    destination,
                    safe,
                    content,
                } => self.inlines(
                    content,
                    if *safe { style.patch(LINK) } else { style },
                    safe.then_some(destination.as_str()),
                ),
                MarkdownInline::SoftBreak => self.raw(" ", style, link),
                MarkdownInline::HardBreak => self.newline(),
            }
        }
    }

    fn text(&mut self, value: &str, style: Style, link: Option<&str>) {
        let safe = crate::render::terminal_safe(value);
        for (index, part) in safe.split('\n').enumerate() {
            if index > 0 {
                self.newline();
            }
            if !part.is_empty() {
                self.raw(part, style, link);
            }
        }
    }

    fn raw(&mut self, value: &str, style: Style, link: Option<&str>) {
        self.raw_with_path(value, style, link, None);
    }

    fn path_text(
        &mut self,
        value: &str,
        style: Style,
        link: Option<&str>,
        path: Option<ResolvedPathTarget>,
    ) {
        let safe = crate::render::terminal_safe(value);
        self.raw_with_path(&safe, style, link, path);
    }

    fn raw_with_path(
        &mut self,
        value: &str,
        style: Style,
        link: Option<&str>,
        path: Option<ResolvedPathTarget>,
    ) {
        self.ensure_prefix();
        if value.is_empty() {
            return;
        }
        if let Some(last) = self.current.last_mut()
            && last.style == style
            && last.link.as_deref() == link
            && last.path == path
        {
            last.text.push_str(value);
        } else {
            self.current.push(RichSpan {
                text: value.to_owned(),
                style,
                link: link.map(str::to_owned),
                path,
            });
        }
        self.line_start = false;
    }

    fn set_prefix(&mut self, prefix: Vec<RichSpan>, continuation: Vec<RichSpan>) {
        self.prefix = prefix;
        self.continuation = continuation;
    }

    fn ensure_prefix(&mut self) {
        if self.prefix.is_empty() && self.continuation.is_empty() {
            self.prefix = self.base_prefix(self.indent);
            self.continuation = self.prefix.clone();
        }
    }

    fn base_prefix(&self, indent: usize) -> Vec<RichSpan> {
        let mut prefix = Vec::new();
        if self.quote_depth > 0 {
            prefix.push(RichSpan::styled("┃ ".repeat(self.quote_depth), QUOTE_BAR));
        }
        if indent > 0 {
            prefix.push(RichSpan::styled("  ".repeat(indent), Style::default()));
        }
        if self.in_code_block {
            prefix.push(RichSpan::styled("│ ", CODE_BAR));
        }
        prefix
    }

    fn content_style(&self) -> Style {
        if self.quote_depth > 0 {
            QUOTE_TEXT
        } else {
            Style::default()
        }
    }

    fn block_spacing(&mut self) {
        if self.quote_depth == 0 {
            self.ensure_blank_line();
        } else if !self.lines.is_empty() && self.line_start {
            self.ensure_quote_blank_line();
        }
    }

    fn ensure_newline(&mut self) {
        if !self.line_start || !self.current.is_empty() {
            self.newline();
        }
    }

    fn ensure_blank_line(&mut self) {
        self.ensure_newline();
        if !self.lines.is_empty() && !self.last_line_blank() {
            self.lines.push(LogicalLine {
                prefix: self.base_prefix(self.indent),
                continuation: self.base_prefix(self.indent),
                body: Vec::new(),
                wrap: false,
            });
        }
    }

    fn ensure_quote_blank_line(&mut self) {
        self.ensure_newline();
        let prefix = self.base_prefix(self.indent);
        if !self.lines.last().is_some_and(|line| {
            line.body.is_empty() && rich_text(&line.prefix) == rich_text(&prefix)
        }) {
            self.lines.push(LogicalLine {
                prefix: prefix.clone(),
                continuation: prefix,
                body: Vec::new(),
                wrap: false,
            });
        }
    }

    fn last_line_blank(&self) -> bool {
        self.lines
            .last()
            .is_some_and(|line| line.body.is_empty() && line.prefix.is_empty())
    }

    fn newline(&mut self) {
        self.ensure_prefix();
        self.lines.push(LogicalLine {
            prefix: std::mem::take(&mut self.prefix),
            continuation: std::mem::take(&mut self.continuation),
            body: std::mem::take(&mut self.current),
            wrap: true,
        });
        self.line_start = true;
    }

    fn resolve_path(&self, path: &str, line: Option<usize>) -> Option<ResolvedPathTarget> {
        let workspace = self.workspace.as_deref()?;
        let path = cagent_agent::presentation::resolve_display_path(path, workspace);
        if !path.exists() || line.is_some() && !path.is_file() {
            return None;
        }
        Some(ResolvedPathTarget {
            path,
            line,
            directory: false,
        })
    }

    fn collect_table_row(&self, row: &MarkdownTableRow) -> Vec<Vec<RichSpan>> {
        row.cells
            .iter()
            .map(|cell| self.collect_table_cell(cell))
            .collect()
    }

    fn collect_table_cell(&self, cell: &MarkdownTableCell) -> Vec<RichSpan> {
        let mut spans = Vec::new();
        collect_inlines(
            &cell.content,
            Style::default(),
            None,
            self.workspace.as_deref(),
            &mut spans,
        );
        spans
    }
}

fn collect_inlines(
    inlines: &[MarkdownInline],
    style: Style,
    link: Option<&str>,
    workspace: Option<&std::path::Path>,
    output: &mut Vec<RichSpan>,
) {
    for inline in inlines {
        match inline {
            MarkdownInline::Text(text) => push_collected(text, style, link, output),
            MarkdownInline::Code(code) => {
                push_collected(code, style.patch(INLINE_CODE), link, output);
            }
            MarkdownInline::PathReference {
                display,
                path,
                line,
                code,
            } => {
                let target = workspace.and_then(|workspace| {
                    let path = cagent_agent::presentation::resolve_display_path(path, workspace);
                    (path.exists() && (line.is_none() || path.is_file())).then_some(
                        ResolvedPathTarget {
                            path,
                            line: *line,
                            directory: false,
                        },
                    )
                });
                let path_style = if target.is_some() {
                    style.patch(PATH_REFERENCE)
                } else if *code {
                    style.patch(INLINE_CODE)
                } else {
                    style
                };
                push_collected_with_path(display, path_style, link, target, output);
            }
            MarkdownInline::Emphasis(content) => collect_inlines(
                content,
                style.patch(Style::new().add_modifier(Modifier::ITALIC)),
                link,
                workspace,
                output,
            ),
            MarkdownInline::Strong(content) => collect_inlines(
                content,
                style.patch(Style::new().add_modifier(Modifier::BOLD)),
                link,
                workspace,
                output,
            ),
            MarkdownInline::Strikethrough(content) => collect_inlines(
                content,
                style.patch(Style::new().add_modifier(Modifier::CROSSED_OUT)),
                link,
                workspace,
                output,
            ),
            MarkdownInline::Link {
                destination,
                safe,
                content,
            } => collect_inlines(
                content,
                if *safe { style.patch(LINK) } else { style },
                safe.then_some(destination.as_str()),
                workspace,
                output,
            ),
            MarkdownInline::SoftBreak | MarkdownInline::HardBreak => {
                push_collected(" ", style, link, output);
            }
        }
    }
}

fn push_collected(value: &str, style: Style, link: Option<&str>, output: &mut Vec<RichSpan>) {
    push_collected_with_path(value, style, link, None, output);
}

fn push_collected_with_path(
    value: &str,
    style: Style,
    link: Option<&str>,
    path: Option<ResolvedPathTarget>,
    output: &mut Vec<RichSpan>,
) {
    let value = crate::render::terminal_safe(value).replace('\n', " ");
    if let Some(last) = output.last_mut()
        && last.style == style
        && last.link.as_deref() == link
        && last.path == path
    {
        last.text.push_str(&value);
    } else if !value.is_empty() {
        output.push(RichSpan {
            text: value,
            style,
            link: link.map(str::to_owned),
            path,
        });
    }
}

fn wrap_logical_line(line: LogicalLine, width: usize) -> Vec<DisplayRow> {
    if !line.wrap {
        return vec![finish_row(
            line.prefix.into_iter().chain(line.body).collect(),
        )];
    }
    let graphemes = explode_spans(line.body);
    if graphemes.is_empty() {
        return vec![finish_row(line.prefix)];
    }
    let mut rows = Vec::new();
    let mut start = 0;
    let mut first = true;
    while start < graphemes.len() {
        let prefix = if first {
            line.prefix.clone()
        } else {
            line.continuation.clone()
        };
        let available = width.saturating_sub(rich_width(&prefix)).max(1);
        let (end, next) = line_break(&graphemes, start, available);
        let mut spans = prefix;
        spans.extend(collapse_graphemes(&graphemes[start..end]));
        rows.push(finish_row(spans));
        start = next;
        first = false;
    }
    rows
}

#[derive(Clone)]
struct RichGrapheme {
    text: String,
    style: Style,
    link: Option<String>,
    path: Option<ResolvedPathTarget>,
}

fn explode_spans(spans: Vec<RichSpan>) -> Vec<RichGrapheme> {
    spans
        .into_iter()
        .flat_map(|span| {
            let style = span.style;
            let link = span.link;
            span.text
                .graphemes(true)
                .map(str::to_owned)
                .collect::<Vec<_>>()
                .into_iter()
                .map(move |text| RichGrapheme {
                    text,
                    style,
                    link: link.clone(),
                    path: span.path.clone(),
                })
        })
        .collect()
}

fn line_break(graphemes: &[RichGrapheme], start: usize, width: usize) -> (usize, usize) {
    let mut end = start;
    let mut used = 0;
    let mut last_break = None;
    while end < graphemes.len() {
        let grapheme_width = graphemes[end].text.width();
        if used > 0 && used + grapheme_width > width {
            break;
        }
        used += grapheme_width;
        if graphemes[end].text.chars().all(char::is_whitespace) {
            last_break = Some(end);
        }
        end += 1;
    }
    if end < graphemes.len()
        && let Some(break_at) = last_break.filter(|break_at| *break_at > start)
    {
        let mut next = break_at + 1;
        while next < graphemes.len() && graphemes[next].text.chars().all(char::is_whitespace) {
            next += 1;
        }
        (break_at, next)
    } else {
        (
            end.max(start + 1).min(graphemes.len()),
            end.max(start + 1).min(graphemes.len()),
        )
    }
}

fn collapse_graphemes(graphemes: &[RichGrapheme]) -> Vec<RichSpan> {
    let mut spans: Vec<RichSpan> = Vec::new();
    for grapheme in graphemes {
        if let Some(last) = spans.last_mut()
            && last.style == grapheme.style
            && last.link == grapheme.link
            && last.path == grapheme.path
        {
            last.text.push_str(&grapheme.text);
        } else {
            spans.push(RichSpan {
                text: grapheme.text.clone(),
                style: grapheme.style,
                link: grapheme.link.clone(),
                path: grapheme.path.clone(),
            });
        }
    }
    spans
}

fn finish_row(spans: Vec<RichSpan>) -> DisplayRow {
    let mut column = 0;
    let mut links: Vec<LinkSegment> = Vec::new();
    let mut paths: Vec<PathSegment> = Vec::new();
    let mut ratatui = Vec::with_capacity(spans.len());
    for span in spans {
        if let Some(destination) = span.link {
            if let Some(last) = links.last_mut()
                && last.destination == destination
                && last.column + last.text.width() == column
            {
                last.text.push_str(&span.text);
            } else {
                links.push(LinkSegment {
                    column,
                    text: span.text.clone(),
                    destination,
                });
            }
        }
        if let Some(target) = span.path {
            let width = span.text.width();
            if let Some(last) = paths.last_mut()
                && last.target == target
                && last.column + last.width == column
            {
                last.width = last.width.saturating_add(width);
            } else {
                paths.push(PathSegment {
                    column,
                    width,
                    target,
                });
            }
        }
        column = column.saturating_add(span.text.width());
        ratatui.push(Span::styled(span.text, span.style));
    }
    DisplayRow {
        line: Line::from(ratatui),
        links,
        paths,
        images: Vec::new(),
        web_fetch_output: None,
        mcp_call: None,
        compaction: None,
        terminal_output: None,
        diff_line: None,
        exploration_toggle: None,
        activity_collapse: None,
    }
}

fn wrap_rich_spans(spans: Vec<RichSpan>, width: usize) -> Vec<Vec<RichSpan>> {
    let graphemes = explode_spans(spans);
    if graphemes.is_empty() {
        return vec![Vec::new()];
    }
    let mut rows = Vec::new();
    let mut start = 0;
    while start < graphemes.len() {
        let (end, next) = line_break(&graphemes, start, width.max(1));
        rows.push(collapse_graphemes(&graphemes[start..end]));
        start = next;
    }
    rows
}

fn aligned_cell(cell: Vec<RichSpan>, width: usize, alignment: MarkdownAlignment) -> Vec<RichSpan> {
    let padding = width.saturating_sub(rich_width(&cell));
    let (left, right) = match alignment {
        MarkdownAlignment::Right => (padding, 0),
        MarkdownAlignment::Center => (padding / 2, padding - padding / 2),
        MarkdownAlignment::None | MarkdownAlignment::Left => (0, padding),
    };
    let mut output = Vec::new();
    if left > 0 {
        output.push(RichSpan::styled(" ".repeat(left), DIM));
    }
    output.extend(cell);
    if right > 0 {
        output.push(RichSpan::styled(" ".repeat(right), DIM));
    }
    output
}

fn table_header_style(style: Style, link: bool) -> Style {
    if link {
        style.add_modifier(Modifier::BOLD)
    } else {
        style.patch(TABLE_HEADER)
    }
}

fn table_width(widths: &[usize]) -> usize {
    widths
        .iter()
        .sum::<usize>()
        .saturating_add(widths.len().saturating_mul(TABLE_CELL_PADDING * 2))
        .saturating_add(
            widths
                .len()
                .saturating_sub(1)
                .saturating_mul(TABLE_COLUMN_GAP),
        )
}

fn fit_table_widths(mut widths: Vec<usize>, total: usize) -> Vec<usize> {
    while table_width(&widths) > total {
        let Some((index, _)) = widths
            .iter()
            .enumerate()
            .filter(|(_, width)| **width > 1)
            .max_by_key(|(_, width)| **width)
        else {
            break;
        };
        widths[index] -= 1;
    }
    widths
}

fn rich_width(spans: &[RichSpan]) -> usize {
    spans.iter().map(RichSpan::width).sum()
}

fn rich_text(spans: &[RichSpan]) -> String {
    spans.iter().map(|span| span.text.as_str()).collect()
}

pub(crate) fn code_style(kind: CodeTokenKind) -> Style {
    match kind {
        CodeTokenKind::Plain => CODE_PLAIN,
        CodeTokenKind::Comment => CODE_COMMENT,
        CodeTokenKind::String => CODE_STRING,
        CodeTokenKind::Number | CodeTokenKind::Constant => CODE_NUMBER,
        CodeTokenKind::Keyword => CODE_KEYWORD,
        CodeTokenKind::Type => CODE_TYPE,
        CodeTokenKind::Function => CODE_FUNCTION,
        CodeTokenKind::Macro => CODE_MACRO,
        CodeTokenKind::Attribute => CODE_ATTRIBUTE,
        CodeTokenKind::Operator => CODE_OPERATOR,
        CodeTokenKind::MarkupHeading => CODE_MARKUP_HEADING,
        CodeTokenKind::MarkupStrong => CODE_MARKUP_STRONG,
        CodeTokenKind::MarkupEmphasis => CODE_MARKUP_EMPHASIS,
        CodeTokenKind::MarkupLink => CODE_MARKUP_LINK,
        CodeTokenKind::MarkupRaw => CODE_MARKUP_RAW,
    }
}

fn heading_style(level: u8) -> Style {
    match level {
        1 => Style::new()
            .add_modifier(Modifier::BOLD)
            .add_modifier(Modifier::UNDERLINED),
        2 => Style::new().add_modifier(Modifier::BOLD),
        3 => Style::new()
            .add_modifier(Modifier::BOLD)
            .add_modifier(Modifier::ITALIC),
        _ => Style::new().add_modifier(Modifier::ITALIC),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_link_overlay_preserves_the_requested_background() {
        let overlay = external_hyperlink_overlay(
            7,
            3,
            "https://example.com/device",
            "https://example.com/device",
            80,
            Some(236),
        )
        .unwrap();
        assert_eq!(overlay.background, Some(236));
    }

    fn lines(source: &str) -> Vec<Line<'static>> {
        render_document(&cagent_agent::presentation::parse_markdown(source))
    }

    #[test]
    fn existing_path_references_keep_targets_across_wrapping_lists_quotes_and_tables() {
        let temporary = tempfile::tempdir().unwrap();
        let file = temporary.path().join("path with spaces.rs");
        std::fs::write(&file, "one\ntwo\nthree\n").unwrap();
        let document = cagent_agent::presentation::parse_markdown(
            "- @{path with spaces.rs}:2:8 in a list that wraps\n\n> @{path with spaces.rs}:3\n\n| file |\n| --- |\n| @{path with spaces.rs}:1-2 |",
        );
        let rows = layout_document_with_paths(&document, 18, temporary.path(), true);
        let targets = rows
            .iter()
            .flat_map(DisplayRow::paths)
            .map(|segment| segment.target.clone())
            .collect::<Vec<_>>();
        assert!(
            targets
                .iter()
                .any(|target| target.path == file && target.line == Some(1))
        );
        assert!(
            targets
                .iter()
                .any(|target| target.path == file && target.line == Some(2)),
            "{targets:?}"
        );
        assert!(
            targets
                .iter()
                .any(|target| target.path == file && target.line == Some(3))
        );
    }

    #[test]
    fn disabled_missing_and_directory_line_references_are_plain_text() {
        let temporary = tempfile::tempdir().unwrap();
        std::fs::create_dir(temporary.path().join("folder")).unwrap();
        let document = cagent_agent::presentation::parse_markdown("@missing.rs @folder:2 @folder");
        let enabled = layout_document_with_paths(&document, 80, temporary.path(), true);
        assert_eq!(enabled.iter().flat_map(DisplayRow::paths).count(), 1);
        let rendered = enabled
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(rendered, "missing.rs folder:2 folder");
        let disabled = layout_document_with_paths(&document, 80, temporary.path(), false);
        assert_eq!(disabled.iter().flat_map(DisplayRow::paths).count(), 0);
    }

    fn text(source: &str) -> Vec<String> {
        lines(source).iter().map(ToString::to_string).collect()
    }

    #[test]
    fn renders_structure_and_styles_without_terminal_sequences() {
        let rendered = lines("# Heading\n\n- `code`\n\n> quote\n\n```rs\nfn main() {}\n```");
        let content = rendered.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert!(content.contains(&"# Heading".to_owned()));
        assert!(content.iter().any(|line| line.contains("• code")));
        assert!(content.iter().any(|line| line.contains("┃ quote")));
        assert!(rendered.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.style.fg == Some(Color::Indexed(141)))
        }));
        assert!(content.iter().all(|line| !line.contains('\x1b')));
    }

    #[test]
    fn nested_inline_styles_do_not_leak() {
        let rendered = lines("plain **bold *italic*** plain");
        let line = &rendered[0];
        assert!(!line.spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert!(line.spans.iter().any(|span| {
            span.content.contains("bold") && span.style.add_modifier.contains(Modifier::BOLD)
        }));
        assert!(
            !line
                .spans
                .last()
                .unwrap()
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
    }

    #[test]
    fn headings_paragraphs_and_soft_breaks_keep_spacing() {
        assert_eq!(
            text("before\n\n# Heading\n\nafter"),
            vec!["before", "", "# Heading", "", "after"]
        );
        assert_eq!(text("first\nsecond"), vec!["first second"]);
    }

    #[test]
    fn task_lists_and_nested_quotes_are_structured() {
        let rendered = text("- [x] done\n- [ ] todo\n\n> outer\n> > nested");
        assert!(rendered.iter().any(|line| line.contains("• [x] done")));
        assert!(rendered.iter().any(|line| line.contains("• [ ] todo")));
        assert!(rendered.iter().any(|line| line.contains("┃ ┃ nested")));
    }

    #[test]
    fn list_markers_use_the_dim_style() {
        let document = cagent_agent::presentation::parse_markdown("- item");
        let rows = layout_document(&document, 80);

        assert_eq!(rows[0].line.spans[0].style, DIM);
    }

    #[test]
    fn list_items_stay_compact_at_every_depth() {
        assert_eq!(
            text(
                "- `config.toml` — main configuration:\n  - Default model: `openai/gpt-5.6-luna`\n  - Default effort: `none`\n  - OpenAI provider enabled; ChatGPT provider disabled\n- `permissions.toml` — grants this project read access to `~/.config/cagent/**`, including external reads.\n- `config.toml.lock` — currently empty.",
            ),
            vec![
                "• config.toml — main configuration:",
                "  • Default model: openai/gpt-5.6-luna",
                "  • Default effort: none",
                "  • OpenAI provider enabled; ChatGPT provider disabled",
                "• permissions.toml — grants this project read access to ~/.config/cagent/**, including external reads.",
                "• config.toml.lock — currently empty.",
            ]
        );
    }

    #[test]
    fn list_attaches_to_intro_and_keeps_inline_bold_on_the_item_row() {
        let document = cagent_agent::presentation::parse_markdown(
            "Verification:\n- Markdown tests: **17 passed**\n- Formatting check: **passed**\n- Full Ratatui suite: **501 passed, 3 unrelated tests failed**.",
        );
        let rows = layout_document(&document, 80);
        let rendered = rows.iter().map(ToString::to_string).collect::<Vec<_>>();

        assert_eq!(
            rendered,
            vec![
                "Verification:",
                "• Markdown tests: 17 passed",
                "• Formatting check: passed",
                "• Full Ratatui suite: 501 passed, 3 unrelated tests failed.",
            ]
        );
        for row in &rows[1..] {
            assert!(row.line.spans.iter().any(|span| {
                span.style.add_modifier.contains(Modifier::BOLD)
                    && (span.content.contains("passed") || span.content.contains("501"))
            }));
        }
    }

    #[test]
    fn code_blocks_have_a_dim_gutter_that_repeats_when_wrapped() {
        let document = cagent_agent::presentation::parse_markdown("```text\nabcdefghij\nxy\n```");
        let rows = layout_document(&document, 6);
        let rendered = rows.iter().map(ToString::to_string).collect::<Vec<_>>();

        assert_eq!(rendered, vec!["│ abcd", "│ efgh", "│ ij", "│ xy"]);
        assert!(rows.iter().all(|row| row.line.width() <= 6));
        assert!(rows.iter().all(|row| row.line.spans[0].style == CODE_BAR));
    }

    #[test]
    fn quote_blank_lines_and_nested_depth_remain_visible() {
        assert_eq!(text("> aa\n>\n> b"), vec!["┃ aa", "┃ ", "┃ b"]);
        let rendered = text("> outer\n>\n> > inner\n> >\n> > > deep\n> >\n> > back\n>\n> end");
        assert_eq!(
            rendered,
            vec![
                "┃ outer",
                "┃ ",
                "┃ ┃ inner",
                "┃ ┃ ",
                "┃ ┃ ┃ deep",
                "┃ ┃ ",
                "┃ ┃ back",
                "┃ ",
                "┃ end",
            ]
        );
    }

    #[test]
    fn quoted_tables_keep_the_quote_gutter() {
        let rendered = text("> intro\n>\n> | Name | Value |\n> | --- | --- |\n> | A | B |");
        assert!(rendered.iter().any(|line| line.starts_with("┃ ─")));
        assert!(
            rendered
                .iter()
                .filter(|line| line.contains('─'))
                .all(|line| line.starts_with("┃ "))
        );
    }

    #[test]
    fn tables_preserve_alignment_styles_and_links() {
        let document = cagent_agent::presentation::parse_markdown(
            "| left | right |\n| :--- | ---: |\n| **bold** | [site](https://example.com) |",
        );
        let rows = layout_document(&document, 40);
        assert_eq!(
            rows.iter()
                .filter(|row| row.line.to_string().contains('─'))
                .count(),
            2
        );
        assert!(rows.iter().all(|row| {
            let text = row.line.to_string();
            !text.contains('┌')
                && !text.contains('┬')
                && !text.contains('┐')
                && !text.contains('├')
                && !text.contains('┼')
                && !text.contains('┤')
                && !text.contains('└')
                && !text.contains('┴')
                && !text.contains('┘')
                && !text.contains('│')
        }));
        let header = rows[0]
            .line
            .spans
            .iter()
            .find(|span| span.content.contains("left"))
            .expect("table header span");
        assert_eq!(header.style.fg, Some(Color::LightYellow));
        assert!(header.style.add_modifier.contains(Modifier::BOLD));
        assert!(rows.iter().any(|row| row.line.to_string().contains("bold")));
        let overlays = hyperlink_overlays(&rows, Rect::new(0, 0, 40, 20));
        assert_eq!(overlays.len(), 1);
        assert_eq!(overlays[0].text, "site");
        assert_eq!(overlays[0].destination, "https://example.com");
    }

    #[test]
    fn narrow_tables_use_a_bounded_stacked_layout() {
        let document = cagent_agent::presentation::parse_markdown(
            "| Name | Description |\n| --- | --- |\n| Alpha | deliberately long content |",
        );
        let rows = layout_document(&document, 8);
        assert!(rows.iter().all(|row| row.line.width() <= 8));
        assert!(
            rows.iter()
                .any(|row| row.line.to_string().contains("Name:"))
        );
        assert!(
            rows.iter()
                .map(|row| row.line.to_string())
                .collect::<String>()
                .contains("Description:")
        );
    }

    #[test]
    fn link_hits_use_clipped_display_cells_and_screen_offsets() {
        let document =
            cagent_agent::presentation::parse_markdown("x [界e\u{301}long](https://example.com)");
        let rows = layout_document(&document, 80);
        let overlays = hyperlink_overlays(&rows, Rect::new(10, 5, 5, 1));
        assert_eq!(overlays.len(), 1);
        let hit = &overlays[0];
        assert_eq!(hit.text, "界e\u{301}");
        for column in 12..15 {
            assert_eq!(hit.destination_at(5, column), Some("https://example.com"));
        }
        assert_eq!(hit.destination_at(5, 11), None);
        assert_eq!(hit.destination_at(5, 15), None);
        assert_eq!(hit.destination_at(6, 12), None);
    }

    #[test]
    fn wrapped_links_keep_explicit_metadata_on_each_row() {
        let document = cagent_agent::presentation::parse_markdown(
            "[a deliberately long link label](https://example.com)",
        );
        let rows = layout_document(&document, 10);
        assert!(rows.len() > 1);
        let overlays = hyperlink_overlays(&rows, Rect::new(0, 0, 10, 10));
        assert_eq!(overlays.len(), rows.len());
        assert!(
            overlays
                .iter()
                .all(|link| link.destination == "https://example.com")
        );
        for link in overlays {
            assert_eq!(
                link.destination_at(link.y, link.x),
                Some("https://example.com")
            );
        }
    }

    #[test]
    fn bare_web_urls_have_clickable_metadata() {
        let document = cagent_agent::presentation::parse_markdown(
            "Return exactly this Markdown: https://example.com",
        );
        let rows = layout_document(&document, 80);
        let overlays = hyperlink_overlays(&rows, Rect::new(0, 0, 80, 4));
        assert_eq!(overlays.len(), 1);
        assert_eq!(overlays[0].text, "https://example.com");
        assert_eq!(overlays[0].destination, "https://example.com");
    }

    #[test]
    fn unsafe_links_and_controls_are_never_emitted_as_terminal_metadata() {
        let document =
            cagent_agent::presentation::parse_markdown("[bad\u{1b}](javascript:alert(1))");
        let rows = layout_document(&document, 40);
        assert!(hyperlink_overlays(&rows, Rect::new(0, 0, 40, 4)).is_empty());
        assert!(rows[0].line.to_string().contains("\\u{1b}"));
        assert!(!rows[0].line.to_string().contains('\u{1b}'));
    }
}
