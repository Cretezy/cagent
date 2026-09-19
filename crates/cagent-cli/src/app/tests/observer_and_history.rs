use super::*;

#[test]
fn composer_cursor_respects_a_shifted_main_pane() {
    let app = App::new(
        Path::new("/workspace"),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );

    assert_eq!(
        app.cursor_position(ratatui::layout::Rect::new(32, 10, 88, 3), 88, 2),
        Some(ratatui::layout::Position::new(34, 10))
    );
}

#[test]
fn observer_composer_is_passive_and_has_no_cursor() {
    let mut app = App::new(
        Path::new("/workspace"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.access = cagent_agent::protocol::SessionAccess::Observer {
        takeover_available: false,
    };
    app.draft = "must stay unchanged".into();
    app.cursor = app.draft.len();

    let (_, popup, composer, _) = app.control_lines_with_draft_limit(80, usize::MAX);
    let rendered = composer.iter().map(ToString::to_string).collect::<Vec<_>>();
    assert!(popup.is_empty());
    assert_eq!(rendered[0], "› Viewing conversation in read-only mode");
    assert_eq!(composer[0].spans.len(), 2);
    assert_eq!(composer[0].spans[0].style, DIM_STYLE);
    assert_eq!(composer[0].spans[1].style, DIM_STYLE);
    assert!(
        !rendered
            .iter()
            .any(|line| line.contains("must stay unchanged"))
    );
    assert!(
        app.cursor_position(ratatui::layout::Rect::new(0, 0, 80, 3), 80, 2)
            .is_none()
    );

    app.insert_text("ignored");
    app.insert_paste("ignored");
    assert_eq!(app.draft, "must stay unchanged");
}

#[test]
fn observer_command_palette_is_local_filtered_and_cursored() {
    let mut app = App::new(
        Path::new("/workspace"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.access = cagent_agent::protocol::SessionAccess::Observer {
        takeover_available: false,
    };
    app.activate_observer_command();

    assert_eq!(app.observer_command_text(), Some("/"));
    assert_eq!(
        app.slash_suggestions()
            .iter()
            .map(|command| command.name.as_str())
            .collect::<Vec<_>>(),
        vec![
            "/background",
            "/copy",
            "/diff",
            "/files",
            "/help",
            "/mcp",
            "/new",
            "/providers",
            "/quit",
            "/resume",
            "/search",
            "/statusline",
        ]
    );
    assert!(
        !app.slash_suggestions()
            .iter()
            .any(|command| command.name == "/plan" || command.name == "/model")
    );
    let (_, popup, composer, _) = app.control_lines_with_draft_limit(80, usize::MAX);
    assert_eq!(composer[0].to_string(), "› /");
    assert!(!popup.is_empty());
    assert!(
        app.cursor_position(ratatui::layout::Rect::new(0, 0, 80, 3), 80, 2)
            .is_some()
    );

    app.observer_command_list.select(1, VISIBLE_MENU_ITEMS);
    app.accept_slash_command();
    assert_eq!(app.observer_command_text(), Some("/copy"));
    app.replace_observer_command("/");
    app.insert_observer_command_text("copy");
    assert_eq!(app.slash_suggestions()[0].name, "/copy");
    app.delete_observer_command_range(0, 1);
    assert!(!app.observer_command_active());
    assert_eq!(
        app.control_lines_with_draft_limit(80, usize::MAX).2[0].to_string(),
        "› Viewing conversation in read-only mode"
    );
}

#[test]
fn history_preview_is_passive_filters_commands_and_preserves_the_picker() {
    let mut app = App::new(
        Path::new("/workspace"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    let live = session_snapshot_with_interaction(None);
    app.surfaces.push(Surface::HistoryTree {
        rows: Vec::new(),
        list: ListState::selectable(0),
        purpose: TreePurpose::Fork,
        loading: false,
        revision: None,
        query: "needle".into(),
        query_cursor: 3,
        opened_at_millis: 0,
    });
    app.history_preview = Some(HistoryPreviewState {
        latest_live: live.clone(),
    });

    assert!(app.history_preview_showing());
    assert_eq!(
        app.control_lines_with_draft_limit(80, usize::MAX).2[0].to_string(),
        "› Viewing conversation in read-only mode"
    );
    app.activate_observer_command();
    assert_eq!(app.observer_command_text(), Some("/"));
    assert!(!app.slash_suggestions().is_empty());
    assert!(app.slash_suggestions().iter().all(|suggestion| {
        SLASH_COMMANDS
            .iter()
            .find(|command| command.name == suggestion.name)
            .is_some_and(|command| command.observer_safe)
    }));
    assert!(
        app.cursor_position(ratatui::layout::Rect::new(0, 0, 80, 3), 80, 2)
            .is_some()
    );
    app.clear_observer_command();

    let mut newer = live;
    newer.title = Some("newest live title".into());
    app.apply_session_snapshot(&newer);
    app.close_history_preview();
    assert_eq!(app.conversation_title.as_deref(), Some("newest live title"));
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::HistoryTree {
            purpose: TreePurpose::Fork,
            query,
            query_cursor: 3,
            ..
        }) if query == "needle"
    ));
}

#[tokio::test]
async fn history_preview_opens_from_browse_and_fork_and_escape_returns() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = AgentRuntime::open(cagent_agent::runtime::RuntimeOptions::new(
        temporary.path().join("history-preview.db"),
    ))
    .await
    .unwrap();
    let session = runtime
        .create_session(cagent_agent::runtime::NewSession {
            workspace: temporary.path().to_path_buf(),
        })
        .await
        .unwrap();
    let rows = session.tree_history_snapshot().await.unwrap().rows;

    for purpose in [TreePurpose::Browse, TreePurpose::Fork] {
        let mut app = App::new(
            temporary.path(),
            ("mock".into(), "echo".into(), None),
            BTreeSet::new(),
            "ask",
            None,
        );
        app.apply_session_snapshot(&session.attach().await.unwrap().snapshot);
        app.surfaces.push(Surface::HistoryTree {
            rows: rows.clone(),
            list: ListState::selectable(rows.len()),
            purpose,
            loading: false,
            revision: None,
            query: String::new(),
            query_cursor: 0,
            opened_at_millis: 0,
        });

        app.handle_surface_key(
            &session,
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE),
        )
        .await
        .unwrap();
        assert!(app.history_preview_showing());
        app.handle_key(&session, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .await
            .unwrap();
        assert!(app.history_preview.is_none());
        assert!(matches!(
            app.surfaces.last(),
            Some(Surface::HistoryTree {
                purpose: restored,
                ..
            }) if *restored == purpose
        ));
    }
}

#[tokio::test]
async fn shift_f_requests_a_hard_fork_from_both_history_pickers() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = AgentRuntime::open(cagent_agent::runtime::RuntimeOptions::new(
        temporary.path().join("hard-fork-picker"),
    ))
    .await
    .unwrap();
    let session = runtime
        .create_session(cagent_agent::runtime::NewSession {
            workspace: temporary.path().to_path_buf(),
        })
        .await
        .unwrap();
    let rows = session.tree_history_snapshot().await.unwrap().rows;
    let expected = cagent_agent::presentation::history_fork_target(&rows[0]);

    for purpose in [TreePurpose::Browse, TreePurpose::Fork] {
        let mut app = App::new(
            temporary.path(),
            ("mock".into(), "echo".into(), None),
            BTreeSet::new(),
            "ask",
            None,
        );
        app.surfaces.push(Surface::HistoryTree {
            rows: rows.clone(),
            list: ListState::selectable(rows.len()),
            purpose,
            loading: false,
            revision: None,
            query: String::new(),
            query_cursor: 0,
            opened_at_millis: 0,
        });

        app.handle_surface_key(
            &session,
            KeyEvent::new(KeyCode::Char('F'), KeyModifiers::SHIFT),
        )
        .await
        .unwrap();
        assert_eq!(
            app.pending_action.take(),
            Some(AppAction::HardFork(expected))
        );
    }
}

#[test]
fn permissions_command_is_registered_but_not_observer_safe() {
    let command = SLASH_COMMANDS
        .iter()
        .find(|command| command.name == "/permissions")
        .expect("permissions command");
    assert!(!command.observer_safe);
    assert_eq!(
        command.description,
        "manage permissions or simulate a Bash decision"
    );
    assert_eq!(command.argument_hint, Some("[simulate <bash command>]"));
}

#[test]
fn permissions_surface_lists_close_then_project_before_global() {
    let rule = |id: &str, path: &str| cagent_agent::permissions::PermissionRule {
        id: id.into(),
        effect: cagent_agent::permissions::PermissionEffect::Allow,
        tool: Some("bash".into()),
        server: None,
        operation: None,
        path: Some(path.into()),
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
    let surface = Surface::Permissions {
        rows: vec![
            PermissionMenuRow {
                scope: cagent_agent::permissions::PermissionScope::Project,
                rule: {
                    let mut rule = rule("p", "project/**");
                    rule.access = Some("read".into());
                    rule.external = true;
                    rule
                },
            },
            PermissionMenuRow {
                scope: cagent_agent::permissions::PermissionScope::Global,
                rule: rule("g", "global/**"),
            },
        ],
        list: ListState::selectable(3),
    };
    let rendered = surface_lines(&surface, Path::new("/workspace"), 80)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>();
    let close = rendered
        .iter()
        .position(|line| line.contains("Close"))
        .unwrap();
    let project = rendered
        .iter()
        .position(|line| line.contains("project/**"))
        .unwrap();
    let global = rendered
        .iter()
        .position(|line| line.contains("global/**"))
        .unwrap();
    assert!(close < project && project < global);
    assert!(rendered[project].contains("external read"));
    assert!(surface_status(&surface).contains("e edit · d delete"));
}

#[tokio::test]
async fn observer_new_command_starts_a_new_session_without_touching_the_observed_one() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = AgentRuntime::open(cagent_agent::runtime::RuntimeOptions::new(
        temporary.path().join("observer-new.db"),
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
    app.access = cagent_agent::protocol::SessionAccess::Observer {
        takeover_available: false,
    };
    app.activate_observer_command();
    app.insert_observer_command_text("new");

    app.dispatch_observer_command(&session).await.unwrap();

    assert!(!app.observer_command_active());
    assert_eq!(app.pending_action.take(), Some(AppAction::NewSession(None)));
}

#[tokio::test]
async fn observer_resume_command_opens_picker_or_schedules_a_valid_id() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = AgentRuntime::open(cagent_agent::runtime::RuntimeOptions::new(
        temporary.path().join("observer-resume.db"),
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
    app.access = cagent_agent::protocol::SessionAccess::Observer {
        takeover_available: false,
    };

    app.activate_observer_command();
    app.insert_observer_command_text("resume");
    app.dispatch_observer_command(&session).await.unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Conversations { .. })
    ));
    let mut snapshot = session.attach().await.unwrap().snapshot;
    snapshot.access = cagent_agent::protocol::SessionAccess::Observer {
        takeover_available: false,
    };
    app.apply_session_snapshot(&snapshot);
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Conversations { .. })
    ));

    let id = cagent_agent::protocol::ConversationId::new();
    app.activate_observer_command();
    app.insert_observer_command_text(&format!("resume {id}"));
    app.dispatch_observer_command(&session).await.unwrap();
    assert_eq!(app.pending_action.take(), Some(AppAction::Resume(Some(id))));
}

