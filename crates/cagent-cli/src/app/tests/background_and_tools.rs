use super::*;

#[test]
fn onboarding_records_dismissal_and_completion_as_separate_outcomes() {
    let mut app = App::new(
        Path::new("/workspace"),
        ("mock".into(), "echo".into(), None),
        BTreeSet::new(),
        "ask",
        None,
    );

    app.begin_onboarding();
    assert!(matches!(app.surfaces.last(), Some(Surface::Onboarding)));
    app.dismiss_onboarding();
    assert_eq!(
        app.take_onboarding_outcome(),
        Some(OnboardingOutcome::Dismissed)
    );
    assert!(!app.onboarding);

    app.begin_onboarding();
    app.complete_onboarding();
    assert_eq!(
        app.take_onboarding_outcome(),
        Some(OnboardingOutcome::Completed)
    );
    assert!(!app.onboarding);
}

#[test]
fn terminal_update_inserts_and_replaces_its_background_work_row() {
    let node_id = cagent_agent::protocol::NodeId::new();
    let terminal = cagent_agent::tools::TerminalSnapshot {
        id: cagent_agent::tools::TerminalId::new(),
        owner: cagent_agent::protocol::ConversationId::new(),
        owner_agent_run_id: None,
        tool_call_node_id: Some(node_id),
        read_safe: None,
        command: "printf fast".into(),
        status: cagent_agent::tools::TerminalStatus::Running,
        created_at: "3".into(),
        started_at: "3".into(),
        completed_at: None,
        exit_code: None,
        output_base: 0,
        output_cursor: 10,
        output_bytes: 10,
        discarded_bytes: 0,
        truncated: false,
        ansi_output: "one\ntwo\n… 1 lines omitted …\nfour\nfive".into(),
        output: "one\ntwo\n… 1 lines omitted …\nfour\nfive".into(),
    };
    let mut app = App::new(
        Path::new("/workspace"),
        ("mock".into(), "echo".into(), None),
        BTreeSet::new(),
        "ask",
        None,
    );
    app.supervised_work
        .rows
        .push(cagent_agent::presentation::SupervisedWork::Agent {
            run: Box::new(cagent_agent::protocol::AgentRun {
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
                created_at: "2".into(),
                started_at: Some("2".into()),
                completed_at: None,
                timeline: Vec::new(),
                activity: Vec::new(),
            }),
        });
    app.history = vec![
        test_lines(vec![Line::from("unchanged")]),
        test_tool_groups(vec![cagent_agent::presentation::ToolActivityGroup::Bash {
            node_id: Some(node_id),
            terminal_id: None,
            command: "printf fast".into(),
            status: cagent_agent::presentation::ToolActivityStatus::Pending,
            output: None,
            ansi_output: None,
            exit_code: None,
        }]),
    ];
    app.history[0].id =
        cagent_agent::protocol::TranscriptBlockId::node(cagent_agent::protocol::NodeId::new());
    app.history[1].id = cagent_agent::protocol::TranscriptBlockId::derived("tools", node_id);
    app.ensure_history_layout(80);
    assert_eq!(app.history_block_layouts.len(), 2);
    let rows = app.supervised_work.rows();
    app.surfaces.push(Surface::SupervisedWork {
        rows,
        list: ListState::selectable(1),
        show_past: false,
    });

    app.apply_terminal_update(&cagent_agent::protocol::TerminalTranscriptUpdate {
        id: cagent_agent::protocol::TranscriptBlockId::derived("terminal", node_id),
        terminal: terminal.clone(),
    });

    let terminal_id = terminal.id;
    assert_eq!(app.supervised_work.rows.len(), 2);
    assert!(matches!(
        &app.supervised_work.rows[0],
        cagent_agent::presentation::SupervisedWork::Terminal { terminal }
            if terminal.id == terminal_id && terminal.command == "printf fast"
    ));
    assert!(matches!(
        &app.supervised_work.rows[1],
        cagent_agent::presentation::SupervisedWork::Agent { run } if run.created_at == "2"
    ));
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::SupervisedWork { list, .. }) if list.selected == Some(1)
    ));

    let mut updated_terminal = terminal;
    updated_terminal.output = "latest output".into();
    app.apply_terminal_update(&cagent_agent::protocol::TerminalTranscriptUpdate {
        id: cagent_agent::protocol::TranscriptBlockId::derived("terminal", node_id),
        terminal: updated_terminal,
    });
    assert_eq!(app.supervised_work.rows.len(), 2);
    assert!(matches!(
        &app.supervised_work.rows[0],
        cagent_agent::presentation::SupervisedWork::Terminal { terminal }
            if terminal.id == terminal_id && terminal.output == "latest output"
    ));

    assert_eq!(app.history.len(), 2);
    assert_eq!(app.history_block_layouts.len(), 1);
    assert!(matches!(
        &app.history[0].kind,
        cagent_agent::protocol::TranscriptBlockKind::Assistant { source, .. }
            if source == "unchanged"
    ));
    let cagent_agent::protocol::TranscriptBlockKind::ToolGroups { groups } = &app.history[1].kind
    else {
        panic!("Bash activity card");
    };
    let cagent_agent::presentation::ToolActivityGroup::Bash { output, .. } = &groups[0] else {
        panic!("Bash activity");
    };
    assert_eq!(output.as_deref(), Some("latest output"));
}

