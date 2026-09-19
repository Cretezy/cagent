use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;

use crate::protocol::{ComposerHistoryEntry, ConversationSummary, HistoryNode, NodeId, NodeKind};

/// Agent-owned history rows tied to the durable cursor used to project them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryRowsSnapshot {
    pub revision: Option<crate::protocol::EventCursor>,
    pub rows: Vec<HistoryRow>,
}

/// Durable history projection requested by a frontend.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HistoryScope {
    FullTree,
    ActiveBranch,
}

/// Source indexes for rows shown by a conversation picker. Keeping this
/// projection frontend-neutral ensures rendering, navigation, and activation
/// all translate through the same ordered view.
#[must_use]
pub fn conversation_picker_indexes(rows: &[ConversationSummary], query: &str) -> Vec<usize> {
    let query = query.to_lowercase();
    rows.iter()
        .enumerate()
        .filter(|(_, row)| {
            !row.is_untitled()
                && (query.is_empty()
                    || row.title.to_lowercase().contains(&query)
                    || row.mode.to_lowercase().contains(&query)
                    || row
                        .model
                        .as_deref()
                        .is_some_and(|model| model.to_lowercase().contains(&query)))
        })
        .map(|(index, _)| index)
        .collect()
}

/// Initial visible position for a conversation picker. Favourites remain
/// pinned at the top, but opening the picker targets the newest ordinary row.
#[must_use]
pub fn conversation_picker_initial_selection(rows: &[ConversationSummary], query: &str) -> usize {
    conversation_picker_indexes(rows, query)
        .iter()
        .position(|source| !rows[*source].favourite)
        .unwrap_or(0)
}

/// Source indexes for history rows matching the visible picker query.
#[must_use]
pub fn history_picker_indexes(rows: &[HistoryRow], query: &str) -> Vec<usize> {
    let query = query.to_lowercase();
    rows.iter()
        .enumerate()
        .filter(|(_, row)| {
            query.is_empty()
                || row.preview.to_lowercase().contains(&query)
                || row.label().contains(&query)
        })
        .map(|(index, _)| index)
        .collect()
}

/// Keeps only the most recent copy of each composer entry.
///
/// Entries are compared as a whole, so attachment specifications are part of
/// the identity. Re-sending an older entry moves it to the end of recall.
pub fn collapse_consecutive_composer_entries(
    entries: Vec<ComposerHistoryEntry>,
) -> Vec<ComposerHistoryEntry> {
    let mut collapsed = Vec::with_capacity(entries.len());
    for entry in entries.into_iter().rev() {
        if !collapsed.contains(&entry) {
            collapsed.push(entry);
        }
    }
    collapsed.reverse();
    collapsed
}

/// Semantic role of a row in a conversation-history picker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HistoryRowKind {
    Conversation,
    Branch,
    User,
    Assistant,
    AssistantPlan,
    AcceptedPlan,
    Compact,
    Exploration,
    Run,
    Mutation,
    ToolCall,
    ToolResult,
    Permission,
    Notice,
}

/// Typed, frontend-neutral history data.
///
/// `id` is the stable identity of the displayed row. `fork_target` is the
/// durable node the runtime should activate when the row is selected; these
/// differ for grouped tool activity and proposed plans. A row's `parent_id`
/// always refers to another displayed row, never to hidden durable activity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryRow {
    pub id: NodeId,
    pub parent_id: Option<NodeId>,
    pub created_at: String,
    pub kind: HistoryRowKind,
    pub preview: String,
    pub status: Option<crate::NodeStatus>,
    pub active: bool,
    pub selectable: bool,
    pub fork_target: NodeId,
    /// Transcript block represented by this row when it belongs to the active branch.
    pub transcript_target: Option<crate::protocol::TranscriptBlockId>,
    pub user_draft: Option<ComposerHistoryEntry>,
    pub guide: String,
    /// Present only for an accepted-plan row.
    pub reset_context: Option<bool>,
    /// Present only for an accepted-plan row.
    pub compact_context: Option<bool>,
}

