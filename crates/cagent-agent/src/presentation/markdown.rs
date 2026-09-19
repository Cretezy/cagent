use std::path::Path;
use std::sync::OnceLock;

use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use syntect::parsing::{ParseState, ScopeStack, SyntaxSet};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodeTokenKind {
    Plain,
    Comment,
    String,
    Number,
    Keyword,
    Type,
    Function,
    Macro,
    Attribute,
    Constant,
    Operator,
    MarkupHeading,
    MarkupStrong,
    MarkupEmphasis,
    MarkupLink,
    MarkupRaw,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodeToken {
    pub kind: CodeTokenKind,
    pub text: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MarkdownDocument {
    pub blocks: Vec<MarkdownBlock>,
}

impl MarkdownDocument {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    pub fn clear(&mut self) {
        self.blocks.clear();
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarkdownBlock {
    Paragraph(Vec<MarkdownInline>),
    Heading {
        level: u8,
        content: Vec<MarkdownInline>,
    },
    BlockQuote(Vec<MarkdownBlock>),
    List {
        start: Option<u64>,
        items: Vec<MarkdownListItem>,
    },
    CodeBlock {
        language: Option<String>,
        tokens: Vec<CodeToken>,
    },
    Table(MarkdownTable),
    Rule,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownListItem {
    pub checked: Option<bool>,
    pub blocks: Vec<MarkdownBlock>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarkdownInline {
    Text(String),
    Code(String),
    PathReference {
        display: String,
        path: String,
        line: Option<usize>,
        code: bool,
    },
    Emphasis(Vec<MarkdownInline>),
    Strong(Vec<MarkdownInline>),
    Strikethrough(Vec<MarkdownInline>),
    Link {
        destination: String,
        safe: bool,
        content: Vec<MarkdownInline>,
    },
    SoftBreak,
    HardBreak,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownTable {
    pub alignments: Vec<MarkdownAlignment>,
    pub header: MarkdownTableRow,
    pub rows: Vec<MarkdownTableRow>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MarkdownTableRow {
    pub cells: Vec<MarkdownTableCell>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MarkdownTableCell {
    pub content: Vec<MarkdownInline>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownAlignment {
    None,
    Left,
    Center,
    Right,
}

#[must_use]
pub fn parse_markdown(source: &str) -> MarkdownDocument {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut events = Parser::new_ext(source, options).peekable();
    let mut unused_task = None;
    MarkdownDocument {
        blocks: parse_blocks(&mut events, None, &mut unused_task),
    }
}

/// Parses Markdown while an assistant response is still arriving.
///
/// A Markdown parser must treat an unfinished link as ordinary text. For a
/// streaming UI that exposes the URL syntax and causes it to jump when the
/// closing parenthesis arrives. Keep the visible label, but defer the link
/// itself until its destination is complete.
#[must_use]
pub fn parse_streaming_markdown(source: &str) -> MarkdownDocument {
    parse_markdown(&streaming_source(source))
}

fn streaming_source(source: &str) -> String {
    let Some(open_destination) = find_unfinished_link_destination(source) else {
        return source.to_owned();
    };
    let Some(label_start) = find_link_label_start(&source[..open_destination - 2]) else {
        return source.to_owned();
    };
    format!(
        "{}{}",
        &source[..label_start],
        &source[label_start + 1..open_destination - 2]
    )
}

fn find_unfinished_link_destination(source: &str) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut index = 0;
    let mut code_ticks = None;
    let mut candidate = None::<(usize, usize)>;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            index += 2;
            continue;
        }
        if let Some((_start, depth)) = &mut candidate {
            match bytes[index] {
                b'(' => *depth += 1,
                b')' => {
                    *depth -= 1;
                    if *depth == 0 {
                        candidate = None;
                    }
                }
                _ => {}
            }
            index += 1;
            continue;
        }
        if bytes[index] == b'`' {
            let run = bytes[index..]
                .iter()
                .take_while(|byte| **byte == b'`')
                .count();
            match code_ticks {
                None => code_ticks = Some(run),
                Some(open) if open == run => code_ticks = None,
                Some(_) => {}
            }
            index += run;
            continue;
        }
        if code_ticks.is_none() && bytes[index] == b']' && bytes.get(index + 1) == Some(&b'(') {
            candidate = Some((index + 2, 1));
            index += 2;
        } else {
            index += 1;
        }
    }
    candidate.map(|(start, _)| start)
}

fn find_link_label_start(source: &str) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut index = bytes.len();
    while index > 0 {
        index -= 1;
        if bytes[index] != b'[' {
            continue;
        }
        let escapes = bytes[..index]
            .iter()
            .rev()
            .take_while(|byte| **byte == b'\\')
            .count();
        if escapes % 2 == 0 {
            return Some(index);
        }
    }
    None
}

fn parse_blocks<'a>(
    events: &mut std::iter::Peekable<impl Iterator<Item = Event<'a>>>,
    end: Option<TagEnd>,
    task: &mut Option<bool>,
) -> Vec<MarkdownBlock> {
    let mut blocks = Vec::new();
    while let Some(event) = events.next() {
        match event {
            Event::End(found) if Some(found) == end => break,
            Event::Start(Tag::Emphasis) => push_fallback_inline(
                &mut blocks,
                MarkdownInline::Emphasis(parse_inlines(events, TagEnd::Emphasis, task, true)),
            ),
            Event::Start(Tag::Strong) => push_fallback_inline(
                &mut blocks,
                MarkdownInline::Strong(parse_inlines(events, TagEnd::Strong, task, true)),
            ),
            Event::Start(Tag::Strikethrough) => push_fallback_inline(
                &mut blocks,
                MarkdownInline::Strikethrough(parse_inlines(
                    events,
                    TagEnd::Strikethrough,
                    task,
                    true,
                )),
            ),
            Event::Start(Tag::Link { dest_url, .. }) => {
                let destination = dest_url.into_string();
                let content = parse_inlines(events, TagEnd::Link, task, false);
                push_fallback_inline(
                    &mut blocks,
                    MarkdownInline::Link {
                        safe: safe_link(&destination),
                        destination,
                        content,
                    },
                );
            }
            Event::Start(Tag::Image { .. }) => {
                for inline in parse_inlines(events, TagEnd::Image, task, true) {
                    push_fallback_inline(&mut blocks, inline);
                }
            }
            Event::Start(tag) => {
                if let Some(block) = parse_block(tag, events, task) {
                    blocks.push(block);
                }
            }
            Event::Rule => blocks.push(MarkdownBlock::Rule),
            Event::TaskListMarker(checked) => *task = Some(checked),
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => {
                push_fallback_text(&mut blocks, text.into_string());
            }
            Event::Code(code) => {
                let mut inlines = Vec::new();
                push_path_aware_text(&mut inlines, code.as_ref(), true, true);
                for inline in inlines {
                    push_fallback_inline(&mut blocks, inline);
                }
            }
            Event::InlineMath(text) | Event::DisplayMath(text) => {
                push_fallback_text(&mut blocks, text.into_string());
            }
            Event::FootnoteReference(reference) => {
                push_fallback_text(&mut blocks, format!("[{reference}]"));
            }
            Event::SoftBreak => {
                push_fallback_inline(&mut blocks, MarkdownInline::SoftBreak);
            }
            Event::HardBreak => {
                push_fallback_inline(&mut blocks, MarkdownInline::HardBreak);
            }
            Event::End(_) => {}
        }
    }
    blocks
}

fn parse_block<'a>(
    tag: Tag<'a>,
    events: &mut std::iter::Peekable<impl Iterator<Item = Event<'a>>>,
    task: &mut Option<bool>,
) -> Option<MarkdownBlock> {
    Some(match tag {
        Tag::Paragraph => {
            MarkdownBlock::Paragraph(parse_inlines(events, TagEnd::Paragraph, task, true))
        }
        Tag::Heading { level, .. } => MarkdownBlock::Heading {
            level: level as u8,
            content: parse_inlines(events, TagEnd::Heading(level), task, true),
        },
        Tag::BlockQuote(kind) => {
            MarkdownBlock::BlockQuote(parse_blocks(events, Some(TagEnd::BlockQuote(kind)), task))
        }
        Tag::List(start) => MarkdownBlock::List {
            start,
            items: parse_list(events, start.is_some()),
        },
        Tag::CodeBlock(kind) => parse_code_block(events, kind),
        Tag::Table(alignments) => MarkdownBlock::Table(parse_table(events, alignments)),
        Tag::Link { dest_url, .. } => {
            let destination = dest_url.into_string();
            MarkdownBlock::Paragraph(vec![MarkdownInline::Link {
                safe: safe_link(&destination),
                destination,
                content: parse_inlines(events, TagEnd::Link, task, false),
            }])
        }
        Tag::HtmlBlock => {
            let content = parse_inlines(events, TagEnd::HtmlBlock, task, true);
            MarkdownBlock::Paragraph(content)
        }
        other => {
            let content = parse_inlines(events, other.to_end(), task, true);
            if content.is_empty() {
                return None;
            }
            MarkdownBlock::Paragraph(content)
        }
    })
}

fn parse_list<'a>(
    events: &mut std::iter::Peekable<impl Iterator<Item = Event<'a>>>,
    ordered: bool,
) -> Vec<MarkdownListItem> {
    let mut items = Vec::new();
    while let Some(event) = events.next() {
        match event {
            Event::End(TagEnd::List(found)) if found == ordered => break,
            Event::Start(Tag::Item) => {
                let mut checked = None;
                let blocks = parse_blocks(events, Some(TagEnd::Item), &mut checked);
                items.push(MarkdownListItem { checked, blocks });
            }
            _ => {}
        }
    }
    items
}

fn parse_code_block<'a>(
    events: &mut std::iter::Peekable<impl Iterator<Item = Event<'a>>>,
    kind: CodeBlockKind<'a>,
) -> MarkdownBlock {
    let language = match kind {
        CodeBlockKind::Fenced(language) => {
            let language = language.into_string();
            (!language.is_empty()).then_some(language)
        }
        CodeBlockKind::Indented => None,
    };
    let mut source = String::new();
    for event in events.by_ref() {
        match event {
            Event::End(TagEnd::CodeBlock) => break,
            Event::Text(text)
            | Event::Code(text)
            | Event::Html(text)
            | Event::InlineHtml(text)
            | Event::InlineMath(text)
            | Event::DisplayMath(text) => source.push_str(&text),
            Event::SoftBreak | Event::HardBreak => source.push('\n'),
            _ => {}
        }
    }
    let tokens = highlight_code(language.as_deref().unwrap_or_default(), &source);
    MarkdownBlock::CodeBlock { language, tokens }
}

fn parse_table<'a>(
    events: &mut std::iter::Peekable<impl Iterator<Item = Event<'a>>>,
    alignments: Vec<Alignment>,
) -> MarkdownTable {
    let mut header = MarkdownTableRow::default();
    let mut rows = Vec::new();
    let mut unused_task = None;
    while let Some(event) = events.next() {
        match event {
            Event::End(TagEnd::Table) => break,
            Event::Start(Tag::TableHead) => {
                header = parse_table_row(events, TagEnd::TableHead, &mut unused_task);
            }
            Event::Start(Tag::TableRow) => {
                rows.push(parse_table_row(events, TagEnd::TableRow, &mut unused_task));
            }
            _ => {}
        }
    }
    MarkdownTable {
        alignments: alignments.into_iter().map(Into::into).collect(),
        header,
        rows,
    }
}

fn parse_table_row<'a>(
    events: &mut std::iter::Peekable<impl Iterator<Item = Event<'a>>>,
    end: TagEnd,
    task: &mut Option<bool>,
) -> MarkdownTableRow {
    let mut cells = Vec::new();
    while let Some(event) = events.next() {
        match event {
            Event::End(found) if found == end => break,
            Event::Start(Tag::TableCell) => cells.push(MarkdownTableCell {
                content: parse_inlines(events, TagEnd::TableCell, task, true),
            }),
            _ => {}
        }
    }
    MarkdownTableRow { cells }
}

