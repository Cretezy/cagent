use super::interaction_surfaces::{permission_request, session_snapshot_with_interaction};
use super::*;

#[test]
fn collapsed_transcript_fills_the_screen_without_an_upward_gesture() {
    use cagent_agent::protocol::{TranscriptCursor, TranscriptPage};
    use ratatui::{Terminal, backend::TestBackend};

    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    app.collapse_tool_activity = true;
    app.transcript_prefetch_armed = false;
    let mut snapshot = session_snapshot_with_interaction(None);
    let cursor = || -> TranscriptCursor {
        serde_json::from_value(serde_json::json!({
            "conversation_id": snapshot.conversation_id,
            "older_node_id": cagent_agent::protocol::NodeId::new(),
            "newer_node_id": cagent_agent::protocol::NodeId::new(),
        }))
        .unwrap()
    };
    snapshot.transcript.older = Some(cursor());
    snapshot.transcript.blocks = (0..64)
        .map(|_| {
            crate::app::test_tool_groups(vec![
                cagent_agent::presentation::ToolActivityGroup::Bash {
                    node_id: None,
                    terminal_id: None,
                    command: "cargo check".into(),
                    status: cagent_agent::presentation::ToolActivityStatus::Succeeded,
                    output: None,
                    ansi_output: None,
                    exit_code: Some(0),
                },
            ])
        })
        .collect();
    app.apply_session_snapshot(&snapshot);
    let mut terminal = Terminal::new(TestBackend::new(80, 30)).unwrap();
    terminal
        .draw(|frame| drop(app.render_frame(frame)))
        .unwrap();
    assert!(
        !app.history_layout
            .rendered
            .as_ref()
            .unwrap()
            .rows
            .iter()
            .any(|row| row.to_string().contains("Cagent"))
    );
    assert!(app.transcript_fill_needed);
    let requested = app
        .take_transcript_page_request()
        .expect("fill the short collapsed page");
    assert!(
        app.take_transcript_page_request().is_none(),
        "one request at a time"
    );
    assert!(app.finish_transcript_page_request(&requested));
    assert!(snapshot.transcript.prepend(
        &requested,
        TranscriptPage {
            blocks: vec![crate::app::test_lines(vec![Line::from("older message")])],
            older: Some(cursor()),
        }
    ));
    app.apply_session_snapshot(&snapshot);
    terminal
        .draw(|frame| drop(app.render_frame(frame)))
        .unwrap();
    let requested = app
        .take_transcript_page_request()
        .expect("still underfull after one page");
    assert!(app.finish_transcript_page_request(&requested));
    assert!(snapshot.transcript.prepend(
        &requested,
        TranscriptPage {
            blocks: vec![crate::app::test_lines(
                    (0..40)
                        .map(|n| Line::from(format!("older row {n}")))
                        .collect()
                )],
            older: Some(cursor()),
        }
    ));
    app.apply_session_snapshot(&snapshot);
    terminal
        .draw(|frame| drop(app.render_frame(frame)))
        .unwrap();
    assert!(!app.transcript_fill_needed);
    assert!(
        app.take_transcript_page_request().is_none(),
        "stop as soon as viewport is filled"
    );
    assert!(app.follow_history_tail);

    // A later collapse can underfill it again, but paused scrolling and a
    // failed request must not start a background pagination loop.
    app.transcript_fill_needed = true;
    app.follow_history_tail = false;
    assert!(app.take_transcript_page_request().is_none());
    app.follow_history_tail = true;
    app.transcript_fill_failed = app.transcript_older.clone();
    assert!(app.take_transcript_page_request().is_none());
    app.arm_transcript_prefetch_if_idle();
    let requested = app.take_transcript_page_request().unwrap();
    assert!(app.finish_transcript_page_request(&requested));

    // Reaching the true beginning adds the intro for the first time. Measure
    // the old anchor without it so a paused viewport doesn't jump by its height.
    app.follow_history_tail = false;
    app.history_scroll = 2;
    let before = app.history_layout.rendered.as_ref().unwrap().rows[2].to_string();
    assert!(snapshot.transcript.prepend(
        &requested,
        TranscriptPage {
            blocks: vec![crate::app::test_lines(vec![Line::from("first message")])],
            older: None,
        }
    ));
    app.apply_session_snapshot(&snapshot);
    terminal
        .draw(|frame| drop(app.render_frame(frame)))
        .unwrap();
    assert_eq!(
        app.history_layout.rendered.as_ref().unwrap().rows[app.history_scroll].to_string(),
        before
    );
    assert!(app.take_transcript_page_request().is_none());
    app.scroll_history_to_top();
    terminal
        .draw(|frame| drop(app.render_frame(frame)))
        .unwrap();
    assert!(
        app.history_layout.rendered.as_ref().unwrap().rows[0]
            .to_string()
            .starts_with('╭')
    );
}

#[test]
fn streaming_snapshots_keep_the_latest_content_visible_at_the_tail() {
    use cagent_agent::protocol::{TranscriptBlockKind, TranscriptBlockStatus};
    use ratatui::{Terminal, backend::TestBackend};

    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    let mut snapshot = session_snapshot_with_interaction(None);
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    app.apply_session_snapshot(&snapshot);
    terminal
        .draw(|frame| drop(app.render_frame(frame)))
        .unwrap();
    for turn in 0..30 {
        snapshot.transcript.push(crate::app::local_transcript_block(
            TranscriptBlockKind::User {
                label: None,
                text: format!("turn {turn}"),
                attachments: Vec::new(),
                images: Vec::new(),
                image_chips: Vec::new(),
            },
        ));
        let mut live = crate::app::local_transcript_block(TranscriptBlockKind::Assistant {
            document: Default::default(),
            source: String::new(),
            message: None,
        });
        live.status = TranscriptBlockStatus::Streaming;
        snapshot.transcript.push(live);
        for chunk in 1..=10 {
            let source = (0..chunk)
                .map(|index| format!("turn-{turn}-chunk-{index}\n\n"))
                .collect::<String>();
            snapshot.transcript.last_mut().unwrap().kind = TranscriptBlockKind::Assistant {
                document: cagent_agent::presentation::parse_streaming_markdown(&source),
                source,
                message: None,
            };
            app.apply_session_snapshot(&snapshot);
            terminal
                .draw(|frame| drop(app.render_frame(frame)))
                .unwrap();
            assert!(app.follow_history_tail);
            let bottom = 24 - app.controls_height(80, 24);
            let row = (0..bottom)
                .flat_map(|y| (0..80).map(move |x| (x, y)))
                .map(|position| terminal.backend().buffer()[position].symbol())
                .collect::<String>();
            assert!(
                row.contains(&format!("turn-{turn}-chunk-{}", chunk - 1)),
                "{row:?}"
            );
        }
        snapshot.transcript.last_mut().unwrap().status = TranscriptBlockStatus::Completed;
        app.apply_session_snapshot(&snapshot);
        terminal
            .draw(|frame| drop(app.render_frame(frame)))
            .unwrap();
    }
}

#[test]
fn terminal_titles_strip_control_sequences_without_truncating_text() {
    let long = "x".repeat(200);
    assert_eq!(safe_terminal_title(&long), long);
    assert_eq!(
        safe_terminal_title("  Project\u{1b}]0;injected\u{7}\nTitle  "),
        "Project]0;injectedTitle"
    );
    assert_eq!(safe_terminal_title("\u{1b}\u{7}"), "Cagent");
}