impl HistoryRow {
    /// Lowercase label displayed before this row's preview in history pickers.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self.kind {
            HistoryRowKind::Conversation => "root",
            HistoryRowKind::Branch => "branch",
            HistoryRowKind::User => "user",
            HistoryRowKind::Assistant => "assistant",
            HistoryRowKind::AssistantPlan => "plan",
            HistoryRowKind::AcceptedPlan if self.reset_context.unwrap_or(false) => {
                "accepted plan (clear)"
            }
            HistoryRowKind::AcceptedPlan if self.compact_context.unwrap_or(false) => {
                "accepted plan (compact)"
            }
            HistoryRowKind::AcceptedPlan => "accepted plan",
            HistoryRowKind::Mutation => "assistant",
            HistoryRowKind::Compact => "compact",
            HistoryRowKind::Exploration => "explore",
            HistoryRowKind::Run => "run",
            HistoryRowKind::ToolCall => "tool call",
            HistoryRowKind::ToolResult => "tool result",
            HistoryRowKind::Permission => "permission",
            HistoryRowKind::Notice => "notice",
        }
    }
}

#[derive(Clone, Debug)]
struct ProjectedNode {
    id: NodeId,
    parent_id: Option<NodeId>,
    created_at: String,
    kind: HistoryRowKind,
    preview: String,
    status: crate::NodeStatus,
    active: bool,
    fork_target: NodeId,
    user_draft: Option<ComposerHistoryEntry>,
    tools: Vec<crate::ToolActivity>,
    content: Value,
}

