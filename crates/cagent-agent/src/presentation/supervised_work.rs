use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::activity::terminal_activity_status;

pub const READ_TASK_VISIBILITY_DELAY: std::time::Duration = std::time::Duration::from_secs(1);

/// Read-safe terminals stay out of task-oriented UI until they have run for a
/// full second. Missing metadata remains visible for backward compatibility.
#[must_use]
pub fn terminal_visible_as_task(terminal: &crate::TerminalSnapshot) -> bool {
    if terminal.read_safe != Some(true) {
        return true;
    }
    let Ok(started) = terminal.started_at.parse::<u128>() else {
        return true;
    };
    let end = terminal
        .completed_at
        .as_deref()
        .and_then(|value| value.parse::<u128>().ok())
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        });
    end.saturating_sub(started) >= READ_TASK_VISIBILITY_DELAY.as_millis()
}

/// A frontend-neutral row for supervised work. Compact frontends should not
/// display the embedded result or output; those are retained for detail views.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SupervisedWork {
    Agent {
        run: Box<crate::AgentRun>,
    },
    Terminal {
        terminal: Box<crate::TerminalSnapshot>,
    },
}

impl SupervisedWork {
    #[must_use]
    pub fn status(&self) -> crate::ToolActivityStatus {
        match self {
            Self::Agent { run } => match run.status {
                crate::AgentRunStatus::Queued | crate::AgentRunStatus::Running => {
                    crate::ToolActivityStatus::Pending
                }
                crate::AgentRunStatus::Completed => crate::ToolActivityStatus::Succeeded,
                crate::AgentRunStatus::Failed
                | crate::AgentRunStatus::Cancelled
                | crate::AgentRunStatus::Interrupted => crate::ToolActivityStatus::Failed,
            },
            Self::Terminal { terminal } => terminal_activity_status(terminal),
        }
    }

    #[must_use]
    pub fn active(&self) -> bool {
        self.status() == crate::ToolActivityStatus::Pending
    }
}

/// Ephemeral, agent-owned detail state for an active delegated run. It is
/// included in snapshots so a frontend can recover an open detail view after
/// coalescing or resynchronization.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DelegatedRunLive {
    pub id: crate::AgentRunId,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<crate::ModelUsage>,
}

/// Returns whether a terminal belongs to a delegated run. Some persisted
/// terminal records predate the owner column, but the delegated Bash result
/// still retains the terminal ID, so ownership can be recovered from the run
/// activity when necessary.
#[must_use]
pub fn terminal_belongs_to_agent_run(
    terminal: &crate::TerminalSnapshot,
    run: &crate::AgentRun,
) -> bool {
    terminal.owner_agent_run_id == Some(run.id)
        || run
            .timeline
            .iter()
            .filter_map(|entry| match entry {
                crate::AgentRunTimelineEntry::Tool { activity } => Some(&activity.output),
                crate::AgentRunTimelineEntry::Assistant { .. } => None,
            })
            .chain(run.activity.iter().map(|activity| &activity.output))
            .filter_map(|output| {
                output
                    .get("terminal_id")
                    .or_else(|| output.get("id"))
                    .and_then(serde_json::Value::as_str)
                    .and_then(|id| id.parse::<crate::TerminalId>().ok())
            })
            .any(|id| id == terminal.id)
}

#[must_use]
pub fn terminal_belongs_to_any_agent(
    terminal: &crate::TerminalSnapshot,
    runs: &[crate::AgentRun],
) -> bool {
    terminal.owner_agent_run_id.is_some()
        || runs
            .iter()
            .any(|run| terminal_belongs_to_agent_run(terminal, run))
}

/// Inputs used to build a frontend-neutral supervised-work list.
#[derive(Clone, Debug, Default)]
pub struct SupervisedWorkSources {
    pub agents: Vec<crate::AgentRun>,
    pub terminals: Vec<crate::TerminalSnapshot>,
}

#[must_use]
pub fn project_supervised_work(sources: SupervisedWorkSources) -> Vec<SupervisedWork> {
    let mut work = sources
        .agents
        .into_iter()
        .map(|run| SupervisedWork::Agent { run: Box::new(run) })
        .chain(
            sources
                .terminals
                .into_iter()
                .map(|terminal| SupervisedWork::Terminal {
                    terminal: Box::new(terminal),
                }),
        )
        .collect::<Vec<_>>();
    sort_supervised_work_newest_first(&mut work);
    work
}

