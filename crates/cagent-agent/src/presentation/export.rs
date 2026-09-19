use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::{
    MarkdownAlignment, MarkdownBlock, MarkdownDocument, MarkdownInline, MarkdownListItem,
    MarkdownTable, parse_markdown,
};

const TABLE_WIDTH: usize = 100;

/// A chat application's Markdown-compatible text dialect.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownDialect {
    Slack,
    Discord,
}

/// Prepares assistant Markdown for `/copy` in the selected output dialect.
#[must_use]
pub fn copy_markdown(source: &str, dialect: Option<MarkdownDialect>) -> String {
    dialect.map_or_else(
        || super::strip_path_reference_markers(source),
        |dialect| convert_markdown(source, dialect),
    )
}

/// Converts standard Markdown into text suitable for pasting into a chat application.
#[must_use]
pub fn convert_markdown(source: &str, dialect: MarkdownDialect) -> String {
    let source = super::strip_path_reference_markers(source);
    Renderer { dialect }.document(&parse_markdown(&source))
}

#[derive(Clone, Copy, Default)]
struct InlineStyle {
    strong: bool,
    emphasis: bool,
    strikethrough: bool,
}

struct Renderer {
    dialect: MarkdownDialect,
}

impl Renderer {
    fn document(&self, document: &MarkdownDocument) -> String {
        self.blocks(&document.blocks).join("\n")
    }

    fn blocks(&self, blocks: &[MarkdownBlock]) -> Vec<String> {
        let mut lines = Vec::new();
        for block in blocks {
            if !lines.is_empty() && lines.last().is_some_and(|line: &String| !line.is_empty()) {
                lines.push(String::new());
            }
            lines.extend(self.block(block));
        }
        while lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        lines
    }

    fn block(&self, block: &MarkdownBlock) -> Vec<String> {
        match block {
            MarkdownBlock::Paragraph(content) => {
                split_lines(&self.inlines(content, InlineStyle::default()))
            }
            MarkdownBlock::Heading { content, .. } => {
                let style = InlineStyle {
                    strong: true,
                    ..InlineStyle::default()
                };
                let content = self.inlines(content, style);
                let marker = self.strong_marker();
                split_lines(&content)
                    .into_iter()
                    .map(|line| format!("{marker}{line}{marker}"))
                    .collect()
            }
            MarkdownBlock::BlockQuote(blocks) => self
                .blocks(blocks)
                .into_iter()
                .map(|line| {
                    if line.is_empty() {
                        ">".to_owned()
                    } else {
                        format!("> {line}")
                    }
                })
                .collect(),
            MarkdownBlock::List { start, items } => self.list(*start, items),
            MarkdownBlock::CodeBlock { language, tokens } => {
                let source = tokens
                    .iter()
                    .map(|token| token.text.as_str())
                    .collect::<String>();
                let opener = match self.dialect {
                    MarkdownDialect::Slack => "```".to_owned(),
                    MarkdownDialect::Discord => language
                        .as_deref()
                        .map_or_else(|| "```".to_owned(), |language| format!("```{language}")),
                };
                let mut lines = vec![opener];
                lines.extend(source.trim_end_matches('\n').split('\n').map(str::to_owned));
                lines.push("```".to_owned());
                lines
            }
            MarkdownBlock::Table(table) => render_table(table),
            MarkdownBlock::Rule => vec!["---".to_owned()],
        }
    }

    fn list(&self, start: Option<u64>, items: &[MarkdownListItem]) -> Vec<String> {
        let mut output = Vec::new();
        for (index, item) in items.iter().enumerate() {
            let mut marker = match start {
                Some(start) => format!("{}. ", start.saturating_add(index as u64)),
                None if self.dialect == MarkdownDialect::Slack => "• ".to_owned(),
                None => "- ".to_owned(),
            };
            if let Some(checked) = item.checked {
                marker.push_str(if checked { "[x] " } else { "[ ] " });
            }
            let indent = " ".repeat(marker.width());
            let lines = self.blocks(&item.blocks);
            if lines.is_empty() {
                output.push(marker.trim_end().to_owned());
                continue;
            }
            for (line_index, line) in lines.into_iter().enumerate() {
                if line_index == 0 {
                    output.push(format!("{marker}{line}"));
                } else if line.is_empty() {
                    output.push(String::new());
                } else {
                    output.push(format!("{indent}{line}"));
                }
            }
        }
        output
    }