/// Projects durable history into rows suitable for a history surface.
///
/// The workspace and terminal snapshots are inputs because tool summaries and
/// live Bash state are presentation semantics, not durable node metadata.
#[must_use]
pub fn project_history_rows_with_context(
    durable: &[HistoryNode],
    workspace: &Path,
    terminals: &[crate::TerminalSnapshot],
) -> Vec<HistoryRow> {
    let parents = durable
        .iter()
        .map(|node| (node.id, node.parent_id))
        .collect::<HashMap<_, _>>();
    let nodes_by_id = durable
        .iter()
        .map(|node| (node.id, node))
        .collect::<HashMap<_, _>>();
    let mut calls_by_owner = HashMap::<NodeId, Vec<&HistoryNode>>::new();
    let mut results_by_call = HashMap::<NodeId, &HistoryNode>::new();
    for node in durable {
        match node.kind {
            NodeKind::ToolCall => {
                if let Some(owner) = node.owner_id {
                    calls_by_owner.entry(owner).or_default().push(node);
                }
            }
            NodeKind::ToolResult => {
                if let Some(call) = node.owner_id {
                    results_by_call.insert(call, node);
                }
            }
            _ => {}
        }
    }
    let terminals_by_call = terminals
        .iter()
        .filter_map(|terminal| terminal.tool_call_node_id.map(|id| (id, terminal)))
        .collect::<HashMap<_, _>>();
    let durable_active = durable.iter().find(|node| node.active).map(|node| node.id);
    let mut active_branch = std::collections::HashSet::new();
    let mut cursor = durable_active;
    while let Some(id) = cursor {
        active_branch.insert(id);
        cursor = parents.get(&id).copied().flatten();
    }
    // Branch markers remain in durable history for restoration, but are
    // internal to the presentation projection. `nearest_visible` below
    // reparents their displayed descendants to the visible ancestor.
    let visible_kinds = [
        NodeKind::ConversationRoot,
        NodeKind::UserMessage,
        NodeKind::AssistantMessage,
        NodeKind::AcceptedPlan,
        NodeKind::CompactionSummary,
    ];
    let mut projected = durable
        .iter()
        .filter(|node| visible_kinds.contains(&node.kind))
        .map(|node| ProjectedNode {
            id: node.id,
            parent_id: node.parent_id,
            created_at: node.created_at.clone(),
            kind: row_kind(node.kind.clone()),
            preview: node_preview(node, None),
            status: node.status,
            active: false,
            fork_target: node.id,
            user_draft: crate::history_user_draft(node).map(|(text, attachment_specs)| {
                ComposerHistoryEntry {
                    kind: crate::ComposerInputKind::Prompt,
                    text,
                    attachment_specs,
                    images: Vec::new(),
                    image_chips: Vec::new(),
                }
            }),
            tools: Vec::new(),
            content: node.content.clone(),
        })
        .collect::<Vec<_>>();
    let projected_indexes = projected
        .iter()
        .enumerate()
        .map(|(index, node)| (node.id, index))
        .collect::<HashMap<_, _>>();

    let mut aliases = HashMap::<NodeId, NodeId>::new();

    // A durable plan is the response node itself. Older rows used a child plan
    // node, so retain that projection for replay compatibility.
    for plan in durable.iter().filter(|node| {
        node.kind == NodeKind::AssistantMessage
            && node.content.get("flavor").and_then(Value::as_str) == Some("plan")
    }) {
        if let Some(index) = projected_indexes.get(&plan.id).copied() {
            let row = &mut projected[index];
            row.kind = HistoryRowKind::AssistantPlan;
            row.preview = node_preview(plan, None);
            row.fork_target = plan.id;
            continue;
        }
        let Some(parent_id) = plan.parent_id else {
            continue;
        };
        let Some(index) = projected_indexes.get(&parent_id).copied() else {
            continue;
        };
        let assistant = &mut projected[index];
        if assistant.kind != HistoryRowKind::Assistant {
            continue;
        }
        assistant.kind = HistoryRowKind::AssistantPlan;
        assistant.preview = node_preview(plan, None);
        assistant.fork_target = plan.id;
        aliases.insert(plan.id, assistant.id);
    }

    // Group tool activity under its assistant response, and split mutations
    // into independent rows so each edit remains a valid fork point.
    let mut mutation_rows = Vec::new();
    for assistant in projected.iter_mut().filter(|node| {
        matches!(
            node.kind,
            HistoryRowKind::Assistant | HistoryRowKind::AssistantPlan
        )
    }) {
        let calls = calls_by_owner
            .get(&assistant.id)
            .map(Vec::as_slice)
            .unwrap_or_default();
        if calls.is_empty() {
            continue;
        }
        let mut activities = calls
            .iter()
            .map(|call| {
                let call = *call;
                let result = results_by_call.get(&call.id).copied();
                let name = call.content["name"].as_str().unwrap_or("tool");
                let arguments = call
                    .content
                    .get("arguments")
                    .cloned()
                    .unwrap_or(Value::Null);
                let status = match result {
                    Some(result) if result.content["is_error"] == true => {
                        crate::ToolActivityStatus::Failed
                    }
                    Some(result)
                        if crate::detached_bash_result_is_running(
                            name,
                            &arguments,
                            &result.content["output"],
                        ) =>
                    {
                        crate::ToolActivityStatus::Pending
                    }
                    Some(_) => crate::ToolActivityStatus::Succeeded,
                    None => crate::ToolActivityStatus::Pending,
                };
                let mut tool = crate::ToolActivity {
                    node_id: call.id,
                    name: name.into(),
                    arguments,
                    result: result.and_then(|node| node.content.get("output")).cloned(),
                    status,
                };
                if name == "bash"
                    && let Some(terminal) = terminals_by_call.get(&call.id)
                    && terminal.owner_agent_run_id.is_none()
                {
                    tool.status = crate::presentation::terminal_activity_status(terminal);
                    tool.result = Some(crate::presentation::terminal_activity_result(terminal));
                }
                (call, result, tool)
            })
            .collect::<Vec<_>>();
        let (mutations, other_tools): (Vec<_>, Vec<_>) = activities
            .drain(..)
            .partition(|(_, _, tool)| crate::is_mutation_tool(&tool.name));
        if !other_tools.is_empty() {
            assistant.tools = other_tools
                .iter()
                .map(|(_, _, tool)| tool.clone())
                .collect();
            assistant.kind = if other_tools.iter().all(|(_, _, tool)| {
                crate::exploration_activity_kind_for(&tool.name, &tool.arguments).is_some()
            }) {
                HistoryRowKind::Exploration
            } else if other_tools.iter().all(|(_, _, tool)| tool.name == "bash") {
                HistoryRowKind::Run
            } else {
                HistoryRowKind::Assistant
            };
            assistant.preview = summarize_tools(&assistant.tools, workspace);
            if let Some(result) = other_tools
                .iter()
                .filter_map(|(_, result, _)| *result)
                .next_back()
            {
                assistant.fork_target = result.id;
            }
            if let Some((call, _, _)) = other_tools
                .iter()
                .find(|(_, _, tool)| tool.name == "request_user_input")
            {
                assistant.fork_target = call.id;
            }
            for (call, result, _) in &other_tools {
                aliases.insert(call.id, assistant.id);
                if let Some(result) = result {
                    aliases.insert(result.id, assistant.id);
                }
            }
        }

        let text = nodes_by_id
            .get(&assistant.id)
            .copied()
            .and_then(|node| node.content.get("text"))
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty());
        let replace_assistant = !mutations.is_empty() && other_tools.is_empty() && text.is_none();
        let mut previous = assistant.id;
        for (index, (call, result, tool)) in mutations.into_iter().enumerate() {
            let summary = crate::mutation_activity_summaries(tool.as_ref(), workspace)
                .into_iter()
                .next()
                .unwrap_or_else(|| {
                    crate::summarize_tool_activities(
                        std::slice::from_ref(&tool),
                        workspace,
                        tool.status == crate::ToolActivityStatus::Pending,
                    )
                });
            let use_assistant = replace_assistant && index == 0;
            if use_assistant {
                assistant.kind = HistoryRowKind::Mutation;
                assistant.preview = summary;
                assistant.tools = vec![tool.clone()];
                assistant.status = tool_status(&tool);
            } else {
                let id = call.id;
                let row = ProjectedNode {
                    id,
                    parent_id: Some(previous),
                    created_at: call.created_at.clone(),
                    kind: HistoryRowKind::Mutation,
                    preview: summary.clone(),
                    status: tool_status(&tool),
                    active: false,
                    fork_target: result.map_or(id, |node| node.id),
                    user_draft: None,
                    tools: vec![tool.clone()],
                    content: serde_json::json!({"text": summary}),
                };
                mutation_rows.push(row);
                aliases.insert(call.id, id);
                if let Some(result) = result {
                    aliases.insert(result.id, id);
                }
                previous = id;
            }
            if use_assistant {
                if let Some(result) = result {
                    assistant.fork_target = result.id;
                    aliases.insert(result.id, assistant.id);
                }
                aliases.insert(call.id, assistant.id);
            }
        }
    }
    projected.extend(mutation_rows);

    let included = projected
        .iter()
        .map(|node| node.id)
        .collect::<std::collections::HashSet<_>>();
    let nearest_visible = |mut id: Option<NodeId>| {
        while let Some(current) = id {
            if let Some(alias) = aliases.get(&current) {
                return Some(*alias);
            }
            if included.contains(&current) {
                return Some(current);
            }
            id = parents.get(&current).copied().flatten();
        }
        None
    };
    let active = nearest_visible(durable_active);
    for node in &mut projected {
        node.parent_id = nearest_visible(node.parent_id);
        node.active = Some(node.id) == active;
    }
    let rows = depth_first_projected(&projected)
        .into_iter()
        .map(|node| {
            let selectable =
                node.status == "completed" && !matches!(node.kind, HistoryRowKind::Branch);
            let status = (!matches!(
                node.status,
                crate::NodeStatus::Completed | crate::NodeStatus::Streaming
            ))
            .then_some(node.status);
            let on_active_branch = active_branch.contains(&node.id)
                || node
                    .tools
                    .iter()
                    .any(|tool| active_branch.contains(&tool.node_id));
            let transcript_target =
                if !on_active_branch || node.kind == HistoryRowKind::Conversation {
                    None
                } else if let Some(tool) = node.tools.first() {
                    Some(crate::protocol::TranscriptBlockId::derived(
                        "tools",
                        tool.node_id,
                    ))
                } else {
                    Some(crate::protocol::TranscriptBlockId::node(node.id))
                };
            HistoryRow {
                id: node.id,
                parent_id: node.parent_id,
                created_at: node.created_at,
                kind: node.kind,
                preview: if node.kind == HistoryRowKind::Conversation {
                    "conversation start".into()
                } else {
                    node.preview
                },
                status,
                active: node.active,
                selectable,
                fork_target: node.fork_target,
                transcript_target,
                user_draft: node.user_draft,
                guide: String::new(),
                reset_context: (node.kind == HistoryRowKind::AcceptedPlan)
                    .then(|| node.content["reset_context"].as_bool().unwrap_or(false)),
                compact_context: (node.kind == HistoryRowKind::AcceptedPlan)
                    .then(|| node.content["compact_context"].as_bool().unwrap_or(false)),
            }
        })
        .collect::<Vec<_>>();
    add_guides(rows)
}