fn parse_inlines<'a>(
    events: &mut std::iter::Peekable<impl Iterator<Item = Event<'a>>>,
    end: TagEnd,
    task: &mut Option<bool>,
    allow_path_references: bool,
) -> Vec<MarkdownInline> {
    let mut inlines = Vec::new();
    while let Some(event) = events.next() {
        match event {
            Event::End(found) if found == end => break,
            Event::Start(Tag::Emphasis) => push_inline(
                &mut inlines,
                MarkdownInline::Emphasis(parse_inlines(
                    events,
                    TagEnd::Emphasis,
                    task,
                    allow_path_references,
                )),
            ),
            Event::Start(Tag::Strong) => push_inline(
                &mut inlines,
                MarkdownInline::Strong(parse_inlines(
                    events,
                    TagEnd::Strong,
                    task,
                    allow_path_references,
                )),
            ),
            Event::Start(Tag::Strikethrough) => push_inline(
                &mut inlines,
                MarkdownInline::Strikethrough(parse_inlines(
                    events,
                    TagEnd::Strikethrough,
                    task,
                    allow_path_references,
                )),
            ),
            Event::Start(Tag::Link { dest_url, .. }) => {
                let destination = dest_url.into_string();
                let content = parse_inlines(events, TagEnd::Link, task, false);
                push_inline(
                    &mut inlines,
                    MarkdownInline::Link {
                        safe: safe_link(&destination),
                        destination,
                        content,
                    },
                );
            }
            Event::Start(Tag::Image { .. }) => {
                for inline in parse_inlines(events, TagEnd::Image, task, allow_path_references) {
                    push_inline(&mut inlines, inline);
                }
            }
            Event::Start(tag) => {
                for inline in parse_inlines(events, tag.to_end(), task, allow_path_references) {
                    push_inline(&mut inlines, inline);
                }
            }
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => {
                push_inline(&mut inlines, MarkdownInline::Text(text.into_string()));
            }
            Event::Code(code) => {
                push_path_aware_text(&mut inlines, code.as_ref(), true, allow_path_references);
            }
            Event::SoftBreak => push_inline(&mut inlines, MarkdownInline::SoftBreak),
            Event::HardBreak => push_inline(&mut inlines, MarkdownInline::HardBreak),
            Event::TaskListMarker(checked) => *task = Some(checked),
            Event::FootnoteReference(reference) => {
                push_inline(&mut inlines, MarkdownInline::Text(format!("[{reference}]")));
            }
            Event::InlineMath(math) | Event::DisplayMath(math) => {
                push_inline(&mut inlines, MarkdownInline::Text(math.into_string()));
            }
            Event::Rule | Event::End(_) => {}
        }
    }
    if allow_path_references {
        annotate_text_path_references(&mut inlines);
    }
    inlines
}