#[test]
fn terminal_title_marks_and_clears_pending_user_interaction() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.set_requested_input_title(&cagent_agent::config::RequestedInputTitle::from_name(
        "warnings",
    ));
    let mut snapshot = session_snapshot_with_interaction(Some(question_request()));
    snapshot.title = Some("Needs an answer".into());

    app.apply_session_snapshot(&snapshot);
    assert_eq!(
        app.pending_terminal_title.as_deref(),
        Some(" Needs an answer")
    );
    assert!(!app.osc_progress_active());

    app.advance_progress_frame();
    assert_eq!(
        app.pending_terminal_title.as_deref(),
        Some(" Needs an answer")
    );

    snapshot.pending_interaction = None;
    app.apply_session_snapshot(&snapshot);
    assert_eq!(
        app.pending_terminal_title.as_deref(),
        Some("Needs an answer")
    );
}

#[test]
fn cleared_interaction_snapshot_restores_the_pending_bash_card() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    let mut snapshot = session_snapshot_with_interaction(Some(question_request()));
    snapshot.title = Some("Running work".into());
    app.apply_session_snapshot(&snapshot);
    assert!(app.pending_interaction.is_some());

    snapshot.pending_interaction = None;
    snapshot.transcript = vec![cagent_agent::protocol::TranscriptBlock {
        id: cagent_agent::protocol::TranscriptBlockId::derived(
            "tools",
            cagent_agent::protocol::NodeId::new(),
        ),
        status: cagent_agent::protocol::TranscriptBlockStatus::Pending,
        kind: cagent_agent::protocol::TranscriptBlockKind::ToolGroups {
            groups: vec![cagent_agent::presentation::ToolActivityGroup::Bash {
                node_id: Some(cagent_agent::protocol::NodeId::new()),
                terminal_id: None,
                command: "cargo test".into(),
                status: cagent_agent::presentation::ToolActivityStatus::Pending,
                output: None,
                ansi_output: None,
                exit_code: None,
            }],
        },
    }]
    .into();
    app.apply_session_snapshot(&snapshot);

    assert!(app.pending_interaction.is_none());
    assert_eq!(app.pending_terminal_title.as_deref(), Some("Running work"));
    assert!(matches!(
        app.history.as_slice(),
        [TranscriptBlock { kind: cagent_agent::protocol::TranscriptBlockKind::ToolGroups { groups }, .. }]
            if matches!(groups.as_slice(), [cagent_agent::presentation::ToolActivityGroup::Bash { command, .. }] if command == "cargo test")
    ));
}

#[test]
fn terminal_title_uses_a_custom_pending_interaction_prefix() {
    let title = cagent_agent::config::RequestedInputTitle::from_name("warning");
    assert_eq!(
        session_terminal_title(Some("Needs an answer"), true, &title),
        " Needs an answer"
    );
    assert_eq!(
        session_terminal_title(
            Some("Needs an answer"),
            true,
            &cagent_agent::config::RequestedInputTitle::from_name(""),
        ),
        "Needs an answer"
    );
}

#[test]
fn terminal_title_progress_uses_frames_and_pending_prefix_wins() {
    let title = cagent_agent::config::RequestedInputTitle::from_name("warnings");
    let mode = cagent_agent::config::ProgressMode::Braille;
    assert_eq!(
        session_terminal_title_with_progress(Some("Project"), false, &title, &mode, true, 0),
        "⠋ Project"
    );
    assert_eq!(
        session_terminal_title_with_progress(Some("Project"), false, &title, &mode, true, 1),
        "⠙ Project"
    );
    assert_eq!(
        session_terminal_title_with_progress(Some("Project"), true, &title, &mode, true, 1),
        " Project"
    );
    assert_eq!(
        session_terminal_title_with_progress(Some("Project"), true, &title, &mode, true, 2),
        " Project"
    );
    assert_eq!(
        session_terminal_title_with_progress(
            Some("Project"),
            false,
            &title,
            &cagent_agent::config::ProgressMode::False,
            true,
            0,
        ),
        "Project"
    );
    assert_eq!(
        session_terminal_title_with_progress(
            Some("Project"),
            false,
            &title,
            &cagent_agent::config::ProgressMode::False,
            true,
            0,
        ),
        "Project"
    );
}

#[test]
fn active_snapshot_keeps_previous_replies_and_places_exploration_before_the_live_reply() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    let previous = cagent_agent::protocol::NodeId::new();
    let user = cagent_agent::protocol::NodeId::new();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let started_at = now.saturating_sub(1_000).to_string();
    let last_activity_at = now.saturating_sub(100).to_string();
    let mut snapshot = cagent_agent::protocol::SessionSnapshot {
        conversation_id: cagent_agent::protocol::ConversationId::new(),
        project_dir: "/tmp/project".into(),
        cwd: "/tmp/project".into(),
        worktree: None,
        access: cagent_agent::protocol::SessionAccess::Owner,
        cursor: None,
        title: Some("Generated title".into()),
        transcript: vec![
            cagent_agent::protocol::TranscriptBlock {
                id: cagent_agent::protocol::TranscriptBlockId::node(user),
                status: cagent_agent::protocol::TranscriptBlockStatus::Completed,
                kind: cagent_agent::protocol::TranscriptBlockKind::User {
                    label: None,
                    text: "Read the spec".into(),
                    attachments: Vec::new(),
                    images: Vec::new(),
                    image_chips: Vec::new(),
                },
            },
            cagent_agent::protocol::TranscriptBlock {
                id: cagent_agent::protocol::TranscriptBlockId::node(previous),
                status: cagent_agent::protocol::TranscriptBlockStatus::Completed,
                kind: cagent_agent::protocol::TranscriptBlockKind::Assistant {
                    source: "The spec is clear.".into(),
                    document: cagent_agent::presentation::parse_markdown("The spec is clear."),
                    message: None,
                },
            },
            cagent_agent::protocol::TranscriptBlock {
                id: cagent_agent::protocol::TranscriptBlockId("live-assistant".into()),
                status: cagent_agent::protocol::TranscriptBlockStatus::Streaming,
                kind: cagent_agent::protocol::TranscriptBlockKind::Assistant {
                    source: "Here is the one-line summary.".into(),
                    document: cagent_agent::presentation::parse_markdown(
                        "Here is the one-line summary.",
                    ),
                    message: None,
                },
            },
        ]
        .into(),
        turn: cagent_agent::protocol::TurnState::Working {
            started_at: started_at.clone(),
            turn_id: cagent_agent::protocol::TurnId::new(),
        },
        active_plan: None,
        last_activity_at: Some(last_activity_at),
        model_selection: Some(("mock".into(), "echo".into(), None)),
        fast: false,
        fast_effective: false,
        active_agent: "general".into(),
        active_mode: "ask".into(),
        queue: Vec::new(),
        composer_history: Vec::new(),
        usage: cagent_agent::protocol::SessionUsage::default(),
        context: None,
        provider_usage: None,
        pending_interaction: None,
        agent_runs: Vec::new(),
        delegated_live: Vec::new(),
        terminals: Vec::new(),
        supervised_work: Vec::new(),
        enabled_providers: vec!["mock".into()],
        startup_resources: cagent_agent::protocol::StartupResourceStatus::default(),
    };

    app.apply_session_snapshot(&snapshot);

    assert!(app.active);
    assert_eq!(
        app.pending_terminal_title.as_deref(),
        Some("Generated title")
    );
    assert!(matches!(
        app.history.as_slice(),
        [
            TranscriptBlock { kind: cagent_agent::protocol::TranscriptBlockKind::User { .. }, .. },
            TranscriptBlock { kind: cagent_agent::protocol::TranscriptBlockKind::Assistant { source, .. }, .. },
        ] if source == "The spec is clear."
    ));
    assert_eq!(app.streaming_source, "Here is the one-line summary.");

    let working_started_at = Instant::now() - Duration::from_secs(1);
    app.working_started_at = Some(working_started_at);
    app.apply_session_snapshot(&snapshot);

    assert_ne!(app.working_started_at, Some(working_started_at));
    let elapsed = app.working_started_at.unwrap().elapsed();
    assert!(elapsed >= Duration::from_millis(900));
    assert!(app.last_activity_at.unwrap() > app.working_started_at.unwrap());

    snapshot.turn = cagent_agent::protocol::TurnState::Cancelling {
        started_at: started_at.clone(),
        turn_id: cagent_agent::protocol::TurnId::new(),
    };
    app.apply_session_snapshot(&snapshot);
    assert!(app.active);
    assert!(!app.working_indicator_visible());

    snapshot.transcript = vec![cagent_agent::protocol::TranscriptBlock {
        id: cagent_agent::protocol::TranscriptBlockId("pending-interrupt".into()),
        status: cagent_agent::protocol::TranscriptBlockStatus::Pending,
        kind: cagent_agent::protocol::TranscriptBlockKind::Interrupt {
            queued_steering: true,
        },
    }]
    .into();
    snapshot.turn = cagent_agent::protocol::TurnState::Idle;
    app.apply_session_snapshot(&snapshot);
    assert!(
        !app.history
            .iter()
            .any(|block| matches!(&block.kind, cagent_agent::protocol::TranscriptBlockKind::Notice { message } if message.starts_with("Worked for ")))
    );
    assert!(app.history.iter().any(|block| matches!(
        &block.kind,
        cagent_agent::protocol::TranscriptBlockKind::Interrupt {
            queued_steering: true
        }
    )));
    let interruption = app
        .history
        .iter()
        .find_map(|block| match &block.kind {
            cagent_agent::protocol::TranscriptBlockKind::Interrupt {
                queued_steering: true,
            } => Some(block),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        interruption.status,
        cagent_agent::protocol::TranscriptBlockStatus::Pending
    );
}