#[test]
fn fast_read_terminal_is_hidden_from_task_ui() {
    let mut app = App::new(
        Path::new("/workspace"),
        ("mock".into(), "echo".into(), None),
        BTreeSet::new(),
        "ask",
        None,
    );
    app.supervised_work
        .rows
        .push(cagent_agent::presentation::SupervisedWork::Terminal {
            terminal: Box::new(cagent_agent::tools::TerminalSnapshot {
                id: cagent_agent::tools::TerminalId::new(),
                owner: cagent_agent::protocol::ConversationId::new(),
                owner_agent_run_id: None,
                tool_call_node_id: None,
                read_safe: Some(true),
                command: "cat README.md".into(),
                status: cagent_agent::tools::TerminalStatus::Exited,
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
            }),
        });

    assert!(!app.supervised_work.has_work());
    assert_eq!(app.supervised_work.running_terminal_count(), 0);
    assert!(!app.status_line(80).to_string().contains("background"));

    let cagent_agent::presentation::SupervisedWork::Terminal { terminal } =
        &mut app.supervised_work.rows[0]
    else {
        unreachable!();
    };
    terminal.completed_at = Some("2000".into());
    assert!(app.supervised_work.has_work());
    assert_eq!(app.supervised_work.rows().len(), 1);
}

#[test]
fn background_work_footer_advertises_kill() {
    let active = super::surfaces::surface_status(&Surface::SupervisedWork {
        rows: Vec::new(),
        list: ListState::selectable(0),
        show_past: false,
    });
    assert!(active.contains("k kill"));

    let past = super::surfaces::surface_status(&Surface::SupervisedWork {
        rows: Vec::new(),
        list: ListState::selectable(0),
        show_past: true,
    });
    assert!(!past.contains("k kill"));
}

#[tokio::test]
async fn alt_down_opens_background_before_queue_navigation() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = AgentRuntime::open(cagent_agent::runtime::RuntimeOptions::new(
        temporary.path().join("background-shortcut.db"),
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
        BTreeSet::new(),
        "ask",
        None,
    );
    app.supervised_work
        .rows
        .push(cagent_agent::presentation::SupervisedWork::Terminal {
            terminal: Box::new(cagent_agent::tools::TerminalSnapshot {
                id: cagent_agent::tools::TerminalId::new(),
                owner: session.id(),
                owner_agent_run_id: None,
                tool_call_node_id: Some(cagent_agent::protocol::NodeId::new()),
                read_safe: None,
                command: "sleep 1".into(),
                status: cagent_agent::tools::TerminalStatus::Running,
                created_at: "2026-08-11T00:00:00Z".into(),
                started_at: "2026-08-11T00:00:00Z".into(),
                completed_at: None,
                exit_code: None,
                output_base: 0,
                output_cursor: 0,
                output_bytes: 0,
                discarded_bytes: 0,
                truncated: false,
                ansi_output: String::new(),
                output: String::new(),
            }),
        });
    for position in 0..2 {
        app.queued.push(QueuedMessage {
            id: cagent_agent::protocol::QueuedMessageId::new(),
            position,
            target: QueueTarget::NextBoundary,
            kind: cagent_agent::protocol::QueuedItemKind::Prompt,
            mode: None,
            command_text: None,
            text: format!("queued {position}"),
            attachments: Vec::new(),
            images: Vec::new(),
            image_chips: Vec::new(),
            blocked_by_startup: false,
            require_subagent: false,
        });
    }
    app.push_user_with_label(None, "first", &[]);
    app.push_assistant(
        cagent_agent::presentation::parse_markdown("answer"),
        "answer".into(),
        None,
    );
    app.push_user_with_label(None, "second", &[]);

    app.handle_key(&session, KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert!(!app.follow_history_tail);
    app.handle_key(
        &session,
        KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL),
    )
    .await
    .unwrap();
    assert!(app.follow_history_tail);

    app.handle_key(&session, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.surfaces.is_empty());
    assert!(app.selected_queue.is_none());

    app.handle_key(&session, KeyEvent::new(KeyCode::Down, KeyModifiers::ALT))
        .await
        .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::SupervisedWork { .. })
    ));
    assert!(app.selected_queue.is_none());

    app.handle_key(&session, KeyEvent::new(KeyCode::Up, KeyModifiers::ALT))
        .await
        .unwrap();
    assert!(app.surfaces.is_empty());
    assert!(app.selected_queue.is_none());

    app.handle_key(&session, KeyEvent::new(KeyCode::Up, KeyModifiers::ALT))
        .await
        .unwrap();
    assert_eq!(app.selected_queue, Some(1));

    app.handle_key(&session, KeyEvent::new(KeyCode::Up, KeyModifiers::ALT))
        .await
        .unwrap();
    assert_eq!(app.selected_queue, Some(0));

    app.handle_key(&session, KeyEvent::new(KeyCode::Down, KeyModifiers::ALT))
        .await
        .unwrap();
    assert_eq!(app.selected_queue, Some(1));

    app.handle_key(&session, KeyEvent::new(KeyCode::Down, KeyModifiers::ALT))
        .await
        .unwrap();
    assert!(app.selected_queue.is_none());

    app.handle_key(&session, KeyEvent::new(KeyCode::Down, KeyModifiers::ALT))
        .await
        .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::SupervisedWork { .. })
    ));
}

