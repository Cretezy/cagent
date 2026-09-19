//! Welcome-card sizing and metadata presentation.

use std::path::{Path, PathBuf};

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr as _;

use super::{ACCENT_STYLE, DIM_STYLE, byte_index_after_display_width, terminal_safe};

pub(crate) const WELCOME_TIPS: [&str; 13] = [
    "Use /help to see commands and keybindings.",
    "Use Alt+P or /model to choose a model and reasoning effort.",
    "Use /providers to enable and configure providers.",
    "Use @ to attach files or directories from the workspace.",
    "Use /copy to copy the latest response.",
    "Press Shift+Enter to add a newline without sending.",
    "Use Alt+T or /tree to browse the conversation tree.",
    "Press Ctrl+G to edit your draft in $EDITOR.",
    "Use Alt+↓ or /background to inspect background work.",
    "Use Alt+N or /rename to name the current conversation.",
    "Use Alt+F or /fork to branch from an earlier message.",
    "Use /resume to reopen a conversation.",
    "Use Ctrl+W to delete a word; Ctrl+A/E move to line start/end.",
];

const WELCOME_DIRECTORY_MAX_WIDTH: usize = 70;

pub(crate) fn tip_for_session(id: cagent_agent::protocol::ConversationId) -> &'static str {
    let index = id
        .to_string()
        .bytes()
        .fold(0_usize, |sum, byte| sum.wrapping_add(usize::from(byte)))
        % WELCOME_TIPS.len();
    WELCOME_TIPS[index]
}

pub(crate) fn welcome_tip_line(tip: &str) -> Line<'static> {
    Line::from(Span::styled(format!("  Tip: {tip}"), DIM_STYLE))
}

#[allow(clippy::too_many_lines)]
pub(crate) fn welcome_lines(
    model: &str,
    effort: Option<&str>,
    has_provider: bool,
    width: u16,
) -> Vec<Line<'static>> {
    let version = format!("(v{})", env!("CARGO_PKG_VERSION"));
    let command = "/model to change";
    let command_style = ACCENT_STYLE.remove_modifier(Modifier::BOLD);
    let alternate_command = "↳ Or Alt+P";
    let label_width = "directory:".width();
    let column_gap = 1;
    let action_gap = 2;
    let selection = cagent_agent::provider::ModelRef::parse(model).ok();
    let action_width = if has_provider && selection.is_some() {
        command.width().max(alternate_command.width())
    } else {
        0
    };
    let (selected_model, selection_detail) = selection.as_ref().map_or_else(
        || {
            (
                "None, set with /model".to_owned(),
                alternate_command.to_owned(),
            )
        },
        |selection| {
            (
                terminal_safe(&selection.model),
                format!(
                    "{} · {}",
                    effort.unwrap_or("default"),
                    terminal_safe(&selection.provider)
                ),
            )
        },
    );
    let middle_desired = selected_model.width().max(selection_detail.width());
    let model_metadata_width = label_width
        + column_gap
        + middle_desired
        + usize::from(action_width > 0) * (action_gap + action_width);
    let desired_content = if has_provider {
        model_metadata_width
    } else {
        56_usize
            .max(model_metadata_width)
            .max("Use /providers to configure providers".width())
    };
    let card_width = welcome_card_width(width, desired_content.saturating_add(4));
    let inner = usize::from(card_width.saturating_sub(2));
    let content = inner.saturating_sub(2);
    let fixed_metadata_width =
        label_width + column_gap + usize::from(action_width > 0) * (action_gap + action_width);
    let middle_width = content.saturating_sub(fixed_metadata_width).max(1);
    let metadata_line =
        |label: &str, value: &str, value_style: Style, action: &str, action_style: Style| {
            let value = truncate_with_ellipsis(value, middle_width);
            let mut spans = vec![
                Span::raw(" "),
                Span::styled(format!("{label:<label_width$}"), DIM_STYLE),
                Span::raw(" ".repeat(column_gap)),
                Span::styled(
                    format!(
                        "{value}{}",
                        " ".repeat(middle_width.saturating_sub(value.width()))
                    ),
                    value_style,
                ),
            ];
            if action_width > 0 {
                spans.push(Span::raw(" ".repeat(action_gap)));
                spans.push(Span::styled(
                    format!("{action:<action_width$}"),
                    action_style,
                ));
            }
            Line::from(spans)
        };
    let title_width = 3 + "Cagent".width() + 1 + version.width();
    let mut lines = vec![
        Line::from(vec![
            Span::raw(" "),
            Span::styled(">_ ", ACCENT_STYLE),
            Span::styled("Cagent", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(format!(" {version}"), DIM_STYLE),
            Span::raw(" ".repeat(content.saturating_sub(title_width))),
        ]),
        Line::from(" ".repeat(inner)),
    ];
    if has_provider {
        if selection.is_some() {
            lines.push(metadata_line(
                "model:",
                &selected_model,
                Style::default(),
                command,
                command_style,
            ));
        } else {
            let value = "None, set with /model";
            lines.push(Line::from(vec![
                Span::raw(" "),
                Span::styled(format!("{:<label_width$}", "model:"), DIM_STYLE),
                Span::raw(" ".repeat(column_gap)),
                Span::raw("None, "),
                Span::styled("set with /model", ACCENT_STYLE),
                Span::raw(" ".repeat(middle_width.saturating_sub(value.width()))),
            ]));
        }
        lines.push(metadata_line(
            "",
            &selection_detail,
            DIM_STYLE,
            if selection.is_some() {
                alternate_command
            } else {
                ""
            },
            DIM_STYLE,
        ));
    } else {
        let used = "Use ".width() + "/providers".width() + " to configure providers".width();
        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::raw("Use "),
            Span::styled("/providers", ACCENT_STYLE),
            Span::raw(" to configure providers"),
            Span::raw(" ".repeat(content.saturating_sub(used))),
        ]));
        lines.push(Line::from(" ".repeat(inner)));
    }
    lines
}

