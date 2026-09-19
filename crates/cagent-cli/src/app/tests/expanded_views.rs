use super::interaction_surfaces::{permission_request, session_snapshot_with_interaction};
use super::*;

#[tokio::test]
async fn right_click_closes_only_the_top_expanded_view_in_main_content() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = AgentRuntime::open(cagent_agent::runtime::RuntimeOptions::new(
        temporary.path().join("expanded-right-click.db"),
    ))
    .await
    .unwrap();
    let session = runtime
        .create_session(cagent_agent::runtime::NewSession {
            workspace: temporary.path().to_path_buf(),
        })
        .await
        .unwrap();
    let mut app = App::new(
        temporary.path(),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    app.render_width = 60;
    app.render_height = 24;
    app.files_sidebar.area = Some(ratatui::layout::Rect::new(0, 1, 20, 22));
    app.surfaces.push(Surface::Expanded {
        view: ExpandedView::Compaction {
            summary: "underlying view".into(),
        },
        scroll: 3,
        viewport_rows: 10,
    });
    app.surfaces.push(Surface::Expanded {
        view: ExpandedView::Terminal {
            terminal_id: None,
            command: "build".into(),
            output: String::new(),
            ansi_output: String::new(),
            completion: None,
            started_at: None,
            completed_at: None,
        },
        scroll: 0,
        viewport_rows: 10,
    });
    let mouse = |kind, column, row| {
        Event::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })
    };
    for (column, row) in [(5, 10), (40, 0), (40, 23), (80, 10)] {
        app.handle_terminal_event(
            &session,
            mouse(MouseEventKind::Down(MouseButton::Right), column, row),
        )
        .await
        .unwrap();
        assert_eq!(app.surfaces.len(), 2);
    }
    app.handle_terminal_event(
        &session,
        mouse(MouseEventKind::Down(MouseButton::Right), 40, 10),
    )
    .await
    .unwrap();
    assert!(matches!(
        app.surfaces.as_slice(),
        [Surface::Expanded {
            view: ExpandedView::Compaction { .. },
            scroll: 3,
            ..
        }]
    ));
    app.handle_terminal_event(
        &session,
        mouse(MouseEventKind::Up(MouseButton::Right), 40, 10),
    )
    .await
    .unwrap();
    assert_eq!(app.surfaces.len(), 1);

    app.set_pending_interaction(permission_request(None));
    app.handle_terminal_event(
        &session,
        mouse(MouseEventKind::Down(MouseButton::Right), 40, 10),
    )
    .await
    .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Permission { .. })
    ));
    app.handle_terminal_event(
        &session,
        mouse(MouseEventKind::Down(MouseButton::Right), 40, 10),
    )
    .await
    .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Permission { .. })
    ));
}

#[test]
fn expanded_headers_share_title_subject_and_detail_formatting() {
    use cagent_agent::presentation::{
        FileHighlighting, FileView, FileViewContent, FullFileDiffView,
    };

    let views = [
        (
            ExpandedView::Compaction {
                summary: String::new(),
            },
            "  Compacted context",
        ),
        (
            ExpandedView::RepositoryDiff {
                diff: Default::default(),
                title: "Repository diff",
            },
            "  Repository diff  0 files",
        ),
        (
            ExpandedView::WebFetch {
                url: "https://example.test".into(),
                redirected_url: Some("https://example.test/docs".into()),
                format: cagent_agent::WebFetchFormat::Text,
                output: String::new(),
            },
            "  Web Fetch  https://example.test → https://example.test/docs",
        ),
        (
            ExpandedView::WebSearch {
                provider: "brave".into(),
                query: "rust docs".into(),
                results: Vec::new(),
            },
            "  Web Search  rust docs · brave · 0 results",
        ),
        (
            ExpandedView::File {
                view: FileView {
                    path: "data.bin".into(),
                    bytes: 12,
                    content: FileViewContent::Binary,
                },
            },
            "  File  data.bin · binary · 12 B",
        ),
        (
            ExpandedView::Diff {
                view: FullFileDiffView {
                    path: "main.rs".into(),
                    language: "rs".into(),
                    lines: Vec::new(),
                    highlighting: FileHighlighting::Eager,
                    added_lines: 2,
                    removed_lines: 1,
                },
            },
            "  Diff  main.rs · +2 -1",
        ),
        (
            ExpandedView::Image {
                metadata: cagent_agent::protocol::ImageAttachment {
                    id: cagent_agent::protocol::ImageAttachmentId::new(),
                    number: 1,
                    sha256: String::new(),
                    mime_type: "image/png".into(),
                    width: 16,
                    height: 8,
                    size_bytes: 100,
                    blob_id: cagent_agent::protocol::BlobId::new(),
                },
                png: Vec::new(),
            },
            "  Image  #1 · PNG · 16×8 · 100 B",
        ),
        (
            ExpandedView::Directory {
                browser: crate::app::file_tree::DirectoryBrowserState::open(".".into()).unwrap(),
            },
            "  Directory  . · 0 entries",
        ),
    ];
    for (view, expected) in views {
        let surface = Surface::Expanded {
            view,
            scroll: 0,
            viewport_rows: 20,
        };
        let lines = surface_lines(&surface, Path::new("/workspace"), 100);
        assert_eq!(lines[0].to_string(), expected);
        assert_eq!(lines[0].spans[1].style, MENU_TITLE_STYLE);
        assert!(lines[1].to_string().is_empty());
        for span in &lines[0].spans {
            if span.content == " · " {
                assert_eq!(span.style, DIM_STYLE);
            }
            if span.content == "+2" {
                assert_eq!(span.style, crate::render::DIFF_ADDITION_STYLE);
            }
            if span.content == "-1" {
                assert_eq!(span.style, crate::render::DIFF_DELETION_STYLE);
            }
        }
        for width in [0, 1, 8, 24] {
            let lines = surface_lines(&surface, Path::new("/workspace"), width);
            assert!(lines[0].width() <= usize::from(width));
        }
    }
}

#[test]
fn expanded_mcp_headers_use_lowercase_status_and_adjacent_duration() {
    use cagent_agent::presentation::{McpCall, ToolActivityStatus};
    for (status, duration_millis, expected) in [
        (ToolActivityStatus::Pending, None, "running"),
        (
            ToolActivityStatus::Succeeded,
            Some(61_000),
            "completed 1m 01s",
        ),
        (ToolActivityStatus::Failed, Some(125), "failed 125ms"),
    ] {
        let surface = Surface::Expanded {
            view: ExpandedView::Mcp {
                call: std::sync::Arc::new(McpCall {
                    node_id: None,
                    server: "github".into(),
                    tool: "release".into(),
                    status,
                    duration_millis,
                    parameters: serde_json::json!({}),
                    output: None,
                }),
            },
            scroll: 0,
            viewport_rows: 20,
        };
        let lines = surface_lines(&surface, Path::new("/workspace"), 80);
        assert_eq!(
            lines[0].to_string(),
            format!("  MCP  github/release · {expected}")
        );
        assert_eq!(lines[0].spans.last().unwrap().style, DIM_STYLE);
        assert!(lines[1].to_string().is_empty());
    }
}

#[test]
fn mcp_detail_refresh_preserves_scroll_and_ignores_stale_pending_loads() {
    use cagent_agent::presentation::{McpCall, ToolActivityStatus};
    let mut app = App::new(
        Path::new("/workspace"),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    let pending = McpCall {
        node_id: Some(cagent_agent::protocol::NodeId::new()),
        server: "github".into(),
        tool: "release".into(),
        status: ToolActivityStatus::Pending,
        duration_millis: None,
        parameters: serde_json::json!({}),
        output: None,
    };
    app.surfaces.push(Surface::Expanded {
        view: ExpandedView::Mcp {
            call: std::sync::Arc::new(pending.clone()),
        },
        scroll: 3,
        viewport_rows: 20,
    });
    let mut completed = pending.clone();
    completed.status = ToolActivityStatus::Succeeded;
    completed.output = Some(serde_json::json!({"structured_content": {"release": 2}}));
    app.apply_mcp_detail(completed.clone());
    app.apply_mcp_detail(pending);
    let Some(Surface::Expanded {
        view: ExpandedView::Mcp { call },
        scroll,
        ..
    }) = app.surfaces.last()
    else {
        panic!("missing detail")
    };
    assert_eq!(call.as_ref(), &completed);
    assert_eq!(*scroll, 3);
    app.surfaces.clear();
    app.apply_mcp_detail(completed);
    assert!(
        app.surfaces.is_empty(),
        "late loads must not reopen a closed view"
    );
}

#[test]
fn right_click_target_closes_an_expanded_activity_block_without_reopening_it() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    let target = crate::markdown::ActivityCollapseTarget::Transcript(
        cagent_agent::protocol::TranscriptBlockId("activity".into()),
    );
    app.expanded_activity_runs.insert(target.clone());
    app.activity_collapse_hits.push(ActivityCollapseHit {
        row: 4,
        target: target.clone(),
    });

    assert!(app.close_activity_collapse_at(4));
    assert!(!app.expanded_activity_runs.contains(&target));
    assert!(!app.close_activity_collapse_at(4));
}