#[tokio::test]
async fn observer_resume_transition_does_not_end_the_read_only_session() {
    let temporary = tempfile::tempdir().unwrap();
    let storage = temporary.path().join("observer-transition.db");
    let owner_runtime =
        AgentRuntime::open(cagent_agent::runtime::RuntimeOptions::new(storage.clone()))
            .await
            .unwrap();
    let owner = owner_runtime
        .create_session(cagent_agent::runtime::NewSession {
            workspace: temporary.path().to_path_buf(),
        })
        .await
        .unwrap();
    let observer_runtime = AgentRuntime::open(cagent_agent::runtime::RuntimeOptions::new(storage))
        .await
        .unwrap();
    let observer = observer_runtime.resume_session(owner.id()).await.unwrap();
    let mut app = App::new(
        temporary.path(),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.access = cagent_agent::protocol::SessionAccess::Observer {
        takeover_available: false,
    };

    super::controller::end_session_before_transition(&app, &observer)
        .await
        .unwrap();
    assert!(matches!(
        observer.attach().await.unwrap().snapshot.access,
        cagent_agent::protocol::SessionAccess::Observer { .. }
    ));
}

#[tokio::test]
async fn repository_diff_reports_an_unsupported_workspace() {
    let mut app = App::new(
        Path::new("/proc"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );

    app.open_repository_diff().await;

    assert_eq!(
        app.notice.as_deref(),
        Some("not inside a Git or Jujutsu repository")
    );
    assert!(app.surfaces.is_empty());
}

#[test]
fn diff_argument_completion_includes_modes_but_observers_cannot_clear() {
    assert!(
        super::super::helpers::help_command_rows()
            .iter()
            .any(|(name, _)| name == "/diff [conversation|git|clear]")
    );
    assert!(
        super::super::helpers::observer_help_command_rows()
            .iter()
            .any(|(name, _)| name == "/diff [conversation|git]")
    );
    let mut app = App::new(
        Path::new("/workspace"),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    app.replace_draft("/diff ");
    assert_eq!(
        app.slash_suggestions()
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>(),
        ["/diff conversation", "/diff git", "/diff clear"]
    );
    app.replace_draft("/diff g");
    app.normalize_slash();
    assert_eq!(app.accept_slash_command().unwrap().name, "/diff git");
    assert_eq!(app.draft, "/diff git");
    app.access = cagent_agent::protocol::SessionAccess::Observer {
        takeover_available: false,
    };
    app.activate_observer_command();
    app.replace_observer_command("/diff ");
    assert_eq!(
        app.slash_suggestions()
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>(),
        ["/diff conversation", "/diff git"]
    );
}

#[tokio::test]
async fn diff_routing_defaults_overrides_clear_and_observer_validation_are_local() {
    let temporary = tempfile::tempdir().unwrap();
    let config_path = temporary.path().join("config.toml");
    std::fs::write(&config_path, "version = 1\n").unwrap();
    let config = cagent_agent::config::ConfigStore::open(&config_path).unwrap();
    let storage = temporary.path().join("diff.db");
    let runtime = AgentRuntime::open(
        cagent_agent::runtime::RuntimeOptions::new(storage.clone()).with_config(config.clone()),
    )
    .await
    .unwrap();
    let owner = runtime
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
    let before = owner.attach().await.unwrap().snapshot;
    for (command, expected) in [
        ("/diff", "no recorded conversation changes"),
        ("/diff git", "not inside a Git or Jujutsu repository"),
        ("/diff conversation", "no recorded conversation changes"),
        ("/diff git extra", cagent_agent::tools::DiffCommand::USAGE),
        (
            "/diff clear",
            "conversation diff tracking cleared; files and history unchanged",
        ),
    ] {
        app.replace_draft(command);
        app.submit_draft(&owner, QueueTarget::NextBoundary)
            .await
            .unwrap();
        assert_eq!(app.notice.as_deref(), Some(expected), "{command}");
        assert!(app.surfaces.is_empty());
    }
    config.save_setting("ui.diff_mode", "\"git\"").unwrap();
    app.open_diff(&owner, None).await;
    assert_eq!(
        app.notice.as_deref(),
        Some("not inside a Git or Jujutsu repository")
    );
    app.open_diff(&owner, Some("conversation")).await;
    assert_eq!(
        app.notice.as_deref(),
        Some("no recorded conversation changes")
    );
    app.open_diff(&owner, Some("clear")).await;
    assert_eq!(owner.diff_mode(), cagent_agent::config::UiDiffMode::Git);

    let observer_runtime =
        AgentRuntime::open(cagent_agent::runtime::RuntimeOptions::new(storage).with_config(config))
            .await
            .unwrap();
    let observer = observer_runtime.resume_session(owner.id()).await.unwrap();
    app.access = cagent_agent::protocol::SessionAccess::Observer {
        takeover_available: false,
    };
    assert!(
        observer
            .load_conversation_diff()
            .await
            .unwrap()
            .diff
            .files
            .is_empty()
    );
    assert!(matches!(
        observer.clear_conversation_diff().await,
        Err(cagent_agent::protocol::RuntimeError::ReadOnlyObserver(_))
    ));
    for (command, expected) in [
        ("/diff", "not inside a Git or Jujutsu repository"),
        ("/diff conversation", "no recorded conversation changes"),
        ("/diff git", "not inside a Git or Jujutsu repository"),
        (
            "/diff clear",
            "observers cannot clear conversation diff tracking",
        ),
        ("/diff nonsense", cagent_agent::tools::DiffCommand::USAGE),
        ("/diff clear extra", cagent_agent::tools::DiffCommand::USAGE),
    ] {
        app.activate_observer_command();
        app.replace_observer_command(command);
        app.dispatch_observer_command(&observer).await.unwrap();
        assert_eq!(app.notice.as_deref(), Some(expected), "{command}");
    }
    let after = owner.attach().await.unwrap().snapshot;
    assert_eq!(before.cursor, after.cursor);
    assert_eq!(before.transcript.len(), after.transcript.len());
}

#[test]
fn repository_diff_uses_the_shared_expanded_diff_renderer() {
    let diff = cagent_agent::tools::parse_git_diff(
        "diff --git a/main.rs b/main.rs\n--- a/main.rs\n+++ b/main.rs\n@@ -1 +1 @@\n-old\n+new\n",
    );
    let surface = Surface::Expanded {
        view: ExpandedView::RepositoryDiff {
            diff,
            title: "Repository diff",
        },
        scroll: 0,
        viewport_rows: 20,
    };

    let text = surface_lines(&surface, Path::new("/tmp/project"), 80)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("Repository diff  1 files"));
    assert!(text.contains("main.rs"));
    assert!(text.contains("old"));
    assert!(text.contains("new"));
}

#[test]
fn conversation_diff_view_uses_recorded_hunks_and_bounds_display_rows() {
    let mut diff = cagent_agent::tools::parse_git_diff(
        "diff --git a/missing.rs b/missing.rs\n--- a/missing.rs\n+++ b/missing.rs\n@@ -1 +1 @@\n-before\n+after\n",
    );
    let surface = Surface::Expanded {
        view: ExpandedView::RepositoryDiff {
            diff: diff.clone(),
            title: "Conversation diff",
        },
        scroll: 0,
        viewport_rows: 20,
    };
    let text = surface_lines(&surface, Path::new("/nonexistent"), 80)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("Conversation diff"));
    assert!(text.contains("missing.rs"));
    assert!(text.contains("before"));
    assert!(text.contains("after"));
    diff.files[0].hunks[0].lines[0].text = "x\n".repeat(50_001);
    let lines = crate::render::render_expanded_diff(&diff, 80);
    assert_eq!(lines.len(), 1);
    assert!(lines[0].to_string().contains("too large to display"));
}

#[test]
fn expanded_repository_diff_wraps_and_fills_changed_rows() {
    let diff = cagent_agent::tools::parse_git_diff(
        "diff --git a/main.rs b/main.rs\n--- a/main.rs\n+++ b/main.rs\n@@ -1 +1 @@\n-old value that is wider than the viewport\n+new value that is wider than the viewport\n",
    );
    let surface = Surface::Expanded {
        view: ExpandedView::RepositoryDiff {
            diff,
            title: "Repository diff",
        },
        scroll: 0,
        viewport_rows: 20,
    };

    let lines = surface_lines(&surface, Path::new("/tmp/project"), 24);
    assert!(lines.iter().all(|line| line.width() <= 24));
    let changed = lines
        .iter()
        .filter(|line| {
            line.style.bg.is_some()
                && (line.to_string().contains("old")
                    || line.to_string().contains("new")
                    || line.to_string().contains("viewport"))
        })
        .collect::<Vec<_>>();
    assert!(changed.len() >= 4);
    assert!(changed.iter().all(|line| line.width() == 24));
}

#[test]
fn repository_diff_command_assigns_a_visible_viewport() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.render_height = 24;
    let diff = cagent_agent::tools::parse_git_diff(
        "diff --git a/main.rs b/main.rs\n--- a/main.rs\n+++ b/main.rs\n@@ -1 +1 @@\n-old\n+new\n",
    );

    app.show_repository_diff(diff);

    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Expanded {
            view: ExpandedView::RepositoryDiff { .. },
            viewport_rows: 20,
            ..
        })
    ));
}

