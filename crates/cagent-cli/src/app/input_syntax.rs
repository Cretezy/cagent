use cagent_agent::presentation::{CodeToken, CodeTokenKind, highlight_code};
use ratatui::style::Style;
use ratatui::text::Span;

use super::composer::next_grapheme;

/// Renders a source range using optional agent-provided syntax tokens.
///
/// The caller owns cursor geometry. Keeping tokenization here makes the input
/// widgets responsible only for editing and viewport behavior.
pub(super) fn syntax_tokens(source: &str, language: Option<&str>) -> Option<Vec<CodeToken>> {
    language.map(|language| highlight_code(language, source))
}

pub(super) fn highlighted_spans(
    source: &str,
    range: (usize, usize),
    tokens: Option<&[CodeToken]>,
    text_style: Style,
    cursor: Option<(usize, Style)>,
) -> Vec<Span<'static>> {
    let syntax_enabled = tokens.is_some();
    let plain = [CodeToken {
        kind: CodeTokenKind::Plain,
        text: source.to_owned(),
    }];
    let tokens = tokens.unwrap_or(&plain);
    let cursor_range = cursor.map(|(cursor, _)| (cursor, next_grapheme(source, cursor)));
    let mut spans = Vec::new();
    let mut token_start = 0;

    for token in tokens {
        let token_end = token_start + token.text.len();
        let start = range.0.max(token_start);
        let end = range.1.min(token_end);
        if start < end {
            let style = if syntax_enabled {
                crate::markdown::code_style(token.kind)
            } else {
                text_style
            };
            let cursor_style = cursor.and_then(|(_, style)| {
                cursor_range
                    .filter(|(start, end)| *start < token_end && *end > token_start)
                    .map(|range| (range, style))
            });
            push_segmented_token(
                &mut spans,
                &token.text,
                token_start,
                start,
                end,
                style,
                cursor_style,
            );
        }
        token_start = token_end;
    }
    spans
}

#[allow(clippy::too_many_arguments)]
fn push_segmented_token(
    spans: &mut Vec<Span<'static>>,
    text: &str,
    token_start: usize,
    start: usize,
    end: usize,
    style: Style,
    cursor: Option<((usize, usize), Style)>,
) {
    let mut push = |start: usize, end: usize, style: Style| {
        if start < end {
            spans.push(Span::styled(
                crate::render::terminal_safe(&text[start - token_start..end - token_start]),
                style,
            ));
        }
    };
    let Some(((cursor_start, cursor_end), cursor_style)) = cursor else {
        push(start, end, style);
        return;
    };
    let cursor_start = cursor_start.max(start);
    let cursor_end = cursor_end.min(end);
    if cursor_start >= cursor_end {
        push(start, end, style);
        return;
    }
    push(start, cursor_start, style);
    push(cursor_start, cursor_end, cursor_style);
    push(cursor_end, end, style);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    #[test]
    fn cursor_style_overrides_the_syntax_style() {
        let source = r#"{"enabled": true}"#;
        let tokens = syntax_tokens(source, Some("json"));
        let spans = highlighted_spans(
            source,
            (0, source.len()),
            tokens.as_deref(),
            Style::default(),
            Some((
                source.find("true").unwrap(),
                Style::default().bg(Color::White),
            )),
        );

        assert!(
            spans
                .iter()
                .any(|span| span.content == "t" && span.style.bg == Some(Color::White))
        );
    }
}
