#![allow(clippy::type_complexity)] // The public projection returns borrowed labels and target slices together.
//! UI-independent projection of tool calls into human-readable activity groups.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::NodeId;

/// The lifecycle state of a tool call as observed by a frontend.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolActivityStatus {
    Pending,
    Succeeded,
    Failed,
}

/// A borrowed tool call used to build a frontend-neutral activity projection.
#[derive(Clone, Copy, Debug)]
pub struct ToolActivityRef<'a> {
    pub node_id: Option<NodeId>,
    pub name: &'a str,
    pub arguments: &'a Value,
    pub result: Option<&'a Value>,
    pub status: ToolActivityStatus,
}

/// An owned tool call tracked while a frontend is presenting live activity.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ToolActivity {
    pub node_id: NodeId,
    pub name: String,
    pub arguments: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    pub status: ToolActivityStatus,
}

impl ToolActivity {
    #[must_use]
    pub fn as_ref(&self) -> ToolActivityRef<'_> {
        ToolActivityRef {
            node_id: Some(self.node_id),
            name: &self.name,
            arguments: &self.arguments,
            result: self.result.as_ref(),
            status: self.status,
        }
    }
}

/// Reduces individual tool events into reusable live presentation batches.
///
/// It owns grouping lifecycle policy, but deliberately knows nothing about a
/// frontend's layout, colors, output medium, or transcript storage.
#[derive(Clone, Debug, Default)]
pub struct ToolActivityTracker {
    activities: Vec<ToolActivity>,
}

impl ToolActivityTracker {
    #[must_use]
    pub fn activities(&self) -> &[ToolActivity] {
        &self.activities
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.activities.is_empty()
    }

    /// Returns whether a call in the open presentation batch is still running.
    #[must_use]
    pub fn has_pending(&self) -> bool {
        self.activities
            .iter()
            .any(|activity| activity.status == ToolActivityStatus::Pending)
    }

    /// Adds a call and returns a completed preceding batch when the new call
    /// starts a separate presentation batch.
    pub fn push(&mut self, activity: ToolActivity) -> Option<Vec<ToolActivity>> {
        let starts_new_batch = !self.activities.is_empty()
            && (exploration_activity_kind_for(&activity.name, &activity.arguments).is_none()
                || self.activities.iter().any(|tool| {
                    exploration_activity_kind_for(&tool.name, &tool.arguments).is_none()
                }));
        let completed = starts_new_batch.then(|| self.take_completed()).flatten();
        self.activities.push(activity);
        completed
    }

    /// Applies a tool result to the matching live call.
    pub fn finish(&mut self, node_id: NodeId, result: Option<Value>, failed: bool) -> bool {
        let Some(activity) = self
            .activities
            .iter_mut()
            .find(|activity| activity.node_id == node_id)
        else {
            return false;
        };
        activity.status = if failed {
            ToolActivityStatus::Failed
        } else {
            ToolActivityStatus::Succeeded
        };
        activity.result = result;
        true
    }

    /// Updates a long-running tool without closing its presentation card.
    pub fn update_running(&mut self, node_id: NodeId, result: Value) -> bool {
        let Some(activity) = self
            .activities
            .iter_mut()
            .find(|activity| activity.node_id == node_id)
        else {
            return false;
        };
        activity.status = ToolActivityStatus::Pending;
        activity.result = Some(result);
        true
    }

    /// Removes a call that should not be presented after authorization fails.
    pub fn remove(&mut self, node_id: NodeId) -> bool {
        let original_len = self.activities.len();
        self.activities
            .retain(|activity| activity.node_id != node_id);
        self.activities.len() != original_len
    }

    /// Takes the current batch once every call in it has finished.
    pub fn take_completed(&mut self) -> Option<Vec<ToolActivity>> {
        (!self.activities.is_empty()
            && self
                .activities
                .iter()
                .all(|activity| activity.status != ToolActivityStatus::Pending))
        .then(|| std::mem::take(&mut self.activities))
    }

    /// Removes matching activities while preserving the relative order of
    /// both the removed activities and the remaining live batch.
    pub fn take_matching(
        &mut self,
        mut predicate: impl FnMut(&ToolActivity) -> bool,
    ) -> Vec<ToolActivity> {
        let activities = std::mem::take(&mut self.activities);
        let (matching, remaining) = activities
            .into_iter()
            .partition(|activity| predicate(activity));
        self.activities = remaining;
        matching
    }

    pub fn clear(&mut self) {
        self.activities.clear();
    }
}

impl From<Vec<ToolActivity>> for ToolActivityTracker {
    fn from(activities: Vec<ToolActivity>) -> Self {
        Self { activities }
    }
}

/// The semantic kind of an activity shown inside an exploration group.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExplorationActivityKind {
    Read,
    List,
    Search,
}

/// One action represented by a collapsed run of tool activity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CollapsedActivityKind {
    Read,
    List,
    Search,
    WebSearch,
    WebFetch,
    Edit,
    Bash,
}

/// Ordered counts for a collapsed run. Kinds retain the order in which they
/// first appeared, independently of later live updates.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CollapsedActivitySummary {
    counts: Vec<(CollapsedActivityKind, usize)>,
}

impl CollapsedActivitySummary {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.counts.is_empty()
    }

    pub fn add(&mut self, kind: CollapsedActivityKind, count: usize) {
        if count == 0 {
            return;
        }
        if let Some((_, existing)) = self
            .counts
            .iter_mut()
            .find(|(candidate, _)| *candidate == kind)
        {
            *existing += count;
        } else {
            self.counts.push((kind, count));
        }
    }

    pub fn merge(&mut self, other: &Self) {
        for (kind, count) in &other.counts {
            self.add(*kind, *count);
        }
    }

    #[must_use]
    pub fn text(&self) -> String {
        self.counts
            .iter()
            .enumerate()
            .map(|(index, (kind, count))| {
                let (upper, lower, singular, plural) = match kind {
                    CollapsedActivityKind::Read => ("Read", "read", "file", "files"),
                    CollapsedActivityKind::List => ("Listed", "listed", "path", "paths"),
                    CollapsedActivityKind::Search => {
                        ("Searched", "searched", "pattern", "patterns")
                    }
                    CollapsedActivityKind::WebSearch => {
                        ("Ran", "ran", "web search", "web searches")
                    }
                    CollapsedActivityKind::WebFetch => {
                        ("Fetched", "fetched", "web page", "web pages")
                    }
                    CollapsedActivityKind::Edit => ("Edited", "edited", "file", "files"),
                    CollapsedActivityKind::Bash => ("Ran", "ran", "command", "commands"),
                };
                format!(
                    "{} {count} {}",
                    if index == 0 { upper } else { lower },
                    if *count == 1 { singular } else { plural }
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Adds one group when it belongs in a collapsed activity run. Returns false
/// for groups that terminate such a run.
pub fn summarize_collapsible_group(
    summary: &mut CollapsedActivitySummary,
    group: &ToolActivityGroup,
) -> bool {
    match group {
        ToolActivityGroup::Exploration { activities, .. } => {
            for activity in activities {
                let kind = match activity.kind {
                    ExplorationActivityKind::Read => CollapsedActivityKind::Read,
                    ExplorationActivityKind::List => CollapsedActivityKind::List,
                    ExplorationActivityKind::Search => CollapsedActivityKind::Search,
                };
                let (_, targets, _) = exploration_activity_parts(activity);
                summary.add(kind, targets.len().max(1));
            }
            true
        }
        ToolActivityGroup::Bash { .. } => {
            summary.add(CollapsedActivityKind::Bash, 1);
            true
        }
        ToolActivityGroup::WebSearch { .. } => {
            summary.add(CollapsedActivityKind::WebSearch, 1);
            true
        }
        ToolActivityGroup::WebFetch { .. } => {
            summary.add(CollapsedActivityKind::WebFetch, 1);
            true
        }
        ToolActivityGroup::Edit { diff, status } => {
            if *status != ToolActivityStatus::Failed {
                summary.add(CollapsedActivityKind::Edit, diff.files.len());
            }
            true
        }
        ToolActivityGroup::Tool {
            label,
            target,
            status,
        } => {
            let Some(file_count) = projected_edit_file_count(label, target.as_deref()) else {
                return *status == ToolActivityStatus::Failed && label == "Apply Patch";
            };
            if *status != ToolActivityStatus::Failed {
                summary.add(CollapsedActivityKind::Edit, file_count);
            }
            true
        }
        _ => false,
    }
}

fn projected_edit_file_count(label: &str, target: Option<&str>) -> Option<usize> {
    if target.is_some() && matches!(label, "Add" | "Edit" | "Delete" | "Move") {
        return Some(1);
    }

    let actions = label.split(" · ").collect::<Vec<_>>();
    (!actions.is_empty()
        && actions.iter().all(|action| {
            ["Add ", "Edit ", "Delete ", "Move "]
                .iter()
                .any(|prefix| action.starts_with(prefix))
        }))
    .then_some(actions.len())
}

pub fn summarize_collapsible_diff(
    summary: &mut CollapsedActivitySummary,
    diff: &crate::tools::SemanticDiff,
) {
    summary.add(CollapsedActivityKind::Edit, diff.files.len());
}

#[cfg(test)]
mod collapsed_summary_tests {
    use super::*;

    #[test]
    fn summary_preserves_first_appearance_and_pluralizes() {
        let mut summary = CollapsedActivitySummary::default();
        summary.add(CollapsedActivityKind::Read, 2);
        summary.add(CollapsedActivityKind::Bash, 1);
        summary.add(CollapsedActivityKind::Search, 1);
        summary.add(CollapsedActivityKind::Read, 1);
        assert_eq!(
            summary.text(),
            "Read 3 files, ran 1 command, searched 1 pattern"
        );
    }

    #[test]
    fn summary_includes_lists_and_edits() {
        let mut summary = CollapsedActivitySummary::default();
        summary.add(CollapsedActivityKind::List, 1);
        summary.add(CollapsedActivityKind::Edit, 2);
        assert_eq!(summary.text(), "Listed 1 path, edited 2 files");
    }

    #[test]
    fn web_search_and_fetch_join_collapsed_runs() {
        let mut summary = CollapsedActivitySummary::default();
        assert!(summarize_collapsible_group(
            &mut summary,
            &ToolActivityGroup::WebSearch {
                provider: "exa".into(),
                query: "rust tui".into(),
                status: ToolActivityStatus::Succeeded,
                result_count: Some(2),
                results: Vec::new(),
            }
        ));
        assert!(summarize_collapsible_group(
            &mut summary,
            &ToolActivityGroup::WebFetch {
                node_id: None,
                url: "https://example.com".into(),
                redirected_url: None,
                format: crate::WebFetchFormat::Markdown,
                content_type: Some("text/html".into()),
                status: ToolActivityStatus::Succeeded,
                output: Some("Example".into()),
            }
        ));
        assert_eq!(summary.text(), "Ran 1 web search, fetched 1 web page");
    }

    #[test]
    fn failed_edits_join_a_run_without_changing_its_count() {
        let mut summary = CollapsedActivitySummary::default();
        summary.add(CollapsedActivityKind::Read, 1);
        assert!(summarize_collapsible_group(
            &mut summary,
            &ToolActivityGroup::Tool {
                label: "Edit src/main.rs · Add src/new.rs".into(),
                target: None,
                status: ToolActivityStatus::Failed,
            }
        ));
        assert_eq!(summary.text(), "Read 1 file");
    }

    #[test]
    fn failed_single_file_and_semantic_edits_join_without_being_counted() {
        let mut summary = CollapsedActivitySummary::default();
        summary.add(CollapsedActivityKind::Bash, 1);
        assert!(summarize_collapsible_group(
            &mut summary,
            &ToolActivityGroup::Tool {
                label: "Edit".into(),
                target: Some("src/main.rs".into()),
                status: ToolActivityStatus::Failed,
            }
        ));
        assert!(summarize_collapsible_group(
            &mut summary,
            &ToolActivityGroup::Edit {
                diff: crate::tools::SemanticDiff { files: Vec::new() },
                status: ToolActivityStatus::Failed,
            }
        ));
        assert_eq!(summary.text(), "Ran 1 command");
    }

    #[test]
    fn projected_pending_and_succeeded_edit_labels_are_counted() {
        for status in [ToolActivityStatus::Pending, ToolActivityStatus::Succeeded] {
            let mut single = CollapsedActivitySummary::default();
            assert!(summarize_collapsible_group(
                &mut single,
                &ToolActivityGroup::Tool {
                    label: "Edit".into(),
                    target: Some("src/main.rs".into()),
                    status,
                }
            ));
            assert_eq!(single.text(), "Edited 1 file");

            let mut multiple = CollapsedActivitySummary::default();
            assert!(summarize_collapsible_group(
                &mut multiple,
                &ToolActivityGroup::Tool {
                    label: "Add src/new.rs · Delete src/old.rs · Move before.rs → after.rs".into(),
                    target: None,
                    status,
                }
            ));
            assert_eq!(multiple.text(), "Edited 3 files");
        }
    }

    #[test]
    fn completed_edit_diff_has_the_same_count_as_its_pending_label() {
        let mut pending = CollapsedActivitySummary::default();
        assert!(summarize_collapsible_group(
            &mut pending,
            &ToolActivityGroup::Tool {
                label: "Edit".into(),
                target: Some("src/main.rs".into()),
                status: ToolActivityStatus::Pending,
            }
        ));

        let mut completed = CollapsedActivitySummary::default();
        assert!(summarize_collapsible_group(
            &mut completed,
            &ToolActivityGroup::Edit {
                diff: crate::tools::SemanticDiff {
                    files: vec![crate::tools::DiffFile {
                        old_path: Some("src/main.rs".into()),
                        new_path: Some("src/main.rs".into()),
                        kind: crate::tools::DiffFileKind::Modified,
                        language: Some("rust".into()),
                        added_lines: 1,
                        removed_lines: 1,
                        old_no_final_newline: false,
                        new_no_final_newline: false,
                        hunks: Vec::new(),
                    }],
                },
                status: ToolActivityStatus::Succeeded,
            }
        ));

        assert_eq!(pending, completed);
        assert_eq!(completed.text(), "Edited 1 file");
    }

    #[test]
    fn failed_projected_edit_labels_join_without_being_counted() {
        let mut summary = CollapsedActivitySummary::default();
        summary.add(CollapsedActivityKind::Read, 1);
        assert!(summarize_collapsible_group(
            &mut summary,
            &ToolActivityGroup::Tool {
                label: "Edit".into(),
                target: Some("src/main.rs".into()),
                status: ToolActivityStatus::Failed,
            }
        ));
        assert!(summarize_collapsible_group(
            &mut summary,
            &ToolActivityGroup::Tool {
                label: "Add src/new.rs · Delete src/old.rs".into(),
                target: None,
                status: ToolActivityStatus::Failed,
            }
        ));
        assert_eq!(summary.text(), "Read 1 file");
    }
}

impl ExplorationActivityKind {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Read => "Read",
            Self::List => "List",
            Self::Search => "Search",
        }
    }
}

/// One display row within an exploration group.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExplorationActivity {
    pub kind: ExplorationActivityKind,
    pub targets: Vec<String>,
    /// Search and list scopes displayed after the targets.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
}

const RICH_LABEL_PREFIX: &str = "\u{1f}label:";
const RICH_RELATION_PREFIX: &str = "\u{1f}relation:";

fn has_rich_exploration_detail(activity: &ExplorationActivity) -> bool {
    activity
        .targets
        .first()
        .is_some_and(|value| value.starts_with(RICH_LABEL_PREFIX))
}

#[must_use]
pub fn exploration_activity_parts(
    activity: &ExplorationActivity,
) -> (&str, &[String], Vec<(&str, &[String])>) {
    let (label, targets) = activity
        .targets
        .first()
        .and_then(|value| {
            value
                .strip_prefix(RICH_LABEL_PREFIX)
                .map(|label| (label, &activity.targets[1..]))
        })
        .unwrap_or((activity.kind.label(), activity.targets.as_slice()));
    let mut relations = Vec::new();
    let mut start = 0;
    while start < activity.scopes.len() {
        let Some(relation) = activity.scopes[start].strip_prefix(RICH_RELATION_PREFIX) else {
            break;
        };
        let end = activity.scopes[start + 1..]
            .iter()
            .position(|value| value.starts_with(RICH_RELATION_PREFIX))
            .map_or(activity.scopes.len(), |offset| start + 1 + offset);
        relations.push((relation, &activity.scopes[start + 1..end]));
        start = end;
    }
    (label, targets, relations)
}

/// An external tool call, retaining its input and result for explicit inspection.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct McpCall {
    #[serde(default)]
    pub node_id: Option<NodeId>,
    pub server: String,
    pub tool: String,
    pub status: ToolActivityStatus,
    pub duration_millis: Option<u64>,
    #[serde(default)]
    pub parameters: Value,
    #[serde(default)]
    pub output: Option<Value>,
}