/// Projects raw nodes for callers that do not have a workspace or terminal
/// snapshot. Runtime history queries use [`project_history_rows_with_context`].
#[must_use]
pub fn project_history_rows(nodes: &[HistoryNode]) -> Vec<HistoryRow> {
    let mut rows = project_history_rows_with_context(nodes, Path::new("."), &[]);
    // Read-only compatibility for callers that still hand this helper an
    // old projected node. Runtime queries never emit these keys; new
    // callers should use the typed fields returned by the runtime.
    for row in &mut rows {
        let Some(node) = nodes.iter().find(|node| node.id == row.id) else {
            continue;
        };
        if node.content["history_plan"] == true {
            row.kind = HistoryRowKind::AssistantPlan;
            row.fork_target = node.content["history_plan_id"]
                .as_str()
                .and_then(|id| id.parse().ok())
                .unwrap_or(row.fork_target);
        } else if node.content["history_exploration"] == true {
            row.kind = HistoryRowKind::Exploration;
        } else if node.content["history_mutation"] == true {
            row.kind = HistoryRowKind::Mutation;
            row.fork_target = node.content["history_fork_at"]
                .as_str()
                .and_then(|id| id.parse().ok())
                .unwrap_or(row.fork_target);
        }
    }
    rows
}

fn add_guides(mut rows: Vec<HistoryRow>) -> Vec<HistoryRow> {
    let mut children = HashMap::<NodeId, Vec<NodeId>>::new();
    let parents = rows
        .iter()
        .map(|row| (row.id, row.parent_id))
        .collect::<HashMap<_, _>>();
    for row in &rows {
        if let Some(parent) = row.parent_id {
            children.entry(parent).or_default().push(row.id);
        }
    }
    for row in &mut rows {
        let mut lanes = Vec::new();
        let mut child = row.id;
        while let Some(parent) = parents.get(&child).copied().flatten() {
            if let Some(siblings) = children.get(&parent)
                && siblings.len() > 1
            {
                let index = siblings
                    .iter()
                    .position(|id| *id == child)
                    .unwrap_or_default();
                lanes.push((index + 1 == siblings.len(), child == row.id));
            }
            child = parent;
        }
        lanes.reverse();
        let lane_count = lanes.len();
        row.guide = lanes
            .into_iter()
            .enumerate()
            .map(|(index, (last, direct))| {
                if index + 1 == lane_count && direct {
                    if last { "└─ " } else { "├─ " }
                } else if last {
                    "   "
                } else {
                    "│  "
                }
            })
            .collect();
    }
    rows
}