fn annotate_text_path_references(inlines: &mut Vec<MarkdownInline>) {
    let original = std::mem::take(inlines);
    for inline in original {
        if let MarkdownInline::Text(text) = inline {
            push_path_aware_text(inlines, &text, false, true);
        } else {
            push_inline(inlines, inline);
        }
    }
}

fn push_path_aware_text(inlines: &mut Vec<MarkdownInline>, text: &str, code: bool, allow: bool) {
    if !allow {
        push_inline(
            inlines,
            if code {
                MarkdownInline::Code(text.to_owned())
            } else {
                MarkdownInline::Text(text.to_owned())
            },
        );
        return;
    }
    if !code {
        let links = bare_web_links(text);
        if !links.is_empty() {
            let mut offset = 0;
            for link in links {
                if offset < link.start {
                    push_path_references(inlines, &text[offset..link.start], false);
                }
                let destination = text[link.clone()].to_owned();
                push_inline(
                    inlines,
                    MarkdownInline::Link {
                        safe: true,
                        destination: destination.clone(),
                        content: vec![MarkdownInline::Text(destination)],
                    },
                );
                offset = link.end;
            }
            if offset < text.len() {
                push_path_references(inlines, &text[offset..], false);
            }
            return;
        }
    }
    push_path_references(inlines, text, code);
}