#[test]
fn snapshot_opens_completed_plan_interaction_after_hydrating_the_plan() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "plan",
        None,
    );
    let plan = "# Authentication\n\nUse PKCE.";
    let snapshot = cagent_agent::protocol::SessionSnapshot {
        conversation_id: cagent_agent::protocol::ConversationId::new(),
        project_dir: "/tmp/project".into(),
        cwd: "/tmp/project".into(),
        worktree: None,
        access: cagent_agent::protocol::SessionAccess::Owner,
        cursor: None,
        title: None,
        transcript: vec![cagent_agent::protocol::TranscriptBlock {
            id: cagent_agent::protocol::TranscriptBlockId::node(
                cagent_agent::protocol::NodeId::new(),
            ),
            status: cagent_agent::protocol::TranscriptBlockStatus::Completed,
            kind: cagent_agent::protocol::TranscriptBlockKind::Plan {
                source: plan.into(),
                document: cagent_agent::presentation::parse_markdown(plan),
            },
        }]
        .into(),
        turn: cagent_agent::protocol::TurnState::Idle,
        active_plan: None,
        last_activity_at: None,
        model_selection: Some(("mock".into(), "echo".into(), None)),
        fast: false,
        fast_effective: false,
        active_agent: "general".into(),
        active_mode: "plan".into(),
        queue: Vec::new(),
        composer_history: Vec::new(),
        usage: cagent_agent::protocol::SessionUsage::default(),
        context: None,
        provider_usage: None,
        pending_interaction: Some(InteractionRequest {
            id: cagent_agent::protocol::InteractionRequestId::new(),
            origin: None,
            kind: InteractionRequestKind::PlanCompletion {
                plan: plan.into(),
                implementation_modes: vec!["edit".into(), "auto".into()],
                default_mode: "edit".into(),
            },
        }),
        agent_runs: Vec::new(),
        delegated_live: Vec::new(),
        terminals: Vec::new(),
        supervised_work: Vec::new(),
        enabled_providers: vec!["mock".into()],
        startup_resources: cagent_agent::protocol::StartupResourceStatus::default(),
    };

    app.apply_session_snapshot(&snapshot);

    assert!(matches!(
        app.history.last(),
        Some(TranscriptBlock { kind: cagent_agent::protocol::TranscriptBlockKind::Plan { source, .. }, .. }) if source == plan
    ));
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::PlanCompletion {
            implementation_model,
            ..
        }) if implementation_model == "mock/echo"
    ));
}

#[test]
fn snapshot_renders_an_accepted_plan_card_and_preserves_its_copy_source() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "edit",
        None,
    );
    let source = "# Build\n\nA deliberately long accepted-plan sentence that wraps.\n\n| File | Change |\n| --- | --- |\n| `src/lib.rs` | Parse Markdown |";
    let mut snapshot = session_snapshot_with_interaction(None);
    snapshot.transcript = vec![cagent_agent::protocol::TranscriptBlock {
        id: cagent_agent::protocol::TranscriptBlockId::node(cagent_agent::protocol::NodeId::new()),
        status: cagent_agent::protocol::TranscriptBlockStatus::Completed,
        kind: cagent_agent::protocol::TranscriptBlockKind::AcceptedPlan {
            source: source.into(),
            document: cagent_agent::presentation::parse_markdown(source),
            clear_context: true,
            compact_context: false,
            compaction_summary: None,
        },
    }]
    .into();

    app.apply_session_snapshot(&snapshot);
    app.welcome.clear();
    app.ensure_history_layout(32);

    let rows = &app.history_layout.rendered.as_ref().unwrap().rows;
    let rendered = rows
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("• Accepted plan"));
    assert!(rendered.contains("deliberately long"));
    assert!(rendered.contains("File"));
    assert!(rendered.contains("src/lib.rs"));
    assert!(rows.iter().any(|row| row.line.style == USER_STYLE));
    assert_eq!(app.latest_assistant_source(), Some(source));
    assert!(matches!(
        app.history.last(),
        Some(TranscriptBlock { kind: cagent_agent::protocol::TranscriptBlockKind::AcceptedPlan { source: stored, .. }, .. }) if stored == source
    ));
}

#[test]
fn compact_accepted_plan_only_renders_a_confirmation() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "edit",
        None,
    );
    let source = "# Build\n\nThis plan is already visible above.";
    let mut snapshot = session_snapshot_with_interaction(None);
    snapshot.transcript = vec![cagent_agent::protocol::TranscriptBlock {
        id: cagent_agent::protocol::TranscriptBlockId::node(cagent_agent::protocol::NodeId::new()),
        status: cagent_agent::protocol::TranscriptBlockStatus::Completed,
        kind: cagent_agent::protocol::TranscriptBlockKind::AcceptedPlan {
            source: source.into(),
            document: cagent_agent::presentation::parse_markdown(source),
            clear_context: false,
            compact_context: true,
            compaction_summary: Some("Preserve the build constraints.".into()),
        },
    }]
    .into();

    app.apply_session_snapshot(&snapshot);
    app.welcome.clear();
    app.ensure_history_layout(32);
    let rendered = app
        .history_layout
        .rendered
        .as_ref()
        .unwrap()
        .rows
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    assert!(rendered.starts_with("• Plan accepted\n\n─ Compacted "));
    let accepted_plan = &app.history_layout.rendered.as_ref().unwrap().rows[0]
        .line
        .spans[1];
    assert!(accepted_plan.style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(app.latest_assistant_source(), Some(source));
}

