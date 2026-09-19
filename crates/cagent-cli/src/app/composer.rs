//! Shared Unicode cursor handling for the composer and anchored text inputs.

use unicode_segmentation::UnicodeSegmentation;

/// Normalize a byte-offset cursor before it is used to index text.
pub(crate) fn normalize_cursor(value: &str, cursor: usize) -> usize {
    let cursor = cursor.min(value.len());
    if value.is_char_boundary(cursor) {
        cursor
    } else {
        value
            .char_indices()
            .rfind(|(index, _)| *index < cursor)
            .map_or(0, |(index, _)| index)
    }
}

/// Return the cursor position used when opening an anchored editor.
pub(crate) fn cursor_at_end(value: &str) -> usize {
    value.len()
}

pub(crate) fn previous_grapheme(value: &str, cursor: usize) -> usize {
    value[..cursor.min(value.len())]
        .grapheme_indices(true)
        .next_back()
        .map_or(0, |(offset, _)| offset)
}

pub(crate) fn next_grapheme(value: &str, cursor: usize) -> usize {
    value[cursor.min(value.len())..]
        .graphemes(true)
        .next()
        .map_or(value.len(), |grapheme| cursor + grapheme.len())
}

pub(crate) fn previous_word_boundary(value: &str, cursor: usize) -> usize {
    let mut index = cursor.min(value.len());
    while index > 0 {
        let (start, grapheme) = value[..index].grapheme_indices(true).next_back().unwrap();
        if !grapheme.chars().all(char::is_whitespace) {
            break;
        }
        index = start;
    }
    while index > 0 {
        let (start, grapheme) = value[..index].grapheme_indices(true).next_back().unwrap();
        if grapheme.chars().all(char::is_whitespace) {
            break;
        }
        index = start;
    }
    index
}

pub(crate) fn next_word_boundary(value: &str, cursor: usize) -> usize {
    let mut index = cursor.min(value.len());
    while index < value.len() {
        let grapheme = value[index..].graphemes(true).next().unwrap();
        if !grapheme.chars().all(char::is_whitespace) {
            break;
        }
        index += grapheme.len();
    }
    while index < value.len() {
        let grapheme = value[index..].graphemes(true).next().unwrap();
        if grapheme.chars().all(char::is_whitespace) {
            break;
        }
        index += grapheme.len();
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_cursors_to_character_boundaries() {
        let value = "a👩‍💻b";
        assert_eq!(normalize_cursor(value, 2), 1);
        assert_eq!(normalize_cursor(value, usize::MAX), value.len());
    }

    #[test]
    fn word_navigation_keeps_emoji_graphemes_whole() {
        let value = "hi 👩‍💻 world";
        assert_eq!(previous_grapheme(value, value.len()), "hi 👩‍💻 worl".len());
        assert_eq!(previous_word_boundary(value, value.len()), "hi 👩‍💻 ".len());
        assert_eq!(next_word_boundary(value, 0), "hi".len());
    }
}