    fn inlines(&self, inlines: &[MarkdownInline], style: InlineStyle) -> String {
        let mut output = String::new();
        for inline in inlines {
            match inline {
                MarkdownInline::Text(text) => output.push_str(&self.text(text)),
                MarkdownInline::PathReference { display, .. } => {
                    output.push_str(&self.text(display));
                }
                MarkdownInline::Code(code) => {
                    output.push('`');
                    output.push_str(&code.replace('\n', " "));
                    output.push('`');
                }
                MarkdownInline::Emphasis(content) => {
                    let nested = InlineStyle {
                        emphasis: true,
                        ..style
                    };
                    if style.emphasis {
                        output.push_str(&self.inlines(content, nested));
                    } else {
                        let marker = Self::emphasis_marker();
                        output.push_str(marker);
                        output.push_str(&self.inlines(content, nested));
                        output.push_str(marker);
                    }
                }
                MarkdownInline::Strong(content) => {
                    let nested = InlineStyle {
                        strong: true,
                        ..style
                    };
                    if style.strong {
                        output.push_str(&self.inlines(content, nested));
                    } else {
                        let marker = self.strong_marker();
                        output.push_str(marker);
                        output.push_str(&self.inlines(content, nested));
                        output.push_str(marker);
                    }
                }
                MarkdownInline::Strikethrough(content) => {
                    let nested = InlineStyle {
                        strikethrough: true,
                        ..style
                    };
                    if style.strikethrough {
                        output.push_str(&self.inlines(content, nested));
                    } else {
                        let marker = self.strikethrough_marker();
                        output.push_str(marker);
                        output.push_str(&self.inlines(content, nested));
                        output.push_str(marker);
                    }
                }
                MarkdownInline::Link {
                    destination,
                    safe,
                    content,
                } => {
                    if !safe {
                        output.push_str(&self.inlines(content, style));
                    } else if self.dialect == MarkdownDialect::Slack {
                        let label = plain_inlines(content);
                        output.push('<');
                        output.push_str(&escape_slack_url(destination));
                        output.push('|');
                        output.push_str(&escape_slack_link_label(&label));
                        output.push('>');
                    } else {
                        output.push('[');
                        output.push_str(&self.inlines(content, style));
                        output.push_str("](");
                        output.push_str(destination);
                        output.push(')');
                    }
                }
                MarkdownInline::SoftBreak | MarkdownInline::HardBreak => output.push('\n'),
            }
        }
        output
    }

    fn text(&self, text: &str) -> String {
        match self.dialect {
            MarkdownDialect::Slack => escape_slack_text(text),
            MarkdownDialect::Discord => escape_discord_text(text),
        }
    }

    const fn strong_marker(&self) -> &'static str {
        match self.dialect {
            MarkdownDialect::Slack => "*",
            MarkdownDialect::Discord => "**",
        }
    }

    const fn emphasis_marker() -> &'static str {
        "_"
    }

    const fn strikethrough_marker(&self) -> &'static str {
        match self.dialect {
            MarkdownDialect::Slack => "~",
            MarkdownDialect::Discord => "~~",
        }
    }
}

fn split_lines(value: &str) -> Vec<String> {
    value.split('\n').map(str::to_owned).collect()
}

fn escape_slack_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_slack_url(value: &str) -> String {
    escape_slack_text(value).replace('|', "%7C")
}

fn escape_slack_link_label(value: &str) -> String {
    escape_slack_text(value).replace('|', "&#124;")
}

fn escape_discord_text(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for character in value.chars() {
        if matches!(character, '\\' | '*' | '_' | '~' | '`' | '[' | ']') {
            output.push('\\');
        }
        output.push(character);
    }
    output
}

fn plain_inlines(inlines: &[MarkdownInline]) -> String {
    let mut output = String::new();
    for inline in inlines {
        match inline {
            MarkdownInline::Text(text) | MarkdownInline::Code(text) => output.push_str(text),
            MarkdownInline::PathReference { display, .. } => output.push_str(display),
            MarkdownInline::Emphasis(content)
            | MarkdownInline::Strong(content)
            | MarkdownInline::Strikethrough(content)
            | MarkdownInline::Link { content, .. } => output.push_str(&plain_inlines(content)),
            MarkdownInline::SoftBreak | MarkdownInline::HardBreak => output.push(' '),
        }
    }
    output
}

