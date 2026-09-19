use super::{DIM_STYLE, Line, Modifier, Span, Style};
use cagent_agent::presentation::StatusLineModule;
use ratatui::style::Color;
use unicode_width::UnicodeWidthStr;

pub(crate) fn status_line_content(
    config: &cagent_agent::presentation::StatusLineConfig,
    values: &cagent_agent::presentation::StatusLineValues,
    width: usize,
) -> Line<'static> {
    let segments = cagent_agent::presentation::prepare_status_line(config, values, width);
    let mut spans = Vec::new();
    for (index, segment) in segments.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" · ", DIM_STYLE));
        }
        let color = status_line_color(segment.color);
        if let Some(label) = segment.label {
            spans.push(Span::styled(
                label,
                Style::new().fg(color).add_modifier(Modifier::DIM),
            ));
        }
        let style = Style::new().fg(color);
        let value_style = if segment.module == StatusLineModule::Hint {
            style
        } else {
            style.add_modifier(Modifier::BOLD)
        };
        if segment.value_spans.is_empty() {
            spans.push(Span::styled(segment.value, value_style));
        } else {
            spans.extend(segment.value_spans.into_iter().map(|part| {
                Span::styled(
                    part.text,
                    if part.bold {
                        style.add_modifier(Modifier::BOLD)
                    } else {
                        style
                    },
                )
            }));
        }
        if let Some(effort) = segment.effort {
            spans.push(Span::styled(format!(" {effort}"), style));
        }
    }
    Line::from(spans)
}

pub(crate) fn status_line_module_at(
    config: &cagent_agent::presentation::StatusLineConfig,
    values: &cagent_agent::presentation::StatusLineValues,
    width: u16,
    column: u16,
) -> Option<StatusLineModule> {
    let segments = cagent_agent::presentation::prepare_status_line(
        config,
        values,
        usize::from(width.saturating_sub(4).max(1)),
    );
    let mut offset = 2usize;
    let column = usize::from(column);
    for (index, segment) in segments.iter().enumerate() {
        if index > 0 {
            if column < offset + 3 {
                return None;
            }
            offset += 3;
        }
        if column < offset {
            return None;
        }
        let segment_width = segment.text().width();
        if column < offset + segment_width {
            return Some(segment.module);
        }
        offset += segment_width;
    }
    None
}

pub(crate) fn status_line_background_hint_at(
    config: &cagent_agent::presentation::StatusLineConfig,
    values: &cagent_agent::presentation::StatusLineValues,
    width: u16,
    column: u16,
) -> bool {
    let segments = cagent_agent::presentation::prepare_status_line(
        config,
        values,
        usize::from(width.saturating_sub(4).max(1)),
    );
    let hint = (!values.composer_has_text
        && values.primary_hint.is_none()
        && (values.background != 0 || values.subagents != 0))
        .then(|| {
            format!(
                "Alt+↓ {} background",
                values.background.saturating_add(values.subagents)
            )
        });
    let column = usize::from(column);
    let mut offset = 2usize;
    for (index, segment) in segments.iter().enumerate() {
        if index > 0 {
            offset += 3;
        }
        if segment.module == StatusLineModule::Hint
            && let Some(hint) = &hint
            && let Some(start) = segment.text().find(hint)
        {
            let start = offset + segment.text()[..start].width();
            return (start..start + hint.width()).contains(&column);
        }
        offset += segment.text().width();
    }
    false
}

pub(crate) const fn status_line_color(color: cagent_agent::presentation::StatusLineColor) -> Color {
    use cagent_agent::presentation::StatusLineColor;
    match color {
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
        StatusLineColor::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}