#[test]
fn clicking_an_exploration_toggle_inside_expanded_activity_keeps_outer_activity_open() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    let block_id = cagent_agent::protocol::TranscriptBlockId::derived(
        "tools",
        cagent_agent::protocol::NodeId::new(),
    );
    let activity_target = crate::markdown::ActivityCollapseTarget::Transcript(block_id.clone());
    let exploration_target = crate::markdown::ExplorationToggleTarget::Transcript {
        block_id,
        group_index: 0,
    };
    app.expanded_activity_runs.insert(activity_target.clone());
    app.activity_collapse_hits.push(ActivityCollapseHit {
        row: 4,
        target: activity_target.clone(),
    });
    app.exploration_toggle_hits.push(ExplorationToggleHit {
        row: 4,
        target: exploration_target.clone(),
    });

    app.select_mouse_at(80, 24, 4, 8);

    assert!(app.expanded_activity_runs.contains(&activity_target));
    assert!(app.expanded_explorations.contains(&exploration_target));
}

#[test]
fn clicking_a_subagent_prompt_opens_its_expanded_log() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    let run = cagent_agent::protocol::AgentRun {
        id: cagent_agent::protocol::AgentRunId::new(),
        conversation_id: cagent_agent::protocol::ConversationId::new(),
        parent_turn_id: cagent_agent::protocol::TurnId::new(),
        sequence: 0,
        profile: "explore".into(),
        model: cagent_agent::provider::ModelRef::parse("mock/echo").unwrap(),
        effort: None,
        task: "Inspect the codebase".into(),
        status: cagent_agent::protocol::AgentRunStatus::Running,
        result: None,
        error: None,
        usage: None,
        created_at: "2026-08-11T00:00:00Z".into(),
        started_at: None,
        completed_at: None,
        timeline: Vec::new(),
        activity: Vec::new(),
    };
    app.agent_log_hits.push(AgentLogHit {
        row: 4,
        run_id: run.id,
    });
    app.supervised_work
        .rows
        .push(cagent_agent::presentation::SupervisedWork::Agent { run: Box::new(run) });

    app.select_mouse_at(80, 24, 4, 8);

    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Expanded {
            view: ExpandedView::AgentLog { .. },
            ..
        })
    ));
}

#[test]
fn clicking_a_compacted_divider_opens_its_markdown_summary() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.compaction_hits.push(CompactionHit {
        row: 4,
        summary: "# Carried context\n\n- Keep the API stable".into(),
    });

    app.select_mouse_at(80, 24, 4, 8);

    let Some(
        surface @ Surface::Expanded {
            view: ExpandedView::Compaction { summary },
            ..
        },
    ) = app.surfaces.last()
    else {
        panic!("compaction view should be open");
    };
    assert_eq!(summary, "# Carried context\n\n- Keep the API stable");
    let rendered = surface_lines(surface, &app.workspace, 80)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("Compacted context"));
    assert!(rendered.contains("Carried context"));
    assert!(rendered.contains("Keep the API stable"));
}

#[test]
fn clicking_transcript_exploration_toggle_expands_inline() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    let block_id = cagent_agent::protocol::TranscriptBlockId::derived(
        "tools",
        cagent_agent::protocol::NodeId::new(),
    );
    let target = crate::markdown::ExplorationToggleTarget::Transcript {
        block_id: block_id.clone(),
        group_index: 1,
    };
    app.exploration_toggle_hits.push(ExplorationToggleHit {
        row: 4,
        target: target.clone(),
    });
    app.history_block_layouts.insert((block_id, 80), Vec::new());

    app.select_mouse_at(80, 24, 4, 8);

    assert!(app.surfaces.is_empty());
    assert!(app.expanded_explorations.contains(&target));
    assert!(app.history_block_layouts.is_empty());

    app.select_mouse_at(80, 24, 4, 8);
    assert!(!app.expanded_explorations.contains(&target));
}

#[test]
fn clicking_subagent_exploration_toggle_expands_inside_log() {
    let run_id = cagent_agent::protocol::AgentRunId::new();
    let run = cagent_agent::protocol::AgentRun {
        id: run_id,
        conversation_id: cagent_agent::protocol::ConversationId::new(),
        parent_turn_id: cagent_agent::protocol::TurnId::new(),
        sequence: 0,
        profile: "explore".into(),
        model: cagent_agent::provider::ModelRef::parse("mock/echo").unwrap(),
        effort: None,
        task: "Inspect the codebase".into(),
        status: cagent_agent::protocol::AgentRunStatus::Running,
        result: None,
        error: None,
        usage: None,
        created_at: "0".into(),
        started_at: Some("0".into()),
        completed_at: None,
        timeline: Vec::new(),
        activity: Vec::new(),
    };
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    app.surfaces.push(Surface::Expanded {
        view: ExpandedView::AgentLog {
            run: Box::new(run),
            streaming: String::new(),
            terminals: Vec::new(),
            expanded_explorations: Default::default(),
            expanded_activity_runs: Default::default(),
            collapse_tool_activity: false,
            max_scroll: Cell::new(Some(0)),
        },
        scroll: 0,
        viewport_rows: 18,
    });
    app.exploration_toggle_hits.push(ExplorationToggleHit {
        row: 4,
        target: crate::markdown::ExplorationToggleTarget::AgentLog {
            run_id,
            group_index: 2,
        },
    });

    app.select_mouse_at(80, 24, 4, 8);

    assert!(
        app.expanded_text_render_cache.borrow().is_some(),
        "scroll metrics retain the layout for the next render"
    );
    assert!(matches!(
        app.surfaces.as_slice(),
        [Surface::Expanded {
            view: ExpandedView::AgentLog { expanded_explorations, .. },
            ..
        }] if expanded_explorations.contains(&2)
    ));
}

#[test]
fn clicking_a_delegated_bash_group_opens_the_clicked_command() {
    let terminal_id = cagent_agent::tools::TerminalId::new();
    let run = cagent_agent::protocol::AgentRun {
        id: cagent_agent::protocol::AgentRunId::new(),
        conversation_id: cagent_agent::protocol::ConversationId::new(),
        parent_turn_id: cagent_agent::protocol::TurnId::new(),
        sequence: 0,
        profile: "explore".into(),
        model: cagent_agent::provider::ModelRef::parse("mock/echo").unwrap(),
        effort: None,
        task: "Run two commands".into(),
        status: cagent_agent::protocol::AgentRunStatus::Completed,
        result: None,
        error: None,
        usage: None,
        created_at: "0".into(),
        started_at: Some("0".into()),
        completed_at: Some("2".into()),
        timeline: Vec::new(),
        activity: vec![
            cagent_agent::protocol::AgentRunActivity {
                sequence: 1,
                tool: "bash".into(),
                arguments: serde_json::json!({"command": "echo first", "wait": true}),
                output: serde_json::json!({
                    "id": cagent_agent::tools::TerminalId::new(),
                    "output": "first\n",
                    "exit_code": 0
                }),
                is_error: false,
                permission_audit: None,
                created_at: "1".into(),
            },
            cagent_agent::protocol::AgentRunActivity {
                sequence: 2,
                tool: "bash".into(),
                arguments: serde_json::json!({"command": "echo second", "wait": true}),
                output: serde_json::json!({
                    "id": terminal_id,
                    "output": "second\n",
                    "exit_code": 0
                }),
                is_error: false,
                permission_audit: None,
                created_at: "2".into(),
            },
        ],
    };
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.render_width = 80;
    app.render_height = 24;
    app.surfaces.push(Surface::Expanded {
        view: ExpandedView::AgentLog {
            run: Box::new(run),
            streaming: String::new(),
            terminals: Vec::new(),
            expanded_explorations: Default::default(),
            expanded_activity_runs: Default::default(),
            collapse_tool_activity: false,
            max_scroll: Cell::new(None),
        },
        scroll: 0,
        viewport_rows: 18,
    });
    let lines = surface_lines(app.surfaces.last().unwrap(), &app.workspace, 80);
    let second_row = lines
        .iter()
        .position(|line| line.to_string().contains("Ran echo second"))
        .expect("second Bash group should be rendered");

    app.select_mouse_at(80, 24, u16::try_from(second_row + 2).unwrap(), 4);

    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Expanded {
            view: ExpandedView::Terminal { command, terminal_id: Some(id), .. },
            ..
        }) if command == "echo second" && *id == terminal_id
    ));
}