#[tokio::test]
async fn background_k_opens_stable_confirmation_for_agent_and_terminal() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = AgentRuntime::open(cagent_agent::runtime::RuntimeOptions::new(
        temporary.path().join("background-stop.db"),
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
        BTreeSet::new(),
        "ask",
        None,
    );
    let agent_id = cagent_agent::protocol::AgentRunId::new();
    app.surfaces.push(Surface::SupervisedWork {
        rows: vec![cagent_agent::presentation::SupervisedWork::Agent {
            run: Box::new(cagent_agent::protocol::AgentRun {
                id: agent_id,
                conversation_id: session.id(),
                parent_turn_id: cagent_agent::protocol::TurnId::new(),
                sequence: 0,
                profile: "explore".into(),
                model: cagent_agent::provider::ModelRef::parse("mock/echo").unwrap(),
                effort: None,
                task: "inspect".into(),
                status: cagent_agent::protocol::AgentRunStatus::Running,
                result: None,
                error: None,
                usage: None,
                created_at: "1".into(),
                started_at: Some("1".into()),
                completed_at: None,
                timeline: Vec::new(),
                activity: Vec::new(),
            }),
        }],
        list: ListState::selectable(1),
        show_past: false,
    });
    app.handle_surface_key(
        &session,
        KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
    )
    .await
    .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::KillSupervisedWork {
            target: cagent_agent::runtime::SupervisedWorkTarget::Agent(id),
            selected: 0,
        }) if *id == agent_id
    ));
    app.handle_surface_key(&session, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::SupervisedWork { .. })
    ));
    let terminal_id = cagent_agent::tools::TerminalId::new();
    if let Some(Surface::SupervisedWork { rows, list, .. }) = app.surfaces.last_mut() {
        *rows = vec![cagent_agent::presentation::SupervisedWork::Terminal {
            terminal: Box::new(cagent_agent::tools::TerminalSnapshot {
                id: terminal_id,
                owner: session.id(),
                owner_agent_run_id: None,
                tool_call_node_id: None,
                read_safe: None,
                command: "sleep 1".into(),
                status: cagent_agent::tools::TerminalStatus::Running,
                created_at: "2".into(),
                started_at: "2".into(),
                completed_at: None,
                exit_code: None,
                output_base: 0,
                output_cursor: 0,
                output_bytes: 0,
                discarded_bytes: 0,
                truncated: false,
                ansi_output: String::new(),
                output: String::new(),
            }),
        }];
        *list = ListState::selectable(1);
    }
    app.handle_surface_key(
        &session,
        KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
    )
    .await
    .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::KillSupervisedWork {
            target: cagent_agent::runtime::SupervisedWorkTarget::Terminal(id),
            selected: 0,
        }) if *id == terminal_id
    ));
    let Some(Surface::SupervisedWork { rows, .. }) = app.surfaces.get(app.surfaces.len() - 2)
    else {
        panic!("background browser remains below confirmation");
    };
    assert!(!super::surface_control::background_work_remains_after_kill(
        rows,
        cagent_agent::runtime::SupervisedWorkTarget::Terminal(terminal_id),
    ));
    app.handle_surface_key(&session, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_surface_key(&session, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::SupervisedWork { .. })
    ));
}