impl McpCall {
    /// Prefer structured output; unwrap a single text block (including JSON
    /// encoded as text), but retain mixed/non-text MCP content without loss.
    /// This preparation is deferred until a frontend opens the call.
    pub fn display_output(&self) -> Option<Value> {
        let output = self.output.as_ref()?;
        if let Some(structured) = output.get("structured_content").filter(|v| !v.is_null()) {
            return Some(structured.clone());
        }
        let content = output.get("content").unwrap_or(output);
        if let Some(blocks) = content.as_array()
            && blocks.len() == 1
            && blocks[0].get("type").and_then(Value::as_str) == Some("text")
            && let Some(text) = blocks[0].get("text").and_then(Value::as_str)
        {
            return Some(serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.into())));
        }
        Some(content.clone())
    }
}

/// A color- and layout-neutral activity section suitable for any frontend.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolActivityGroup {
    Exploration {
        /// Whether this group is still open in the live activity stream.
        active: bool,
        activities: Vec<ExplorationActivity>,
    },
    Tool {
        label: String,
        target: Option<String>,
        status: ToolActivityStatus,
    },
    Question {
        transcript: crate::presentation::QuestionTranscript,
    },
    /// A completed file mutation with the structured diff returned by the
    /// mutation tool. Keeping this semantic lets each frontend render the
    /// same edit details in its own output medium.
    Edit {
        diff: crate::tools::SemanticDiff,
        status: ToolActivityStatus,
    },
    Mcp {
        #[serde(flatten)]
        call: McpCall,
    },
    WebSearch {
        provider: String,
        query: String,
        status: ToolActivityStatus,
        result_count: Option<usize>,
        /// Normalized results retain their semantic shape so frontends can
        /// present a detailed view without reparsing a provider payload.
        results: Vec<crate::WebSearchResult>,
    },
    WebFetch {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        url: String,
        /// Final destination, included only when the fetch followed redirects.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        redirected_url: Option<String>,
        format: crate::WebFetchFormat,
        content_type: Option<String>,
        status: ToolActivityStatus,
        output: Option<String>,
    },
    Bash {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        /// The supervised terminal backing this Bash call, when retained by
        /// the runtime. Older records may not contain this presentation
        /// reference and use their bounded result as a legacy fallback.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        terminal_id: Option<crate::TerminalId>,
        command: String,
        status: ToolActivityStatus,
        output: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ansi_output: Option<String>,
        exit_code: Option<i32>,
    },
    TerminalWrite {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        terminal_id: Option<crate::TerminalId>,
        command: String,
        data: String,
        status: ToolActivityStatus,
    },
    TerminalKill {
        command: String,
        status: ToolActivityStatus,
    },
    Delegate {
        action: String,
        profile: String,
        task: Option<String>,
        status: ToolActivityStatus,
    },
}

/// Classifies the built-in read-only tools that share an exploration group.
#[must_use]
pub fn exploration_activity_kind(name: &str) -> Option<ExplorationActivityKind> {
    match name {
        "read" => Some(ExplorationActivityKind::Read),
        "list" => Some(ExplorationActivityKind::List),
        "grep" => Some(ExplorationActivityKind::Search),
        _ => None,
    }
}

/// Classifies a complete tool call for Explore presentation.
///
/// Native read-only names remain recognized for durable transcript replay;
/// new Bash calls are classified from their literal command.
#[must_use]
pub fn exploration_activity_kind_for(
    name: &str,
    arguments: &Value,
) -> Option<ExplorationActivityKind> {
    if name != "bash" {
        return exploration_activity_kind(name);
    }
    if arguments.get("wait").is_some_and(|wait| wait == false)
        || arguments
            .get("env")
            .and_then(Value::as_object)
            .is_some_and(|env| !env.is_empty())
        || arguments
            .get("forward_env")
            .and_then(Value::as_array)
            .is_some_and(|env| !env.is_empty())
    {
        return None;
    }
    let command = string_argument(arguments, "command")?;
    match crate::classify_shell_exploration(&command)?
        .exploration?
        .first()?
        .presentation
    {
        crate::SafeShellPresentation::Read => Some(ExplorationActivityKind::Read),
        crate::SafeShellPresentation::List => Some(ExplorationActivityKind::List),
        crate::SafeShellPresentation::Search => Some(ExplorationActivityKind::Search),
    }
}

/// Returns whether a tool call represents a guarded file mutation.
#[must_use]
pub fn is_mutation_tool(name: &str) -> bool {
    matches!(
        name,
        "apply_patch" | "change_working_directory" | "enter_worktree"
    )
}

/// Produces one compact history label per file action in a mutation call.
#[must_use]
pub fn mutation_activity_summaries(tool: ToolActivityRef<'_>, workspace: &Path) -> Vec<String> {
    match tool.name {
        "apply_patch" => {
            let actions = string_argument(tool.arguments, "patch")
                .map(|patch| apply_patch_actions(&patch, workspace))
                .unwrap_or_default()
                .into_iter()
                .map(|(label, target)| format!("{label} {target}"))
                .collect::<Vec<_>>();
            (!actions.is_empty())
                .then(|| actions.join(" · "))
                .into_iter()
                .collect()
        }
        _ => Vec::new(),
    }
}

/// Groups adjacent exploration calls while leaving other calls independent.
///
/// Returned strings are deliberately unescaped. Each frontend remains responsible
/// for escaping untrusted text for its own output medium.
pub fn project_tool_activities<'a>(
    tools: impl IntoIterator<Item = ToolActivityRef<'a>>,
    workspace: &Path,
) -> Vec<ToolActivityGroup> {
    project_tool_activities_with_agent_runs(tools, workspace, &[])
}

/// Projects tool activity using the current delegated-run lifecycle when it
/// is available. A delegation tool call completes as soon as it creates a
/// run, so its own status cannot describe the child run's ongoing work.
#[must_use]
pub fn project_tool_activities_with_agent_runs<'a>(
    tools: impl IntoIterator<Item = ToolActivityRef<'a>>,
    workspace: &Path,
    agent_runs: &[crate::AgentRun],
) -> Vec<ToolActivityGroup> {
    project_tool_activities_with_context(tools, workspace, agent_runs, &[])
}

/// Projects tool activity using the current delegated-run and terminal state.
///
/// Terminal writes and kills refer to a terminal ID, while people need the
/// original Bash command. The terminal snapshots provide that association.
#[must_use]
pub fn project_tool_activities_with_context<'a>(
    tools: impl IntoIterator<Item = ToolActivityRef<'a>>,
    workspace: &Path,
    agent_runs: &[crate::AgentRun],
    terminals: &[crate::TerminalSnapshot],
) -> Vec<ToolActivityGroup> {
    project_tool_activities_with_state(tools, workspace, false, agent_runs, terminals)
}

/// Appends projected activity groups while joining exploration groups that are
/// adjacent across live-event or persistence boundaries.
///
/// The runtime may commit one completed batch before the next read-only call
/// arrives. That boundary is not meaningful in the transcript, so it must not
/// create a second visual exploration card or an extra spacer row.
pub fn append_tool_activity_groups(
    groups: &mut Vec<ToolActivityGroup>,
    appended: impl IntoIterator<Item = ToolActivityGroup>,
) {
    for group in appended {
        let ToolActivityGroup::Exploration { active, activities } = group else {
            groups.push(group);
            continue;
        };

        let Some(ToolActivityGroup::Exploration {
            active: previous_active,
            activities: previous_activities,
        }) = groups.last_mut()
        else {
            groups.push(ToolActivityGroup::Exploration { active, activities });
            continue;
        };

        *previous_active |= active;
        for activity in activities {
            let can_merge_targets = matches!(
                activity.kind,
                ExplorationActivityKind::Read | ExplorationActivityKind::List
            ) && !has_rich_exploration_detail(&activity);
            if can_merge_targets
                && previous_activities
                    .last()
                    .is_some_and(|previous| previous.kind == activity.kind)
            {
                let previous = previous_activities
                    .last_mut()
                    .expect("the preceding activity was just checked");
                for target in activity.targets {
                    if !previous.targets.contains(&target) {
                        previous.targets.push(target);
                    }
                }
                if activity.kind == ExplorationActivityKind::List {
                    for scope in activity.scopes {
                        if !previous.scopes.contains(&scope) {
                            previous.scopes.push(scope);
                        }
                    }
                }
            } else {
                previous_activities.push(activity);
            }
        }
    }
}