#[test]
fn snapshot_keeps_a_streaming_plan_in_the_live_plan_viewport() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "plan",
        None,
    );
    let source = "# Build\n\n- Persist this";
    let snapshot = cagent_agent::protocol::SessionSnapshot {
        conversation_id: cagent_agent::protocol::ConversationId::new(),
        project_dir: "/tmp/project".into(),
        cwd: "/tmp/project".into(),
        worktree: None,
        access: cagent_agent::protocol::SessionAccess::Owner,
        cursor: None,
        title: None,
        transcript: vec![cagent_agent::protocol::TranscriptBlock {
            id: cagent_agent::protocol::TranscriptBlockId::node(
                cagent_agent::protocol::NodeId::new(),
            ),
            status: cagent_agent::protocol::TranscriptBlockStatus::Streaming,
            kind: cagent_agent::protocol::TranscriptBlockKind::Plan {
                source: source.into(),
                document: cagent_agent::presentation::parse_markdown(source),
            },
        }]
        .into(),
        turn: cagent_agent::protocol::TurnState::Working {
            started_at: "now".into(),
            turn_id: cagent_agent::protocol::TurnId::new(),
        },
        active_plan: None,
        last_activity_at: None,
        model_selection: None,
        fast: false,
        fast_effective: false,
        active_agent: "general".into(),
        active_mode: "plan".into(),
        queue: Vec::new(),
        composer_history: Vec::new(),
        usage: cagent_agent::protocol::SessionUsage::default(),
        context: None,
        provider_usage: None,
        pending_interaction: None,
        agent_runs: Vec::new(),
        terminals: Vec::new(),
        enabled_providers: Vec::new(),
        startup_resources: cagent_agent::protocol::StartupResourceStatus::default(),
        supervised_work: Vec::new(),
        delegated_live: Vec::new(),
    };
    app.apply_session_snapshot(&snapshot);
    assert_eq!(app.streaming_plan_source, source);
    assert!(app.history.is_empty());
}

#[test]
fn ctrl_v_is_the_paste_key() {
    assert!(is_paste_key(KeyEvent::new(
        KeyCode::Char('v'),
        KeyModifiers::CONTROL,
    )));
    assert!(!is_paste_key(KeyEvent::new(
        KeyCode::Char('v'),
        KeyModifiers::NONE,
    )));
}

#[test]
fn escape_key_accepts_terminal_escape_byte() {
    assert!(is_escape_key(KeyEvent::new(
        KeyCode::Esc,
        KeyModifiers::NONE,
    )));
    assert!(is_escape_key(KeyEvent::new(
        KeyCode::Char('\x1b'),
        KeyModifiers::NONE,
    )));
    assert!(!is_escape_key(KeyEvent::new(
        KeyCode::Backspace,
        KeyModifiers::NONE,
    )));

    let app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    for code in [KeyCode::Esc, KeyCode::Char('\x1b')] {
        assert!(app.matches_action(
            cagent_agent::presentation::KeyBindingAction::CloseSurface,
            KeyEvent::new(code, KeyModifiers::NONE),
        ));
    }
}

#[test]
fn ctrl_j_and_ctrl_k_map_to_menu_navigation() {
    assert_eq!(
        menu_navigation_code(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL,)),
        KeyCode::Up
    );
    assert_eq!(
        menu_navigation_code(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL,)),
        KeyCode::Down
    );
    assert_eq!(
        menu_navigation_code(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)),
        KeyCode::Char('j')
    );
}

#[test]
fn word_navigation_accepts_terminal_modifier_chords() {
    assert!(is_word_backspace(KeyEvent::new(
        KeyCode::Backspace,
        KeyModifiers::ALT,
    )));
    assert!(is_word_left(KeyEvent::new(
        KeyCode::Left,
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    )));
    assert!(is_word_right(KeyEvent::new(
        KeyCode::Right,
        KeyModifiers::CONTROL,
    )));
    assert!(!is_word_left(KeyEvent::new(
        KeyCode::Left,
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    )));

    let draft = "one two three";
    assert_eq!(previous_word_boundary(draft, draft.len()), "one two ".len());
    assert_eq!(next_word_boundary(draft, "one ".len()), "one two".len());
}

#[tokio::test]
async fn composer_word_navigation_lands_on_word_edges() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = AgentRuntime::open(cagent_agent::runtime::RuntimeOptions::new(
        temporary.path().join("composer-word-navigation.db"),
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
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.replace_draft("one two three");
    app.cursor = 0;

    app.handle_key(
        &session,
        KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL),
    )
    .await
    .unwrap();
    assert_eq!(app.cursor, "one".len());
    app.handle_key(&session, KeyEvent::new(KeyCode::Right, KeyModifiers::ALT))
        .await
        .unwrap();
    assert_eq!(app.cursor, "one two".len());
    app.handle_key(
        &session,
        KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL),
    )
    .await
    .unwrap();
    assert_eq!(app.cursor, "one ".len());
    app.handle_key(&session, KeyEvent::new(KeyCode::Left, KeyModifiers::ALT))
        .await
        .unwrap();
    assert_eq!(app.cursor, 0);
}

#[test]
fn permission_denial_clears_deferred_draft_state_without_status_notice() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.interaction_clears_draft = true;

    app.finish_interaction_response("deny");

    assert!(!app.interaction_clears_draft);
    assert!(app.notice.is_none());
}

#[test]
fn pending_interaction_reconciliation_is_idempotent() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    let request = permission_request(None);

    app.set_pending_interaction(request.clone());
    app.set_pending_interaction(request);

    assert_eq!(app.surfaces.len(), 1);
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Permission { .. })
    ));
}

#[test]
fn snapshot_dismisses_an_interaction_surface_that_is_no_longer_pending() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.set_pending_interaction(permission_request(None));
    app.interaction_clears_draft = true;

    app.apply_session_snapshot(&session_snapshot_with_interaction(None));

    assert!(app.pending_interaction.is_none());
    assert!(app.surfaces.is_empty());
    assert!(!app.interaction_clears_draft);
}

#[test]
fn snapshot_replaces_a_stale_interaction_surface() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    let stale = permission_request(None);
    let current = permission_request(None);
    app.set_pending_interaction(stale);

    app.apply_session_snapshot(&session_snapshot_with_interaction(Some(current.clone())));

    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Permission { request, .. }) if request.id == current.id
    ));
    assert_eq!(app.surfaces.len(), 1);
}

#[test]
fn permission_prompt_waits_for_an_open_menu_to_close() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.surfaces.push(Surface::Profiles {
        kind: ProfileKind::Mode,
        rows: vec![("ask".into(), "ask before mutations".into(), true)],
        list: ListState::selectable(1),
    });

    app.set_pending_interaction(permission_request(None));

    assert!(app.pending_interaction.is_some());
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Profiles { .. })
    ));

    app.surfaces.pop();
    app.open_pending_interaction();

    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Permission { .. })
    ));
}

#[test]
fn pending_interaction_temporarily_covers_a_file_opened_from_sidebar() {
    let temporary = tempfile::tempdir().unwrap();
    let file = temporary.path().join("large.rs");
    std::fs::write(&file, "fn main() {}\n").unwrap();
    let mut app = App::new(
        temporary.path(),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.toggle_files_sidebar();
    assert!(app.open_builtin_path(&file, true));
    app.files_sidebar.focused = false;
    app.files_sidebar.view_opened_from_sidebar = true;
    let Some(Surface::Expanded { scroll, .. }) = app.surfaces.last_mut() else {
        panic!("expected expanded file");
    };
    *scroll = 37;

    app.set_pending_interaction(permission_request(None));

    assert_eq!(app.surfaces.len(), 2);
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Permission { .. })
    ));
    assert!(matches!(
        app.surfaces.first(),
        Some(Surface::Expanded {
            view: ExpandedView::File { view },
            scroll: 37,
            ..
        }) if view.path == file
    ));

    app.surfaces.pop();
    app.finish_interaction_response("allow_once");

    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Expanded {
            view: ExpandedView::File { view },
            scroll: 37,
            ..
        }) if view.path == file
    ));
    assert!(!app.files_sidebar.focused);
}

#[test]
fn permission_prompt_waits_for_nested_expanded_views_to_close() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.surfaces.push(Surface::SupervisedWork {
        rows: Vec::new(),
        list: ListState::selectable(0),
        show_past: false,
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
        viewport_rows: 1,
    });

    app.set_pending_interaction(permission_request(None));
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Expanded { .. })
    ));

    app.surfaces.pop();
    app.open_pending_interaction();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::SupervisedWork { .. })
    ));

    app.surfaces.pop();
    app.open_pending_interaction();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Permission { .. })
    ));
}