fn render_table(table: &MarkdownTable) -> Vec<String> {
    let mut rows = Vec::with_capacity(table.rows.len() + 1);
    rows.push(
        table
            .header
            .cells
            .iter()
            .map(|cell| table_cell_text(&cell.content))
            .collect::<Vec<_>>(),
    );
    rows.extend(table.rows.iter().map(|row| {
        row.cells
            .iter()
            .map(|cell| table_cell_text(&cell.content))
            .collect::<Vec<_>>()
    }));
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    if columns == 0 {
        return Vec::new();
    }

    let natural = (0..columns)
        .map(|column| {
            rows.iter()
                .filter_map(|row| row.get(column))
                .map(|cell| cell.width().max(1))
                .max()
                .unwrap_or(1)
        })
        .collect::<Vec<_>>();
    let minimum = (0..columns)
        .map(|column| {
            rows.iter()
                .filter_map(|row| row.get(column))
                .flat_map(|cell| cell.graphemes(true))
                .map(UnicodeWidthStr::width)
                .max()
                .unwrap_or(1)
                .max(1)
        })
        .collect::<Vec<_>>();

    let mut groups = Vec::new();
    let mut start = 0;
    while start < columns {
        let mut end = start;
        let mut minimum_width = 1;
        while end < columns {
            let next = minimum_width + minimum[end] + 3;
            if end > start && next > TABLE_WIDTH {
                break;
            }
            minimum_width = next;
            end += 1;
        }
        groups.push(start..end);
        start = end;
    }

    let mut output = Vec::new();
    for (group_index, group) in groups.into_iter().enumerate() {
        if group_index > 0 {
            output.push(String::new());
        }
        output.push("```".to_owned());
        let mut widths = natural[group.clone()].to_vec();
        fit_table_widths(&mut widths, &minimum[group.clone()]);
        render_table_group(
            &rows,
            group.start,
            &widths,
            &table.alignments[group.clone()],
            &mut output,
        );
        output.push("```".to_owned());
    }
    output
}

fn table_cell_text(inlines: &[MarkdownInline]) -> String {
    plain_inlines(inlines)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('|', "\\|")
}

fn fit_table_widths(widths: &mut [usize], minimum: &[usize]) {
    while table_line_width(widths) > TABLE_WIDTH {
        let Some((index, _)) = widths
            .iter()
            .enumerate()
            .filter(|(index, width)| **width > minimum[*index])
            .max_by_key(|(_, width)| **width)
        else {
            break;
        };
        widths[index] -= 1;
    }
}

fn table_line_width(widths: &[usize]) -> usize {
    1 + widths.iter().sum::<usize>() + widths.len() * 3
}

fn render_table_group(
    rows: &[Vec<String>],
    start: usize,
    widths: &[usize],
    alignments: &[MarkdownAlignment],
    output: &mut Vec<String>,
) {
    for (row_index, row) in rows.iter().enumerate() {
        let wrapped = widths
            .iter()
            .enumerate()
            .map(|(offset, width)| {
                wrap_text(row.get(start + offset).map_or("", String::as_str), *width)
            })
            .collect::<Vec<_>>();
        let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
        for line_index in 0..height {
            let mut line = String::from("|");
            for (offset, width) in widths.iter().enumerate() {
                let content = wrapped[offset].get(line_index).map_or("", String::as_str);
                let alignment = alignments
                    .get(offset)
                    .copied()
                    .unwrap_or(MarkdownAlignment::None);
                line.push(' ');
                line.push_str(&align_text(content, *width, alignment));
                line.push_str(" |");
            }
            output.push(line);
        }
        if row_index == 0 {
            let mut separator = String::from("|");
            for width in widths {
                separator.push_str(&"-".repeat(width + 2));
                separator.push('|');
            }
            output.push(separator);
        }
    }
}

fn align_text(value: &str, width: usize, alignment: MarkdownAlignment) -> String {
    let padding = width.saturating_sub(value.width());
    let (left, right) = match alignment {
        MarkdownAlignment::Right => (padding, 0),
        MarkdownAlignment::Center => (padding / 2, padding - padding / 2),
        MarkdownAlignment::None | MarkdownAlignment::Left => (0, padding),
    };
    format!("{}{}{}", " ".repeat(left), value, " ".repeat(right))
}

