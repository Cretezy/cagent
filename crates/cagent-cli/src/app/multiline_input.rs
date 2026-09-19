use super::composer::{
    next_grapheme, next_word_boundary, normalize_cursor, previous_grapheme, previous_word_boundary,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

#[cfg(test)]
use super::scroll::ScrollViewHit;
use super::scroll::{ScrollViewAction, ScrollViewLayout, ScrollViewState, ScrollViewWidget};

pub(crate) const INTERACTION_NOTE_MAX_VISUAL_ROWS: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InteractionNoteOutcome {
    Ignored,
    Edited,
    Closed,
    Discarded,
    Submit,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct InteractionNoteView<'a> {
    pub(crate) text: &'a str,
    pub(crate) active: bool,
    pub(crate) cursor: usize,
}

enum InteractionNoteValue<'a> {
    Required(&'a mut String),
    Optional(&'a mut Option<String>),
}

/// Shared editing state and behavior for notes attached to interaction menus.
///
/// The protocol deliberately represents question notes as `Option<String>` while
/// permission and plan surfaces keep an empty draft string. This adapter hides
/// that storage distinction so all three interactions use the same editor.
pub(crate) struct InteractionNote<'a> {
    value: InteractionNoteValue<'a>,
    active: &'a mut bool,
    cursor: &'a mut usize,
}

impl<'a> InteractionNote<'a> {
    pub(crate) fn required(
        value: &'a mut String,
        active: &'a mut bool,
        cursor: &'a mut usize,
    ) -> Self {
        Self {
            value: InteractionNoteValue::Required(value),
            active,
            cursor,
        }
    }

    pub(crate) fn optional(
        value: &'a mut Option<String>,
        active: &'a mut bool,
        cursor: &'a mut usize,
    ) -> Self {
        Self {
            value: InteractionNoteValue::Optional(value),
            active,
            cursor,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn is_active(&self) -> bool {
        *self.active
    }

    pub(crate) fn text(&self) -> &str {
        match &self.value {
            InteractionNoteValue::Required(value) => value,
            InteractionNoteValue::Optional(value) => value.as_deref().unwrap_or_default(),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn cursor(&self) -> usize {
        *self.cursor
    }

    #[allow(dead_code)]
    pub(crate) fn trimmed(&self) -> Option<&str> {
        let value = self.text().trim();
        (!value.is_empty()).then_some(value)
    }

    pub(crate) fn open_at_end(&mut self) {
        *self.active = true;
        *self.cursor = self.text().len();
    }

    pub(crate) fn close(&mut self) {
        *self.active = false;
    }

    pub(crate) fn discard(&mut self) {
        self.replace(String::new(), 0);
        *self.active = false;
    }

    pub(crate) fn insert_text(&mut self, text: &str, width: u16) {
        self.edit(width, |input, width, capacity| {
            input.insert_text_in_view(text, width, capacity);
        });
    }

    pub(crate) fn apply_scroll_action(&mut self, action: ScrollViewAction, width: u16) {
        self.edit(width, |input, width, capacity| {
            input.apply_scroll_action(action, width, capacity);
        });
    }

    pub(crate) fn set_cursor(&mut self, cursor: usize, width: u16) {
        self.edit(width, |input, width, capacity| {
            input.set_cursor_in_view(cursor, width, capacity);
        });
    }

    pub(crate) fn handle_key(
        &mut self,
        key: KeyEvent,
        width: u16,
        submit: bool,
        insert_newline: bool,
        cancel: bool,
        escape: bool,
    ) -> InteractionNoteOutcome {
        if cancel && !escape {
            self.close();
            return InteractionNoteOutcome::Closed;
        }
        if escape || (matches!(key.code, KeyCode::Backspace) && self.text().is_empty()) {
            self.discard();
            return InteractionNoteOutcome::Discarded;
        }
        if insert_newline {
            self.edit(width, |input, _, _| input.insert_newline());
            return InteractionNoteOutcome::Edited;
        }
        if submit {
            return InteractionNoteOutcome::Submit;
        }
        if matches!(key.code, KeyCode::Tab) {
            self.close();
            return InteractionNoteOutcome::Closed;
        }
        let mut handled = false;
        self.edit(width, |input, width, capacity| {
            handled = input.handle_key_in_view(key, width, capacity);
        });
        if handled {
            InteractionNoteOutcome::Edited
        } else {
            InteractionNoteOutcome::Ignored
        }
    }

    fn edit(&mut self, width: u16, edit: impl FnOnce(&mut MultilineInput, u16, usize)) {
        let value = self.take();
        let input_width = width.max(1);
        let mut input = MultilineInput::new(value, *self.cursor);
        let capacity = input
            .visual_row_count(input_width)
            .clamp(1, INTERACTION_NOTE_MAX_VISUAL_ROWS);
        edit(&mut input, input_width, capacity);
        let (value, cursor) = input.into_parts();
        self.replace(value, cursor);
    }

    fn take(&mut self) -> String {
        match &mut self.value {
            InteractionNoteValue::Required(value) => std::mem::take(*value),
            InteractionNoteValue::Optional(value) => value.take().unwrap_or_default(),
        }
    }

    fn replace(&mut self, value: String, cursor: usize) {
        *self.cursor = cursor.min(value.len());
        match &mut self.value {
            InteractionNoteValue::Required(target) => **target = value,
            InteractionNoteValue::Optional(target) => {
                **target = (!value.is_empty()).then_some(value);
            }
        }
    }
}

/// A reusable multiline editor for anchored text surfaces.
#[derive(Clone, Debug)]
pub(crate) struct MultilineInput {
    value: String,
    cursor: usize,
    scroll: ScrollViewState,
    syntax_language: Option<String>,
}

impl MultilineInput {
    pub(crate) fn new(value: String, cursor: usize) -> Self {
        let cursor = normalize_cursor(&value, cursor);
        Self {
            value,
            cursor,
            scroll: ScrollViewState::default(),
            syntax_language: None,
        }
    }

    pub(crate) fn with_syntax_language(mut self, language: impl Into<String>) -> Self {
        self.syntax_language = Some(language.into());
        self
    }

    pub(super) fn handle_key(&mut self, key: KeyEvent) -> bool {
        let word_modifier = key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
            && !key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Char('a' | 'A') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_to_line_start();
                true
            }
            KeyCode::Char('e' | 'E') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_to_line_end();
                true
            }
            KeyCode::Char('w' | 'W') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.delete_previous(true);
                true
            }
            KeyCode::Enter if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT => {
                self.insert_newline();
                true
            }
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
                if self.cursor > 0 {
                    let start = previous_grapheme(&self.value, self.cursor);
                    self.value.drain(start..self.cursor);
                    self.cursor = start;
                }
                true
            }
            KeyCode::Delete => {
                if self.cursor < self.value.len() {
                    let end = next_grapheme(&self.value, self.cursor);
                    self.value.drain(self.cursor..end);
                }
                true
            }
            KeyCode::Left if word_modifier => {
                self.cursor = previous_word_boundary(&self.value, self.cursor);
                true
            }
            KeyCode::Right if word_modifier => {
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
            KeyCode::Up if key.modifiers.is_empty() => {
                self.move_vertically(false);
                true
            }
            KeyCode::Down if key.modifiers.is_empty() => {
                self.move_vertically(true);
                true
            }
            KeyCode::Home => {
                self.move_to_line_start();
                true
            }
            KeyCode::End => {
                self.move_to_line_end();
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

    pub(super) fn insert_newline(&mut self) {
        self.insert_text("\n");
    }

    pub(super) fn text(&self) -> &str {
        &self.value
    }

    pub(super) fn is_empty(&self) -> bool {
        self.value.is_empty()
    }

    pub(super) const fn cursor(&self) -> usize {
        self.cursor
    }

    pub(super) fn set_cursor(&mut self, cursor: usize) {
        self.cursor = normalize_cursor(&self.value, cursor);
    }

    pub(super) fn move_to_end(&mut self) {
        self.cursor = self.value.len();
    }

    /// Insert pasted text, normalizing all line-ending conventions to LF.
    pub(super) fn insert_text(&mut self, text: &str) {
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        self.value.insert_str(self.cursor, &normalized);
        self.cursor += normalized.len();
    }

    pub(super) fn into_parts(self) -> (String, usize) {
        (self.value, self.cursor)
    }

    pub(super) fn visual_row_count(&self, width: u16) -> usize {
        self.rendered_rows(width).ranges.len()
    }

    /// Renders the editor through the shared visual-row scroll view. The
    /// returned content hits are source visual-row indexes and are therefore
    /// also the canonical click map for the editor.
    pub(super) fn render_scrolled(
        &self,
        width: u16,
        capacity: usize,
        empty_line: Option<&Line<'static>>,
    ) -> ScrollViewLayout {
        let rows = self.rendered_rows(width);
        let tokens =
            crate::app::input_syntax::syntax_tokens(&self.value, self.syntax_language.as_deref());
        let mut state = self.scroll;
        state.reconcile_focus(rows.ranges.len(), rows.cursor_row, capacity);
        ScrollViewWidget::new(capacity).render(&state, |row| {
            if self.value.is_empty()
                && let Some(line) = empty_line
            {
                return line.clone();
            }
            self.render_row(rows.ranges[row], row == rows.cursor_row, tokens.as_deref())
        })
    }

    /// Maps a source visual row and content column from `render_scrolled`
    /// back to a grapheme-safe source cursor boundary.
    pub(super) fn cursor_at_rendered_row(
        &self,
        width: u16,
        row: usize,
        column: usize,
    ) -> Option<usize> {
        let rows = self.rendered_rows(width);
        let (source_start, source_end) = *rows.ranges.get(row)?;
        Some(
            source_start
                + crate::render::byte_offset_at_display_column(
                    &self.value[source_start..source_end],
                    column,
                ),
        )
    }

    pub(super) fn handle_key_in_view(
        &mut self,
        key: KeyEvent,
        width: u16,
        capacity: usize,
    ) -> bool {
        let handled = self.handle_key(key);
        if handled {
            self.reconcile_view(width, capacity);
        }
        handled
    }

    pub(super) fn insert_text_in_view(&mut self, text: &str, width: u16, capacity: usize) {
        self.insert_text(text);
        self.reconcile_view(width, capacity);
    }

    pub(super) fn set_cursor_in_view(&mut self, cursor: usize, width: u16, capacity: usize) {
        self.set_cursor(cursor);
        self.reconcile_view(width, capacity);
    }

    pub(super) fn apply_scroll_action(
        &mut self,
        action: ScrollViewAction,
        width: u16,
        capacity: usize,
    ) -> bool {
        let page = isize::try_from(capacity.max(1)).unwrap_or(isize::MAX);
        let amount = match action {
            ScrollViewAction::Previous => -1,
            ScrollViewAction::Next => 1,
            ScrollViewAction::PagePrevious => -page,
            ScrollViewAction::PageNext => page,
            ScrollViewAction::WheelPrevious => -3,
            ScrollViewAction::WheelNext => 3,
            ScrollViewAction::Home => isize::MIN,
            ScrollViewAction::End => isize::MAX,
        };
        let before = self.cursor;
        self.move_by_rendered_rows(width, amount);
        self.reconcile_view(width, capacity);
        self.cursor != before
    }

    fn rendered_rows(&self, width: u16) -> RenderedRows {
        let ranges = crate::render::wrap_ranges(&self.value, width.saturating_sub(4).max(1));
        let cursor_row = crate::render::rendered_cursor_line(&self.value, &ranges, self.cursor)
            .map_or(0, |(row, _)| row);
        RenderedRows { ranges, cursor_row }
    }

    fn render_row(
        &self,
        (start, end): (usize, usize),
        has_cursor: bool,
        tokens: Option<&[cagent_agent::presentation::CodeToken]>,
    ) -> Line<'static> {
        let mut spans = vec![Span::raw("  ")];
        let cursor = has_cursor.then_some(self.cursor.clamp(start, end));
        spans.extend(crate::app::input_syntax::highlighted_spans(
            &self.value,
            (start, end),
            tokens,
            Style::default(),
            cursor
                .filter(|cursor| *cursor < end)
                .map(|cursor| (cursor, super::SEARCH_CURSOR_STYLE)),
        ));
        if cursor == Some(end) {
            spans.push(Span::styled(" ", super::SEARCH_CURSOR_STYLE));
        }
        Line::from(spans)
    }

    fn reconcile_view(&mut self, width: u16, capacity: usize) {
        let rows = self.rendered_rows(width);
        self.scroll
            .reconcile_focus(rows.ranges.len(), rows.cursor_row, capacity);
    }

    fn move_by_rendered_rows(&mut self, width: u16, amount: isize) {
        let rows = self.rendered_rows(width);
        let current = rows.cursor_row;
        let target = if amount == isize::MIN {
            0
        } else if amount == isize::MAX {
            rows.ranges.len().saturating_sub(1)
        } else if amount.is_negative() {
            current.saturating_sub(amount.unsigned_abs())
        } else {
            current
                .saturating_add(amount.unsigned_abs())
                .min(rows.ranges.len().saturating_sub(1))
        };
        let (current_start, current_end) = rows.ranges[current];
        let column = self.value[current_start..self.cursor.min(current_end)].width();
        let (target_start, target_end) = rows.ranges[target];
        self.cursor = target_start
            + crate::render::byte_offset_at_display_column(
                &self.value[target_start..target_end],
                column,
            );
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

    fn move_to_line_start(&mut self) {
        self.cursor = self.value[..self.cursor]
            .rfind('\n')
            .map_or(0, |index| index + 1);
    }

    fn move_to_line_end(&mut self) {
        self.cursor = self.value[self.cursor..]
            .find('\n')
            .map_or(self.value.len(), |index| self.cursor + index);
    }

    fn move_vertically(&mut self, down: bool) {
        let line_start = self.value[..self.cursor]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        let column = self.value[line_start..self.cursor].width();
        let (target_start, target_end) = if down {
            let Some(newline) = self.value[self.cursor..]
                .find('\n')
                .map(|index| self.cursor + index)
            else {
                return;
            };
            let target_start = newline + 1;
            let target_end = self.value[target_start..]
                .find('\n')
                .map_or(self.value.len(), |index| target_start + index);
            (target_start, target_end)
        } else {
            let Some(previous_end) = line_start.checked_sub(1) else {
                return;
            };
            let target_start = self.value[..previous_end]
                .rfind('\n')
                .map_or(0, |index| index + 1);
            (target_start, previous_end)
        };
        self.cursor = target_start
            + crate::render::byte_offset_at_display_column(
                &self.value[target_start..target_end],
                column,
            );
    }
}

struct RenderedRows {
    ranges: Vec<(usize, usize)>,
    cursor_row: usize,
}

/// Recognizes the terminal encodings commonly used for Ctrl+Enter.
pub(super) fn is_control_enter(key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Enter => key.modifiers.contains(KeyModifiers::CONTROL),
        KeyCode::Char('\n' | '\r') => {
            key.modifiers.is_empty() || key.modifiers.contains(KeyModifiers::CONTROL)
        }
        KeyCode::Char('j' | 'J' | 'm' | 'M') => {
            key.modifiers.contains(KeyModifiers::CONTROL)
                && !key
                    .modifiers
                    .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interaction_note_opens_at_end_and_normalizes_paste() {
        let mut value = "existing".to_owned();
        let mut active = false;
        let mut cursor = 0;
        let mut note = InteractionNote::required(&mut value, &mut active, &mut cursor);
        note.open_at_end();
        note.insert_text("\r\nmore", 40);
        assert!(note.is_active());
        assert_eq!(note.text(), "existing\nmore");
        assert_eq!(note.cursor(), note.text().len());
    }

    #[test]
    fn interaction_note_tab_preserves_and_escape_discards() {
        let mut value = "keep me".to_owned();
        let mut active = true;
        let mut cursor = value.len();
        let mut note = InteractionNote::required(&mut value, &mut active, &mut cursor);
        let tab = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(
            note.handle_key(tab, 40, false, false, false, false),
            InteractionNoteOutcome::Closed
        );
        assert_eq!(note.text(), "keep me");
        note.open_at_end();
        assert_eq!(
            note.handle_key(
                KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
                40,
                false,
                false,
                false,
                true,
            ),
            InteractionNoteOutcome::Discarded
        );
        assert_eq!(note.text(), "");
        assert_eq!(note.cursor(), 0);
    }

    #[test]
    fn interaction_note_empty_backspace_dismisses_and_trimmed_omits_blank() {
        let mut value = Some("  \n".to_owned());
        let mut active = true;
        let mut cursor = value.as_deref().unwrap().len();
        let mut note = InteractionNote::optional(&mut value, &mut active, &mut cursor);
        assert_eq!(note.trimmed(), None);
        note.discard();
        note.open_at_end();
        assert_eq!(
            note.handle_key(
                KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
                40,
                false,
                false,
                false,
                false,
            ),
            InteractionNoteOutcome::Discarded
        );
        assert!(!note.is_active());
        assert_eq!(note.trimmed(), None);
    }

    #[test]
    fn interaction_note_reports_newline_and_submit() {
        let mut value = String::new();
        let mut active = true;
        let mut cursor = 0;
        let mut note = InteractionNote::required(&mut value, &mut active, &mut cursor);
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            note.handle_key(enter, 40, false, true, false, false),
            InteractionNoteOutcome::Edited
        );
        assert_eq!(note.text(), "\n");
        assert_eq!(
            note.handle_key(enter, 40, true, false, false, false),
            InteractionNoteOutcome::Submit
        );
    }

    #[test]
    fn supports_word_and_line_navigation() {
        let mut input = MultilineInput::new("one two\nthree four\nlast".into(), 23);
        assert!(input.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL)));
        assert_eq!(input.cursor, "one two\nthree four\n".len());
        assert!(input.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::ALT)));
        assert_eq!(input.cursor, 23);
        assert!(input.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL,)));
        assert_eq!(input.cursor, "one two\nthree four\n".len());
        assert_eq!(input.value, "one two\nthree four\n");

        input = MultilineInput::new("one two\nthree four\nla".into(), 21);
        assert!(input.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)));
        assert_eq!(input.cursor, "one two\nth".len());
        assert!(input.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)));
        assert_eq!(input.cursor, 21);
    }

    #[test]
    fn recognizes_control_enter_encodings() {
        assert!(is_control_enter(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::CONTROL,
        )));
        assert!(is_control_enter(KeyEvent::new(
            KeyCode::Char('j'),
            KeyModifiers::CONTROL,
        )));
        assert!(!is_control_enter(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
    }

    #[test]
    fn vertical_navigation_visits_empty_lines() {
        let mut input = MultilineInput::new("first\n\nlast\n".into(), 0);
        input.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(input.cursor, "first\n".len());
        input.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(input.cursor, "first\n\n".len());
        input.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(input.cursor, "first\n\nlast\n".len());
    }

    #[test]
    fn paste_preserves_lines_and_normalizes_line_endings() {
        let mut input = MultilineInput::new("before\nafter".into(), "before\n".len());
        input.insert_text("one\r\ntwo\rthree");
        assert_eq!(
            input.into_parts(),
            (
                "before\none\ntwo\nthreeafter".into(),
                "before\none\ntwo\nthree".len()
            )
        );
    }

    #[test]
    fn enter_and_insert_newline_share_the_same_insertion_path() {
        let mut by_key = MultilineInput::new("beforeafter".into(), "before".len());
        let mut by_method = MultilineInput::new("beforeafter".into(), "before".len());

        assert!(by_key.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        by_method.insert_newline();

        assert_eq!(by_key.into_parts(), by_method.into_parts());
    }

    #[test]
    fn highlights_tagged_rows_without_changing_cursor_style() {
        let source = "{\n  \"enabled\": true\n}";
        let input = MultilineInput::new(source.into(), 0).with_syntax_language("json");
        let tokens =
            crate::app::input_syntax::syntax_tokens(source, input.syntax_language.as_deref());
        let line = input.render_row((0, source.len()), true, tokens.as_deref());

        assert!(line.spans.iter().any(|span| {
            span.content == "true"
                && span.style
                    == crate::markdown::code_style(
                        cagent_agent::presentation::CodeTokenKind::Constant,
                    )
        }));
        assert_eq!(line.spans[1].style, crate::app::SEARCH_CURSOR_STYLE);
    }

    #[test]
    fn scrolled_render_keeps_the_cursor_visible() {
        let source = (0..20)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let input = MultilineInput::new(source.clone(), source.len());
        let rendered = input.render_scrolled(80, 4, None);

        assert_eq!(rendered.lines.len(), 6);
        assert!(rendered.lines[4].to_string().contains("line 19"));
        assert_eq!(
            rendered.lines[4].spans.last().unwrap().style,
            crate::app::SEARCH_CURSOR_STYLE
        );
        assert_eq!(
            rendered.hit(0),
            ScrollViewHit::Indicator(ScrollViewAction::PagePrevious)
        );
    }

    #[test]
    fn click_mapping_handles_empty_wrapped_and_clipped_rows() {
        let input = MultilineInput::new("ab界\n\nuvwxyz".into(), 0);
        // Width eight leaves four content columns after the editor gutter.
        assert_eq!(input.cursor_at_rendered_row(8, 0, 2), Some(2));
        assert_eq!(input.cursor_at_rendered_row(8, 1, 99), Some(6));
        assert_eq!(input.cursor_at_rendered_row(8, 2, 99), Some(11));

        let source = "zero\none\ntwo\nthree";
        let input = MultilineInput::new(source.into(), source.len());
        assert_eq!(
            input.cursor_at_rendered_row(80, 2, 0),
            Some("zero\none\n".len())
        );
        assert_eq!(input.cursor_at_rendered_row(80, 3, 99), Some(source.len()));
    }

    #[test]
    fn cursor_movement_does_not_change_wrapping_or_empty_rows() {
        let source = "abcdefgh\n\nijklmnop";
        let before = MultilineInput::new(source.into(), 4).render_scrolled(8, 20, None);
        let after = MultilineInput::new(source.into(), 5).render_scrolled(8, 20, None);
        let text = |layout: ScrollViewLayout| {
            layout.lines[1..layout.lines.len() - 1]
                .iter()
                .map(|line| line.to_string().trim_end().to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(text(before), text(after));
        assert_eq!(
            text(MultilineInput::new(source.into(), 9).render_scrolled(8, 20, None))[2],
            ""
        );
    }

    #[test]
    fn trailing_newline_keeps_the_previous_row_visible_when_it_fits() {
        let source = "aaaaa\n";
        let rendered =
            MultilineInput::new(source.into(), source.len()).render_scrolled(80, 2, None);

        assert_eq!(rendered.lines.len(), 4);
        assert_eq!(rendered.lines[1].to_string().trim(), "aaaaa");
        assert_eq!(rendered.lines[2].to_string().trim(), "");
        assert_eq!(
            rendered.lines[2].spans.last().unwrap().style,
            crate::app::SEARCH_CURSOR_STYLE
        );
    }

    #[test]
    fn caret_navigation_uses_list_style_scroll_padding() {
        let source = (0..10)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut input = MultilineInput::new(source, 0);
        let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);

        for _ in 0..3 {
            input.handle_key_in_view(down, 80, 5);
        }
        assert_eq!(input.scroll.offset, 0);
        input.handle_key_in_view(down, 80, 5);
        assert_eq!(input.scroll.offset, 1);

        input.handle_key_in_view(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), 80, 5);
        assert_eq!(input.scroll.offset, 1);
        input.handle_key_in_view(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), 80, 5);
        input.handle_key_in_view(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), 80, 5);
        assert_eq!(input.scroll.offset, 0);
    }

    #[test]
    fn page_and_wheel_actions_move_by_rendered_rows() {
        let source = (0..12)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut input = MultilineInput::new(source, 0);

        assert!(input.apply_scroll_action(ScrollViewAction::PageNext, 80, 5));
        assert!(input.text()[input.cursor()..].starts_with("line 5"));
        assert_eq!(input.scroll.offset, 2);
        assert!(input.apply_scroll_action(ScrollViewAction::WheelNext, 80, 5));
        assert!(input.text()[input.cursor()..].starts_with("line 8"));
        assert_eq!(input.scroll.offset, 5);
        assert!(input.apply_scroll_action(ScrollViewAction::PagePrevious, 80, 5));
        assert!(input.text()[input.cursor()..].starts_with("line 3"));
    }
}