#[test]
fn expanded_subagent_hits_exclude_the_status_row() {
    let run = cagent_agent::protocol::AgentRun {
        id: cagent_agent::protocol::AgentRunId::new(),
        conversation_id: cagent_agent::protocol::ConversationId::new(),
        parent_turn_id: cagent_agent::protocol::TurnId::new(),
        sequence: 0,
        profile: "explore".into(),
        model: cagent_agent::provider::ModelRef::parse("mock/echo").unwrap(),
        effort: None,
        task: "Run a command".into(),
        status: cagent_agent::protocol::AgentRunStatus::Completed,
        result: None,
        error: None,
        usage: None,
        created_at: "0".into(),
        started_at: Some("0".into()),
        completed_at: Some("1".into()),
        timeline: Vec::new(),
        activity: vec![cagent_agent::protocol::AgentRunActivity {
            sequence: 1,
            tool: "bash".into(),
            arguments: serde_json::json!({"command": "echo status", "wait": true}),
            output: serde_json::json!({"output": "status\n", "exit_code": 0}),
            is_error: false,
            permission_audit: None,
            created_at: "1".into(),
        }],
    };
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    app.render_width = 80;
    app.render_height = 24;
    app.surfaces.push(Surface::Expanded {
        view: ExpandedView::AgentLog {
            run: Box::new(run),
            streaming: String::new(),
            terminals: Vec::new(),
            expanded_explorations: Default::default(),
            expanded_activity_runs: Default::default(),
            collapse_tool_activity: false,
            max_scroll: Cell::new(None),
        },
        scroll: 0,
        viewport_rows: 18,
    });

    assert!(app.delegated_bash_hit(23, 24).is_none());
    assert!(!app.open_agent_web_search_results_at(23, 24));
}

#[test]
fn expanded_terminal_output_keeps_raw_control_data_out_of_surface_lines() {
    let surface = Surface::Expanded {
        view: ExpandedView::Terminal {
            terminal_id: Some(cagent_agent::tools::TerminalId::new()),
            command: "cargo test".into(),
            output: "red".into(),
            ansi_output: "\x1b[31mred\x1b[0m\x1b]0;unsafe title\x07".into(),
            completion: None,
            started_at: None,
            completed_at: None,
        },
        scroll: 0,
        viewport_rows: 20,
    };
    let lines = surface_lines(&surface, Path::new("/tmp/project"), 80);
    assert_eq!(lines.len(), 2);
    assert!(
        !lines
            .iter()
            .map(ToString::to_string)
            .any(|line| line.contains('\x1b'))
    );
}

#[test]
fn expanded_terminal_header_reports_completed_and_failed_statuses() {
    let surface = |completion: &str| Surface::Expanded {
        view: ExpandedView::Terminal {
            terminal_id: None,
            command: "cargo test".into(),
            output: String::new(),
            ansi_output: String::new(),
            completion: Some(completion.into()),
            started_at: Some("1000".into()),
            completed_at: Some("62000".into()),
        },
        scroll: 0,
        viewport_rows: 20,
    };
    let completed = surface_lines(
        &surface("[exited with status 0]"),
        Path::new("/tmp/project"),
        80,
    );
    let failed = surface_lines(
        &surface("[exited with status 1]"),
        Path::new("/tmp/project"),
        80,
    );
    assert_eq!(
        completed[0].to_string(),
        "  Bash output  cargo test · completed 1m 01s"
    );
    assert_eq!(
        failed[0].to_string(),
        "  Bash output  cargo test · failed 1m 01s"
    );
}

#[test]
#[allow(clippy::format_collect)]
fn expanded_terminal_mouse_wheel_controls_vt_scrollback() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    let output = (0..30).fold(String::new(), |mut output, line| {
        writeln!(output, "line {line}\r").unwrap();
        output
    });
    app.surfaces.push(Surface::Expanded {
        view: ExpandedView::Terminal {
            terminal_id: Some(cagent_agent::tools::TerminalId::new()),
            command: "build".into(),
            output: output.clone(),
            ansi_output: output,
            completion: None,
            started_at: None,
            completed_at: None,
        },
        scroll: 0,
        viewport_rows: 4,
    });
    assert!(app.scroll_expanded_mouse(MouseEventKind::ScrollDown));
    let Surface::Expanded { scroll, .. } = app.surfaces.last().unwrap() else {
        unreachable!();
    };
    assert!(*scroll > 0);
    assert!(app.scroll_expanded_mouse(MouseEventKind::ScrollUp));
    let Surface::Expanded { scroll, .. } = app.surfaces.last().unwrap() else {
        unreachable!();
    };
    assert_eq!(*scroll, 0);
}

#[test]
fn page_and_boundary_navigation_use_shared_list_and_scroll_sizes() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.surfaces.push(Surface::Profiles {
        kind: ProfileKind::Agent,
        rows: (0..20)
            .map(|index| (format!("agent-{index}"), String::new(), true))
            .collect(),
        list: ListState::selectable(20),
    });
    assert_eq!(
        app.apply_current_surface_list_action(ListAction::PageNext),
        Some(true)
    );
    let Some(Surface::Profiles { list, .. }) = app.surfaces.last() else {
        unreachable!();
    };
    assert!(list.selected.unwrap_or(0) > 1);

    app.surfaces.clear();
    let output: String = (0..80).map(|line| format!("line {line}\r\n")).collect();
    app.surfaces.push(Surface::Expanded {
        view: ExpandedView::Terminal {
            terminal_id: None,
            command: "build".into(),
            output: output.clone(),
            ansi_output: output,
            completion: None,
            started_at: None,
            completed_at: None,
        },
        scroll: 0,
        viewport_rows: 18,
    });
    assert!(app.apply_current_surface_scroll_action(ScrollViewAction::PageNext, 80, 24,));
    let Some(Surface::Expanded { scroll, .. }) = app.surfaces.last() else {
        unreachable!();
    };
    assert!(*scroll > 1);
    assert!(app.apply_current_surface_scroll_action(ScrollViewAction::End, 80, 24));
    let Some(Surface::Expanded { scroll, .. }) = app.surfaces.last() else {
        unreachable!();
    };
    let bottom = *scroll;
    assert!(app.apply_current_surface_scroll_action(ScrollViewAction::PageNext, 80, 24,));
    let Some(Surface::Expanded { scroll, .. }) = app.surfaces.last() else {
        unreachable!();
    };
    assert_eq!(*scroll, bottom);
}

#[test]
fn multiline_scroll_arrows_and_wheel_move_the_caret() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    let width = 40;
    let height = 10;
    app.render_width = width;
    app.render_height = height;
    let source = (0..12)
        .map(|line| format!("line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    app.surfaces.push(Surface::McpJsonEdit {
        location: cagent_agent::mcp::McpLocation::global(),
        separate_name: None,
        editor: MultilineInput::new(source, 0),
    });

    let controls_height = app.controls_height(width, height);
    let (_, _, composer_height, _) = app.control_heights_within(width, height);
    let panel_top = height
        .saturating_sub(controls_height)
        .saturating_add(height.saturating_sub(1).min(2));
    let layout = super::surfaces::surface_layout_with_viewport(
        app.surfaces.last().unwrap(),
        &app.workspace,
        width,
        usize::from(composer_height),
    );
    let down = layout
        .surface_hits
        .iter()
        .position(|hit| {
            *hit == SurfaceHit::Scroll(ScrollViewHit::Indicator(ScrollViewAction::PageNext))
        })
        .unwrap();
    app.select_mouse_at(width, height, panel_top + u16::try_from(down).unwrap(), 2);
    let Some(Surface::McpJsonEdit { editor, .. }) = app.surfaces.last() else {
        unreachable!();
    };
    assert!(editor.text()[editor.cursor()..].starts_with("line 4"));

    let content = layout
        .surface_hits
        .iter()
        .position(|hit| matches!(hit, SurfaceHit::Scroll(ScrollViewHit::Content(_))))
        .unwrap();
    assert!(app.handle_mouse_scroll_within(
        MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 2,
            row: panel_top + u16::try_from(content).unwrap(),
            modifiers: KeyModifiers::NONE,
        },
        width,
        height,
    ));
    let Some(Surface::McpJsonEdit { editor, .. }) = app.surfaces.last() else {
        unreachable!();
    };
    assert!(editor.text()[editor.cursor()..].starts_with("line 7"));
}