#[test]
fn observer_command_survives_snapshot_updates_and_ctrl_c_state_reset_is_local() {
    let mut app = App::new(
        Path::new("/workspace"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.access = cagent_agent::protocol::SessionAccess::Observer {
        takeover_available: false,
    };
    app.activate_observer_command();
    app.insert_observer_command_text("status");
    let mut snapshot = session_snapshot_with_interaction(None);
    snapshot.access = cagent_agent::protocol::SessionAccess::Observer {
        takeover_available: false,
    };
    app.apply_session_snapshot(&snapshot);
    assert_eq!(app.observer_command_text(), Some("/status"));
    app.clear_observer_command();
    assert!(!app.observer_command_active());
}

#[test]
fn observer_command_editing_supports_word_navigation_and_undo() {
    let mut app = App::new(
        Path::new("/workspace"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.access = cagent_agent::protocol::SessionAccess::Observer {
        takeover_available: false,
    };
    app.activate_observer_command();
    app.replace_observer_command("/copy slack");
    app.delete_observer_command_previous_word();
    assert_eq!(app.observer_command_text(), Some("/copy "));
    app.undo_observer_command();
    assert_eq!(app.observer_command_text(), Some("/copy slack"));
    app.move_observer_command_word_left();
    assert_eq!(app.observer_command_cursor(), "/copy ".len());
    app.move_observer_command_word_right();
    assert_eq!(app.observer_command_cursor(), "/copy slack".len());
}

#[test]
fn observer_snapshot_suppresses_pending_interaction_and_actionable_hints() {
    let mut app = App::new(
        Path::new("/workspace"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    let mut snapshot = session_snapshot_with_interaction(Some(question_request()));
    snapshot.access = cagent_agent::protocol::SessionAccess::Observer {
        takeover_available: false,
    };
    app.apply_session_snapshot(&snapshot);

    assert!(app.pending_interaction.is_none());
    assert!(app.surfaces.is_empty());
    assert!(!app.status_line(100).to_string().contains("Enter to"));
    assert!(!app.active);
}

#[test]
fn web_search_results_format_title_url_date_and_snippet() {
    let lines = web_search_result_lines(
        &[cagent_agent::web_search::WebSearchResult {
            title: "Result title".into(),
            url: "https://example.com/result".into(),
            snippet: "A concise result summary.".into(),
            published_at: Some("2026-08-12".into()),
        }],
        80,
    );
    let rendered = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("• Result title"));
    assert!(rendered.contains("https://example.com/result"));
    assert!(rendered.contains("2026-08-12"));
    assert!(rendered.contains("A concise result summary."));
}

#[test]
fn web_search_results_only_space_after_a_url_when_details_follow() {
    let lines = web_search_result_lines(
        &[
            cagent_agent::web_search::WebSearchResult {
                title: "Bare result".into(),
                url: "https://example.com/bare".into(),
                snippet: String::new(),
                published_at: None,
            },
            cagent_agent::web_search::WebSearchResult {
                title: "Detailed result".into(),
                url: "https://example.com/detailed".into(),
                snippet: "Details".into(),
                published_at: None,
            },
        ],
        80,
    );
    let rendered = lines.iter().map(ToString::to_string).collect::<Vec<_>>();
    let bare_url = rendered
        .iter()
        .position(|line| line.contains("/bare"))
        .unwrap();
    let detailed_url = rendered
        .iter()
        .position(|line| line.contains("/detailed"))
        .unwrap();
    assert_eq!(rendered[bare_url + 1], "");
    assert!(rendered[bare_url + 2].contains("Detailed result"));
    assert_eq!(rendered[detailed_url + 1], "");
    assert_eq!(rendered[detailed_url + 2], "  Details");
    assert_ne!(rendered.last().map(String::as_str), Some(""));
}