#[tokio::test]
async fn permission_prompt_reopens_after_real_surface_close_events() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = AgentRuntime::open(cagent_agent::runtime::RuntimeOptions::new(
        temporary.path().join("permission-surface.db"),
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
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.surfaces.push(Surface::Profiles {
        kind: ProfileKind::Mode,
        rows: vec![("ask".into(), "ask before mutations".into(), true)],
        list: ListState::selectable(1),
    });
    app.surfaces.push(Surface::Expanded {
        view: ExpandedView::Terminal {
            terminal_id: None,
            command: "sleep 1".into(),
            output: String::new(),
            ansi_output: String::new(),
            completion: None,
            started_at: None,
            completed_at: None,
        },
        scroll: 0,
        viewport_rows: 1,
    });
    app.set_pending_interaction(permission_request(None));

    app.handle_surface_key(&session, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Profiles { .. })
    ));

    app.handle_surface_key(&session, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Permission { .. })
    ));
}

#[test]
fn bash_permission_surface_separates_the_question_from_the_command() {
    let request = InteractionRequest {
        id: cagent_agent::protocol::InteractionRequestId::new(),
        origin: None,
        kind: InteractionRequestKind::PermissionApproval {
            resource: cagent_agent::permissions::PermissionResource {
                tool: "bash".into(),
                server: None,
                operation: None,
                path: Some("/home/charles/.config/cagent".into()),
                access: Some(cagent_agent::permissions::PermissionAccess::Execute),
                mode: "ask".into(),
                agent: "general".into(),
                command: vec!["echo".into(), "hello".into()],
                raw_command: Some("echo hello".into()),
                cwd: Some("/tmp/project".into()),
            },
            decision: cagent_agent::permissions::FilesystemPermissionDecision {
                effect: cagent_agent::permissions::PermissionEffect::Ask,
                operation: cagent_agent::permissions::PermissionDecision {
                    effect: cagent_agent::permissions::PermissionEffect::Ask,
                    layer: cagent_agent::permissions::PermissionLayerKind::Default,
                    rule_id: None,
                    reason: "bash default".into(),
                },
                external: None,
            },
            message: "Allow bash access to /home/charles/.config/cagent?".into(),
            queued_message_id: None,
            preview: None,
            arguments: None,
            auto_review: Some(cagent_agent::protocol::AutoReviewSummary {
                decision: "ask".into(),
                risk: "high".into(),
                authorization: "medium".into(),
                reason: "needs confirmation".into(),
            }),
            suggested_rule: None,
        },
    };
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

    assert!(rendered.contains("Permission  bash"));
    assert!(rendered.contains("Allow running the following?\n\n  │ echo hello"));
    assert!(!rendered.contains("Allow bash access to"));
    assert!(rendered.contains("Allow"));
    assert!(rendered.contains("Auto review  needs confirmation"));
    assert!(!rendered.contains("Operation"));
    assert!(!rendered.contains("Boundary"));
    assert!(!rendered.contains("Result"));
    assert!(!rendered.contains("```"));
    assert!(!rendered.contains("not an OS sandbox"));
}

#[test]
fn permission_surface_wraps_the_auto_review_explanation() {
    let mut request = permission_request(None);
    let InteractionRequestKind::PermissionApproval { auto_review, .. } = &mut request.kind else {
        unreachable!();
    };
    *auto_review = Some(cagent_agent::protocol::AutoReviewSummary {
        decision: "ask".into(),
        risk: "high".into(),
        authorization: "medium".into(),
        reason: "the action may persist data outside the workspace and needs confirmation".into(),
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

    let lines = surface_lines(&surface, Path::new("/tmp/project"), 36);
    assert!(lines.iter().all(|line| line.width() <= 36));
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.to_string().contains("Auto review"))
            .count(),
        1
    );
    let rendered = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("  Auto review  the action may"));
    assert!(rendered.contains("  persist data outside the workspace"));
    assert!(rendered.contains("  and needs confirmation"));
}

#[test]
fn bash_read_permission_surface_shows_the_external_path_and_read_menu() {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/home/test"));
    let target = home.join("worktree-test/test-git");
    let target = target.to_string_lossy().replace('\\', "/");
    let mut request = permission_request(None);
    let InteractionRequestKind::PermissionApproval {
        resource,
        decision,
        message,
        suggested_rule,
        ..
    } = &mut request.kind
    else {
        unreachable!();
    };
    resource.tool = "bash".into();
    resource.path = Some(target.clone());
    resource.access = Some(cagent_agent::permissions::PermissionAccess::Read);
    resource.command.clear();
    resource.raw_command = Some("ls ~/worktree-test/test-git".into());
    decision.operation.effect = cagent_agent::permissions::PermissionEffect::Allow;
    decision.operation.reason = "recognized read-safe command".into();
    decision.external = Some(cagent_agent::permissions::PermissionDecision {
        effect: cagent_agent::permissions::PermissionEffect::Ask,
        layer: cagent_agent::permissions::PermissionLayerKind::Default,
        rule_id: None,
        reason: "outside workspace".into(),
    });
    decision.effect = cagent_agent::permissions::PermissionEffect::Ask;
    *message = "Allow reading from ~/worktree-test/test-git?".into();
    let rule = suggested_rule.as_mut().unwrap();
    rule.tool = Some("bash".into());
    rule.path = Some(target);
    rule.access = Some("read".into());

    let surface = Surface::Permission {
        request: Box::new(request),
        selected: 1,
        scope: cagent_agent::permissions::PermissionScope::Project,
        diff_scroll: 0,
        denial_note: String::new(),
        editing_note: false,
        note_cursor: 0,
    };
    let lines = surface_lines(&surface, Path::new("/tmp/project"), 100);
    let rendered = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    assert!(rendered.contains("Allow reading from ~/worktree-test/test-git?"));
    assert!(rendered.contains("Allow reading"));
    assert!(rendered.contains("Always allow reading ~/worktree-test/test-git"));
    assert!(!rendered.contains("Operation"));
    assert!(!rendered.contains("Boundary"));
    assert!(!rendered.contains("Result"));
    assert!(!rendered.contains("Auto review"));
    assert!(!rendered.contains("Allow running the following?"));
    assert!(!rendered.contains("│ ls ~/worktree-test/test-git"));
    let path = lines
        .iter()
        .flat_map(|line| &line.spans)
        .find(|span| span.content == "~/worktree-test/test-git")
        .expect("read path chip");
    assert_eq!(path.style, CHIP_STYLE);
}

#[test]
fn enter_worktree_permission_highlights_the_formatted_target() {
    let mut request = permission_request(None);
    let InteractionRequestKind::PermissionApproval {
        resource, message, ..
    } = &mut request.kind
    else {
        unreachable!();
    };
    resource.tool = "enter_worktree".into();
    resource.path = Some("/tmp/project/.cagent/worktrees/blue-button".into());
    resource.command = vec!["blue-button".into()];
    *message = "Allow changing worktree to blue-button?".into();
    let surface = Surface::Permission {
        request: Box::new(request),
        selected: 0,
        scope: cagent_agent::permissions::PermissionScope::Project,
        diff_scroll: 0,
        denial_note: String::new(),
        editing_note: false,
        note_cursor: 0,
    };

    let lines = surface_lines(&surface, Path::new("/tmp/project"), 80);
    let prompt = lines
        .iter()
        .find(|line| line.to_string().contains("Allow changing worktree"))
        .unwrap();

    assert_eq!(
        prompt.to_string(),
        "  Allow changing worktree to blue-button?"
    );
    assert_eq!(prompt.spans[1].content, "blue-button");
    assert_eq!(prompt.spans[1].style, CHIP_STYLE);
}