#[test]
fn composer_uses_shared_scroll_padding_arrows_and_visual_row_navigation() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        Some(4),
    );
    let width = 40;
    let height = 12;
    app.render_width = width;
    app.render_height = height;
    app.draft = (0..9)
        .map(|line| format!("line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    app.cursor = 0;

    let (_, _, composer_rows, status_height) = app.control_heights_within(width, height);
    let capacity = usize::from(composer_rows.saturating_sub(1));
    let initial = app.composer_scroll_layout(width, capacity);
    assert_eq!(initial.lines.len(), capacity + 2);
    assert_eq!(initial.hit(0), ScrollViewHit::None);
    assert_eq!(
        initial.hit(initial.lines.len() - 1),
        ScrollViewHit::Indicator(ScrollViewAction::PageNext)
    );

    let controls_top = height.saturating_sub(app.controls_height(width, height));
    let top = controls_top.saturating_add(height.saturating_sub(status_height).min(1));
    let bottom_arrow = top + u16::try_from(initial.lines.len() - 1).unwrap();
    app.select_mouse_at(width, height, bottom_arrow, 2);
    assert!(app.draft[app.cursor..].starts_with("line 4"));

    let paged = app.composer_scroll_layout(width, capacity);
    assert_eq!(
        paged.hit(0),
        ScrollViewHit::Indicator(ScrollViewAction::PagePrevious)
    );
    assert!(matches!(
        paged.hit(paged.lines.len() - 1),
        ScrollViewHit::Indicator(ScrollViewAction::PageNext)
    ));
    assert!(app.handle_mouse_scroll_within(
        MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 3,
            row: top + 1,
            modifiers: KeyModifiers::NONE,
        },
        width,
        height,
    ));
    assert!(app.draft[app.cursor..].starts_with("line 7"));
}

#[test]
fn composer_vertical_navigation_uses_the_current_render_width_after_resize() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.render_height = 24;
    app.draft = "abcdefghijklmnop".into();

    app.render_width = 12;
    app.cursor = 0;
    assert!(app.move_vertical(true));
    assert_eq!(app.cursor, 8);

    app.render_width = 8;
    app.cursor = 0;
    app.preferred_column = None;
    assert!(app.move_vertical(true));
    assert_eq!(app.cursor, 4);
}

#[test]
fn composer_viewport_shares_unicode_cursor_click_scroll_and_chip_geometry() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        Some(2),
    );
    let width = 30;
    let height = 12;
    app.render_width = width;
    app.render_height = height;
    app.insert_text("界x\nbefore\n@TEST.md\nafter🙂");
    let chip_start = "界x\nbefore\n".len();
    let chip_end = chip_start + "@TEST.md".len();
    app.confirm_attachment(chip_start, chip_end, ListEntryKind::File);

    // A wide grapheme and an explicit newline use the same caret geometry as
    // the visible scroll row and mouse hit map.
    app.cursor = app.draft.len();
    let (_, _, composer_rows, status_height) = app.control_heights_within(width, height);
    let capacity = usize::from(composer_rows.saturating_sub(1));
    let viewport = app.composer_viewport_layout(width, capacity);
    let (caret_row, caret_column) = viewport.caret.unwrap();
    assert!(viewport.state.offset > 0);
    let area = ratatui::layout::Rect::new(0, 0, width, composer_rows);
    assert_eq!(
        viewport.cursor_position(area),
        Some(ratatui::layout::Position::new(
            u16::try_from(caret_column + 2).unwrap(),
            u16::try_from(caret_row - viewport.state.offset).unwrap(),
        ))
    );

    let source_row = caret_row;
    let layout_row = viewport
        .scroll
        .hits
        .iter()
        .position(|hit| *hit == ScrollViewHit::Content(source_row))
        .unwrap();
    let rendered_offset = viewport.rendered_offset_at(source_row, 2).unwrap();
    let expected_source = viewport.source_offset_at(rendered_offset);
    let controls_top = height.saturating_sub(app.controls_height(width, height));
    let sticky_rows = height.saturating_sub(status_height).min(2);
    let screen_row = controls_top + sticky_rows + u16::try_from(layout_row).unwrap() - 1;
    app.select_mouse_at(width, height, screen_row, 4);
    assert_eq!(app.cursor, expected_source);

    // Re-focus the collapsed attachment and click its rendered label. The
    // viewport's substitution mapping must reopen the source-owned chip.
    app.cursor = chip_start;
    app.preferred_column = None;
    let viewport = app.composer_viewport_layout(width, capacity);
    let label_offset = viewport.rendered.find("[File · TEST.md]").unwrap() + 2;
    let chip_row = viewport
        .ranges
        .iter()
        .position(|(start, end)| (*start..=*end).contains(&label_offset))
        .unwrap();
    let chip_column = viewport.rendered[viewport.ranges[chip_row].0..label_offset].width();
    let layout_row = viewport
        .scroll
        .hits
        .iter()
        .position(|hit| *hit == ScrollViewHit::Content(chip_row))
        .unwrap();
    let screen_row = controls_top + sticky_rows + u16::try_from(layout_row).unwrap() - 1;
    app.select_mouse_at(
        width,
        height,
        screen_row,
        u16::try_from(chip_column + 2).unwrap(),
    );
    assert!(app.attachment_completion.is_some());
    assert_eq!(app.cursor, chip_end);
}

#[test]
fn clicking_expanded_output_scroll_arrows_jumps_to_each_end() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    let output: String = (0..30).map(|line| format!("line {line}\n")).collect();
    app.surfaces.push(Surface::Expanded {
        view: ExpandedView::Terminal {
            terminal_id: None,
            command: "build".into(),
            output: output.clone(),
            ansi_output: output,
            completion: None,
            started_at: None,
            completed_at: None,
        },
        scroll: usize::MAX,
        viewport_rows: 18,
    });

    // Two shared sticky rows precede the expanded panel; its top-arrow
    // padding row is therefore screen row 3.
    app.select_mouse_at(80, 24, 3, 2);
    let Surface::Expanded { scroll, .. } = app.surfaces.last().unwrap() else {
        unreachable!();
    };
    assert_eq!(*scroll, 0);

    app.select_mouse_at(80, 24, 22, 2);
    let Surface::Expanded { scroll, .. } = app.surfaces.last().unwrap() else {
        unreachable!();
    };
    assert!(*scroll > 0);
}

#[test]
fn clicking_expanded_text_output_scroll_arrows_uses_the_cached_hit_test() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    let output: String = (0..30).map(|line| format!("line {line}\n")).collect();
    app.surfaces.push(Surface::Expanded {
        view: ExpandedView::WebFetch {
            url: "https://example.test".into(),
            redirected_url: None,
            format: cagent_agent::WebFetchFormat::Text,
            output,
        },
        scroll: usize::MAX,
        viewport_rows: 18,
    });

    // The Web Fetch heading occupies the first row and the top indicator is
    // immediately below it, matching the terminal expanded view geometry.
    assert!(app.click_expanded_output_indicator(80, 24, 3));
    let Surface::Expanded { scroll, .. } = app.surfaces.last().unwrap() else {
        unreachable!();
    };
    assert_eq!(*scroll, 0);
    assert!(app.expanded_text_render_cache.borrow().is_some());

    assert!(app.click_expanded_output_indicator(80, 24, 22));
    let Surface::Expanded { scroll, .. } = app.surfaces.last().unwrap() else {
        unreachable!();
    };
    assert!(*scroll > 0);
}