#[tokio::test]
async fn background_k_ignores_past_rows_and_observers() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = AgentRuntime::open(cagent_agent::runtime::RuntimeOptions::new(
        temporary.path().join("background-stop-passive.db"),
    ))
    .await
    .unwrap();
    let session = runtime
        .create_session(cagent_agent::runtime::NewSession {
            workspace: temporary.path().to_path_buf(),
        })
        .await
        .unwrap();
    let terminal = cagent_agent::tools::TerminalSnapshot {
        id: cagent_agent::tools::TerminalId::new(),
        owner: session.id(),
        owner_agent_run_id: None,
        tool_call_node_id: None,
        read_safe: None,
        command: "sleep 1".into(),
        status: cagent_agent::tools::TerminalStatus::Exited,
        created_at: "1".into(),
        started_at: "1".into(),
        completed_at: Some("2".into()),
        exit_code: Some(0),
        output_base: 0,
        output_cursor: 0,
        output_bytes: 0,
        discarded_bytes: 0,
        truncated: false,
        ansi_output: String::new(),
        output: String::new(),
    };
    let mut app = App::new(
        temporary.path(),
        ("mock".into(), "echo".into(), None),
        BTreeSet::new(),
        "ask",
        None,
    );
    app.surfaces.push(Surface::SupervisedWork {
        rows: vec![cagent_agent::presentation::SupervisedWork::Terminal {
            terminal: Box::new(terminal),
        }],
        list: ListState::selectable(1),
        show_past: true,
    });
    app.handle_surface_key(
        &session,
        KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
    )
    .await
    .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::SupervisedWork { .. })
    ));

    app.access = cagent_agent::protocol::SessionAccess::Observer {
        takeover_available: false,
    };
    app.handle_surface_key(
        &session,
        KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
    )
    .await
    .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::SupervisedWork { .. })
    ));
}

#[test]
fn delegated_terminal_update_stays_out_of_the_main_transcript() {
    let node_id = cagent_agent::protocol::NodeId::new();
    let terminal = cagent_agent::tools::TerminalSnapshot {
        id: cagent_agent::tools::TerminalId::new(),
        owner: cagent_agent::protocol::ConversationId::new(),
        owner_agent_run_id: Some(cagent_agent::protocol::AgentRunId::new()),
        tool_call_node_id: None,
        read_safe: None,
        command: "echo child".into(),
        status: cagent_agent::tools::TerminalStatus::Exited,
        created_at: "1".into(),
        started_at: "1".into(),
        completed_at: Some("2".into()),
        exit_code: Some(0),
        output_base: 0,
        output_cursor: 11,
        output_bytes: 11,
        discarded_bytes: 0,
        truncated: false,
        ansi_output: "child output\n".into(),
        output: "child output\n".into(),
    };
    let mut app = App::new(
        Path::new("/workspace"),
        ("mock".into(), "echo".into(), None),
        BTreeSet::new(),
        "ask",
        None,
    );
    app.history = vec![test_tool_groups(vec![
        cagent_agent::presentation::ToolActivityGroup::Bash {
            node_id: Some(node_id),
            terminal_id: None,
            command: "echo child".into(),
            status: cagent_agent::presentation::ToolActivityStatus::Pending,
            output: None,
            ansi_output: None,
            exit_code: None,
        },
    ])];
    app.history[0].id = cagent_agent::protocol::TranscriptBlockId::derived("tools", node_id);

    app.apply_delegated_terminal_update(&terminal);

    let cagent_agent::protocol::TranscriptBlockKind::ToolGroups { groups } = &app.history[0].kind
    else {
        panic!("expected the main Bash group");
    };
    assert!(matches!(
        &groups[0],
        cagent_agent::presentation::ToolActivityGroup::Bash {
            status: cagent_agent::presentation::ToolActivityStatus::Pending,
            output: None,
            ..
        }
    ));
    assert!(app.supervised_work.rows.iter().any(|work| matches!(
        work,
        cagent_agent::presentation::SupervisedWork::Terminal { terminal: retained }
            if retained.id == terminal.id
    )));
}

#[test]
fn terminal_detail_normalizes_ansi_and_keeps_surface_shape_consistent() {
    let terminal_id = cagent_agent::tools::TerminalId::new();
    let retained = App::terminal_detail(
        Some(terminal_id),
        "printf hi".into(),
        "plain output".into(),
        "\x1b[32mplain output\x1b[0m".into(),
        Some("[exited with status 0]".into()),
    )
    .into_surface(80, 24);
    let cached = App::terminal_detail(
        None,
        "printf hi".into(),
        "plain output".into(),
        String::new(),
        Some("[exited with status 0]".into()),
    )
    .into_surface(80, 24);

    let terminal_data = |surface: Surface| {
        let Surface::Expanded {
            view:
                ExpandedView::Terminal {
                    command,
                    output,
                    ansi_output,
                    completion,
                    ..
                },
            viewport_rows,
            ..
        } = surface
        else {
            panic!("expected terminal surface");
        };
        (command, output, ansi_output, completion, viewport_rows)
    };

    let (command, output, ansi_output, completion, viewport_rows) = terminal_data(retained);
    assert_eq!(command, "printf hi");
    assert_eq!(output, "plain output");
    assert_eq!(ansi_output, "\x1b[32mplain output\x1b[0m");
    assert_eq!(completion.as_deref(), Some("[exited with status 0]"));
    assert_eq!(viewport_rows, 18);

    let (_, output, ansi_output, completion, viewport_rows) = terminal_data(cached);
    assert_eq!(output, "plain output");
    assert_eq!(ansi_output, output);
    assert_eq!(completion.as_deref(), Some("[exited with status 0]"));
    assert_eq!(viewport_rows, 18);
}

