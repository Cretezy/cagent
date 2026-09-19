//! Width-aware projection of durable transcript items into terminal rows.

use super::welcome::welcome_tip_line;
use super::{
    DIM_STYLE, USER_STYLE, dim_separator_rows, edit_history_line_targets, pad_history_line,
    render_edit_history, welcome_card_rows, wrap_log_lines,
};
use crate::app::TranscriptBlock;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use std::path::Path;
use std::sync::Arc;
use unicode_width::UnicodeWidthStr;

type ExplorationRenderState = (crate::markdown::ExplorationToggleTarget, bool);

/// The semantic role of one visible transcript block. Flow composition uses
/// roles rather than frontend-specific block types so primary and delegated
/// transcripts share exactly the same boundaries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TranscriptFlowRole {
    Assistant,
    NonMessage,
    Other,
}

/// Transcript entries whose block rendering is shared by the primary and
/// delegated logs.
pub(crate) enum TranscriptEntry<'a> {
    Assistant(&'a cagent_agent::presentation::MarkdownDocument),
    ToolGroups(&'a [cagent_agent::presentation::ToolActivityGroup]),
}

pub(crate) struct WrappedLayout {
    pub(crate) width: u16,
    pub(crate) rows: Vec<crate::markdown::DisplayRow>,
}

/// The currently materialized portion of the durable transcript.
pub(crate) struct HistoryLayoutState {
    pub(crate) rendered: Option<WrappedLayout>,
    pub(crate) start_block: usize,
    pub(crate) width: Option<u16>,
}

impl HistoryLayoutState {
    pub(crate) fn clear(&mut self) {
        self.rendered = None;
        self.start_block = 0;
        self.width = None;
    }
}

impl Default for HistoryLayoutState {
    fn default() -> Self {
        Self {
            rendered: None,
            start_block: 0,
            width: None,
        }
    }
}

pub(crate) type BlockLayoutCache = std::collections::HashMap<
    (cagent_agent::protocol::TranscriptBlockId, u16),
    Vec<crate::markdown::DisplayRow>,
>;

#[cfg(test)]
pub(crate) fn history_rows(
    welcome: &[Line<'static>],
    tip: Option<&str>,
    history: &[TranscriptBlock],
    workspace: &Path,
    session_id: cagent_agent::protocol::ConversationId,
    width: u16,
    editor_enabled: bool,
) -> Vec<crate::markdown::DisplayRow> {
    let items = history
        .iter()
        .map(|item| history_item_rows_with_editor(item, workspace, width, editor_enabled))
        .collect::<Vec<_>>();
    history_rows_from_items(welcome, tip, history, &items, workspace, session_id, width)
}

#[cfg(test)]
pub(crate) fn history_item_rows(
    item: &TranscriptBlock,
    workspace: &Path,
    width: u16,
) -> Vec<crate::markdown::DisplayRow> {
    history_item_rows_with_editor(item, workspace, width, true)
}

#[cfg(test)]
pub(crate) fn history_item_rows_with_editor(
    item: &TranscriptBlock,
    workspace: &Path,
    width: u16,
    editor_enabled: bool,
) -> Vec<crate::markdown::DisplayRow> {
    history_item_rows_with_explorations(
        item,
        workspace,
        width,
        editor_enabled,
        None,
        &Default::default(),
    )
}

pub(crate) fn history_item_rows_with_explorations(
    item: &TranscriptBlock,
    workspace: &Path,
    width: u16,
    editor_enabled: bool,
    block_id: Option<&cagent_agent::protocol::TranscriptBlockId>,
    expanded: &std::collections::HashSet<crate::markdown::ExplorationToggleTarget>,
) -> Vec<crate::markdown::DisplayRow> {
    let mut rows = match &item.kind {
        cagent_agent::protocol::TranscriptBlockKind::WorkspaceTransition {
            label,
            target,
            base,
            path,
        } => workspace_transition_rows(label, target, base.as_deref(), path.as_deref()),
        cagent_agent::protocol::TranscriptBlockKind::User {
            label,
            text,
            attachments,
            images,
            image_chips,
        } => user_rows(
            label.as_deref(),
            text,
            attachments,
            images,
            image_chips,
            workspace,
            width,
        ),
        cagent_agent::protocol::TranscriptBlockKind::Compacted { summary } => {
            vec![
                crate::markdown::DisplayRow::plain(super::compacted_line(width))
                    .with_compaction(summary.clone()),
            ]
        }
        cagent_agent::protocol::TranscriptBlockKind::Assistant {
            document,
            source: _,
            message,
        } => {
            let mut rows = transcript_entry_rows_with_state(
                TranscriptEntry::Assistant(document),
                workspace,
                width,
                editor_enabled,
                |_, _| None,
            );
            if let Some(message) = message {
                rows.extend(plain_rows(&[Line::from(message.clone())], width));
            }
            rows
        }
        cagent_agent::protocol::TranscriptBlockKind::Plan { document, .. } => {
            plan_markdown_rows_with_paths(document, workspace, width, editor_enabled)
        }
        cagent_agent::protocol::TranscriptBlockKind::AcceptedPlan {
            document,
            clear_context,
            ..
        } if *clear_context => plan_markdown_rows_with_heading(
            document,
            workspace,
            width,
            "Accepted plan",
            editor_enabled,
        ),
        cagent_agent::protocol::TranscriptBlockKind::AcceptedPlan {
            compaction_summary, ..
        } => {
            let mut rows = plain_rows(
                &[Line::from(vec![
                    Span::styled("• ", DIM_STYLE),
                    Span::styled(
                        "Plan accepted",
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                ])],
                width,
            );
            if let Some(summary) = compaction_summary {
                rows.push(crate::markdown::DisplayRow::plain(Line::default()));
                rows.push(
                    crate::markdown::DisplayRow::plain(super::compacted_line(width))
                        .with_compaction(summary.clone()),
                );
            }
            rows
        }
        cagent_agent::protocol::TranscriptBlockKind::ToolGroups { groups } => {
            transcript_entry_rows_with_state(
                TranscriptEntry::ToolGroups(groups),
                workspace,
                width,
                editor_enabled,
                |group_index, group| {
                    block_id.and_then(|block_id| {
                    matches!(group, cagent_agent::presentation::ToolActivityGroup::Exploration { activities, .. } if activities.len() > 4)
                        .then(|| {
                            let target = crate::markdown::ExplorationToggleTarget::Transcript {
                                block_id: block_id.clone(),
                                group_index,
                            };
                            let is_expanded = expanded.contains(&target);
                            (target, is_expanded)
                        })
                })
                },
            )
        }
        cagent_agent::protocol::TranscriptBlockKind::Edits { diff } => {
            let targets = edit_path_targets(diff, workspace);
            let diff_targets = edit_history_line_targets(diff);
            let diff = Arc::new(diff.clone());
            render_edit_history(&diff, workspace, width)
                .into_iter()
                .enumerate()
                .flat_map(|(line_index, line)| {
                    let target = diff_targets.get(line_index).copied().flatten();
                    let diff = Arc::clone(&diff);
                    plain_rows(std::slice::from_ref(&line), width)
                        .into_iter()
                        .map(move |row| match target {
                            Some((file_index, old_line, new_line)) => {
                                row.with_diff_line(crate::markdown::DiffLineTarget {
                                    diff: Arc::clone(&diff),
                                    file_index,
                                    old_line,
                                    new_line,
                                })
                            }
                            None => row,
                        })
                })
                .map(|row| annotate_owned_path_labels(row, &targets))
                .collect()
        }
        cagent_agent::protocol::TranscriptBlockKind::Notice { message } => {
            if let Some(elapsed) = message.strip_prefix("Worked for ") {
                vec![crate::markdown::DisplayRow::plain(super::worked_line(
                    elapsed, width,
                ))]
            } else {
                let style = if message == "Retried response" {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                plain_rows(
                    &[Line::from(vec![
                        Span::styled("• ", DIM_STYLE),
                        Span::styled(message.clone(), style),
                    ])],
                    width,
                )
            }
        }
        cagent_agent::protocol::TranscriptBlockKind::Recap { text } => {
            let line = Line::from(vec![
                Span::styled("• ", DIM_STYLE),
                Span::styled(format!("Recap: {text}"), DIM_STYLE),
            ]);
            vec![crate::markdown::DisplayRow::plain(pad_history_line(
                &crate::app::list::truncate_line(line, usize::from(width)),
                width,
            ))]
        }
        cagent_agent::protocol::TranscriptBlockKind::PermissionDenied { resource, reason } => {
            permission_denied_rows(resource, reason, width)
        }
        cagent_agent::protocol::TranscriptBlockKind::Interrupt { queued_steering } => {
            let (message, style) = if *queued_steering {
                (
                    "• Model interrupted to submit steering instructions",
                    crate::app::NOTICE_STYLE,
                )
            } else {
                ("■ Conversation interrupted", crate::app::ERROR_STYLE)
            };
            plain_rows(&[Line::from(Span::styled(message, style))], width)
        }
        cagent_agent::protocol::TranscriptBlockKind::Work { .. } => Vec::new(),
    };
    trim_outer_trailing_blank_rows(&mut rows);
    rows
}

fn workspace_transition_rows(
    label: &str,
    target: &str,
    base: Option<&str>,
    path: Option<&Path>,
) -> Vec<crate::markdown::DisplayRow> {
    let target = crate::render::terminal_safe(target);
    let mut spans = vec![
        Span::styled(
            format!("• {label}"),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(target.clone(), crate::app::CHIP_STYLE),
    ];
    if let Some(base) = base {
        spans.push(Span::styled(format!(" · {base}"), DIM_STYLE));
    }
    let line = Line::from(spans);
    let row = crate::markdown::DisplayRow::plain(line);
    let target_column = format!("• {label} ").width();
    let row = match path {
        Some(path) => row.with_directory_path(target_column, target.width(), path.to_path_buf()),
        None => row,
    };
    vec![row]
}

#[cfg(test)]
pub(crate) fn tool_group_rows(
    groups: &[cagent_agent::presentation::ToolActivityGroup],
    workspace: &Path,
    width: u16,
) -> Vec<crate::markdown::DisplayRow> {
    tool_group_rows_with_state(groups, workspace, width, |_, _| None)
}

pub(crate) fn tool_group_rows_with_state(
    groups: &[cagent_agent::presentation::ToolActivityGroup],
    workspace: &Path,
    width: u16,
    mut exploration_state: impl FnMut(
        usize,
        &cagent_agent::presentation::ToolActivityGroup,
    ) -> Option<ExplorationRenderState>,
) -> Vec<crate::markdown::DisplayRow> {
    groups
        .iter()
        .enumerate()
        .flat_map(|(group_index, group)| {
            let exploration_state = exploration_state(group_index, group);
            let mcp_call = match group {
                cagent_agent::presentation::ToolActivityGroup::Mcp { call } => {
                    Some(Arc::new(call.clone()))
                }
                _ => None,
            };
            let web_fetch_target = match group {
                cagent_agent::presentation::ToolActivityGroup::WebFetch {
                    url,
                    redirected_url,
                    format,
                    output: Some(output),
                    status: cagent_agent::presentation::ToolActivityStatus::Succeeded,
                    ..
                } if !output.is_empty() => Some(crate::markdown::WebFetchOutputTarget {
                    url: url.clone(),
                    redirected_url: redirected_url.clone(),
                    format: *format,
                    output: Arc::<str>::from(output.as_str()),
                }),
                _ => None,
            };
            let path_targets = match group {
                cagent_agent::presentation::ToolActivityGroup::Exploration {
                    activities, ..
                } => activities
                    .iter()
                    .flat_map(|activity| {
                        let targets = match activity.kind {
                            cagent_agent::presentation::ExplorationActivityKind::Search => {
                                activity.scopes.as_slice()
                            }
                            cagent_agent::presentation::ExplorationActivityKind::Read => {
                                activity.targets.as_slice()
                            }
                            cagent_agent::presentation::ExplorationActivityKind::List => {
                                if activity.scopes.is_empty() {
                                    activity.targets.as_slice()
                                } else {
                                    activity.scopes.as_slice()
                                }
                            }
                        };
                        targets.iter()
                    })
                    .map(|label| {
                        (
                            label.clone(),
                            cagent_agent::presentation::resolve_display_path(label, workspace),
                        )
                    })
                    .collect::<Vec<_>>(),
                cagent_agent::presentation::ToolActivityGroup::Edit { diff, .. } => {
                    edit_path_targets(diff, workspace)
                }
                _ => Vec::new(),
            };
            let terminal_id = match group {
                cagent_agent::presentation::ToolActivityGroup::TerminalWrite {
                    terminal_id: Some(terminal_id),
                    ..
                } => Some(*terminal_id),
                _ => None,
            };
            let (diff, diff_targets) = match group {
                cagent_agent::presentation::ToolActivityGroup::Edit { diff, .. } => (
                    Some(Arc::new(diff.clone())),
                    edit_history_line_targets(diff),
                ),
                _ => (None, Vec::new()),
            };
            super::activity::render_tool_group_with_workspace(
                group,
                workspace,
                width,
                exploration_state
                    .as_ref()
                    .is_some_and(|(_, expanded)| *expanded),
            )
            .into_iter()
            .enumerate()
            .flat_map(move |(line_index, rendered)| {
                let path_targets = path_targets.clone();
                let web_fetch_target = web_fetch_target.clone();
                let mcp_call = mcp_call.clone();
                let diff = diff.clone();
                let diff_target = diff_targets.get(line_index).copied().flatten();
                let toggle_target = rendered
                    .exploration_toggle
                    .then(|| exploration_state.as_ref().map(|(target, _)| target.clone()))
                    .flatten();
                plain_rows(std::slice::from_ref(&rendered.line), width)
                    .into_iter()
                    .map(move |row| {
                        let row = annotate_owned_path_labels(row, &path_targets);
                        let row = match &mcp_call {
                            Some(call) => row.with_mcp_call(Arc::clone(call)),
                            None => row,
                        };
                        let row = match &web_fetch_target {
                            Some(target) => row.with_web_fetch_output(target.clone()),
                            None => row,
                        };
                        let row = match (&diff, diff_target) {
                            (Some(diff), Some((file_index, old_line, new_line))) => row
                                .with_diff_line(crate::markdown::DiffLineTarget {
                                    diff: Arc::clone(diff),
                                    file_index,
                                    old_line,
                                    new_line,
                                }),
                            _ => row,
                        };
                        let row = match &toggle_target {
                            Some(target) => row.with_exploration_toggle(target.clone()),
                            None => row,
                        };
                        if line_index > 0 {
                            if let Some(terminal_id) = terminal_id {
                                row.with_terminal_output(terminal_id)
                            } else {
                                row
                            }
                        } else {
                            row
                        }
                    })
            })
        })
        .collect()
}

pub(crate) fn transcript_entry_rows_with_state(
    entry: TranscriptEntry<'_>,
    workspace: &Path,
    width: u16,
    editor_enabled: bool,
    exploration_state: impl FnMut(
        usize,
        &cagent_agent::presentation::ToolActivityGroup,
    ) -> Option<ExplorationRenderState>,
) -> Vec<crate::markdown::DisplayRow> {
    match entry {
        TranscriptEntry::Assistant(document) => {
            assistant_markdown_rows_with_paths(document, workspace, width, editor_enabled)
        }
        TranscriptEntry::ToolGroups(groups) => {
            tool_group_rows_with_state(groups, workspace, width, exploration_state)
        }
    }
}

/// Composes already-rendered semantic blocks into one transcript flow.
/// Presentation-only trailing blanks are discarded before each boundary.
pub(crate) fn transcript_flow_rows(
    items: impl IntoIterator<Item = (TranscriptFlowRole, Vec<crate::markdown::DisplayRow>)>,
    width: u16,
) -> Vec<crate::markdown::DisplayRow> {
    let mut rows = Vec::new();
    let mut previous_role = None;
    for (role, item_rows) in items {
        if item_rows.is_empty() {
            continue;
        }
        if !rows.is_empty() {
            append_transcript_boundary(&mut rows, previous_role, role, width);
        }
        rows.extend(item_rows);
        previous_role = Some(role);
    }
    rows
}

pub(crate) fn append_transcript_boundary(
    rows: &mut Vec<crate::markdown::DisplayRow>,
    previous_role: Option<TranscriptFlowRole>,
    role: TranscriptFlowRole,
    width: u16,
) {
    trim_outer_trailing_blank_rows(rows);
    if previous_role == Some(TranscriptFlowRole::NonMessage)
        && role == TranscriptFlowRole::Assistant
    {
        rows.extend(dim_separator_rows(width));
    } else {
        rows.push(crate::markdown::DisplayRow::plain(Line::default()));
    }
}

#[cfg(test)]
pub(crate) fn history_rows_from_items<T: AsRef<[crate::markdown::DisplayRow]>>(
    welcome: &[Line<'static>],
    tip: Option<&str>,
    history: &[TranscriptBlock],
    items: &[T],
    workspace: &Path,
    session_id: cagent_agent::protocol::ConversationId,
    width: u16,
) -> Vec<crate::markdown::DisplayRow> {
    history_rows_from_items_with_collapsed_activity(
        welcome,
        tip,
        history,
        items,
        workspace,
        session_id,
        width,
        false,
        &Default::default(),
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn history_rows_from_items_with_collapsed_activity<
    T: AsRef<[crate::markdown::DisplayRow]>,
>(
    welcome: &[Line<'static>],
    tip: Option<&str>,
    history: &[TranscriptBlock],
    items: &[T],
    workspace: &Path,
    session_id: cagent_agent::protocol::ConversationId,
    width: u16,
    collapse: bool,
    expanded: &std::collections::HashSet<crate::markdown::ActivityCollapseTarget>,
) -> Vec<crate::markdown::DisplayRow> {
    let mut rows = plain_rows(
        &welcome_card_rows(welcome, workspace, session_id, width),
        width,
    );
    if items.is_empty() {
        if let Some(tip) = tip {
            rows.push(crate::markdown::DisplayRow::plain(Line::default()));
            rows.extend(plain_rows(&[welcome_tip_line(tip)], width));
        }
    }
    let flow_items = if collapse {
        collapsed_history_flow(history, items, width, expanded)
    } else {
        items
            .iter()
            .enumerate()
            .map(|(index, item_rows)| {
                (
                    transcript_block_role(&history[index]),
                    item_rows.as_ref().to_vec(),
                )
            })
            .collect()
    };
    let flow = transcript_flow_rows(flow_items, width);
    if !flow.is_empty() {
        if !rows.is_empty() {
            trim_outer_trailing_blank_rows(&mut rows);
            rows.push(crate::markdown::DisplayRow::plain(Line::default()));
        }
        rows.extend(flow);
    }
    rows
}

fn collapsed_history_flow<T: AsRef<[crate::markdown::DisplayRow]>>(
    history: &[TranscriptBlock],
    items: &[T],
    width: u16,
    expanded: &std::collections::HashSet<crate::markdown::ActivityCollapseTarget>,
) -> Vec<(TranscriptFlowRole, Vec<crate::markdown::DisplayRow>)> {
    let mut flow = Vec::new();
    let mut index = 0;
    while index < history.len() {
        let Some(mut summary) = collapsible_block_summary(&history[index]) else {
            flow.push((
                transcript_block_role(&history[index]),
                items[index].as_ref().to_vec(),
            ));
            index += 1;
            continue;
        };
        let start = index;
        let mut active = collapsible_block_is_active(&history[index]);
        index += 1;
        while index < history.len() {
            let Some(next) = collapsible_block_summary(&history[index]) else {
                break;
            };
            summary.merge(&next);
            active |= collapsible_block_is_active(&history[index]);
            index += 1;
        }
        if summary.is_empty() {
            for item_index in start..index {
                flow.push((
                    transcript_block_role(&history[item_index]),
                    items[item_index].as_ref().to_vec(),
                ));
            }
            continue;
        }
        let target = crate::markdown::ActivityCollapseTarget::Transcript(history[start].id.clone());
        let is_expanded = expanded.contains(&target);
        let pulse = active && index == history.len();
        let mut rows = collapsed_summary_rows(&summary, &target, is_expanded, pulse, width);
        if is_expanded {
            for item in &items[start..index] {
                rows.extend(expanded_activity_rows(item.as_ref(), &target, width));
            }
        }
        flow.push((TranscriptFlowRole::NonMessage, rows));
    }
    flow
}

fn collapsible_block_summary(
    block: &TranscriptBlock,
) -> Option<cagent_agent::presentation::CollapsedActivitySummary> {
    let mut summary = cagent_agent::presentation::CollapsedActivitySummary::default();
    match &block.kind {
        cagent_agent::protocol::TranscriptBlockKind::ToolGroups { groups }
            if groups.iter().all(|group| {
                cagent_agent::presentation::summarize_collapsible_group(&mut summary, group)
            }) => {}
        cagent_agent::protocol::TranscriptBlockKind::Edits { diff } => {
            cagent_agent::presentation::summarize_collapsible_diff(&mut summary, diff);
        }
        _ => return None,
    }
    Some(summary)
}

fn collapsible_block_is_active(block: &TranscriptBlock) -> bool {
    matches!(
        &block.kind,
        cagent_agent::protocol::TranscriptBlockKind::ToolGroups { groups }
            if groups.iter().any(activity_group_is_active)
    )
}

pub(crate) fn activity_group_is_active(
    group: &cagent_agent::presentation::ToolActivityGroup,
) -> bool {
    use cagent_agent::presentation::{ToolActivityGroup, ToolActivityStatus};

    match group {
        ToolActivityGroup::Exploration { active, .. } => *active,
        ToolActivityGroup::Tool { status, .. }
        | ToolActivityGroup::Edit { status, .. }
        | ToolActivityGroup::WebSearch { status, .. }
        | ToolActivityGroup::WebFetch { status, .. }
        | ToolActivityGroup::Bash { status, .. }
        | ToolActivityGroup::TerminalWrite { status, .. }
        | ToolActivityGroup::TerminalKill { status, .. }
        | ToolActivityGroup::Delegate { status, .. } => *status == ToolActivityStatus::Pending,
        ToolActivityGroup::Mcp { call } => call.status == ToolActivityStatus::Pending,
        ToolActivityGroup::Question { .. } => false,
    }
}

pub(crate) fn collapsed_summary_rows(
    summary: &cagent_agent::presentation::CollapsedActivitySummary,
    target: &crate::markdown::ActivityCollapseTarget,
    expanded: bool,
    pulse: bool,
    width: u16,
) -> Vec<crate::markdown::DisplayRow> {
    let line = Line::from(vec![
        Span::styled(
            "• ",
            if pulse {
                super::activity::pending_bullet_style()
            } else {
                DIM_STYLE
            },
        ),
        Span::styled(
            summary.text(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                " ({})",
                if expanded {
                    "click to hide"
                } else {
                    "click to view"
                }
            ),
            DIM_STYLE,
        ),
    ]);
    plain_rows(&[line], width)
        .into_iter()
        .map(|row| row.with_activity_collapse(target.clone()))
        .collect()
}

pub(crate) fn expanded_activity_rows(
    rows: &[crate::markdown::DisplayRow],
    target: &crate::markdown::ActivityCollapseTarget,
    width: u16,
) -> Vec<crate::markdown::DisplayRow> {
    let mut expanded = Vec::with_capacity(rows.len() + 2);
    expanded.push(
        crate::markdown::DisplayRow::plain(pad_history_line(
            &Line::default().style(USER_STYLE),
            width,
        ))
        .with_activity_collapse(target.clone()),
    );
    expanded.extend(rows.iter().cloned().map(|mut row| {
        if row.line.style.bg.is_none() {
            row.line = row.line.style(USER_STYLE);
        }
        row.line = pad_history_line(&row.line, width);
        row.with_activity_collapse(target.clone())
    }));
    expanded.push(
        crate::markdown::DisplayRow::plain(pad_history_line(
            &Line::default().style(USER_STYLE),
            width,
        ))
        .with_activity_collapse(target.clone()),
    );
    expanded
}

/// Builds only the rows that must be inserted before an already-rendered
/// transcript suffix when one older block becomes visible.
pub(crate) fn history_prefix_rows(
    welcome: &[Line<'static>],
    item: &TranscriptBlock,
    item_rows: &[crate::markdown::DisplayRow],
    next_visible_item: Option<&TranscriptBlock>,
    workspace: &Path,
    session_id: cagent_agent::protocol::ConversationId,
    width: u16,
) -> Vec<crate::markdown::DisplayRow> {
    let mut rows = plain_rows(
        &welcome_card_rows(welcome, workspace, session_id, width),
        width,
    );
    if !item_rows.is_empty() {
        if !rows.is_empty() {
            trim_outer_trailing_blank_rows(&mut rows);
            rows.push(crate::markdown::DisplayRow::plain(Line::default()));
        }
        rows.extend(item_rows.iter().cloned());
    }
    if next_visible_item.is_some() && !rows.is_empty() {
        append_transcript_boundary(
            &mut rows,
            (!item_rows.is_empty()).then(|| transcript_block_role(item)),
            next_visible_item
                .map(transcript_block_role)
                .unwrap_or(TranscriptFlowRole::Other),
            width,
        );
    }
    rows
}

fn is_assistant_message(item: &TranscriptBlock) -> bool {
    matches!(
        item.kind,
        cagent_agent::protocol::TranscriptBlockKind::Assistant { .. }
    )
}

fn transcript_block_role(item: &TranscriptBlock) -> TranscriptFlowRole {
    if is_assistant_message(item) {
        TranscriptFlowRole::Assistant
    } else if is_non_message_block(item) {
        TranscriptFlowRole::NonMessage
    } else {
        TranscriptFlowRole::Other
    }
}

fn user_rows(
    label: Option<&str>,
    text: &str,
    attachments: &[cagent_agent::presentation::SubmittedAttachment],
    images: &[cagent_agent::protocol::ImageAttachment],
    image_chips: &[cagent_agent::protocol::ImageChipRange],
    workspace: &Path,
    width: u16,
) -> Vec<crate::markdown::DisplayRow> {
    let substitutions = super::submitted_attachment_labels(text, attachments, image_chips);
    let mut lines = vec![Line::default().style(USER_STYLE)];
    let mut line_start = 0;
    for (index, line) in text.split('\n').enumerate() {
        let line_end = line_start + line.len();
        let mut spans = vec![Span::styled(
            if index == 0 { "› " } else { "  " },
            super::ACCENT_STYLE,
        )];
        if index == 0
            && let Some(label) = label
        {
            spans.push(Span::styled(
                format!("{label}: "),
                Style::default().add_modifier(Modifier::BOLD),
            ));
        }
        spans.extend(super::styled_user_slice(
            text,
            line_start,
            line_end,
            &substitutions,
        ));
        lines.push(Line::from(spans).style(USER_STYLE));
        line_start = line_end.saturating_add(1);
    }
    lines.push(Line::default().style(USER_STYLE));
    lines.push(Line::default());
    let targets = attachments
        .iter()
        .map(|attachment| {
            let label = super::attachment_spec_label(
                &attachment.spec,
                super::attachment_kind_label(
                    attachment.kind
                        == cagent_agent::presentation::SubmittedAttachmentKind::Directory,
                ),
            );
            (
                label,
                cagent_agent::presentation::resolve_display_path(
                    &attachment.spec.path.to_string_lossy(),
                    workspace,
                ),
            )
        })
        .collect::<Vec<_>>();
    plain_rows(&lines, width)
        .into_iter()
        .map(|row| {
            let row = annotate_owned_path_labels(row, &targets);
            annotate_image_labels(row, images)
        })
        .collect()
}

fn annotate_image_labels(
    mut row: crate::markdown::DisplayRow,
    images: &[cagent_agent::protocol::ImageAttachment],
) -> crate::markdown::DisplayRow {
    use unicode_width::UnicodeWidthStr as _;

    let mut column = 0;
    let spans = row.line.spans.clone();
    for span in spans {
        let content = span.content.as_ref();
        if span.style == super::CHIP_STYLE {
            for image in images {
                let label = format!("[Image #{}]", image.number);
                let mut start = 0;
                while let Some(relative) = content[start..].find(&label) {
                    let byte = start + relative;
                    row = row.with_image(
                        column + content[..byte].width(),
                        label.width(),
                        image.clone(),
                    );
                    start = byte + label.len();
                }
            }
        }
        column += content.width();
    }
    row
}

fn annotate_path_labels(
    mut row: crate::markdown::DisplayRow,
    targets: &[(&str, std::path::PathBuf)],
) -> crate::markdown::DisplayRow {
    use unicode_width::UnicodeWidthStr as _;

    let rendered = row.line.to_string();
    for (label, path) in targets {
        let mut start = 0;
        while let Some(relative) = rendered[start..].find(label) {
            let byte = start + relative;
            let column = rendered[..byte].width();
            row = row.with_path(column, label.width(), path.clone());
            start = byte + label.len();
        }
    }
    row
}

fn annotate_owned_path_labels(
    row: crate::markdown::DisplayRow,
    targets: &[(String, std::path::PathBuf)],
) -> crate::markdown::DisplayRow {
    let borrowed = targets
        .iter()
        .map(|(label, path)| (label.as_str(), path.clone()))
        .collect::<Vec<_>>();
    annotate_path_labels(row, &borrowed)
}

fn edit_path_targets(
    diff: &cagent_agent::tools::SemanticDiff,
    workspace: &Path,
) -> Vec<(String, std::path::PathBuf)> {
    diff.files
        .iter()
        .filter(|file| file.kind != cagent_agent::tools::DiffFileKind::Deleted)
        .filter_map(|file| file.new_path.as_ref().or(file.old_path.as_ref()))
        .map(|path| {
            (
                cagent_agent::presentation::display_activity_path(path, workspace),
                if path.is_absolute() {
                    path.clone()
                } else {
                    workspace.join(path)
                },
            )
        })
        .collect()
}

pub(super) fn is_non_message_block(item: &TranscriptBlock) -> bool {
    matches!(
        item.kind,
        cagent_agent::protocol::TranscriptBlockKind::Notice { .. }
            | cagent_agent::protocol::TranscriptBlockKind::Recap { .. }
            | cagent_agent::protocol::TranscriptBlockKind::ToolGroups { .. }
            | cagent_agent::protocol::TranscriptBlockKind::Edits { .. }
    )
}

fn permission_denied_rows(
    resource: &str,
    reason: &str,
    width: u16,
) -> Vec<crate::markdown::DisplayRow> {
    let mut reason_rows = reason.split('\n');
    let first_reason = reason_rows.next().unwrap_or_default();
    let mut lines = vec![Line::from(vec![
        Span::styled("• Permission denied: ", crate::app::ERROR_STYLE),
        Span::raw(resource.to_owned()),
        Span::styled(
            format!(" · {}", crate::render::terminal_safe(first_reason)),
            DIM_STYLE,
        ),
    ])];
    lines.extend(reason_rows.map(|row| {
        Line::from(vec![
            Span::raw("  "),
            Span::styled(crate::render::terminal_safe(row), DIM_STYLE),
        ])
    }));
    plain_rows(&lines, width)
}

/// Removes presentation-only spacing. Styled blank rows are part of a user or
/// plan card and must remain attached to that block.
fn trim_outer_trailing_blank_rows(rows: &mut Vec<crate::markdown::DisplayRow>) {
    while rows
        .last()
        .is_some_and(|row| row.line.style != USER_STYLE && row.line.to_string().trim().is_empty())
    {
        rows.pop();
    }
}

pub(crate) fn plan_markdown_rows_with_paths(
    document: &cagent_agent::presentation::MarkdownDocument,
    workspace: &Path,
    width: u16,
    editor_enabled: bool,
) -> Vec<crate::markdown::DisplayRow> {
    plan_markdown_rows_with_heading(document, workspace, width, "Proposed Plan", editor_enabled)
}

fn plan_markdown_rows_with_heading(
    document: &cagent_agent::presentation::MarkdownDocument,
    workspace: &Path,
    width: u16,
    heading: &str,
    editor_enabled: bool,
) -> Vec<crate::markdown::DisplayRow> {
    use ratatui::style::{Modifier, Style};

    let mut rows = vec![
        crate::markdown::DisplayRow::plain(Line::from(vec![
            Span::styled("• ", DIM_STYLE),
            Span::styled(
                heading.to_owned(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ])),
        crate::markdown::DisplayRow::plain(Line::default()),
        crate::markdown::DisplayRow::plain(pad_history_line(
            &Line::default().style(USER_STYLE),
            width,
        )),
    ];
    rows.extend(
        crate::markdown::layout_document_with_paths(
            document,
            width.saturating_sub(4).max(1),
            workspace,
            editor_enabled,
        )
        .into_iter()
        .map(|mut row| {
            row = row.prefixed(Span::raw("  "));
            row.line = pad_history_line(&row.line.style(USER_STYLE), width);
            row
        }),
    );
    rows.push(crate::markdown::DisplayRow::plain(pad_history_line(
        &Line::default().style(USER_STYLE),
        width,
    )));
    rows.push(crate::markdown::DisplayRow::plain(Line::default()));
    rows
}

pub(super) fn streaming_rows(
    document: &cagent_agent::presentation::MarkdownDocument,
    status: Option<&Line<'static>>,
    width: u16,
    workspace: &Path,
    editor_enabled: bool,
) -> Vec<crate::markdown::DisplayRow> {
    if document.is_empty() && status.is_none() {
        return Vec::new();
    }
    let mut rows = assistant_markdown_rows_with_paths(document, workspace, width, editor_enabled);
    if let Some(status) = status {
        rows.extend(plain_rows(std::slice::from_ref(status), width));
    }
    rows.push(crate::markdown::DisplayRow::plain(Line::default()));
    rows
}

#[cfg(test)]
pub(crate) fn assistant_markdown_rows(
    document: &cagent_agent::presentation::MarkdownDocument,
    width: u16,
) -> Vec<crate::markdown::DisplayRow> {
    assistant_markdown_rows_impl(document, width, None, false)
}

pub(crate) fn assistant_markdown_rows_with_paths(
    document: &cagent_agent::presentation::MarkdownDocument,
    workspace: &Path,
    width: u16,
    editor_enabled: bool,
) -> Vec<crate::markdown::DisplayRow> {
    assistant_markdown_rows_impl(document, width, Some(workspace), editor_enabled)
}

fn assistant_markdown_rows_impl(
    document: &cagent_agent::presentation::MarkdownDocument,
    width: u16,
    workspace: Option<&Path>,
    editor_enabled: bool,
) -> Vec<crate::markdown::DisplayRow> {
    workspace
        .map_or_else(
            || crate::markdown::layout_document(document, width.saturating_sub(2).max(1)),
            |workspace| {
                crate::markdown::layout_document_with_paths(
                    document,
                    width.saturating_sub(2).max(1),
                    workspace,
                    editor_enabled,
                )
            },
        )
        .into_iter()
        .enumerate()
        .map(|(index, row)| {
            row.prefixed(if index == 0 {
                Span::styled("• ", Style::default())
            } else {
                Span::raw("  ")
            })
        })
        .collect()
}

pub(crate) fn append_visible_rows(
    rows: &[crate::markdown::DisplayRow],
    offset: &mut usize,
    remaining: &mut usize,
    output: &mut Vec<crate::markdown::DisplayRow>,
) {
    if *remaining == 0 {
        return;
    }
    if *offset >= rows.len() {
        *offset -= rows.len();
        return;
    }
    let start = *offset;
    let end = start.saturating_add(*remaining).min(rows.len());
    output.extend(rows[start..end].iter().cloned());
    *offset = 0;
    *remaining = remaining.saturating_sub(end.saturating_sub(start));
}

fn plain_rows(lines: &[Line<'static>], width: u16) -> Vec<crate::markdown::DisplayRow> {
    wrap_log_lines(lines, width)
        .into_iter()
        .map(|line| pad_history_line(&line, width))
        .map(crate::markdown::DisplayRow::plain)
        .collect()
}

#[cfg(test)]
mod flow_tests {
    use super::*;

    fn block(text: &str) -> Vec<crate::markdown::DisplayRow> {
        vec![crate::markdown::DisplayRow::plain(Line::from(
            text.to_owned(),
        ))]
    }

    fn strings(rows: Vec<crate::markdown::DisplayRow>) -> Vec<String> {
        rows.into_iter().map(|row| row.to_string()).collect()
    }

    #[test]
    fn activity_to_persisted_or_streaming_assistant_uses_the_same_separator() {
        let expected = vec!["tools", "", "────────", "", "answer"];
        for assistant in ["persisted", "streaming"] {
            assert_eq!(
                strings(transcript_flow_rows(
                    [
                        (TranscriptFlowRole::NonMessage, block("tools")),
                        (TranscriptFlowRole::Assistant, block("answer")),
                    ],
                    8,
                )),
                expected,
                "{assistant} assistant flow",
            );
        }
    }

    #[test]
    fn other_visible_boundaries_use_one_blank_row() {
        assert_eq!(
            strings(transcript_flow_rows(
                [
                    (TranscriptFlowRole::Assistant, block("answer")),
                    (TranscriptFlowRole::NonMessage, block("tools one")),
                    (TranscriptFlowRole::NonMessage, block("tools two")),
                ],
                8,
            )),
            ["answer", "", "tools one", "", "tools two"],
        );
    }

    #[test]
    fn empty_assistants_are_ignored_and_trailing_blanks_are_trimmed() {
        let mut assistant = block("answer");
        assistant.push(crate::markdown::DisplayRow::plain(Line::default()));
        assistant.push(crate::markdown::DisplayRow::plain(Line::default()));
        assert_eq!(
            strings(transcript_flow_rows(
                [
                    (TranscriptFlowRole::Assistant, assistant),
                    (TranscriptFlowRole::Assistant, Vec::new()),
                    (TranscriptFlowRole::NonMessage, block("tools")),
                ],
                8,
            )),
            ["answer", "", "tools"],
        );
    }

    #[test]
    fn primary_and_delegated_tool_blocks_wrap_identically_at_the_same_width() {
        let groups = vec![cagent_agent::presentation::ToolActivityGroup::Bash {
            node_id: None,
            terminal_id: None,
            command: "echo a deliberately long command that wraps".into(),
            status: cagent_agent::presentation::ToolActivityStatus::Succeeded,
            output: Some("done".into()),
            ansi_output: None,
            exit_code: Some(0),
        }];
        let primary = crate::app::test_tool_groups(groups.clone());
        let primary_rows = history_item_rows(&primary, Path::new("/workspace"), 24);
        let delegated_rows = transcript_entry_rows_with_state(
            TranscriptEntry::ToolGroups(&groups),
            Path::new("/workspace"),
            24,
            true,
            |_, _| None,
        );

        assert_eq!(strings(primary_rows), strings(delegated_rows));
    }
}
