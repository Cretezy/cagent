use super::*;

pub(super) fn edit_diff(path: &str, line: u64, text: &str) -> cagent_agent::tools::SemanticDiff {
    cagent_agent::tools::SemanticDiff {
        files: vec![cagent_agent::tools::DiffFile {
            old_path: Some(Path::new("/tmp/project").join(path)),
            new_path: Some(Path::new("/tmp/project").join(path)),
            kind: cagent_agent::tools::DiffFileKind::Modified,
            language: Some("rust".into()),
            added_lines: 1,
            removed_lines: 0,
            old_no_final_newline: false,
            new_no_final_newline: false,
            hunks: vec![cagent_agent::tools::DiffHunk {
                header: format!("@@ -{line},0 +{line} @@"),
                lines: vec![cagent_agent::tools::DiffLine {
                    kind: cagent_agent::tools::DiffLineKind::Addition,
                    old_line: None,
                    new_line: Some(line),
                    text: text.into(),
                }],
            }],
        }],
    }
}

#[test]
fn committed_activity_starts_a_new_edit_history_block() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.push_edit_history(edit_diff("src/one.rs", 4, "let one = 1;"));
    app.history.push(test_tool_groups(vec![]));
    app.push_edit_history(edit_diff("src/two.rs", 8, "let two = 2;"));

    assert!(matches!(
        app.history.as_slice(),
        [
            TranscriptBlock {
                kind: cagent_agent::protocol::TranscriptBlockKind::Edits { .. },
                ..
            },
            TranscriptBlock {
                kind: cagent_agent::protocol::TranscriptBlockKind::ToolGroups { .. },
                ..
            },
            TranscriptBlock {
                kind: cagent_agent::protocol::TranscriptBlockKind::Edits { .. },
                ..
            }
        ]
    ));
}

pub(super) fn permission_request(
    preview: Option<cagent_agent::tools::SemanticDiff>,
) -> InteractionRequest {
    InteractionRequest {
        id: cagent_agent::protocol::InteractionRequestId::new(),
        origin: None,
        kind: InteractionRequestKind::PermissionApproval {
            resource: cagent_agent::permissions::PermissionResource {
                tool: "apply_patch".into(),
                server: None,
                operation: None,
                path: Some("/tmp/project/TEST.md".into()),
                access: Some(cagent_agent::permissions::PermissionAccess::Write),
                mode: "ask".into(),
                agent: "general".into(),
                command: Vec::new(),
                raw_command: None,
                cwd: None,
            },
            decision: cagent_agent::permissions::FilesystemPermissionDecision {
                effect: cagent_agent::permissions::PermissionEffect::Ask,
                operation: cagent_agent::permissions::PermissionDecision {
                    effect: cagent_agent::permissions::PermissionEffect::Ask,
                    layer: cagent_agent::permissions::PermissionLayerKind::Default,
                    rule_id: None,
                    reason: "write default".into(),
                },
                external: None,
            },
            message: "Allow edit to TEST.md?".into(),
            queued_message_id: None,
            preview,
            arguments: None,
            auto_review: None,
            suggested_rule: Some(cagent_agent::permissions::PermissionRule {
                id: "test-write".into(),
                effect: cagent_agent::permissions::PermissionEffect::Allow,
                tool: Some("apply_patch".into()),
                server: None,
                operation: None,
                path: Some("/tmp/project/TEST.md".into()),
                command: None,
                raw_command: None,
                cwd: None,
                access: Some("write".into()),
                external: false,
                mode: None,
                agent: None,
                source: Some("approval".into()),
                created_at: None,
            }),
        },
    }
}

pub(super) fn session_snapshot_with_interaction(
    pending_interaction: Option<InteractionRequest>,
) -> cagent_agent::protocol::SessionSnapshot {
    cagent_agent::protocol::SessionSnapshot {
        conversation_id: cagent_agent::protocol::ConversationId::new(),
        project_dir: "/tmp/project".into(),
        cwd: "/tmp/project".into(),
        worktree: None,
        access: cagent_agent::protocol::SessionAccess::Owner,
        cursor: None,
        title: None,
        transcript: Vec::new().into(),
        turn: cagent_agent::protocol::TurnState::Idle,
        active_plan: None,
        last_activity_at: None,
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
        pending_interaction,
        agent_runs: Vec::new(),
        delegated_live: Vec::new(),
        terminals: Vec::new(),
        supervised_work: Vec::new(),
        enabled_providers: vec!["mock".into()],
        startup_resources: cagent_agent::protocol::StartupResourceStatus::default(),
    }
}