#[test]
fn subagent_log_scroll_stops_at_the_last_content_row() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.render_width = 80;
    app.render_height = 24;
    let run = cagent_agent::protocol::AgentRun {
        id: cagent_agent::protocol::AgentRunId::new(),
        conversation_id: cagent_agent::protocol::ConversationId::new(),
        parent_turn_id: cagent_agent::protocol::TurnId::new(),
        sequence: 0,
        profile: "explore".into(),
        model: cagent_agent::provider::ModelRef::parse("mock/echo").unwrap(),
        effort: None,
        task: "Inspect".into(),
        status: cagent_agent::protocol::AgentRunStatus::Failed,
        result: None,
        error: Some(
            "The delegated task failed after receiving a response that exceeds the panel width"
                .into(),
        ),
        usage: Some(cagent_agent::provider::ModelUsage {
            input_tokens: Some(1_000),
            output_tokens: Some(250),
            total_tokens: Some(1_250),
            ..Default::default()
        }),
        created_at: "0".into(),
        started_at: Some("0".into()),
        completed_at: None,
        timeline: Vec::new(),
        activity: Vec::new(),
    };
    app.surfaces.push(Surface::Expanded {
        view: ExpandedView::AgentLog {
            run: Box::new(run),
            streaming: (0..20).map(|line| format!("line {line}\n\n")).collect(),
            terminals: Vec::new(),
            expanded_explorations: Default::default(),
            expanded_activity_runs: Default::default(),
            collapse_tool_activity: false,
            max_scroll: Cell::new(None),
        },
        scroll: 0,
        viewport_rows: 4,
    });
    let (_, _, composer, _) = app.control_lines_with_draft_limit(80, 20);
    assert_eq!(
        composer.last().map(ToString::to_string).as_deref(),
        Some("  ↓")
    );
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Expanded {
            view: ExpandedView::AgentLog { max_scroll, .. },
            ..
        }) if max_scroll.get().is_some()
    ));
    let expected_maximum = match app.surfaces.last().unwrap() {
        Surface::Expanded {
            view,
            viewport_rows,
            ..
        } => expanded_scroll_metrics(view, *viewport_rows, &app.workspace, app.render_width, 20).1,
        _ => unreachable!(),
    };
    for _ in 0..20 {
        assert!(app.scroll_expanded_mouse(MouseEventKind::ScrollDown));
    }
    let Surface::Expanded { scroll, .. } = app.surfaces.last().unwrap() else {
        unreachable!();
    };
    assert_eq!(*scroll, expected_maximum);
    assert_eq!(
        surface_status(app.surfaces.last().unwrap()),
        "↑/↓ scroll · PgUp/PgDn page · Home/End jump · Enter/Esc close"
    );
    assert!(
        surface_lines_with_viewport(
            app.surfaces.last().unwrap(),
            Path::new("/tmp/project"),
            80,
            20
        )
        .iter()
        .all(|line| line.to_string() != "  ↓")
    );
    assert!(
        surface_lines_with_viewport(
            app.surfaces.last().unwrap(),
            Path::new("/tmp/project"),
            80,
            20
        )
        .iter()
        .any(|line| line.to_string().contains("in:1k out:250"))
    );

    let top = Surface::Expanded {
        view: ExpandedView::AgentLog {
            run: app
                .surfaces
                .last()
                .and_then(|surface| match surface {
                    Surface::Expanded {
                        view: ExpandedView::AgentLog { run, .. },
                        ..
                    } => Some(run.clone()),
                    _ => None,
                })
                .unwrap(),
            streaming: (0..20).map(|line| format!("line {line}\n\n")).collect(),
            terminals: Vec::new(),
            expanded_explorations: Default::default(),
            expanded_activity_runs: Default::default(),
            collapse_tool_activity: false,
            max_scroll: Cell::new(None),
        },
        scroll: 0,
        viewport_rows: 4,
    };
    let top_lines = surface_lines_with_viewport(&top, Path::new("/tmp/project"), 80, 8);
    assert_eq!(
        top_lines.last().map(ToString::to_string).as_deref(),
        Some("  ↓")
    );
}

#[test]
fn subagent_log_formats_header_and_wraps_the_task_inside_its_inset() {
    let run = cagent_agent::protocol::AgentRun {
        id: cagent_agent::protocol::AgentRunId::new(),
        conversation_id: cagent_agent::protocol::ConversationId::new(),
        parent_turn_id: cagent_agent::protocol::TurnId::new(),
        sequence: 0,
        profile: "explore".into(),
        model: cagent_agent::provider::ModelRef::parse("mock/echo").unwrap(),
        effort: Some("high".into()),
        task: "Inspect the detailed PHASES.md migration plan before making changes".into(),
        status: cagent_agent::protocol::AgentRunStatus::Running,
        result: None,
        error: None,
        usage: None,
        created_at: "0".into(),
        started_at: Some("0".into()),
        completed_at: None,
        timeline: Vec::new(),
        activity: Vec::new(),
    };
    let mut surface = Surface::Expanded {
        view: ExpandedView::AgentLog {
            run: Box::new(run),
            streaming: String::new(),
            terminals: Vec::new(),
            expanded_explorations: Default::default(),
            expanded_activity_runs: Default::default(),
            collapse_tool_activity: false,
            max_scroll: Cell::new(None),
        },
        scroll: 0,
        viewport_rows: 20,
    };

    let lines = surface_lines(&surface, Path::new("/tmp/project"), 32)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>();
    assert!(
        lines
            .first()
            .is_some_and(|line| line.starts_with("  Sub-agent log  explore · "))
    );
    let task_start = lines
        .iter()
        .position(|line| line.starts_with("  Task  Inspect"))
        .expect("task start");
    assert!(lines[task_start + 1].starts_with("        "));
    assert!(
        lines[task_start..]
            .iter()
            .take_while(|line| !line.is_empty())
            .all(|line| line.width() <= 32)
    );
    assert!(lines.iter().all(|line| line.width() <= 32));

    let bounded = surface_lines_with_viewport(&surface, Path::new("/tmp/project"), 16, 8);
    assert_eq!(bounded.len(), 8);
    assert!(bounded.iter().all(|line| line.width() <= 16));

    for (effort, expected_header) in [
        (
            Some("high"),
            "  Sub-agent log  explore · mock/echo high · running",
        ),
        (None, "  Sub-agent log  explore · mock/echo · running"),
    ] {
        if let Surface::Expanded {
            view: ExpandedView::AgentLog { run, .. },
            ..
        } = &mut surface
        {
            run.effort = effort.map(str::to_owned);
            run.started_at = None;
        }
        let lines = surface_lines(&surface, Path::new("/tmp/project"), 80);
        assert_eq!(lines[0].to_string(), expected_header);
    }
}

#[test]
fn subagent_log_refreshes_elapsed_header_without_usage_or_body_updates() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    let run = cagent_agent::protocol::AgentRun {
        id: cagent_agent::protocol::AgentRunId::new(),
        conversation_id: cagent_agent::protocol::ConversationId::new(),
        parent_turn_id: cagent_agent::protocol::TurnId::new(),
        sequence: 0,
        profile: "general".into(),
        model: cagent_agent::provider::ModelRef::parse("mock/echo").unwrap(),
        effort: Some("high".into()),
        task: "Inspect".into(),
        status: cagent_agent::protocol::AgentRunStatus::Running,
        result: None,
        error: None,
        usage: None,
        created_at: "0".into(),
        // A future start deterministically clamps the live duration to zero.
        started_at: Some(u128::MAX.to_string()),
        completed_at: None,
        timeline: Vec::new(),
        activity: Vec::new(),
    };
    app.surfaces.push(Surface::Expanded {
        view: ExpandedView::AgentLog {
            run: Box::new(run),
            streaming: String::new(),
            terminals: Vec::new(),
            expanded_explorations: Default::default(),
            expanded_activity_runs: Default::default(),
            collapse_tool_activity: false,
            max_scroll: Cell::new(None),
        },
        scroll: 0,
        viewport_rows: 10,
    });
    let cached_lines = |app: &App| {
        let Surface::Expanded { view, .. } = app.surfaces.last().unwrap() else {
            unreachable!();
        };
        surface_expanded::cached_expanded_surface_lines(
            &app.expanded_text_render_cache,
            view,
            0,
            10,
            Path::new("/tmp/project"),
            80,
            10,
            false,
        )
    };

    assert!(!app.active);
    assert!(app.bash_activity_animation_visible());
    assert_eq!(
        cached_lines(&app)[0].to_string(),
        "  Sub-agent log  general · mock/echo high · running 0s"
    );
    app.invalidate_activity_layout();
    assert!(app.expanded_text_render_cache.borrow().is_some());

    let Surface::Expanded {
        view: ExpandedView::AgentLog { run, .. },
        ..
    } = app.surfaces.last_mut().unwrap()
    else {
        unreachable!();
    };
    run.status = cagent_agent::protocol::AgentRunStatus::Completed;
    run.started_at = Some("1000".into());
    run.completed_at = Some("62000".into());
    assert!(!app.bash_activity_animation_visible());
    for _ in 0..2 {
        // The same cached body is reused, but the heading must not be stale.
        let lines = cached_lines(&app);
        assert_eq!(
            lines[0].to_string(),
            "  Sub-agent log  general · mock/echo high · completed 1m 01s"
        );
        assert!(!lines.iter().any(|line| line.to_string().contains("Usage")));
    }
}