/// Keeps the frontend-neutral work projection in reverse creation order.
pub fn sort_supervised_work_newest_first(work: &mut [SupervisedWork]) {
    work.sort_by(|left, right| {
        let left_created = match left {
            SupervisedWork::Agent { run } => &run.created_at,
            SupervisedWork::Terminal { terminal } => &terminal.created_at,
        };
        let right_created = match right {
            SupervisedWork::Agent { run } => &run.created_at,
            SupervisedWork::Terminal { terminal } => &terminal.created_at,
        };
        right_created.cmp(left_created)
    });
}

#[derive(Clone, Debug, PartialEq)]
pub struct AgentRunLog {
    pub entries: Vec<AgentRunLogEntry>,
    pub error: Option<String>,
    pub usage: Option<crate::SessionUsage>,
}

/// A color-neutral chronological item in a delegated-run detail view.
#[derive(Clone, Debug, PartialEq)]
pub enum AgentRunLogEntry {
    Assistant(crate::MarkdownDocument),
    ToolGroups(Vec<crate::ToolActivityGroup>),
}

fn append_agent_run_tool_entries(
    entries: &mut Vec<AgentRunLogEntry>,
    tools: &mut Vec<&crate::AgentRunActivity>,
    workspace: &std::path::Path,
    terminals: Option<&HashMap<crate::TerminalId, &crate::TerminalSnapshot>>,
    used_terminals: &mut HashSet<crate::TerminalId>,
) {
    if tools.is_empty() {
        return;
    }

    let append_projected = |entries: &mut Vec<AgentRunLogEntry>,
                            projected: Vec<crate::ToolActivityGroup>| {
        let joins_previous_exploration = projected
            .iter()
            .all(|group| matches!(group, crate::ToolActivityGroup::Exploration { .. }))
            && entries.last().is_some_and(|entry| {
                matches!(
                    entry,
                    AgentRunLogEntry::ToolGroups(groups)
                        if groups.last().is_some_and(|group| matches!(group, crate::ToolActivityGroup::Exploration { .. }))
                )
            });
        if joins_previous_exploration {
            let Some(AgentRunLogEntry::ToolGroups(groups)) = entries.last_mut() else {
                unreachable!("the preceding entry was just checked as tool groups");
            };
            crate::append_tool_activity_groups(groups, projected);
        } else {
            entries.push(AgentRunLogEntry::ToolGroups(projected));
        }
    };
    let project = |activities: Vec<&crate::AgentRunActivity>| {
        crate::project_tool_activities(
            activities
                .into_iter()
                .map(|activity| crate::ToolActivityRef {
                    node_id: None,
                    name: &activity.tool,
                    arguments: &activity.arguments,
                    result: Some(&activity.output),
                    status: if activity.is_error {
                        crate::ToolActivityStatus::Failed
                    } else if crate::detached_bash_result_is_running(
                        &activity.tool,
                        &activity.arguments,
                        &activity.output,
                    ) {
                        crate::ToolActivityStatus::Pending
                    } else {
                        crate::ToolActivityStatus::Succeeded
                    },
                }),
            workspace,
        )
    };

    let Some(terminals) = terminals else {
        append_projected(entries, project(std::mem::take(tools)));
        return;
    };

    let mut pending = Vec::new();
    for activity in tools.drain(..) {
        let terminal = (activity.tool == "bash")
            .then(|| {
                activity
                    .output
                    .get("terminal_id")
                    .or_else(|| activity.output.get("id"))
                    .and_then(serde_json::Value::as_str)
                    .and_then(|id| id.parse::<crate::TerminalId>().ok())
            })
            .flatten()
            .and_then(|id| terminals.get(&id).copied());
        let Some(terminal) = terminal else {
            pending.push(activity);
            continue;
        };

        if !pending.is_empty() {
            append_projected(entries, project(std::mem::take(&mut pending)));
        }
        used_terminals.insert(terminal.id);
        let projected = project(vec![activity]);
        if projected
            .iter()
            .all(|group| matches!(group, crate::ToolActivityGroup::Exploration { .. }))
        {
            append_projected(entries, projected);
        } else {
            append_projected(
                entries,
                vec![crate::ToolActivityGroup::Bash {
                    node_id: None,
                    terminal_id: Some(terminal.id),
                    command: terminal.command.clone(),
                    status: terminal_activity_status(terminal),
                    output: Some(terminal.output.clone()),
                    ansi_output: Some(if terminal.ansi_output.is_empty() {
                        terminal.output.clone()
                    } else {
                        terminal.ansi_output.clone()
                    }),
                    exit_code: terminal.exit_code,
                }],
            );
        }
    }
    if !pending.is_empty() {
        append_projected(entries, project(pending));
    }
}

