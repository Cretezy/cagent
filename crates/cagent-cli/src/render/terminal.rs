//! VT100-backed rendering helpers for retained PTY output.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::widgets::Widget;
use std::borrow::Cow;
use tui_term::widget::{Cursor, PseudoTerminal};

const MAX_SCROLLBACK_ROWS: usize = 262_144;

/// Cached terminal emulation for an expanded Bash view.
///
/// VT100 state depends on every earlier byte, so rebuilding it during every
/// draw makes scrolling proportional to the full retained buffer. Keep the
/// parser for the lifetime of one output/viewport version instead.
pub(crate) struct TerminalRenderCache {
    output_address: usize,
    output_len: usize,
    completion_address: Option<usize>,
    completion_len: usize,
    width: u16,
    height: u16,
    maximum_scrollback: usize,
    parser: tui_term::vt100::Parser,
}

impl TerminalRenderCache {
    pub(crate) fn new(output: &str, completion: Option<&str>, width: u16, height: u16) -> Self {
        let mut parser = parser_with_completion(output, completion, width, height, usize::MAX);
        let maximum_scrollback = parser.screen().scrollback();
        parser.screen_mut().set_scrollback(maximum_scrollback);
        Self {
            output_address: output.as_ptr() as usize,
            output_len: output.len(),
            completion_address: completion.map(|value| value.as_ptr() as usize),
            completion_len: completion.map_or(0, str::len),
            width,
            height,
            maximum_scrollback,
            parser,
        }
    }

    pub(crate) fn matches_by_identity(
        &self,
        output_address: usize,
        output_len: usize,
        completion_address: Option<usize>,
        completion_len: usize,
        width: u16,
        height: u16,
    ) -> bool {
        self.output_address == output_address
            && self.output_len == output_len
            && self.completion_address == completion_address
            && self.completion_len == completion_len
            && self.width == width
            && self.height == height
    }

    pub(crate) const fn maximum_scrollback(&self) -> usize {
        self.maximum_scrollback
    }

    pub(crate) fn set_top_scroll_offset(&mut self, offset: usize) {
        self.parser
            .screen_mut()
            .set_scrollback(bottom_relative_scrollback(offset, self.maximum_scrollback));
    }

    pub(crate) fn screen(&self) -> &tui_term::vt100::Screen {
        self.parser.screen()
    }
}

pub(crate) fn parser(
    output: &str,
    width: u16,
    height: u16,
    scrollback: usize,
) -> tui_term::vt100::Parser {
    let mut parser = tui_term::vt100::Parser::new(height.max(1), width.max(1), MAX_SCROLLBACK_ROWS);
    parser.process(output.as_bytes());
    parser.screen_mut().set_scrollback(scrollback);
    parser
}

pub(crate) fn parser_with_completion(
    output: &str,
    completion: Option<&str>,
    width: u16,
    height: u16,
    scrollback: usize,
) -> tui_term::vt100::Parser {
    let output = output_with_completion(output, completion);
    parser(&output, width, height, scrollback)
}

pub(crate) fn scrollback_max_with_completion(
    output: &str,
    completion: Option<&str>,
    width: u16,
    height: u16,
) -> usize {
    let parser = parser_with_completion(output, completion, width, height, usize::MAX);
    parser.screen().scrollback()
}

/// Converts the application-wide top-origin offset into vt100's
/// bottom-relative scrollback coordinate.
pub(crate) const fn bottom_relative_scrollback(top_offset: usize, maximum: usize) -> usize {
    maximum.saturating_sub(if top_offset < maximum {
        top_offset
    } else {
        maximum
    })
}

fn output_with_completion<'a>(output: &'a str, completion: Option<&str>) -> Cow<'a, str> {
    match completion {
        Some(completion) => {
            let separator = if output.ends_with('\n') { "" } else { "\r\n" };
            Cow::Owned(format!(
                "{output}{separator}\x1b[38;5;244m{completion}\x1b[0m\r\n"
            ))
        }
        None if output.is_empty() || output.ends_with('\n') => Cow::Borrowed(output),
        None => Cow::Owned(format!("{output}\r\n")),
    }
}

/// Renders a terminal screen without letting default VT cells erase the
/// expanded surface background. Explicit ANSI background colors still win.
pub(crate) struct TerminalView<'a> {
    screen: &'a tui_term::vt100::Screen,
    default_background: Color,
}

impl<'a> TerminalView<'a> {
    pub(crate) const fn new(
        screen: &'a tui_term::vt100::Screen,
        default_background: Color,
    ) -> Self {
        Self {
            screen,
            default_background,
        }
    }
}

impl Widget for TerminalView<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        PseudoTerminal::new(self.screen)
            .cursor(Cursor::default().visibility(false))
            .render(area, buffer);
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                let cell = &mut buffer[(x, y)];
                if cell.bg == Color::Reset {
                    cell.set_bg(self.default_background);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn terminal_emulation_applies_carriage_return_and_clear_line() {
        let parser = parser("progress 10%\r\x1b[2Kdone", 40, 4, 0);
        let contents = parser.screen().contents();
        assert!(contents.contains("done"));
        assert!(!contents.contains("progress"));
    }

    #[test]
    fn terminal_view_inherits_surface_background_without_overriding_ansi_backgrounds() {
        let parser = parser("plain \x1b[41mred", 20, 2, 0);
        let mut terminal = Terminal::new(TestBackend::new(20, 2)).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(
                    TerminalView::new(parser.screen(), Color::Indexed(236)),
                    frame.area(),
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 0)].bg, Color::Indexed(236));
        assert_ne!(buffer[(6, 0)].bg, Color::Indexed(236));
    }

    #[test]
    fn completed_terminal_adds_a_distinct_gray_status_and_one_blank_row() {
        let parser = parser_with_completion("done", Some("[exited with status 0]"), 40, 4, 0);
        let mut terminal = Terminal::new(TestBackend::new(40, 4)).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(
                    TerminalView::new(parser.screen(), Color::Indexed(236)),
                    frame.area(),
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 1)].fg, Color::Indexed(244));
        assert_eq!(buffer[(0, 2)].symbol(), " ");
    }

    #[test]
    fn terminal_padding_does_not_duplicate_an_existing_newline() {
        assert_eq!(output_with_completion("done\r\n", None), "done\r\n");
        assert_eq!(output_with_completion("done", None), "done\r\n");
    }

    #[test]
    fn top_origin_offsets_translate_to_vt100_scrollback() {
        assert_eq!(bottom_relative_scrollback(0, 20), 20);
        assert_eq!(bottom_relative_scrollback(7, 20), 13);
        assert_eq!(bottom_relative_scrollback(20, 20), 0);
        assert_eq!(bottom_relative_scrollback(usize::MAX, 20), 0);
    }

    #[test]
    fn terminal_render_cache_reuses_matching_terminal_state() {
        let output = "first\r\nsecond\r\nthird\r\nfourth\r\n".to_owned();
        let mut cache = TerminalRenderCache::new(&output, None, 20, 2);

        assert!(cache.matches_by_identity(output.as_ptr() as usize, output.len(), None, 0, 20, 2,));
        assert!(
            !cache.matches_by_identity(output.as_ptr() as usize, output.len(), None, 0, 21, 2,)
        );
        assert!(cache.maximum_scrollback() > 0);

        cache.set_top_scroll_offset(0);
        assert!(cache.screen().contents().contains("first"));
        cache.set_top_scroll_offset(cache.maximum_scrollback());
        assert_eq!(cache.screen().scrollback(), 0);
    }
}