fn push_path_references(inlines: &mut Vec<MarkdownInline>, text: &str, code: bool) {
    let references = super::path_references::parse_plain_path_references(text);
    if references.is_empty() {
        push_inline(
            inlines,
            if code {
                MarkdownInline::Code(text.to_owned())
            } else {
                MarkdownInline::Text(text.to_owned())
            },
        );
        return;
    }
    let mut offset = 0;
    for reference in references {
        if offset < reference.span.start {
            push_inline(
                inlines,
                if code {
                    MarkdownInline::Code(text[offset..reference.span.start].to_owned())
                } else {
                    MarkdownInline::Text(text[offset..reference.span.start].to_owned())
                },
            );
        }
        let source = &text[reference.span.clone()];
        push_inline(
            inlines,
            MarkdownInline::PathReference {
                display: super::path_references::strip_path_reference_markers(source),
                path: reference.path,
                line: reference.line,
                code,
            },
        );
        offset = reference.span.end;
    }
    if offset < text.len() {
        push_inline(
            inlines,
            if code {
                MarkdownInline::Code(text[offset..].to_owned())
            } else {
                MarkdownInline::Text(text[offset..].to_owned())
            },
        );
    }
}

fn bare_web_links(text: &str) -> Vec<std::ops::Range<usize>> {
    let mut links = Vec::new();
    let mut offset = 0;
    while offset < text.len() {
        let remainder = &text[offset..];
        let Some(relative_start) = [remainder.find("https://"), remainder.find("http://")]
            .into_iter()
            .flatten()
            .min()
        else {
            break;
        };
        let start = offset + relative_start;
        if start > 0
            && text[..start]
                .chars()
                .next_back()
                .is_some_and(|character| character.is_alphanumeric() || character == '_')
        {
            offset = start + 1;
            continue;
        }
        let mut end = text[start..]
            .char_indices()
            .take_while(|(_, character)| {
                !character.is_whitespace()
                    && !matches!(character, '<' | '>' | '"' | '\'' | '`' | '[' | ']')
            })
            .last()
            .map_or(start, |(index, character)| {
                start + index + character.len_utf8()
            });
        while end > start
            && text[..end].chars().next_back().is_some_and(|character| {
                matches!(character, '.' | ',' | ';' | ':' | '!' | '?' | ')' | '}')
            })
        {
            end -= text[..end].chars().next_back().unwrap().len_utf8();
        }
        let candidate = &text[start..end];
        if url::Url::parse(candidate)
            .ok()
            .is_some_and(|url| matches!(url.scheme(), "http" | "https") && url.host().is_some())
        {
            links.push(start..end);
            offset = end;
        } else {
            offset = start + 1;
        }
    }
    links
}

fn push_inline(inlines: &mut Vec<MarkdownInline>, inline: MarkdownInline) {
    if let MarkdownInline::Text(text) = &inline
        && let Some(MarkdownInline::Text(previous)) = inlines.last_mut()
    {
        previous.push_str(text);
    } else {
        inlines.push(inline);
    }
}

fn push_fallback_text(blocks: &mut Vec<MarkdownBlock>, text: String) {
    if let Some(MarkdownBlock::Paragraph(inlines)) = blocks.last_mut() {
        push_path_aware_text(inlines, &text, false, true);
    } else {
        let mut inlines = Vec::new();
        push_path_aware_text(&mut inlines, &text, false, true);
        blocks.push(MarkdownBlock::Paragraph(inlines));
    }
}

fn push_fallback_inline(blocks: &mut Vec<MarkdownBlock>, inline: MarkdownInline) {
    if let Some(MarkdownBlock::Paragraph(inlines)) = blocks.last_mut() {
        push_inline(inlines, inline);
    } else {
        blocks.push(MarkdownBlock::Paragraph(vec![inline]));
    }
}

impl From<Alignment> for MarkdownAlignment {
    fn from(value: Alignment) -> Self {
        match value {
            Alignment::None => Self::None,
            Alignment::Left => Self::Left,
            Alignment::Center => Self::Center,
            Alignment::Right => Self::Right,
        }
    }
}

fn safe_link(value: &str) -> bool {
    (value.starts_with("https://") || value.starts_with("http://"))
        && !value.chars().any(char::is_control)
}