#[test]
fn subagent_log_renders_assistant_markdown_like_the_primary_log() {
    let run = cagent_agent::protocol::AgentRun {
        id: cagent_agent::protocol::AgentRunId::new(),
        conversation_id: cagent_agent::protocol::ConversationId::new(),
        parent_turn_id: cagent_agent::protocol::TurnId::new(),
        sequence: 0,
        profile: "explore".into(),
        model: cagent_agent::provider::ModelRef::parse("mock/echo").unwrap(),
        effort: None,
        task: "Read the specification".into(),
        status: cagent_agent::protocol::AgentRunStatus::Completed,
        result: None,
        error: None,
        usage: None,
        created_at: "0".into(),
        started_at: Some("0".into()),
        completed_at: Some("1".into()),
        timeline: vec![cagent_agent::protocol::AgentRunTimelineEntry::Assistant {
            sequence: 1,
            text: "Explored\n- Read `SPEC.md`\n\n# SPEC.md Summary".into(),
            created_at: "1".into(),
        }],
        activity: Vec::new(),
    };
    let surface = Surface::Expanded {
        view: ExpandedView::AgentLog {
            run: Box::new(run),
            streaming: String::new(),
            terminals: Vec::new(),
            expanded_explorations: Default::default(),
            expanded_activity_runs: Default::default(),
            collapse_tool_activity: false,
            max_scroll: Cell::new(None),
        },
        scroll: 0,
        viewport_rows: 20,
    };

    let rendered = surface_lines(&surface, Path::new("/tmp/project"), 80);
    let lines = rendered.iter().map(ToString::to_string).collect::<Vec<_>>();
    assert!(lines.contains(&"• Explored".into()));
    assert!(lines.contains(&"  • Read SPEC.md".into()));
    let heading = lines
        .iter()
        .position(|line| line == "  # SPEC.md Summary")
        .unwrap();
    assert!(
        rendered[heading]
            .spans
            .iter()
            .any(|span| span.style.add_modifier.contains(Modifier::UNDERLINED))
    );
}

#[test]
fn subagent_log_separates_completed_entries_like_the_primary_log() {
    let run = cagent_agent::protocol::AgentRun {
        id: cagent_agent::protocol::AgentRunId::new(),
        conversation_id: cagent_agent::protocol::ConversationId::new(),
        parent_turn_id: cagent_agent::protocol::TurnId::new(),
        sequence: 0,
        profile: "explore".into(),
        model: cagent_agent::provider::ModelRef::parse("mock/echo").unwrap(),
        effort: None,
        task: "Inspect the specification".into(),
        status: cagent_agent::protocol::AgentRunStatus::Completed,
        result: None,
        error: None,
        usage: None,
        created_at: "0".into(),
        started_at: Some("0".into()),
        completed_at: Some("2".into()),
        timeline: vec![
            cagent_agent::protocol::AgentRunTimelineEntry::Assistant {
                sequence: 1,
                text: "I will inspect the specification.".into(),
                created_at: "1".into(),
            },
            cagent_agent::protocol::AgentRunTimelineEntry::Assistant {
                sequence: 2,
                text: "The specification is clear.".into(),
                created_at: "2".into(),
            },
        ],
        activity: Vec::new(),
    };
    let surface = Surface::Expanded {
        view: ExpandedView::AgentLog {
            run: Box::new(run),
            streaming: String::new(),
            terminals: Vec::new(),
            expanded_explorations: Default::default(),
            expanded_activity_runs: Default::default(),
            collapse_tool_activity: false,
            max_scroll: Cell::new(None),
        },
        scroll: 0,
        viewport_rows: 20,
    };

    let lines = surface_lines(&surface, Path::new("/tmp/project"), 80)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>();
    let first = lines
        .iter()
        .position(|line| line == "• I will inspect the specification.")
        .unwrap();
    assert_eq!(lines[first + 1], "");
    assert_eq!(lines[first + 2], "• The specification is clear.");
}

#[test]
fn subagent_log_separates_consecutive_activity_groups() {
    let run = cagent_agent::protocol::AgentRun {
        id: cagent_agent::protocol::AgentRunId::new(),
        conversation_id: cagent_agent::protocol::ConversationId::new(),
        parent_turn_id: cagent_agent::protocol::TurnId::new(),
        sequence: 0,
        profile: "explore".into(),
        model: cagent_agent::provider::ModelRef::parse("mock/echo").unwrap(),
        effort: None,
        task: "Inspect the project".into(),
        status: cagent_agent::protocol::AgentRunStatus::Completed,
        result: None,
        error: None,
        usage: None,
        created_at: "0".into(),
        started_at: Some("0".into()),
        completed_at: Some("2".into()),
        timeline: vec![
            cagent_agent::protocol::AgentRunTimelineEntry::Tool {
                activity: cagent_agent::protocol::AgentRunActivity {
                    sequence: 1,
                    tool: "grep".into(),
                    arguments: serde_json::json!({"pattern": "needle", "path": "src"}),
                    output: serde_json::json!({"output": "src/lib.rs:needle\n"}),
                    is_error: false,
                    permission_audit: None,
                    created_at: "1".into(),
                },
            },
            cagent_agent::protocol::AgentRunTimelineEntry::Tool {
                activity: cagent_agent::protocol::AgentRunActivity {
                    sequence: 2,
                    tool: "bash".into(),
                    arguments: serde_json::json!({"command": "cargo check", "wait": true}),
                    output: serde_json::json!({"output": "Finished\n", "exit_code": 0}),
                    is_error: false,
                    permission_audit: None,
                    created_at: "2".into(),
                },
            },
        ],
        activity: Vec::new(),
    };
    let surface = Surface::Expanded {
        view: ExpandedView::AgentLog {
            run: Box::new(run),
            streaming: String::new(),
            terminals: Vec::new(),
            expanded_explorations: Default::default(),
            expanded_activity_runs: Default::default(),
            collapse_tool_activity: false,
            max_scroll: Cell::new(None),
        },
        scroll: 0,
        viewport_rows: 20,
    };

    let lines = surface_lines(&surface, Path::new("/tmp/project"), 80)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>();
    let ran = lines
        .iter()
        .position(|line| line.contains("Ran cargo check"))
        .unwrap();
    assert_eq!(lines[ran - 1], "");
    assert!(
        lines[..ran - 1]
            .iter()
            .any(|line| line.contains("Explored"))
    );
}

#[test]
fn subagent_explore_to_streaming_reply_uses_the_full_width_transcript_separator() {
    let run = cagent_agent::protocol::AgentRun {
        id: cagent_agent::protocol::AgentRunId::new(),
        conversation_id: cagent_agent::protocol::ConversationId::new(),
        parent_turn_id: cagent_agent::protocol::TurnId::new(),
        sequence: 0,
        profile: "explore".into(),
        model: cagent_agent::provider::ModelRef::parse("mock/echo").unwrap(),
        effort: None,
        task: "Inspect".into(),
        status: cagent_agent::protocol::AgentRunStatus::Running,
        result: None,
        error: None,
        usage: None,
        created_at: "0".into(),
        started_at: Some("0".into()),
        completed_at: None,
        timeline: vec![cagent_agent::protocol::AgentRunTimelineEntry::Tool {
            activity: cagent_agent::protocol::AgentRunActivity {
                sequence: 1,
                tool: "bash".into(),
                arguments: serde_json::json!({"command": "rg needle src", "wait": true}),
                output: serde_json::json!({"output": "src/lib.rs:needle\n", "exit_code": 0}),
                is_error: false,
                permission_audit: None,
                created_at: "1".into(),
            },
        }],
        activity: Vec::new(),
    };
    let surface = Surface::Expanded {
        view: ExpandedView::AgentLog {
            run: Box::new(run),
            streaming: "Found it.".into(),
            terminals: Vec::new(),
            expanded_explorations: Default::default(),
            expanded_activity_runs: Default::default(),
            collapse_tool_activity: false,
            max_scroll: Cell::new(None),
        },
        scroll: 0,
        viewport_rows: 20,
    };

    let rendered = surface_lines(&surface, Path::new("/tmp/project"), 40);
    let lines = rendered.iter().map(ToString::to_string).collect::<Vec<_>>();
    let reply = lines.iter().position(|line| line == "• Found it.").unwrap();
    assert_eq!(lines[reply - 3], "");
    assert_eq!(lines[reply - 2], "─".repeat(40));
    assert_eq!(rendered[reply - 2].style, DIM_STYLE);
    assert_eq!(lines[reply - 1], "");
    assert!(
        lines[..reply - 3]
            .iter()
            .any(|line| line.contains("needle"))
    );
}