pub(crate) fn welcome_directory_line(workspace: &Path, width: u16) -> Line<'static> {
    let directory = display_workspace(workspace);
    welcome_path_line(
        "directory:",
        &directory,
        width,
        "directory:".width(),
        Style::default(),
        WELCOME_DIRECTORY_MAX_WIDTH,
    )
}

pub(crate) fn welcome_session_line(
    session_id: cagent_agent::protocol::ConversationId,
    width: u16,
) -> Line<'static> {
    welcome_path_line(
        "session:",
        &session_id.to_string(),
        width,
        "directory:".width(),
        DIM_STYLE,
        usize::MAX,
    )
}

fn welcome_path_line(
    label: &str,
    value: &str,
    width: u16,
    label_width: usize,
    value_style: Style,
    maximum_value_width: usize,
) -> Line<'static> {
    let prefix_width = 2 + label_width;
    let available_width = usize::from(width)
        .saturating_sub(prefix_width)
        .min(maximum_value_width);
    Line::from(vec![
        Span::raw(" "),
        Span::styled(format!("{label:<label_width$}"), DIM_STYLE),
        Span::raw(" "),
        Span::styled(truncate_with_ellipsis(value, available_width), value_style),
    ])
}

pub(crate) fn welcome_card_rows(
    lines: &[Line<'static>],
    workspace: &Path,
    session_id: cagent_agent::protocol::ConversationId,
    width: u16,
) -> Vec<Line<'static>> {
    if lines.is_empty() {
        return Vec::new();
    }

    let desired_width = lines
        .iter()
        .map(Line::width)
        .max()
        .unwrap_or_default()
        .max(2 + "directory:".width() + session_id.to_string().width())
        .max(
            2 + "directory:".width()
                + display_workspace(workspace)
                    .width()
                    .min(WELCOME_DIRECTORY_MAX_WIDTH),
        )
        .saturating_add(3);
    let card_width = welcome_card_width(width, desired_width).max(2);
    let inner_width = usize::from(card_width.saturating_sub(2));
    let directory = welcome_directory_line(workspace, inner_width as u16);
    let session = welcome_session_line(session_id, inner_width as u16);
    let border_style = Style::default().fg(Color::White);
    let mut rows = Vec::with_capacity(lines.len().saturating_add(4));
    rows.push(Line::from(Span::styled(
        format!("╭{}╮", "─".repeat(inner_width)),
        border_style,
    )));
    let mut append_row = |line: Line<'static>| {
        let mut spans = Vec::with_capacity(line.spans.len().saturating_add(2));
        spans.push(Span::styled("│", border_style));
        spans.extend(line.spans.iter().cloned());
        let padding = inner_width.saturating_sub(line.width());
        if padding > 0 {
            spans.push(Span::raw(" ".repeat(padding)));
        }
        spans.push(Span::styled("│", border_style));
        rows.push(Line::from(spans));
    };
    for (index, line) in lines.iter().enumerate() {
        if index == 2 {
            append_row(directory.clone());
            append_row(session.clone());
        }
        append_row(line.clone());
    }
    rows.push(Line::from(Span::styled(
        format!("╰{}╯", "─".repeat(inner_width)),
        border_style,
    )));
    rows
}

fn welcome_card_width(width: u16, desired_width: usize) -> u16 {
    u16::try_from(desired_width).unwrap_or(u16::MAX).min(width)
}

pub(crate) fn truncate_with_ellipsis(value: &str, width: usize) -> String {
    if value.width() <= width {
        return value.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let end = byte_index_after_display_width(value, width.saturating_sub(1));
    format!("{}…", &value[..end])
}

fn display_workspace(workspace: &Path) -> String {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return terminal_safe(&workspace.to_string_lossy());
    };
    if workspace == home {
        "~".into()
    } else {
        workspace.strip_prefix(home).map_or_else(
            |_| terminal_safe(&workspace.to_string_lossy()),
            |relative| format!("~/{}", terminal_safe(&relative.to_string_lossy())),
        )
    }
}