pub(super) fn mcp_test_definition() -> cagent_agent::mcp::McpServerDefinition {
    cagent_agent::mcp::McpServerDefinition {
        transport: cagent_agent::mcp::McpTransportConfig::Stdio {
            command: "docs-mcp".into(),
            args: vec!["serve".into()],
            cwd: None,
            env: BTreeMap::new(),
            env_remove: Vec::new(),
            inherit_env: true,
        },
        enabled: true,
        agents: Vec::new(),
        eager: false,
        startup_timeout_seconds: 10,
        request_timeout_seconds: 60,
        read_only_tools: vec!["search".into()],
        ..cagent_agent::mcp::McpServerDefinition::default()
    }
}

#[test]
fn mcp_main_orders_close_add_and_effective_servers_with_override_detail() {
    let surface = Surface::McpServers {
        rows: vec![cagent_agent::mcp::McpEffectiveServer {
            name: "docs".into(),
            location: cagent_agent::mcp::McpLocation::project(),
            definition: mcp_test_definition(),
            status: cagent_agent::mcp::McpRuntimeStatus::Connected,
            agents: Vec::new(),
            allowed_for_agent: true,
            overridden: vec![cagent_agent::mcp::McpLocation::global()],
            tools: Vec::new(),
            diagnostics: Vec::new(),
            generation: 1,
        }],
        list: ListState::selectable(3),
    };
    let lines = surface_lines(&surface, Path::new("/workspace"), 80);
    assert!(lines.iter().flat_map(|line| &line.spans).any(|span| {
        span.content.as_ref() == "Connected" && span.style == super::ENABLED_STYLE
    }));
    let rendered = lines
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>();
    let close = rendered
        .iter()
        .position(|line| line.contains("Close"))
        .unwrap();
    let add = rendered
        .iter()
        .position(|line| line.contains("Add"))
        .unwrap();
    let server = rendered
        .iter()
        .position(|line| line.contains("docs"))
        .unwrap();
    assert!(close < add && add < server);
    assert!(rendered[server].contains("Project"));
    assert!(rendered[server].contains("Connected"));
    assert!(rendered[server].contains("overrides 1"));
}

#[test]
fn mcp_structured_and_raw_surfaces_expose_nested_fields_and_fit_narrow_layouts() {
    let draft = McpFormDraft {
        original: None,
        location: cagent_agent::mcp::McpLocation::global(),
        name: "docs".into(),
        definition: mcp_test_definition(),
    };
    let form = [0, 8, 14]
        .into_iter()
        .flat_map(|selected| {
            surface_lines(
                &Surface::McpForm {
                    draft: Box::new(draft.clone()),
                    list: ListState::selectable_at(15, selected, VISIBLE_MENU_ITEMS),
                },
                Path::new("/workspace"),
                80,
            )
        })
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    for field in [
        "Arguments",
        "Environment",
        "Removed variables",
        "Read-only tools",
        "Startup timeout",
        "Request timeout",
    ] {
        assert!(form.contains(field), "missing {field}");
    }

    let json = Surface::McpJsonEdit {
        location: cagent_agent::mcp::McpLocation::global(),
        separate_name: None,
        editor: MultilineInput::new(format!("{{\"long\":\"{}\"}}", "value".repeat(20)), 5),
    };
    assert!(
        surface_lines(&json, Path::new("/workspace"), 32)
            .iter()
            .all(|line| line.width() <= 32)
    );

    let source = (0..20)
        .map(|line| format!("\"field-{line}\": true"))
        .collect::<Vec<_>>()
        .join("\n");
    let json = Surface::McpJsonEdit {
        location: cagent_agent::mcp::McpLocation::global(),
        separate_name: None,
        editor: MultilineInput::new(source.clone(), source.len()),
    };
    let bounded = surface_lines_with_viewport(&json, Path::new("/workspace"), 40, 8);
    assert_eq!(bounded.len(), 8);
    assert!(
        bounded
            .iter()
            .any(|line| line.to_string().contains("field-19"))
    );
    assert!(bounded.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.style == SEARCH_CURSOR_STYLE)
    }));
}

