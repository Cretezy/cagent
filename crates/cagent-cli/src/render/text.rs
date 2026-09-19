use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub(crate) fn padded_status_line(line: Line<'static>) -> Line<'static> {
    let style = line.style;
    let mut spans = vec![Span::raw("  ")];
    spans.extend(line.spans);
    spans.push(Span::raw("  "));
    Line::from(spans).style(style)
}

pub(crate) fn visual_text_rows(text: &str, width: u16) -> u16 {
    u16::try_from(wrap_ranges(text, width).len()).unwrap_or(u16::MAX)
}

pub(crate) fn wrap_ranges(text: &str, width: u16) -> Vec<(usize, usize)> {
    let width = usize::from(width.max(1));
    let mut ranges = Vec::new();
    let mut start = 0;
    let mut used = 0;
    let mut last_break = None;
    for (offset, grapheme) in text.grapheme_indices(true) {
        if grapheme == "\n" {
            ranges.push((start, offset));
            start = offset + grapheme.len();
            used = 0;
            last_break = None;
            continue;
        }
        let grapheme_width = grapheme.width();
        if used > 0 && used + grapheme_width > width {
            if let Some(break_at) = last_break.filter(|break_at| *break_at > start) {
                ranges.push((start, break_at));
                start = break_at;
                used = text[start..offset].width();
            } else {
                ranges.push((start, offset));
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
    ranges.push((start, text.len()));
    ranges
}

pub(crate) fn rendered_cursor_line(
    text: &str,
    ranges: &[(usize, usize)],
    cursor: usize,
) -> Option<(usize, usize)> {
    for (index, (start, end)) in ranges.iter().enumerate() {
        if cursor <= *end || index + 1 == ranges.len() {
            return Some((index, text[*start..cursor.min(*end)].width()));
        }
    }
    None
}

pub(crate) fn byte_offset_at_display_column(line: &str, target: usize) -> usize {
    let mut width = 0;
    for (offset, grapheme) in line.grapheme_indices(true) {
        if width + grapheme.width() > target {
            return offset;
        }
        width += grapheme.width();
    }
    line.len()
}

pub(crate) fn compact_text(text: &str, width: u16) -> String {
    let width = usize::from(width.max(1));
    if text.width() <= width {
        return text.replace('\n', " ");
    }
    let mut output = String::new();
    let mut used = 0;
    for character in text.chars() {
        let character_width = character.width().unwrap_or(0);
        if used + character_width + 1 > width {
            break;
        }
        output.push(if character == '\n' { ' ' } else { character });
        used += character_width;
    }
    output.push('…');
    output
}
