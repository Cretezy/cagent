use super::composer::{
    next_grapheme, next_word_boundary, normalize_cursor, previous_grapheme, previous_word_boundary,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// A small, reusable single-line editor for anchored inputs such as menu
/// filters. The composer intentionally has its own multiline editor.
#[derive(Clone, Debug)]
pub(crate) struct SingleLineInput {
    value: String,
    cursor: usize,
    syntax_language: Option<String>,
}

impl SingleLineInput {
    pub(super) fn new(value: String, cursor: usize) -> Self {
        let cursor = normalize_cursor(&value, cursor);
        Self {
            value,
            cursor,
            syntax_language: None,
        }
    }

    pub(super) fn with_syntax_language(mut self, language: impl Into<String>) -> Self {
        self.syntax_language = Some(language.into());
        self
    }

    pub(super) fn text(&self) -> &str {
        &self.value
    }

    pub(super) fn cursor(&self) -> usize {
        self.cursor
    }

    pub(super) fn set_cursor(&mut self, cursor: usize) {
        self.cursor = normalize_cursor(&self.value, cursor);
    }

    /// Insert pasted text while preserving this editor's one-line invariant.
    pub(super) fn insert_text(&mut self, text: &str) {
        let mut normalized = String::with_capacity(text.len());
        let mut characters = text.chars().peekable();
        while let Some(character) = characters.next() {
            match character {
                '\r' => {
                    if characters.peek() == Some(&'\n') {
                        characters.next();
                    }
                    normalized.push(' ');
                }
                '\n' => normalized.push(' '),
                _ => normalized.push(character),
            }
        }
        self.value.insert_str(self.cursor, &normalized);
        self.cursor += normalized.len();
    }

    pub(super) fn handle_key(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && !key.modifiers.contains(KeyModifiers::SHIFT)
        {
            match key.code {
                KeyCode::Char('a' | 'A') => {
                    self.cursor = 0;
                    return true;
                }
                KeyCode::Char('e' | 'E') => {
                    self.cursor = self.value.len();
                    return true;
                }
                KeyCode::Char('w' | 'W') => {
                    self.delete_previous(true);
                    return true;
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Backspace
                if key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && !key.modifiers.contains(KeyModifiers::SHIFT) =>
            {
                self.delete_previous(true);
                true
            }
            KeyCode::Backspace => {
                self.delete_previous(false);
                true
            }
            KeyCode::Delete => {
                let end = next_grapheme(&self.value, self.cursor);
                self.value.drain(self.cursor..end);
                true
            }
            KeyCode::Left
                if key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && !key.modifiers.contains(KeyModifiers::SHIFT) =>
            {
                self.cursor = previous_word_boundary(&self.value, self.cursor);
                true
            }
            KeyCode::Right
                if key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && !key.modifiers.contains(KeyModifiers::SHIFT) =>
            {
                self.cursor = next_word_boundary(&self.value, self.cursor);
                true
            }
            KeyCode::Left => {
                self.cursor = previous_grapheme(&self.value, self.cursor);
                true
            }
            KeyCode::Right => {
                self.cursor = next_grapheme(&self.value, self.cursor);
                true
            }
            KeyCode::Char(character)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                self.value.insert(self.cursor, character);
                self.cursor += character.len_utf8();
                true
            }
            _ => false,
        }
    }

    pub(super) fn into_parts(self) -> (String, usize) {
        (self.value, self.cursor)
    }

    /// Renders a horizontal window that always contains the cursor.
    pub(super) fn render_bounded(
        &self,
        width: usize,
        text_style: Style,
        cursor_style: Style,
    ) -> Line<'static> {
        if width == 0 {
            return Line::default();
        }
        let (start, end) = self.bounded_range(width);
        let tokens =
            crate::app::input_syntax::syntax_tokens(&self.value, self.syntax_language.as_deref());
        let mut spans = crate::app::input_syntax::highlighted_spans(
            &self.value,
            (start, end),
            tokens.as_deref(),
            text_style,
            (self.cursor < end).then_some((self.cursor, cursor_style)),
        );
        if self.cursor == self.value.len() && Line::from(spans.clone()).width() < width {
            spans.push(Span::styled(" ", cursor_style));
        }
        Line::from(spans)
    }

    pub(super) fn render_bounded_with_placeholder(
        &self,
        width: usize,
        placeholder: &str,
        text_style: Style,
        placeholder_style: Style,
        cursor_style: Style,
    ) -> Line<'static> {
        if !self.value.is_empty() {
            return self.render_bounded(width, text_style, cursor_style);
        }
        let mut placeholder = placeholder.to_owned();
        placeholder.truncate(crate::render::byte_offset_at_display_column(
            &placeholder,
            width,
        ));
        let split = placeholder.chars().next().map_or(0, char::len_utf8);
        Line::from(vec![
            Span::styled(placeholder[..split].to_owned(), cursor_style),
            Span::styled(placeholder[split..].to_owned(), placeholder_style),
        ])
    }

    /// Returns the source cursor boundary represented by a column in the
    /// rendered input. A bounded input uses the same cursor-following window
    /// as `render_bounded`.
    pub(super) fn cursor_at_display_column(&self, column: usize, width: Option<usize>) -> usize {
        let (start, end) = width.map_or((0, self.value.len()), |width| self.bounded_range(width));
        start + crate::render::byte_offset_at_display_column(&self.value[start..end], column)
    }

    fn bounded_range(&self, width: usize) -> (usize, usize) {
        if width == 0 {
            return (self.cursor, self.cursor);
        }
        let cursor_column = self.value[..self.cursor].width();
        let window_start = cursor_column.saturating_sub(width.saturating_sub(1));
        let mut source_column: usize = 0;
        let mut start = self.value.len();
        let mut end = self.value.len();
        let mut used: usize = 0;
        for (index, grapheme) in self.value.grapheme_indices(true) {
            let grapheme_width = grapheme.width();
            let next_column = source_column.saturating_add(grapheme_width);
            if next_column <= window_start {
                source_column = next_column;
                continue;
            }
            if used > 0 && used.saturating_add(grapheme_width) > width {
                break;
            }
            start = start.min(index);
            end = index + grapheme.len();
            used = used.saturating_add(grapheme_width);
            source_column = next_column;
        }
        if start == self.value.len() {
            (self.value.len(), self.value.len())
        } else {
            (start, end)
        }
    }

    fn delete_previous(&mut self, word: bool) {
        let start = if word {
            previous_word_boundary(&self.value, self.cursor)
        } else {
            previous_grapheme(&self.value, self.cursor)
        };
        self.value.drain(start..self.cursor);
        self.cursor = start;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supports_line_and_word_editing() {
        let mut input = SingleLineInput::new("one two three".into(), "one two three".len());
        assert!(input.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL,)));
        assert_eq!(input.cursor, 0);
        assert!(input.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL,)));
        assert!(input.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL,)));
        assert_eq!(input.into_parts(), ("one two ".into(), "one two ".len()));

        let mut input = SingleLineInput::new("one two three".into(), "one two ".len());
        assert!(input.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::ALT,)));
        assert_eq!(input.cursor, "one two three".len());
        assert!(input.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL,)));
        assert_eq!(input.cursor, "one two ".len());
    }

    #[test]
    fn renders_a_visible_cursor_at_the_end() {
        let input = SingleLineInput::new("filter".into(), 6);
        let line = input.render_bounded(80, Style::default(), Style::default().reversed());
        assert_eq!(line.to_string(), "filter ");
        assert_eq!(
            line.spans.last().unwrap().style,
            Style::default().reversed()
        );
    }

    #[test]
    fn bounded_render_keeps_the_cursor_visible() {
        let input = SingleLineInput::new("abcdefghijklmnopqrstuvwxyz".into(), 26);
        let line = input.render_bounded(8, Style::default(), Style::default().reversed());
        assert!(line.width() <= 8);
        assert_eq!(
            line.spans.last().unwrap().style,
            Style::default().reversed()
        );
    }

    #[test]
    fn highlights_tagged_input_without_changing_cursor_style() {
        let source = r#"{"enabled":true}"#;
        let input = SingleLineInput::new(source.into(), 0).with_syntax_language("json");
        let line = input.render_bounded(80, Style::default(), Style::default().reversed());

        assert!(line.spans.iter().any(|span| {
            span.content == "true"
                && span.style
                    == crate::markdown::code_style(
                        cagent_agent::presentation::CodeTokenKind::Constant,
                    )
        }));
        assert_eq!(line.spans[0].style, Style::default().reversed());
    }

    #[test]
    fn click_mapping_preserves_grapheme_boundaries_and_bounded_windows() {
        let input = SingleLineInput::new("a界e\u{301}".into(), 0);
        assert_eq!(input.cursor_at_display_column(0, None), 0);
        assert_eq!(input.cursor_at_display_column(1, None), 1);
        // Both cells occupied by the wide grapheme select its leading boundary.
        assert_eq!(input.cursor_at_display_column(2, None), 1);
        assert_eq!(input.cursor_at_display_column(3, None), "a界".len());
        assert_eq!(
            input.cursor_at_display_column(99, None),
            "a界e\u{301}".len()
        );

        let input = SingleLineInput::new("abcdefgh".into(), 8);
        assert_eq!(input.cursor_at_display_column(0, Some(4)), 5);
        assert_eq!(input.cursor_at_display_column(99, Some(4)), 8);
    }

    #[test]
    fn accepts_spaces_anywhere_in_the_input() {
        let mut input = SingleLineInput::new(String::new(), 0);
        assert!(input.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE,)));
        assert_eq!(input.into_parts(), (" ".into(), 1));

        let mut input = SingleLineInput::new("one".into(), 3);
        assert!(input.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE,)));
        assert!(input.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE,)));
        assert_eq!(input.into_parts(), ("one t".into(), 5));
    }

    #[test]
    fn paste_remains_on_one_line_and_normalizes_line_endings() {
        let mut input = SingleLineInput::new("before after".into(), "before ".len());
        input.insert_text("one\r\ntwo\nthree\rfour");
        assert_eq!(
            input.into_parts(),
            (
                "before one two three fourafter".into(),
                "before one two three four".len()
            )
        );
    }
}