#[test]
fn snapshots_hydrate_and_clear_context_and_provider_usage() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    let mut snapshot = session_snapshot_with_interaction(None);
    snapshot.context = Some(cagent_agent::protocol::ContextUsage {
        used_tokens: 136_001,
        context_window: 272_000,
    });
    app.apply_session_snapshot(&snapshot);
    assert_eq!(app.context_percent, Some(50));
    assert_eq!(app.context_used_tokens, Some(136_001));
    assert_eq!(app.context_window, Some(272_000));

    snapshot.provider_usage = Some(cagent_agent::provider::ProviderUsageReport {
        windows: vec![cagent_agent::provider::ProviderUsageWindow {
            id: "codex:weekly".into(),
            label: "weekly".into(),
            remaining_percent: 83,
        }],
    });
    app.apply_session_snapshot(&snapshot);
    assert_eq!(app.provider_usage, snapshot.provider_usage);

    snapshot.context = None;
    snapshot.provider_usage = None;
    app.apply_session_snapshot(&snapshot);
    assert_eq!(app.context_percent, None);
    assert_eq!(app.context_used_tokens, None);
    assert_eq!(app.context_window, None);
    assert_eq!(app.provider_usage, None);

    assert_eq!(
        cagent_agent::protocol::ContextUsage {
            used_tokens: 999,
            context_window: 0,
        }
        .percent_used(),
        None
    );
    assert_eq!(
        cagent_agent::protocol::ContextUsage {
            used_tokens: 500,
            context_window: 333,
        }
        .percent_used(),
        Some(100)
    );
}

#[test]
fn snapshots_replace_and_clear_the_active_plan() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    let mut snapshot = session_snapshot_with_interaction(None);
    snapshot.active_plan = Some(cagent_agent::protocol::UpdatePlanArgs {
        explanation: Some("Starting".into()),
        plan: vec![cagent_agent::protocol::PlanItemArg {
            step: "Inspect".into(),
            status: cagent_agent::protocol::PlanStepStatus::InProgress,
        }],
    });
    app.apply_session_snapshot(&snapshot);
    assert_eq!(app.active_plan, snapshot.active_plan);

    snapshot.active_plan = None;
    app.apply_session_snapshot(&snapshot);
    assert_eq!(app.active_plan, None);
}

#[test]
fn model_snapshot_updates_welcome_for_an_empty_conversation() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "old-model".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    let mut snapshot = session_snapshot_with_interaction(None);
    snapshot.model_selection = Some(("mock".into(), "new-model".into(), Some("high".into())));

    app.apply_session_snapshot(&snapshot);

    let welcome = app
        .welcome
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(welcome.contains("new-model"));
    assert!(welcome.contains("high · mock"));
    assert!(!welcome.contains("old-model"));
}

#[test]
fn unchanged_snapshot_blocks_keep_their_cached_layouts() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    let id = cagent_agent::protocol::TranscriptBlockId::node(cagent_agent::protocol::NodeId::new());
    let mut snapshot = session_snapshot_with_interaction(None);
    snapshot
        .transcript
        .push(cagent_agent::protocol::TranscriptBlock {
            id: id.clone(),
            status: cagent_agent::protocol::TranscriptBlockStatus::Completed,
            kind: cagent_agent::protocol::TranscriptBlockKind::User {
                label: None,
                text: "unchanged".into(),
                attachments: Vec::new(),
                images: Vec::new(),
                image_chips: Vec::new(),
            },
        });

    app.apply_session_snapshot(&snapshot);
    app.ensure_history_layout(80);
    assert_eq!(app.history_block_layouts.len(), 1);

    app.apply_session_snapshot(&snapshot);
    assert_eq!(app.history_block_layouts.len(), 1);

    let cagent_agent::protocol::TranscriptBlockKind::User { text, .. } =
        &mut snapshot.transcript[0].kind
    else {
        unreachable!();
    };
    *text = "changed".into();
    app.apply_session_snapshot(&snapshot);
    assert!(app.history_block_layouts.is_empty());
}