/// Projects tool calls that are still part of an open live activity block.
///
/// A block remains open after its current calls finish; the tracker closes it
/// when a different kind of activity is emitted. This differs from
/// [`project_tool_activities`], which projects calls that have already been
/// committed to history.
pub fn project_live_tool_activities<'a>(
    tools: impl IntoIterator<Item = ToolActivityRef<'a>>,
    workspace: &Path,
) -> Vec<ToolActivityGroup> {
    project_tool_activities_with_state(tools, workspace, true, &[], &[])
}

/// Applies a live or final Bash result to an already projected history group.
pub fn update_projected_bash_activity(
    group: &mut ToolActivityGroup,
    node_id: NodeId,
    result: &Value,
    next_status: ToolActivityStatus,
) -> bool {
    let ToolActivityGroup::Bash {
        node_id: Some(group_node_id),
        terminal_id,
        status,
        output,
        ansi_output,
        exit_code,
        ..
    } = group
    else {
        return false;
    };
    if *group_node_id != node_id {
        return false;
    }
    *status = next_status;
    if let Some(id) = terminal_id_from_result(Some(result)) {
        *terminal_id = Some(id);
    }
    *output = result
        .get("output")
        .and_then(Value::as_str)
        .map(str::to_owned);
    *ansi_output = result
        .get("ansi_output")
        .and_then(Value::as_str)
        .map(str::to_owned);
    *exit_code = result
        .get("exit_code")
        .and_then(Value::as_i64)
        .and_then(|code| i32::try_from(code).ok());
    true
}

/// Returns the presentation state represented by a supervised terminal snapshot.
#[must_use]
pub fn terminal_activity_status(terminal: &crate::TerminalSnapshot) -> ToolActivityStatus {
    if terminal.status.is_active() {
        ToolActivityStatus::Pending
    } else if terminal.exit_code.is_some_and(|code| code != 0)
        || terminal.status != crate::TerminalStatus::Exited
    {
        ToolActivityStatus::Failed
    } else {
        ToolActivityStatus::Succeeded
    }
}

/// Projects an explicit detached Bash terminal as standalone transcript activity.
#[must_use]
pub fn detached_bash_transcript_block(
    terminal: &crate::TerminalSnapshot,
) -> crate::TranscriptBlock {
    let status = terminal_activity_status(terminal);
    crate::TranscriptBlock {
        id: crate::TranscriptBlockId(format!("detached-bash:{}", terminal.id)),
        status: if status == ToolActivityStatus::Pending {
            crate::TranscriptBlockStatus::Streaming
        } else {
            crate::TranscriptBlockStatus::Completed
        },
        kind: crate::TranscriptBlockKind::ToolGroups {
            groups: vec![ToolActivityGroup::Bash {
                node_id: terminal.tool_call_node_id,
                terminal_id: Some(terminal.id),
                command: terminal.command.clone(),
                status,
                output: Some(terminal.output.clone()),
                ansi_output: Some(terminal.ansi_output.clone()),
                exit_code: terminal.exit_code,
            }],
        },
    }
}

/// Converts a supervised terminal snapshot into the Bash result shape used by
/// transcript and history projections.
#[must_use]
pub fn terminal_activity_result(terminal: &crate::TerminalSnapshot) -> Value {
    let ansi_output = if terminal.ansi_output.is_empty() {
        &terminal.output
    } else {
        &terminal.ansi_output
    };
    serde_json::json!({
        "terminal_id": terminal.id,
        "output": terminal.output,
        "ansi_output": ansi_output,
        "exit_code": terminal.exit_code,
        "status": match terminal.status {
            crate::TerminalStatus::Running => "running",
            crate::TerminalStatus::Terminating => "terminating",
            crate::TerminalStatus::Exited => "exited",
            crate::TerminalStatus::Killed => "killed",
            crate::TerminalStatus::TimedOut => "timed_out",
            crate::TerminalStatus::Orphaned => "orphaned",
        },
    })
}

fn terminal_id_from_result(result: Option<&Value>) -> Option<crate::TerminalId> {
    result
        .and_then(|result| result.get("terminal_id").or_else(|| result.get("id")))
        .and_then(Value::as_str)
        .and_then(|id| id.parse().ok())
}

pub(crate) fn detached_bash_result_is_running(
    name: &str,
    arguments: &Value,
    result: &Value,
) -> bool {
    name == "bash"
        && arguments
            .get("wait")
            .and_then(Value::as_bool)
            .is_some_and(|wait| !wait)
        && result.get("status").and_then(Value::as_str) == Some("running")
}

/// Produces the compact one-line label used when a tool activity group is
/// embedded in another history view.
#[must_use]
pub fn tool_activity_group_summary(group: &ToolActivityGroup) -> String {
    match group {
        ToolActivityGroup::Exploration { active, activities } => {
            let details = activities
                .iter()
                .map(|activity| {
                    format!("{} {}", activity.kind.label(), activity.targets.join(", "))
                })
                .collect::<Vec<_>>()
                .join(" · ");
            if details.is_empty() {
                if *active { "Exploring" } else { "Explored" }.into()
            } else {
                format!(
                    "{} · {details}",
                    if *active { "Exploring" } else { "Explored" }
                )
            }
        }
        ToolActivityGroup::Tool {
            label,
            target,
            status: _,
        } => target
            .as_ref()
            .map_or_else(|| label.clone(), |target| format!("{label} {target}")),
        ToolActivityGroup::Question { transcript } => {
            if transcript.cancelled {
                "Question not answered".into()
            } else {
                format!(
                    "Questions {}/{} answered",
                    transcript.answered, transcript.total
                )
            }
        }
        ToolActivityGroup::Edit { diff, .. } => diff
            .files
            .first()
            .and_then(|file| file.new_path.as_ref().or(file.old_path.as_ref()))
            .map_or_else(
                || "Edit".into(),
                |path| format!("Edit {}", display_activity_path(path, Path::new(""))),
            ),
        ToolActivityGroup::Mcp { call } => format!("MCP {} · {}", call.server, call.tool),
        ToolActivityGroup::WebSearch {
            provider, query, ..
        } => format!("Web search {provider} · {query}"),
        ToolActivityGroup::WebFetch { url, .. } => format!("Web Fetch {url}"),
        ToolActivityGroup::Bash {
            command, status, ..
        } => format!(
            "{} {command}",
            if *status == ToolActivityStatus::Pending {
                "Running"
            } else {
                "Ran"
            }
        ),
        ToolActivityGroup::TerminalWrite { command, data, .. } => {
            format!("Terminal Write: {command} · {data}")
        }
        ToolActivityGroup::TerminalKill { command, .. } => format!("Terminal Kill: {command}"),
        ToolActivityGroup::Delegate {
            action,
            profile,
            task,
            ..
        } => task.as_ref().map_or_else(
            || format!("{action} {profile} sub-agent"),
            |task| format!("{action} {profile} sub-agent · {task}"),
        ),
    }
}

