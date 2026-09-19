//! Rendering for tool activity, delegated agent runs, and working state.

use std::path::Path;
use std::time::{Duration, Instant};

use cagent_agent::presentation::{
    ExplorationActivity, ExplorationActivityKind, ToolActivityGroup, ToolActivityStatus,
    format_elapsed,
};
use cagent_agent::protocol::{PlanStepStatus, UpdatePlanArgs};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr as _;

use crate::app::{NOTICE_STYLE, ReconnectStatus};

use super::{
    CHIP_STYLE, DIM_STYLE, ERROR_STYLE, WORKING_SHIMMER_PERIOD, question_transcript_lines,
    render_edit_history, terminal_safe, truncate_with_ellipsis, wrap_ranges,
};

const INACTIVITY_WARNING_THRESHOLD: Duration = Duration::from_secs(90);

pub(crate) fn active_plan_lines(plan: &UpdatePlanArgs, width: u16) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::styled("• ", DIM_STYLE),
        Span::styled("Plan", Style::default().add_modifier(Modifier::BOLD)),
    ])];
    let mut first = true;
    let mut push_text = |marker: &str, text: &str, style: Style| {
        let prefix = if first { "  └ " } else { "    " };
        first = false;
        let first_prefix = format!("{prefix}{marker}");
        let continuation = " ".repeat(first_prefix.width());
        let available =
            width.saturating_sub(u16::try_from(first_prefix.width()).unwrap_or(u16::MAX));
        for (index, (start, end)) in super::wrap_ranges(text, available.max(1))
            .into_iter()
            .enumerate()
        {
            let prefix = if index == 0 {
                first_prefix.clone()
            } else {
                continuation.clone()
            };
            lines.push(Line::from(vec![
                Span::styled(prefix, DIM_STYLE),
                Span::styled(text[start..end].trim_end().to_owned(), style),
            ]));
        }
    };
    if let Some(explanation) = plan
        .explanation
        .as_deref()
        .map(str::trim)
        .filter(|explanation| !explanation.is_empty())
    {
        push_text("", explanation, DIM_STYLE.add_modifier(Modifier::ITALIC));
    }
    if plan.plan.is_empty() {
        push_text(
            "",
            "(no steps provided)",
            DIM_STYLE.add_modifier(Modifier::ITALIC),
        );
    } else {
        for item in &plan.plan {
            let (marker, style) = match item.status {
                PlanStepStatus::Pending => ("□ ", DIM_STYLE),
                PlanStepStatus::InProgress => (
                    "□ ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                PlanStepStatus::Completed => ("✔ ", DIM_STYLE.add_modifier(Modifier::CROSSED_OUT)),
            };
            push_text(marker, &item.step, style);
        }
    }
    lines
}

#[cfg(test)]
pub(crate) fn render_tool_groups(groups: &[ToolActivityGroup], width: u16) -> Vec<Line<'static>> {
    render_tool_groups_with_workspace(groups, Path::new(""), width)
}

pub(crate) fn render_tool_groups_with_workspace(
    groups: &[ToolActivityGroup],
    workspace: &Path,
    width: u16,
) -> Vec<Line<'static>> {
    groups
        .iter()
        .flat_map(|group| render_tool_group(group, workspace, width))
        .collect()
}

pub(crate) struct RenderedToolGroupLine {
    pub(crate) line: Line<'static>,
    pub(crate) exploration_toggle: bool,
}

pub(crate) fn render_tool_group_with_workspace(
    group: &ToolActivityGroup,
    workspace: &Path,
    width: u16,
    exploration_expanded: bool,
) -> Vec<RenderedToolGroupLine> {
    match group {
        ToolActivityGroup::Exploration { active, activities } => {
            render_exploration_group_with_state(*active, activities, width, exploration_expanded)
        }
        _ => render_tool_group(group, workspace, width)
            .into_iter()
            .map(|line| RenderedToolGroupLine {
                line,
                exploration_toggle: false,
            })
            .collect(),
    }
}