pub(super) fn question_request() -> InteractionRequest {
    InteractionRequest {
        id: cagent_agent::protocol::InteractionRequestId::new(),
        origin: None,
        kind: InteractionRequestKind::Question {
            questions: vec![cagent_agent::QuestionPrompt {
                id: "availability".into(),
                header: "Availability".into(),
                question: "Where should this be available?".into(),
                options: vec![
                    cagent_agent::QuestionOption {
                        label: "Root".into(),
                        description: "Only the root agent".into(),
                    },
                    cagent_agent::QuestionOption {
                        label: "Both".into(),
                        description: "Root and delegated agents".into(),
                    },
                ],
            }],
        },
    }
}

#[test]
fn question_surface_shows_synthetic_choice_and_navigation_help() {
    let mut request = question_request();
    let InteractionRequestKind::Question { questions } = &mut request.kind else {
        panic!("expected question request");
    };
    questions.push(cagent_agent::QuestionPrompt {
        id: "detail".into(),
        header: "Detail".into(),
        question: "How much detail should I provide?".into(),
        options: vec![
            cagent_agent::QuestionOption {
                label: "Brief".into(),
                description: "Keep it concise".into(),
            },
            cagent_agent::QuestionOption {
                label: "Thorough".into(),
                description: "Include implementation detail".into(),
            },
        ],
    });
    let rendered = surface_lines(
        &Surface::Question {
            request: Box::new(request),
            question_index: 0,
            option_index: 2,
            answers: vec![
                cagent_agent::QuestionAnswer::default(),
                cagent_agent::QuestionAnswer::default(),
            ],
            answered: vec![false, false],
            editing_note: true,
            note_cursor: 0,
        },
        Path::new("/tmp/project"),
        100,
    );
    assert_eq!(rendered.last().expect("question lines").width(), 0);
    let lines = rendered
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(lines.contains(cagent_agent::QUESTION_NONE_OF_THE_ABOVE));
    assert!(lines.contains("Add note"));
    assert!(!lines.contains("availability"));
    assert!(!lines.contains("How much detail should I provide?"));
    let title = lines.lines().next().unwrap();
    assert!(title.contains("Questions  1/2"));
    assert!(title.contains("Availability"));
    assert!(title.contains("Detail"));
    assert!(!title.contains("Submit"));
    let prompt = rendered
        .iter()
        .find(|line| line.to_string().contains("Where should this be available?"))
        .unwrap();
    assert_eq!(prompt.spans[0].style.fg, None);
    assert!(prompt.spans[0].style.add_modifier.contains(Modifier::BOLD));
    let root = rendered
        .iter()
        .find(|line| line.to_string().contains("Only the root agent"))
        .unwrap()
        .to_string();
    let prompt_row = rendered
        .iter()
        .position(|line| std::ptr::eq(line, prompt))
        .unwrap();
    let root_row = rendered
        .iter()
        .position(|line| line.to_string().contains("Only the root agent"))
        .unwrap();
    assert_eq!(root_row - prompt_row, 2);
    let none = rendered
        .iter()
        .find(|line| line.to_string().contains("Optionally add a note with Tab"))
        .unwrap()
        .to_string();
    assert!(root.starts_with("  Root"));
    assert!(none.starts_with("› None"));
    let root_row = rendered
        .iter()
        .find(|line| line.to_string().contains("Root"))
        .unwrap();
    let none_row = rendered
        .iter()
        .find(|line| line.to_string().contains("None of the above"))
        .unwrap();
    assert_eq!(root_row.spans[1].style, Style::default());
    assert_eq!(none_row.spans[1].style, SELECTED_STYLE);
    assert_eq!(root_row.spans[3].style, DIM_STYLE);
    assert_eq!(none_row.spans[3].style.fg, None);
    assert!(
        none_row.spans[3]
            .style
            .add_modifier
            .contains(Modifier::BOLD)
    );
    let root_column = root
        .find("Only the root agent")
        .map(|index| root[..index].width());
    let none_column = none
        .find("Optionally add a note with Tab")
        .map(|index| none[..index].width());
    assert_eq!(root_column, none_column);
    let note = rendered
        .iter()
        .find(|line| line.to_string().contains("Add note"))
        .unwrap();
    assert_eq!(note.to_string(), "› Add note");
    assert_eq!(note.spans[1].style, SEARCH_CURSOR_STYLE);
    let note_row = rendered
        .iter()
        .position(|line| line.to_string().contains("Add note"))
        .unwrap();
    assert_eq!(rendered[note_row - 1].width(), 0);
    assert!(
        rendered[note_row - 2]
            .to_string()
            .contains("Optionally add a note with Tab")
    );
    let with_note = surface_lines(
        &Surface::Question {
            request: Box::new(question_request()),
            question_index: 0,
            option_index: 0,
            answers: vec![cagent_agent::QuestionAnswer {
                selection: Some("Root".into()),
                note: Some("Keep the current behavior".into()),
            }],
            answered: vec![true],
            editing_note: false,
            note_cursor: 0,
        },
        Path::new("/tmp/project"),
        100,
    )
    .into_iter()
    .map(|line| line.to_string())
    .collect::<Vec<_>>()
    .join("\n");
    assert!(with_note.contains("Optionally add a note with Tab"));
    assert!(with_note.contains("› Keep the current behavior"));
    let saved_note = surface_lines(
        &Surface::Question {
            request: Box::new(question_request()),
            question_index: 0,
            option_index: 0,
            answers: vec![cagent_agent::QuestionAnswer {
                selection: Some("Root".into()),
                note: Some("Keep the current behavior".into()),
            }],
            answered: vec![true],
            editing_note: false,
            note_cursor: 0,
        },
        Path::new("/tmp/project"),
        100,
    )
    .into_iter()
    .find(|line| line.to_string().contains("Keep the current behavior"))
    .unwrap();
    assert_eq!(saved_note.spans[0].style, Style::default());
    assert_eq!(
        surface_status(&Surface::Question {
            request: Box::new(question_request()),
            question_index: 0,
            option_index: 0,
            answers: vec![cagent_agent::QuestionAnswer::default()],
            answered: vec![false],
            editing_note: false,
            note_cursor: 0,
        }),
        "↑/↓ choose · ←/→ tabs · Tab add note · Enter select · Esc cancel"
    );
}