#[tokio::test]
async fn mcp_editors_open_at_the_end_of_their_values() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = AgentRuntime::open(cagent_agent::runtime::RuntimeOptions::new(
        temporary.path().join("mcp-editor-cursor.db"),
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
    app.surfaces.push(Surface::McpForm {
        draft: Box::new(McpFormDraft {
            original: None,
            location: cagent_agent::mcp::McpLocation::global(),
            name: "docs".into(),
            definition: mcp_test_definition(),
        }),
        list: ListState::selectable_at(15, 2, VISIBLE_MENU_ITEMS),
    });

    app.handle_surface_key(&session, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::McpFieldEdit { value, cursor, .. })
            if value == "docs" && *cursor == value.len()
    ));

    app.surfaces.clear();
    app.surfaces.push(Surface::McpTransport {
        location: cagent_agent::mcp::McpLocation::global(),
        selected: 3,
    });
    app.handle_surface_key(&session, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::McpJsonEdit { editor, .. }) if editor.cursor() == editor.text().len()
    ));
}

#[test]
fn mcp_form_truncates_long_values_to_one_terminal_row() {
    let mut definition = mcp_test_definition();
    let cagent_agent::mcp::McpTransportConfig::Stdio { env, .. } = &mut definition.transport else {
        unreachable!("test definition is stdio");
    };
    env.insert("HOMEASSISTANT_TOKEN".into(), "token".repeat(40).into());
    let draft = McpFormDraft {
        original: None,
        location: cagent_agent::mcp::McpLocation::global(),
        name: "home-assistant".into(),
        definition,
    };

    let lines = surface_lines(
        &Surface::McpForm {
            draft: Box::new(draft),
            list: ListState::selectable_at(15, 12, VISIBLE_MENU_ITEMS),
        },
        Path::new("/workspace"),
        80,
    );

    assert!(lines.iter().all(|line| line.width() <= 80));
    assert!(lines.iter().any(|line| line.to_string().contains('…')));
}

#[test]
fn mcp_submenus_use_shared_lists_and_keep_a_line_below_inputs() {
    let server = cagent_agent::mcp::McpEffectiveServer {
        name: "docs".into(),
        location: cagent_agent::mcp::McpLocation::project(),
        definition: mcp_test_definition(),
        status: cagent_agent::mcp::McpRuntimeStatus::Connected,
        agents: Vec::new(),
        allowed_for_agent: true,
        overridden: Vec::new(),
        tools: Vec::new(),
        diagnostics: Vec::new(),
        generation: 1,
    };
    let menu = Surface::McpServer {
        server,
        selected: 0,
        oauth_connected: false,
    };
    let layout = super::surfaces::surface_layout_with_viewport(
        &menu,
        Path::new("/workspace"),
        80,
        usize::MAX,
    );
    assert_eq!(layout.hits[1], ListHit::None);
    assert_eq!(layout.hits[2], ListHit::Item(0));
    assert_eq!(layout.hits[7], ListHit::Item(5));
    assert_eq!(layout.hits[8], ListHit::None);

    let field = Surface::McpFieldEdit {
        draft: Box::new(McpFormDraft {
            original: None,
            location: cagent_agent::mcp::McpLocation::global(),
            name: "docs".into(),
            definition: mcp_test_definition(),
        }),
        field: McpFormField::Name,
        value: "docs".into(),
        cursor: 4,
    };
    assert_eq!(
        surface_lines(&field, Path::new("/workspace"), 80)
            .last()
            .map(ToString::to_string),
        Some(String::new())
    );

    let json = Surface::McpJsonEdit {
        location: cagent_agent::mcp::McpLocation::global(),
        separate_name: None,
        editor: MultilineInput::new("{}".into(), 1),
    };
    assert_eq!(
        surface_lines(&json, Path::new("/workspace"), 80)
            .last()
            .map(ToString::to_string),
        Some(String::new())
    );
}

#[test]
fn http_mcp_server_menu_exposes_oauth_login_actions() {
    let mut definition = mcp_test_definition();
    definition.transport = cagent_agent::mcp::McpTransportConfig::StreamableHttp {
        url: "https://example.com/mcp".into(),
        headers: BTreeMap::new(),
        allow_insecure: false,
    };
    let surface = Surface::McpServer {
        server: cagent_agent::mcp::McpEffectiveServer {
            name: "remote".into(),
            location: cagent_agent::mcp::McpLocation::global(),
            definition,
            status: cagent_agent::mcp::McpRuntimeStatus::NotStarted,
            agents: Vec::new(),
            allowed_for_agent: true,
            overridden: Vec::new(),
            tools: Vec::new(),
            diagnostics: Vec::new(),
            generation: 0,
        },
        selected: 0,
        oauth_connected: false,
    };

    let rendered = surface_lines(&surface, Path::new("/workspace"), 80)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("Login with OAuth"));
    assert!(!rendered.contains("Log out OAuth"));

    let layout = super::surfaces::surface_layout_with_viewport(
        &surface,
        Path::new("/workspace"),
        80,
        usize::MAX,
    );
    assert_eq!(layout.hits[2], ListHit::Item(0));
    assert_eq!(layout.hits[8], ListHit::Item(6));

    let mut connected = surface;
    let Surface::McpServer {
        oauth_connected, ..
    } = &mut connected
    else {
        unreachable!()
    };
    *oauth_connected = true;
    let rendered = surface_lines(&connected, Path::new("/workspace"), 80)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("Log out OAuth"));
    assert!(!rendered.contains("Login with OAuth"));
}