/// Reconstructs the chronological, transcript-style detail available for a
/// delegated run. Exploration calls are grouped by the same projection used by
/// the primary agent transcript.
#[must_use]
pub fn project_agent_run_log(run: &crate::AgentRun, workspace: &std::path::Path) -> AgentRunLog {
    project_agent_run_log_inner(run, workspace, None, &mut HashSet::new())
}

fn project_agent_run_log_inner(
    run: &crate::AgentRun,
    workspace: &std::path::Path,
    terminals: Option<&HashMap<crate::TerminalId, &crate::TerminalSnapshot>>,
    used_terminals: &mut HashSet<crate::TerminalId>,
) -> AgentRunLog {
    let mut entries = Vec::new();
    let mut tools = Vec::new();
    for entry in &run.timeline {
        match entry {
            crate::AgentRunTimelineEntry::Assistant { text, .. } => {
                append_agent_run_tool_entries(
                    &mut entries,
                    &mut tools,
                    workspace,
                    terminals,
                    used_terminals,
                );
                if !text.is_empty() {
                    entries.push(AgentRunLogEntry::Assistant(crate::parse_markdown(text)));
                }
            }
            crate::AgentRunTimelineEntry::Tool { activity } => tools.push(activity),
        }
    }
    append_agent_run_tool_entries(
        &mut entries,
        &mut tools,
        workspace,
        terminals,
        used_terminals,
    );
    if entries.is_empty() && !run.activity.is_empty() {
        entries.push(AgentRunLogEntry::ToolGroups(
            crate::project_tool_activities(
                run.activity.iter().map(|activity| crate::ToolActivityRef {
                    node_id: None,
                    name: &activity.tool,
                    arguments: &activity.arguments,
                    result: Some(&activity.output),
                    status: if activity.is_error {
                        crate::ToolActivityStatus::Failed
                    } else if crate::detached_bash_result_is_running(
                        &activity.tool,
                        &activity.arguments,
                        &activity.output,
                    ) {
                        crate::ToolActivityStatus::Pending
                    } else {
                        crate::ToolActivityStatus::Succeeded
                    },
                }),
                workspace,
            ),
        ));
        if let Some(result) = &run.result {
            entries.push(AgentRunLogEntry::Assistant(crate::parse_markdown(result)));
        }
    }
    if !run.status.is_terminal()
        && let Some(AgentRunLogEntry::ToolGroups(groups)) = entries.last_mut()
        && groups
            .iter()
            .all(|group| matches!(group, crate::ToolActivityGroup::Exploration { .. }))
    {
        for group in groups {
            let crate::ToolActivityGroup::Exploration { active, .. } = group else {
                unreachable!("all trailing groups were checked as exploration");
            };
            *active = true;
        }
    }
    AgentRunLog {
        entries,
        error: run.error.clone(),
        usage: run.usage.as_ref().map(|usage| {
            let mut summary = crate::SessionUsage::default();
            summary.add(usage);
            summary
        }),
    }
}