fn render_tool_group(
    group: &ToolActivityGroup,
    workspace: &Path,
    width: u16,
) -> Vec<Line<'static>> {
    match group {
        ToolActivityGroup::Exploration { active, activities } => {
            render_exploration_group_with_state(*active, activities, width, false)
                .into_iter()
                .map(|rendered| rendered.line)
                .collect()
        }
        ToolActivityGroup::Tool {
            label,
            target,
            status,
        } => {
            let bullet_style = if *status == ToolActivityStatus::Failed {
                ERROR_STYLE
            } else {
                DIM_STYLE
            };
            let mut spans = vec![
                Span::styled("• ", bullet_style),
                Span::styled(label.clone(), Style::default().add_modifier(Modifier::BOLD)),
            ];
            if let Some(target) = target {
                spans.push(Span::raw(format!(" {target}")));
            }
            vec![Line::from(spans)]
        }
        ToolActivityGroup::Question { transcript } => {
            if transcript.cancelled {
                vec![Line::from(vec![
                    Span::styled("• ", DIM_STYLE),
                    Span::raw("Question not answered"),
                ])]
            } else {
                question_transcript_lines(transcript)
            }
        }
        ToolActivityGroup::Edit { diff, .. } => render_edit_history(diff, workspace, width),
        ToolActivityGroup::Mcp { call } => render_mcp_activity(call, width),
        ToolActivityGroup::WebSearch {
            provider,
            query,
            status,
            result_count,
            ..
        } => render_web_search_activity(provider, query, *status, *result_count, width),
        ToolActivityGroup::WebFetch {
            url,
            status,
            output,
            ..
        } => render_web_fetch_activity(url, *status, output.as_deref(), width),
        ToolActivityGroup::Bash {
            command,
            status,
            output,
            exit_code,
            ..
        } => render_bash_activity(command, *status, output.as_deref(), *exit_code, width),
        ToolActivityGroup::TerminalWrite {
            terminal_id: _,
            command,
            data,
            status,
        } => render_terminal_write_activity(command, data, *status, width),
        ToolActivityGroup::TerminalKill { command, status } => {
            render_terminal_kill_activity(command, *status)
        }
        ToolActivityGroup::Delegate {
            action,
            profile,
            task,
            status,
        } => {
            let bullet = if *status == ToolActivityStatus::Pending {
                pending_bullet_style()
            } else if *status == ToolActivityStatus::Failed {
                ERROR_STYLE
            } else {
                DIM_STYLE
            };
            let mut lines = vec![Line::from(vec![
                Span::styled("• ", bullet),
                Span::styled(
                    format!("{action} "),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(profile.clone(), Style::default().fg(Color::Cyan)),
                Span::styled(" sub-agent", Style::default().add_modifier(Modifier::BOLD)),
            ])];
            if let Some(task) = task {
                lines.push(Line::from(vec![
                    Span::styled("  └ ", DIM_STYLE),
                    Span::styled(terminal_safe(task), DIM_STYLE),
                ]));
            }
            lines
        }
    }
}

fn render_terminal_write_activity(
    command: &str,
    data: &str,
    status: ToolActivityStatus,
    width: u16,
) -> Vec<Line<'static>> {
    let bullet_style = if status == ToolActivityStatus::Failed {
        ERROR_STYLE
    } else {
        DIM_STYLE
    };
    let mut heading = vec![
        Span::styled("• ", bullet_style),
        Span::styled(
            "Terminal Write: ",
            Style::default().add_modifier(Modifier::BOLD),
        ),
    ];
    heading.extend(bash_command_spans(&command.replace('\n', " ↵ ")));
    let mut lines = vec![Line::from(heading)];
    let preview = bash_output_preview(data, usize::from(width.saturating_sub(5)));
    let last = preview.len().saturating_sub(1);
    for (index, line) in preview.into_iter().enumerate() {
        lines.push(Line::from(vec![
            Span::styled(if index == last { "  └  " } else { "  │  " }, DIM_STYLE),
            Span::styled(line, DIM_STYLE),
        ]));
    }
    lines
}

fn render_terminal_kill_activity(command: &str, status: ToolActivityStatus) -> Vec<Line<'static>> {
    let bullet_style = if status == ToolActivityStatus::Failed {
        ERROR_STYLE
    } else {
        DIM_STYLE
    };
    let mut heading = vec![
        Span::styled("• ", bullet_style),
        Span::styled(
            "Terminal Kill: ",
            Style::default().add_modifier(Modifier::BOLD),
        ),
    ];
    heading.extend(bash_command_spans(&command.replace('\n', " ↵ ")));
    vec![Line::from(heading)]
}