#[test]
fn question_surface_wraps_long_prompts_inside_the_menu_width() {
    let mut request = question_request();
    let InteractionRequestKind::Question { questions } = &mut request.kind else {
        panic!("expected question request");
    };
    questions[0].question =
        "Where should this exceptionally long question be available to delegated agents?".into();

    let width = 32;
    let rendered = surface_lines(
        &Surface::Question {
            request: Box::new(request),
            question_index: 0,
            option_index: 0,
            answers: vec![cagent_agent::QuestionAnswer::default()],
            answered: vec![false],
            editing_note: false,
            note_cursor: 0,
        },
        Path::new("/tmp/project"),
        width,
    );
    let prompt_start = rendered
        .iter()
        .position(|line| line.to_string().contains("Where should"))
        .unwrap();
    let prompt_rows = rendered[prompt_start..]
        .iter()
        .take_while(|line| line.width() > 0)
        .collect::<Vec<_>>();

    assert!(prompt_rows.len() > 1);
    assert!(
        prompt_rows
            .iter()
            .all(|line| line.width() <= usize::from(width))
    );
    assert!(
        prompt_rows
            .iter()
            .all(|line| line.spans.first().is_some_and(|span| span.content == "  "))
    );
}

#[test]
fn question_surface_wraps_option_descriptions_inside_their_column() {
    let mut request = question_request();
    let InteractionRequestKind::Question { questions } = &mut request.kind else {
        panic!("expected question request");
    };
    questions[0].options[0].description =
        "Only the root agent, with a deliberately long explanation that needs wrapping".into();

    let width = 50;
    let rendered = surface_lines(
        &Surface::Question {
            request: Box::new(request),
            question_index: 0,
            option_index: 0,
            answers: vec![cagent_agent::QuestionAnswer::default()],
            answered: vec![false],
            editing_note: false,
            note_cursor: 0,
        },
        Path::new("/tmp/project"),
        width,
    );
    let root_index = rendered
        .iter()
        .position(|line| line.to_string().contains("Only the root agent"))
        .unwrap();
    let root = rendered[root_index].to_string();
    let description_start = root.find("Only the root agent").unwrap();
    let description_column = root[..description_start].width();
    let continuation = rendered[root_index + 1].to_string();

    assert!(rendered[root_index].width() <= usize::from(width));
    assert!(rendered[root_index + 1].width() <= usize::from(width));
    assert!(continuation.starts_with(&" ".repeat(description_column)));
}