#[test]
fn workspace_transition_notice_bolds_only_the_default_colored_label() {
    let mut snapshot = session_snapshot_with_interaction(None);
    snapshot
        .transcript
        .push(cagent_agent::protocol::TranscriptBlock {
            id: cagent_agent::protocol::TranscriptBlockId::node(
                cagent_agent::protocol::NodeId::new(),
            ),
            status: cagent_agent::protocol::TranscriptBlockStatus::Completed,
            kind: cagent_agent::protocol::TranscriptBlockKind::WorkspaceTransition {
                label: "Enter Worktree".into(),
                target: "blue-button".into(),
                base: Some("release/123".into()),
                path: None,
            },
        });
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );

    app.apply_session_snapshot(&snapshot);

    let cagent_agent::protocol::TranscriptBlockKind::WorkspaceTransition {
        label,
        target,
        base,
        path,
    } = &app.history.last().unwrap().kind
    else {
        panic!("expected workspace transition");
    };
    let lines = crate::render::transcript::history_item_rows_with_editor(
        &app.history[0],
        Path::new("/tmp/project"),
        80,
        true,
    );
    assert_eq!(
        lines[0].to_string(),
        "• Enter Worktree blue-button · release/123"
    );
    assert_eq!(
        lines[0].line.spans[0].style,
        Style::default().add_modifier(Modifier::BOLD)
    );
    assert_eq!(lines[0].line.spans[2].style, CHIP_STYLE);
    assert_eq!(lines[0].line.spans[3].style, DIM_STYLE);
    assert_eq!(label, "Enter Worktree");
    assert_eq!(target, "blue-button");
    assert_eq!(base.as_deref(), Some("release/123"));
    assert!(path.is_none());
}

#[test]
fn clicking_a_workspace_transition_target_opens_its_directory() {
    let temporary = tempfile::tempdir().unwrap();
    let target = temporary.path().join("leveled-shell-safety");
    std::fs::create_dir(&target).unwrap();
    let mut app = App::new(
        temporary.path(),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.path_hits.push(PathHit {
        row: 7,
        columns: 2..23,
        path: target.clone(),
        line: None,
        directory: true,
    });

    assert!(app.open_path_at(7, 4));
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Expanded {
            view: ExpandedView::Directory { browser },
            ..
        }) if browser.tree.root() == target
    ));
}

#[test]
fn web_search_permission_highlights_an_unquoted_query() {
    let mut request = permission_request(None);
    let InteractionRequestKind::PermissionApproval {
        resource, message, ..
    } = &mut request.kind
    else {
        unreachable!();
    };
    resource.tool = "web_search".into();
    resource.command = vec!["cretezy.com".into()];
    *message = "Allow web search of cretezy.com?".into();
    let surface = Surface::Permission {
        request: Box::new(request),
        selected: 0,
        scope: cagent_agent::permissions::PermissionScope::Project,
        diff_scroll: 0,
        denial_note: String::new(),
        editing_note: false,
        note_cursor: 0,
    };

    let lines = surface_lines(&surface, Path::new("/tmp/project"), 80);
    let query = lines
        .iter()
        .flat_map(|line| &line.spans)
        .find(|span| span.content == "cretezy.com")
        .expect("web-search query");

    assert_eq!(query.style, CHIP_STYLE);
    assert!(
        lines
            .iter()
            .any(|line| { line.to_string() == "  Allow web search of cretezy.com?" })
    );
}

#[test]
fn web_fetch_permission_labels_a_nondefault_format() {
    let mut request = permission_request(None);
    let InteractionRequestKind::PermissionApproval {
        resource,
        message,
        arguments,
        ..
    } = &mut request.kind
    else {
        unreachable!();
    };
    resource.tool = "web_fetch".into();
    resource.command = vec!["https://cretezy.com/2026/worktree-copy/".into()];
    *message = "Allow web fetch of https://cretezy.com/2026/worktree-copy/?".into();
    *arguments = Some(serde_json::json!({ "format": "html" }));
    let surface = Surface::Permission {
        request: Box::new(request),
        selected: 0,
        scope: cagent_agent::permissions::PermissionScope::Project,
        diff_scroll: 0,
        denial_note: String::new(),
        editing_note: false,
        note_cursor: 0,
    };

    let lines = surface_lines(&surface, Path::new("/tmp/project"), 80);
    assert!(lines.iter().any(|line| {
        line.to_string() == "  Allow web fetch of https://cretezy.com/2026/worktree-copy/? (html)"
    }));
    assert!(
        lines
            .iter()
            .all(|line| !line.to_string().contains("\"format\""))
    );

    let mut default_request = permission_request(None);
    let InteractionRequestKind::PermissionApproval {
        resource,
        arguments,
        ..
    } = &mut default_request.kind
    else {
        unreachable!();
    };
    resource.tool = "web_fetch".into();
    resource.command = vec!["https://example.com/".into()];
    *arguments = Some(serde_json::json!({ "format": "markdown" }));
    let default_surface = Surface::Permission {
        request: Box::new(default_request),
        selected: 0,
        scope: cagent_agent::permissions::PermissionScope::Project,
        diff_scroll: 0,
        denial_note: String::new(),
        editing_note: false,
        note_cursor: 0,
    };
    let default_lines = surface_lines(&default_surface, Path::new("/tmp/project"), 80);
    assert!(
        default_lines
            .iter()
            .all(|line| !line.to_string().contains("(markdown)"))
    );
}

#[test]
fn web_fetch_permission_shows_the_complete_redirect_chain() {
    let mut request = permission_request(None);
    let InteractionRequestKind::PermissionApproval {
        resource,
        arguments,
        ..
    } = &mut request.kind
    else {
        unreachable!();
    };
    resource.tool = "web_fetch".into();
    resource.command = vec!["https://www.apple.com/".into()];
    *arguments = Some(serde_json::json!({
        "format": "markdown",
        "redirected_from": ["http://apple.com/", "https://apple.com/"]
    }));
    let surface = Surface::Permission {
        request: Box::new(request),
        selected: 0,
        scope: cagent_agent::permissions::PermissionScope::Project,
        diff_scroll: 0,
        denial_note: String::new(),
        editing_note: false,
        note_cursor: 0,
    };

    let lines = surface_lines(&surface, Path::new("/tmp/project"), 120);
    let redirect = lines
        .iter()
        .flat_map(|line| &line.spans)
        .find(|span| span.content.contains("redirected from"))
        .expect("redirect detail");
    assert_eq!(
        redirect.content,
        " (redirected from http://apple.com/ → https://apple.com/)"
    );
    assert_eq!(redirect.style, DIM_STYLE);
}

#[test]
fn permission_surface_shows_and_edits_a_denial_note() {
    let request = permission_request(None);
    let surface = Surface::Permission {
        request: Box::new(request.clone()),
        selected: 2,
        scope: cagent_agent::permissions::PermissionScope::Project,
        diff_scroll: 0,
        denial_note: String::new(),
        editing_note: false,
        note_cursor: 0,
    };
    let rendered = surface_lines(&surface, Path::new("/tmp/project"), 80)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!rendered.contains("Add note"));

    let surface = Surface::Permission {
        request: Box::new(request),
        selected: 2,
        scope: cagent_agent::permissions::PermissionScope::Project,
        diff_scroll: 0,
        denial_note: "too broad".into(),
        editing_note: true,
        note_cursor: 9,
    };
    let rendered = surface_lines(&surface, Path::new("/tmp/project"), 80)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>();
    assert!(rendered.iter().any(|line| line.contains("too broad")));
    assert_eq!(rendered.last().map(String::as_str), Some(""));
    assert!(
        rendered
            .get(rendered.len().saturating_sub(2))
            .is_some_and(|line| line.starts_with("› too broad"))
    );

    let closed = Surface::Permission {
        request: Box::new(permission_request(None)),
        selected: 2,
        scope: cagent_agent::permissions::PermissionScope::Project,
        diff_scroll: 0,
        denial_note: String::new(),
        editing_note: false,
        note_cursor: 0,
    };
    let open_rows = permission_surface_line_count(&surface, Path::new("/tmp/project"), 80);
    assert_eq!(
        open_rows,
        permission_surface_line_count(&closed, Path::new("/tmp/project"), 80) + 3,
        "the note editor uses its built-in leading and trailing scroll rows"
    );

    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.surfaces.push(surface);
    assert_eq!(
        app.control_heights(80).2,
        u16::try_from(open_rows).unwrap(),
        "permission note panels must not add padding outside the scroll view"
    );
}

