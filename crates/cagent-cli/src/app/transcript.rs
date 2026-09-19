//! Durable, width-independent transcript data.
//!
//! Semantic transcript blocks are owned by `cagent-agent`. Ratatui stores
//! those blocks directly and adds only width-dependent layout state.

pub(crate) use cagent_agent::protocol::TranscriptBlock;

pub(crate) fn local_transcript_block(
    kind: cagent_agent::protocol::TranscriptBlockKind,
) -> TranscriptBlock {
    TranscriptBlock {
        id: cagent_agent::protocol::TranscriptBlockId::derived(
            "frontend",
            cagent_agent::protocol::NodeId::new(),
        ),
        status: cagent_agent::protocol::TranscriptBlockStatus::Completed,
        kind,
    }
}

#[cfg(test)]
pub(crate) fn test_lines(lines: Vec<ratatui::text::Line<'static>>) -> TranscriptBlock {
    let source = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("  \n");
    local_transcript_block(cagent_agent::protocol::TranscriptBlockKind::Assistant {
        document: cagent_agent::presentation::parse_markdown(&source),
        source,
        message: None,
    })
}

#[cfg(test)]
pub(crate) fn test_tool_groups(
    groups: Vec<cagent_agent::presentation::ToolActivityGroup>,
) -> TranscriptBlock {
    local_transcript_block(cagent_agent::protocol::TranscriptBlockKind::ToolGroups { groups })
}

#[cfg(test)]
pub(crate) fn test_edits(diff: cagent_agent::tools::SemanticDiff) -> TranscriptBlock {
    local_transcript_block(cagent_agent::protocol::TranscriptBlockKind::Edits { diff })
}