fn depth_first_projected(nodes: &[ProjectedNode]) -> Vec<ProjectedNode> {
    let mut children = HashMap::<Option<NodeId>, Vec<usize>>::new();
    for (index, node) in nodes.iter().enumerate() {
        children.entry(node.parent_id).or_default().push(index);
    }
    let mut stack = children.get(&None).cloned().unwrap_or_default();
    stack.reverse();
    let mut ordered = Vec::with_capacity(nodes.len());
    while let Some(index) = stack.pop() {
        let node = nodes[index].clone();
        if let Some(descendants) = children.get(&Some(node.id)) {
            stack.extend(descendants.iter().rev());
        }
        ordered.push(node);
    }
    ordered
}

fn row_kind(kind: NodeKind) -> HistoryRowKind {
    match kind {
        NodeKind::ConversationRoot => HistoryRowKind::Conversation,
        NodeKind::UserMessage => HistoryRowKind::User,
        NodeKind::AssistantMessage => HistoryRowKind::Assistant,
        NodeKind::AcceptedPlan => HistoryRowKind::AcceptedPlan,
        NodeKind::CompactionSummary => HistoryRowKind::Compact,
        NodeKind::ToolCall => HistoryRowKind::ToolCall,
        NodeKind::ToolResult => HistoryRowKind::ToolResult,
        NodeKind::PermissionDecision => HistoryRowKind::Permission,
        NodeKind::System => HistoryRowKind::Notice,
    }
}