/// Produces color-neutral syntax tokens for a frontend-owned renderer.
#[must_use]
pub fn highlight_code(language: &str, source: &str) -> Vec<CodeToken> {
    let raw_token = language.split_whitespace().next().unwrap_or_default();
    let token = syntax_token_alias(raw_token.trim_start_matches('.'));
    if token.eq_ignore_ascii_case("toml") {
        return highlight_toml(source);
    }
    let defaults = default_syntaxes();
    if let Some(syntax) = defaults
        .find_syntax_by_token(raw_token)
        .or_else(|| defaults.find_syntax_by_token(token))
        .or_else(|| defaults.find_syntax_by_extension(token))
    {
        return highlight_with_syntax(defaults, syntax, source);
    }
    let extras = extra_syntaxes();
    if let Some(syntax) = extras
        .find_syntax_by_token(raw_token)
        .or_else(|| extras.find_syntax_by_token(token))
        .or_else(|| extras.find_syntax_by_extension(token))
    {
        return highlight_with_syntax(extras, syntax, source);
    }
    vec![CodeToken {
        kind: CodeTokenKind::Plain,
        text: source.to_owned(),
    }]
}

/// Finds a bundled syntax token for a path from its full name, extension, or first line.
///
/// Full-name matching covers conventional extensionless and compound names such as
/// `Dockerfile`, `Makefile`, `.env`, and `requirements.txt`. First-line matching covers
/// extensionless scripts and files with editor modelines. `mdx` is rendered with the bundled
/// Markdown grammar. Unknown files return `None`.
#[must_use]
pub fn syntax_language_for_path(path: &Path, source: &str) -> Option<String> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if file_name.eq_ignore_ascii_case("Cargo.lock") {
        return Some("toml".to_owned());
    }
    if file_name.eq_ignore_ascii_case("Containerfile") {
        return Some("Dockerfile".to_owned());
    }

    if let Some(syntax_name) = syntax_name_for_filename(file_name) {
        return Some(syntax_name);
    }

    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let token = syntax_token_alias(&extension);
    if has_syntax_for_token(token) {
        return Some(token.to_owned());
    }

    let first_line = source.split_inclusive('\n').next().unwrap_or_default();
    syntax_name_for_first_line(first_line)
}

/// Chooses a syntax token for a file from its full name, extension, or first line.
///
/// Full-name matching covers conventional extensionless and compound names such as
/// `Dockerfile`, `Makefile`, `.env`, and `requirements.txt`. First-line matching covers
/// extensionless scripts and files with editor modelines. Unknown files retain their extension so
/// frontends can still display a useful language hint while rendering them as plain text.
#[must_use]
pub fn detect_code_language(path: &Path, source: &str) -> String {
    if let Some(language) = syntax_language_for_path(path, source) {
        return language;
    }
    path.extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn syntax_token_alias(token: &str) -> &str {
    if token.eq_ignore_ascii_case("mdx") {
        "markdown"
    } else {
        token
    }
}

fn default_syntaxes() -> &'static SyntaxSet {
    static DEFAULT_SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
    DEFAULT_SYNTAXES.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn has_syntax_for_token(token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    default_syntaxes().find_syntax_by_token(token).is_some()
        || extra_syntaxes().find_syntax_by_token(token).is_some()
}

fn syntax_name_for_filename(file_name: &str) -> Option<String> {
    if file_name.is_empty() {
        return None;
    }
    default_syntaxes()
        .find_syntax_by_extension(file_name)
        .or_else(|| extra_syntaxes().find_syntax_by_extension(file_name))
        .map(|syntax| syntax.name.clone())
}

fn syntax_name_for_first_line(first_line: &str) -> Option<String> {
    if first_line.is_empty() {
        return None;
    }
    default_syntaxes()
        .find_syntax_by_first_line(first_line)
        .or_else(|| extra_syntaxes().find_syntax_by_first_line(first_line))
        .map(|syntax| syntax.name.clone())
}

fn highlight_with_syntax(
    syntaxes: &SyntaxSet,
    syntax: &syntect::parsing::SyntaxReference,
    source: &str,
) -> Vec<CodeToken> {
    let mut parser = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut tokens = Vec::new();
    for line in source.split_inclusive('\n') {
        let Ok(operations) = parser.parse_line(line, syntaxes) else {
            push_code_token(&mut tokens, CodeTokenKind::Plain, line);
            continue;
        };
        let mut offset = 0;
        for (end, operation) in operations {
            if end > offset {
                push_code_token(&mut tokens, code_kind(&stack), &line[offset..end]);
            }
            let _ = stack.apply(&operation);
            offset = end;
        }
        if offset < line.len() {
            push_code_token(&mut tokens, code_kind(&stack), &line[offset..]);
        }
    }
    tokens
}

fn extra_syntaxes() -> &'static SyntaxSet {
    static EXTRA_SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
    EXTRA_SYNTAXES.get_or_init(sublime_syntaxes::extra_syntax_set)
}