#[test]
fn permission_denial_note_preserves_multiline_paste() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.surfaces.push(Surface::Permission {
        request: Box::new(permission_request(None)),
        selected: 2,
        scope: cagent_agent::permissions::PermissionScope::Project,
        diff_scroll: 0,
        denial_note: String::new(),
        editing_note: true,
        note_cursor: 0,
    });

    assert!(app.insert_surface_paste("too broad\r\nlimit it to src"));
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Permission {
            denial_note,
            note_cursor,
            ..
        }) if denial_note == "too broad\nlimit it to src" && *note_cursor == denial_note.len()
    ));
}

#[test]
fn permission_status_only_offers_a_denial_note_for_deny() {
    let allow_once = Surface::Permission {
        request: Box::new(permission_request(None)),
        selected: 0,
        scope: cagent_agent::permissions::PermissionScope::Project,
        diff_scroll: 0,
        denial_note: String::new(),
        editing_note: false,
        note_cursor: 0,
    };
    assert!(!surface_status(&allow_once).contains("denial note"));

    let deny = Surface::Permission {
        request: Box::new(permission_request(None)),
        selected: 2,
        scope: cagent_agent::permissions::PermissionScope::Project,
        diff_scroll: 0,
        denial_note: String::new(),
        editing_note: false,
        note_cursor: 0,
    };
    assert!(surface_status(&deny).contains("Enter/Esc deny"));
    assert!(surface_status(&deny).contains("Tab add denial note"));
}

#[test]
fn usage_surface_renders_windows_actions_and_breakdowns() {
    let mut overview = cagent_agent::UsageOverview::default();
    overview.total.total_tokens = Some(1_250);
    let lines = surface_lines(
        &Surface::Usage {
            overview,
            list: ListState::selectable(4),
        },
        Path::new("."),
        100,
    );
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!text.contains("rolling UTC windows"));
    assert!(text.contains("Current conversation"));
    assert!(text.contains("Last 24 hours"));
    assert!(text.contains("Total"));
    assert!(text.contains("View per project"));
    assert!(text.contains("View per model"));

    let lines = surface_lines(
        &Surface::UsageBreakdown {
            kind: UsageBreakdownKind::Model,
            rows: vec![cagent_agent::UsageBreakdown {
                label: "openai/gpt-test".into(),
                usage: cagent_agent::protocol::SessionUsage::default(),
            }],
            list: ListState::selectable(1),
        },
        Path::new("."),
        80,
    );
    assert!(
        lines
            .iter()
            .any(|line| line.to_string().contains("openai/gpt-test"))
    );
}

#[test]
fn bash_permission_surface_wraps_commands_inside_the_gutter() {
    let mut request = permission_request(None);
    let InteractionRequestKind::PermissionApproval { resource, .. } = &mut request.kind else {
        panic!("permission_request returns a permission approval");
    };
    resource.tool = "bash".into();
    resource.raw_command = Some(format!("echo {} first second third fourth", "a".repeat(80)));

    let surface = Surface::Permission {
        request: Box::new(request),
        selected: 0,
        scope: cagent_agent::permissions::PermissionScope::Project,
        diff_scroll: 0,
        denial_note: String::new(),
        editing_note: false,
        note_cursor: 0,
    };
    let command_rows = surface_lines(&surface, Path::new("/tmp/project"), 24)
        .into_iter()
        .map(|line| line.to_string())
        .filter(|line| line.starts_with("  │ "))
        .collect::<Vec<_>>();

    assert!(command_rows.len() > 2);
    assert!(command_rows.iter().all(|line| line.starts_with("  │ ")));
    assert!(command_rows.iter().all(|line| line.width() <= 24));
    assert!(command_rows.iter().any(|line| line.ends_with("third")));
    assert!(command_rows.iter().any(|line| line.ends_with("fourth")));
}

#[test]
fn permission_surface_keeps_choices_visible_when_its_command_is_tall() {
    let mut request = permission_request(None);
    let InteractionRequestKind::PermissionApproval { resource, .. } = &mut request.kind else {
        panic!("permission_request returns a permission approval");
    };
    resource.tool = "bash".into();
    resource.raw_command = Some(
        (0..20)
            .map(|line| format!("echo {line}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );

    let lines = surface_lines_with_viewport(
        &Surface::Permission {
            request: Box::new(request),
            selected: 0,
            scope: cagent_agent::permissions::PermissionScope::Project,
            diff_scroll: 0,
            denial_note: String::new(),
            editing_note: false,
            note_cursor: 0,
        },
        Path::new("/tmp/project"),
        80,
        10,
    );
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(lines.len() <= 10);
    assert!(text.contains("…"));
    assert!(text.contains("Allow"));
}

#[test]
fn plan_completion_surface_only_renders_the_decision() {
    let surface = Surface::PlanCompletion {
        request: Box::new(InteractionRequest {
            id: cagent_agent::protocol::InteractionRequestId::new(),
            origin: None,
            kind: InteractionRequestKind::PlanCompletion {
                plan: "# Authentication\n\n## Summary\n\nUse PKCE.".into(),
                implementation_modes: vec!["edit".into(), "auto".into()],
                default_mode: "edit".into(),
            },
        }),
        selected: 0,
        note: String::new(),
        editing_note: false,
        note_cursor: 0,
        implementation_mode_index: 0,
        implementation_mode_colors: vec![
            StatusLineColor::LightGreen,
            StatusLineColor::LightMagenta,
        ],
        implementation_model: "mock/echo".into(),
        context_percent: Some(42),
    };

    let rendered = surface_lines(&surface, Path::new("/tmp/project"), 80)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    assert!(rendered.contains("Plan"));
    assert!(!rendered.contains("Authentication"));
    assert!(rendered.contains("Yes, with edit and mock/echo"));
    assert!(rendered.contains("Yes, with edit and mock/echo and clear context"));
    assert!(rendered.contains("Yes, with edit and mock/echo and compact context"));
    let clear_context = surface_lines(&surface, Path::new("/tmp/project"), 80)
        .into_iter()
        .find(|line| line.to_string().contains("context used:"))
        .expect("context usage on ordinary plan choice");
    assert_eq!(
        clear_context.spans.last().unwrap().content,
        " context used: 42%"
    );
    assert!(
        clear_context
            .spans
            .last()
            .unwrap()
            .style
            .add_modifier
            .contains(Modifier::DIM)
    );
    let prompt = surface_lines(&surface, Path::new("/tmp/project"), 80)
        .into_iter()
        .find(|line| line.to_string().contains("Implement this plan?"))
        .expect("plan prompt");
    assert!(prompt.spans[0].style.add_modifier.contains(Modifier::BOLD));
    let edit = surface_lines(&surface, Path::new("/tmp/project"), 80)
        .into_iter()
        .flat_map(|line| line.spans)
        .find(|span| span.content == "edit")
        .expect("plan mode should be rendered as a colored span");
    assert_eq!(edit.style.fg, Some(Color::LightGreen));
    let selected_choice = surface_lines(&surface, Path::new("/tmp/project"), 80)
        .into_iter()
        .find(|line| line.to_string().contains("Yes, with edit and mock/echo"))
        .expect("selected plan choice");
    assert_eq!(selected_choice.spans[1].style.fg, Some(Color::Cyan));
    assert_eq!(selected_choice.spans[2].style.fg, Some(Color::LightGreen));
    let lines = surface_lines(&surface, Path::new("/tmp/project"), 80);
    assert_eq!(lines.last().map(ToString::to_string).as_deref(), Some(""));
    assert_ne!(lines[lines.len() - 2].to_string(), "");
}

#[test]
fn clicking_the_clear_context_plan_choice_selects_it() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "plan",
        None,
    );
    app.surfaces.push(Surface::PlanCompletion {
        request: Box::new(InteractionRequest {
            id: cagent_agent::protocol::InteractionRequestId::new(),
            origin: None,
            kind: InteractionRequestKind::PlanCompletion {
                plan: "# Plan".into(),
                implementation_modes: vec!["edit".into()],
                default_mode: "edit".into(),
            },
        }),
        selected: 0,
        note: String::new(),
        editing_note: false,
        note_cursor: 0,
        implementation_mode_index: 0,
        implementation_mode_colors: vec![StatusLineColor::LightGreen],
        implementation_model: "mock/echo".into(),
        context_percent: None,
    });

    let width = 80;
    let height = 24;
    let panel_top = height - app.controls_height(width, height);
    // Two sticky rows precede the anchored panel; the second plan choice is
    // panel row five after its title/question rows.
    app.select_mouse_at(width, height, panel_top + 7, 3);

    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::PlanCompletion { selected: 1, .. })
    ));
}

