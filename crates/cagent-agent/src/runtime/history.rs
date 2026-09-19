#[allow(clippy::wildcard_imports)]
use super::*;

/// Projects durable history into the navigable user/assistant tree in one
/// pass: non-message parents become transparent, the active durable tip maps
/// to its nearest message ancestor, and the result is depth-first ordered.
pub(super) fn project_message_history(history: &[crate::HistoryNode]) -> Vec<crate::HistoryNode> {
    let parents = history
        .iter()
        .map(|node| (node.id, node.parent_id))
        .collect::<HashMap<_, _>>();
    let messages = history
        .iter()
        .filter(|node| {
            matches!(
                node.kind,
                crate::NodeKind::UserMessage | crate::NodeKind::AssistantMessage
            )
        })
        .map(|node| node.id)
        .collect::<HashSet<_>>();
    let nearest_message = |mut node_id: Option<crate::NodeId>| {
        while let Some(id) = node_id {
            if messages.contains(&id) {
                return Some(id);
            }
            node_id = parents.get(&id).copied().flatten();
        }
        None
    };
    let active = history
        .iter()
        .find(|node| node.active)
        .and_then(|node| nearest_message(Some(node.id)));

    let projected = history
        .iter()
        .filter(|node| messages.contains(&node.id))
        .map(|node| {
            let mut node = node.clone();
            node.parent_id = nearest_message(node.parent_id);
            node.active = Some(node.id) == active;
            node
        })
        .collect::<Vec<_>>();
    let mut children = HashMap::<Option<crate::NodeId>, Vec<usize>>::new();
    for (index, node) in projected.iter().enumerate() {
        children.entry(node.parent_id).or_default().push(index);
    }
    let mut stack = children.get(&None).cloned().unwrap_or_default();
    stack.reverse();
    let mut ordered = Vec::with_capacity(projected.len());
    while let Some(index) = stack.pop() {
        let node = projected[index].clone();
        if let Some(descendants) = children.get(&Some(node.id)) {
            stack.extend(descendants.iter().rev());
        }
        ordered.push(node);
    }
    ordered
}

#[derive(Default)]
pub(super) struct MarkdownProjection {
    sources: HashMap<crate::NodeId, String>,
}

impl MarkdownProjection {
    pub(super) fn project(&mut self, event: &mut crate::DurableEventKind) {
        match event {
            crate::DurableEventKind::NodeAppended {
                node_id,
                node_kind: crate::NodeKind::AssistantMessage,
                content,
                ..
            } => {
                self.sources.insert(
                    *node_id,
                    content
                        .get("text")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                );
            }
            crate::DurableEventKind::AssistantDelta {
                node_id,
                delta,
                document,
            } => {
                let source = self.sources.entry(*node_id).or_default();
                source.push_str(delta);
                *document = crate::parse_streaming_markdown(source);
            }
            crate::DurableEventKind::PlanStarted { node_id } => {
                self.sources.insert(*node_id, String::new());
            }
            crate::DurableEventKind::PlanDelta {
                node_id,
                delta,
                document,
            } => {
                let source = self.sources.entry(*node_id).or_default();
                source.push_str(delta);
                *document = crate::parse_streaming_markdown(source);
            }
            crate::DurableEventKind::NodeStatusChanged { node_id, status }
                if matches!(
                    status.as_str(),
                    "completed" | "cancelled" | "failed" | "interrupted"
                ) =>
            {
                self.sources.remove(node_id);
            }
            _ => {}
        }
    }
}