#[test]
fn mcp_oauth_browser_flow_uses_a_dedicated_surface() {
    let (_sender, completion) =
        tokio::sync::watch::channel(cagent_agent::mcp::McpOAuthCompletion::Waiting);
    let surface = Surface::McpOAuth {
        server: "remote".into(),
        label: "Remote".into(),
        attempt: cagent_agent::mcp::McpOAuthAttempt {
            prompt: cagent_agent::mcp::McpOAuthPrompt::Browser {
                authorization_url: "https://login.example.com/authorize".into(),
                callback_url: "http://127.0.0.1/callback".into(),
            },
            completion,
        },
    };
    let rendered = surface_lines(&surface, Path::new("/workspace"), 80)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("Configure MCP"));
    assert!(rendered.contains("https://login.example.com/authorize"));
    assert!(rendered.contains("Waiting for authentication"));
}

#[test]
fn completed_mcp_oauth_uses_a_continue_screen() {
    let (_sender, completion) =
        tokio::sync::watch::channel(cagent_agent::mcp::McpOAuthCompletion::Connected);
    let surface = Surface::McpOAuth {
        server: "github".into(),
        label: "GitHub".into(),
        attempt: cagent_agent::mcp::McpOAuthAttempt {
            prompt: cagent_agent::mcp::McpOAuthPrompt::Device {
                verification_url: "https://github.com/login/device".into(),
                user_code: "ABCD-1234".into(),
            },
            completion,
        },
    };
    let rendered = surface_lines(&surface, Path::new("/workspace"), 80)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("GitHub connected"));
    assert!(!rendered.contains("ABCD-1234"));
    assert_eq!(surface_status(&surface), "Enter/Esc continue");
}

#[test]
fn mcp_package_secret_editor_masks_secret_values() {
    let package = cagent_agent::mcp::builtin_packages()
        .into_iter()
        .find(|package| package.id == "github")
        .unwrap();
    let draft = McpPackageDraft {
        server_name: package.id.clone(),
        package,
        location: cagent_agent::mcp::McpLocation::global(),
        parameters: BTreeMap::new(),
        secret_statuses: BTreeMap::new(),
        secret_updates: BTreeMap::new(),
        installed: false,
    };
    let surface = Surface::McpPackageValueEdit {
        draft: Box::new(draft),
        field: McpPackageField::Secret("pat".into()),
        value: "never-render-this-token".into(),
        cursor: "never-render-this-token".len(),
    };
    let rendered = surface_lines(&surface, Path::new("/workspace"), 80)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!rendered.contains("never-render-this-token"));
    assert!(rendered.contains('•'));
    assert!(rendered.contains("create a fine-grained personal access token"));
    assert!(rendered.contains("https://github.com/settings/personal-access-tokens/new"));
}

#[test]
fn mcp_package_setup_lists_secrets_before_parameters() {
    let package = cagent_agent::mcp::builtin_packages()
        .into_iter()
        .find(|package| package.id == "github")
        .unwrap();
    let surface = Surface::McpPackageSetup {
        draft: Box::new(McpPackageDraft {
            server_name: package.id.clone(),
            package,
            location: cagent_agent::mcp::McpLocation::global(),
            parameters: BTreeMap::new(),
            secret_statuses: BTreeMap::new(),
            secret_updates: BTreeMap::new(),
            installed: false,
        }),
        selected: 0,
    };
    let rendered = surface_lines(&surface, Path::new("/workspace"), 160)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        rendered.find("GitHub personal access token").unwrap()
            < rendered.find("Enabled toolsets").unwrap()
    );
}