fn render_web_search_activity(
    provider: &str,
    query: &str,
    status: ToolActivityStatus,
    result_count: Option<usize>,
    width: u16,
) -> Vec<Line<'static>> {
    let bullet_style = match status {
        ToolActivityStatus::Pending => pending_bullet_style(),
        ToolActivityStatus::Succeeded => Style::default().fg(Color::Green),
        ToolActivityStatus::Failed => ERROR_STYLE,
    };
    let status_label = match status {
        ToolActivityStatus::Pending => "running".into(),
        ToolActivityStatus::Succeeded => {
            result_count.map_or_else(|| "done".into(), |count| format!("{count} results"))
        }
        ToolActivityStatus::Failed => "failed".into(),
    };
    let heading = Line::from(vec![
        Span::styled("• ", bullet_style),
        Span::styled("Web search ", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(terminal_safe(provider), Style::default().fg(Color::Cyan)),
        Span::styled(format!(" · {status_label}"), DIM_STYLE),
    ]);
    let safe_query = terminal_safe(query);
    let mut lines = vec![heading];
    for (index, (start, end)) in wrap_ranges(&safe_query, width.saturating_sub(6))
        .into_iter()
        .enumerate()
    {
        lines.push(Line::from(vec![
            Span::styled(if index == 0 { "  └ " } else { "    " }, DIM_STYLE),
            Span::styled(safe_query[start..end].to_owned(), CHIP_STYLE),
        ]));
    }
    lines
}

pub(crate) fn web_fetch_output_preview(output: &str, width: usize) -> Vec<String> {
    bash_output_preview(output, width)
}

fn render_web_fetch_activity(
    url: &str,
    status: ToolActivityStatus,
    output: Option<&str>,
    width: u16,
) -> Vec<Line<'static>> {
    let bullet = match status {
        ToolActivityStatus::Pending => pending_bullet_style(),
        ToolActivityStatus::Succeeded => Style::default().fg(Color::Green),
        ToolActivityStatus::Failed => ERROR_STYLE,
    };
    let mut lines = vec![Line::from(vec![
        Span::styled("• ", bullet),
        Span::styled("Web Fetch ", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(terminal_safe(url), CHIP_STYLE),
    ])];
    if status == ToolActivityStatus::Succeeded
        && let Some(output) = output.filter(|output| !output.is_empty())
    {
        let preview = web_fetch_output_preview(output, usize::from(width.saturating_sub(7)));
        let last = preview.len().saturating_sub(1);
        for (index, text) in preview.into_iter().enumerate() {
            for (wrap_index, (start, end)) in wrap_ranges(&text, width.saturating_sub(6))
                .into_iter()
                .enumerate()
            {
                lines.push(Line::from(vec![
                    Span::styled(
                        if index == last && wrap_index == 0 {
                            "    └ "
                        } else {
                            "    │ "
                        },
                        DIM_STYLE,
                    ),
                    Span::styled(text[start..end].to_owned(), DIM_STYLE),
                ]));
            }
        }
    }
    lines
}