#[tokio::test]
async fn question_enter_selects_the_choice_and_advances_to_the_next_tab() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = AgentRuntime::open(cagent_agent::runtime::RuntimeOptions::new(
        temporary.path().join("question-navigation.db"),
    ))
    .await
    .unwrap();
    let session = runtime
        .create_session(cagent_agent::runtime::NewSession {
            workspace: temporary.path().to_path_buf(),
        })
        .await
        .unwrap();
    let mut request = question_request();
    let InteractionRequestKind::Question { questions } = &mut request.kind else {
        panic!("expected question request");
    };
    questions.push(cagent_agent::QuestionPrompt {
        id: "detail".into(),
        header: "Detail".into(),
        question: "How much detail should I provide?".into(),
        options: vec![
            cagent_agent::QuestionOption {
                label: "Brief".into(),
                description: "Keep it concise".into(),
            },
            cagent_agent::QuestionOption {
                label: "Thorough".into(),
                description: "Include implementation detail".into(),
            },
        ],
    });
    let mut app = App::new(
        temporary.path(),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.surfaces.push(Surface::Question {
        request: Box::new(request),
        question_index: 0,
        option_index: 1,
        answers: vec![
            cagent_agent::QuestionAnswer {
                selection: None,
                note: Some("use the existing style".into()),
            },
            cagent_agent::QuestionAnswer::default(),
        ],
        answered: vec![false, false],
        editing_note: true,
        note_cursor: "use the existing style".len(),
    });

    app.handle_key(&session, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Question {
            question_index: 1,
            option_index: 0,
            answers,
            answered,
            ..
        }) if answered == &vec![true, false]
            && answers[0].selection.as_deref() == Some("Both")
            && answers[0].note.as_deref() == Some("use the existing style")
    ));
}

#[tokio::test]
async fn question_note_uses_multiline_bindings_and_preserves_pasted_lines() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = AgentRuntime::open(cagent_agent::runtime::RuntimeOptions::new(
        temporary.path().join("question-multiline-note.db"),
    ))
    .await
    .unwrap();
    let session = runtime
        .create_session(cagent_agent::runtime::NewSession {
            workspace: temporary.path().to_path_buf(),
        })
        .await
        .unwrap();
    let mut request = question_request();
    let InteractionRequestKind::Question { questions } = &mut request.kind else {
        panic!("expected question request");
    };
    questions.push(cagent_agent::QuestionPrompt {
        id: "detail".into(),
        header: "Detail".into(),
        question: "How much detail?".into(),
        options: vec![
            cagent_agent::QuestionOption {
                label: "Brief".into(),
                description: "Keep it concise".into(),
            },
            cagent_agent::QuestionOption {
                label: "Thorough".into(),
                description: "Include details".into(),
            },
        ],
    });
    let mut app = App::new(
        temporary.path(),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.surfaces.push(Surface::Question {
        request: Box::new(request),
        question_index: 0,
        option_index: 0,
        answers: vec![
            cagent_agent::QuestionAnswer::default(),
            cagent_agent::QuestionAnswer::default(),
        ],
        answered: vec![false, false],
        editing_note: true,
        note_cursor: 0,
    });

    app.handle_key(&session, KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT))
        .await
        .unwrap();
    assert!(app.insert_surface_paste("first\r\nsecond"));
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Question {
            question_index: 0,
            editing_note: true,
            answers,
            ..
        }) if answers[0].note.as_deref() == Some("\nfirst\nsecond")
    ));

    app.handle_key(&session, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Question {
            question_index: 1,
            editing_note: false,
            answers,
            ..
        }) if answers[0].note.as_deref() == Some("\nfirst\nsecond")
    ));
}