/// Reconstructs a delegated log and appends supervised terminals that have
/// not yet become durable run activities. The log remains text-backed; a
/// terminal ID on the projected Bash group lets a frontend open its retained
/// transcript through the terminal surface.
#[must_use]
pub fn project_agent_run_log_with_terminals(
    run: &crate::AgentRun,
    workspace: &std::path::Path,
    terminals: &[crate::TerminalSnapshot],
) -> AgentRunLog {
    let terminal_by_id = terminals
        .iter()
        .filter(|terminal| terminal_belongs_to_agent_run(terminal, run))
        .map(|terminal| (terminal.id, terminal))
        .collect::<HashMap<_, _>>();
    let mut used_terminals = HashSet::new();
    let mut log =
        project_agent_run_log_inner(run, workspace, Some(&terminal_by_id), &mut used_terminals);
    let mut live_groups = Vec::new();
    for terminal in terminals
        .iter()
        .filter(|terminal| terminal_belongs_to_agent_run(terminal, run))
    {
        if used_terminals.contains(&terminal.id) {
            continue;
        }
        // A completed delegated Bash activity may already be present in the
        // durable run timeline. Its retained terminal snapshot is fresher
        // than the activity envelope, so merge the live output into that
        // existing group instead of skipping it or rendering a duplicate.
        let mut updated_existing = false;
        for entry in &mut log.entries {
            let AgentRunLogEntry::ToolGroups(groups) = entry else {
                continue;
            };
            for group in groups {
                let crate::ToolActivityGroup::Bash {
                    terminal_id: Some(group_terminal_id),
                    status,
                    output,
                    ansi_output,
                    exit_code,
                    ..
                } = group
                else {
                    continue;
                };
                if *group_terminal_id != terminal.id {
                    continue;
                }
                *status = terminal_activity_status(terminal);
                *output = Some(terminal.output.clone());
                *ansi_output = Some(if terminal.ansi_output.is_empty() {
                    terminal.output.clone()
                } else {
                    terminal.ansi_output.clone()
                });
                *exit_code = terminal.exit_code;
                updated_existing = true;
                break;
            }
            if updated_existing {
                break;
            }
        }
        if updated_existing {
            continue;
        }

        if terminal.read_safe == Some(true) {
            let arguments = serde_json::json!({
                "command": terminal.command,
                "wait": true,
            });
            let result = crate::terminal_activity_result(terminal);
            let mut projected = crate::project_tool_activities(
                [crate::ToolActivityRef {
                    node_id: terminal.tool_call_node_id,
                    name: "bash",
                    arguments: &arguments,
                    result: Some(&result),
                    status: terminal_activity_status(terminal),
                }],
                workspace,
            );
            if projected
                .iter()
                .all(|group| matches!(group, crate::ToolActivityGroup::Exploration { .. }))
            {
                if !run.status.is_terminal() {
                    for group in &mut projected {
                        let crate::ToolActivityGroup::Exploration { active, .. } = group else {
                            unreachable!("all projected groups were checked as exploration");
                        };
                        *active = true;
                    }
                }
                crate::append_tool_activity_groups(&mut live_groups, projected);
                continue;
            }
        }
        live_groups.push(crate::ToolActivityGroup::Bash {
            node_id: terminal.tool_call_node_id,
            terminal_id: Some(terminal.id),
            command: terminal.command.clone(),
            status: terminal_activity_status(terminal),
            output: Some(terminal.output.clone()),
            ansi_output: Some(if terminal.ansi_output.is_empty() {
                terminal.output.clone()
            } else {
                terminal.ansi_output.clone()
            }),
            exit_code: terminal.exit_code,
        });
    }
    if !live_groups.is_empty() {
        if let Some(AgentRunLogEntry::ToolGroups(groups)) = log.entries.last_mut() {
            crate::append_tool_activity_groups(groups, live_groups);
        } else {
            log.entries.push(AgentRunLogEntry::ToolGroups(live_groups));
        }
    }
    log
}

/// Formats the elapsed wall-clock time for a delegated run.
///
/// Running agents use the current time; completed agents retain their final
/// duration when their log is reopened.
#[must_use]
pub fn agent_run_elapsed(run: &crate::AgentRun) -> Option<String> {
    timestamp_elapsed(run.started_at.as_deref()?, run.completed_at.as_deref())
}

/// Formats elapsed wall-clock time between Unix-millisecond timestamps.
/// A missing end timestamp uses the current time for live work.
#[must_use]
pub fn timestamp_elapsed(started_at: &str, completed_at: Option<&str>) -> Option<String> {
    let started = started_at.parse::<u128>().ok()?;
    let ended = completed_at
        .and_then(|value| value.parse::<u128>().ok())
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        });
    let seconds = ended.saturating_sub(started) / 1_000;
    Some(crate::presentation::format_elapsed(
        u64::try_from(seconds).unwrap_or(u64::MAX),
    ))
}