#[test]
fn mcp_mutation_preview_keeps_choices_visible_and_scrolls_bounded_details() {
    let preview = cagent_agent::mcp::McpMutationPreview {
        affected: (0..20).map(|index| format!("server-{index}")).collect(),
        ..Default::default()
    };
    let top = Surface::McpMutationPreview {
        preview: preview.clone(),
        list: ListState::selectable(2),
        details: ScrollViewState::new(20),
    };
    let layout =
        super::surfaces::surface_layout_with_viewport(&top, Path::new("/workspace"), 80, 12);
    let text = layout
        .lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(layout.lines.len(), 12);
    assert!(text.contains("Cancel"));
    assert!(text.contains("Apply"));
    assert!(text.contains("server-0"));
    assert!(!text.contains("server-19"));
    assert_eq!(layout.hits[2], ListHit::Item(0));
    assert_eq!(layout.hits[3], ListHit::Item(1));
    assert_eq!(
        layout.surface_hit(11),
        SurfaceHit::Scroll(ScrollViewHit::Indicator(ScrollViewAction::PageNext))
    );

    let bottom = Surface::McpMutationPreview {
        preview,
        list: ListState::selectable_at(2, 1, VISIBLE_MENU_ITEMS),
        details: ScrollViewState {
            offset: usize::MAX,
            content_rows: 20,
        },
    };
    let layout =
        super::surfaces::surface_layout_with_viewport(&bottom, Path::new("/workspace"), 80, 12);
    let text = layout
        .lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("Cancel"));
    assert!(text.contains("Apply"));
    assert!(text.contains("server-19"));
    assert!(!text.contains("server-0"));
    assert_eq!(
        layout.surface_hit(5),
        SurfaceHit::Scroll(ScrollViewHit::Indicator(ScrollViewAction::PagePrevious))
    );
}

#[test]
fn mcp_paste_targets_the_open_editor_instead_of_the_composer() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.surfaces.push(Surface::McpJsonEdit {
        location: cagent_agent::mcp::McpLocation::global(),
        separate_name: None,
        editor: MultilineInput::new("{}".into(), 1),
    });
    assert!(app.insert_surface_paste("\n  \"docs\": {}\n"));
    let Some(Surface::McpJsonEdit { editor, .. }) = app.surfaces.last() else {
        panic!("expected MCP JSON editor");
    };
    assert!(editor.text().contains("\"docs\""));
    assert!(app.draft.is_empty());
}

#[test]
fn surface_paste_uses_the_matching_shared_editor() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );

    app.surfaces.push(Surface::Rename {
        title: "before after".into(),
        cursor: "before ".len(),
    });
    assert!(app.insert_surface_paste("one\r\ntwo"));
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Rename { title, cursor })
            if title == "before one twoafter" && *cursor == "before one two".len()
    ));

    app.surfaces.clear();
    app.surfaces.push(Surface::Providers {
        rows: Vec::new(),
        list: ListState::selectable(1),
        query: String::new(),
        query_cursor: 0,
        model_configuration_follows: false,
    });
    assert!(app.insert_surface_paste("mock\nprovider"));
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Providers { query, query_cursor, .. })
            if query == "mock provider" && *query_cursor == query.len()
    ));

    app.surfaces.clear();
    app.surfaces.push(Surface::AgentWizard {
        name: String::new(),
        description: String::new(),
        parent: String::new(),
        parents: vec!["None".into()],
        parent_list: ListState::selectable(1),
        prompt: MultilineInput::new(String::new(), 0),
        availability: 0,
        step: AgentWizardStep::Prompt,
        cursor: 0,
        editing: false,
        original_name: None,
    });
    assert!(app.insert_surface_paste("first\r\nsecond"));
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::AgentWizard { prompt, .. })
            if prompt.text() == "first\nsecond" && prompt.cursor() == prompt.text().len()
    ));

    app.surfaces.clear();
    app.surfaces.push(Surface::Question {
        request: Box::new(question_request()),
        question_index: 0,
        option_index: 0,
        answers: vec![cagent_agent::QuestionAnswer::default()],
        answered: vec![false],
        editing_note: true,
        note_cursor: 0,
    });
    assert!(app.insert_surface_paste("one\r\ntwo"));
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Question { answers, note_cursor, .. })
            if answers[0].note.as_deref() == Some("one\ntwo") && *note_cursor == "one\ntwo".len()
    ));

    app.surfaces.clear();
    app.surfaces.push(Surface::SettingInput {
        setting: cagent_agent::config::SettingDefinition {
            key: "test.positive_integer".into(),
            label: "Test positive integer".into(),
            description: String::new(),
            kind: cagent_agent::config::SettingKind::PositiveInteger,
            section: cagent_agent::config::SettingSection::General,
            default_value: "1".into(),
        },
        value: String::new(),
        cursor: 0,
    });
    assert!(app.insert_surface_paste("a1\n2b3"));
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::SettingInput { value, cursor, .. }) if value == "123" && *cursor == 3
    ));
}

#[tokio::test]
async fn terminal_paste_does_not_leak_into_the_composer_behind_a_menu() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = AgentRuntime::open(cagent_agent::runtime::RuntimeOptions::new(
        temporary.path().join("paste-menu.db"),
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
    app.replace_draft("composer text");
    app.surfaces.push(Surface::Onboarding);

    app.handle_terminal_event(&session, Event::Paste("hidden paste".into()))
        .await
        .unwrap();

    assert_eq!(app.draft, "composer text");
}