/// Summarizes one transcript-style batch for compact history pickers.
#[must_use]
pub fn summarize_tool_activities(
    tools: &[ToolActivity],
    workspace: &Path,
    _active: bool,
) -> String {
    let groups = project_tool_activities(tools.iter().map(ToolActivity::as_ref), workspace);
    groups
        .iter()
        .map(|group| match group {
            ToolActivityGroup::Exploration { activities, .. } => activities
                .iter()
                .map(|activity| {
                    format!("{} {}", activity.kind.label(), activity.targets.join(", "))
                })
                .collect::<Vec<_>>()
                .join(", "),
            ToolActivityGroup::Tool { .. }
            | ToolActivityGroup::Question { .. }
            | ToolActivityGroup::Edit { .. }
            | ToolActivityGroup::Mcp { .. }
            | ToolActivityGroup::WebSearch { .. }
            | ToolActivityGroup::WebFetch { .. }
            | ToolActivityGroup::Bash { .. }
            | ToolActivityGroup::TerminalWrite { .. }
            | ToolActivityGroup::TerminalKill { .. }
            | ToolActivityGroup::Delegate { .. } => tool_activity_group_summary(group),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn project_tool_activities_with_state<'a>(
    tools: impl IntoIterator<Item = ToolActivityRef<'a>>,
    workspace: &Path,
    active: bool,
    agent_runs: &[crate::AgentRun],
    terminals: &[crate::TerminalSnapshot],
) -> Vec<ToolActivityGroup> {
    let tools = tools.into_iter().collect::<Vec<_>>();
    let mut groups = Vec::new();
    let mut index = 0;

    while let Some(tool) = tools.get(index).copied() {
        if is_exploration_tool(tool) {
            let end = index
                + tools[index..]
                    .iter()
                    .take_while(|tool| is_exploration_tool(**tool))
                    .count();
            if let Some(group) = project_exploration_group(&tools[index..end], workspace, active) {
                groups.push(group);
            }
            index = end;
        } else if tool.name.starts_with("mcp__") {
            groups.push(project_mcp(tool));
            index += 1;
        } else if tool.name == "web_search" {
            groups.push(project_web_search(tool));
            index += 1;
        } else if tool.name == "web_fetch" {
            groups.push(project_web_fetch(tool));
            index += 1;
        } else if tool.name == "bash" {
            groups.push(ToolActivityGroup::Bash {
                node_id: tool.node_id,
                terminal_id: terminal_id_from_result(tool.result),
                command: string_argument(tool.arguments, "command").unwrap_or_default(),
                status: tool.status,
                output: tool
                    .result
                    .and_then(|result| result.get("output"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                ansi_output: tool
                    .result
                    .and_then(|result| result.get("ansi_output"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                exit_code: tool
                    .result
                    .and_then(|result| result.get("exit_code"))
                    .and_then(Value::as_i64)
                    .and_then(|code| i32::try_from(code).ok()),
            });
            index += 1;
        } else if tool.name == "terminal_write" {
            groups.push(ToolActivityGroup::TerminalWrite {
                terminal_id: terminal_id_from_arguments(tool.arguments),
                command: terminal_command(tool.arguments, terminals),
                data: string_argument(tool.arguments, "data").unwrap_or_default(),
                status: tool.status,
            });
            index += 1;
        } else if tool.name == "terminal_kill" {
            groups.push(ToolActivityGroup::TerminalKill {
                command: terminal_command(tool.arguments, terminals),
                status: tool.status,
            });
            index += 1;
        } else if tool.name == "delegate_agent" {
            let linked_run = tool
                .result
                .and_then(|result| result.get("id"))
                .and_then(Value::as_str)
                .and_then(|id| id.parse::<crate::AgentRunId>().ok())
                .and_then(|id| agent_runs.iter().find(|run| run.id == id));
            let (action, status) = match (tool.status, linked_run) {
                (ToolActivityStatus::Failed, _) => ("Failed to spawn", ToolActivityStatus::Failed),
                (_, Some(run)) if !run.status.is_terminal() => {
                    ("Running", ToolActivityStatus::Pending)
                }
                (_, Some(_)) => ("Finished", ToolActivityStatus::Succeeded),
                (ToolActivityStatus::Pending, None) => ("Running", ToolActivityStatus::Pending),
                // Preserve the tool-call presentation when no child run was
                // created or cannot be hydrated (for example, an old session).
                (ToolActivityStatus::Succeeded, None) => ("Running", ToolActivityStatus::Succeeded),
            };
            let profile =
                string_argument(tool.arguments, "agent").unwrap_or_else(|| "explore".into());
            groups.push(ToolActivityGroup::Delegate {
                action: action.into(),
                profile,
                task: string_argument(tool.arguments, "task"),
                status,
            });
            index += 1;
        } else if tool.name == "request_user_input" {
            if let Some(result) = tool.result
                && let Some(transcript) = crate::presentation::project_question_transcript(
                    tool.name,
                    tool.arguments,
                    Some(result),
                )
            {
                groups.push(ToolActivityGroup::Question { transcript });
            } else {
                groups.push(ToolActivityGroup::Tool {
                    label: tool_display_name(tool.name).into(),
                    target: None,
                    status: tool.status,
                });
            }
            index += 1;
        } else if matches!(tool.name, "wait_join" | "terminal_output" | "update_plan") {
            // Waiting is reflected by the primary working indicator, and
            // terminal polling is reflected by the existing live Bash card.
            // The latest plan has its own transient transcript-tail surface.
            // Separate tool rows add noise without describing useful work.
            index += 1;
        } else if tool.name == "apply_patch" {
            groups.push(project_apply_patch(tool, workspace));
            index += 1;
        } else {
            groups.push(ToolActivityGroup::Tool {
                label: tool_display_name(tool.name).into(),
                target: string_argument(tool.arguments, "path")
                    .or_else(|| string_argument(tool.arguments, "command")),
                status: tool.status,
            });
            index += 1;
        }
    }

    groups
}

fn terminal_command(arguments: &Value, terminals: &[crate::TerminalSnapshot]) -> String {
    let id = string_argument(arguments, "id");
    terminal_id_from_arguments(arguments)
        .and_then(|id| terminals.iter().find(|terminal| terminal.id == id))
        .map(|terminal| terminal.command.clone())
        .or_else(|| id.map(|id| format!("terminal {id}")))
        .unwrap_or_else(|| "terminal".into())
}

fn terminal_id_from_arguments(arguments: &Value) -> Option<crate::TerminalId> {
    arguments
        .get("id")
        .and_then(Value::as_str)
        .and_then(|id| id.parse().ok())
}

/// Presentable read-safe Bash calls retain the compact Read/List/Search
/// presentation even though supervised execution also retains a terminal.
/// Detached calls are rejected by `exploration_activity_kind_for` through
/// their `wait: false` argument and remain expandable Bash cards.
fn is_exploration_tool(tool: ToolActivityRef<'_>) -> bool {
    exploration_activity_kind_for(tool.name, tool.arguments).is_some()
}

fn project_web_search(tool: ToolActivityRef<'_>) -> ToolActivityGroup {
    let provider = tool
        .result
        .and_then(|result| result.get("provider"))
        .and_then(Value::as_str)
        .unwrap_or("configured")
        .to_owned();
    ToolActivityGroup::WebSearch {
        provider,
        query: string_argument(tool.arguments, "query").unwrap_or_default(),
        status: tool.status,
        result_count: tool.result.and_then(|result| {
            result
                .get("results")
                .and_then(Value::as_array)
                .map(Vec::len)
        }),
        results: tool
            .result
            .and_then(|result| result.get("results"))
            .cloned()
            .and_then(|results| serde_json::from_value(results).ok())
            .unwrap_or_default(),
    }
}

fn project_web_fetch(tool: ToolActivityRef<'_>) -> ToolActivityGroup {
    let format = string_argument(tool.arguments, "format")
        .and_then(|value| serde_json::from_value(serde_json::Value::String(value)).ok())
        .unwrap_or_default();
    ToolActivityGroup::WebFetch {
        node_id: tool.node_id,
        url: string_argument(tool.arguments, "url").unwrap_or_default(),
        redirected_url: tool
            .result
            .and_then(|result| result.get("url"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        format,
        content_type: tool
            .result
            .and_then(|result| result.get("content_type"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        status: tool.status,
        output: tool
            .result
            .and_then(|result| result.get("output"))
            .and_then(Value::as_str)
            .map(str::to_owned),
    }
}

fn project_mcp(tool: ToolActivityRef<'_>) -> ToolActivityGroup {
    let result = tool.result;
    let fallback = tool.name.strip_prefix("mcp__").unwrap_or(tool.name);
    let (fallback_server, fallback_tool) = fallback
        .split_once("__")
        .map_or((fallback, "unknown"), |(server, tool)| (server, tool));
    ToolActivityGroup::Mcp {
        call: McpCall {
            node_id: tool.node_id,
            server: result
                .and_then(|value| value.get("server"))
                .and_then(Value::as_str)
                .unwrap_or(fallback_server)
                .to_owned(),
            tool: result
                .and_then(|value| value.get("tool"))
                .and_then(Value::as_str)
                .unwrap_or(fallback_tool)
                .to_owned(),
            status: tool.status,
            duration_millis: result
                .and_then(|value| value.get("duration_millis"))
                .and_then(Value::as_u64),
            parameters: tool.arguments.clone(),
            output: result.cloned(),
        },
    }
}

fn project_apply_patch(tool: ToolActivityRef<'_>, workspace: &Path) -> ToolActivityGroup {
    if let Some(diff) = tool
        .result
        .and_then(|result| result.get("diff"))
        .and_then(|diff| serde_json::from_value::<crate::tools::SemanticDiff>(diff.clone()).ok())
    {
        return ToolActivityGroup::Edit {
            diff,
            status: tool.status,
        };
    }

    let Some(patch) = string_argument(tool.arguments, "patch") else {
        return ToolActivityGroup::Tool {
            label: "Apply Patch".into(),
            target: None,
            status: tool.status,
        };
    };
    let actions = apply_patch_actions(&patch, workspace);

    match actions.as_slice() {
        [(label, target)] => ToolActivityGroup::Tool {
            label: label.clone(),
            target: Some(target.clone()),
            status: tool.status,
        },
        [] => ToolActivityGroup::Tool {
            label: "Apply Patch".into(),
            target: None,
            status: tool.status,
        },
        _ => ToolActivityGroup::Tool {
            label: actions
                .into_iter()
                .map(|(label, target)| format!("{label} {target}"))
                .collect::<Vec<_>>()
                .join(" · "),
            target: None,
            status: tool.status,
        },
    }
}

fn apply_patch_actions(patch: &str, workspace: &Path) -> Vec<(String, String)> {
    let mut actions = Vec::<(String, String)>::new();
    for line in patch.lines() {
        let action = [
            ("*** Add File: ", "Add"),
            ("*** Delete File: ", "Delete"),
            ("*** Update File: ", "Edit"),
        ]
        .into_iter()
        .find_map(|(prefix, label)| {
            line.strip_prefix(prefix)
                .filter(|path| !path.is_empty())
                .map(|path| {
                    (
                        label.to_owned(),
                        display_activity_path(Path::new(path), workspace),
                    )
                })
        });
        if let Some(action) = action {
            actions.push(action);
        } else if let Some(destination) = line
            .strip_prefix("*** Move to: ")
            .filter(|path| !path.is_empty())
            && let Some((label, source)) = actions.last_mut()
            && label == "Edit"
        {
            *label = "Move".into();
            source.push_str(" → ");
            source.push_str(&display_activity_path(Path::new(destination), workspace));
        }
    }
    actions
}

fn project_exploration_group(
    tools: &[ToolActivityRef<'_>],
    workspace: &Path,
    active: bool,
) -> Option<ToolActivityGroup> {
    let mut activities = Vec::<ExplorationActivity>::new();

    for tool in tools {
        if tool.name == "read" && tool.status == ToolActivityStatus::Failed {
            continue;
        }
        let Some(projected) = exploration_details(*tool, workspace) else {
            continue;
        };
        for activity in projected {
            let group_adjacent = matches!(
                activity.kind,
                ExplorationActivityKind::Read | ExplorationActivityKind::List
            ) && !has_rich_exploration_detail(&activity);
            if group_adjacent
                && activities
                    .last()
                    .is_some_and(|previous| previous.kind == activity.kind)
            {
                let targets = &mut activities
                    .last_mut()
                    .expect("the preceding activity was just checked")
                    .targets;
                for target in activity.targets {
                    if !targets.contains(&target) {
                        targets.push(target);
                    }
                }
                if activity.kind == ExplorationActivityKind::List {
                    let scopes = &mut activities
                        .last_mut()
                        .expect("the preceding activity was just checked")
                        .scopes;
                    for scope in activity.scopes {
                        if !scopes.contains(&scope) {
                            scopes.push(scope);
                        }
                    }
                }
            } else {
                activities.push(activity);
            }
        }
    }

    (!activities.is_empty()).then_some(ToolActivityGroup::Exploration { active, activities })
}

fn exploration_details(
    tool: ToolActivityRef<'_>,
    workspace: &Path,
) -> Option<Vec<ExplorationActivity>> {
    let kind = exploration_activity_kind_for(tool.name, tool.arguments)?;
    if tool.name == "bash" {
        let command = string_argument(tool.arguments, "command")?;
        let classification = crate::classify_shell_exploration(&command)?;
        return classification
            .exploration?
            .into_iter()
            .map(|activity| {
                let kind = match activity.presentation {
                    crate::SafeShellPresentation::Read => ExplorationActivityKind::Read,
                    crate::SafeShellPresentation::List => ExplorationActivityKind::List,
                    crate::SafeShellPresentation::Search => ExplorationActivityKind::Search,
                };
                let paths = activity
                    .paths
                    .into_iter()
                    .map(|path| display_activity_path(Path::new(&path), workspace))
                    .collect::<Vec<_>>();
                if let Some(detail) = activity.detail {
                    let display_value = |value: crate::SafePresentationValue| {
                        if value.path {
                            display_activity_path(Path::new(&value.value), workspace)
                        } else {
                            value.value
                        }
                    };
                    let mut targets = vec![format!("{RICH_LABEL_PREFIX}{}", detail.label)];
                    targets.extend(detail.arguments.into_iter().map(display_value));
                    let mut scopes = Vec::new();
                    for relation in detail.relations {
                        if relation.values.is_empty() {
                            continue;
                        }
                        scopes.push(format!("{RICH_RELATION_PREFIX}{}", relation.label));
                        scopes.extend(relation.values.into_iter().map(display_value));
                    }
                    return ExplorationActivity {
                        kind,
                        targets,
                        scopes,
                    };
                }
                match kind {
                    ExplorationActivityKind::Search => ExplorationActivity {
                        kind,
                        targets: vec![activity.query.unwrap_or(activity.command)],
                        scopes: paths,
                    },
                    ExplorationActivityKind::Read => ExplorationActivity {
                        kind,
                        targets: paths,
                        scopes: Vec::new(),
                    },
                    ExplorationActivityKind::List => {
                        let has_targets = !activity.targets.is_empty();
                        ExplorationActivity {
                            kind,
                            targets: if has_targets {
                                activity.targets
                            } else {
                                paths.clone()
                            },
                            scopes: if has_targets { paths } else { Vec::new() },
                        }
                    }
                }
            })
            .collect::<Vec<_>>()
            .into();
    }
    match kind {
        ExplorationActivityKind::Read | ExplorationActivityKind::List => {
            string_argument(tool.arguments, "path").map(|path| {
                vec![ExplorationActivity {
                    kind,
                    targets: vec![display_activity_path(Path::new(&path), workspace)],
                    scopes: Vec::new(),
                }]
            })
        }
        ExplorationActivityKind::Search => {
            let pattern = string_argument(tool.arguments, "pattern")?;
            let scopes = string_argument_list(tool.arguments, "paths")
                .into_iter()
                .map(|path| display_activity_path(Path::new(&path), workspace))
                .chain(string_argument_list(tool.arguments, "globs"))
                .collect::<Vec<_>>();
            Some(vec![ExplorationActivity {
                kind,
                targets: vec![pattern],
                scopes,
            }])
        }
    }
}

/// Formats a path for activity cards without exposing the user's home prefix.
///
/// Conversation scratchpad paths use `~scratchpad`. Workspace paths stay
/// relative to the workspace. Other paths inside the current user's home
/// directory use `~`; paths elsewhere remain absolute.
#[must_use]
pub fn display_activity_path(path: &Path, workspace: &Path) -> String {
    let home = directories::BaseDirs::new().map(|directories| directories.home_dir().to_path_buf());
    display_activity_path_with_home(path, workspace, home.as_deref())
}

fn display_activity_path_with_home(path: &Path, workspace: &Path, home: Option<&Path>) -> String {
    if let Some(relative) = scratchpad_relative_path(path) {
        if relative.as_os_str().is_empty() {
            return "~scratchpad".into();
        }
        let displayed = PathBuf::from("~scratchpad").join(relative);
        return displayed.to_string_lossy().replace('\\', "/");
    }
    let workspace_relative = (!workspace.as_os_str().is_empty())
        .then(|| path.strip_prefix(workspace).ok())
        .flatten()
        .filter(|relative| !relative.as_os_str().is_empty());
    let displayed = workspace_relative.map_or_else(
        || {
            home.and_then(|home| path.strip_prefix(home).ok())
                .map_or_else(
                    || path.to_path_buf(),
                    |relative| PathBuf::from("~").join(relative),
                )
        },
        Path::to_path_buf,
    );
    if displayed.as_os_str().is_empty() {
        ".".into()
    } else {
        displayed.to_string_lossy().replace('\\', "/")
    }
}

fn scratchpad_relative_path(path: &Path) -> Option<PathBuf> {
    let components = path.components().collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        if component.as_os_str() != "scratchpad" || index < 3 {
            continue;
        }
        let user = components[index - 3].as_os_str().to_string_lossy();
        let project = components[index - 2].as_os_str().to_string_lossy();
        let conversation = components[index - 1].as_os_str().to_string_lossy();
        if !user.starts_with("cagent-")
            || project.len() != 16
            || !project.bytes().all(|byte| byte.is_ascii_hexdigit())
            || conversation.parse::<crate::ConversationId>().is_err()
        {
            continue;
        }
        return Some(
            components[index + 1..]
                .iter()
                .map(|component| component.as_os_str())
                .collect(),
        );
    }
    None
}

fn string_argument(arguments: &Value, key: &str) -> Option<String> {
    arguments.get(key)?.as_str().map(str::to_owned)
}

fn string_argument_list(arguments: &Value, key: &str) -> Vec<String> {
    arguments
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

fn tool_display_name(name: &str) -> &str {
    match name {
        "apply_patch" => "Apply Patch",
        "bash" => "Bash",
        "change_working_directory" => "Change Working Directory",
        "delegate_agent" => "Delegate Agent",
        "enter_worktree" => "Enter Worktree",
        "request_user_input" => "Request User Input",
        "terminal_kill" => "Terminal Kill",
        "terminal_output" => "Terminal Output",
        "terminal_write" => "Terminal Write",
        "update_plan" => "Update Plan",
        "wait_join" => "Wait Join",
        "web_fetch" => "Web Fetch",
        "web_search" => "Web Search",
        // Retained conversations can contain these legacy built-in tools.
        "grep" => "Search",
        "read" => "Read",
        "list" => "List",
        _ => name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn activity<'a>(
        name: &'a str,
        arguments: &'a Value,
        status: ToolActivityStatus,
    ) -> ToolActivityRef<'a> {
        ToolActivityRef {
            node_id: None,
            name,
            arguments,
            result: None,
            status,
        }
    }

    fn delegated_run(id: crate::AgentRunId, status: crate::AgentRunStatus) -> crate::AgentRun {
        crate::AgentRun {
            id,
            conversation_id: crate::ConversationId::new(),
            parent_turn_id: crate::TurnId::new(),
            sequence: 0,
            profile: "code-review".into(),
            model: crate::ModelRef::parse("mock/echo").unwrap(),
            effort: None,
            task: "Inspect the parser".into(),
            status,
            result: None,
            error: None,
            usage: None,
            created_at: "1".into(),
            started_at: None,
            completed_at: None,
            timeline: Vec::new(),
            activity: Vec::new(),
        }
    }

    #[test]
    fn activity_paths_are_workspace_or_home_relative() {
        let workspace = Path::new("/work/project");
        let home = Path::new("/home/alice");

        assert_eq!(
            display_activity_path_with_home(
                Path::new("/work/project/src/lib.rs"),
                workspace,
                Some(home),
            ),
            "src/lib.rs"
        );
        assert_eq!(
            display_activity_path_with_home(
                Path::new("/home/alice/.agents/skills/agent-browser/SKILL.md"),
                workspace,
                Some(home),
            ),
            "~/.agents/skills/agent-browser/SKILL.md"
        );
        assert_eq!(
            display_activity_path_with_home(Path::new("/etc/hosts"), workspace, Some(home)),
            "/etc/hosts"
        );
    }

    #[test]
    fn activity_paths_abbreviate_conversation_scratchpads() {
        let workspace = Path::new("/work/project");
        let scratchpad = PathBuf::from("/tmp/cagent-1000/0123456789abcdef")
            .join(crate::ConversationId::new().to_string())
            .join("scratchpad");

        assert_eq!(
            display_activity_path_with_home(
                &scratchpad.join("results/output.txt"),
                workspace,
                Some(Path::new("/home/alice")),
            ),
            "~scratchpad/results/output.txt"
        );
        assert_eq!(
            display_activity_path_with_home(&scratchpad, workspace, Some(Path::new("/home/alice")),),
            "~scratchpad"
        );
        assert_eq!(
            display_activity_path_with_home(
                Path::new("/tmp/scratchpad/results.txt"),
                workspace,
                Some(Path::new("/home/alice")),
            ),
            "/tmp/scratchpad/results.txt"
        );
    }

    #[test]
    fn read_list_search_and_patch_labels_abbreviate_home_paths() {
        let home = directories::BaseDirs::new()
            .expect("test platform has a home directory")
            .home_dir()
            .to_path_buf();
        let path = home.join(".agents/skills/agent-browser/SKILL.md");
        let path = path.to_string_lossy().into_owned();
        let expected = "~/.agents/skills/agent-browser/SKILL.md";
        let arguments = [
            serde_json::json!({ "path": path }),
            serde_json::json!({ "path": path }),
            serde_json::json!({ "pattern": "description", "paths": [path] }),
            serde_json::json!({ "patch": format!("*** Begin Patch\n*** Update File: {path}\n-old\n+new\n*** End Patch") }),
        ];

        let projected = project_tool_activities(
            [
                activity("read", &arguments[0], ToolActivityStatus::Succeeded),
                activity("list", &arguments[1], ToolActivityStatus::Succeeded),
                activity("grep", &arguments[2], ToolActivityStatus::Succeeded),
                activity("apply_patch", &arguments[3], ToolActivityStatus::Succeeded),
            ],
            Path::new("/workspace"),
        );

        assert!(matches!(
            projected.as_slice(),
            [
                ToolActivityGroup::Exploration { activities, .. },
                ToolActivityGroup::Tool { label, target: Some(target), .. },
            ] if activities.as_slice() == [
                ExplorationActivity {
                    kind: ExplorationActivityKind::Read,
                    targets: vec![expected.into()],
                    scopes: Vec::new(),
                },
                ExplorationActivity {
                    kind: ExplorationActivityKind::List,
                    targets: vec![expected.into()],
                    scopes: Vec::new(),
                },
                ExplorationActivity {
                    kind: ExplorationActivityKind::Search,
                    targets: vec!["description".into()],
                    scopes: vec![expected.into()],
                },
            ] && label == "Edit" && target == expected
        ));
    }

    #[test]
    fn groups_adjacent_reads_and_lists_and_keeps_searches_separate() {
        let arguments = [
            serde_json::json!({ "path": "tui.rs" }),
            serde_json::json!({ "path": "README.md" }),
            serde_json::json!({ "path": "tui.rs" }),
            serde_json::json!({ "path": "missing.rs" }),
            serde_json::json!({ "path": "src" }),
            serde_json::json!({ "pattern": "first", "paths": ["tui.rs"] }),
            serde_json::json!({ "pattern": "second", "globs": ["*.rs"] }),
        ];
        let projected = project_tool_activities(
            [
                activity("read", &arguments[0], ToolActivityStatus::Succeeded),
                activity("read", &arguments[1], ToolActivityStatus::Succeeded),
                activity("read", &arguments[2], ToolActivityStatus::Succeeded),
                activity("read", &arguments[3], ToolActivityStatus::Failed),
                activity("list", &arguments[4], ToolActivityStatus::Succeeded),
                activity("grep", &arguments[5], ToolActivityStatus::Succeeded),
                activity("grep", &arguments[6], ToolActivityStatus::Succeeded),
            ],
            Path::new("/workspace"),
        );

        assert_eq!(
            projected,
            vec![ToolActivityGroup::Exploration {
                active: false,
                activities: vec![
                    ExplorationActivity {
                        kind: ExplorationActivityKind::Read,
                        targets: vec!["tui.rs".into(), "README.md".into()],
                        scopes: Vec::new(),
                    },
                    ExplorationActivity {
                        kind: ExplorationActivityKind::List,
                        targets: vec!["src".into()],
                        scopes: Vec::new(),
                    },
                    ExplorationActivity {
                        kind: ExplorationActivityKind::Search,
                        targets: vec!["first".into()],
                        scopes: vec!["tui.rs".into()],
                    },
                    ExplorationActivity {
                        kind: ExplorationActivityKind::Search,
                        targets: vec!["second".into()],
                        scopes: vec!["*.rs".into()],
                    },
                ],
            }]
        );
    }

    #[test]
    fn projects_presentable_bash_compounds_as_ordered_exploration_rows() {
        let arguments = serde_json::json!({
            "command": "cat TEST.md && ls; cat OTHER.md || ls docs"
        });
        let projected = project_live_tool_activities(
            [activity("bash", &arguments, ToolActivityStatus::Succeeded)],
            Path::new("/workspace"),
        );

        assert_eq!(
            projected,
            vec![ToolActivityGroup::Exploration {
                active: true,
                activities: vec![
                    ExplorationActivity {
                        kind: ExplorationActivityKind::Read,
                        targets: vec!["TEST.md".into()],
                        scopes: Vec::new(),
                    },
                    ExplorationActivity {
                        kind: ExplorationActivityKind::List,
                        targets: vec![".".into()],
                        scopes: Vec::new(),
                    },
                    ExplorationActivity {
                        kind: ExplorationActivityKind::Read,
                        targets: vec!["OTHER.md".into()],
                        scopes: Vec::new(),
                    },
                    ExplorationActivity {
                        kind: ExplorationActivityKind::List,
                        targets: vec!["docs".into()],
                        scopes: Vec::new(),
                    },
                ],
            }]
        );
    }

    #[test]
    fn projects_compound_searches_with_unquoted_path_globs_as_exploration() {
        let arguments = serde_json::json!({
            "command": r"rg -n 'pub\(crate\).*enum TranscriptBlock|enum TranscriptBlock|TranscriptBlock \{' crates/cagent-cli/src/app/mod.rs crates/cagent-cli/src/app/*.rs | head -80; sed -n '20,65p' crates/cagent-cli/src/render/transcript.rs; rg -n 'TranscriptBlock::User \{' crates/cagent-cli/src | cat"
        });
        let projected = project_tool_activities(
            [activity("bash", &arguments, ToolActivityStatus::Succeeded)],
            Path::new("/workspace"),
        );

        assert_eq!(
            projected,
            vec![ToolActivityGroup::Exploration {
                active: false,
                activities: vec![
                    ExplorationActivity {
                        kind: ExplorationActivityKind::Search,
                        targets: vec![
                            r"pub\(crate\).*enum TranscriptBlock|enum TranscriptBlock|TranscriptBlock \{".into(),
                        ],
                        scopes: vec![
                            "crates/cagent-cli/src/app/mod.rs".into(),
                            "crates/cagent-cli/src/app/*.rs".into(),
                        ],
                    },
                    ExplorationActivity {
                        kind: ExplorationActivityKind::Read,
                        targets: vec!["crates/cagent-cli/src/render/transcript.rs".into()],
                        scopes: Vec::new(),
                    },
                    ExplorationActivity {
                        kind: ExplorationActivityKind::Search,
                        targets: vec![r"TranscriptBlock::User \{".into()],
                        scopes: vec!["crates/cagent-cli/src".into()],
                    },
                ],
            }]
        );
    }

    #[test]
    fn keeps_globs_outside_filesystem_operands_as_bash_activity() {
        for command in [
            "rg TranscriptBlock* .",
            "rg --glob *.rs TranscriptBlock .",
            "cat \"$(printf README.md)\"",
        ] {
            let arguments = serde_json::json!({ "command": command });
            assert!(matches!(
                project_tool_activities(
                    [activity("bash", &arguments, ToolActivityStatus::Succeeded)],
                    Path::new("/workspace"),
                )
                .as_slice(),
                [ToolActivityGroup::Bash { .. }]
            ));
        }
    }

    #[test]
    fn projects_searches_with_the_query_and_ignores_safe_true_fallbacks() {
        let arguments = serde_json::json!({
            "command": "rg -n -i 'ctx|context' crates/cagent-agent crates/cagent-cli --glob '*.rs' 2>/dev/null || true"
        });
        let projected = project_tool_activities(
            [activity("bash", &arguments, ToolActivityStatus::Succeeded)],
            Path::new("/workspace"),
        );

        assert_eq!(
            projected,
            vec![ToolActivityGroup::Exploration {
                active: false,
                activities: vec![ExplorationActivity {
                    kind: ExplorationActivityKind::Search,
                    targets: vec!["ctx|context".into()],
                    scopes: vec!["crates/cagent-agent".into(), "crates/cagent-cli".into()],
                }],
            }]
        );
    }

    #[test]
    fn projects_sed_reads_through_null_stderr_suppression_and_true() {
        let arguments = serde_json::json!({
            "command": "sed -n '180,440p' crates/cagent-agent/src/presentation/statusline.rs && sed -n '730,810p' crates/cagent-cli/src/render/tests.rs 2>/dev/null || true && sed -n '760,800p' crates/cagent-cli/src/render/mod.rs"
        });
        let projected = project_tool_activities(
            [activity("bash", &arguments, ToolActivityStatus::Succeeded)],
            Path::new("/workspace"),
        );
        assert!(matches!(
            projected.as_slice(),
            [ToolActivityGroup::Exploration { activities, .. }]
                if activities.len() == 1
                    && activities[0].kind == ExplorationActivityKind::Read
                    && activities[0].targets == [
                        "crates/cagent-agent/src/presentation/statusline.rs",
                        "crates/cagent-cli/src/render/tests.rs",
                        "crates/cagent-cli/src/render/mod.rs",
                    ]
        ));
    }

    #[test]
    fn omits_stdin_only_head_from_file_listing_exploration() {
        let arguments = serde_json::json!({
            "command": "rg --files -g 'SPEC.md' -g 'AGENTS.md' -g 'Cargo.toml' -g '*.rs' | head -n 80"
        });
        let projected = project_tool_activities(
            [activity("bash", &arguments, ToolActivityStatus::Succeeded)],
            Path::new("/workspace"),
        );

        assert_eq!(
            projected,
            vec![ToolActivityGroup::Exploration {
                active: false,
                activities: vec![ExplorationActivity {
                    kind: ExplorationActivityKind::List,
                    targets: vec![
                        "SPEC.md".into(),
                        "AGENTS.md".into(),
                        "Cargo.toml".into(),
                        "*.rs".into(),
                    ],
                    scopes: vec![".".into()],
                }],
            }]
        );
    }

    #[test]
    fn projects_unfiltered_rg_file_listing_as_list_dot() {
        let arguments = serde_json::json!({ "command": "rg --files" });
        let projected = project_tool_activities(
            [activity("bash", &arguments, ToolActivityStatus::Succeeded)],
            Path::new("/workspace"),
        );

        assert_eq!(
            projected,
            vec![ToolActivityGroup::Exploration {
                active: false,
                activities: vec![ExplorationActivity {
                    kind: ExplorationActivityKind::List,
                    targets: vec![".".into()],
                    scopes: Vec::new(),
                }],
            }]
        );
    }

    #[test]
    fn merges_adjacent_list_targets_and_scopes_without_duplicates() {
        let arguments = [
            serde_json::json!({ "command": "rg --files -g '*.rs' src" }),
            serde_json::json!({ "command": "rg --files --glob '*.md' src" }),
            serde_json::json!({ "command": "rg --files -g '*.rs' tests" }),
        ];
        let projected = project_tool_activities(
            arguments
                .iter()
                .map(|arguments| activity("bash", arguments, ToolActivityStatus::Succeeded)),
            Path::new("/workspace"),
        );

        assert_eq!(
            projected,
            vec![ToolActivityGroup::Exploration {
                active: false,
                activities: vec![ExplorationActivity {
                    kind: ExplorationActivityKind::List,
                    targets: vec!["*.rs".into(), "*.md".into()],
                    scopes: vec!["src".into(), "tests".into()],
                }],
            }]
        );
    }

    #[test]
    fn projects_fd_as_a_list_with_pattern_targets_and_scopes() {
        let arguments = serde_json::json!({
            "command": "fd -a 'SPEC.md|Cargo.toml|AGENTS.md' ."
        });
        let projected = project_tool_activities(
            [activity("bash", &arguments, ToolActivityStatus::Succeeded)],
            Path::new("/workspace"),
        );

        assert_eq!(
            projected,
            vec![ToolActivityGroup::Exploration {
                active: false,
                activities: vec![ExplorationActivity {
                    kind: ExplorationActivityKind::List,
                    targets: vec!["SPEC.md|Cargo.toml|AGENTS.md".into()],
                    scopes: vec![".".into()],
                }],
            }]
        );
    }

    #[test]
    fn projects_fd_extension_filters_and_unfiltered_roots_like_rg_files() {
        let extension = serde_json::json!({ "command": "fd -e rs . crates" });
        let extension_projected = project_tool_activities(
            [activity("bash", &extension, ToolActivityStatus::Succeeded)],
            Path::new("/workspace"),
        );
        assert_eq!(
            extension_projected,
            vec![ToolActivityGroup::Exploration {
                active: false,
                activities: vec![ExplorationActivity {
                    kind: ExplorationActivityKind::List,
                    targets: vec!["*.rs".into()],
                    scopes: vec!["crates".into()],
                }],
            }]
        );

        let unfiltered = serde_json::json!({ "command": "fd --hidden -t f ." });
        let unfiltered_projected = project_tool_activities(
            [activity("bash", &unfiltered, ToolActivityStatus::Succeeded)],
            Path::new("/workspace"),
        );
        assert_eq!(
            unfiltered_projected,
            vec![ToolActivityGroup::Exploration {
                active: false,
                activities: vec![ExplorationActivity {
                    kind: ExplorationActivityKind::List,
                    targets: vec![".".into()],
                    scopes: Vec::new(),
                }],
            }]
        );
    }

    #[test]
    fn projects_bare_ls_as_the_current_directory() {
        let arguments = serde_json::json!({ "command": "ls" });
        let projected = project_tool_activities(
            [activity("bash", &arguments, ToolActivityStatus::Succeeded)],
            Path::new("/workspace"),
        );

        assert_eq!(
            projected,
            vec![ToolActivityGroup::Exploration {
                active: false,
                activities: vec![ExplorationActivity {
                    kind: ExplorationActivityKind::List,
                    targets: vec![".".into()],
                    scopes: Vec::new(),
                }],
            }]
        );
    }

    #[test]
    fn projects_a_safe_jq_filter_as_file_reading() {
        let arguments = serde_json::json!({ "command": "cat test.json | jq ." });
        let projected = project_tool_activities(
            [activity("bash", &arguments, ToolActivityStatus::Succeeded)],
            Path::new("/workspace"),
        );

        assert_eq!(
            projected,
            vec![ToolActivityGroup::Exploration {
                active: false,
                activities: vec![ExplorationActivity {
                    kind: ExplorationActivityKind::Read,
                    targets: vec!["test.json".into()],
                    scopes: Vec::new(),
                }],
            }]
        );

        for arguments in [
            serde_json::json!({ "command": "cat TEST.md", "wait": false }),
            serde_json::json!({ "command": "cat TEST.md", "env": {"LANG": "C"} }),
            serde_json::json!({ "command": "cat TEST.md", "forward_env": ["LANG"] }),
        ] {
            assert!(matches!(
                project_tool_activities(
                    [activity("bash", &arguments, ToolActivityStatus::Succeeded)],
                    Path::new("/workspace"),
                )
                .as_slice(),
                [ToolActivityGroup::Bash { .. }]
            ));
        }
    }

    #[test]
    fn projects_rich_vcs_labels_and_relations() {
        let commands = [
            "jj status",
            "jj file show -r @ SPEC.md",
            "git status --short",
            "git diff HEAD -- SPEC.md",
        ];
        let tools = commands
            .iter()
            .map(|command| {
                let arguments = Box::leak(Box::new(serde_json::json!({ "command": command })));
                activity("bash", arguments, ToolActivityStatus::Succeeded)
            })
            .collect::<Vec<_>>();
        let projected = project_tool_activities(tools, Path::new("/workspace"));
        let ToolActivityGroup::Exploration { activities, .. } = &projected[0] else {
            panic!("expected exploration group");
        };

        let (label, targets, _relations) = exploration_activity_parts(&activities[0]);
        assert_eq!((label, targets), ("Read jj status", &[][..]));
        let (label, targets, relations) = exploration_activity_parts(&activities[1]);
        assert_eq!((label, targets), ("Read jj file", &["SPEC.md".into()][..]));
        assert_eq!(relations, [("in", &["@".into()][..])]);
        assert_eq!(
            exploration_activity_parts(&activities[2]).0,
            "Read git status"
        );
        let (label, targets, relations) = exploration_activity_parts(&activities[3]);
        assert_eq!(label, "Read git diff");
        assert_eq!(targets, ["SPEC.md"]);
        assert_eq!(relations, [("in", &["HEAD".into()][..])]);
    }

    #[test]
    fn appends_adjacent_exploration_groups_without_a_new_card() {
        let mut groups = vec![ToolActivityGroup::Exploration {
            active: false,
            activities: vec![ExplorationActivity {
                kind: ExplorationActivityKind::List,
                targets: vec!["*.rs".into()],
                scopes: vec!["src".into()],
            }],
        }];
        append_tool_activity_groups(
            &mut groups,
            [ToolActivityGroup::Exploration {
                active: false,
                activities: vec![
                    ExplorationActivity {
                        kind: ExplorationActivityKind::List,
                        targets: vec!["*.rs".into(), "*.md".into()],
                        scopes: vec!["src".into(), "tests".into()],
                    },
                    ExplorationActivity {
                        kind: ExplorationActivityKind::Read,
                        targets: vec!["SPEC.md".into()],
                        scopes: Vec::new(),
                    },
                ],
            }],
        );

        assert_eq!(
            groups,
            vec![ToolActivityGroup::Exploration {
                active: false,
                activities: vec![
                    ExplorationActivity {
                        kind: ExplorationActivityKind::List,
                        targets: vec!["*.rs".into(), "*.md".into()],
                        scopes: vec!["src".into(), "tests".into()],
                    },
                    ExplorationActivity {
                        kind: ExplorationActivityKind::Read,
                        targets: vec!["SPEC.md".into()],
                        scopes: Vec::new(),
                    },
                ],
            }]
        );
    }

    #[test]
    fn omits_an_exploration_group_containing_only_failed_reads() {
        let arguments = serde_json::json!({ "path": "missing.rs" });
        assert!(
            project_tool_activities(
                [activity("read", &arguments, ToolActivityStatus::Failed)],
                Path::new("/workspace"),
            )
            .is_empty()
        );
    }

    #[test]
    fn built_in_tool_labels_are_explicit_and_unknown_names_are_preserved() {
        for (name, expected) in [
            ("apply_patch", "Apply Patch"),
            ("bash", "Bash"),
            ("change_working_directory", "Change Working Directory"),
            ("delegate_agent", "Delegate Agent"),
            ("enter_worktree", "Enter Worktree"),
            ("request_user_input", "Request User Input"),
            ("terminal_kill", "Terminal Kill"),
            ("terminal_output", "Terminal Output"),
            ("terminal_write", "Terminal Write"),
            ("update_plan", "Update Plan"),
            ("wait_join", "Wait Join"),
            ("web_fetch", "Web Fetch"),
            ("web_search", "Web Search"),
            ("grep", "Search"),
            ("read", "Read"),
            ("list", "List"),
            ("custom_tool", "custom_tool"),
            ("Mixed-case_tool", "Mixed-case_tool"),
            ("mcp__server__custom_tool", "mcp__server__custom_tool"),
        ] {
            assert_eq!(tool_display_name(name), expected);
        }

        for name in [
            "change_working_directory",
            "enter_worktree",
            "request_user_input",
        ] {
            let arguments = serde_json::json!({});
            assert!(matches!(
                &project_tool_activities(
                    [activity(name, &arguments, ToolActivityStatus::Pending)],
                    Path::new("/workspace"),
                )[0],
                ToolActivityGroup::Tool { label, .. } if label == tool_display_name(name)
            ));
        }
    }

    #[test]
    fn delegation_profile_names_are_not_tool_labels() {
        for (arguments, expected) in [
            (serde_json::json!({"agent": "explore"}), "explore"),
            (
                serde_json::json!({"agent": "Mixed-case_agent"}),
                "Mixed-case_agent",
            ),
            (serde_json::json!({"agent": "web_search"}), "web_search"),
            (serde_json::json!({}), "explore"),
        ] {
            assert!(matches!(
                &project_tool_activities(
                    [activity("delegate_agent", &arguments, ToolActivityStatus::Succeeded)],
                    Path::new("/workspace"),
                )[0],
                ToolActivityGroup::Delegate { profile, .. } if profile == expected
            ));
        }
    }

    #[test]
    fn projects_regular_tools_with_state_and_original_names() {
        let arguments = serde_json::json!({ "command": "build" });
        assert_eq!(
            project_tool_activities(
                [activity(
                    "custom_tool",
                    &arguments,
                    ToolActivityStatus::Pending,
                )],
                Path::new("/workspace"),
            ),
            vec![ToolActivityGroup::Tool {
                label: "custom_tool".into(),
                target: Some("build".into()),
                status: ToolActivityStatus::Pending,
            }]
        );
    }

    #[test]
    fn projects_terminal_writes_and_kills_with_the_original_command() {
        let terminal_id = crate::TerminalId::new();
        let terminal = crate::TerminalSnapshot {
            id: terminal_id,
            owner: crate::ConversationId::new(),
            owner_agent_run_id: None,
            tool_call_node_id: None,
            read_safe: None,
            command: "cat".into(),
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
        let write = serde_json::json!({ "id": terminal_id, "data": "hello" });
        let kill = serde_json::json!({ "id": terminal_id });

        assert_eq!(
            project_tool_activities_with_context(
                [
                    activity("terminal_write", &write, ToolActivityStatus::Succeeded),
                    activity("terminal_kill", &kill, ToolActivityStatus::Succeeded),
                ],
                Path::new("/workspace"),
                &[],
                &[terminal],
            ),
            vec![
                ToolActivityGroup::TerminalWrite {
                    terminal_id: Some(terminal_id),
                    command: "cat".into(),
                    data: "hello".into(),
                    status: ToolActivityStatus::Succeeded,
                },
                ToolActivityGroup::TerminalKill {
                    command: "cat".into(),
                    status: ToolActivityStatus::Succeeded,
                },
            ]
        );
    }

    #[test]
    fn projects_web_search_without_exposing_provider_payload_shape() {
        let arguments = serde_json::json!({ "query": "rust async" });
        let result = serde_json::json!({
            "provider": "searxng",
            "results": [{ "title": "Rust", "url": "https://example.com" }]
        });
        let projected = project_tool_activities(
            [ToolActivityRef {
                node_id: None,
                name: "web_search",
                arguments: &arguments,
                result: Some(&result),
                status: ToolActivityStatus::Succeeded,
            }],
            Path::new("/workspace"),
        );
        assert_eq!(
            projected,
            vec![ToolActivityGroup::WebSearch {
                provider: "searxng".into(),
                query: "rust async".into(),
                status: ToolActivityStatus::Succeeded,
                result_count: Some(1),
                results: vec![crate::WebSearchResult {
                    title: "Rust".into(),
                    url: "https://example.com".into(),
                    snippet: String::new(),
                    published_at: None,
                }],
            }]
        );
        assert_eq!(
            tool_activity_group_summary(&projected[0]),
            "Web search searxng · rust async"
        );
    }

    #[test]
    fn projects_mcp_tools_with_original_identity_duration_and_summary() {
        let arguments = serde_json::json!({"city": "Toronto"});
        let result = serde_json::json!({
            "server": "weather-service",
            "tool": "current_forecast",
            "provider_name": "mcp__weather_service__current_forecast",
            "content": [],
            "is_error": false,
            "duration_millis": 42,
            "summary": "Sunny, 24 C"
        });
        let projected = project_tool_activities(
            [ToolActivityRef {
                node_id: None,
                name: "mcp__weather_service__current_forecast",
                arguments: &arguments,
                result: Some(&result),
                status: ToolActivityStatus::Succeeded,
            }],
            Path::new("/workspace"),
        );

        assert_eq!(
            projected,
            vec![ToolActivityGroup::Mcp {
                call: McpCall {
                    node_id: None,
                    server: "weather-service".into(),
                    tool: "current_forecast".into(),
                    status: ToolActivityStatus::Succeeded,
                    duration_millis: Some(42),
                    parameters: arguments.clone(),
                    output: Some(result.clone()),
                }
            }]
        );
        assert_eq!(
            tool_activity_group_summary(&projected[0]),
            "MCP weather-service · current_forecast"
        );
    }

    #[test]
    fn pending_mcp_tools_fall_back_to_provider_safe_identity() {
        let arguments = serde_json::json!({});
        assert_eq!(
            project_tool_activities(
                [activity(
                    "mcp__weather_service__current_forecast",
                    &arguments,
                    ToolActivityStatus::Pending,
                )],
                Path::new("/workspace"),
            ),
            vec![ToolActivityGroup::Mcp {
                call: McpCall {
                    node_id: None,
                    server: "weather_service".into(),
                    tool: "current_forecast".into(),
                    status: ToolActivityStatus::Pending,
                    duration_millis: None,
                    parameters: arguments.clone(),
                    output: None,
                }
            }]
        );
    }

    #[test]
    fn mcp_display_output_formats_json_text_and_preserves_other_content() {
        let mut call = McpCall {
            node_id: None,
            server: "test".into(),
            tool: "test".into(),
            status: ToolActivityStatus::Succeeded,
            duration_millis: None,
            parameters: serde_json::json!({}),
            output: None,
        };
        assert_eq!(call.display_output(), None);
        for (output, expected) in [
            (
                serde_json::json!({"structured_content": {"ok": true}, "content": []}),
                serde_json::json!({"ok": true}),
            ),
            (
                serde_json::json!({"content": [{"type": "text", "text": "{\"ok\":true}"}]}),
                serde_json::json!({"ok": true}),
            ),
            (
                serde_json::json!({"content": [{"type": "text", "text": "not JSON\nhello"}]}),
                serde_json::json!("not JSON\nhello"),
            ),
            (
                serde_json::json!({"content": [{"type": "image", "data": "abc"}]}),
                serde_json::json!([{"type": "image", "data": "abc"}]),
            ),
            (
                serde_json::json!({"error": "failed"}),
                serde_json::json!({"error": "failed"}),
            ),
        ] {
            call.output = Some(output);
            assert_eq!(call.display_output(), Some(expected));
        }
    }

    #[test]
    fn delegation_projection_follows_linked_run_lifecycle() {
        let arguments = serde_json::json!({
            "agent": "code-review",
            "task": "Inspect the parser"
        });
        for (status, expected) in [
            (ToolActivityStatus::Pending, "Running"),
            (ToolActivityStatus::Failed, "Failed to spawn"),
            (ToolActivityStatus::Succeeded, "Running"),
        ] {
            assert_eq!(
                project_tool_activities(
                    [activity("delegate_agent", &arguments, status)],
                    Path::new("/workspace"),
                ),
                vec![ToolActivityGroup::Delegate {
                    action: expected.into(),
                    profile: "code-review".into(),
                    task: Some("Inspect the parser".into()),
                    status,
                }]
            );
        }

        let id = crate::AgentRunId::new();
        let result = serde_json::json!({"id": id});
        for status in [
            crate::AgentRunStatus::Queued,
            crate::AgentRunStatus::Running,
        ] {
            assert_eq!(
                project_tool_activities_with_agent_runs(
                    [ToolActivityRef {
                        result: Some(&result),
                        ..activity("delegate_agent", &arguments, ToolActivityStatus::Succeeded)
                    }],
                    Path::new("/workspace"),
                    &[delegated_run(id, status)],
                ),
                vec![ToolActivityGroup::Delegate {
                    action: "Running".into(),
                    profile: "code-review".into(),
                    task: Some("Inspect the parser".into()),
                    status: ToolActivityStatus::Pending,
                }]
            );
        }
        for status in [
            crate::AgentRunStatus::Completed,
            crate::AgentRunStatus::Failed,
            crate::AgentRunStatus::Cancelled,
            crate::AgentRunStatus::Interrupted,
        ] {
            assert_eq!(
                project_tool_activities_with_agent_runs(
                    [ToolActivityRef {
                        result: Some(&result),
                        ..activity("delegate_agent", &arguments, ToolActivityStatus::Succeeded)
                    }],
                    Path::new("/workspace"),
                    &[delegated_run(id, status)],
                )[0],
                ToolActivityGroup::Delegate {
                    action: "Finished".into(),
                    profile: "code-review".into(),
                    task: Some("Inspect the parser".into()),
                    status: ToolActivityStatus::Succeeded,
                }
            );
        }
    }

    #[test]
    fn projects_completed_questions_with_their_transcript() {
        let arguments = serde_json::json!({
            "questions": [{
                "id": "scope",
                "header": "Scope",
                "question": "Where?",
                "options": [{"label": "Root", "description": "Main"}]
            }]
        });
        let result = serde_json::json!({
            "answers": {"scope": {"selection": "Root", "note": "Keep it local."}},
            "cancelled": false
        });
        let projected = project_tool_activities(
            [ToolActivityRef {
                node_id: None,
                name: "request_user_input",
                arguments: &arguments,
                result: Some(&result),
                status: ToolActivityStatus::Succeeded,
            }],
            Path::new("/workspace"),
        );

        let [ToolActivityGroup::Question { transcript }] = projected.as_slice() else {
            panic!("expected a projected question transcript");
        };
        assert_eq!(transcript.answered, 1);
        assert_eq!(transcript.entries[0].answer.as_deref(), Some("Root"));
        assert_eq!(
            transcript.entries[0].note.as_deref(),
            Some("Keep it local.")
        );
    }

    #[test]
    fn coordination_helpers_are_not_presented_as_activity_cards() {
        let arguments = serde_json::json!({"id": "agent-1"});
        for name in ["wait_join", "terminal_output", "update_plan"] {
            assert!(
                project_tool_activities(
                    [activity(name, &arguments, ToolActivityStatus::Pending)],
                    Path::new("/workspace"),
                )
                .is_empty()
            );
        }
    }

    #[test]
    fn web_fetch_uses_the_requested_format() {
        let arguments = serde_json::json!({"url": "https://example.test", "format": "html"});
        let result = serde_json::json!({
            "url": "https://example.test",
            "content_type": "text/html",
            "output": "<main>raw html</main>"
        });
        let projected = project_tool_activities(
            [ToolActivityRef {
                node_id: None,
                name: "web_fetch",
                arguments: &arguments,
                result: Some(&result),
                status: ToolActivityStatus::Succeeded,
            }],
            Path::new("/workspace"),
        );

        assert!(matches!(
            projected.as_slice(),
            [ToolActivityGroup::WebFetch {
                format: crate::WebFetchFormat::Html,
                redirected_url: Some(url),
                output: Some(output),
                ..
            }] if url == "https://example.test" && output == "<main>raw html</main>"
        ));
    }

    #[test]
    fn apply_patch_summaries_name_each_file_action() {
        let arguments = serde_json::json!({
            "patch": "*** Begin Patch\n*** Update File: TEST.md\n-old\n+new\n*** End Patch"
        });
        let projected = project_tool_activities(
            [activity(
                "apply_patch",
                &arguments,
                ToolActivityStatus::Succeeded,
            )],
            Path::new("/workspace"),
        );
        assert_eq!(tool_activity_group_summary(&projected[0]), "Edit TEST.md");

        let arguments = serde_json::json!({
            "patch": "*** Begin Patch\n*** Add File: added.md\n+new\n*** Delete File: old.md\n*** Update File: before.md\n*** Move to: after.md\n-old\n+new\n*** End Patch"
        });
        let projected = project_tool_activities(
            [activity(
                "apply_patch",
                &arguments,
                ToolActivityStatus::Succeeded,
            )],
            Path::new("/workspace"),
        );
        assert_eq!(
            tool_activity_group_summary(&projected[0]),
            "Add added.md · Delete old.md · Move before.md → after.md"
        );
    }

    #[test]
    fn completed_apply_patch_preserves_its_structured_diff() {
        let arguments = serde_json::json!({
            "patch": "*** Begin Patch\n*** Update File: TEST.md\n-old\n+new\n*** End Patch"
        });
        let result = serde_json::json!({
            "diff": {
                "files": [{
                    "old_path": "/workspace/TEST.md",
                    "new_path": "/workspace/TEST.md",
                    "kind": "modified",
                    "language": "markdown",
                    "added_lines": 1,
                    "removed_lines": 1,
                    "old_no_final_newline": false,
                    "new_no_final_newline": false,
                    "hunks": [{
                        "header": "@@ -1 +1 @@",
                        "lines": [
                            {"kind": "deletion", "old_line": 1, "new_line": null, "text": "old"},
                            {"kind": "addition", "old_line": null, "new_line": 1, "text": "new"}
                        ]
                    }]
                }]
            }
        });
        let projected = project_tool_activities(
            [ToolActivityRef {
                node_id: None,
                name: "apply_patch",
                arguments: &arguments,
                result: Some(&result),
                status: ToolActivityStatus::Succeeded,
            }],
            Path::new("/workspace"),
        );

        assert!(matches!(
            projected.as_slice(),
            [ToolActivityGroup::Edit { diff, .. }]
                if diff.files[0].hunks[0].lines[1].text == "new"
        ));
    }

    #[test]
    fn leaves_output_medium_escaping_to_the_frontend() {
        let arguments = serde_json::json!({ "path": "src/\u{1b}[31m.rs" });
        let projected = project_tool_activities(
            [activity("read", &arguments, ToolActivityStatus::Pending)],
            Path::new("/workspace"),
        );
        let ToolActivityGroup::Exploration { activities, .. } = &projected[0] else {
            panic!("expected exploration group");
        };
        assert_eq!(activities[0].targets, ["src/\u{1b}[31m.rs"]);
    }

    #[test]
    fn live_exploration_stays_open_after_all_calls_finish() {
        let arguments = serde_json::json!({ "path": "src/lib.rs" });
        let projected = project_live_tool_activities(
            [activity("read", &arguments, ToolActivityStatus::Succeeded)],
            Path::new("/workspace"),
        );

        assert_eq!(
            projected,
            vec![ToolActivityGroup::Exploration {
                active: true,
                activities: vec![ExplorationActivity {
                    kind: ExplorationActivityKind::Read,
                    targets: vec!["src/lib.rs".into()],
                    scopes: Vec::new(),
                }],
            }]
        );
    }

    #[test]
    fn projected_groups_round_trip_across_frontend_boundaries() {
        let group = ToolActivityGroup::Exploration {
            active: true,
            activities: vec![ExplorationActivity {
                kind: ExplorationActivityKind::Search,
                targets: vec!["ToolActivity".into()],
                scopes: vec!["crates/cagent-agent/src".into()],
            }],
        };

        let value = serde_json::to_value(&group).unwrap();
        assert_eq!(value["kind"], "exploration");
        assert_eq!(value["activities"][0]["kind"], "search");
        assert_eq!(
            serde_json::from_value::<ToolActivityGroup>(value).unwrap(),
            group
        );
    }

    #[test]
    fn bash_projection_keeps_command_exit_code_and_complete_output() {
        let node_id = NodeId::new();
        let terminal_id = crate::TerminalId::new();
        let arguments = serde_json::json!({ "command": "cargo test" });
        let result = serde_json::json!({
            "terminal_id": terminal_id,
            "output": "first\nsecond\nthird\nfourth\nfifth\n",
            "ansi_output": "\u{1b}[31mfirst\u{1b}[0m\nsecond\nthird\nfourth\nfifth\n",
            "exit_code": 1
        });
        let projected = project_tool_activities(
            [ToolActivityRef {
                node_id: Some(node_id),
                name: "bash",
                arguments: &arguments,
                result: Some(&result),
                status: ToolActivityStatus::Failed,
            }],
            Path::new("/workspace"),
        );

        assert_eq!(
            projected,
            vec![ToolActivityGroup::Bash {
                node_id: Some(node_id),
                terminal_id: Some(terminal_id),
                command: "cargo test".into(),
                status: ToolActivityStatus::Failed,
                output: Some("first\nsecond\nthird\nfourth\nfifth\n".into()),
                ansi_output: Some(
                    "\u{1b}[31mfirst\u{1b}[0m\nsecond\nthird\nfourth\nfifth\n".into(),
                ),
                exit_code: Some(1),
            }]
        );
    }

    #[test]
    fn retained_read_safe_bash_projects_as_exploration() {
        let terminal_id = crate::TerminalId::new();
        let arguments = serde_json::json!({ "command": "cat TEST.md" });
        let result = serde_json::json!({
            "terminal_id": terminal_id,
            "output": "contents\n",
            "exit_code": 0,
        });

        let projected = project_tool_activities(
            [ToolActivityRef {
                node_id: None,
                name: "bash",
                arguments: &arguments,
                result: Some(&result),
                status: ToolActivityStatus::Succeeded,
            }],
            Path::new("/workspace"),
        );

        assert!(matches!(
            projected.as_slice(),
            [ToolActivityGroup::Exploration { activities, .. }]
                if activities == &[ExplorationActivity {
                    kind: ExplorationActivityKind::Read,
                    targets: vec!["TEST.md".into()],
                    scopes: Vec::new(),
                }]
        ));
    }

    #[test]
    fn retained_tilde_list_projects_as_exploration() {
        let arguments = serde_json::json!({
            "command": "ls ~/worktree-test/test-git",
            "wait": true,
        });
        let result = serde_json::json!({
            "output": "README.md\n",
            "exit_code": 0,
        });

        let projected = project_tool_activities(
            [ToolActivityRef {
                node_id: None,
                name: "bash",
                arguments: &arguments,
                result: Some(&result),
                status: ToolActivityStatus::Succeeded,
            }],
            Path::new("/workspace"),
        );

        assert!(matches!(
            projected.as_slice(),
            [ToolActivityGroup::Exploration { activities, .. }]
                if activities == &[ExplorationActivity {
                    kind: ExplorationActivityKind::List,
                    targets: vec!["~/worktree-test/test-git".into()],
                    scopes: Vec::new(),
                }]
        ));
    }

    #[test]
    fn retained_fd_pipeline_omits_pwd_and_stdin_only_head() {
        let terminal_id = crate::TerminalId::new();
        let arguments = serde_json::json!({
            "command": "pwd && fd -a -t f '(test|output|.*\\.sh$)' . | head -100",
            "wait": true,
        });
        // Foreground Bash completion envelopes use `id`, while terminal
        // snapshots use `terminal_id`; neither should change the semantics.
        let result = serde_json::json!({
            "id": terminal_id,
            "output": "/workspace\n/workspace/test.sh\n",
            "exit_code": 0,
        });

        let projected = project_tool_activities(
            [ToolActivityRef {
                node_id: None,
                name: "bash",
                arguments: &arguments,
                result: Some(&result),
                status: ToolActivityStatus::Succeeded,
            }],
            Path::new("/workspace"),
        );

        assert_eq!(
            projected,
            vec![ToolActivityGroup::Exploration {
                active: false,
                activities: vec![ExplorationActivity {
                    kind: ExplorationActivityKind::List,
                    targets: vec![r"(test|output|.*\.sh$)".into()],
                    scopes: vec![".".into()],
                }],
            }]
        );
    }

    #[test]
    fn projected_bash_updates_in_place_after_it_is_committed() {
        let node_id = NodeId::new();
        let arguments = serde_json::json!({ "command": "cargo run", "wait": false });
        let mut projected = project_tool_activities(
            [ToolActivityRef {
                node_id: Some(node_id),
                name: "bash",
                arguments: &arguments,
                result: None,
                status: ToolActivityStatus::Pending,
            }],
            Path::new("/workspace"),
        );
        let result = serde_json::json!({
            "output": "finished\n",
            "ansi_output": "\u{1b}[32mfinished\u{1b}[0m\r\n",
            "exit_code": 0,
        });

        assert!(update_projected_bash_activity(
            &mut projected[0],
            node_id,
            &result,
            ToolActivityStatus::Succeeded,
        ));
        assert_eq!(
            projected[0],
            ToolActivityGroup::Bash {
                node_id: Some(node_id),
                terminal_id: None,
                command: "cargo run".into(),
                status: ToolActivityStatus::Succeeded,
                output: Some("finished\n".into()),
                ansi_output: Some("\u{1b}[32mfinished\u{1b}[0m\r\n".into()),
                exit_code: Some(0),
            }
        );
    }

    #[test]
    fn presents_workspace_paths_relative_to_the_workspace() {
        let arguments = [
            serde_json::json!({ "path": "/workspace/crates/cagent-agent/src/lib.rs" }),
            serde_json::json!({
                "pattern": "ToolActivity",
                "paths": ["/workspace/crates/cagent-agent/src"]
            }),
        ];
        let projected = project_tool_activities(
            [
                activity("read", &arguments[0], ToolActivityStatus::Succeeded),
                activity("grep", &arguments[1], ToolActivityStatus::Succeeded),
            ],
            Path::new("/workspace"),
        );
        let ToolActivityGroup::Exploration { activities, .. } = &projected[0] else {
            panic!("expected exploration group");
        };

        assert_eq!(activities[0].targets, ["crates/cagent-agent/src/lib.rs"]);
        assert_eq!(activities[1].targets, ["ToolActivity"]);
        assert_eq!(activities[1].scopes, ["crates/cagent-agent/src"]);
    }

    #[test]
    fn tracker_extends_exploration_and_releases_it_before_a_regular_tool() {
        let mut tracker = ToolActivityTracker::default();
        let read_id = NodeId::new();
        let list_id = NodeId::new();
        let edit_id = NodeId::new();
        assert!(
            tracker
                .push(ToolActivity {
                    node_id: read_id,
                    name: "read".into(),
                    arguments: serde_json::json!({ "path": "src/lib.rs" }),
                    result: None,
                    status: ToolActivityStatus::Pending,
                })
                .is_none()
        );
        assert!(tracker.finish(read_id, None, false));
        assert!(
            tracker
                .push(ToolActivity {
                    node_id: list_id,
                    name: "list".into(),
                    arguments: serde_json::json!({ "path": "src" }),
                    result: None,
                    status: ToolActivityStatus::Pending,
                })
                .is_none()
        );
        assert!(tracker.finish(list_id, None, false));

        let completed = tracker
            .push(ToolActivity {
                node_id: edit_id,
                name: "apply_patch".into(),
                arguments: serde_json::json!({ "path": "src/lib.rs" }),
                result: None,
                status: ToolActivityStatus::Pending,
            })
            .expect("the completed exploration batch should be released");

        assert_eq!(completed.len(), 2);
        assert_eq!(tracker.activities().len(), 1);
        assert_eq!(tracker.activities()[0].node_id, edit_id);
        assert!(tracker.take_completed().is_none());
    }

    #[test]
    fn tracker_distinguishes_an_open_completed_batch_from_running_work() {
        let mut tracker = ToolActivityTracker::default();
        let node_id = NodeId::new();
        tracker.push(ToolActivity {
            node_id,
            name: "list".into(),
            arguments: serde_json::json!({ "path": "." }),
            result: None,
            status: ToolActivityStatus::Pending,
        });

        assert!(tracker.has_pending());
        assert!(tracker.finish(node_id, None, false));
        assert!(!tracker.has_pending());
        assert!(!tracker.is_empty());
    }

    #[test]
    fn tracker_takes_matching_work_without_reordering_either_partition() {
        let mut tracker = ToolActivityTracker::from(
            [
                ("read", serde_json::json!({ "path": "first.rs" })),
                (
                    "bash",
                    serde_json::json!({ "command": "cargo run", "wait": false }),
                ),
                ("grep", serde_json::json!({ "pattern": "needle" })),
                (
                    "bash",
                    serde_json::json!({ "command": "cargo test", "wait": false }),
                ),
            ]
            .into_iter()
            .map(|(name, arguments)| ToolActivity {
                node_id: NodeId::new(),
                name: name.into(),
                arguments,
                result: None,
                status: ToolActivityStatus::Pending,
            })
            .collect::<Vec<_>>(),
        );

        let background = tracker.take_matching(|activity| {
            activity.name == "bash" && activity.arguments["wait"] == false
        });

        assert_eq!(
            background
                .iter()
                .map(|activity| activity.arguments["command"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["cargo run", "cargo test"]
        );
        assert_eq!(
            tracker
                .activities()
                .iter()
                .map(|activity| activity.name.as_str())
                .collect::<Vec<_>>(),
            ["read", "grep"]
        );
    }
}