fn render_mcp_activity(
    call: &cagent_agent::presentation::McpCall,
    width: u16,
) -> Vec<Line<'static>> {
    let bullet_style = match call.status {
        ToolActivityStatus::Pending => pending_bullet_style(),
        ToolActivityStatus::Succeeded => Style::default().fg(Color::Green),
        ToolActivityStatus::Failed => ERROR_STYLE,
    };
    let duration_millis = call.duration_millis;
    let status_label = match call.status {
        ToolActivityStatus::Pending => "running".to_owned(),
        ToolActivityStatus::Succeeded => {
            duration_millis.map_or_else(|| "done".into(), |duration| format!("{duration} ms"))
        }
        ToolActivityStatus::Failed => duration_millis.map_or_else(
            || "failed".into(),
            |duration| format!("failed · {duration} ms"),
        ),
    };
    let line = Line::from(vec![
        Span::styled("• ", bullet_style),
        Span::styled("MCP ", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(
            terminal_safe(&call.server),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled(" · ", DIM_STYLE),
        Span::styled(terminal_safe(&call.tool), CHIP_STYLE),
        Span::styled(format!(" · {status_label}"), DIM_STYLE),
        Span::styled(
            format!(" · {}", terminal_safe(&call.parameters.to_string())),
            DIM_STYLE,
        ),
    ]);
    vec![crate::app::list::truncate_line(line, usize::from(width))]
}

fn render_exploration_group_with_state(
    active: bool,
    activities: &[ExplorationActivity],
    width: u16,
    expanded: bool,
) -> Vec<RenderedToolGroupLine> {
    let mut lines = vec![RenderedToolGroupLine {
        line: Line::from(vec![
            Span::styled(
                "• ",
                if active {
                    pending_bullet_style()
                } else {
                    DIM_STYLE
                },
            ),
            Span::styled(
                if active { "Exploring" } else { "Explored" },
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]),
        exploration_toggle: false,
    }];
    let collapsible = activities.len() > 4;
    for (index, activity) in activities.iter().enumerate() {
        if collapsible && !expanded && index == 2 {
            let hidden = activities.len() - 4;
            lines.push(RenderedToolGroupLine {
                line: Line::from(vec![
                    Span::raw("  "),
                    Span::styled("│", DIM_STYLE),
                    Span::raw(" "),
                    Span::styled(
                        truncate_with_ellipsis(
                            &format!("… +{hidden} items (click to expand)"),
                            usize::from(width.saturating_sub(4)),
                        ),
                        DIM_STYLE,
                    ),
                ]),
                exploration_toggle: true,
            });
            continue;
        } else if collapsible && !expanded && (2..activities.len() - 2).contains(&index) {
            continue;
        }
        lines.extend(
            render_exploration_activity(
                activity,
                index + 1 == activities.len() && !(collapsible && expanded),
                width,
            )
            .into_iter()
            .map(|line| RenderedToolGroupLine {
                line,
                exploration_toggle: false,
            }),
        );
    }
    if collapsible && expanded {
        lines.push(RenderedToolGroupLine {
            line: Line::from(vec![
                Span::raw("  "),
                Span::styled("└", DIM_STYLE),
                Span::raw(" "),
                Span::styled(
                    truncate_with_ellipsis(
                        "… click to collapse",
                        usize::from(width.saturating_sub(4)),
                    ),
                    DIM_STYLE,
                ),
            ]),
            exploration_toggle: true,
        });
    }
    lines
}

fn render_exploration_activity(
    activity: &ExplorationActivity,
    last: bool,
    width: u16,
) -> Vec<Line<'static>> {
    let connector = if last { "└" } else { "│" };
    let continuation = if last { " " } else { "│" };
    let (targets, dim_ranges) = exploration_target_text(activity);
    let (label, _, _) = cagent_agent::presentation::exploration_activity_parts(activity);
    let target_width = width.saturating_sub(u16::try_from(5 + label.width()).unwrap_or(u16::MAX));
    wrap_ranges(&targets, target_width)
        .into_iter()
        .enumerate()
        .map(|(line_index, (start, end))| {
            let mut spans = vec![
                Span::raw("  "),
                Span::styled(
                    if line_index == 0 {
                        connector
                    } else {
                        continuation
                    },
                    DIM_STYLE,
                ),
                Span::raw(" "),
            ];
            if line_index == 0 {
                spans.push(Span::styled(label.to_owned(), CHIP_STYLE));
                spans.push(Span::raw(" "));
            } else {
                spans.push(Span::raw(" ".repeat(label.width() + 1)));
            }
            spans.extend(styled_target_slice(&targets, &dim_ranges, start, end));
            Line::from(spans)
        })
        .collect()
}

fn render_bash_activity(
    command: &str,
    status: ToolActivityStatus,
    output: Option<&str>,
    exit_code: Option<i32>,
    width: u16,
) -> Vec<Line<'static>> {
    let running = status == ToolActivityStatus::Pending;
    let bullet_style = if running {
        pending_bullet_style()
    } else if status == ToolActivityStatus::Failed || exit_code.is_some_and(|code| code != 0) {
        ERROR_STYLE
    } else {
        Style::default().fg(Color::Green)
    };
    let mut heading = vec![
        Span::styled("• ", bullet_style),
        Span::styled(
            if running { "Running " } else { "Ran " },
            Style::default().add_modifier(Modifier::BOLD),
        ),
    ];
    heading.extend(bash_command_spans(&command.replace('\n', " ↵ ")));
    let mut lines = vec![Line::from(heading)];
    if let Some(output) = output.filter(|output| !output.is_empty()) {
        let preview = bash_output_preview(output, usize::from(width.saturating_sub(5)));
        let last = preview.len().saturating_sub(1);
        for (index, line) in preview.into_iter().enumerate() {
            lines.push(Line::from(vec![
                Span::styled(if index == last { "  └  " } else { "  │  " }, DIM_STYLE),
                Span::styled(line, DIM_STYLE),
            ]));
        }
    }
    lines
}