fn wrap_text(value: &str, width: usize) -> Vec<String> {
    let graphemes = value.graphemes(true).collect::<Vec<_>>();
    if graphemes.is_empty() {
        return vec![String::new()];
    }
    let mut output = Vec::new();
    let mut start = 0;
    while start < graphemes.len() {
        let mut end = start;
        let mut used = 0;
        let mut last_break = None;
        while end < graphemes.len() {
            let grapheme_width = graphemes[end].width();
            if used > 0 && used + grapheme_width > width {
                break;
            }
            used += grapheme_width;
            if graphemes[end].chars().all(char::is_whitespace) {
                last_break = Some(end);
            }
            end += 1;
        }
        let (line_end, next) = if end < graphemes.len() {
            if let Some(break_at) = last_break.filter(|break_at| *break_at > start) {
                let mut next = break_at + 1;
                while next < graphemes.len() && graphemes[next].chars().all(char::is_whitespace) {
                    next += 1;
                }
                (break_at, next)
            } else {
                (end.max(start + 1), end.max(start + 1))
            }
        } else {
            (end, end)
        };
        output.push(graphemes[start..line_end].concat());
        start = next;
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_slack_formatting_and_removes_code_languages() {
        let source = "# Heading\n\n**bold** *italic* ~~gone~~ [site](https://example.com?a=1&b=2)\n\n- one\n- two\n\n```rust\nfn main() {}\n```";
        assert_eq!(
            convert_markdown(source, MarkdownDialect::Slack),
            "*Heading*\n\n*bold* _italic_ ~gone~ <https://example.com?a=1&amp;b=2|site>\n\n• one\n• two\n\n```\nfn main() {}\n```"
        );
    }

    #[test]
    fn converts_discord_headings_and_preserves_code_languages() {
        let source = "## Heading\n\n**bold** *italic* ~~gone~~ [site](https://example.com)\n\n```rust\nfn main() {}\n```";
        assert_eq!(
            convert_markdown(source, MarkdownDialect::Discord),
            "**Heading**\n\n**bold** _italic_ ~~gone~~ [site](https://example.com)\n\n```rust\nfn main() {}\n```"
        );
    }

    #[test]
    fn chat_dialects_strip_path_markers_but_preserve_code_blocks() {
        let source = "Use **@src/lib.rs:4** and `@{path with spaces}:2`.\n\n```text\n@keep/me\n```";
        for dialect in [MarkdownDialect::Slack, MarkdownDialect::Discord] {
            let converted = copy_markdown(source, Some(dialect));
            assert!(converted.contains("src/lib.rs:4"));
            assert!(converted.contains("`path with spaces:2`"));
            assert!(converted.contains("@keep/me"));
            assert!(!converted.contains("@src/lib.rs"));
        }
        assert_eq!(copy_markdown("See @src/lib.rs:4", None), "See src/lib.rs:4");
    }

    #[test]
    fn converts_quotes_tasks_nested_heading_styles_and_literals() {
        let source =
            "# **Status** *now*\n\n> ready & waiting\n\n- [x] done\n- [ ] later\n\n\\*literal\\*";
        assert_eq!(
            convert_markdown(source, MarkdownDialect::Slack),
            "*Status _now_*\n\n> ready &amp; waiting\n\n• [x] done\n• [ ] later\n\n*literal*"
        );
        assert_eq!(
            convert_markdown(source, MarkdownDialect::Discord),
            "**Status _now_**\n\n> ready & waiting\n\n- [x] done\n- [ ] later\n\n\\*literal\\*"
        );
    }

    #[test]
    fn renders_tables_as_padded_code_blocks() {
        let source = "| A | B |\n|-|-|\n| a | b |";
        let expected = "```\n| A | B |\n|---|---|\n| a | b |\n```";
        assert_eq!(convert_markdown(source, MarkdownDialect::Slack), expected);
        assert_eq!(convert_markdown(source, MarkdownDialect::Discord), expected);
    }

    #[test]
    fn table_alignment_wrapping_and_unicode_stay_bounded() {
        let source = format!(
            "| left | centered | right |\n| :--- | :---: | ---: |\n| a \\| b | {} | 界界界 |",
            "long content ".repeat(20)
        );
        let converted = convert_markdown(&source, MarkdownDialect::Discord);
        assert!(converted.contains("a \\| b"));
        for line in converted.lines() {
            assert!(line.width() <= TABLE_WIDTH, "line is too wide: {line:?}");
        }
        assert!(
            converted
                .lines()
                .filter(|line| line.starts_with('|'))
                .count()
                > 3
        );
    }

    #[test]
    fn very_wide_tables_split_into_repeated_header_groups() {
        let header = (0..40).map(|index| format!("c{index}")).collect::<Vec<_>>();
        let separator = vec!["---"; 40];
        let row = vec!["x"; 40];
        let source = format!(
            "|{}|\n|{}|\n|{}|",
            header.join("|"),
            separator.join("|"),
            row.join("|")
        );
        let converted = convert_markdown(&source, MarkdownDialect::Slack);
        assert!(converted.matches("```").count() >= 4);
        assert!(converted.lines().all(|line| line.width() <= TABLE_WIDTH));
    }
}