#[test]
fn subagent_log_does_not_claim_it_is_empty_while_bash_is_running() {
    let run = cagent_agent::protocol::AgentRun {
        id: cagent_agent::protocol::AgentRunId::new(),
        conversation_id: cagent_agent::protocol::ConversationId::new(),
        parent_turn_id: cagent_agent::protocol::TurnId::new(),
        sequence: 0,
        profile: "explore".into(),
        model: cagent_agent::provider::ModelRef::parse("mock/echo").unwrap(),
        effort: None,
        task: "Inspect".into(),
        status: cagent_agent::protocol::AgentRunStatus::Running,
        result: None,
        error: None,
        usage: None,
        created_at: "0".into(),
        started_at: Some("0".into()),
        completed_at: None,
        timeline: Vec::new(),
        activity: vec![cagent_agent::protocol::AgentRunActivity {
            sequence: 1,
            tool: "bash".into(),
            arguments: serde_json::json!({"command": "echo running", "wait": false}),
            output: serde_json::json!({"output": "running\n", "status": "running"}),
            is_error: false,
            permission_audit: None,
            created_at: "1".into(),
        }],
    };
    let surface = Surface::Expanded {
        view: ExpandedView::AgentLog {
            run: Box::new(run),
            streaming: String::new(),
            terminals: Vec::new(),
            expanded_explorations: Default::default(),
            expanded_activity_runs: Default::default(),
            collapse_tool_activity: false,
            max_scroll: Cell::new(None),
        },
        scroll: 0,
        viewport_rows: 20,
    };

    let rendered = surface_lines(&surface, Path::new("/tmp/project"), 80)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>();
    assert!(
        rendered
            .iter()
            .any(|line| line.contains("Running echo running"))
    );
    assert!(
        !rendered
            .iter()
            .any(|line| line.contains("No activity yet."))
    );
}

#[test]
fn web_fetch_bottom_arrow_hides_at_the_rendered_bottom() {
    let output: String = (0..30).map(|line| format!("line {line}\n")).collect();
    let top = Surface::Expanded {
        view: ExpandedView::WebFetch {
            url: "https://example.test".into(),
            redirected_url: None,
            format: cagent_agent::WebFetchFormat::Text,
            output: output.clone(),
        },
        scroll: 0,
        viewport_rows: 20,
    };
    let top_lines = surface_lines_with_viewport(&top, Path::new("/tmp/project"), 80, 20);
    assert_eq!(
        top_lines[0].to_string(),
        "  Web Fetch  https://example.test"
    );
    assert_eq!(top_lines[0].spans.last().unwrap().style, Style::default());
    assert_eq!(
        top_lines.last().map(ToString::to_string).as_deref(),
        Some("  ↓")
    );
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.surfaces.push(top);
    let (_, _, composer, _) = app.control_lines_with_draft_limit(80, 20);
    assert_eq!(
        composer.last().map(ToString::to_string).as_deref(),
        Some("  ↓")
    );

    // The panel reserves its title, top-arrow, and bottom-arrow rows, leaving
    // 17 rows of content when it overflows.
    let bottom = Surface::Expanded {
        view: ExpandedView::WebFetch {
            url: "https://example.test".into(),
            redirected_url: None,
            format: cagent_agent::WebFetchFormat::Text,
            output,
        },
        scroll: 13,
        viewport_rows: 20,
    };
    let bottom_lines = surface_lines_with_viewport(&bottom, Path::new("/tmp/project"), 80, 20);
    assert!(bottom_lines.iter().all(|line| line.to_string() != "  ↓"));
    assert_eq!(bottom_lines[1].to_string(), "  ↑");
}

#[test]
fn expanded_web_fetch_wraps_text_and_html_to_the_content_width() {
    let text = "abcdefghijklmnopqrstuvwxyz0123456789";
    for format in [
        cagent_agent::WebFetchFormat::Text,
        cagent_agent::WebFetchFormat::Html,
    ] {
        let rows = web_fetch_content_lines(&format, text, 20);
        assert!(rows.len() > 1, "{format:?} should wrap long source lines");
        assert!(
            rows.iter()
                .all(|row| UnicodeWidthStr::width(row.line.to_string().as_str()) <= 16),
            "{format:?} emitted a row wider than the expanded content region"
        );
    }
}

#[test]
fn mouse_wheel_over_history_scrolls_it_while_a_completion_menu_is_open() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.render_width = 80;
    app.render_height = 24;
    app.history_scroll = 12;
    app.follow_history_tail = false;
    app.replace_draft("/m");
    app.slash_list.select(1, VISIBLE_MENU_ITEMS);
    assert!(!app.slash_suggestions().is_empty());

    assert!(app.handle_mouse_scroll_within(
        MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        },
        80,
        24,
    ));

    assert_eq!(app.history_scroll, 9);
    assert!(!app.follow_history_tail);
    assert_eq!(app.slash_list.selected, Some(1));
}

#[test]
fn permission_surface_identifies_a_subagent_request() {
    let mut request = permission_request(None);
    let run_id = cagent_agent::protocol::AgentRunId::new();
    request.origin = Some(cagent_agent::protocol::InteractionOrigin::SubAgent {
        id: run_id,
        profile: "explore".into(),
    });
    let surface = Surface::Permission {
        request: Box::new(request),
        selected: 0,
        scope: cagent_agent::permissions::PermissionScope::Project,
        diff_scroll: 0,
        denial_note: String::new(),
        editing_note: false,
        note_cursor: 0,
    };
    let rendered = surface_lines(&surface, Path::new("/tmp/project"), 80)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("Permission  explore sub-agent · edit"));
    assert!(rendered.contains(&format!("Requested by  explore sub-agent · {run_id}")));
}

#[test]
fn permission_surface_shows_the_diff_and_persistent_rule_editor() {
    let diff = cagent_agent::tools::SemanticDiff {
        files: vec![cagent_agent::tools::DiffFile {
            old_path: Some("/tmp/project/TEST.md".into()),
            new_path: Some("/tmp/project/TEST.md".into()),
            kind: cagent_agent::tools::DiffFileKind::Modified,
            language: Some("markdown".into()),
            added_lines: 1,
            removed_lines: 1,
            old_no_final_newline: false,
            new_no_final_newline: true,
            hunks: vec![cagent_agent::tools::DiffHunk {
                header: "@@ -1 +1 @@".into(),
                lines: vec![
                    cagent_agent::tools::DiffLine {
                        kind: cagent_agent::tools::DiffLineKind::Deletion,
                        old_line: Some(1),
                        new_line: None,
                        text: "old".into(),
                    },
                    cagent_agent::tools::DiffLine {
                        kind: cagent_agent::tools::DiffLineKind::Addition,
                        old_line: None,
                        new_line: Some(1),
                        text: "new".into(),
                    },
                ],
            }],
        }],
    };
    let request = permission_request(Some(diff));
    let surface = Surface::Permission {
        request: Box::new(request.clone()),
        selected: 1,
        scope: cagent_agent::permissions::PermissionScope::Project,
        diff_scroll: 0,
        denial_note: String::new(),
        editing_note: false,
        note_cursor: 0,
    };
    let lines = surface_lines(&surface, Path::new("/tmp/project"), 80);
    let rendered = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    assert!(!rendered.contains("TEST.md +1 -1"));
    assert!(!rendered.contains("@@ -1 +1 @@"));
    assert!(rendered.contains("1 -old"));
    assert!(rendered.contains("1 +new"));
    assert!(rendered.contains("\\ No newline at end of file"));
    assert!(surface_status(&surface).contains("e edit"));
    assert_eq!(
        lines
            .iter()
            .find(|line| line.to_string().contains("-old"))
            .unwrap()
            .style
            .bg,
        Some(Color::Rgb(74, 34, 29))
    );
    assert_eq!(
        lines
            .iter()
            .find(|line| line.to_string().contains("+new"))
            .unwrap()
            .style
            .bg,
        Some(Color::Rgb(33, 58, 43))
    );
    assert!(
        lines
            .iter()
            .find(|line| line.to_string().contains("+new"))
            .unwrap()
            .width()
            >= 80
    );

    let (rule, cursor, scope) = permission_rule_edit_details(
        &request,
        1,
        cagent_agent::permissions::PermissionScope::Project,
    )
    .unwrap();
    assert_eq!(scope, cagent_agent::permissions::PermissionScope::Project);
    assert_eq!(cursor, "/tmp/project/TEST.md".len());
    let editor = Surface::PermissionRuleEdit {
        request: Box::new(request),
        pattern: rule.editable_pattern().unwrap().0,
        rule,
        cursor,
        scope,
    };
    let editor_lines = surface_lines(&editor, Path::new("/tmp/project"), 80);
    let editor_text = editor_lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(editor_text.contains("Permission rule"));
    assert!(editor_text.contains("/tmp/project/TEST.md"));
    assert!(editor_text.contains("glob patterns: *, **, ?"));
    assert_eq!(
        editor_lines[0].spans.last().unwrap().style,
        MENU_DETAIL_STYLE
    );
    assert_eq!(
        editor_lines
            .last()
            .expect("permission editor lines")
            .width(),
        0
    );
    assert_eq!(
        surface_status(&editor),
        "Enter submit for project · Esc cancel"
    );
}