fn highlight_toml_keys(tokens: Vec<CodeToken>) -> Vec<CodeToken> {
    let mut highlighted = Vec::new();
    for token in tokens {
        if token.kind != CodeTokenKind::Plain {
            push_code_token(&mut highlighted, token.kind, &token.text);
            continue;
        }
        let mut text = token.text.as_str();
        if highlighted
            .last()
            .is_some_and(|token: &CodeToken| token.kind == CodeTokenKind::Comment)
            && text.starts_with('\n')
        {
            if let Some(comment) = highlighted.last_mut() {
                comment.text.push('\n');
            }
            text = &text[1..];
        }
        for line in text.split_inclusive('\n') {
            let line_without_newline = line.strip_suffix('\n').unwrap_or(line);
            let Some(equal) = line_without_newline.find('=') else {
                push_code_token(&mut highlighted, CodeTokenKind::Plain, line);
                continue;
            };
            let key_end = line_without_newline[..equal].trim_end().len();
            let key_start = line_without_newline[..key_end]
                .rfind(char::is_whitespace)
                .map_or(0, |index| index + 1);
            let key = &line_without_newline[key_start..key_end];
            if key.is_empty()
                || !key.chars().all(|character| {
                    character.is_ascii_alphanumeric() || character == '_' || character == '-'
                })
            {
                push_code_token(&mut highlighted, CodeTokenKind::Plain, line);
                continue;
            }
            push_code_token(&mut highlighted, CodeTokenKind::Plain, &line[..key_start]);
            push_code_token(&mut highlighted, CodeTokenKind::Attribute, key);
            push_code_token(&mut highlighted, CodeTokenKind::Plain, &line[key_end..]);
        }
    }
    highlighted
}

fn highlight_toml(source: &str) -> Vec<CodeToken> {
    let syntaxes = extra_syntaxes();
    let syntax = syntaxes
        .find_syntax_by_token("toml")
        .or_else(|| syntaxes.find_syntax_by_extension("toml"))
        .expect("the bundled extra syntax set includes TOML");
    let mut parser = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut tokens = Vec::new();
    for line in source.split_inclusive('\n') {
        let Ok(operations) = parser.parse_line(line, syntaxes) else {
            push_code_token(&mut tokens, CodeTokenKind::Plain, line);
            continue;
        };
        let mut offset = 0;
        for (end, operation) in operations {
            if end > offset {
                push_code_token(&mut tokens, code_kind(&stack), &line[offset..end]);
            }
            let _ = stack.apply(&operation);
            offset = end;
        }
        if offset < line.len() {
            push_code_token(&mut tokens, code_kind(&stack), &line[offset..]);
        }
    }
    highlight_toml_keys(tokens)
}

fn code_kind(stack: &ScopeStack) -> CodeTokenKind {
    let scopes = stack
        .scopes
        .iter()
        .rev()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let has = |needle: &str| scopes.iter().any(|scope| scope.contains(needle));
    if has("markup.raw") {
        CodeTokenKind::MarkupRaw
    } else if has("markup.heading") {
        CodeTokenKind::MarkupHeading
    } else if has("markup.bold") {
        CodeTokenKind::MarkupStrong
    } else if has("markup.italic") {
        CodeTokenKind::MarkupEmphasis
    } else if has("markup.underline.link") || has("meta.link") || has("string.other.link") {
        CodeTokenKind::MarkupLink
    } else if has("comment") || has("markup.quote") {
        CodeTokenKind::Comment
    } else if has("string") || has("character") {
        CodeTokenKind::String
    } else if has("constant.numeric") {
        CodeTokenKind::Number
    } else if has("keyword") || has("storage") {
        CodeTokenKind::Keyword
    } else if has("attribute") {
        CodeTokenKind::Attribute
    } else if has("macro") {
        CodeTokenKind::Macro
    } else if has("entity.name.type") || has("support.type") || has("storage.type") {
        CodeTokenKind::Type
    } else if has("entity.name.function") || has("variable.function") {
        CodeTokenKind::Function
    } else if has("constant") {
        CodeTokenKind::Constant
    } else if has("operator") {
        CodeTokenKind::Operator
    } else {
        CodeTokenKind::Plain
    }
}