#[must_use]
pub fn agent_run_usage_line(usage: &crate::SessionUsage) -> Option<String> {
    let mut values = Vec::new();
    if usage.input_tokens.is_some() || usage.output_tokens.is_some() {
        values.push(format!(
            "in:{} out:{}",
            usage
                .input_tokens
                .map(crate::presentation::format_compact_tokens)
                .unwrap_or_else(|| "—".into()),
            usage
                .output_tokens
                .map(crate::presentation::format_compact_tokens)
                .unwrap_or_else(|| "—".into()),
        ));
    }
    if let Some(cost) = crate::presentation::format_session_cost(usage) {
        values.push(format!("Cost  {cost}"));
    }
    (!values.is_empty()).then(|| format!("Usage {}", values.join(" · ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_safe_terminals_become_tasks_only_after_one_second() {
        let mut terminal = crate::TerminalSnapshot {
            id: crate::TerminalId::new(),
            owner: crate::ConversationId::new(),
            owner_agent_run_id: None,
            tool_call_node_id: None,
            read_safe: Some(true),
            command: "cat README.md".into(),
            status: crate::TerminalStatus::Exited,
            created_at: "1000".into(),
            started_at: "1000".into(),
            completed_at: Some("1999".into()),
            exit_code: Some(0),
            output_base: 0,
            output_cursor: 0,
            output_bytes: 0,
            discarded_bytes: 0,
            truncated: false,
            ansi_output: String::new(),
            output: String::new(),
        };
        assert!(!terminal_visible_as_task(&terminal));
        terminal.completed_at = Some("2000".into());
        assert!(terminal_visible_as_task(&terminal));
        terminal.read_safe = Some(false);
        terminal.completed_at = Some("1001".into());
        assert!(terminal_visible_as_task(&terminal));
        terminal.read_safe = None;
        assert!(terminal_visible_as_task(&terminal));
    }

    #[test]
    fn usage_line_has_one_space_after_label() {
        let mut usage = crate::SessionUsage::default();
        usage.input_tokens = Some(1_000);
        usage.output_tokens = Some(250);

        assert_eq!(
            agent_run_usage_line(&usage).as_deref(),
            Some("Usage in:1k out:250")
        );
    }

    #[test]
    fn usage_line_places_cost_after_tokens() {
        let mut usage = crate::SessionUsage::default();
        usage.add(&crate::ModelUsage {
            input_tokens: Some(1_000),
            output_tokens: Some(250),
            cost: Some(crate::ModelCost {
                total_cost: Some("0.012".into()),
                currency: "USD".into(),
                pricing_source: "provider_reported".into(),
                pricing_version: "response".into(),
                ..crate::ModelCost::default()
            }),
            ..crate::ModelUsage::default()
        });

        assert_eq!(
            agent_run_usage_line(&usage).as_deref(),
            Some("Usage in:1k out:250 · Cost  $0.012")
        );
    }

    #[test]
    fn agent_run_elapsed_uses_started_and_completed_timestamps() {
        let run = crate::AgentRun {
            id: crate::AgentRunId::new(),
            conversation_id: crate::ConversationId::new(),
            parent_turn_id: crate::TurnId::new(),
            sequence: 0,
            profile: "explore".into(),
            model: crate::ModelRef::parse("mock/echo").unwrap(),
            effort: None,
            task: "inspect".into(),
            status: crate::AgentRunStatus::Completed,
            result: None,
            error: None,
            usage: None,
            created_at: "500".into(),
            started_at: Some("1000".into()),
            completed_at: Some("62000".into()),
            timeline: Vec::new(),
            activity: Vec::new(),
        };

        assert_eq!(agent_run_elapsed(&run).as_deref(), Some("1m 01s"));
    }

    #[test]
    fn combines_work_newest_first_and_counts_only_active_rows() {
        let conversation_id = crate::ConversationId::new();
        let run = crate::AgentRun {
            id: crate::AgentRunId::new(),
            conversation_id,
            parent_turn_id: crate::TurnId::new(),
            sequence: 0,
            profile: "explore".into(),
            model: crate::ModelRef::parse("mock/echo").unwrap(),
            effort: None,
            task: "inspect".into(),
            status: crate::AgentRunStatus::Running,
            result: None,
            error: None,
            usage: None,
            created_at: "2".into(),
            started_at: Some("2".into()),
            completed_at: None,
            timeline: Vec::new(),
            activity: Vec::new(),
        };
        let terminal = crate::TerminalSnapshot {
            id: crate::TerminalId::new(),
            owner: conversation_id,
            owner_agent_run_id: None,
            tool_call_node_id: Some(crate::NodeId::new()),
            read_safe: None,
            command: "test".into(),
            status: crate::TerminalStatus::Exited,
            created_at: "1".into(),
            started_at: "1".into(),
            completed_at: Some("3".into()),
            exit_code: Some(0),
            output_base: 0,
            output_cursor: 0,
            output_bytes: 0,
            discarded_bytes: 0,
            truncated: false,
            ansi_output: String::new(),
            output: String::new(),
        };
        let work = project_supervised_work(SupervisedWorkSources {
            agents: vec![run],
            terminals: vec![terminal],
        });
        assert!(matches!(work[0], SupervisedWork::Agent { .. }));
        assert!(work[0].active());
        assert!(matches!(work[1], SupervisedWork::Terminal { .. }));
        assert!(!work[1].active());
    }

    #[test]
    fn delegated_log_preserves_assistant_and_tool_order() {
        let activity = crate::AgentRunActivity {
            sequence: 2,
            tool: "read".into(),
            arguments: serde_json::json!({"path": "src/lib.rs"}),
            output: serde_json::json!({"content": "ok"}),
            is_error: false,
            permission_audit: None,
            created_at: "2".into(),
        };
        let run = crate::AgentRun {
            id: crate::AgentRunId::new(),
            conversation_id: crate::ConversationId::new(),
            parent_turn_id: crate::TurnId::new(),
            sequence: 0,
            profile: "explore".into(),
            model: crate::ModelRef::parse("mock/echo").unwrap(),
            effort: None,
            task: "inspect".into(),
            status: crate::AgentRunStatus::Completed,
            result: Some("final".into()),
            error: None,
            usage: None,
            created_at: "0".into(),
            started_at: Some("0".into()),
            completed_at: Some("3".into()),
            timeline: vec![
                crate::AgentRunTimelineEntry::Assistant {
                    sequence: 1,
                    text: "Looking.".into(),
                    created_at: "1".into(),
                },
                crate::AgentRunTimelineEntry::Tool { activity },
                crate::AgentRunTimelineEntry::Assistant {
                    sequence: 3,
                    text: "Done.".into(),
                    created_at: "3".into(),
                },
            ],
            activity: Vec::new(),
        };
        let log = project_agent_run_log(&run, std::path::Path::new("/workspace"));
        assert!(matches!(log.entries[0], AgentRunLogEntry::Assistant(_)));
        assert!(matches!(log.entries[1], AgentRunLogEntry::ToolGroups(_)));
        assert!(matches!(log.entries[2], AgentRunLogEntry::Assistant(_)));
    }

    #[test]
    fn delegated_log_merges_adjacent_bash_exploration_and_keeps_it_active() {
        let activity = |sequence, command: &str| crate::AgentRunActivity {
            sequence,
            tool: "bash".into(),
            arguments: serde_json::json!({"command": command, "wait": true}),
            output: serde_json::json!({"output": ""}),
            is_error: false,
            permission_audit: None,
            created_at: sequence.to_string(),
        };
        let run = crate::AgentRun {
            id: crate::AgentRunId::new(),
            conversation_id: crate::ConversationId::new(),
            parent_turn_id: crate::TurnId::new(),
            sequence: 0,
            profile: "explore".into(),
            model: crate::ModelRef::parse("mock/echo").unwrap(),
            effort: None,
            task: "inspect".into(),
            status: crate::AgentRunStatus::Running,
            result: None,
            error: None,
            usage: None,
            created_at: "0".into(),
            started_at: Some("0".into()),
            completed_at: None,
            timeline: vec![
                crate::AgentRunTimelineEntry::Tool {
                    activity: activity(1, "cat TEST.md"),
                },
                crate::AgentRunTimelineEntry::Assistant {
                    sequence: 2,
                    text: String::new(),
                    created_at: "2".into(),
                },
                crate::AgentRunTimelineEntry::Tool {
                    activity: activity(3, "ls"),
                },
            ],
            activity: Vec::new(),
        };

        let log = project_agent_run_log(&run, std::path::Path::new("/workspace"));
        assert!(matches!(
            log.entries.as_slice(),
            [AgentRunLogEntry::ToolGroups(groups)]
                if matches!(
                    groups.as_slice(),
                    [crate::ToolActivityGroup::Exploration { active: true, activities }]
                        if matches!(
                            activities.as_slice(),
                            [
                                crate::ExplorationActivity { kind: crate::ExplorationActivityKind::Read, .. },
                                crate::ExplorationActivity { kind: crate::ExplorationActivityKind::List, targets, .. },
                            ] if targets == &["."]
                        )
                )
        ));
    }

    #[test]
    fn delegated_log_projects_live_terminal_with_its_retained_id() {
        let mut run = crate::AgentRun {
            id: crate::AgentRunId::new(),
            conversation_id: crate::ConversationId::new(),
            parent_turn_id: crate::TurnId::new(),
            sequence: 0,
            profile: "explore".into(),
            model: crate::ModelRef::parse("mock/echo").unwrap(),
            effort: None,
            task: "run a command".into(),
            status: crate::AgentRunStatus::Running,
            result: None,
            error: None,
            usage: None,
            created_at: "0".into(),
            started_at: Some("0".into()),
            completed_at: None,
            timeline: vec![crate::AgentRunTimelineEntry::Tool {
                activity: crate::AgentRunActivity {
                    sequence: 1,
                    tool: "bash".into(),
                    arguments: serde_json::json!({
                        "command": "echo live",
                        "wait": false,
                    }),
                    output: serde_json::json!({
                        "id": "placeholder",
                        "output": "stale\n",
                        "status": "running",
                    }),
                    is_error: false,
                    permission_audit: None,
                    created_at: "1".into(),
                },
            }],
            activity: Vec::new(),
        };
        let terminal = crate::TerminalSnapshot {
            id: crate::TerminalId::new(),
            owner: run.conversation_id,
            owner_agent_run_id: None,
            tool_call_node_id: Some(crate::NodeId::new()),
            read_safe: None,
            command: "echo live".into(),
            status: crate::TerminalStatus::Running,
            created_at: "1".into(),
            started_at: "1".into(),
            completed_at: None,
            exit_code: None,
            output_base: 0,
            output_cursor: 5,
            output_bytes: 5,
            discarded_bytes: 0,
            truncated: false,
            ansi_output: "live\n".into(),
            output: "live\n".into(),
        };
        if let crate::AgentRunTimelineEntry::Tool { activity } = &mut run.timeline[0] {
            activity.output = serde_json::json!({
                "id": terminal.id,
                "output": "stale\n",
                "status": "running",
            });
        }

        let log = project_agent_run_log_with_terminals(
            &run,
            std::path::Path::new("/workspace"),
            std::slice::from_ref(&terminal),
        );
        assert!(matches!(
            log.entries.as_slice(),
            [AgentRunLogEntry::ToolGroups(groups)]
                if matches!(groups.as_slice(), [crate::ToolActivityGroup::Bash {
                    terminal_id: Some(id), command, output, ..
                }] if *id == terminal.id
                    && command == "echo live"
                    && output.as_deref() == Some("live\n"))
        ));
    }

    #[test]
    fn delegated_read_safe_bash_is_exploration_at_its_timeline_position() {
        let run_id = crate::AgentRunId::new();
        let conversation_id = crate::ConversationId::new();
        let terminal_id = crate::TerminalId::new();
        let run = crate::AgentRun {
            id: run_id,
            conversation_id,
            parent_turn_id: crate::TurnId::new(),
            sequence: 0,
            profile: "explore".into(),
            model: crate::ModelRef::parse("mock/echo").unwrap(),
            effort: None,
            task: "inspect".into(),
            status: crate::AgentRunStatus::Completed,
            result: Some("Done.".into()),
            error: None,
            usage: None,
            created_at: "0".into(),
            started_at: Some("0".into()),
            completed_at: Some("4".into()),
            timeline: vec![
                crate::AgentRunTimelineEntry::Assistant {
                    sequence: 1,
                    text: "Checking the file.".into(),
                    created_at: "1".into(),
                },
                crate::AgentRunTimelineEntry::Tool {
                    activity: crate::AgentRunActivity {
                        sequence: 2,
                        tool: "bash".into(),
                        arguments: serde_json::json!({"command": "cat TEST.md"}),
                        output: serde_json::json!({"id": terminal_id}),
                        is_error: false,
                        permission_audit: None,
                        created_at: "2".into(),
                    },
                },
                crate::AgentRunTimelineEntry::Assistant {
                    sequence: 3,
                    text: "File checked.".into(),
                    created_at: "3".into(),
                },
            ],
            activity: Vec::new(),
        };
        let terminal = crate::TerminalSnapshot {
            id: terminal_id,
            owner: conversation_id,
            owner_agent_run_id: Some(run_id),
            tool_call_node_id: None,
            read_safe: Some(true),
            command: "cat TEST.md".into(),
            status: crate::TerminalStatus::Exited,
            created_at: "2".into(),
            started_at: "2".into(),
            completed_at: Some("2".into()),
            exit_code: Some(0),
            output_base: 0,
            output_cursor: 3,
            output_bytes: 3,
            discarded_bytes: 0,
            truncated: false,
            ansi_output: "ok\n".into(),
            output: "ok\n".into(),
        };

        let log = project_agent_run_log_with_terminals(
            &run,
            std::path::Path::new("/workspace"),
            &[terminal],
        );

        assert!(matches!(
            log.entries.as_slice(),
            [
                AgentRunLogEntry::Assistant(_),
                AgentRunLogEntry::ToolGroups(groups),
                AgentRunLogEntry::Assistant(_),
            ] if matches!(groups.as_slice(), [crate::ToolActivityGroup::Exploration {
                active: false,
                activities,
            }] if matches!(activities.as_slice(), [crate::ExplorationActivity {
                kind: crate::ExplorationActivityKind::Read,
                targets,
                ..
            }] if targets == &["TEST.md"]))
        ));
    }

    #[test]
    fn delegated_live_read_safe_terminal_is_exploration_before_timeline_persistence() {
        let run_id = crate::AgentRunId::new();
        let conversation_id = crate::ConversationId::new();
        let run = crate::AgentRun {
            id: run_id,
            conversation_id,
            parent_turn_id: crate::TurnId::new(),
            sequence: 0,
            profile: "explore".into(),
            model: crate::ModelRef::parse("mock/echo").unwrap(),
            effort: None,
            task: "inspect".into(),
            status: crate::AgentRunStatus::Running,
            result: None,
            error: None,
            usage: None,
            created_at: "0".into(),
            started_at: Some("0".into()),
            completed_at: None,
            timeline: Vec::new(),
            activity: Vec::new(),
        };
        let terminal = crate::TerminalSnapshot {
            id: crate::TerminalId::new(),
            owner: conversation_id,
            owner_agent_run_id: Some(run_id),
            tool_call_node_id: None,
            read_safe: Some(true),
            command: "rg needle src".into(),
            status: crate::TerminalStatus::Running,
            created_at: "1".into(),
            started_at: "1".into(),
            completed_at: None,
            exit_code: None,
            output_base: 0,
            output_cursor: 0,
            output_bytes: 0,
            discarded_bytes: 0,
            truncated: false,
            ansi_output: String::new(),
            output: String::new(),
        };

        let log = project_agent_run_log_with_terminals(
            &run,
            std::path::Path::new("/workspace"),
            &[terminal],
        );

        assert!(matches!(
            log.entries.as_slice(),
            [AgentRunLogEntry::ToolGroups(groups)]
                if matches!(groups.as_slice(), [crate::ToolActivityGroup::Exploration {
                    active: true,
                    activities,
                }] if matches!(activities.as_slice(), [crate::ExplorationActivity {
                    kind: crate::ExplorationActivityKind::Search,
                    targets,
                    scopes,
                }] if targets == &["needle"] && scopes == &["src"]))
        ));
    }

    #[test]
    fn delegated_log_projects_completed_terminal_output_and_ran_heading() {
        let run_id = crate::AgentRunId::new();
        let conversation_id = crate::ConversationId::new();
        let terminal_id = crate::TerminalId::new();
        let run = crate::AgentRun {
            id: run_id,
            conversation_id,
            parent_turn_id: crate::TurnId::new(),
            sequence: 0,
            profile: "explore".into(),
            model: crate::ModelRef::parse("mock/echo").unwrap(),
            effort: None,
            task: "run a command".into(),
            status: crate::AgentRunStatus::Completed,
            result: None,
            error: None,
            usage: None,
            created_at: "0".into(),
            started_at: Some("0".into()),
            completed_at: Some("3".into()),
            timeline: vec![crate::AgentRunTimelineEntry::Tool {
                activity: crate::AgentRunActivity {
                    sequence: 1,
                    tool: "bash".into(),
                    arguments: serde_json::json!({
                        "command": "cargo test",
                        "wait": false,
                    }),
                    output: serde_json::json!({
                        "id": terminal_id,
                        "output": "preview\n",
                        "status": "running",
                    }),
                    is_error: false,
                    permission_audit: None,
                    created_at: "1".into(),
                },
            }],
            activity: Vec::new(),
        };
        let terminal = crate::TerminalSnapshot {
            id: terminal_id,
            owner: conversation_id,
            owner_agent_run_id: Some(run_id),
            tool_call_node_id: None,
            read_safe: None,
            command: "cargo test".into(),
            status: crate::TerminalStatus::Exited,
            created_at: "1".into(),
            started_at: "1".into(),
            completed_at: Some("3".into()),
            exit_code: Some(0),
            output_base: 0,
            output_cursor: 42,
            output_bytes: 42,
            discarded_bytes: 0,
            truncated: false,
            ansi_output: "retained\n".into(),
            output: "retained\n".into(),
        };

        let log = project_agent_run_log_with_terminals(
            &run,
            std::path::Path::new("/workspace"),
            &[terminal],
        );
        assert!(matches!(
            log.entries.as_slice(),
            [AgentRunLogEntry::ToolGroups(groups)]
                if matches!(groups.as_slice(), [crate::ToolActivityGroup::Bash {
                    command, status, output, ..
                }] if command == "cargo test"
                    && *status == crate::ToolActivityStatus::Succeeded
                    && output.as_deref() == Some("retained\n"))
        ));
    }
}