#[test]
fn interaction_note_editor_grows_to_five_visual_rows_then_scrolls() {
    let two_rows = note_input_layout("one\ntwo", "one\ntwo".len(), 40);
    assert_eq!(
        two_rows
            .hits
            .iter()
            .filter(|hit| matches!(hit, ScrollViewHit::Content(_)))
            .count(),
        2
    );

    let wrapped = "abcdefghij".repeat(12);
    let capped = note_input_layout(&wrapped, wrapped.len(), 20);
    assert_eq!(
        capped
            .hits
            .iter()
            .filter(|hit| matches!(hit, ScrollViewHit::Content(_)))
            .count(),
        5
    );
    assert!(matches!(
        capped.hits.first(),
        Some(ScrollViewHit::Indicator(_))
    ));
    let (first_visible, source_row) = capped
        .lines
        .iter()
        .zip(&capped.hits)
        .find_map(|(line, hit)| match hit {
            ScrollViewHit::Content(source_row) => Some((line, *source_row)),
            _ => None,
        })
        .unwrap();
    assert!(
        source_row > 0,
        "the fixture should be scrolled past its first row"
    );
    assert!(first_visible.to_string().starts_with('›'));
}

#[tokio::test]
async fn existing_question_notes_open_at_the_end() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = AgentRuntime::open(cagent_agent::runtime::RuntimeOptions::new(
        temporary.path().join("question-note-cursor.db"),
    ))
    .await
    .unwrap();
    let session = runtime
        .create_session(cagent_agent::runtime::NewSession {
            workspace: temporary.path().to_path_buf(),
        })
        .await
        .unwrap();
    let note = "keep this context";
    let mut app = App::new(
        temporary.path(),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.surfaces.push(Surface::Question {
        request: Box::new(question_request()),
        question_index: 0,
        option_index: 0,
        answers: vec![cagent_agent::QuestionAnswer {
            selection: None,
            note: Some(note.into()),
        }],
        answered: vec![false],
        editing_note: false,
        note_cursor: 0,
    });

    app.handle_surface_key(&session, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();

    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Question {
            editing_note: true,
            note_cursor,
            ..
        }) if *note_cursor == note.len()
    ));
}

#[tokio::test]
async fn question_note_escape_and_empty_backspace_close_the_note_editor() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = AgentRuntime::open(cagent_agent::runtime::RuntimeOptions::new(
        temporary.path().join("question-note-dismissal.db"),
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
    app.surfaces.push(Surface::Question {
        request: Box::new(question_request()),
        question_index: 0,
        option_index: 0,
        answers: vec![cagent_agent::QuestionAnswer {
            selection: None,
            note: Some("discard this".into()),
        }],
        answered: vec![false],
        editing_note: true,
        note_cursor: 12,
    });

    app.handle_key(&session, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Question {
            editing_note: false,
            note_cursor: 0,
            answers,
            ..
        }) if answers[0].note.is_none()
    ));

    let Some(Surface::Question {
        editing_note,
        note_cursor,
        ..
    }) = app.surfaces.last_mut()
    else {
        panic!("expected question surface");
    };
    *editing_note = true;
    *note_cursor = 0;
    app.handle_key(
        &session,
        KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
    )
    .await
    .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Question {
            editing_note: false,
            note_cursor: 0,
            answers,
            ..
        }) if answers[0].note.is_none()
    ));
}

#[test]
fn interactions_wait_for_composer_input_to_settle_before_opening() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.insert_text("still drafting");
    app.set_pending_interaction(question_request());
    assert!(app.surfaces.is_empty());
    assert!(app.interaction_presentation_is_deferred());

    app.last_composer_input_at = Some(Instant::now() - Duration::from_millis(749));
    app.open_pending_interaction();
    assert!(app.surfaces.is_empty());

    app.last_composer_input_at = Some(Instant::now() - Duration::from_millis(751));
    app.open_pending_interaction();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Question { .. })
    ));
}

#[tokio::test]
async fn cursor_movement_restarts_the_interaction_delay() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = AgentRuntime::open(cagent_agent::runtime::RuntimeOptions::new(
        temporary.path().join("cursor-interaction-delay.db"),
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
    app.insert_text("still drafting");
    app.set_pending_interaction(question_request());
    let stale = Instant::now() - Duration::from_secs(1);
    app.last_composer_input_at = Some(stale);

    app.handle_key(&session, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.cursor, app.draft.len() - 1);
    assert!(
        app.last_composer_input_at
            .is_some_and(|input| input > stale)
    );
    assert!(app.interaction_presentation_is_deferred());
    assert!(app.surfaces.is_empty());
}