#[test]
fn bash_permission_editor_describes_command_wildcards() {
    let mut rule = cagent_agent::permissions::PermissionRule {
        id: "bash-echo".into(),
        effect: cagent_agent::permissions::PermissionEffect::Allow,
        tool: None,
        server: None,
        operation: None,
        path: None,
        command: None,
        raw_command: None,
        cwd: None,
        access: None,
        external: false,
        mode: None,
        agent: None,
        source: None,
        created_at: None,
    };
    rule.tool = Some("bash".into());
    rule.command = Some(vec!["echo".into(), "hello".into()]);
    let editor = Surface::PersistentPermissionEdit {
        selected: 0,
        pattern: "echo hello".into(),
        cursor: "echo hello".len(),
        rule,
        scope: cagent_agent::permissions::PermissionScope::Project,
    };

    let rendered = surface_lines(&editor, Path::new("/tmp/project"), 80)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("wildcard: * (for example, echo *)"));
    assert!(!rendered.contains("glob patterns"));
}

#[test]
fn permission_surface_keeps_choices_visible_while_scrolling_long_diffs() {
    let diff = cagent_agent::tools::SemanticDiff {
        files: vec![cagent_agent::tools::DiffFile {
            old_path: Some("/tmp/project/TEST.md".into()),
            new_path: Some("/tmp/project/TEST.md".into()),
            kind: cagent_agent::tools::DiffFileKind::Modified,
            language: Some("markdown".into()),
            added_lines: 10_000,
            removed_lines: 0,
            old_no_final_newline: false,
            new_no_final_newline: false,
            hunks: vec![cagent_agent::tools::DiffHunk {
                header: "@@ -1,10000 +1,10000 @@".into(),
                lines: (0..10_000)
                    .map(|line| cagent_agent::tools::DiffLine {
                        kind: cagent_agent::tools::DiffLineKind::Context,
                        old_line: Some(line + 1),
                        new_line: Some(line + 1),
                        text: format!("diff line {line}"),
                    })
                    .collect(),
            }],
        }],
    };
    let request = permission_request(Some(diff));
    let top = Surface::Permission {
        request: Box::new(request.clone()),
        selected: 0,
        scope: cagent_agent::permissions::PermissionScope::Project,
        diff_scroll: 0,
        denial_note: String::new(),
        editing_note: false,
        note_cursor: 0,
    };
    let top_lines = surface_lines_with_viewport(&top, Path::new("/tmp/project"), 80, 12);
    let top_text = top_lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(top_text.contains("diff line 0"));
    assert!(!top_text.contains("diff line 9999"));
    assert!(!top_text.contains("↑"));
    assert!(top_text.contains("↓"));
    assert!(top_text.contains("Allow"));
    assert_eq!(top_lines.len(), 12);

    let bottom = Surface::Permission {
        request: Box::new(request),
        selected: 0,
        scope: cagent_agent::permissions::PermissionScope::Project,
        diff_scroll: usize::MAX,
        denial_note: String::new(),
        editing_note: false,
        note_cursor: 0,
    };
    let bottom_text = surface_lines_with_viewport(&bottom, Path::new("/tmp/project"), 80, 12)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!bottom_text.contains("diff line 0"));
    assert!(bottom_text.contains("diff line 9999"));
    assert!(bottom_text.contains("↑"));
    assert!(!bottom_text.contains("↓"));
    assert!(bottom_text.contains("Allow"));

    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.render_width = 80;
    app.render_height = 18;
    app.surfaces.push(top);
    let (_, _, composer_height, _) = app.control_heights_within(80, 18);
    assert!(
        18_u16.saturating_sub(app.controls_height(80, 18)) > 0,
        "an oversized permission prompt must leave transcript rows visible"
    );
    assert!(
        24_u16.saturating_sub(app.controls_height(80, 24)) >= 6,
        "a normal-height terminal should retain useful transcript context"
    );
    let render_limit = usize::from(composer_height.saturating_sub(1));
    let (_, _, top_panel, _) = app.control_lines_with_draft_limit(80, render_limit);
    assert_eq!(
        usize::from(composer_height),
        top_panel.len() + 1,
        "scrollable permission prompts leave one blank row below the choices"
    );
    for _ in 0..20 {
        assert!(app.scroll_expanded_mouse(MouseEventKind::ScrollDown));
        let (_, _, composer_height, _) = app.control_heights_within(80, 18);
        let _ =
            app.control_lines_with_draft_limit(80, usize::from(composer_height.saturating_sub(1)));
    }
    assert!(app.permission_diff_render_cache.borrow().is_some());
    assert!(app.apply_current_surface_scroll_action(super::scroll::ScrollViewAction::End, 80, 18,));

    let controls_height = app.controls_height(80, 18);
    let (_, _, composer_height, status_height) = app.control_heights_within(80, 18);
    let viewport_rows = usize::from(composer_height.saturating_sub(1));
    let (_, _, bottom_panel, _) = app.control_lines_with_draft_limit(80, viewport_rows);
    assert_eq!(
        usize::from(composer_height),
        bottom_panel.len() + 1,
        "scrollable permission prompts keep the bottom padding at the end"
    );
    let panel_top = 18 - controls_height;
    let surface_top = controls_height
        .saturating_sub(composer_height)
        .saturating_sub(status_height);
    let bottom_lines = surface_lines_with_viewport(
        app.surfaces.last().unwrap(),
        &app.workspace,
        80,
        viewport_rows,
    );
    assert!(
        bottom_lines
            .iter()
            .any(|line| line.to_string().trim() == "↑")
    );
    assert!(
        !bottom_lines
            .iter()
            .any(|line| line.to_string().trim() == "↓")
    );
    let up_index = bottom_lines
        .iter()
        .position(|line| line.to_string().trim() == "↑")
        .unwrap();
    let bottom_scroll = match app.surfaces.last().unwrap() {
        Surface::Permission { diff_scroll, .. } => *diff_scroll,
        _ => unreachable!(),
    };
    app.select_mouse_at(80, 18, panel_top + surface_top + up_index as u16, 3);
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Permission { diff_scroll, .. })
            if *diff_scroll > 0 && *diff_scroll < bottom_scroll
    ));

    let down_index = surface_lines_with_viewport(
        app.surfaces.last().unwrap(),
        &app.workspace,
        80,
        viewport_rows,
    )
    .iter()
    .position(|line| line.to_string().trim() == "↓")
    .unwrap();
    app.select_mouse_at(80, 18, panel_top + surface_top + down_index as u16, 3);
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Permission { diff_scroll, .. }) if *diff_scroll == bottom_scroll
    ));
    assert!(
        !surface_lines_with_viewport(
            app.surfaces.last().unwrap(),
            &app.workspace,
            80,
            viewport_rows,
        )
        .iter()
        .any(|line| line.to_string().trim() == "↓")
    );

    let mut compact_app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    compact_app.surfaces.push(Surface::Permission {
        request: Box::new(permission_request(None)),
        selected: 0,
        scope: cagent_agent::permissions::PermissionScope::Project,
        diff_scroll: 0,
        denial_note: String::new(),
        editing_note: false,
        note_cursor: 0,
    });
    assert!(compact_app.controls_height(80, 24) < 24);
}
