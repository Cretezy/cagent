use std::io::Write;

use super::composer::{
    next_grapheme, next_word_boundary, normalize_cursor, previous_grapheme, previous_word_boundary,
};

/// The context-free part of the composer input.
///
/// This deliberately knows nothing about attachments, slash completion, or
/// submission. Those behaviors belong to the owner of the input (the normal
/// composer or the observer command palette), while this type keeps their
/// text-editing behavior identical.
#[derive(Clone, Debug, Default)]
pub(super) struct ComposerEditor {
    text: String,
    cursor: usize,
    preferred_column: Option<usize>,
    undo: Vec<TextSnapshot>,
}

#[derive(Clone, Debug)]
struct TextSnapshot {
    text: String,
    cursor: usize,
}

impl ComposerEditor {
    pub(super) fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let cursor = text.len();
        Self {
            text,
            cursor,
            preferred_column: None,
            undo: Vec::new(),
        }
    }

    pub(super) fn from_parts(
        text: impl Into<String>,
        cursor: usize,
        preferred_column: Option<usize>,
    ) -> Self {
        let text = text.into();
        Self {
            cursor: normalize_cursor(&text, cursor),
            text,
            preferred_column,
            undo: Vec::new(),
        }
    }

    pub(super) fn text(&self) -> &str {
        &self.text
    }

    pub(super) fn cursor(&self) -> usize {
        self.cursor
    }

    pub(super) fn preferred_column(&self) -> Option<usize> {
        self.preferred_column
    }

    pub(super) fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.text.len();
        self.preferred_column = None;
        self.undo.clear();
    }

    fn checkpoint(&mut self) {
        if self.undo.len() >= 100 {
            self.undo.remove(0);
        }
        self.undo.push(TextSnapshot {
            text: self.text.clone(),
            cursor: self.cursor,
        });
    }

    pub(super) fn insert(&mut self, value: &str) {
        if value.is_empty() {
            return;
        }
        self.checkpoint();
        self.text.insert_str(self.cursor, value);
        self.cursor += value.len();
        self.preferred_column = None;
    }

    pub(super) fn delete_range(&mut self, start: usize, end: usize) {
        if start >= end || end > self.text.len() {
            return;
        }
        self.checkpoint();
        self.text.replace_range(start..end, "");
        self.cursor = start;
        self.preferred_column = None;
    }

    pub(super) fn backspace(&mut self) {
        let start = previous_grapheme(&self.text, self.cursor);
        self.delete_range(start, self.cursor);
    }

    pub(super) fn delete_forward(&mut self) {
        let end = next_grapheme(&self.text, self.cursor);
        self.delete_range(self.cursor, end);
    }

    pub(super) fn delete_previous_word(&mut self) {
        let start = previous_word_boundary(&self.text, self.cursor);
        self.delete_range(start, self.cursor);
    }

    pub(super) fn backspace_range(&self) -> Option<(usize, usize)> {
        (self.cursor > 0).then_some((previous_grapheme(&self.text, self.cursor), self.cursor))
    }

    pub(super) fn delete_forward_range(&self) -> Option<(usize, usize)> {
        (self.cursor < self.text.len())
            .then_some((self.cursor, next_grapheme(&self.text, self.cursor)))
    }

    pub(super) fn move_left(&mut self) {
        self.cursor = previous_grapheme(&self.text, self.cursor);
        self.preferred_column = None;
    }

    pub(super) fn move_right(&mut self) {
        self.cursor = next_grapheme(&self.text, self.cursor);
        self.preferred_column = None;
    }

    pub(super) fn move_word_left(&mut self) {
        self.cursor = previous_word_boundary(&self.text, self.cursor);
        self.preferred_column = None;
    }

    pub(super) fn move_word_right(&mut self) {
        self.cursor = next_word_boundary(&self.text, self.cursor);
        self.preferred_column = None;
    }

    pub(super) fn move_to_line_start(&mut self) {
        self.cursor = self.text[..self.cursor]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        self.preferred_column = None;
    }

    pub(super) fn move_to_line_end(&mut self) {
        self.cursor = self.text[self.cursor..]
            .find('\n')
            .map_or(self.text.len(), |index| self.cursor + index);
        self.preferred_column = None;
    }

    pub(super) fn undo(&mut self) {
        if let Some(snapshot) = self.undo.pop() {
            self.text = snapshot.text;
            self.cursor = snapshot.cursor;
            self.preferred_column = None;
        }
    }
}

