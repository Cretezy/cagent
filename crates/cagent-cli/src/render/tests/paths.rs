//! Semantic path hit-testing behavior.

use super::*;

#[test]
fn semantic_transcript_paths_survive_layout_for_click_hit_testing() {
    let workspace = Path::new("/tmp/project");
    let exploration = crate::render::transcript::tool_group_rows(
        &[ToolActivityGroup::Exploration {
            active: false,
            activities: vec![ExplorationActivity {
                kind: ExplorationActivityKind::Read,
                targets: vec!["src/lib.rs".into()],
                scopes: Vec::new(),
            }],
        }],
        workspace,
        80,
    );
    assert!(exploration.iter().any(|row| {
        row.paths()
            .iter()
            .any(|path| path.target.path == workspace.join("src/lib.rs"))
    }));

    let user =
        crate::app::local_transcript_block(cagent_agent::protocol::TranscriptBlockKind::User {
            label: None,
            text: "Review @src/lib.rs".into(),
            attachments: vec![cagent_agent::presentation::SubmittedAttachment {
                start: 7,
                end: 18,
                spec: cagent_agent::protocol::AttachmentSpec {
                    path: PathBuf::from("src/lib.rs"),
                    start_line: None,
                    end_line: None,
                },
                kind: cagent_agent::presentation::SubmittedAttachmentKind::File,
            }],
            images: Vec::new(),
            image_chips: Vec::new(),
        });
    let rows = crate::render::transcript::history_item_rows(&user, workspace, 80);
    assert!(rows.iter().any(|row| {
        row.paths()
            .iter()
            .any(|path| path.target.path == workspace.join("src/lib.rs"))
    }));
}