fn node_preview(node: &HistoryNode, tools: Option<&[crate::ToolActivity]>) -> String {
    if let Some(tools) = tools {
        return summarize_tools(tools, Path::new("."));
    }
    let preview = if node.kind == NodeKind::UserMessage {
        node.content
            .get("display_text")
            .and_then(Value::as_str)
            .or_else(|| node.content.get("text").and_then(Value::as_str))
    } else {
        ["text", "plan_markdown", "summary", "name"]
            .into_iter()
            .filter_map(|key| node.content.get(key).and_then(Value::as_str))
            .find(|value| !value.is_empty())
            .or(node.summary.as_deref())
    }
    .map(|value| {
        value
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or_default()
            .trim_start_matches('#')
            .trim()
            .to_owned()
    })
    .unwrap_or_default();
    if node.kind == NodeKind::UserMessage
        && let Some(label) = node
            .content
            .get("display_label")
            .and_then(Value::as_str)
            .filter(|label| !label.is_empty())
    {
        return format!("{label}: {preview}");
    }
    preview
}

fn summarize_tools(tools: &[crate::ToolActivity], workspace: &Path) -> String {
    crate::summarize_tool_activities(
        tools,
        workspace,
        tools
            .iter()
            .any(|tool| tool.status == crate::ToolActivityStatus::Pending),
    )
}

const fn tool_status(tool: &crate::ToolActivity) -> crate::NodeStatus {
    match tool.status {
        crate::ToolActivityStatus::Pending => crate::NodeStatus::Pending,
        crate::ToolActivityStatus::Succeeded => crate::NodeStatus::Completed,
        crate::ToolActivityStatus::Failed => crate::NodeStatus::Failed,
    }
}

/// Returns the durable fork target represented by a history row.
#[must_use]
pub const fn history_fork_target(row: &HistoryRow) -> NodeId {
    row.fork_target
}

/// Builds compact tree guides for a typed history projection.
#[must_use]
pub fn message_tree_guides(rows: &[HistoryRow]) -> Vec<String> {
    rows.iter().map(|row| row.guide.clone()).collect()
}