pub(super) fn pending_bullet_style() -> Style {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let period = 1_400_u128;
    let half = period / 2;
    let phase = millis % period;
    let distance = phase.abs_diff(half);
    let brightness = 110_u128 + (half - distance) * 145 / half;
    let shade = u8::try_from(brightness).unwrap_or(255);
    Style::default().fg(Color::Rgb(shade, shade, shade))
}

pub(crate) fn bash_command_spans(command: &str) -> Vec<Span<'static>> {
    cagent_agent::presentation::highlight_code("bash", command)
        .into_iter()
        .map(|token| {
            Span::styled(
                terminal_safe(&token.text),
                crate::markdown::code_style(token.kind),
            )
        })
        .collect()
}

pub(crate) fn bash_output_preview(output: &str, width: usize) -> Vec<String> {
    // Tool output can be hundreds of kilobytes, while the collapsed card only
    // displays its first and last two lines. Do not sanitize and allocate every
    // hidden line just to discard it afterward.
    let line_count = output.lines().count();
    let preview = if line_count <= 4 {
        output.lines().map(terminal_safe).collect::<Vec<_>>()
    } else {
        let mut head = output.lines();
        let first = head.next().map(terminal_safe).unwrap_or_default();
        let second = head.next().map(terminal_safe).unwrap_or_default();
        let mut tail = output.lines().rev();
        let last = tail.next().map(terminal_safe).unwrap_or_default();
        let penultimate = tail.next().map(terminal_safe).unwrap_or_default();
        vec![
            first,
            second,
            format!("… +{} lines (click to view full output)", line_count - 4),
            penultimate,
            last,
        ]
    };
    preview
        .into_iter()
        .map(|line| truncate_with_ellipsis(&line, width))
        .collect()
}

/// Number of terminal rows a shared collapsed-output preview occupies.
pub(crate) fn output_preview_row_count(output: &str, width: u16) -> usize {
    bash_output_preview(output, usize::from(width))
        .iter()
        .map(|line| wrap_ranges(line, width).len().max(1))
        .sum()
}

fn exploration_target_text(activity: &ExplorationActivity) -> (String, Vec<(usize, usize)>) {
    let mut text = String::new();
    let mut dim_ranges = Vec::new();
    let (_, targets, relations) = cagent_agent::presentation::exploration_activity_parts(activity);
    append_exploration_targets(&mut text, &mut dim_ranges, targets);
    if relations.is_empty()
        && matches!(
            activity.kind,
            ExplorationActivityKind::List | ExplorationActivityKind::Search
        )
        && !activity.scopes.is_empty()
    {
        append_dimmed(&mut text, &mut dim_ranges, " in ");
        append_exploration_targets(&mut text, &mut dim_ranges, &activity.scopes);
    }
    for (relation, values) in relations {
        append_dimmed(&mut text, &mut dim_ranges, &format!(" {relation} "));
        append_exploration_targets(&mut text, &mut dim_ranges, values);
    }
    (text, dim_ranges)
}

fn append_exploration_targets(
    text: &mut String,
    dim_ranges: &mut Vec<(usize, usize)>,
    targets: &[String],
) {
    for (index, target) in targets.iter().enumerate() {
        if index > 0 {
            append_dimmed(text, dim_ranges, ", ");
        }
        text.push_str(&terminal_safe(target));
    }
}