fn push_code_token(tokens: &mut Vec<CodeToken>, kind: CodeTokenKind, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = tokens.last_mut()
        && last.kind == kind
    {
        last.text.push_str(text);
    } else {
        tokens.push(CodeToken {
            kind,
            text: text.to_owned(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inline_text(inlines: &[MarkdownInline]) -> String {
        let mut text = String::new();
        for inline in inlines {
            match inline {
                MarkdownInline::Text(value) | MarkdownInline::Code(value) => text.push_str(value),
                MarkdownInline::PathReference { display, .. } => text.push_str(display),
                MarkdownInline::Emphasis(content)
                | MarkdownInline::Strong(content)
                | MarkdownInline::Strikethrough(content)
                | MarkdownInline::Link { content, .. } => text.push_str(&inline_text(content)),
                MarkdownInline::SoftBreak => text.push(' '),
                MarkdownInline::HardBreak => text.push('\n'),
            }
        }
        text
    }

    #[test]
    fn markdown_and_rust_are_parsed_without_terminal_colors() {
        let document = parse_markdown("# Title\n\n```rust\nfn main() { println!(\"hi\"); }\n```");
        assert!(matches!(
            document.blocks.first(),
            Some(MarkdownBlock::Heading { level: 1, .. })
        ));
        let code = document
            .blocks
            .iter()
            .find_map(|block| match block {
                MarkdownBlock::CodeBlock { tokens, .. } => Some(tokens),
                _ => None,
            })
            .unwrap();
        assert!(
            code.iter()
                .any(|token| token.kind == CodeTokenKind::Keyword)
        );
        assert!(code.iter().any(|token| token.kind == CodeTokenKind::String));
        assert!(code.iter().all(|token| !token.text.contains('\x1b')));
    }

    #[test]
    fn toml_is_highlighted_when_the_default_syntax_set_has_no_toml_grammar() {
        let tokens = highlight_code(
            "toml",
            "[providers.openai]\nenabled = true\nmodel = \"gpt-5.6\" # preferred\nretries = 3\n",
        );
        assert!(
            tokens
                .iter()
                .any(|token| token.kind == CodeTokenKind::Attribute && token.text == "enabled")
        );
        assert!(
            tokens
                .iter()
                .any(|token| token.kind == CodeTokenKind::Constant && token.text == "true")
        );
        assert!(
            tokens
                .iter()
                .any(|token| token.kind == CodeTokenKind::String && token.text == "\"gpt-5.6\"")
        );
        assert!(
            tokens
                .iter()
                .any(|token| token.kind == CodeTokenKind::Comment && token.text == "# preferred\n")
        );
        assert!(
            tokens
                .iter()
                .any(|token| token.kind == CodeTokenKind::Number && token.text == "3")
        );
    }

    #[test]
    fn typescript_uses_the_extra_syntax_set() {
        for language in ["ts", "tsx", "mts", "cts"] {
            let tokens = highlight_code(
                language,
                "interface User { name: string }\nconst user: User = { name: \"Ada\" }; // person\n",
            );
            for expected in [
                CodeTokenKind::Keyword,
                CodeTokenKind::String,
                CodeTokenKind::Comment,
            ] {
                assert!(
                    tokens.iter().any(|token| token.kind == expected),
                    "{language} did not include {expected:?} in {tokens:?}"
                );
            }
        }
    }

    #[test]
    fn syntax_lookup_includes_default_and_extra_grammars_and_mdx() {
        assert_eq!(
            syntax_language_for_path(Path::new("guide.mdx"), "# Guide\n"),
            Some("markdown".into())
        );
        for path in [
            "component.tsx",
            "schema.graphql",
            "shader.wgsl",
            "document.typ",
        ] {
            assert!(
                syntax_language_for_path(Path::new(path), "").is_some(),
                "missing bundled syntax for {path}"
            );
        }
        assert_eq!(
            syntax_language_for_path(Path::new("data.unknown"), ""),
            None
        );
    }

    #[test]
    fn mdx_fences_use_markdown_highlighting() {
        let document = parse_markdown("```mdx\n# Heading\n```");
        let MarkdownBlock::CodeBlock { language, tokens } = &document.blocks[0] else {
            panic!("expected code block");
        };
        assert_eq!(language.as_deref(), Some("mdx"));
        assert!(
            tokens
                .iter()
                .any(|token| token.kind == CodeTokenKind::MarkupHeading)
        );
    }

    #[test]
    fn markdown_scopes_keep_markup_semantics() {
        let tokens = highlight_code(
            "md",
            "# Heading\n\n**strong** and *emphasis* with [link](https://example.com) and `code`.\n",
        );
        for expected in [
            CodeTokenKind::MarkupHeading,
            CodeTokenKind::MarkupStrong,
            CodeTokenKind::MarkupEmphasis,
            CodeTokenKind::MarkupLink,
            CodeTokenKind::MarkupRaw,
        ] {
            assert!(
                tokens.iter().any(|token| token.kind == expected),
                "missing {expected:?} in {tokens:?}"
            );
        }
    }

    #[test]
    fn streaming_links_show_the_label_without_an_unfinished_destination() {
        let document = parse_streaming_markdown("See [OpenAI](https://www.openai.com");
        let MarkdownBlock::Paragraph(content) = &document.blocks[0] else {
            panic!("paragraph");
        };
        let text = inline_text(content);
        assert_eq!(text, "See OpenAI");

        let complete = parse_streaming_markdown("See [OpenAI](https://www.openai.com)");
        let MarkdownBlock::Paragraph(content) = &complete.blocks[0] else {
            panic!("paragraph");
        };
        assert!(content.iter().any(|inline| matches!(
            inline,
            MarkdownInline::Link { destination, safe: true, .. }
                if destination == "https://www.openai.com"
        )));

        let literal = parse_streaming_markdown("See [OpenAI]");
        let MarkdownBlock::Paragraph(content) = &literal.blocks[0] else {
            panic!("paragraph");
        };
        assert_eq!(inline_text(content), "See [OpenAI]");

        let nested = parse_streaming_markdown("[site](https://example.com/a_(b))");
        let MarkdownBlock::Paragraph(content) = &nested.blocks[0] else {
            panic!("paragraph");
        };
        assert!(matches!(content.first(), Some(MarkdownInline::Link { .. })));

        for code in [
            "`[not a link](unfinished`",
            "```text\n[not a link](unfinished\n```",
            r"\[not a link](unfinished",
        ] {
            assert_eq!(streaming_source(code), code);
        }
    }

    #[test]
    fn soft_and_hard_line_breaks_are_distinguished() {
        let document = parse_markdown("first\nsecond\n\nthird  \nfourth");
        let MarkdownBlock::Paragraph(first) = &document.blocks[0] else {
            panic!("first paragraph");
        };
        let MarkdownBlock::Paragraph(second) = &document.blocks[1] else {
            panic!("second paragraph");
        };
        assert!(first.contains(&MarkdownInline::SoftBreak));
        assert!(second.contains(&MarkdownInline::HardBreak));
    }

    #[test]
    fn path_references_become_semantic_inlines_but_link_labels_do_not() {
        let document = parse_markdown(
            "@src/lib.rs:4 @~/notes @{path with spaces}:2 [@ignored.rs](https://example.com)",
        );
        let MarkdownBlock::Paragraph(content) = &document.blocks[0] else {
            panic!("paragraph");
        };
        let references = content
            .iter()
            .filter_map(|inline| match inline {
                MarkdownInline::PathReference {
                    display,
                    path,
                    line,
                    ..
                } => Some((display.as_str(), path.as_str(), *line)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            references,
            vec![
                ("src/lib.rs:4", "src/lib.rs", Some(4)),
                ("~/notes", "~/notes", None),
                ("path with spaces:2", "path with spaces", Some(2)),
            ]
        );
        assert!(
            content
                .iter()
                .any(|inline| matches!(inline, MarkdownInline::Link { .. }))
        );

        let tight_list = parse_markdown("- [@ignored.rs](https://example.com)");
        let MarkdownBlock::List { items, .. } = &tight_list.blocks[0] else {
            panic!("list");
        };
        assert!(matches!(
            items[0].blocks[0],
            MarkdownBlock::Paragraph(ref content)
                if matches!(content.first(), Some(MarkdownInline::Link { content, .. })
                    if matches!(content.first(), Some(MarkdownInline::Text(text)) if text == "@ignored.rs"))
        ));

        let trailing_colon = parse_markdown("See @TEST.md: for details");
        let MarkdownBlock::Paragraph(content) = &trailing_colon.blocks[0] else {
            panic!("paragraph");
        };
        assert_eq!(
            content,
            &[
                MarkdownInline::Text("See ".into()),
                MarkdownInline::PathReference {
                    display: "TEST.md".into(),
                    path: "TEST.md".into(),
                    line: None,
                    code: false,
                },
                MarkdownInline::Text(": for details".into()),
            ]
        );
    }

    #[test]
    fn bare_web_urls_become_links_in_markdown_prose() {
        let document = parse_markdown(
            "Return exactly this Markdown: https://example.com. See @SPEC.md:2:3 too.",
        );
        let MarkdownBlock::Paragraph(content) = &document.blocks[0] else {
            panic!("paragraph");
        };
        assert!(content.iter().any(|inline| matches!(
            inline,
            MarkdownInline::Link { destination, safe: true, content }
                if destination == "https://example.com"
                    && matches!(content.as_slice(), [MarkdownInline::Text(text)] if text == destination)
        )));
        assert!(content.iter().any(|inline| matches!(
            inline,
            MarkdownInline::PathReference { path, line: Some(2), .. } if path == "SPEC.md"
        )));

        let code = parse_markdown("`https://example.com`\n\n    https://example.com");
        assert!(!code.blocks.iter().any(|block| matches!(
            block,
            MarkdownBlock::Paragraph(content)
                if content.iter().any(|inline| matches!(inline, MarkdownInline::Link { .. }))
        )));
    }

    #[test]
    fn lists_tables_and_nested_inlines_are_structured() {
        let document = parse_markdown(
            "- [x] **done**\n\n| left | right |\n| :--- | ---: |\n| [site](https://example.com) | `code` |",
        );
        let MarkdownBlock::List { items, .. } = &document.blocks[0] else {
            panic!("task list");
        };
        assert_eq!(items[0].checked, Some(true));
        assert!(matches!(
            items[0].blocks.as_slice(),
            [MarkdownBlock::Paragraph(content)]
                if matches!(content.as_slice(), [MarkdownInline::Strong(content)]
                    if matches!(content.as_slice(), [MarkdownInline::Text(text)] if text == "done"))
        ));
        let MarkdownBlock::Table(table) = &document.blocks[1] else {
            panic!("table");
        };
        assert_eq!(
            table.alignments,
            vec![MarkdownAlignment::Left, MarkdownAlignment::Right]
        );
        assert!(matches!(
            table.rows[0].cells[0].content.first(),
            Some(MarkdownInline::Link { safe: true, .. })
        ));
        assert!(matches!(
            table.rows[0].cells[1].content.first(),
            Some(MarkdownInline::Code(code)) if code == "code"
        ));
    }
}
