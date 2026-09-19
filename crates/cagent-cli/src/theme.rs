use cagent_agent::config::{UiColors, UiTheme};
use cagent_agent::presentation::StatusLineColor;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};

#[derive(Clone, Copy, Debug)]
pub(crate) struct Theme {
    kind: UiTheme,
    background: Option<Color>,
    foreground: Option<Color>,
    surface: Color,
    highlight: Color,
    bash: Color,
    muted: Option<Color>,
    diff_added_background: Color,
    diff_removed_background: Color,
}

impl Theme {
    pub(crate) fn new(kind: UiTheme, colors: UiColors) -> Self {
        let defaults = UiColors::for_theme(kind);
        Self {
            kind,
            background: colors.background.map(color),
            foreground: (kind == UiTheme::Light || colors.foreground != defaults.foreground)
                .then(|| color(colors.foreground)),
            surface: if kind == UiTheme::Dark && colors.surface == defaults.surface {
                Color::Indexed(236)
            } else {
                color(colors.surface)
            },
            highlight: color(colors.highlight),
            bash: color(colors.bash),
            muted: (kind == UiTheme::Light || colors.muted != defaults.muted)
                .then(|| color(colors.muted)),
            diff_added_background: color(colors.diff_added_background),
            diff_removed_background: color(colors.diff_removed_background),
        }
    }

    pub(crate) fn dark() -> Self {
        Self::new(UiTheme::Dark, UiColors::for_theme(UiTheme::Dark))
    }

    /// Applies semantic UI tokens after widgets render. This keeps the palette
    /// centralized and also covers third-party widgets and cached transcript rows.
    pub(crate) fn apply(self, buffer: &mut Buffer) {
        for cell in &mut buffer.content {
            if cell.bg == Color::Reset
                && let Some(background) = self.background
            {
                cell.set_bg(background);
            }
            if cell.modifier.contains(Modifier::DIM)
                && matches!(
                    cell.fg,
                    Color::Reset | Color::White | Color::Gray | Color::DarkGray
                )
            {
                if let Some(muted) = self.muted {
                    cell.set_fg(muted);
                    cell.modifier.remove(Modifier::DIM);
                }
            } else if cell.fg == Color::Reset
                && let Some(foreground) = self.foreground
            {
                cell.set_fg(foreground);
            } else if cell.fg == Color::Cyan {
                cell.set_fg(self.highlight);
            } else if cell.fg == Color::Indexed(215) {
                cell.set_fg(self.bash);
            }
            if matches!(cell.bg, Color::Indexed(235 | 236)) {
                cell.set_bg(self.surface);
            } else if cell.bg == Color::Rgb(33, 58, 43) {
                cell.set_bg(self.diff_added_background);
            } else if cell.bg == Color::Rgb(74, 34, 29) {
                cell.set_bg(self.diff_removed_background);
            }
            if self.kind == UiTheme::Light {
                self.remap_dark_palette(cell);
            }
        }
    }

    fn remap_dark_palette(self, cell: &mut ratatui::buffer::Cell) {
        cell.fg = match cell.fg {
            // The working indicator shimmers through this grayscale range. On a
            // light background its direction must be inverted, from muted to
            // foreground, instead of fading toward white.
            Color::Indexed(shade @ 244..=254) => blend(
                self.muted.unwrap_or(Color::Rgb(107, 114, 128)),
                self.foreground.unwrap_or(Color::Rgb(31, 41, 55)),
                shade - 244,
                10,
            ),
            Color::White => self.foreground.unwrap_or(Color::Rgb(31, 41, 55)),
            Color::Indexed(103) => self.muted.unwrap_or(Color::Rgb(107, 114, 128)),
            Color::Indexed(114) => Color::Rgb(21, 128, 61),
            Color::Indexed(215) => Color::Rgb(180, 83, 9),
            Color::Indexed(141) => Color::Rgb(126, 34, 206),
            Color::Indexed(117 | 111 | 81) => Color::Rgb(29, 78, 216),
            Color::Indexed(204 | 216) => Color::Rgb(190, 24, 93),
            Color::Rgb(154, 205, 114) => Color::Rgb(21, 128, 61),
            Color::Rgb(255, 107, 107) => Color::Rgb(185, 28, 28),
            other => other,
        };
    }
}