fn append_dimmed(text: &mut String, dim_ranges: &mut Vec<(usize, usize)>, value: &str) {
    let start = text.len();
    text.push_str(value);
    dim_ranges.push((start, text.len()));
}

fn styled_target_slice(
    text: &str,
    dim_ranges: &[(usize, usize)],
    start: usize,
    end: usize,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut cursor = start;
    for &(dim_start, dim_end) in dim_ranges {
        let dim_start = dim_start.max(start);
        let dim_end = dim_end.min(end);
        if dim_start >= dim_end {
            continue;
        }
        if cursor < dim_start {
            spans.push(Span::raw(text[cursor..dim_start].to_owned()));
        }
        spans.push(Span::styled(text[dim_start..dim_end].to_owned(), DIM_STYLE));
        cursor = dim_end;
    }
    if cursor < end {
        spans.push(Span::raw(text[cursor..end].to_owned()));
    }
    spans
}

pub(crate) fn working_line(
    started: Option<Instant>,
    last_activity: Option<Instant>,
    thinking: bool,
    waiting: bool,
    waiting_for_user: bool,
    compacting: bool,
    task_count: usize,
    reconnect_status: Option<&ReconnectStatus>,
) -> Line<'static> {
    let elapsed = started.map_or(Duration::ZERO, |started| started.elapsed());
    let mut spans = shimmer_spans("•", elapsed);
    spans.push(Span::raw(" "));
    let mut label_spans = shimmer_spans(
        if compacting {
            "Compacting"
        } else if waiting_for_user {
            "Waiting for user"
        } else if waiting {
            "Waiting"
        } else if thinking {
            "Thinking"
        } else {
            "Working"
        },
        elapsed,
    );
    for span in &mut label_spans {
        span.style = span.style.add_modifier(Modifier::BOLD);
    }
    spans.extend(label_spans);
    spans.push(Span::styled(
        format!(
            " ({} • esc to interrupt)",
            format_elapsed(elapsed.as_secs())
        ),
        DIM_STYLE,
    ));
    if !compacting && task_count != 0 {
        spans.push(Span::styled(
            format!(
                " • {task_count} task{}",
                if task_count == 1 { "" } else { "s" }
            ),
            DIM_STYLE,
        ));
    }
    let reconnect = reconnect_status.filter(|reconnect| reconnect.deadline > Instant::now());
    if let Some(reconnect) = reconnect {
        let remaining = reconnect.deadline.saturating_duration_since(Instant::now());
        let seconds = u64::try_from(remaining.as_millis().div_ceil(1_000)).unwrap_or(u64::MAX);
        let reason = reconnect.reason.replace(['_', '-'], " ");
        spans.push(Span::styled(
            format!(
                " • Reconnecting in {seconds}s • attempt {}/{} • {reason}",
                reconnect.next_attempt, reconnect.max_attempts
            ),
            DIM_STYLE,
        ));
    }
    if !waiting
        && !waiting_for_user
        && reconnect.is_none()
        && let Some(inactive_for) = last_activity.map(|activity| activity.elapsed())
        && inactive_for >= INACTIVITY_WARNING_THRESHOLD
    {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!(
                "No activity in past {}",
                format_elapsed(inactive_for.as_secs())
            ),
            NOTICE_STYLE.add_modifier(Modifier::BOLD),
        ));
    }
    Line::from(spans)
}

fn shimmer_spans(text: &str, elapsed: Duration) -> Vec<Span<'static>> {
    let characters = text.chars().collect::<Vec<_>>();
    let padding = 10_usize;
    let period = characters.len() + padding * 2;
    let period_millis = WORKING_SHIMMER_PERIOD.as_millis().max(1);
    let position = usize::try_from(
        (elapsed.as_millis() % period_millis) * u128::try_from(period).unwrap_or_default()
            / period_millis,
    )
    .unwrap_or_default();
    characters
        .into_iter()
        .enumerate()
        .map(|(index, character)| {
            let highlight = 5_usize.saturating_sub((index + padding).abs_diff(position));
            let shade = 244 + u8::try_from(highlight * 2).unwrap_or(10);
            Span::styled(
                character.to_string(),
                Style::default().fg(Color::Indexed(shade)),
            )
        })
        .collect()
}