#[test]
fn clicking_a_bash_output_preview_opens_the_full_screen_output() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.bash_output_hits.push(BashOutputHit {
        row: 4,
        terminal_id: None,
        command: "echo hello".into(),
        output: "hello\ncomplete output\n".into(),
        ansi_output: "hello\ncomplete output\n".into(),
        completion: None,
    });

    app.select_mouse_at(80, 24, 4, 8);

    let Some(Surface::Expanded {
        view: ExpandedView::Terminal {
            command, output, ..
        },
        scroll,
        viewport_rows,
        ..
    }) = app.surfaces.last()
    else {
        panic!("full-screen output should open");
    };
    assert_eq!(command, "echo hello");
    assert_eq!(output, "hello\ncomplete output\n");
    assert_eq!(*scroll, 0);
    assert_eq!(*viewport_rows, 18);
    let lines = surface_lines(app.surfaces.last().unwrap(), &app.workspace, 80);
    assert_eq!(lines[0].to_string(), "  Bash output  echo hello · running");
    assert_eq!(lines[0].spans.last().unwrap().style, DIM_STYLE);
    assert!(
        lines[0]
            .spans
            .iter()
            .any(|span| span.content == " · " && span.style == DIM_STYLE)
    );
    assert_ne!(lines[0].spans[3].style, Style::default());
    assert_eq!(
        surface_status(app.surfaces.last().unwrap()),
        "↑/↓ scroll · PgUp/PgDn page · Home/End jump · Enter/Esc close"
    );
    assert_eq!(tool_output_max_scroll(output, *viewport_rows), 0);
    assert_eq!(tool_output_max_scroll("1\n2\n3\n4", 2), 2);
}

#[test]
fn clicking_a_web_search_query_opens_the_result_view() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.web_search_result_hits.push(WebSearchResultHit {
        row: 4,
        provider: "exa".into(),
        query: "rust async".into(),
        results: vec![cagent_agent::web_search::WebSearchResult {
            title: "Async Rust".into(),
            url: "https://example.com/async".into(),
            snippet: "A result.".into(),
            published_at: None,
        }],
    });

    app.select_mouse_at(80, 24, 4, 8);

    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Expanded {
            view: ExpandedView::WebSearch { query, results, .. },
            ..
        })
            if query == "rust async" && results.len() == 1
    ));
}

#[test]
fn clicking_a_web_fetch_preview_opens_the_shared_expanded_view() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.web_fetch_output_hits.push(WebFetchOutputHit {
        row: 4,
        url: "https://example.com/article".into(),
        redirected_url: Some("https://www.example.com/article".into()),
        format: cagent_agent::WebFetchFormat::Markdown,
        output: "# Article".into(),
    });

    app.select_mouse_at(80, 24, 4, 8);

    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Expanded {
            view: ExpandedView::WebFetch { url, redirected_url, output, .. },
            ..
        }) if url == "https://example.com/article"
            && redirected_url.as_deref() == Some("https://www.example.com/article")
            && output == "# Article"
    ));
}

#[test]
fn expanded_web_fetch_header_shows_only_origin_and_final_destination() {
    let surface = Surface::Expanded {
        view: ExpandedView::WebFetch {
            url: "http://example.com/start".into(),
            redirected_url: Some("https://www.example.com/final".into()),
            format: cagent_agent::WebFetchFormat::Text,
            output: "result".into(),
        },
        scroll: 0,
        viewport_rows: 10,
    };

    let header = surface_lines(&surface, Path::new("/tmp/project"), 120)
        .into_iter()
        .next()
        .unwrap();
    assert!(
        header
            .to_string()
            .contains("http://example.com/start → https://www.example.com/final")
    );
    assert!(
        header
            .spans
            .iter()
            .any(|span| span.content == " → " && span.style == DIM_STYLE)
    );
}

#[test]
fn clicking_a_web_search_query_inside_a_subagent_log_opens_the_same_view() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.render_width = 80;
    app.render_height = 24;
    let result = cagent_agent::web_search::WebSearchResult {
        title: "Async Rust".into(),
        url: "https://example.com/async".into(),
        snippet: "A result.".into(),
        published_at: None,
    };
    let run = cagent_agent::protocol::AgentRun {
        id: cagent_agent::protocol::AgentRunId::new(),
        conversation_id: cagent_agent::protocol::ConversationId::new(),
        parent_turn_id: cagent_agent::protocol::TurnId::new(),
        sequence: 0,
        profile: "explore".into(),
        model: cagent_agent::provider::ModelRef::parse("mock/echo").unwrap(),
        effort: None,
        task: "Research async Rust".into(),
        status: cagent_agent::protocol::AgentRunStatus::Completed,
        result: None,
        error: None,
        usage: Some(cagent_agent::provider::ModelUsage {
            input_tokens: Some(1_000),
            output_tokens: Some(250),
            total_tokens: Some(1_250),
            ..Default::default()
        }),
        created_at: "0".into(),
        started_at: Some("0".into()),
        completed_at: Some("1".into()),
        timeline: Vec::new(),
        activity: vec![cagent_agent::protocol::AgentRunActivity {
            sequence: 1,
            tool: "web_search".into(),
            arguments: serde_json::json!({ "query": "rust async" }),
            output: serde_json::json!({
                "provider": "exa",
                "results": [result],
            }),
            is_error: false,
            permission_audit: None,
            created_at: "1".into(),
        }],
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
        viewport_rows: 18,
    });
    let lines = surface_lines_with_viewport(
        app.surfaces.last().unwrap(),
        &app.workspace,
        app.render_width,
        20,
    );
    let query_row = lines
        .iter()
        .position(|line| line.to_string().contains("rust async"))
        .unwrap();

    // The expanded panel starts below the indicator and composer-padding rows.
    app.select_mouse_at(80, 24, u16::try_from(query_row + 2).unwrap(), 8);

    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Expanded {
            view: ExpandedView::WebSearch { provider, query, results },
            ..
        }) if provider == "exa" && query == "rust async" && results.len() == 1
    ));
}