impl PartialEq<NodeKind> for HistoryRowKind {
    fn eq(&self, other: &NodeKind) -> bool {
        matches!(
            (self, other),
            (Self::Conversation, NodeKind::ConversationRoot)
                | (Self::User, NodeKind::UserMessage)
                | (Self::AcceptedPlan, NodeKind::AcceptedPlan)
                | (Self::Compact, NodeKind::CompactionSummary)
                | (
                    Self::Assistant | Self::AssistantPlan | Self::Mutation,
                    NodeKind::AssistantMessage
                )
                | (Self::ToolCall, NodeKind::ToolCall)
                | (Self::ToolResult, NodeKind::ToolResult)
                | (Self::Permission, NodeKind::PermissionDecision)
                | (Self::Notice, NodeKind::System)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapse_consecutive_composer_entries_keeps_the_most_recent_repeat() {
        let entry = |text: &str| ComposerHistoryEntry {
            kind: crate::ComposerInputKind::Prompt,
            text: text.into(),
            attachment_specs: Vec::new(),
            images: Vec::new(),
            image_chips: Vec::new(),
        };

        assert_eq!(
            collapse_consecutive_composer_entries(vec![
                entry("a"),
                entry("a"),
                entry("b"),
                entry("b"),
                entry("a"),
            ]),
            vec![entry("b"), entry("a")]
        );
    }

    #[test]
    fn picker_projections_return_stable_source_indexes() {
        let conversation = |title: &str, mode: &str| ConversationSummary {
            id: crate::ConversationId::new(),
            workspace: "/workspace".into(),
            title: title.into(),
            created_at: "0".into(),
            updated_at: "0".into(),
            active_node_id: crate::NodeId::new(),
            status: "idle".into(),
            agent: "default".into(),
            mode: mode.into(),
            model: None,
            message_count: 0,
            archived: false,
            favourite: false,
            preview: String::new(),
        };
        let conversations = [
            conversation("", "ask"),
            conversation("First", "ask"),
            conversation("Second", "plan"),
        ];
        assert_eq!(conversation_picker_indexes(&conversations, ""), vec![1, 2]);
        assert_eq!(conversation_picker_indexes(&conversations, "PLAN"), vec![2]);

        let mut conversations = conversations.to_vec();
        conversations[1].favourite = true;
        assert_eq!(conversation_picker_initial_selection(&conversations, ""), 1);

        let row = |kind, preview: &str| HistoryRow {
            id: crate::NodeId::new(),
            parent_id: None,
            created_at: "0".into(),
            kind,
            preview: preview.into(),
            status: None,
            active: true,
            selectable: true,
            fork_target: crate::NodeId::new(),
            transcript_target: None,
            user_draft: None,
            guide: String::new(),
            reset_context: None,
            compact_context: None,
        };
        let rows = [
            row(HistoryRowKind::User, "Review the API"),
            row(HistoryRowKind::Assistant, "Looks good"),
            row(HistoryRowKind::Compact, "Earlier discussion"),
        ];
        assert_eq!(history_picker_indexes(&rows, "api"), vec![0]);
        assert_eq!(history_picker_indexes(&rows, "USER"), vec![0]);
        assert_eq!(history_picker_indexes(&rows, "assistant"), vec![1]);
        assert_eq!(history_picker_indexes(&rows, "compact"), vec![2]);
        assert!(history_picker_indexes(&rows, "missing").is_empty());
    }

    #[test]
    fn collapse_consecutive_composer_entries_keeps_different_attachments() {
        let first = ComposerHistoryEntry {
            kind: crate::ComposerInputKind::Prompt,
            text: "inspect @src/lib.rs".into(),
            attachment_specs: vec![crate::AttachmentSpec {
                path: "src/lib.rs".into(),
                start_line: Some(1),
                end_line: Some(2),
            }],
            images: Vec::new(),
            image_chips: Vec::new(),
        };
        let second = ComposerHistoryEntry {
            attachment_specs: vec![crate::AttachmentSpec {
                path: "src/lib.rs".into(),
                start_line: Some(3),
                end_line: Some(4),
            }],
            images: Vec::new(),
            image_chips: Vec::new(),
            ..first.clone()
        };

        assert_eq!(
            collapse_consecutive_composer_entries(vec![
                first.clone(),
                second.clone(),
                second.clone(),
            ]),
            vec![first, second]
        );
    }

    fn node(
        kind: NodeKind,
        parent_id: Option<NodeId>,
        content: Value,
        active: bool,
    ) -> HistoryNode {
        HistoryNode {
            id: NodeId::new(),
            parent_id,
            turn_id: None,
            owner_id: None,
            request_index: None,
            kind,
            status: "completed".into(),
            role: None,
            summary: None,
            content,
            composer_text: None,
            created_at: "2026-08-11T00:00:00Z".into(),
            completed_at: Some("2026-08-11T00:00:01Z".into()),
            active,
        }
    }

    #[test]
    fn hidden_system_node_after_reset_plan_is_transparent() {
        let root = node(
            NodeKind::ConversationRoot,
            None,
            serde_json::json!({}),
            false,
        );
        let clear = node(
            NodeKind::AcceptedPlan,
            Some(root.id),
            serde_json::json!({"plan_markdown":"", "reset_context":true}),
            false,
        );
        let branch = node(
            NodeKind::System,
            Some(clear.id),
            serde_json::json!({}),
            false,
        );
        let user = node(
            NodeKind::UserMessage,
            Some(branch.id),
            serde_json::json!({"text": "continue"}),
            true,
        );
        let clear_id = clear.id;
        let branch_id = branch.id;
        let user_id = user.id;

        let rows =
            project_history_rows_with_context(&[root, clear, branch, user], Path::new("."), &[]);
        let user_row = rows.iter().find(|row| row.id == user_id).unwrap();
        assert_eq!(rows.len(), 3);
        assert!(!rows.iter().any(|row| row.id == branch_id));
        assert!(rows.iter().all(|row| row.kind != HistoryRowKind::Branch));
        assert_eq!(user_row.parent_id, Some(clear_id));
        assert!(user_row.selectable);
    }

    #[test]
    fn safe_bash_is_an_exploration_row_in_history() {
        let assistant = node(
            NodeKind::AssistantMessage,
            None,
            serde_json::json!({"text": ""}),
            false,
        );
        let mut call = node(
            NodeKind::ToolCall,
            Some(assistant.id),
            serde_json::json!({
                "name": "bash",
                "arguments": {"command": "ls", "wait": true}
            }),
            false,
        );
        call.owner_id = Some(assistant.id);
        let mut result = node(
            NodeKind::ToolResult,
            Some(call.id),
            serde_json::json!({"output": {"output": "SPEC.md\n"}, "is_error": false}),
            true,
        );
        result.owner_id = Some(call.id);
        let call_id = call.id;

        let rows = project_history_rows_with_context(
            &[assistant, call, result],
            Path::new("/workspace"),
            &[],
        );
        let row = rows
            .iter()
            .find(|row| row.kind == HistoryRowKind::Exploration)
            .expect("safe Bash should project as exploration");
        assert_eq!(row.preview, "List .");
        assert_eq!(
            row.transcript_target,
            Some(crate::TranscriptBlockId::derived("tools", call_id))
        );
    }

    #[test]
    fn labelled_user_messages_keep_their_label_in_tree_and_fork_previews() {
        let user = node(
            NodeKind::UserMessage,
            None,
            serde_json::json!({
                "text": "write the implementation plan",
                "display_label": "Plan",
            }),
            true,
        );
        let user_id = user.id;
        let rows = project_history_rows_with_context(&[user], Path::new("."), &[]);
        assert_eq!(
            rows[0].transcript_target,
            Some(crate::TranscriptBlockId::node(user_id))
        );
        assert_eq!(rows[0].kind, HistoryRowKind::User);
        assert_eq!(rows[0].preview, "Plan: write the implementation plan");
    }
}