pub(super) fn edit_draft_in_system_editor(draft: &str) -> Result<String, String> {
    let editor =
        std::env::var("EDITOR").map_err(|_| "editor unavailable: $EDITOR is not set".to_owned())?;
    let command = split_editor_command(&editor)?;
    let (program, arguments) = command
        .split_first()
        .ok_or_else(|| "editor unavailable: $EDITOR is empty".to_owned())?;
    let mut file = tempfile::NamedTempFile::new()
        .map_err(|error| format!("editor temporary file: {error}"))?;
    file.write_all(draft.as_bytes())
        .and_then(|()| file.flush())
        .map_err(|error| format!("editor temporary file: {error}"))?;
    let status = std::process::Command::new(program)
        .args(arguments)
        .arg(file.path())
        .status()
        .map_err(|error| format!("editor could not start: {error}"))?;
    if !status.success() {
        return Err(format!(
            "editor exited without saving ({})",
            status
                .code()
                .map_or_else(|| "terminated by signal".into(), |code| code.to_string())
        ));
    }
    let bytes = std::fs::read(file.path())
        .map_err(|error| format!("editor draft could not be read: {error}"))?;
    String::from_utf8(bytes).map_err(|_| "editor draft is not valid UTF-8".into())
}

fn split_editor_command(command: &str) -> Result<Vec<String>, String> {
    let mut arguments = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut token_started = false;
    let mut characters = command.chars();
    while let Some(character) = characters.next() {
        match (quote, character) {
            (None, character) if character.is_whitespace() => {
                if token_started {
                    arguments.push(std::mem::take(&mut current));
                    token_started = false;
                }
            }
            (None, '\'' | '"') => {
                quote = Some(character);
                token_started = true;
            }
            (Some(active), character) if active == character => quote = None,
            (None | Some('"'), '\\') => {
                let escaped = characters
                    .next()
                    .ok_or_else(|| "invalid $EDITOR: trailing escape".to_owned())?;
                current.push(escaped);
                token_started = true;
            }
            (_, character) => {
                current.push(character);
                token_started = true;
            }
        }
    }
    if quote.is_some() {
        return Err("invalid $EDITOR: unclosed quote".into());
    }
    if token_started {
        arguments.push(current);
    }
    Ok(arguments)
}

#[cfg(test)]
mod tests {
    use super::{ComposerEditor, split_editor_command};

    #[test]
    fn parses_editor_arguments_without_shell_expansion() {
        assert_eq!(
            split_editor_command(r#""/opt/My Editor/editor" --wait 'two words'"#).unwrap(),
            vec!["/opt/My Editor/editor", "--wait", "two words"]
        );
    }

    #[test]
    fn edits_lines_and_undoes_unicode_safely() {
        let mut editor = ComposerEditor::new("one\ntwo 👩‍💻");
        editor.move_to_line_start();
        assert_eq!(editor.cursor(), "one\n".len());
        editor.move_to_line_end();
        assert_eq!(editor.cursor(), "one\ntwo 👩‍💻".len());

        editor.backspace();
        assert_eq!(editor.text(), "one\ntwo ");
        editor.undo();
        assert_eq!(editor.text(), "one\ntwo 👩‍💻");

        editor.delete_forward();
        assert_eq!(editor.text(), "one\ntwo 👩‍💻");
        editor.move_to_line_start();
        editor.delete_forward();
        assert_eq!(editor.text(), "one\nwo 👩‍💻");
    }

    #[test]
    fn moves_and_deletes_by_words() {
        let mut editor = ComposerEditor::new("/copy slack now");
        editor.move_word_left();
        assert_eq!(editor.cursor(), "/copy slack ".len());
        editor.move_word_left();
        assert_eq!(editor.cursor(), "/copy ".len());
        editor.delete_previous_word();
        assert_eq!(editor.text(), "slack now");

        editor.move_word_right();
        assert_eq!(editor.cursor(), "slack".len());
        editor.move_to_line_start();
        editor.insert("prefix ");
        assert_eq!(editor.text(), "prefix slack now");
    }
}