fn blend(from: Color, to: Color, amount: u8, total: u8) -> Color {
    let (Color::Rgb(from_red, from_green, from_blue), Color::Rgb(to_red, to_green, to_blue)) =
        (from, to)
    else {
        return if amount.saturating_mul(2) < total {
            from
        } else {
            to
        };
    };
    let component = |from: u8, to: u8| {
        let from = i16::from(from);
        let distance = i16::from(to) - from;
        u8::try_from(from + distance * i16::from(amount) / i16::from(total)).unwrap_or(to)
    };
    Color::Rgb(
        component(from_red, to_red),
        component(from_green, to_green),
        component(from_blue, to_blue),
    )
}

pub(crate) const fn color(value: StatusLineColor) -> Color {
    match value {
        StatusLineColor::Black => Color::Black,
        StatusLineColor::Red => Color::Red,
        StatusLineColor::Green => Color::Green,
        StatusLineColor::Yellow => Color::Yellow,
        StatusLineColor::Blue => Color::Blue,
        StatusLineColor::Magenta => Color::Magenta,
        StatusLineColor::Cyan => Color::Cyan,
        StatusLineColor::Gray => Color::Gray,
        StatusLineColor::DarkGray => Color::DarkGray,
        StatusLineColor::LightRed => Color::LightRed,
        StatusLineColor::LightGreen => Color::LightGreen,
        StatusLineColor::LightYellow => Color::LightYellow,
        StatusLineColor::LightBlue => Color::LightBlue,
        StatusLineColor::LightMagenta => Color::LightMagenta,
        StatusLineColor::LightCyan => Color::LightCyan,
        StatusLineColor::White => Color::White,
        StatusLineColor::Rgb(red, green, blue) => Color::Rgb(red, green, blue),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    #[test]
    fn light_theme_and_overrides_apply_semantic_tokens() {
        let mut colors = UiColors::for_theme(UiTheme::Light);
        colors.highlight = StatusLineColor::Rgb(1, 2, 3);
        colors.bash = StatusLineColor::Rgb(4, 5, 6);
        colors.background = Some(StatusLineColor::Rgb(250, 250, 250));
        let theme = Theme::new(UiTheme::Light, colors);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 7, 1));
        buffer[(0, 0)].set_fg(Color::Cyan);
        buffer[(1, 0)].set_bg(Color::Indexed(236));
        buffer[(2, 0)].modifier.insert(Modifier::DIM);
        buffer[(3, 0)].set_fg(Color::Indexed(250));
        buffer[(4, 0)].set_bg(Color::Rgb(33, 58, 43));
        buffer[(5, 0)].set_bg(Color::Rgb(74, 34, 29));
        buffer[(6, 0)].set_fg(Color::Indexed(215));

        theme.apply(&mut buffer);

        assert_eq!(buffer[(0, 0)].fg, Color::Rgb(1, 2, 3));
        assert_eq!(buffer[(0, 0)].bg, Color::Rgb(250, 250, 250));
        assert_eq!(buffer[(1, 0)].bg, color(colors.surface));
        assert_eq!(buffer[(2, 0)].fg, color(colors.muted));
        assert!(!buffer[(2, 0)].modifier.contains(Modifier::DIM));
        assert_eq!(buffer[(3, 0)].fg, Color::Rgb(62, 71, 85));
        assert_eq!(buffer[(4, 0)].bg, color(colors.diff_added_background));
        assert_eq!(buffer[(5, 0)].bg, color(colors.diff_removed_background));
        assert_eq!(buffer[(6, 0)].fg, color(colors.bash));
    }
}
