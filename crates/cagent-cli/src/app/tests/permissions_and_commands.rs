use super::interaction_surfaces::{permission_request, session_snapshot_with_interaction};
use super::*;

#[test]
fn mcp_permission_arguments_render_as_plain_json_not_a_diff() {
    let mut request = permission_request(None);
    let InteractionRequestKind::PermissionApproval {
        resource,
        arguments,
        ..
    } = &mut request.kind
    else {
        unreachable!();
    };
    resource.tool = "mcp".into();
    resource.server = Some("fixture".into());
    resource.operation = Some("echo".into());
    *arguments = Some(serde_json::json!({
        "enabled": true,
        "target": "kitchen",
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
    let text = surface_lines_with_viewport(&surface, Path::new("/tmp/project"), 80, 20)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    assert!(text.contains("│ {"));
    assert!(text.contains("\"enabled\": true"));
    assert!(text.contains("\"target\": \"kitchen\""));
    assert!(!text.contains("MCP parameters"));
    assert!(!text.contains("+\"enabled\""));
}

#[test]
fn permission_previews_wrap_to_the_available_width() {
    let mut request = permission_request(None);
    let InteractionRequestKind::PermissionApproval { arguments, .. } = &mut request.kind else {
        unreachable!();
    };
    *arguments = Some(serde_json::json!({
        "long_value": "abcdefghijklmnopqrstuvwxyz0123456789abcdefghijklmnopqrstuvwxyz",
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

    let lines = surface_lines_with_viewport(&surface, Path::new("/tmp/project"), 32, 20);
    assert!(
        lines
            .iter()
            .any(|line| line.to_string().contains("long_value"))
    );
    assert!(lines.iter().any(|line| line.to_string().contains("uvwxyz")));
    assert!(
        lines
            .iter()
            .filter(|line| line.to_string().contains('│'))
            .all(|line| line.width() <= 32)
    );
}

#[test]
fn web_permission_surface_offers_toggleable_tool_wide_conversation_approval() {
    for tool in ["web_fetch", "web_search"] {
        let mut request = permission_request(None);
        let InteractionRequestKind::PermissionApproval {
            resource,
            suggested_rule,
            ..
        } = &mut request.kind
        else {
            unreachable!();
        };
        resource.tool = tool.into();
        resource.operation = Some(
            if tool == "web_fetch" {
                "fetch"
            } else {
                "search"
            }
            .into(),
        );
        resource.server = (tool == "web_search").then(|| "exa".into());
        resource.command = vec!["request-specific-value".into()];
        let rule = suggested_rule.as_mut().unwrap();
        rule.tool = Some(tool.into());
        rule.operation = resource.operation.clone();
        rule.server = resource.server.clone();
        rule.path = None;
        rule.command = Some(resource.command.clone());

        let surface = Surface::Permission {
            request: Box::new(request.clone()),
            selected: 0,
            scope: cagent_agent::presentation::default_permission_approval_scope(&request),
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
        assert!(rendered.contains("for this conversation"));
        assert!(rendered.contains("globally"));
        assert!(!rendered.contains("Allow all for session"));

        let choice = permission_submission_choice(
            &request,
            0,
            cagent_agent::permissions::PermissionScope::ConversationGlobal,
            false,
        )
        .unwrap();
        assert_eq!(choice.decision, "allow_session");
        let rule = choice.session_rule.unwrap();
        assert_eq!(rule.tool.as_deref(), Some(tool));
        assert!(rule.server.is_none());
        assert!(rule.command.is_none());
        assert!(surface_status(&surface).contains("Tab scope"));
    }
}

#[test]
fn file_permission_surface_offers_directory_and_file_persistence() {
    for tool in ["read", "grep"] {
        let mut request = permission_request(None);
        let InteractionRequestKind::PermissionApproval {
            resource,
            suggested_rule,
            ..
        } = &mut request.kind
        else {
            unreachable!();
        };
        resource.tool = tool.into();
        resource.access = Some(cagent_agent::permissions::PermissionAccess::Read);
        suggested_rule.as_mut().unwrap().tool = Some(tool.into());
        suggested_rule.as_mut().unwrap().access = Some("read".into());
        let surface = Surface::Permission {
            request: Box::new(request.clone()),
            selected: 1,
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

        for label in [
            "Allow",
            "Always allow containing dir",
            "Always allow file",
            "Deny",
        ] {
            assert!(rendered.contains(label), "{tool}: {label}");
        }
        assert!(!rendered.contains("Allow for project"));
        assert!(!rendered.contains("Allow globally"));
        assert!(surface_status(&surface).contains("e edit"));
        assert!(
            permission_rule_edit_details(
                &request,
                1,
                cagent_agent::permissions::PermissionScope::Project
            )
            .is_some()
        );
        assert!(
            permission_rule_edit_details(
                &request,
                2,
                cagent_agent::permissions::PermissionScope::Project
            )
            .is_some()
        );
    }
}

#[test]
fn bash_permission_rule_editor_prefills_the_command_pattern() {
    let mut request = permission_request(None);
    let InteractionRequestKind::PermissionApproval {
        resource,
        suggested_rule,
        ..
    } = &mut request.kind
    else {
        unreachable!();
    };
    resource.tool = "bash".into();
    resource.access = Some(cagent_agent::permissions::PermissionAccess::Execute);
    resource.command = vec![
        "if".into(),
        "true;".into(),
        "then".into(),
        "cargo".into(),
        "test".into(),
        "-p".into(),
        "cagent-cli".into(),
        "statusline_surface_shows_live_preview_and_editor_controls".into(),
        ";".into(),
        "fi".into(),
    ];
    let rule = suggested_rule.as_mut().unwrap();
    rule.tool = Some("bash".into());
    rule.path = None;
    rule.command = Some(vec!["cargo".into(), "test".into()]);
    rule.access = Some("execute".into());

    let (rule, cursor, scope) = permission_rule_edit_details(
        &request,
        1,
        cagent_agent::permissions::PermissionScope::Project,
    )
    .unwrap();
    assert_eq!(
        cursor,
        "if true; then cargo test -p cagent-cli statusline_surface_shows_live_preview_and_editor_controls ; fi"
            .len()
    );
    let editor = Surface::PermissionRuleEdit {
        request: Box::new(request),
        pattern: rule.editable_pattern().unwrap().0,
        rule,
        cursor,
        scope,
    };
    let editor_lines = surface_lines(&editor, Path::new("/tmp/project"), 80);
    let rendered = editor_lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("edit command pattern"));
    assert!(rendered.contains("statusline_surface_shows_live_preview_and_editor_controls"));
    assert!(editor_lines[2].spans.iter().any(|span| {
        span.style
            == crate::markdown::code_style(cagent_agent::presentation::CodeTokenKind::Keyword)
    }));
}

#[tokio::test]
async fn bash_permission_rule_editor_accepts_spaces() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = AgentRuntime::open(cagent_agent::runtime::RuntimeOptions::new(
        temporary.path().join("permission-editor.db"),
    ))
    .await
    .unwrap();
    let session = runtime
        .create_session(cagent_agent::runtime::NewSession {
            workspace: temporary.path().to_path_buf(),
        })
        .await
        .unwrap();
    let mut request = permission_request(None);
    let InteractionRequestKind::PermissionApproval {
        resource,
        suggested_rule,
        ..
    } = &mut request.kind
    else {
        unreachable!();
    };
    resource.tool = "bash".into();
    resource.command = vec!["cargo".into(), "test".into()];
    let rule = suggested_rule.as_mut().unwrap();
    rule.tool = Some("bash".into());
    rule.path = None;
    rule.command = Some(resource.command.clone());
    let rule = rule.clone();

    let mut app = App::new(
        temporary.path(),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.surfaces.push(Surface::PermissionRuleEdit {
        request: Box::new(request),
        rule,
        pattern: "cargo test".into(),
        cursor: "cargo test".len(),
        scope: cagent_agent::permissions::PermissionScope::Project,
    });

    app.handle_surface_key(
        &session,
        KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
    )
    .await
    .unwrap();

    let Some(Surface::PermissionRuleEdit {
        pattern, cursor, ..
    }) = app.surfaces.last()
    else {
        panic!("expected permission rule editor");
    };
    assert_eq!(pattern, "cargo test ");
    assert_eq!(*cursor, pattern.len());
}

#[test]
fn list_permission_surface_offers_exact_directory_persistence() {
    let mut request = permission_request(None);
    let InteractionRequestKind::PermissionApproval {
        resource,
        suggested_rule,
        ..
    } = &mut request.kind
    else {
        unreachable!();
    };
    resource.tool = "list".into();
    resource.access = Some(cagent_agent::permissions::PermissionAccess::Read);
    suggested_rule.as_mut().unwrap().tool = Some("list".into());
    suggested_rule.as_mut().unwrap().access = Some("read".into());
    let surface = Surface::Permission {
        request: Box::new(request.clone()),
        selected: 1,
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

    for label in ["Allow", "Always allow", "Deny"] {
        assert!(rendered.contains(label), "{label}");
    }
    assert!(!rendered.contains("Always allow containing dir"));
    assert!(!rendered.contains("Always allow file"));
    assert!(surface_status(&surface).contains("e edit"));
    assert!(
        permission_rule_edit_details(
            &request,
            1,
            cagent_agent::permissions::PermissionScope::Project
        )
        .is_some()
    );
}

#[tokio::test]
async fn permission_scope_controls_tab_submission_editing_and_prompt_reset() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = AgentRuntime::open(cagent_agent::runtime::RuntimeOptions::new(
        temporary.path().join("permission-scope.db"),
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

    let generic = permission_request(None);
    let mut file = permission_request(None);
    let InteractionRequestKind::PermissionApproval {
        resource,
        suggested_rule,
        ..
    } = &mut file.kind
    else {
        unreachable!();
    };
    resource.tool = "read".into();
    resource.access = Some(cagent_agent::permissions::PermissionAccess::Read);
    suggested_rule.as_mut().unwrap().tool = Some("read".into());
    suggested_rule.as_mut().unwrap().access = Some("read".into());
    let mut list = file.clone();
    let InteractionRequestKind::PermissionApproval {
        resource,
        suggested_rule,
        ..
    } = &mut list.kind
    else {
        unreachable!();
    };
    resource.tool = "list".into();
    suggested_rule.as_mut().unwrap().tool = Some("list".into());
    let mut bash = file.clone();
    let InteractionRequestKind::PermissionApproval {
        resource,
        suggested_rule,
        ..
    } = &mut bash.kind
    else {
        unreachable!();
    };
    resource.tool = "bash".into();
    suggested_rule.as_mut().unwrap().tool = Some("bash".into());

    for (request, persistent_indexes) in [
        (generic.clone(), vec![1]),
        (file.clone(), vec![1, 2]),
        (list, vec![1]),
        (bash, vec![1]),
    ] {
        for selected in persistent_indexes {
            app.surfaces.push(Surface::Permission {
                request: Box::new(request.clone()),
                selected,
                scope: cagent_agent::permissions::PermissionScope::Project,
                diff_scroll: 0,
                denial_note: String::new(),
                editing_note: false,
                note_cursor: 0,
            });
            app.handle_surface_key(&session, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
                .await
                .unwrap();
            assert!(matches!(
                app.surfaces.last(),
                Some(Surface::Permission {
                    scope: cagent_agent::permissions::PermissionScope::Global,
                    ..
                })
            ));
            let visible = app.surfaces.last().unwrap();
            let status = surface_status(visible);
            assert!(status.contains("Enter select · Tab scope"));
            assert!(status.contains("Shift+Enter apply globally"));
            assert!(!status.contains("Shift+Enter/e"));
            let contextual_status = app.contextual_key_hints(&status);
            assert!(contextual_status.contains("Enter select · Tab scope"));
            assert!(contextual_status.contains("Shift+Enter apply globally"));
            assert!(!contextual_status.contains("Alt+Enter"));
            assert!(
                surface_lines(visible, temporary.path(), 120)
                    .iter()
                    .any(|line| line.to_string().contains("globally"))
            );
            app.handle_surface_key(&session, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
                .await
                .unwrap();
            assert!(matches!(
                app.surfaces.pop(),
                Some(Surface::Permission {
                    scope: cagent_agent::permissions::PermissionScope::Project,
                    ..
                })
            ));
        }
    }

    app.surfaces.push(Surface::Permission {
        request: Box::new(generic.clone()),
        selected: 0,
        scope: cagent_agent::permissions::PermissionScope::Project,
        diff_scroll: 0,
        denial_note: String::new(),
        editing_note: false,
        note_cursor: 0,
    });
    app.handle_surface_key(&session, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(
        surface_status(app.surfaces.last().unwrap()).contains("Shift+Enter apply for conversation")
    );
    assert!(matches!(
        app.surfaces.pop(),
        Some(Surface::Permission {
            scope: cagent_agent::permissions::PermissionScope::Conversation,
            editing_note: false,
            ..
        })
    ));

    // Request/conversation and project/global are independent row modes.
    app.surfaces.push(Surface::Permission {
        request: Box::new(generic.clone()),
        selected: 1,
        scope: cagent_agent::permissions::PermissionScope::Project,
        diff_scroll: 0,
        denial_note: String::new(),
        editing_note: false,
        note_cursor: 0,
    });
    app.handle_surface_key(&session, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    if let Some(Surface::Permission { selected, .. }) = app.surfaces.last_mut() {
        *selected = 0;
    }
    app.handle_surface_key(&session, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(matches!(
        app.surfaces.pop(),
        Some(Surface::Permission {
            scope: cagent_agent::permissions::PermissionScope::ConversationGlobal,
            ..
        })
    ));

    app.surfaces.push(Surface::Permission {
        request: Box::new(generic.clone()),
        selected: 2,
        scope: cagent_agent::permissions::PermissionScope::Project,
        diff_scroll: 0,
        denial_note: String::new(),
        editing_note: false,
        note_cursor: 0,
    });
    app.handle_surface_key(&session, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(matches!(
        app.surfaces.pop(),
        Some(Surface::Permission {
            editing_note: true,
            scope: cagent_agent::permissions::PermissionScope::Project,
            ..
        })
    ));

    let project = permission_submission_choice(
        &generic,
        1,
        cagent_agent::permissions::PermissionScope::Project,
        false,
    )
    .unwrap();
    let global = permission_submission_choice(
        &generic,
        1,
        cagent_agent::permissions::PermissionScope::Project,
        true,
    )
    .unwrap();
    assert_eq!(project.decision, "allow_project");
    assert_eq!(global.decision, "allow_global");

    app.surfaces.push(Surface::Permission {
        request: Box::new(generic.clone()),
        selected: 1,
        scope: cagent_agent::permissions::PermissionScope::Global,
        diff_scroll: 0,
        denial_note: "keep me".into(),
        editing_note: false,
        note_cursor: 0,
    });
    app.handle_surface_key(
        &session,
        KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE),
    )
    .await
    .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::PermissionRuleEdit {
            scope: cagent_agent::permissions::PermissionScope::Global,
            ..
        })
    ));
    app.handle_surface_key(&session, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(matches!(
        app.surfaces.pop(),
        Some(Surface::Permission {
            selected: 1,
            scope: cagent_agent::permissions::PermissionScope::Global,
            denial_note,
            ..
        }) if denial_note == "keep me"
    ));

    app.surfaces.push(Surface::Permission {
        request: Box::new(generic.clone()),
        selected: 1,
        scope: cagent_agent::permissions::PermissionScope::Project,
        diff_scroll: 0,
        denial_note: String::new(),
        editing_note: false,
        note_cursor: 0,
    });
    app.handle_surface_key(
        &session,
        KeyEvent::new(KeyCode::Char('E'), KeyModifiers::SHIFT),
    )
    .await
    .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::PermissionRuleEdit {
            scope: cagent_agent::permissions::PermissionScope::Global,
            ..
        })
    ));
    app.handle_surface_key(&session, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(matches!(
        app.surfaces.pop(),
        Some(Surface::Permission {
            scope: cagent_agent::permissions::PermissionScope::Project,
            ..
        })
    ));

    app.set_pending_interaction(generic.clone());
    app.open_pending_interaction();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Permission {
            scope: cagent_agent::permissions::PermissionScope::Project,
            ..
        })
    ));

    for tool in ["bash", "web_fetch", "web_search"] {
        app.surfaces.clear();
        let mut request = generic.clone();
        let InteractionRequestKind::PermissionApproval {
            resource,
            suggested_rule,
            ..
        } = &mut request.kind
        else {
            unreachable!();
        };
        resource.tool = tool.into();
        suggested_rule.as_mut().unwrap().tool = Some(tool.into());
        app.set_pending_interaction(request.clone());
        app.open_pending_interaction();

        // Start conversation/global; toggling either row leaves the other alone.
        for (selected, expected_scope, decisions) in [
            (
                0,
                cagent_agent::permissions::PermissionScope::ConversationGlobal,
                ["allow_session", "allow_global", "deny"],
            ),
            (
                1,
                cagent_agent::permissions::PermissionScope::Global,
                ["allow_once", "allow_global", "deny"],
            ),
            (
                0,
                cagent_agent::permissions::PermissionScope::Project,
                ["allow_once", "allow_project", "deny"],
            ),
            (
                1,
                cagent_agent::permissions::PermissionScope::Conversation,
                ["allow_session", "allow_project", "deny"],
            ),
        ] {
            let Some(Surface::Permission {
                scope,
                selected: row,
                ..
            }) = app.surfaces.last_mut()
            else {
                panic!("expected permission prompt for {tool}");
            };
            assert_eq!(*scope, expected_scope);
            for (index, decision) in decisions.into_iter().enumerate() {
                assert_eq!(
                    permission_submission_choice(&request, index, *scope, false)
                        .unwrap()
                        .decision,
                    decision,
                );
            }
            *row = selected;
            app.handle_surface_key(&session, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
                .await
                .unwrap();
        }
        assert!(matches!(
            app.surfaces.last(),
            Some(Surface::Permission {
                scope: cagent_agent::permissions::PermissionScope::ConversationGlobal,
                ..
            })
        ));
    }
}

#[tokio::test]
async fn shift_tab_skips_non_cycleable_read() {
    let temporary = tempfile::tempdir().unwrap();
    let config = cagent_agent::config::ConfigSnapshot::parse(
        &temporary.path().join("config.toml"),
        "version = 1\ndefault_mode = 'read'\ndefault_model = { model = 'mock/echo' }\n[providers.mock]\ntype = 'mock'\nenabled = true\n",
    ).unwrap();
    let runtime = AgentRuntime::open(
        cagent_agent::runtime::RuntimeOptions::new(temporary.path().join("modes.db"))
            .with_config(config),
    )
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
        "read",
        None,
    );
    app.active = true;
    app.handle_key(
        &session,
        KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
    )
    .await
    .unwrap();
    assert_eq!(session.active_profiles().await.unwrap().1, "edit");
    app.mode = "edit".into();
    app.handle_key(&session, KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT))
        .await
        .unwrap();
    assert_eq!(session.active_profiles().await.unwrap().1, "auto");
}

#[tokio::test]
async fn shifted_arrows_cycle_modes_in_the_composer() {
    let temporary = tempfile::tempdir().unwrap();
    let config = cagent_agent::config::ConfigSnapshot::parse(
        &temporary.path().join("config.toml"),
        "version = 1\ndefault_mode = 'read'\ndefault_model = { model = 'mock/echo' }\n[providers.mock]\ntype = 'mock'\nenabled = true\n",
    )
    .unwrap();
    let runtime = AgentRuntime::open(
        cagent_agent::runtime::RuntimeOptions::new(temporary.path().join("modes.db"))
            .with_config(config),
    )
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
        "read",
        None,
    );

    app.handle_key(&session, KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT))
        .await
        .unwrap();
    assert_eq!(session.active_profiles().await.unwrap().1, "edit");

    app.handle_key(&session, KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT))
        .await
        .unwrap();
    assert_eq!(session.active_profiles().await.unwrap().1, "plan");
}

#[tokio::test]
async fn shift_tab_cycles_the_plan_completion_target() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = AgentRuntime::open(cagent_agent::runtime::RuntimeOptions::new(
        temporary.path().join("plan-menu.db"),
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
        "plan",
        None,
    );
    app.surfaces.push(Surface::PlanCompletion {
        request: Box::new(InteractionRequest {
            id: cagent_agent::protocol::InteractionRequestId::new(),
            origin: None,
            kind: InteractionRequestKind::PlanCompletion {
                plan: "# Plan".into(),
                implementation_modes: vec!["ask".into(), "edit".into(), "auto".into()],
                default_mode: "edit".into(),
            },
        }),
        selected: 0,
        note: String::new(),
        editing_note: false,
        note_cursor: 0,
        implementation_mode_index: 1,
        implementation_mode_colors: vec![
            StatusLineColor::LightYellow,
            StatusLineColor::LightGreen,
            StatusLineColor::LightMagenta,
        ],
        implementation_model: "mock/echo".into(),
        context_percent: None,
    });

    app.handle_key(&session, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::PlanCompletion {
            editing_note: true,
            ..
        })
    ));
    assert!(surface_status(app.surfaces.last().unwrap()).contains("Enter accept"));
    app.handle_key(
        &session,
        KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
    )
    .await
    .unwrap();
    app.handle_key(&session, KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT))
        .await
        .unwrap();
    app.handle_key(
        &session,
        KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE),
    )
    .await
    .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::PlanCompletion { note, .. }) if note == "a\nb"
    ));
    app.handle_key(&session, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::PlanCompletion {
            note,
            editing_note: false,
            ..
        }) if note == "a\nb"
    ));
    let closed = surface_lines(app.surfaces.last().unwrap(), temporary.path(), 100)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!closed.contains("a\nb"));
    app.handle_key(&session, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(
        &session,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
    )
    .await
    .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::PlanCompletion {
            note,
            editing_note: false,
            ..
        }) if note == "a\nb"
    ));

    app.handle_key(
        &session,
        KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
    )
    .await
    .unwrap();

    let surface = app.surfaces.last().unwrap();
    let rendered = surface_lines(surface, temporary.path(), 100)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("Yes, with auto and mock/echo"));
    assert!(rendered.contains("Yes, with auto and mock/echo and clear context"));

    app.handle_key(&session, KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT))
        .await
        .unwrap();
    let surface = app.surfaces.last().unwrap();
    let rendered = surface_lines(surface, temporary.path(), 100)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("Yes, with edit and mock/echo"));
}

#[tokio::test]
async fn plan_menu_model_shortcut_returns_after_selecting_a_model() {
    let temporary = tempfile::tempdir().unwrap();
    let config = cagent_agent::config::ConfigSnapshot::parse(
        &temporary.path().join("config.toml"),
        "version = 1\ndefault_model = { model = 'mock/echo' }\n[providers.mock]\ntype = 'mock'\nenabled = true\n",
    )
    .unwrap();
    let runtime = AgentRuntime::open(
        cagent_agent::runtime::RuntimeOptions::new(temporary.path().join("plan-model-picker.db"))
            .with_config(config),
    )
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

    app.handle_key(
        &session,
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE),
    )
    .await
    .unwrap();
    assert!(matches!(app.surfaces.last(), Some(Surface::Models { .. })));

    app.handle_key(&session, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::PlanCompletion { .. })
    ));
    assert_eq!(
        session.mode_model_selection("edit").await.unwrap(),
        Some(("mock".into(), "echo-fast".into(), None))
    );
}

#[tokio::test]
async fn continue_command_resumes_the_latest_workspace_conversation() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = AgentRuntime::open(cagent_agent::runtime::RuntimeOptions::new(
        temporary.path().join("commands.db"),
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
        "read",
        None,
    );

    app.replace_draft("/continue");
    app.submit_draft(&session, QueueTarget::NextBoundary)
        .await
        .unwrap();

    assert!(app.draft.is_empty());
    assert_eq!(app.pending_action.take(), Some(AppAction::Resume(None)));
}

#[tokio::test]
async fn tab_submits_normally_when_the_agent_is_idle() {
    let temporary = tempfile::tempdir().unwrap();
    let config = cagent_agent::config::ConfigSnapshot::parse(
        &temporary.path().join("config.toml"),
        "version = 1\ndefault_model = { model = 'mock/echo' }\n[title_generation]\nenabled = false\n[providers.mock]\ntype = 'mock'\nenabled = true\n",
    )
    .unwrap();
    let runtime = AgentRuntime::open(
        cagent_agent::runtime::RuntimeOptions::new(temporary.path().join("idle-tab.db"))
            .with_config(config),
    )
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
        "read",
        None,
    );
    app.active = false;

    app.replace_draft("send this now");
    app.handle_key(&session, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();

    assert!(app.draft.is_empty());
    assert!(session.attach().await.unwrap().snapshot.queue.is_empty());
}

#[tokio::test]
async fn new_command_carries_an_optional_conversation_title() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = AgentRuntime::open(cagent_agent::runtime::RuntimeOptions::new(
        temporary.path().join("new-command.db"),
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

    app.replace_draft("/new Cleanup investigation");
    app.submit_draft(&session, QueueTarget::NextBoundary)
        .await
        .unwrap();

    assert_eq!(
        app.pending_action.take(),
        Some(AppAction::NewSession(Some("Cleanup investigation".into())))
    );
}

#[tokio::test]
async fn conversation_surface_refreshes_after_background_maintenance() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = AgentRuntime::open(cagent_agent::runtime::RuntimeOptions::new(
        temporary.path().join("picker-refresh.db"),
    ))
    .await
    .unwrap();
    let first = runtime
        .create_session_named(
            cagent_agent::runtime::NewSession {
                workspace: temporary.path().to_path_buf(),
            },
            Some("First".into()),
        )
        .await
        .unwrap();
    runtime
        .create_session_named(
            cagent_agent::runtime::NewSession {
                workspace: temporary.path().to_path_buf(),
            },
            Some("Second".into()),
        )
        .await
        .unwrap();
    runtime.conversations(Some(temporary.path())).await.unwrap();
    let mut app = App::new(
        temporary.path(),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.surfaces.push(Surface::Conversations {
        rows: Vec::new(),
        list: ListState::selectable(0),
        query: String::new(),
        query_cursor: 0,
        opened_at_millis: picker_opened_at_millis(),
        include_archived: false,
    });

    app.refresh_conversation_surface(&first).await.unwrap();

    let Some(Surface::Conversations { rows, list, .. }) = app.surfaces.last() else {
        panic!("conversation surface should remain open");
    };
    assert_eq!(rows.len(), 2);
    assert_eq!(list.item_count, 2);
}

#[tokio::test]
async fn unknown_agent_command_shows_a_status_notice() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = AgentRuntime::open(cagent_agent::runtime::RuntimeOptions::new(
        temporary.path().join("commands.db"),
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

    app.replace_draft("/agent zxc");
    app.handle_key(&session, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.notice.as_deref(), Some("unknown agent: zxc"));
    assert!(
        app.status_line(80)
            .to_string()
            .contains("unknown agent: zxc")
    );
}

#[tokio::test]
async fn search_query_without_a_ready_provider_opens_setup_with_a_notice() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = AgentRuntime::open(cagent_agent::runtime::RuntimeOptions::new(
        temporary.path().join("commands.db"),
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

    app.replace_draft("/search cretezy.com");
    app.submit_draft(&session, QueueTarget::NextBoundary)
        .await
        .unwrap();

    assert_eq!(app.draft, "cretezy.com");
    assert_eq!(
        app.notice.as_deref(),
        Some("configure a web-search provider to search")
    );
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::WebSearchPicker { .. })
    ));
}

#[tokio::test]
async fn direct_mode_commands_switch_and_optionally_submit() {
    let temporary = tempfile::tempdir().unwrap();
    let config = cagent_agent::config::ConfigSnapshot::parse(
        &temporary.path().join("config.toml"),
        "version = 1\ndefault_model = { model = 'mock/echo' }\n[providers.mock]\ntype = 'mock'\nenabled = true\n",
    )
    .unwrap();
    let runtime = AgentRuntime::open(
        cagent_agent::runtime::RuntimeOptions::new(temporary.path().join("commands.db"))
            .with_config(config),
    )
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
        "read",
        None,
    );
    app.mode_commands = session
        .mode_profiles()
        .unwrap()
        .into_iter()
        .map(|mode| mode.name)
        .collect();

    app.replace_draft("/rea");
    assert!(
        app.slash_suggestions()
            .iter()
            .any(|command| command.name == "/read")
    );

    for mode in ["read", "edit", "auto", "plan"] {
        app.replace_draft(&format!("/{mode}"));
        app.submit_draft(&session, QueueTarget::NextBoundary)
            .await
            .unwrap();
        assert_eq!(session.active_profiles().await.unwrap().1, mode);
        assert!(app.draft.is_empty());
    }

    app.replace_draft("/mode read");
    app.submit_draft(&session, QueueTarget::NextBoundary)
        .await
        .unwrap();
    assert_eq!(session.active_profiles().await.unwrap().1, "read");

    app.replace_draft("/mode abc");
    app.submit_draft(&session, QueueTarget::NextBoundary)
        .await
        .unwrap();
    assert_eq!(session.active_profiles().await.unwrap().1, "read");
    assert_eq!(app.notice.as_deref(), Some("unknown mode: abc"));
    assert!(app.draft.is_empty());

    app.active = true;
    app.replace_draft("/mode abc queued message");
    app.submit_draft(&session, QueueTarget::NextBoundary)
        .await
        .unwrap();
    app.active = false;
    assert_eq!(session.active_profiles().await.unwrap().1, "read");
    assert_eq!(app.notice.as_deref(), Some("unknown mode: abc"));
    assert!(app.draft.is_empty());

    app.replace_draft("/mode edit");
    app.submit_draft(&session, QueueTarget::NextBoundary)
        .await
        .unwrap();
    app.replace_draft("/read");
    app.submit_draft(&session, QueueTarget::NextBoundary)
        .await
        .unwrap();
    assert_eq!(session.active_profiles().await.unwrap().1, "read");

    app.replace_draft("/mode auto hello through mode");
    app.submit_draft(&session, QueueTarget::NextBoundary)
        .await
        .unwrap();
    assert_eq!(session.active_profiles().await.unwrap().1, "auto");
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if session.history().await.unwrap().iter().any(|node| {
                node.kind == NodeKind::UserMessage && node.content["text"] == "hello through mode"
            }) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    app.replace_draft("/plan hello from plan");
    app.submit_draft(&session, QueueTarget::NextBoundary)
        .await
        .unwrap();
    assert_eq!(session.active_profiles().await.unwrap().1, "plan");
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if session.history().await.unwrap().iter().any(|node| {
                node.kind == NodeKind::UserMessage && node.content["text"] == "hello from plan"
            }) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let composer_history = session.reload_composer_history_entries().await.unwrap();
    assert!(
        composer_history
            .iter()
            .any(|entry| entry.text == "/plan hello from plan")
    );
    assert!(
        composer_history
            .iter()
            .any(|entry| entry.text == "hello from plan")
    );
    let snapshot = session.attach().await.unwrap().snapshot;
    assert!(snapshot.transcript.iter().any(|block| matches!(
        &block.kind,
        cagent_agent::protocol::TranscriptBlockKind::User { label: Some(label), text, .. }
            if label == "Plan" && text == "hello from plan"
    )));
    assert!(snapshot.transcript.iter().any(|block| matches!(
        &block.kind,
        cagent_agent::protocol::TranscriptBlockKind::User { label: Some(label), text, .. }
            if label == "Auto" && text == "hello through mode"
    )));
}

#[tokio::test]
async fn model_default_invalid_option_is_shown_in_the_tui() {
    let temporary = tempfile::tempdir().unwrap();
    let config = cagent_agent::config::ConfigSnapshot::parse(
        &temporary.path().join("config.toml"),
        "version = 1\n[providers.mock]\ntype = 'mock'\nenabled = true\n",
    )
    .unwrap();
    let runtime = AgentRuntime::open(
        cagent_agent::runtime::RuntimeOptions::new(temporary.path().join("commands.db"))
            .with_config(config),
    )
    .await
    .unwrap();
    let session = runtime
        .create_session(cagent_agent::runtime::NewSession {
            workspace: temporary.path().to_path_buf(),
        })
        .await
        .unwrap();
    session
        .set_session_model("mock", "echo", None)
        .await
        .unwrap();
    let mut app = App::new(
        temporary.path(),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "read",
        None,
    );

    app.replace_draft("/model default");
    app.submit_draft(&session, QueueTarget::NextBoundary)
        .await
        .unwrap();

    assert_eq!(
        app.notice.as_deref(),
        Some("agent and global defaults do not select a model")
    );
    assert!(app.draft.is_empty());
}

#[tokio::test]
async fn slash_mode_messages_preserve_confirmed_attachment_chips() {
    let temporary = tempfile::tempdir().unwrap();
    std::fs::write(temporary.path().join("SPEC.md"), "# Specification\n").unwrap();
    let config = cagent_agent::config::ConfigSnapshot::parse(
        &temporary.path().join("config.toml"),
        "version = 1\ndefault_model = { model = 'mock/echo' }\n[providers.mock]\ntype = 'mock'\nenabled = true\n",
    )
    .unwrap();
    let runtime = AgentRuntime::open(
        cagent_agent::runtime::RuntimeOptions::new(temporary.path().join("commands.db"))
            .with_config(config),
    )
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
    let command = "/plan @SPEC.md";
    app.replace_draft(command);
    app.confirm_attachment("/plan ".len(), command.len(), ListEntryKind::File);

    app.submit_draft(&session, QueueTarget::NextBoundary)
        .await
        .unwrap();

    let snapshot = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let snapshot = session.attach().await.unwrap().snapshot;
            if snapshot.transcript.iter().any(|block| {
                matches!(
                    &block.kind,
                    cagent_agent::protocol::TranscriptBlockKind::User {
                        label: Some(label),
                        text,
                        attachments,
                        ..
                    } if label == "Plan"
                        && text == "@SPEC.md"
                        && matches!(attachments.as_slice(), [attachment]
                            if attachment.spec.path == Path::new("SPEC.md"))
                )
            }) {
                break snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    assert!(snapshot.transcript.iter().any(|block| matches!(
        &block.kind,
        cagent_agent::protocol::TranscriptBlockKind::User {
            label: Some(label),
            text,
            attachments,
            ..
        } if label == "Plan"
            && text == "@SPEC.md"
            && matches!(attachments.as_slice(), [attachment]
                if attachment.spec.path == Path::new("SPEC.md"))
    )));
}

#[tokio::test]
async fn clicking_actionable_status_modules_opens_their_picker() {
    let temporary = tempfile::tempdir().unwrap();
    let config = cagent_agent::config::ConfigSnapshot::parse(
        &temporary.path().join("config.toml"),
        "version = 1\ndefault_model = { model = 'mock/echo' }\n[providers.mock]\ntype = 'mock'\nenabled = true\n",
    )
    .unwrap();
    let runtime = AgentRuntime::open(
        cagent_agent::runtime::RuntimeOptions::new(temporary.path().join("clicks.db"))
            .with_config(config),
    )
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
    let column_for = |module| {
        (0..80)
            .find(|column| {
                crate::render::status_line_module_at(
                    &app.status_line_config,
                    &app.status_line_values(),
                    80,
                    *column,
                ) == Some(module)
            })
            .unwrap()
    };
    let mode_column = column_for(StatusLineModule::Mode);
    let provider_column = column_for(StatusLineModule::Provider);
    let model_column = column_for(StatusLineModule::Model);
    app.click_status_line_module(&session, 80, 24, 23, mode_column)
        .await
        .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Profiles {
            kind: ProfileKind::Mode,
            ..
        })
    ));
    app.surfaces.clear();

    app.click_status_line_module(&session, 80, 24, 23, provider_column)
        .await
        .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Providers { .. })
    ));
    app.surfaces.clear();

    app.click_status_line_module(&session, 80, 24, 23, model_column)
        .await
        .unwrap();
    assert!(matches!(app.surfaces.last(), Some(Surface::Models { .. })));

    app.surfaces.clear();
    app.supervised_work
        .rows
        .push(cagent_agent::presentation::SupervisedWork::Terminal {
            terminal: Box::new(cagent_agent::tools::TerminalSnapshot {
                id: cagent_agent::tools::TerminalId::new(),
                owner: cagent_agent::protocol::ConversationId::new(),
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
    let background_column = (0..80)
        .find(|column| {
            crate::render::status_line_background_hint_at(
                &app.status_line_config,
                &app.status_line_values(),
                80,
                *column,
            )
        })
        .expect("background hint should have a clickable column");
    app.click_status_line_module(&session, 80, 24, 23, background_column)
        .await
        .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::SupervisedWork { .. })
    ));
}

#[tokio::test]
async fn surface_status_row_consumes_actionable_and_inert_clicks() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = AgentRuntime::open(cagent_agent::runtime::RuntimeOptions::new(
        temporary.path().join("surface-status-clicks.db"),
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
    app.render_width = 80;
    app.render_height = 24;
    let expanded = || Surface::Expanded {
        view: ExpandedView::WebFetch {
            url: "https://example.com".into(),
            redirected_url: None,
            format: cagent_agent::WebFetchFormat::Text,
            output: "content".into(),
        },
        scroll: 0,
        viewport_rows: 20,
    };
    app.surfaces.push(expanded());
    app.bash_output_hits.push(BashOutputHit {
        row: 23,
        terminal_id: None,
        command: "echo underneath".into(),
        output: "underneath".into(),
        ansi_output: "underneath".into(),
        completion: Some("[exited with status 0]".into()),
    });
    let hints = app.contextual_key_hints(&surface_status(app.surfaces.last().unwrap()));
    let close_column = u16::try_from(2 + hints.find("Enter/Esc close").unwrap()).unwrap();
    let click = |column| {
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row: 23,
            modifiers: KeyModifiers::NONE,
        })
    };

    app.handle_terminal_event(&session, click(close_column))
        .await
        .unwrap();
    assert!(app.surfaces.is_empty());

    app.surfaces.push(expanded());
    app.handle_terminal_event(&session, click(0)).await.unwrap();
    assert!(matches!(
        app.surfaces.as_slice(),
        [Surface::Expanded {
            view: ExpandedView::WebFetch { .. },
            ..
        }]
    ));

    app.path_hits.push(PathHit {
        row: 10,
        columns: 4..20,
        path: temporary.path().join("underneath.txt"),
        line: None,
        directory: false,
    });
    app.bash_output_hits.push(BashOutputHit {
        row: 10,
        terminal_id: None,
        command: "echo underneath".into(),
        output: "underneath".into(),
        ansi_output: "underneath".into(),
        completion: Some("[exited with status 0]".into()),
    });
    app.handle_terminal_event(
        &session,
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 4,
            row: 10,
            modifiers: KeyModifiers::NONE,
        }),
    )
    .await
    .unwrap();
    assert_eq!(app.surfaces.len(), 1);
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Expanded {
            view: ExpandedView::WebFetch { .. },
            ..
        })
    ));
}

#[tokio::test]
async fn unknown_slash_commands_are_system_messages_and_leading_space_sends_prompt() {
    let temporary = tempfile::tempdir().unwrap();
    let config = cagent_agent::config::ConfigSnapshot::parse(
        &temporary.path().join("config.toml"),
        "version = 1\ndefault_model = { model = 'mock/echo' }\n[providers.mock]\ntype = 'mock'\nenabled = true\n",
    )
    .unwrap();
    let runtime = AgentRuntime::open(
        cagent_agent::runtime::RuntimeOptions::new(temporary.path().join("commands.db"))
            .with_config(config),
    )
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

    app.replace_draft("/not-a-command");
    app.submit_draft(&session, QueueTarget::NextBoundary)
        .await
        .unwrap();
    assert!(
        session
            .reload_composer_history_entries()
            .await
            .unwrap()
            .iter()
            .any(|entry| entry.text == "/not-a-command")
    );
    assert!(
        session
            .history()
            .await
            .unwrap()
            .iter()
            .all(|node| node.kind != NodeKind::UserMessage)
    );
    let snapshot = session.attach().await.unwrap().snapshot;
    assert!(snapshot.transcript.iter().any(|block| {
        matches!(
            &block.kind,
            cagent_agent::protocol::TranscriptBlockKind::Notice { message }
                if message == "Unknown command: /not-a-command"
        )
    }));
    assert!(app.history.iter().any(|block| {
        matches!(
            &block.kind,
            cagent_agent::protocol::TranscriptBlockKind::Notice { message }
                if message == "Unknown command: /not-a-command"
        )
    }));

    session
        .append_system_message("Worked for 1m 48s")
        .await
        .unwrap();
    app.apply_session_snapshot(&session.attach().await.unwrap().snapshot);
    assert!(matches!(
        app.history.last(),
        Some(TranscriptBlock { kind: cagent_agent::protocol::TranscriptBlockKind::Notice { message }, .. }) if message == "Worked for 1m 48s"
    ));

    session
        .submit(SessionCommand::new(SessionAction::RenameConversation {
            title: "Renamed conversation".into(),
        }))
        .await
        .unwrap();
    let snapshot = session.attach().await.unwrap().snapshot;
    assert!(snapshot.transcript.iter().any(|block| {
        matches!(
            &block.kind,
            cagent_agent::protocol::TranscriptBlockKind::Notice { message }
                if message == "Session renamed to Renamed conversation"
        )
    }));
    app.apply_session_snapshot(&snapshot);
    assert!(app.history.iter().any(|block| {
        matches!(
            &block.kind,
            cagent_agent::protocol::TranscriptBlockKind::Notice { message }
                if message == "Session renamed to Renamed conversation"
        )
    }));

    app.replace_draft(" /not-a-command");
    app.submit_draft(&session, QueueTarget::NextBoundary)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if session.history().await.unwrap().iter().any(|node| {
                node.kind == NodeKind::UserMessage && node.content["text"] == "/not-a-command"
            }) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn dir_command_reports_and_changes_the_session_directory() {
    let temporary = tempfile::tempdir().unwrap();
    let child = temporary.path().join("child");
    std::fs::create_dir(&child).unwrap();
    let config = cagent_agent::config::ConfigSnapshot::parse(
        &temporary.path().join("config.toml"),
        "version = 1\ndefault_model = { model = 'mock/echo' }\n[providers.mock]\ntype = 'mock'\nenabled = true\n",
    )
    .unwrap();
    let runtime = AgentRuntime::open(
        cagent_agent::runtime::RuntimeOptions::new(temporary.path().join("dir-command-data"))
            .with_config(config),
    )
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

    app.replace_draft("/dir");
    app.submit_draft(&session, QueueTarget::NextBoundary)
        .await
        .unwrap();
    let shown = cagent_agent::runtime::worktrees::display_absolute_path(temporary.path());
    assert!(app.history.iter().any(|block| matches!(
        &block.kind,
        cagent_agent::protocol::TranscriptBlockKind::Notice { message }
            if message == &format!("Current directory: {shown}")
    )));
    assert!(
        session
            .history()
            .await
            .unwrap()
            .iter()
            .all(|node| node.kind != NodeKind::UserMessage)
    );

    app.replace_draft("/dir child");
    app.submit_draft(&session, QueueTarget::NextBoundary)
        .await
        .unwrap();
    assert_eq!(app.workspace, child.canonicalize().unwrap());
    assert_eq!(session.attach().await.unwrap().snapshot.cwd, app.workspace);
    assert!(
        session
            .reload_composer_history_entries()
            .await
            .unwrap()
            .iter()
            .any(|entry| entry.text == "/dir child")
    );
}

#[tokio::test]
async fn submitting_a_message_returns_the_log_to_its_tail() {
    let temporary = tempfile::tempdir().unwrap();
    let config = cagent_agent::config::ConfigSnapshot::parse(
        &temporary.path().join("config.toml"),
        "version = 1\ndefault_model = { model = 'mock/echo' }\n[providers.mock]\ntype = 'mock'\nenabled = true\n",
    )
    .unwrap();
    let runtime = AgentRuntime::open(
        cagent_agent::runtime::RuntimeOptions::new(temporary.path().join("submit-scroll.db"))
            .with_config(config),
    )
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
    app.follow_history_tail = false;
    app.history_scroll = 12;
    app.replace_draft("discard this local recall after sending");
    app.clear_draft_for_recall();
    assert!(app.cleared_draft.is_some());
    app.replace_draft("show the newest message");

    app.submit_draft(&session, QueueTarget::NextBoundary)
        .await
        .unwrap();

    assert!(app.follow_history_tail);
    assert!(app.cleared_draft.is_none());
}

#[tokio::test]
async fn interactive_queue_submission_updates_the_ui_before_runtime_acceptance() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = AgentRuntime::open(cagent_agent::runtime::RuntimeOptions::new(
        temporary.path().join("optimistic-queue.db"),
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
    app.active = true;
    app.replace_draft("queue immediately");

    app.submit_draft_deferred(&session, QueueTarget::NextBoundary)
        .await
        .unwrap();

    assert!(app.draft.is_empty());
    assert_eq!(app.queued.len(), 1);
    assert_eq!(app.queued[0].text, "queue immediately");
    assert!(matches!(app.pending_action, Some(AppAction::Submit(_))));
    assert!(session.attach().await.unwrap().snapshot.queue.is_empty());
}

#[tokio::test]
async fn alt_n_opens_the_rename_editor_without_discarding_the_draft() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = AgentRuntime::open(cagent_agent::runtime::RuntimeOptions::new(
        temporary.path().join("rename-shortcut.db"),
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
    app.replace_draft("draft to preserve");

    app.handle_key(
        &session,
        KeyEvent::new(KeyCode::Char('n'), KeyModifiers::ALT),
    )
    .await
    .unwrap();

    assert_eq!(app.draft, "draft to preserve");
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Rename { title, cursor }) if title.is_empty() && *cursor == 0
    ));
}

#[tokio::test]
async fn retry_controls_do_not_replace_the_status_line_with_a_notice() {
    let temporary = tempfile::tempdir().unwrap();
    let config = cagent_agent::config::ConfigSnapshot::parse(
        &temporary.path().join("config.toml"),
        "version = 1\ndefault_model = { model = 'mock/echo' }\n[providers.mock]\ntype = 'mock'\nenabled = true\n",
    )
    .unwrap();
    let runtime = AgentRuntime::open(
        cagent_agent::runtime::RuntimeOptions::new(temporary.path().join("retry-notice.db"))
            .with_config(config),
    )
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

    session
        .submit(SessionCommand::submit_input("hello"))
        .await
        .unwrap();
    wait_for_completed_assistant_nodes(&session, 1).await;

    app.replace_draft("/retry");
    app.submit_draft(&session, QueueTarget::NextBoundary)
        .await
        .unwrap();
    assert!(app.notice.is_none());

    wait_for_completed_assistant_nodes(&session, 2).await;

    app.handle_key(
        &session,
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::ALT),
    )
    .await
    .unwrap();
    assert!(app.notice.is_none());
}

async fn wait_for_completed_assistant_nodes(session: &SessionHandle, expected: usize) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let completed = session
                .history()
                .await
                .unwrap()
                .into_iter()
                .filter(|node| {
                    node.kind == NodeKind::AssistantMessage && node.status == "completed"
                })
                .count();
            if completed >= expected {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn model_picker_ctrl_d_uses_the_agent_default() {
    let temporary = tempfile::tempdir().unwrap();
    let config = cagent_agent::config::ConfigSnapshot::parse(
        &temporary.path().join("config.toml"),
        "version = 1\ndefault_model = { model = 'mock/global' }\n[providers.mock]\ntype = 'mock'\nenabled = true\n[agents.general]\nmodel = { model = 'mock/agent-default' }\n",
    )
    .unwrap();
    let runtime = AgentRuntime::open(
        cagent_agent::runtime::RuntimeOptions::new(temporary.path().join("model-default.db"))
            .with_config(config),
    )
    .await
    .unwrap();
    let session = runtime
        .create_session(cagent_agent::runtime::NewSession {
            workspace: temporary.path().to_path_buf(),
        })
        .await
        .unwrap();
    session
        .submit(SessionCommand::new(SessionAction::ChangeModelAndEffort {
            provider: "mock".into(),
            model: "manual".into(),
            effort: None,
        }))
        .await
        .unwrap();
    let mut app = App::new(
        temporary.path(),
        ("mock".into(), "manual".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.surfaces.push(Surface::Models {
        rows: Vec::new(),
        list: ListState::selectable(0),
        query: String::new(),
        query_cursor: 0,
        selection_target: ModelSelectionTarget::Conversation,
    });

    app.handle_key(
        &session,
        KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
    )
    .await
    .unwrap();

    assert!(app.surfaces.is_empty());
    assert_eq!(
        session.model_selection().await.unwrap().unwrap().1,
        "agent-default"
    );
}

#[test]
fn switching_conversations_clears_transient_notice() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.set_notice("previous notice");
    app.welcome_scrolled_past = true;
    app.transcript_prefetch_armed = false;
    let stale_request: cagent_agent::protocol::TranscriptCursor =
        serde_json::from_value(serde_json::json!({
            "conversation_id": cagent_agent::protocol::ConversationId::new(),
            "older_node_id": cagent_agent::protocol::NodeId::new(),
            "newer_node_id": cagent_agent::protocol::NodeId::new(),
        }))
        .unwrap();
    app.transcript_page_request = Some(stale_request.clone());

    app.reset_for_new_session(None, BTreeSet::new(), "ask", None, 80);

    assert!(app.notice.is_none());
    assert!(app.transcript_prefetch_armed);
    assert!(!app.welcome_scrolled_past);
    assert!(app.transcript_page_request.is_none());
    assert!(!app.finish_transcript_page_request(&stale_request));
}

#[test]
#[allow(clippy::too_many_lines)]
#[allow(clippy::default_trait_access)]
fn stateful_surfaces_replace_their_state_and_escape_closes_once() {
    std::thread::Builder::new()
        .name("stateful-surfaces-test".into())
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(stateful_surfaces_replace_their_state_and_escape_closes_once_inner());
        })
        .unwrap()
        .join()
        .unwrap();
}

async fn stateful_surfaces_replace_their_state_and_escape_closes_once_inner() {
    let temporary = tempfile::tempdir().unwrap();
    let config = cagent_agent::config::ConfigSnapshot::parse(
        &temporary.path().join("config.toml"),
            "version = 1\ndefault_model = { model = 'mock/echo' }\n[providers.mock]\ntype = 'mock'\nenabled = true\n",
        )
        .unwrap();
    let runtime = AgentRuntime::open(
        cagent_agent::runtime::RuntimeOptions::new(temporary.path().join("cagent.db"))
            .with_config(config),
    )
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
    app.surfaces.push(Surface::Models {
        rows: vec![ModelPickerRow {
            provider: "mock".into(),
            id: "echo".into(),
            label: "Echo".into(),
            efforts: Vec::new(),
            reasoning_control: None,
            badges: Vec::new(),
            current: true,
            favourite: false,
            release_date: None,
        }],
        list: ListState::selectable(1),
        query: String::new(),
        query_cursor: 0,
        selection_target: ModelSelectionTarget::Conversation,
    });

    app.handle_key(
        &session,
        KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE),
    )
    .await
    .unwrap();

    assert_eq!(app.surfaces.len(), 1);
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Models { query, .. }) if query == "e"
    ));

    app.active = true;
    app.handle_key(&session, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();

    assert!(app.surfaces.is_empty());
    app.active = false;

    app.draft = "/".into();
    app.cursor = 1;
    app.slash_dismissed = false;
    app.active = true;
    app.handle_key(&session, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.slash_dismissed);
    assert!(app.active);
    app.clear_draft();
    app.active = false;

    app.attachment_completion = Some(AttachmentCompletion {
        start: 0,
        end: 0,
        rows: Vec::new(),
        list: ListState::selectable(0),
    });
    app.active = true;
    app.handle_key(&session, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.attachment_completion.is_none());
    assert!(app.active);
    app.active = false;

    app.queued.push(QueuedMessage {
        id: cagent_agent::protocol::QueuedMessageId::default(),
        position: 0,
        target: QueueTarget::NextBoundary,
        kind: cagent_agent::protocol::QueuedItemKind::Prompt,
        mode: None,
        command_text: None,
        text: "queued".into(),
        attachments: Vec::new(),
        images: Vec::new(),
        image_chips: Vec::new(),
        blocked_by_startup: false,
        require_subagent: false,
    });
    app.selected_queue = Some(0);
    app.active = true;
    app.handle_key(&session, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.selected_queue.is_none());
    assert!(app.active);
    app.queued.clear();

    for key in [
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
    ] {
        session
            .submit(SessionCommand::new(
                SessionAction::QueueInputWithAttachments {
                    text: "queued".into(),
                    target: QueueTarget::NextBoundary,
                    attachments: Vec::new(),
                },
            ))
            .await
            .unwrap();
        app.queued = session.attach().await.unwrap().snapshot.queue;
        let message = app.queued.first().cloned().unwrap();
        app.editing_queue = Some(message.clone());
        app.replace_draft("queued");
        app.handle_key(&session, key).await.unwrap();
        assert!(app.editing_queue.is_none());
        assert!(app.draft.is_empty());
        assert!(app.active);
        assert!(
            !session
                .attach()
                .await
                .unwrap()
                .snapshot
                .queue
                .iter()
                .any(|queued| queued.id == message.id)
        );
    }
    app.active = false;

    app.surfaces.push(Surface::Effort {
        provider: "mock".into(),
        model: "echo".into(),
        rows: vec!["low".into(), "high".into()],
        reasoning_control: Some(cagent_agent::provider::ReasoningControl::Effort),
        list: ListState::selectable(2),
        selection_target: ModelSelectionTarget::Conversation,
    });
    app.handle_key(&session, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.surfaces.len(), 1);
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Effort { list, .. }) if list.selected == Some(1)
    ));
    app.handle_key(
        &session,
        KeyEvent::new(KeyCode::Char('\x1b'), KeyModifiers::NONE),
    )
    .await
    .unwrap();
    assert!(app.surfaces.is_empty());

    app.surfaces.push(Surface::Paths {
        rows: vec![
            ListEntry {
                path: PathBuf::from("first"),
                kind: ListEntryKind::File,
            },
            ListEntry {
                path: PathBuf::from("second"),
                kind: ListEntryKind::File,
            },
        ],
        list: ListState::selectable(2),
        token_start: 0,
    });
    app.handle_key(&session, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.surfaces.len(), 1);
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Paths { list, .. }) if list.selected == Some(1)
    ));
    app.handle_key(&session, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.surfaces.is_empty());

    for key in [KeyCode::Enter, KeyCode::Esc] {
        app.surfaces.push(Surface::ProviderSetup {
            provider_id: "openai".into(),
            provider: "OpenAI".into(),
            instructions: "Set OPENAI_API_KEY".into(),
            credential_environment_variable: Some("OPENAI_API_KEY".into()),
            auth_challenge: None,
            managed_auth: false,
            api_key_auth: false,
            api_key: String::new(),
            api_key_cursor: 0,
            auth_flows: Vec::new(),
            authenticated: false,
        });
        app.handle_key(&session, KeyEvent::new(key, KeyModifiers::NONE))
            .await
            .unwrap();
        assert!(app.surfaces.is_empty());
    }

    app.surfaces.push(Surface::Providers {
        rows: vec![cagent_agent::presentation::ProviderPickerRow {
            id: "mock".into(),
            label: "Mock".into(),
            status: "enabled".into(),
            setup_instructions: None,
            configuration_instructions: "Configure Mock".into(),
            credential_environment_variable: None,
            enabled: true,
            managed_auth: false,
            api_key_auth: false,
            has_managed_api_key: false,
            auth_flows: Vec::new(),
        }],
        list: ListState {
            selected: Some(1),
            offset: 0,
            item_count: 2,
        },
        query: String::new(),
        query_cursor: 0,
        model_configuration_follows: false,
    });
    app.handle_key(
        &session,
        KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
    )
    .await
    .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Providers {
            query,
            query_cursor: 0,
            list: ListState { selected: Some(1), .. },
            ..
        }) if query.is_empty()
    ));
    app.handle_key(&session, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();

    app.surfaces.push(Surface::Models {
        rows: vec![
            ModelPickerRow {
                provider: "mock".into(),
                id: "echo".into(),
                label: "Echo".into(),
                efforts: Vec::new(),
                reasoning_control: None,
                badges: Vec::new(),
                current: true,
                favourite: false,
                release_date: None,
            },
            ModelPickerRow {
                provider: "mock".into(),
                id: "other".into(),
                label: "Other".into(),
                efforts: Vec::new(),
                reasoning_control: None,
                badges: Vec::new(),
                current: false,
                favourite: false,
                release_date: None,
            },
        ],
        list: ListState {
            selected: Some(1),
            offset: 0,
            item_count: 2,
        },
        query: String::new(),
        query_cursor: 0,
        selection_target: ModelSelectionTarget::Conversation,
    });
    app.handle_key(
        &session,
        KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
    )
    .await
    .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Models {
            query,
            query_cursor: 0,
            list: ListState { selected: Some(1), .. },
            ..
        }) if query.is_empty()
    ));
    app.handle_key(&session, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();

    app.surfaces.push(Surface::Providers {
        rows: vec![cagent_agent::presentation::ProviderPickerRow {
            id: "chatgpt".into(),
            label: "ChatGPT subscription".into(),
            status: "enabled".into(),
            setup_instructions: None,
            configuration_instructions: "Reconnect your ChatGPT subscription account.".into(),
            credential_environment_variable: None,
            enabled: true,
            managed_auth: true,
            api_key_auth: false,
            has_managed_api_key: false,
            auth_flows: vec![
                cagent_agent::provider::AuthFlow::BrowserPkce,
                cagent_agent::provider::AuthFlow::DeviceCode,
            ],
        }],
        list: ListState {
            selected: Some(1),
            offset: 0,
            item_count: 2,
        },
        query: String::new(),
        query_cursor: 0,
        model_configuration_follows: false,
    });
    app.handle_key(
        &session,
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
    )
    .await
    .unwrap();
    assert_eq!(app.surfaces.len(), 2);
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::ProviderSetup {
            provider_id,
            managed_auth: true,
            authenticated: false,
            ..
        }) if provider_id == "chatgpt"
    ));

    app.replace_draft("clear through the cancel binding");
    app.active = true;
    assert!(
        !app.handle_key(
            &session,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        )
        .await
        .unwrap()
    );
    assert!(app.draft.is_empty());
    app.recall_previous_history();
    assert_eq!(app.draft, "clear through the cancel binding");
    app.clear_draft();

    assert!(app.notice.is_none());
    assert!(app.active);

    assert!(
        !app.handle_key(
            &session,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        )
        .await
        .unwrap()
    );
    assert_eq!(app.notice.as_deref(), Some(EXIT_NOTICE));
    assert!(app.active);

    assert!(
        app.handle_key(
            &session,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        )
        .await
        .unwrap()
    );
}

#[tokio::test]
async fn effort_escape_returns_to_the_model_surface() {
    let temporary = tempfile::tempdir().unwrap();
    let config = cagent_agent::config::ConfigSnapshot::parse(
        &temporary.path().join("config.toml"),
            "version = 1\ndefault_model = { model = 'mock/echo' }\n[providers.mock]\ntype = 'mock'\nenabled = true\n",
        )
        .unwrap();
    let runtime = AgentRuntime::open(
        cagent_agent::runtime::RuntimeOptions::new(temporary.path().join("cagent.db"))
            .with_config(config),
    )
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
    app.effort = Some("high".into());
    app.surfaces.push(Surface::Models {
        rows: vec![ModelPickerRow {
            provider: "mock".into(),
            id: "echo".into(),
            label: "Echo".into(),
            efforts: vec!["low".into(), "high".into()],
            reasoning_control: Some(cagent_agent::provider::ReasoningControl::Effort),
            badges: Vec::new(),
            current: true,
            favourite: false,
            release_date: None,
        }],
        list: ListState::selectable(1),
        query: String::new(),
        query_cursor: 0,
        selection_target: ModelSelectionTarget::Conversation,
    });

    app.handle_key(&session, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.surfaces.len(), 2);
    assert!(matches!(app.surfaces.first(), Some(Surface::Models { .. })));
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Effort { list, .. }) if list.selected == Some(1)
    ));

    app.handle_key(&session, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.surfaces.len(), 1);
    assert!(matches!(app.surfaces.last(), Some(Surface::Models { .. })));
}

#[test]
fn effort_picker_uses_the_shared_bounded_list() {
    let rows = (0..10)
        .map(|index| format!("effort-{index}"))
        .collect::<Vec<_>>();
    let layout = super::surfaces::surface_layout_with_viewport(
        &Surface::Effort {
            provider: "mock".into(),
            model: "echo".into(),
            list: ListState::selectable(rows.len()),
            rows,
            reasoning_control: Some(cagent_agent::provider::ReasoningControl::Effort),
            selection_target: ModelSelectionTarget::Conversation,
        },
        Path::new("/workspace"),
        80,
        usize::MAX,
    );

    assert_eq!(layout.lines.len(), VISIBLE_MENU_ITEMS + 3);
    assert_eq!(layout.hits[1], ListHit::None);
    assert_eq!(layout.hits[2], ListHit::Item(0));
    assert_eq!(layout.hits[VISIBLE_MENU_ITEMS + 1], ListHit::Item(7));
    assert_eq!(layout.hits.last(), Some(&ListHit::Down));
    assert_eq!(
        layout.lines.last().map(ToString::to_string).as_deref(),
        Some("  ↓")
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn settings_command_opens_and_boolean_edits_persist() {
    std::thread::Builder::new()
        .name("settings-command-test".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(settings_command_opens_and_boolean_edits_persist_inner());
        })
        .unwrap()
        .join()
        .unwrap();
}

#[allow(clippy::too_many_lines)]
async fn settings_command_opens_and_boolean_edits_persist_inner() {
    let temporary = tempfile::tempdir().unwrap();
    let config_path = temporary.path().join("config.toml");
    std::fs::write(
        &config_path,
        "version = 1\ndefault_model = { model = 'mock/echo' }\n[providers.mock]\ntype = 'mock'\nenabled = true\n[ui]\ntheme = 'light'\n",
    )
    .unwrap();
    let config = cagent_agent::config::ConfigStore::open(&config_path).unwrap();
    let runtime = AgentRuntime::open(
        cagent_agent::runtime::RuntimeOptions::new(temporary.path().join("cagent.db"))
            .with_config(config),
    )
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
    app.replace_draft("/settings");

    app.submit_draft(&session, QueueTarget::NextBoundary)
        .await
        .unwrap();

    let Some(Surface::Settings { rows, section, .. }) = app.surfaces.last() else {
        panic!("settings surface should open");
    };
    assert!(rows.len() > VISIBLE_MENU_ITEMS);
    assert!(rows.iter().any(|row| {
        row.definition.key == "ui.title.requested_input"
            && row.definition.section == cagent_agent::config::SettingSection::Ui
    }));
    assert!(rows.iter().any(|row| {
        row.definition.key == "ui.title.progress"
            && row.definition.section == cagent_agent::config::SettingSection::Ui
    }));
    assert!(rows.iter().any(|row| {
        row.definition.key == "ui.progress_osc"
            && row.definition.section == cagent_agent::config::SettingSection::Ui
    }));
    for key in [
        "ui.bell.requested_input",
        "ui.bell.completed_turn",
        "ui.bell.method",
    ] {
        assert!(rows.iter().any(|row| {
            row.definition.key == key
                && row.definition.section == cagent_agent::config::SettingSection::Ui
        }));
    }
    assert_eq!(*section, cagent_agent::config::SettingSection::General);

    for character in "theme".chars() {
        app.handle_key(
            &session,
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
        )
        .await
        .unwrap();
    }
    let Some(Surface::Settings {
        rows,
        section,
        query,
        list,
        ..
    }) = app.surfaces.last()
    else {
        unreachable!();
    };
    assert_eq!(query, "theme");
    let visible = filter_settings_rows(rows, *section, query);
    assert!(visible.iter().any(|row| {
        row.definition.key == "ui.theme"
            && row.definition.section == cagent_agent::config::SettingSection::Ui
    }));
    assert_eq!(list.item_count, visible.len());
    let filtered_lines = surface_lines(app.surfaces.last().unwrap(), &app.workspace, 120);
    assert!(
        !filtered_lines
            .iter()
            .any(|line| line.to_string().contains("[ General ]"))
    );
    assert!(
        filtered_lines
            .iter()
            .any(|line| line.to_string().contains("Theme"))
    );
    app.handle_key(&session, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::SettingChoices { setting, .. }) if setting.key == "ui.theme"
    ));
    app.handle_key(&session, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Settings { query, .. }) if query == "theme"
    ));
    app.handle_key(
        &session,
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
    )
    .await
    .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Settings { query, .. }) if query == "theme"
    ));
    assert!(
        session
            .settings()
            .unwrap()
            .into_iter()
            .find(|row| row.definition.key == "ui.theme")
            .unwrap()
            .explicit_value
            .is_none()
    );

    for _ in 0.."theme".len() {
        app.handle_key(
            &session,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        )
        .await
        .unwrap();
    }
    app.handle_key(
        &session,
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
    )
    .await
    .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Settings { query, .. }) if query == "r"
    ));
    app.handle_key(
        &session,
        KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
    )
    .await
    .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Settings {
            section: cagent_agent::config::SettingSection::General,
            query,
            ..
        }) if query.is_empty()
    ));

    app.render_width = 120;
    app.render_height = 30;
    let tabs = surface_lines(
        app.surfaces.last().unwrap(),
        &app.workspace,
        app.render_width,
    )[2]
    .to_string();
    for label in ["[ UI ]", "[ Keybindings ]"] {
        let column = u16::try_from(tabs.find(label).unwrap() + 2).unwrap();
        let tab_row = app.render_height - app.controls_height(app.render_width, app.render_height)
            + app.render_height.saturating_sub(1).min(2)
            + 2;
        app.handle_terminal_event(
            &session,
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column,
                row: tab_row,
                modifiers: KeyModifiers::NONE,
            }),
        )
        .await
        .unwrap();
    }
    assert_eq!(app.surfaces.len(), 1);
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Settings {
            section: cagent_agent::config::SettingSection::Keybindings,
            list: ListState {
                selected: Some(0),
                offset: 0,
                ..
            },
            ..
        })
    ));

    let general_column = u16::try_from(tabs.find("[ General ]").unwrap() + 2).unwrap();
    let tab_row = app.render_height - app.controls_height(app.render_width, app.render_height)
        + app.render_height.saturating_sub(1).min(2)
        + 2;
    app.handle_terminal_event(
        &session,
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: general_column,
            row: tab_row,
            modifiers: KeyModifiers::NONE,
        }),
    )
    .await
    .unwrap();

    if let Some(Surface::Settings {
        rows,
        section,
        list,
        ..
    }) = app.surfaces.last_mut()
    {
        *section = cagent_agent::config::SettingSection::Keybindings;
        let count = rows
            .iter()
            .filter(|row| row.definition.section == *section)
            .count();
        *list = ListState::selectable(count);
        list.end(ListMode::Selectable, VISIBLE_MENU_ITEMS);
        assert!(list.offset > 0);
    }

    app.handle_key(&session, KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Settings {
            section: cagent_agent::config::SettingSection::General,
            list: ListState {
                selected: Some(0),
                offset: 0,
                ..
            },
            ..
        })
    ));

    app.handle_key(&session, KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Settings {
            section: cagent_agent::config::SettingSection::Ui,
            list: ListState {
                selected: Some(0),
                offset: 0,
                ..
            },
            ..
        })
    ));
    if let Some(Surface::Settings {
        rows,
        section,
        list,
        ..
    }) = app.surfaces.last_mut()
    {
        let scrollback_index = rows
            .iter()
            .filter(|row| row.definition.section == *section)
            .position(|row| row.definition.key == "ui.scrollback_reflow_rows")
            .expect("scrollback setting");
        *list = ListState::selectable_at(
            rows.iter()
                .filter(|row| row.definition.section == *section)
                .count(),
            scrollback_index,
            VISIBLE_MENU_ITEMS,
        );
    }
    app.handle_key(&session, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    let setting_lines = surface_lines(
        app.surfaces.last().expect("numeric setting editor"),
        &app.workspace,
        80,
    );
    assert_eq!(
        setting_lines.last().expect("setting input lines").width(),
        0
    );
    let Some(Surface::SettingInput { value, cursor, .. }) = app.surfaces.last_mut() else {
        panic!("numeric setting should use the input editor");
    };
    assert_eq!(value, "2000");
    *value = "0".into();
    *cursor = 1;
    app.handle_key(&session, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::SettingInput { .. })
    ));
    assert_eq!(
        cagent_agent::config::ConfigSnapshot::load(&config_path)
            .unwrap()
            .scrollback_reflow_rows(),
        2_000
    );
    let Some(Surface::SettingInput { value, cursor, .. }) = app.surfaces.last_mut() else {
        unreachable!();
    };
    *value = "321".into();
    *cursor = 3;
    app.handle_key(&session, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(
        cagent_agent::config::ConfigSnapshot::load(&config_path)
            .unwrap()
            .scrollback_reflow_rows(),
        321
    );
    app.handle_key(&session, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    let Some(Surface::SettingInput { value, cursor, .. }) = app.surfaces.last_mut() else {
        panic!("scrollback should reopen in the input editor");
    };
    value.clear();
    *cursor = 0;
    app.handle_key(&session, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(
        cagent_agent::config::ConfigSnapshot::load(&config_path)
            .unwrap()
            .scrollback_reflow_rows(),
        2_000
    );

    // Presets should not be copied into the custom editor, while an explicit
    // custom array should be restored when the editor is opened again.
    if let Some(Surface::Settings {
        rows,
        section,
        list,
        ..
    }) = app.surfaces.last_mut()
    {
        let requested_input_index = rows
            .iter()
            .filter(|row| row.definition.section == *section)
            .position(|row| row.definition.key == "ui.title.requested_input")
            .expect("requested-input title setting");
        *list = ListState::selectable_at(
            rows.iter()
                .filter(|row| row.definition.section == *section)
                .count(),
            requested_input_index,
            VISIBLE_MENU_ITEMS,
        );
    }
    app.handle_key(&session, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    let custom_index = match app.surfaces.last() {
        Some(Surface::SettingChoices { choices, .. }) => choices
            .iter()
            .position(|(value, _)| value == "custom")
            .expect("custom bell choice"),
        _ => panic!("requested-input setting should use choices"),
    };
    if let Some(Surface::SettingChoices { list, choices, .. }) = app.surfaces.last_mut() {
        *list = ListState::selectable_at(choices.len(), custom_index, VISIBLE_MENU_ITEMS);
    }
    app.handle_key(&session, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    let Some(Surface::SettingInput { value, cursor, .. }) = app.surfaces.last_mut() else {
        panic!("requested-input custom setting should use the input editor");
    };
    assert_eq!(value, "[]");
    *value = r#"["!", " "]"#.into();
    *cursor = value.len();
    app.handle_key(&session, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(&session, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    let custom_index = match app.surfaces.last() {
        Some(Surface::SettingChoices { choices, .. }) => choices
            .iter()
            .position(|(value, _)| value == "custom")
            .expect("custom bell choice"),
        _ => panic!("requested-input setting should reopen choices"),
    };
    if let Some(Surface::SettingChoices { list, choices, .. }) = app.surfaces.last_mut() {
        *list = ListState::selectable_at(choices.len(), custom_index, VISIBLE_MENU_ITEMS);
    }
    app.handle_key(&session, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    let Some(Surface::SettingInput { value, cursor, .. }) = app.surfaces.last() else {
        panic!("requested-input custom setting should reopen in the input editor");
    };
    assert_eq!(value, r#"["!", " "]"#);
    assert_eq!(*cursor, value.len());
}

#[test]
fn settings_specialized_editors_persist_future_defaults_without_changing_the_session() {
    std::thread::Builder::new()
        .name("settings-specialized-editors-test".into())
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(settings_specialized_editors_persist_future_defaults_without_changing_the_session_inner());
        })
        .unwrap()
        .join()
        .unwrap();
}

async fn settings_specialized_editors_persist_future_defaults_without_changing_the_session_inner() {
    let temporary = tempfile::tempdir().unwrap();
    let config_path = temporary.path().join("config.toml");
    std::fs::write(
        &config_path,
        r#"version = 1
default_model = { model = "mock/echo-slow" }

[providers.mock]
type = "mock"
enabled = true

[agents.review]
description = "Review changes"

[agents.hidden]
enabled = false

[agents.worker]
availability = "subagent"
"#,
    )
    .unwrap();
    let config = cagent_agent::config::ConfigStore::open(&config_path).unwrap();
    let runtime = AgentRuntime::open(
        cagent_agent::runtime::RuntimeOptions::new(temporary.path().join("cagent.db"))
            .with_config(config),
    )
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
        ("mock".into(), "echo-slow".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.replace_draft("/settings");
    app.submit_draft(&session, QueueTarget::NextBoundary)
        .await
        .unwrap();

    let select_setting = |app: &mut App, key: &str| {
        let Some(Surface::Settings {
            rows,
            section,
            list,
            ..
        }) = app.surfaces.last_mut()
        else {
            panic!("settings surface should be active");
        };
        let visible = rows
            .iter()
            .filter(|row| row.definition.section == *section)
            .collect::<Vec<_>>();
        let selected = visible
            .iter()
            .position(|row| row.definition.key == key)
            .unwrap_or_else(|| panic!("missing setting {key}"));
        *list = ListState::selectable_at(visible.len(), selected, VISIBLE_MENU_ITEMS);
    };

    select_setting(&mut app, "tiers.small");
    app.handle_key(&session, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::SettingInput { value, .. })
            if value == "[]"
    ));
    app.handle_key(&session, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();
    session
        .save_setting(
            "tiers.small",
            r#"[{ model = "mock/echo-fast", effort = "low" }]"#,
        )
        .unwrap();
    if let Some(Surface::Settings { rows, .. }) = app.surfaces.last_mut() {
        *rows = session.settings().unwrap();
    }
    select_setting(&mut app, "tiers.small");
    app.handle_key(&session, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::SettingInput { value, .. })
            if value == r#"[{ model = "mock/echo-fast", effort = "low" }]"#
    ));
    app.handle_key(&session, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();

    select_setting(&mut app, "subagents.strategy");
    app.handle_key(&session, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::SettingChoices { choices, .. })
            if choices == &[
                (
                    "off".into(),
                    "reject ordinary delegation requests".into(),
                ),
                (
                    "on_demand".into(),
                    "delegate only when the task clearly requires it".into(),
                ),
                (
                    "complex".into(),
                    "keep implementation primary; delegate independent work".into(),
                ),
                (
                    "aggressive".into(),
                    "proactively seek useful work to delegate".into(),
                ),
                ("always".into(), "delegate all substantive work".into()),
            ]
    ));
    assert_eq!(
        surface_lines(app.surfaces.last().unwrap(), temporary.path(), 80,)[1].to_string(),
        "  how readily primary agents delegate work"
    );
    app.handle_key(&session, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();

    select_setting(&mut app, "compaction.enabled");
    app.handle_key(&session, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::SettingChoices { choices, .. })
            if choices == &[
                (
                    "true".into(),
                    "automatically compact conversation context".into(),
                ),
                (
                    "false".into(),
                    "do not automatically compact conversation context".into(),
                ),
            ]
    ));
    assert_eq!(
        surface_lines(app.surfaces.last().unwrap(), temporary.path(), 80,)[1].to_string(),
        "  automatically compact conversation context"
    );
    app.handle_key(&session, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();

    select_setting(&mut app, "default_agent");
    app.handle_key(&session, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(
        surface_lines(app.surfaces.last().unwrap(), temporary.path(), 80,)[1].to_string(),
        "  agent profile used by future sessions"
    );
    let Some(Surface::SettingChoices { choices, list, .. }) = app.surfaces.last_mut() else {
        panic!("default agent should use a choice picker");
    };
    assert_eq!(
        choices,
        &[
            ("general".into(), "General coding agent".into()),
            ("review".into(), "Review changes".into()),
        ]
    );
    list.select(1, VISIBLE_MENU_ITEMS);
    app.handle_key(&session, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(
        cagent_agent::config::ConfigSnapshot::load(&config_path)
            .unwrap()
            .default_agent(),
        "review"
    );
    assert_eq!(session.active_profiles().await.unwrap().0, "general");

    select_setting(&mut app, "subagents.max_concurrent");
    app.handle_key(&session, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    let Some(Surface::SettingInput { value, cursor, .. }) = app.surfaces.last_mut() else {
        panic!("max concurrent should use a numeric input");
    };
    assert_eq!(value, "10");
    *value = "0".into();
    *cursor = 1;
    app.handle_key(&session, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(
        cagent_agent::config::ConfigSnapshot::load(&config_path)
            .unwrap()
            .subagent_max_concurrent(),
        0
    );

    let session_model = session.model_selection().await.unwrap();
    select_setting(&mut app, "tiers.small");
    app.handle_key(&session, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    let small_model_layout = super::surfaces::surface_layout_with_viewport(
        app.surfaces.last().unwrap(),
        temporary.path(),
        80,
        usize::MAX,
    );
    assert_eq!(
        small_model_layout.lines[1].to_string(),
        "  explicit candidates tried before Cagent's built-in small models (default: [])"
    );
    let Some(Surface::SettingInput { value, cursor, .. }) = app.surfaces.last_mut() else {
        panic!("small tier should use a TOML array editor");
    };
    *value = r#"[{ model = "mock/echo-fast", effort = "low" }]"#.into();
    *cursor = value.len();
    app.handle_key(&session, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Settings { .. })
    ));
    assert_eq!(
        cagent_agent::config::ConfigSnapshot::load(&config_path)
            .unwrap()
            .tier("small")
            .unwrap()[0]
            .model
            .to_string(),
        "mock/echo-fast"
    );
    assert_eq!(session.model_selection().await.unwrap(), session_model);

    app.handle_key(
        &session,
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
    )
    .await
    .unwrap();
    assert_eq!(
        cagent_agent::config::ConfigSnapshot::load(&config_path)
            .unwrap()
            .tier("small")
            .unwrap()
            .is_empty(),
        true
    );

    let Some(Surface::Settings { section, .. }) = app.surfaces.last_mut() else {
        unreachable!();
    };
    *section = cagent_agent::config::SettingSection::Ui;
    select_setting(&mut app, "ui.composer_max_rows");
    app.handle_key(&session, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::SettingInput { value, .. }) if value.is_empty()
    ));
}

#[test]
fn clicking_settings_scroll_arrows_moves_one_logical_page() {
    let temporary = tempfile::tempdir().unwrap();
    let config_path = temporary.path().join("config.toml");
    std::fs::write(&config_path, "version = 1\n").unwrap();
    let rows = cagent_agent::config::ConfigStore::open(config_path)
        .unwrap()
        .settings()
        .unwrap();
    let section = cagent_agent::config::SettingSection::Keybindings;
    let maximum = rows
        .iter()
        .filter(|row| row.definition.section == section)
        .count()
        .saturating_sub(1);
    assert!(maximum >= VISIBLE_MENU_ITEMS);

    let mut app = App::new(
        temporary.path(),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    let width = 120;
    let height = 30;
    app.render_width = width;
    app.render_height = height;
    app.surfaces.push(Surface::Settings {
        rows,
        section,
        list: ListState::selectable(maximum + 1),
        query: String::new(),
        query_cursor: 0,
    });

    let (_, _, top_panel, _) = app.control_lines_with_draft_limit(width, usize::MAX);
    assert_eq!(
        top_panel.last().map(ToString::to_string).as_deref(),
        Some("  ↓")
    );
    let layout = super::surfaces::surface_layout_with_viewport(
        app.surfaces.last().unwrap(),
        &app.workspace,
        width,
        usize::MAX,
    );
    assert_eq!(layout.hits[4], ListHit::Item(0));
    assert_eq!(layout.hits[5], ListHit::Item(0));

    if let Some(Surface::Settings { list, .. }) = app.surfaces.last_mut() {
        list.select(maximum, VISIBLE_MENU_ITEMS);
    }
    let (_, _, bottom_panel, _) = app.control_lines_with_draft_limit(width, usize::MAX);
    assert_eq!(bottom_panel.len(), top_panel.len());
    assert_eq!(
        bottom_panel.last().map(ToString::to_string),
        Some(String::new())
    );
    assert!(
        !bottom_panel
            .iter()
            .any(|line| line.to_string().trim() == "↓")
    );
    if let Some(Surface::Settings { list, .. }) = app.surfaces.last_mut() {
        list.home(ListMode::Selectable, VISIBLE_MENU_ITEMS);
    }

    let arrow_row = |app: &App, arrow: &str| {
        let controls_height = app.controls_height(width, height);
        let (_, _, composer_height, status_height) = app.control_heights_within(width, height);
        let surface_top = controls_height
            .saturating_sub(composer_height)
            .saturating_sub(status_height);
        let lines = surface_lines_with_viewport(
            app.surfaces.last().unwrap(),
            &app.workspace,
            width,
            usize::from(composer_height.saturating_sub(1)),
        );
        let index = lines
            .iter()
            .position(|line| line.to_string().trim() == arrow)
            .unwrap();
        height - controls_height + surface_top + u16::try_from(index).unwrap()
    };

    let down = arrow_row(&app, "↓");
    app.select_mouse_at(width, height, down, 2);
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Settings { list, .. })
            if list.selected == Some(VISIBLE_MENU_ITEMS)
    ));

    let up = arrow_row(&app, "↑");
    app.select_mouse_at(width, height, up, 2);
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Settings {
            list: ListState {
                selected: Some(0),
                ..
            },
            ..
        })
    ));
}

#[test]
fn settings_rows_truncate_with_an_ellipsis_and_show_override_metadata() {
    let section = cagent_agent::config::SettingSection::General;
    let rows = vec![cagent_agent::config::SettingRow {
        definition: cagent_agent::config::SettingDefinition {
            key: "example".into(),
            label: "A deliberately long setting title that must stay on one terminal line".into(),
            description:
                "A deliberately long setting description that must stay on one terminal line".into(),
            kind: cagent_agent::config::SettingKind::String,
            section,
            default_value: String::new(),
        },
        effective_value: String::new(),
        display_value: "default".into(),
        explicit_value: None,
    }];
    let layout = super::surfaces::surface_layout_with_viewport(
        &Surface::Settings {
            rows: rows.clone(),
            section,
            list: ListState::selectable(1),
            query: String::new(),
            query_cursor: 0,
        },
        Path::new("/tmp/project"),
        40,
        usize::MAX,
    );
    let item_rows = layout
        .hits
        .iter()
        .enumerate()
        .filter_map(|(index, hit)| (*hit == ListHit::Item(0)).then_some(index))
        .collect::<Vec<_>>();
    assert_eq!(item_rows.len(), 2);
    let title = &layout.lines[item_rows[0]];
    let description = &layout.lines[item_rows[1]];
    assert_eq!(title.width(), 40);
    assert!(title.to_string().ends_with('…'));
    assert_eq!(description.width(), 40);
    assert!(description.to_string().ends_with('…'));
    assert!(!description.to_string().contains("overridden"));
    assert_eq!(description.spans[1].style, DIM_STYLE);

    let overridden = cagent_agent::config::SettingRow {
        explicit_value: Some("custom".into()),
        ..rows[0].clone()
    };
    let overridden_layout = super::surfaces::surface_layout_with_viewport(
        &Surface::Settings {
            rows: vec![overridden],
            section,
            list: ListState::selectable(1),
            query: String::new(),
            query_cursor: 0,
        },
        Path::new("/tmp/project"),
        80,
        usize::MAX,
    );
    let metadata = overridden_layout
        .lines
        .iter()
        .find(|line| line.to_string().starts_with("    overridden · "))
        .unwrap();
    assert_eq!(metadata.spans[1].style, NOTICE_STYLE);
}

#[test]
fn filtered_settings_keep_the_full_height_without_a_tab_spacer() {
    let section = cagent_agent::config::SettingSection::General;
    let rows = (0..VISIBLE_MENU_ITEMS + 2)
        .map(|index| cagent_agent::config::SettingRow {
            definition: cagent_agent::config::SettingDefinition {
                key: format!("example.{index}"),
                label: format!("Example {index}"),
                description: "example setting".into(),
                kind: cagent_agent::config::SettingKind::String,
                section,
                default_value: "default".into(),
            },
            effective_value: "default".into(),
            display_value: "default".into(),
            explicit_value: None,
        })
        .collect::<Vec<_>>();
    let unfiltered = Surface::Settings {
        rows: rows.clone(),
        section,
        list: ListState::selectable(rows.len()),
        query: String::new(),
        query_cursor: 0,
    };
    let filtered = Surface::Settings {
        rows,
        section,
        list: ListState::selectable(1),
        query: "example".into(),
        query_cursor: "example".len(),
    };

    let unfiltered = super::surfaces::surface_layout_with_viewport(
        &unfiltered,
        Path::new("/tmp/project"),
        80,
        usize::MAX,
    );
    let filtered = super::surfaces::surface_layout_with_viewport(
        &filtered,
        Path::new("/tmp/project"),
        80,
        usize::MAX,
    );

    assert_eq!(filtered.lines.len(), unfiltered.lines.len());
    assert_eq!(filtered.hits[1], ListHit::None);
    assert_eq!(filtered.hits[2], ListHit::Item(0));
    assert!(filtered.lines[2].to_string().contains("Example 0"));
    assert_eq!(filtered.hits[18], ListHit::Item(VISIBLE_MENU_ITEMS));
    assert!(filtered.lines[18].to_string().contains("Example 8"));
    assert_eq!(filtered.hits[20], ListHit::Down);
}

#[test]
fn settings_reset_hint_is_dimmed_for_inherited_values() {
    let section = cagent_agent::config::SettingSection::General;
    let definition = cagent_agent::config::SettingDefinition {
        key: "example".into(),
        label: "Example".into(),
        description: "example setting".into(),
        kind: cagent_agent::config::SettingKind::String,
        section,
        default_value: "default".into(),
    };
    let row = |key: &str, explicit_value| cagent_agent::config::SettingRow {
        definition: cagent_agent::config::SettingDefinition {
            key: key.into(),
            ..definition.clone()
        },
        effective_value: "default".into(),
        display_value: "default".into(),
        explicit_value,
    };
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.surfaces.push(Surface::Settings {
        rows: vec![
            row("inherited", None),
            row("explicit", Some("custom".into())),
        ],
        section,
        list: ListState::selectable(2),
        query: String::new(),
        query_cursor: 0,
    });

    let reset_style = |app: &App| {
        app.control_lines_with_draft_limit(100, usize::MAX)
            .3
            .spans
            .into_iter()
            .find(|span| span.content == "Ctrl+R")
            .expect("settings reset key hint")
            .style
    };
    assert!(reset_style(&app).add_modifier.contains(Modifier::DIM));

    if let Some(Surface::Settings { list, .. }) = app.surfaces.last_mut() {
        list.select(1, VISIBLE_MENU_ITEMS);
    }
    assert!(!reset_style(&app).add_modifier.contains(Modifier::DIM));
}

#[test]
fn settings_choice_submenus_use_the_shared_bounded_list() {
    let setting = cagent_agent::config::SettingDefinition {
        key: "example".into(),
        label: "Example".into(),
        description: "Choose an example".into(),
        kind: cagent_agent::config::SettingKind::List(Vec::new()),
        section: cagent_agent::config::SettingSection::General,
        default_value: String::new(),
    };
    let choices = (0..10)
        .map(|index| (format!("choice-{index}"), String::new()))
        .collect::<Vec<_>>();
    let surface = Surface::SettingChoices {
        setting,
        list: ListState::selectable(choices.len()),
        choices,
        custom_value: None,
    };
    let layout = super::surfaces::surface_layout_with_viewport(
        &surface,
        Path::new("/tmp/project"),
        80,
        usize::MAX,
    );

    assert_eq!(layout.lines.len(), VISIBLE_MENU_ITEMS + 4);
    assert_eq!(layout.hits[1], ListHit::None);
    assert_eq!(layout.lines[1].to_string(), "  Choose an example");
    assert_eq!(layout.lines[1].spans[1].style, DIM_STYLE);
    assert_eq!(layout.hits[2], ListHit::None);
    assert_eq!(layout.hits[3], ListHit::Item(0));
    assert_eq!(layout.hits[VISIBLE_MENU_ITEMS + 2], ListHit::Item(7));
    assert_eq!(layout.hits.last(), Some(&ListHit::Down));
    assert_eq!(
        layout.lines.last().map(ToString::to_string).as_deref(),
        Some("  ↓")
    );
}

#[test]
fn default_agent_choices_truncate_like_the_agent_picker() {
    let setting = cagent_agent::config::SettingDefinition {
        key: "default_agent".into(),
        label: "Default agent".into(),
        description: "Choose the profile used for new sessions".into(),
        kind: cagent_agent::config::SettingKind::DefaultAgentPicker(vec![]),
        section: cagent_agent::config::SettingSection::General,
        default_value: "general".into(),
    };
    let layout = super::surfaces::surface_layout_with_viewport(
        &Surface::SettingChoices {
            setting,
            choices: vec![(
                "review".into(),
                "A deliberately long agent description that must stay on one line".into(),
            )],
            list: ListState::selectable(1),
            custom_value: None,
        },
        Path::new("/tmp/project"),
        40,
        usize::MAX,
    );
    let item_lines = layout
        .hits
        .iter()
        .enumerate()
        .filter_map(|(index, hit)| (*hit == ListHit::Item(0)).then_some(&layout.lines[index]))
        .collect::<Vec<_>>();

    assert_eq!(item_lines.len(), 1);
    assert_eq!(item_lines[0].width(), 40);
    assert!(item_lines[0].to_string().ends_with('…'));
}

#[test]
fn described_setting_choices_are_dim_and_truncate_to_one_line() {
    let choices = vec![(
        "on_demand".into(),
        "delegate only when the task clearly requires it".into(),
    )];
    let setting = cagent_agent::config::SettingDefinition {
        key: "subagents.strategy".into(),
        label: "Sub-agent strategy".into(),
        description: "how readily primary agents delegate work".into(),
        kind: cagent_agent::config::SettingKind::List(choices.clone()),
        section: cagent_agent::config::SettingSection::General,
        default_value: "complex".into(),
    };
    let layout = super::surfaces::surface_layout_with_viewport(
        &Surface::SettingChoices {
            setting,
            choices,
            list: ListState::selectable(1),
            custom_value: None,
        },
        Path::new("/tmp/project"),
        40,
        usize::MAX,
    );
    let line = layout
        .hits
        .iter()
        .enumerate()
        .find_map(|(index, hit)| (*hit == ListHit::Item(0)).then_some(&layout.lines[index]))
        .unwrap();

    assert_eq!(line.width(), 40);
    assert!(line.to_string().starts_with("› on_demand  "));
    assert!(line.to_string().ends_with('…'));
    assert!(line.spans[2].style.add_modifier.contains(Modifier::DIM));
}

#[test]
fn setting_input_metadata_truncates_to_the_submenu_width() {
    let setting = cagent_agent::config::SettingDefinition {
        key: "subagents.max_concurrent".into(),
        label: "Max concurrent".into(),
        description: "maximum active sub-agents; zero disables delegation".into(),
        kind: cagent_agent::config::SettingKind::NonNegativeInteger,
        section: cagent_agent::config::SettingSection::General,
        default_value: "10".into(),
    };
    let lines = surface_lines(
        &Surface::SettingInput {
            setting,
            value: "10".into(),
            cursor: 2,
        },
        Path::new("/tmp/project"),
        48,
    );
    let description = lines
        .iter()
        .find(|line| line.to_string().contains("maximum active"))
        .unwrap();

    assert_eq!(&lines[1], description);
    assert!(lines[3].to_string().starts_with("  10"));
    assert_eq!(description.width(), 48);
    assert!(description.to_string().ends_with('…'));
    assert!(
        description.spans[1]
            .style
            .add_modifier
            .contains(Modifier::DIM)
    );
}

#[test]
fn settings_tabs_are_clickable_and_reset_the_list() {
    let temporary = tempfile::tempdir().unwrap();
    let config_path = temporary.path().join("config.toml");
    std::fs::write(&config_path, "version = 1\n").unwrap();
    let rows = cagent_agent::config::ConfigStore::open(config_path)
        .unwrap()
        .settings()
        .unwrap();
    let general_count = rows
        .iter()
        .filter(|row| row.definition.section == cagent_agent::config::SettingSection::General)
        .count();
    let mut list = ListState::selectable(general_count);
    list.end(ListMode::Selectable, VISIBLE_MENU_ITEMS);
    let mut app = App::new(
        temporary.path(),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.surfaces.push(Surface::Settings {
        rows,
        section: cagent_agent::config::SettingSection::General,
        list,
        query: String::new(),
        query_cursor: 0,
    });

    let width: u16 = 100;
    let height: u16 = 30;
    let tabs = surface_lines(app.surfaces.last().unwrap(), &app.workspace, width)[2].to_string();
    let column = u16::try_from(tabs.find("[ Keybindings ]").unwrap() + 2).unwrap();
    let panel_top = height
        .saturating_sub(app.controls_height(width, height))
        .saturating_add(height.saturating_sub(1).min(2));
    app.select_mouse_at(width, height, panel_top + 2, column);

    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Settings {
            section: cagent_agent::config::SettingSection::Keybindings,
            list: ListState {
                selected: Some(0),
                offset: 0,
                ..
            },
            ..
        })
    ));
}

#[test]
fn setting_errors_hide_runtime_and_parser_wrappers() {
    assert_eq!(
        concise_setting_error(&cagent_agent::protocol::RuntimeError::InvalidOption(
            "invalid keybinding array".into(),
        )),
        "invalid keybinding array"
    );
    assert_eq!(
        concise_setting_error(&cagent_agent::protocol::RuntimeError::Config {
            path: "/tmp/config.toml".into(),
            message: "ui.composer_max_rows must be a positive integer\nparser detail".into(),
        }),
        "ui.composer_max_rows must be a positive integer"
    );
}

#[tokio::test]
async fn model_favourite_key_persists_and_reorders_filtered_rows() {
    let temporary = tempfile::tempdir().unwrap();
    let config_path = temporary.path().join("config.toml");
    std::fs::write(
        &config_path,
        "version = 1\ndefault_model = { model = 'mock/echo' }\n[providers.mock]\nenabled = true\n",
    )
    .unwrap();
    let config = cagent_agent::config::ConfigStore::open(&config_path).unwrap();
    let runtime = AgentRuntime::open(
        cagent_agent::runtime::RuntimeOptions::new(temporary.path().join("cagent.db"))
            .with_config(config),
    )
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
    let row = |id: &str, release_date: &str| ModelPickerRow {
        provider: "mock".into(),
        id: id.into(),
        label: id.into(),
        efforts: Vec::new(),
        reasoning_control: None,
        badges: Vec::new(),
        current: false,
        favourite: false,
        release_date: Some(release_date.into()),
    };
    app.surfaces.push(Surface::Models {
        rows: vec![
            row("newer-model", "2026-01-01"),
            row("older-model", "2025-01-01"),
        ],
        list: ListState {
            selected: Some(1),
            offset: 0,
            item_count: 2,
        },
        query: "model".into(),
        query_cursor: 5,
        selection_target: ModelSelectionTarget::Conversation,
    });

    app.handle_key(
        &session,
        KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE),
    )
    .await
    .unwrap();

    let Some(Surface::Models { rows, list, .. }) = app.surfaces.last() else {
        panic!("model surface should remain open");
    };
    assert_eq!(rows[0].id, "older-model");
    assert!(rows[0].favourite);
    assert_eq!(list.selected, Some(0));
    let persisted = cagent_agent::config::ConfigSnapshot::load(&config_path).unwrap();
    assert!(
        persisted
            .favourite_models()
            .contains(&cagent_agent::provider::ModelRef::parse("mock/older-model").unwrap())
    );
}

#[test]
fn startup_welcome_does_not_submit_a_prompt() {
    let app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    let text = app
        .welcome
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("Cagent"));
    assert!(!text.to_ascii_lowercase().contains("summarize"));
}

#[test]
fn slash_commands_filter_and_accept() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.insert_text("/mod");
    assert_eq!(
        app.slash_suggestions()
            .iter()
            .map(|command| command.name.clone())
            .collect::<Vec<_>>(),
        vec!["/model", "/mode"]
    );
    assert_eq!(
        app.accept_slash_command().map(|command| command.name),
        Some("/model".into())
    );
    assert_eq!(app.draft, "/model");
    app.replace_draft("/model gpt-5");
    assert_eq!(app.valid_slash_command_end(), Some("/model".len()));
    app.replace_draft("/models");
    assert!(app.slash_suggestions().is_empty());
    assert_eq!(app.valid_slash_command_end(), None);
    app.replace_draft("please /model");
    assert_eq!(app.valid_slash_command_end(), None);

    app.replace_draft("/cop");
    assert_eq!(
        app.slash_suggestions()[0].argument_hint,
        Some("[slack|discord]")
    );
    assert!(
        app.completion_lines(80)
            .iter()
            .any(|line| line.to_string().contains("/copy [slack|discord]"))
    );
    assert_eq!(
        app.accept_slash_command().map(|command| command.name),
        Some("/copy".into())
    );
    assert_eq!(app.draft, "/copy");

    app.replace_draft("/cont");
    assert_eq!(app.slash_suggestions()[0].name, "/continue");
    let context = app
        .slash_suggestions()
        .into_iter()
        .find(|command| command.name == "/context")
        .unwrap();
    assert_eq!(context.argument_hint, Some("save"));
    let selected = app
        .slash_suggestions()
        .iter()
        .position(|command| command.name == "/context")
        .unwrap();
    app.slash_list.select(selected, VISIBLE_MENU_ITEMS);
    app.accept_slash_command();
    assert_eq!(app.draft, "/context save");

    app.replace_draft("/spa");
    assert_eq!(app.slash_suggestions()[0].name, "/spawn");
    assert_eq!(app.slash_suggestions()[0].argument_hint, Some("<message>"));

    for (typed, expected) in [("/ag", "/agent"), ("/mo", "/mode")] {
        app.replace_draft(typed);
        let command = app
            .slash_suggestions()
            .into_iter()
            .find(|command| command.name == expected)
            .unwrap();
        let selected = app
            .slash_suggestions()
            .iter()
            .position(|candidate| candidate.name == command.name)
            .unwrap();
        app.slash_list.select(selected, VISIBLE_MENU_ITEMS);
        assert_eq!(
            app.accept_slash_command().map(|command| command.name),
            Some(expected.into())
        );
        assert_eq!(app.draft, expected);
    }
}

#[tokio::test]
async fn context_command_rejects_missing_or_unknown_subcommands() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = AgentRuntime::open(cagent_agent::runtime::RuntimeOptions::new(
        temporary.path().join("context-command-data"),
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

    for command in ["/context", "/context unknown"] {
        app.replace_draft(command);
        app.submit_draft(&session, QueueTarget::NextBoundary)
            .await
            .unwrap();
        assert_eq!(app.notice.as_deref(), Some("usage: /context save"));
    }
}

#[tokio::test]
async fn context_save_command_exports_and_displays_the_path() {
    let temporary = tempfile::tempdir().unwrap();
    let data_dir = temporary.path().join("context-command-data");
    let config = cagent_agent::config::ConfigSnapshot::parse(
        &temporary.path().join("config.toml"),
            "version = 1\ndefault_model = { model = 'mock/echo' }\n[providers.mock]\ntype = 'mock'\nenabled = true\n",
    )
    .unwrap();
    let runtime = AgentRuntime::open(
        cagent_agent::runtime::RuntimeOptions::new(data_dir.clone()).with_config(config),
    )
    .await
    .unwrap();
    let session = runtime
        .create_session(cagent_agent::runtime::NewSession {
            workspace: temporary.path().to_path_buf(),
        })
        .await
        .unwrap();
    session
        .submit(SessionCommand::submit_input("save me"))
        .await
        .unwrap();
    wait_for_completed_assistant_nodes(&session, 1).await;
    let mut app = App::new(
        temporary.path(),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );

    app.replace_draft("/context save");
    app.submit_draft(&session, QueueTarget::NextBoundary)
        .await
        .unwrap();

    let message = app.history.iter().find_map(|block| match &block.kind {
        cagent_agent::protocol::TranscriptBlockKind::Notice { message }
            if message.starts_with("Context saved to ") =>
        {
            Some(message)
        }
        _ => None,
    });
    let message = message.expect("saved path is shown in the transcript");
    let path = Path::new(message.trim_start_matches("Context saved to "));
    assert!(path.is_file());
    assert_eq!(path.parent(), Some(data_dir.join("contexts").as_path()));
}

#[tokio::test]
async fn enter_runs_exact_slash_commands_before_the_selected_completion() {
    let temporary = tempfile::tempdir().unwrap();
    let config = cagent_agent::config::ConfigSnapshot::parse(
        &temporary.path().join("config.toml"),
            "version = 1\ndefault_model = { model = 'mock/echo' }\n[providers.mock]\ntype = 'mock'\nenabled = true\n",
    )
    .unwrap();
    let runtime = AgentRuntime::open(
        cagent_agent::runtime::RuntimeOptions::new(temporary.path().join("exact-command.db"))
            .with_config(config),
    )
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

    app.replace_draft("/mode");
    app.handle_key(&session, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Profiles {
            kind: ProfileKind::Mode,
            ..
        })
    ));

    app.surfaces.clear();
    app.replace_draft("/mod");
    let selected = app
        .slash_suggestions()
        .iter()
        .position(|candidate| candidate.name == "/model")
        .unwrap();
    app.slash_list.select(selected, VISIBLE_MENU_ITEMS);
    app.handle_key(&session, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Models { .. } | Surface::ModelsUnavailable)
    ));

    app.surfaces.clear();
    app.replace_draft("/spawn");
    app.handle_key(&session, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.draft, "/spawn ");
    assert!(app.surfaces.is_empty());
}

#[test]
fn slash_completion_popup_is_bounded_and_scrolls_with_selection() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.replace_draft("/");

    let suggestions = app.slash_suggestions();
    assert!(suggestions.len() > VISIBLE_MENU_ITEMS);

    let first_window = app.completion_lines(80);
    assert_eq!(first_window.len(), VISIBLE_MENU_ITEMS + 2);
    assert_eq!(first_window[0].to_string(), "─".repeat(80));
    assert!(first_window[1].to_string().contains(&suggestions[0].name));

    app.slash_list
        .select(VISIBLE_MENU_ITEMS, VISIBLE_MENU_ITEMS);
    let scrolled_window = app.completion_lines(80);
    assert_eq!(scrolled_window.len(), VISIBLE_MENU_ITEMS + 2);
    assert!(scrolled_window[0].to_string().starts_with("─ ↑ ─"));
    assert_eq!(scrolled_window[0].width(), 80);
    assert!(
        scrolled_window[1]
            .to_string()
            .contains(&suggestions[2].name)
    );
    assert!(scrolled_window.iter().any(|line| {
        line.to_string()
            .contains(&suggestions[VISIBLE_MENU_ITEMS].name)
    }));
}

#[test]
fn attachment_completion_popup_merges_its_upper_scroll_indicator_with_the_separator() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    let rows = (0..VISIBLE_MENU_ITEMS + 2)
        .map(|index| cagent_agent::WorkspaceEntry {
            path: PathBuf::from(format!("path-{index}")),
            kind: cagent_agent::WorkspaceEntryKind::File,
        })
        .collect();
    app.attachment_completion = Some(AttachmentCompletion {
        start: 0,
        end: 1,
        rows,
        list: ListState::selectable(VISIBLE_MENU_ITEMS + 2),
    });

    let first_window = app.completion_lines(80);
    assert_eq!(first_window.len(), VISIBLE_MENU_ITEMS + 2);
    assert_eq!(first_window[0].to_string(), "─".repeat(80));
    assert!(first_window[1].to_string().contains("path-0"));
    assert_eq!(app.completion_layout(80).hit(0), ListHit::None);

    app.attachment_completion
        .as_mut()
        .unwrap()
        .list
        .select(VISIBLE_MENU_ITEMS, VISIBLE_MENU_ITEMS);
    let scrolled_window = app.completion_lines(80);
    assert_eq!(scrolled_window.len(), VISIBLE_MENU_ITEMS + 2);
    assert!(scrolled_window[0].to_string().starts_with("─ ↑ ─"));
    assert_eq!(app.completion_layout(80).hit(0), ListHit::Up);
}

#[tokio::test]
async fn slash_completion_double_click_runs_commands_and_prompts_for_required_arguments() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = AgentRuntime::open(cagent_agent::runtime::RuntimeOptions::new(
        temporary.path().join("slash-activation.db"),
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
    app.render_width = 80;
    app.render_height = 24;

    let click_completion = |app: &App| MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 2,
        row: app.render_height - app.controls_height(app.render_width, app.render_height) + 2,
        modifiers: KeyModifiers::NONE,
    };

    app.replace_draft("/he");
    let click = click_completion(&app);
    app.handle_terminal_event(&session, Event::Mouse(click))
        .await
        .unwrap();
    assert!(app.surfaces.is_empty());
    app.handle_terminal_event(&session, Event::Mouse(click))
        .await
        .unwrap();
    assert!(matches!(app.surfaces.last(), Some(Surface::Help { .. })));
    assert!(app.draft.is_empty());

    app.surfaces.clear();
    app.replace_draft("/spa");
    let click = click_completion(&app);
    app.handle_terminal_event(&session, Event::Mouse(click))
        .await
        .unwrap();
    assert_eq!(app.draft, "/spa");
    app.handle_terminal_event(&session, Event::Mouse(click))
        .await
        .unwrap();
    assert_eq!(app.draft, "/spawn ");
    assert!(app.surfaces.is_empty());
}

#[test]
fn clicking_completion_scroll_arrows_moves_one_logical_page() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.replace_draft("/");
    assert!(app.slash_suggestions().len() > VISIBLE_MENU_ITEMS);

    let width = 80;
    let height = 24;
    let arrow_row = |app: &App, arrow: &str| {
        let controls_height = app.controls_height(width, height);
        let (queue_rows, popup_rows, _, status_height) = app.control_heights_within(width, height);
        let sticky_rows = height.saturating_sub(status_height).min(2);
        let popup_start = sticky_rows.min(1) + queue_rows;
        let popup = app.completion_lines(width);
        assert_eq!(usize::from(popup_rows), popup.len());
        let index = popup
            .iter()
            .position(|line| line.to_string().contains(arrow))
            .unwrap();
        height - controls_height + popup_start + u16::try_from(index).unwrap()
    };

    let down = arrow_row(&app, "↓");
    app.select_mouse_at(width, height, down, 2);
    assert_eq!(app.slash_list.selected, Some(VISIBLE_MENU_ITEMS));

    let up = arrow_row(&app, "↑");
    app.select_mouse_at(width, height, up, width - 1);
    assert_eq!(app.slash_list.selected, Some(0));
}

#[test]
fn slash_completion_truncates_long_rows_with_an_ellipsis() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.skill_commands.push((
        "very-long-skill-command".into(),
        "a description that cannot fit in a narrow completion popup".into(),
    ));
    app.replace_draft("/very");

    let lines = app.completion_lines(24);
    let row = lines
        .iter()
        .find(|line| line.to_string().contains("very-long"))
        .expect("skill command row");
    assert!(row.width() <= 24);
    assert!(row.to_string().ends_with('…'));
}

#[test]
fn slash_completion_shows_each_skill_command_once() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.skill_commands.extend([
        ("review".into(), "project review skill".into()),
        ("review".into(), "global review skill".into()),
    ]);
    app.replace_draft("/rev");

    let suggestions = app.slash_suggestions();

    assert_eq!(
        suggestions
            .iter()
            .filter(|suggestion| suggestion.name == "/review")
            .count(),
        1
    );
    assert_eq!(suggestions[0].description, "project review skill");
}

#[test]
fn agent_menu_rows_only_show_name_and_description() {
    let row = agent_menu_row(cagent_agent::config::AgentProfile {
        name: "review".into(),
        description: "Review changes".into(),
        prompt: String::new(),
        model: Some(cagent_agent::config::ModelSelection {
            target: Some(cagent_agent::config::ModelTarget::Model(
                cagent_agent::provider::ModelRef::parse("openai/hidden-model").unwrap(),
            )),
            effort: None,
        }),
        mode_overrides: std::collections::BTreeMap::new(),
        mode: Some("plan".into()),
        availability: cagent_agent::config::AgentAvailability::User,
        enabled: true,
        tools_allow: None,
        tools_deny: Some(["bash".into()].into_iter().collect()),
        mcp_allow: None,
        mcp_deny: None,
    });

    assert_eq!(row, ("review".into(), "Review changes".into()));
}

#[test]
fn agent_editor_uses_shared_lists_and_keeps_a_line_below_inputs() {
    let wizard = |step, editing, prompt: &str, parents: Vec<String>| {
        let parent_list = ListState::selectable(parents.len());
        Surface::AgentWizard {
            name: "review".into(),
            description: "Review changes".into(),
            parent: String::new(),
            parents,
            parent_list,
            prompt: MultilineInput::new(prompt.into(), 0),
            availability: 0,
            step,
            cursor: 0,
            editing,
            original_name: editing.then(|| "review".into()),
        }
    };

    for editing in [false, true] {
        for (step, prompt) in [
            (AgentWizardStep::Name, ""),
            (AgentWizardStep::Description, ""),
            (AgentWizardStep::Prompt, ""),
            (AgentWizardStep::Prompt, "Review carefully"),
        ] {
            let lines = surface_lines(
                &wizard(step, editing, prompt, vec!["None".into()]),
                Path::new("/tmp/project"),
                80,
            );
            assert_eq!(lines.last().map(ToString::to_string), Some(String::new()));
        }
    }

    let edit = Surface::AgentEdit {
        name: "review".into(),
        list: ListState::selectable(5),
    };
    let edit_layout = super::surfaces::surface_layout_with_viewport(
        &edit,
        Path::new("/tmp/project"),
        80,
        usize::MAX,
    );
    assert_eq!(edit_layout.lines.len(), 8);
    assert_eq!(edit_layout.hits[1], ListHit::None);
    assert_eq!(edit_layout.hits[2], ListHit::Item(0));
    assert_eq!(edit_layout.hits[6], ListHit::Item(4));
    assert_eq!(edit_layout.hits[7], ListHit::None);

    let parents = (0..10).map(|index| format!("parent-{index}")).collect();
    let parent_layout = super::surfaces::surface_layout_with_viewport(
        &wizard(AgentWizardStep::Parent, false, "", parents),
        Path::new("/tmp/project"),
        80,
        usize::MAX,
    );
    assert_eq!(parent_layout.hits[1], ListHit::None);
    assert_eq!(parent_layout.hits[2], ListHit::Item(0));
    assert_eq!(parent_layout.hits.last(), Some(&ListHit::Down));

    let wrapped_parent_layout = super::surfaces::surface_layout_with_viewport(
        &wizard(
            AgentWizardStep::Parent,
            false,
            "",
            vec!["agent-parent-with-a-long-name".into()],
        ),
        Path::new("/tmp/project"),
        12,
        usize::MAX,
    );
    assert!(
        wrapped_parent_layout
            .lines
            .iter()
            .all(|line| line.width() <= 12)
    );
    let wrapped_parent_rows = wrapped_parent_layout
        .hits
        .iter()
        .filter(|hit| **hit == ListHit::Item(0))
        .count();
    assert!(wrapped_parent_rows > 1);

    let truncated_parent_layout = super::surfaces::surface_layout_with_viewport(
        &wizard(
            AgentWizardStep::Parent,
            true,
            "",
            vec!["agent-parent-with-a-long-name".into()],
        ),
        Path::new("/tmp/project"),
        12,
        usize::MAX,
    );
    assert_eq!(
        truncated_parent_layout
            .hits
            .iter()
            .filter(|hit| **hit == ListHit::Item(0))
            .count(),
        1
    );
    assert!(
        truncated_parent_layout
            .lines
            .iter()
            .zip(&truncated_parent_layout.hits)
            .find_map(|(line, hit)| (*hit == ListHit::Item(0)).then_some(line))
            .is_some_and(|line| line.to_string().ends_with('…'))
    );

    let availability_lines = surface_lines(
        &wizard(
            AgentWizardStep::Availability,
            false,
            "",
            vec!["None".into()],
        ),
        Path::new("/tmp/project"),
        80,
    );
    let narrow_prompt = surface_lines_with_viewport(
        &wizard(
            AgentWizardStep::Prompt,
            false,
            "These instructions are intentionally much wider than the agent editor",
            vec!["None".into()],
        ),
        Path::new("/tmp/project"),
        16,
        8,
    );
    assert!(narrow_prompt.len() <= 8);
    assert!(narrow_prompt.iter().all(|line| line.width() <= 16));

    let newline_prompt = surface_lines_with_viewport(
        &wizard(
            AgentWizardStep::Prompt,
            false,
            "aaaaa\n",
            vec!["None".into()],
        ),
        Path::new("/tmp/project"),
        80,
        5,
    );
    assert_eq!(newline_prompt.len(), 5);
    assert!(
        newline_prompt
            .iter()
            .any(|line| line.to_string().contains("aaaaa"))
    );

    let narrow_edit = surface_lines(
        &Surface::AgentEdit {
            name: "agent-with-an-extremely-long-name".into(),
            list: ListState::selectable(5),
        },
        Path::new("/tmp/project"),
        16,
    );
    assert!(narrow_edit.iter().all(|line| line.width() <= 16));
    assert_eq!(
        availability_lines.last().map(ToString::to_string),
        Some(String::new())
    );
}

#[tokio::test]
async fn existing_agent_text_fields_open_with_cursor_at_the_end() {
    let temporary = tempfile::tempdir().unwrap();
    let config_path = temporary.path().join("config.toml");
    std::fs::write(
        &config_path,
        "version = 1\ndefault_model = { model = 'mock/echo' }\n[providers.mock]\ntype = 'mock'\nenabled = true\n[agents.review]\ndescription = 'Review code'\nprompt = 'Review prompt'\n",
    )
    .unwrap();
    let config = cagent_agent::config::ConfigStore::open(&config_path).unwrap();
    let runtime = AgentRuntime::open(
        cagent_agent::runtime::RuntimeOptions::new(temporary.path().join("agent-editor-cursor.db"))
            .with_config(config),
    )
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
    let name = "review";
    for (selected, expected_step, expected) in [
        (0, AgentWizardStep::Name, name),
        (1, AgentWizardStep::Description, "Review code"),
        (3, AgentWizardStep::Prompt, "Review prompt"),
    ] {
        app.surfaces.clear();
        app.surfaces.push(Surface::AgentEdit {
            name: name.into(),
            list: ListState::selectable_at(5, selected, VISIBLE_MENU_ITEMS),
        });

        app.handle_surface_key(&session, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();

        let Some(Surface::AgentWizard {
            cursor,
            step,
            prompt,
            ..
        }) = app.surfaces.last()
        else {
            panic!("expected agent wizard");
        };
        assert_eq!(*step, expected_step);
        if *step == AgentWizardStep::Prompt {
            assert_eq!(prompt.cursor(), expected.len());
        } else {
            assert_eq!(*cursor, expected.len());
        }
    }
}

#[tokio::test]
async fn agent_wizard_requires_a_name_before_advancing() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = AgentRuntime::open(cagent_agent::runtime::RuntimeOptions::new(
        temporary.path().join("agent-name-required.db"),
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
    app.surfaces.push(Surface::AgentWizard {
        name: "   ".into(),
        description: String::new(),
        parent: String::new(),
        parents: vec!["None".into()],
        parent_list: ListState::selectable(1),
        prompt: MultilineInput::new(String::new(), 0),
        availability: 0,
        step: AgentWizardStep::Name,
        cursor: 3,
        editing: false,
        original_name: None,
    });

    app.handle_surface_key(&session, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    assert_eq!(app.notice.as_deref(), Some("agent name required"));
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::AgentWizard {
            name,
            step: AgentWizardStep::Name,
            cursor: 3,
            ..
        }) if name == "   "
    ));
}

#[test]
fn agent_and_mode_pickers_use_the_bounded_list_contract() {
    let rows = (0..12)
        .map(|index| {
            (
                format!("profile-{index}"),
                format!("description {index} that is deliberately long enough to truncate"),
                true,
            )
        })
        .collect::<Vec<_>>();
    let width: u16 = 40;
    let height: u16 = 24;

    for kind in [ProfileKind::Agent, ProfileKind::Mode] {
        let mut app = App::new(
            Path::new("/tmp/project"),
            ("mock".into(), "echo".into(), None),
            ["mock".into()].into_iter().collect(),
            "ask",
            None,
        );
        app.render_width = width;
        app.render_height = height;
        app.surfaces.push(Surface::Profiles {
            kind,
            rows: rows.clone(),
            list: ListState::selectable(rows.len()),
        });

        let layout = super::surfaces::surface_layout_with_viewport(
            app.surfaces.last().unwrap(),
            &app.workspace,
            width,
            usize::MAX,
        );
        assert_eq!(layout.lines.len(), VISIBLE_MENU_ITEMS + 3);
        assert_eq!(layout.hits[1], ListHit::None);
        assert_eq!(layout.hits[2], ListHit::Item(0));
        assert_eq!(layout.hits.last(), Some(&ListHit::Down));
        assert!(layout.lines.iter().all(|line| line.width() <= 40));
        assert!(layout.lines[2].to_string().ends_with('…'));

        let controls_height = app.controls_height(width, height);
        let panel_top = height
            .saturating_sub(controls_height)
            .saturating_add(height.saturating_sub(1).min(2));
        let down_row = panel_top
            + u16::try_from(
                layout
                    .hits
                    .iter()
                    .position(|hit| *hit == ListHit::Down)
                    .unwrap(),
            )
            .unwrap();
        app.select_mouse_at(width, height, down_row, 2);
        assert!(matches!(
            app.surfaces.last(),
            Some(Surface::Profiles { list, .. })
                if list.selected == Some(VISIBLE_MENU_ITEMS)
        ));

        assert!(app.scroll_expanded_mouse(MouseEventKind::ScrollDown));
        assert!(matches!(
            app.surfaces.last(),
            Some(Surface::Profiles { list, .. }) if list.selected == Some(11)
        ));
    }
}

#[test]
fn copy_arguments_select_only_supported_dialects() {
    assert_eq!(copy_dialect(None), Ok(None));
    assert_eq!(
        copy_dialect(Some("slack")),
        Ok(Some(cagent_agent::presentation::MarkdownDialect::Slack))
    );
    assert_eq!(
        copy_dialect(Some("discord")),
        Ok(Some(cagent_agent::presentation::MarkdownDialect::Discord))
    );
    assert_eq!(
        copy_dialect(Some("teams")),
        Err("usage: /copy [slack|discord]")
    );
    assert!(copy_dialect(Some("slack extra")).is_err());
    assert!(copy_dialect(Some("Slack")).is_err());
}

#[test]
fn slash_completion_prioritizes_recently_executed_commands() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.record_executed_command(
        SLASH_COMMANDS
            .iter()
            .find(|command| command.name == "/copy")
            .copied()
            .expect("copy command exists"),
        "/copy",
    );
    app.insert_text("/");

    assert_eq!(app.slash_suggestions()[0].name, "/copy");

    app.record_executed_command(
        SLASH_COMMANDS
            .iter()
            .find(|command| command.name == "/help")
            .copied()
            .expect("help command exists"),
        "/help",
    );
    app.replace_draft("/");
    assert_eq!(app.slash_suggestions()[0].name, "/help");
}

#[test]
fn cleanup_command_is_exposed_only_for_manual_cleanup_policy() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.insert_text("/clean");
    assert!(app.slash_suggestions().is_empty());
    assert!(
        !configured_help_command_rows(false)
            .iter()
            .any(|(name, _)| name == "/cleanup")
    );

    app.manual_cleanup_available = true;
    assert_eq!(app.slash_suggestions()[0].name, "/cleanup");
    assert!(!app.has_exact_runnable_slash_command());
    app.replace_draft("/cleanup");
    assert!(app.has_exact_runnable_slash_command());
    assert!(
        configured_help_command_rows(true)
            .iter()
            .any(|(name, _)| name == "/cleanup")
    );
}

#[test]
fn executed_slash_commands_are_available_in_prompt_history() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    let command = SLASH_COMMANDS
        .iter()
        .find(|command| command.name == "/model")
        .copied()
        .expect("model command exists");
    app.record_executed_command(command, "/model gpt-5");

    app.recall_previous_history();

    assert_eq!(app.draft, "/model gpt-5");
    assert!(app.slash_suggestions().is_empty());

    app.recall_next_history();
    assert!(app.draft.is_empty());
    app.insert_text("/");
    assert!(!app.slash_suggestions().is_empty());
}

#[test]
fn help_tabs_wrap_and_show_the_active_category() {
    assert_eq!(HelpTab::default(), HelpTab::Commands);
    assert_eq!(HelpTab::default().previous(), HelpTab::Keybindings);
    assert_eq!(HelpTab::default().next(), HelpTab::Keybindings);

    let commands = surface_lines(
        &Surface::Help {
            tab: HelpTab::Commands,
            command_rows: help_command_rows(),
            key_rows: cagent_agent::presentation::ResolvedKeyBindings::default().help_rows(),
            list: ScrollViewState::new(usize::MAX),
        },
        Path::new("/tmp/project"),
        80,
    );
    let keybindings = surface_lines(
        &Surface::Help {
            tab: HelpTab::Keybindings,
            command_rows: help_command_rows(),
            key_rows: cagent_agent::presentation::ResolvedKeyBindings::default().help_rows(),
            list: ScrollViewState::new(usize::MAX),
        },
        Path::new("/tmp/project"),
        80,
    );
    let commands = commands
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    let keybindings = keybindings
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    assert!(commands.contains("/providers"));
    assert!(commands.contains("/mode [name] [message]"));
    assert!(commands.contains("/<mode> [message]"));
    assert!(!commands.contains('│'));
    assert!(!commands.contains("Alt+P"));
    assert!(keybindings.contains("Alt+P"));
    assert!(keybindings.contains("Alt+T"));
    assert!(keybindings.contains("Alt+F"));
    assert!(!keybindings.contains('│'));
    assert!(!keybindings.contains("/providers"));
    let model = surface_lines(
        &Surface::Help {
            tab: HelpTab::Commands,
            command_rows: help_command_rows(),
            key_rows: Vec::new(),
            list: ScrollViewState::new(usize::MAX),
        },
        Path::new("/tmp/project"),
        80,
    )
    .into_iter()
    .find(|line| line.to_string().contains("/model [query]"))
    .expect("model help row");
    assert_eq!(model.spans[1].content, "/model [query]");
    assert_eq!(model.spans[1].style, ACCENT_STYLE);
    assert_eq!(
        surface_status(&Surface::Help {
            tab: HelpTab::Commands,
            command_rows: help_command_rows(),
            key_rows: Vec::new(),
            list: ScrollViewState::new(usize::MAX),
        }),
        "↑/↓ scroll · ←/→ tabs · Esc close"
    );
}

#[test]
fn help_tabs_are_clickable_without_an_extra_spacer() {
    let command_rows = help_command_rows();
    let key_rows = cagent_agent::presentation::ResolvedKeyBindings::default().help_rows();
    let mut list = ScrollViewState::new(command_rows.len());
    list.apply(ScrollViewAction::End, VISIBLE_HELP_ITEMS);
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.surfaces.push(Surface::Help {
        tab: HelpTab::Commands,
        command_rows,
        key_rows,
        list,
    });

    let width: u16 = 80;
    let height: u16 = 30;
    let lines = surface_lines(app.surfaces.last().unwrap(), &app.workspace, width);
    assert!(lines[2].to_string().contains("[ Commands ]"));
    assert_eq!(lines[3].to_string().trim(), "↑");
    assert!(!lines[4].to_string().is_empty());
    let column = u16::try_from(lines[2].to_string().find("[ Keybindings ]").unwrap() + 2).unwrap();
    let panel_top = height
        .saturating_sub(app.controls_height(width, height))
        .saturating_add(height.saturating_sub(1).min(2));
    app.select_mouse_at(width, height, panel_top + 2, column);

    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Help {
            tab: HelpTab::Keybindings,
            list: ScrollViewState { offset: 0, .. },
            ..
        })
    ));
}

#[test]
fn help_command_rows_cover_static_commands_and_scroll_both_tabs() {
    let rows = help_command_rows();
    for command in SLASH_COMMANDS {
        assert!(
            rows.iter().any(|(name, _)| name.starts_with(command.name)),
            "missing {} from help",
            command.name
        );
    }
    assert_eq!(
        rows.iter()
            .filter(|(name, _)| name.starts_with("/<mode>"))
            .count(),
        1
    );

    let scrolled_commands = surface_lines(
        &Surface::Help {
            tab: HelpTab::Commands,
            command_rows: rows,
            key_rows: Vec::new(),
            list: ScrollViewState {
                offset: usize::MAX,
                content_rows: usize::MAX,
            },
        },
        Path::new("/tmp/project"),
        80,
    );
    assert!(
        scrolled_commands
            .iter()
            .map(ToString::to_string)
            .any(|line| line.contains("/quit"))
    );

    let key_rows = cagent_agent::presentation::ResolvedKeyBindings::default().help_rows();
    let scrolled_keys = surface_lines(
        &Surface::Help {
            tab: HelpTab::Keybindings,
            command_rows: Vec::new(),
            key_rows: key_rows.clone(),
            list: ScrollViewState {
                offset: usize::MAX,
                content_rows: usize::MAX,
            },
        },
        Path::new("/tmp/project"),
        80,
    );
    assert!(scrolled_keys.len() <= VISIBLE_HELP_ITEMS + 6);
    assert!(
        scrolled_keys
            .iter()
            .all(|line| !line.to_string().contains('│'))
    );
    assert_ne!(
        scrolled_keys.last().map(ToString::to_string),
        surface_lines(
            &Surface::Help {
                tab: HelpTab::Keybindings,
                command_rows: Vec::new(),
                key_rows,
                list: ScrollViewState::new(usize::MAX),
            },
            Path::new("/tmp/project"),
            80,
        )
        .last()
        .map(ToString::to_string)
    );
}

#[test]
fn worktree_command_is_registered_with_optional_arguments() {
    let command = SLASH_COMMANDS
        .iter()
        .find(|command| command.name == "/worktree")
        .expect("worktree command exists");
    assert_eq!(command.argument_hint, Some("[worktree] [base]"));
    assert!(!command.observer_safe);
}

#[test]
fn worktree_picker_puts_new_first_and_marks_current_root() {
    let lines = surface_lines(
        &Surface::Worktrees {
            rows: vec![cagent_agent::WorktreeInfo {
                name: "Root".into(),
                path: Path::new("/tmp/project").into(),
                root: true,
                current: true,
            }],
            list: ListState::selectable_at(2, 1, VISIBLE_MENU_ITEMS),
        },
        Path::new("/tmp/project"),
        80,
    );
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.find("New")
            .is_some_and(|new| text.find("Root").is_some_and(|root| new < root))
    );
    assert!(text.contains("Root  current"));
}

#[test]
fn configured_multi_bindings_and_unbound_actions_drive_help_and_hints() {
    let config = cagent_agent::config::ConfigSnapshot::parse(
        Path::new("config.toml"),
        "version = 1\n[keys]\nmodel_picker = [\"alt+x\", \"ctrl+p\"]\ncomplete = []\n",
    )
    .unwrap();
    let (bindings, warnings) =
        cagent_agent::presentation::ResolvedKeyBindings::from_config(&config);
    assert!(warnings.is_empty());
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.key_bindings = bindings;

    assert!(app.matches_action(
        cagent_agent::presentation::KeyBindingAction::ModelPicker,
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT),
    ));
    assert!(app.matches_action(
        cagent_agent::presentation::KeyBindingAction::ModelPicker,
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
    ));
    let rows = app.key_bindings.help_rows();
    assert!(rows.iter().any(|(label, _)| label == "Alt+X / Ctrl+P"));
    assert!(rows.iter().any(|(label, _)| label == "Unbound"));
    assert_eq!(
        app.menu_navigation_code(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
        KeyCode::Null
    );
    assert_eq!(
        app.contextual_key_hints("↑/↓ navigate · Tab complete · Enter run · Esc close"),
        "↑/↓ navigate · Enter run · Esc close"
    );
}

#[test]
fn clicking_combined_close_hint_uses_close_surface_action() {
    use cagent_agent::presentation::KeyBindingAction;

    assert_eq!(
        status_hint_action("Enter/Esc close"),
        Some(KeyBindingAction::CloseSurface)
    );
    assert_eq!(
        status_hint_action("Enter/Esc continue"),
        Some(KeyBindingAction::Submit)
    );
}

#[test]
fn expanded_log_follows_new_output_only_when_already_at_bottom() {
    assert_eq!(follow_expanded_scroll(12, 12, 18), 18);
    assert_eq!(follow_expanded_scroll(5, 12, 18), 5);
}

#[test]
fn expanded_subagent_log_copies_live_usage_into_the_header() {
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
    let mut usage = cagent_agent::provider::ModelUsage::default();
    usage.input_tokens = Some(1_000);
    usage.output_tokens = Some(250);
    app.supervised_work
        .rows
        .push(cagent_agent::presentation::SupervisedWork::Agent {
            run: Box::new(run.clone()),
        });
    app.supervised_work.agent_run_live.insert(
        run.id,
        cagent_agent::presentation::DelegatedRunLive {
            id: run.id,
            text: String::new(),
            usage: Some(usage),
        },
    );
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

    app.refresh_open_agent_log_surfaces();

    assert!(
        surface_lines_with_viewport(
            app.surfaces.last().unwrap(),
            Path::new("/tmp/project"),
            80,
            10,
        )
        .iter()
        .any(|line| line.to_string().contains("Usage in:1k out:250"))
    );
}

#[test]
fn expanded_subagent_log_shows_time_after_status() {
    let mut usage = cagent_agent::provider::ModelUsage::default();
    usage.input_tokens = Some(1_000);
    usage.output_tokens = Some(250);
    usage.cost = Some(cagent_agent::provider::ModelCost {
        total_cost: Some("0.012".into()),
        currency: "USD".into(),
        pricing_source: "provider_reported".into(),
        pricing_version: "response".into(),
        ..cagent_agent::provider::ModelCost::default()
    });
    let run = cagent_agent::protocol::AgentRun {
        id: cagent_agent::protocol::AgentRunId::new(),
        conversation_id: cagent_agent::protocol::ConversationId::new(),
        parent_turn_id: cagent_agent::protocol::TurnId::new(),
        sequence: 0,
        profile: "explore".into(),
        model: cagent_agent::provider::ModelRef::parse("mock/echo").unwrap(),
        effort: None,
        task: "Inspect".into(),
        status: cagent_agent::protocol::AgentRunStatus::Completed,
        result: None,
        error: None,
        usage: Some(usage),
        created_at: "500".into(),
        started_at: Some("1000".into()),
        completed_at: Some("62000".into()),
        timeline: Vec::new(),
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
        viewport_rows: 10,
    };

    let lines = surface_lines_with_viewport(&surface, Path::new("/tmp/project"), 80, 10);
    assert_eq!(
        lines[0].to_string(),
        "  Sub-agent log  explore · mock/echo · completed 1m 01s"
    );
    assert_eq!(lines[1].to_string(), "  Usage in:1k out:250 · Cost  $0.012");
    assert!(lines[2].to_string().is_empty());
    assert!(
        lines
            .iter()
            .any(|line| { line.to_string() == "  Usage in:1k out:250 · Cost  $0.012" })
    );
    assert!(!lines.iter().any(|line| line.to_string().contains("Time  ")));
}

#[test]
fn open_subagent_log_refreshes_with_live_terminal_output() {
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
        task: "Run a command".into(),
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
    let terminal = cagent_agent::tools::TerminalSnapshot {
        id: cagent_agent::tools::TerminalId::new(),
        owner: run.conversation_id,
        owner_agent_run_id: Some(run.id),
        tool_call_node_id: None,
        read_safe: None,
        command: "echo live".into(),
        status: cagent_agent::tools::TerminalStatus::Running,
        created_at: "1".into(),
        started_at: "1".into(),
        completed_at: None,
        exit_code: None,
        output_base: 0,
        output_cursor: 5,
        output_bytes: 5,
        discarded_bytes: 0,
        truncated: false,
        ansi_output: "live\n".into(),
        output: "live\n".into(),
    };
    app.supervised_work
        .rows
        .push(cagent_agent::presentation::SupervisedWork::Agent {
            run: Box::new(run.clone()),
        });
    app.session_id = terminal.owner;
    app.surfaces.push(Surface::Expanded {
        view: ExpandedView::AgentLog {
            run: Box::new(run.clone()),
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

    app.apply_delegated_terminal_update(&terminal);
    app.refresh_open_agent_log_surfaces();

    assert!(app.bash_activity_animation_visible());

    // A snapshot may briefly omit live terminal data. The local terminal
    // state must survive that rehydration and continue feeding the open log.
    let mut snapshot = session_snapshot_with_interaction(None);
    snapshot.conversation_id = terminal.owner;
    snapshot.supervised_work = vec![cagent_agent::presentation::SupervisedWork::Agent {
        run: Box::new(run.clone()),
    }];
    snapshot.agent_runs = vec![run];
    app.apply_session_snapshot(&snapshot);

    let rendered = surface_lines_with_viewport(
        app.surfaces.last().unwrap(),
        Path::new("/tmp/project"),
        80,
        10,
    )
    .into_iter()
    .map(|line| line.to_string())
    .collect::<Vec<_>>();
    assert!(rendered.iter().any(|line| line.contains("live")));
}

#[test]
fn hydrated_subagent_log_uses_snapshot_terminals_for_running_and_completed_bash() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.render_width = 100;
    let run_id = cagent_agent::protocol::AgentRunId::new();
    let conversation_id = cagent_agent::protocol::ConversationId::new();
    let make_terminal =
        |command: &str,
         status: cagent_agent::tools::TerminalStatus,
         output_cursor: u64,
         output: &str| cagent_agent::tools::TerminalSnapshot {
            id: cagent_agent::tools::TerminalId::new(),
            owner: conversation_id,
            owner_agent_run_id: Some(run_id),
            tool_call_node_id: None,
            read_safe: None,
            command: command.into(),
            status,
            created_at: output_cursor.to_string(),
            started_at: output_cursor.to_string(),
            completed_at: status.is_final().then(|| output_cursor.to_string()),
            exit_code: status.is_final().then_some(0),
            output_base: 0,
            output_cursor,
            output_bytes: output_cursor,
            discarded_bytes: 0,
            truncated: false,
            ansi_output: output.into(),
            output: output.into(),
        };
    let running = make_terminal(
        "watch build",
        cagent_agent::tools::TerminalStatus::Running,
        5,
        "still running\n",
    );
    let completed = make_terminal(
        "cargo test",
        cagent_agent::tools::TerminalStatus::Exited,
        12,
        "tests passed\n",
    );
    let run = cagent_agent::protocol::AgentRun {
        id: run_id,
        conversation_id,
        parent_turn_id: cagent_agent::protocol::TurnId::new(),
        sequence: 0,
        profile: "explore".into(),
        model: cagent_agent::provider::ModelRef::parse("mock/echo").unwrap(),
        effort: None,
        task: "Run delegated commands".into(),
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
    let mut snapshot = session_snapshot_with_interaction(None);
    snapshot.conversation_id = conversation_id;
    snapshot.terminals = vec![running.clone(), completed.clone()];
    // Deliberately omit terminal rows: the top-level terminal snapshots are
    // the hydration source for the expanded log in this case.
    snapshot.supervised_work = vec![cagent_agent::presentation::SupervisedWork::Agent {
        run: Box::new(run.clone()),
    }];
    snapshot.agent_runs = vec![run.clone()];
    app.apply_session_snapshot(&snapshot);

    let terminals = app.supervised_work.terminals_for_agent(&run);
    assert_eq!(terminals.len(), 2);
    app.surfaces.push(Surface::Expanded {
        view: ExpandedView::AgentLog {
            run: Box::new(run),
            streaming: String::new(),
            terminals,
            expanded_explorations: Default::default(),
            expanded_activity_runs: Default::default(),
            collapse_tool_activity: false,
            max_scroll: Cell::new(None),
        },
        scroll: 0,
        viewport_rows: 20,
    });
    let rendered = surface_lines_with_viewport(
        app.surfaces.last().unwrap(),
        Path::new("/tmp/project"),
        100,
        20,
    )
    .into_iter()
    .map(|line| line.to_string())
    .collect::<Vec<_>>();
    assert!(
        rendered
            .iter()
            .any(|line| line.contains("Running watch build"))
    );
    assert!(rendered.iter().any(|line| line.contains("Ran cargo test")));
}

#[test]
fn nested_subagent_terminal_output_is_not_stored_in_live_run_state() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.render_width = 80;
    app.render_height = 24;
    let run_id = cagent_agent::protocol::AgentRunId::new();
    app.supervised_work.agent_run_live.insert(
        run_id,
        cagent_agent::presentation::DelegatedRunLive {
            id: run_id,
            text: String::new(),
            usage: None,
        },
    );
    app.surfaces.push(Surface::Expanded {
        view: ExpandedView::Terminal {
            terminal_id: None,
            command: "watch build".into(),
            output: String::new(),
            ansi_output: String::new(),
            completion: None,
            started_at: None,
            completed_at: None,
        },
        scroll: 0,
        viewport_rows: 20,
    });

    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Expanded {
            view: ExpandedView::Terminal { .. },
            ..
        })
    ));
}

#[test]
fn transient_notices_are_plain_and_neutral() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.set_notice(EXIT_NOTICE);

    let status = app.status_line(80);

    assert_eq!(status.to_string(), "  press Ctrl+C again to exit  ");
    assert_eq!(status.style, DIM_STYLE);

    app.set_notice("model not found · gpt-5");
    let status = app.status_line(80);
    assert_eq!(status.to_string(), "  model not found · gpt-5  ");
    assert_eq!(status.style, DIM_STYLE);

    app.surfaces.push(Surface::Help {
        tab: HelpTab::Commands,
        command_rows: help_command_rows(),
        key_rows: app.key_bindings.help_rows(),
        list: ScrollViewState::new(usize::MAX),
    });
    app.set_notice("copied ChatGPT subscription authentication URL");
    let (_, _, _, status) = app.control_lines_with_draft_limit(80, usize::MAX);
    assert_eq!(
        status.to_string(),
        "  copied ChatGPT subscription authentication URL  "
    );
}

#[test]
fn opening_a_menu_keeps_the_working_indicator_visible() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.active = true;
    app.surfaces.push(Surface::Help {
        tab: HelpTab::Commands,
        command_rows: help_command_rows(),
        key_rows: app.key_bindings.help_rows(),
        list: ScrollViewState::new(usize::MAX),
    });

    assert!(app.working_indicator_visible());
}

#[test]
fn permission_prompt_keeps_the_waiting_indicator_visible() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.active = true;
    let request = permission_request(None);
    app.pending_interaction = Some(request.clone());
    app.surfaces.push(Surface::Permission {
        request: Box::new(request),
        selected: 0,
        scope: cagent_agent::permissions::PermissionScope::Project,
        diff_scroll: 0,
        denial_note: String::new(),
        editing_note: false,
        note_cursor: 0,
    });

    assert!(app.working_indicator_visible());
}

#[test]
fn status_line_orders_mode_agent_model_and_provider() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        (
            "openrouter".into(),
            "openai/gpt-example".into(),
            Some("high".into()),
        ),
        ["openrouter".into()].into_iter().collect(),
        "ask",
        None,
    );

    let status = app.status_line(120).to_string();

    assert!(status.starts_with("  ask · openai/gpt-example high · openrouter"));
    assert!(!status.contains("mode:"));
    assert!(!status.contains("model:"));
    assert!(!status.contains("provider:"));
    assert!(!status.contains("agent:general"));
    assert!(!status.contains("openrouter/openai/gpt-example"));

    app.agent = "review".into();
    let status = app.status_line(120).to_string();
    assert!(
        status.starts_with("  ask · review · openai/gpt-example high · openrouter"),
        "unexpected status line: {status:?}"
    );

    app.effort = None;
    let status = app.status_line(120).to_string();
    assert!(status.starts_with("  ask · review · openai/gpt-example · openrouter"));
    assert!(!status.contains("default"));

    app.set_selection(None);
    let status = app.status_line(120).to_string();
    assert!(status.starts_with("  ask · review · none · none"));
    assert!(!status.contains("default"));
}

#[test]
fn status_line_uses_configured_order_and_module_colors() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.status_line_config.modules = vec![StatusLineModule::Model, StatusLineModule::Mode];
    app.status_line_config
        .colors
        .insert(StatusLineModule::Model, StatusLineColor::Rgb(1, 2, 3));
    app.mode_colors
        .insert("ask".into(), StatusLineColor::Rgb(4, 5, 6));

    let status = app.status_line(80);

    assert_eq!(status.to_string(), "  echo · ask  ");
    assert_eq!(
        status
            .spans
            .iter()
            .find(|span| span.content == "echo")
            .and_then(|span| span.style.fg),
        Some(Color::Rgb(1, 2, 3))
    );
    assert_eq!(
        status
            .spans
            .iter()
            .find(|span| span.content == "ask")
            .and_then(|span| span.style.fg),
        Some(Color::Rgb(4, 5, 6))
    );
}

#[test]
fn status_line_shows_the_dark_gray_composer_hint_for_the_conversation_state() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.status_line_config.modules = vec![StatusLineModule::Hint];

    assert_eq!(app.status_line(80).to_string(), "    ");

    app.supervised_work
        .rows
        .push(cagent_agent::presentation::SupervisedWork::Terminal {
            terminal: Box::new(cagent_agent::tools::TerminalSnapshot {
                id: cagent_agent::tools::TerminalId::new(),
                owner: cagent_agent::protocol::ConversationId::new(),
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
    assert!(
        app.status_line(80)
            .to_string()
            .contains("Alt+↓ 1 background")
    );
    app.supervised_work.rows.clear();

    app.insert_text("guide me");
    let status = app.status_line(80);
    let hint = status
        .spans
        .iter()
        .find(|span| span.content == "Enter to send, Ctrl+C to clear")
        .expect("idle hint should be rendered for a nonempty draft");
    assert_eq!(hint.style.fg, Some(Color::DarkGray));
    assert!(!hint.style.add_modifier.contains(Modifier::BOLD));

    app.active = true;
    assert!(
        app.status_line(80)
            .to_string()
            .contains("Tab to queue, Enter to steer")
    );

    app.queued.push(QueuedMessage {
        id: cagent_agent::protocol::QueuedMessageId::default(),
        position: 0,
        target: QueueTarget::NextBoundary,
        kind: cagent_agent::protocol::QueuedItemKind::Prompt,
        mode: None,
        command_text: None,
        text: "follow up".into(),
        attachments: Vec::new(),
        images: Vec::new(),
        image_chips: Vec::new(),
        blocked_by_startup: false,
        require_subagent: false,
    });
    assert!(
        app.status_line(80)
            .to_string()
            .contains("Tab to queue, Enter to steer · Alt+↑/↓ queued")
    );

    app.selected_queue = Some(0);
    assert!(app.status_line(80).to_string().contains("Enter to edit"));
    assert!(!app.status_line(80).to_string().contains("Alt+↑/↓ queued"));

    app.queued[0].kind = cagent_agent::protocol::QueuedItemKind::Compact;
    assert!(!app.status_line(80).to_string().contains("Enter to edit"));
}

#[test]
fn status_line_does_not_add_delegated_usage_twice() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.session_usage.input_tokens = Some(100);
    app.session_usage.non_cached_input_tokens = Some(60);
    app.session_usage.cache_read_input_tokens = Some(40);
    app.session_usage.output_tokens = Some(10);
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
                task: "inspect".into(),
                status: cagent_agent::protocol::AgentRunStatus::Completed,
                result: Some("done".into()),
                error: None,
                usage: Some(cagent_agent::provider::ModelUsage {
                    input_tokens: Some(25),
                    non_cached_input_tokens: Some(20),
                    cache_read_input_tokens: Some(5),
                    output_tokens: Some(5),
                    ..cagent_agent::provider::ModelUsage::default()
                }),
                created_at: "0".into(),
                started_at: Some("0".into()),
                completed_at: Some("1".into()),
                timeline: Vec::new(),
                activity: Vec::new(),
            }),
        });

    let usage = app.status_line_values().usage;
    assert_eq!(usage.input_tokens, Some(100));
    assert_eq!(usage.non_cached_input_tokens, Some(60));
    assert_eq!(usage.cache_read_input_tokens, Some(40));
    assert_eq!(usage.output_tokens, Some(10));
}

#[test]
fn status_line_tracks_usage_deltas_and_resets_when_the_model_changes() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.active = true;
    app.session_usage.non_cached_input_tokens = Some(100);
    app.session_usage.output_tokens = Some(20);
    app.token_rate_tracker
        .observe(&app.session_usage, Some("mock/echo"), true);
    app.session_usage.non_cached_input_tokens = Some(223);
    app.session_usage.output_tokens = Some(52);
    app.token_rate_tracker
        .observe(&app.session_usage, Some("mock/echo"), true);

    assert_eq!(
        app.status_line_values().token_rate,
        cagent_agent::presentation::TokenRate {
            input: 123,
            output: 32,
        }
    );

    app.set_selection(Some(("mock".into(), "other".into(), None)));
    app.token_rate_tracker
        .observe(&app.session_usage, Some("mock/other"), true);
    assert_eq!(
        app.status_line_values().token_rate,
        cagent_agent::presentation::TokenRate::default()
    );
}

#[test]
fn statusline_surface_shows_live_preview_and_editor_controls() {
    let config = StatusLineConfig::default();
    let status_rows = status_line_rows(&config);
    let surface = Surface::StatusLine {
        list: ListState::selectable(status_rows.len()),
        rows: status_rows,
        config,
        mode: StatusLineEditorMode::Modules,
        preview: StatusLineValues {
            mode: "ask".into(),
            model: Some("echo".into()),
            ..StatusLineValues::default()
        },
    };

    let text = surface_lines(&surface, Path::new("/tmp/project"), 100)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    assert!(text.contains("Preview"));
    assert!(text.contains("ask"));
    assert!(text.contains('●'));
    assert!(!text.contains("cyan"));
    assert!(!text.contains('‹'));
    assert!(text.contains("mode  active permission and behavior mode"));
    assert!(text.contains("● mode"));
    assert!(!text.contains("●  mode"));
    let rows = surface_lines(&surface, Path::new("/tmp/project"), 100);
    assert!(rows[3].to_string().is_empty());
    assert!(rows[4].to_string().starts_with("› "));
    assert!(rows[5].to_string().starts_with("  "));

    let color_surface = Surface::StatusLine {
        rows: status_line_rows(&StatusLineConfig::default()),
        config: StatusLineConfig::default(),
        list: ListState::selectable(status_line_rows(&StatusLineConfig::default()).len()),
        mode: StatusLineEditorMode::Colors {
            module: StatusLineModule::Mode,
            list: ListState::selectable(status_line_color_choices().len()),
        },
        preview: StatusLineValues::default(),
    };
    let color_rows = surface_lines(&color_surface, Path::new("/tmp/project"), 100);
    assert!(color_rows[4].to_string().starts_with("› "));
    assert!(color_rows[5].to_string().starts_with("  "));

    let hex_surface = Surface::StatusLine {
        rows: status_line_rows(&StatusLineConfig::default()),
        config: StatusLineConfig::default(),
        list: ListState::selectable(status_line_rows(&StatusLineConfig::default()).len()),
        mode: StatusLineEditorMode::Hex {
            module: StatusLineModule::Mode,
            input: "abcdef".into(),
            cursor: 6,
        },
        preview: StatusLineValues::default(),
    };
    let hex_rows = surface_lines(&hex_surface, Path::new("/tmp/project"), 100);
    assert_eq!(hex_rows.last().expect("statusline hex lines").width(), 0);
    assert!(surface_status(&surface).contains("Enter toggle"));
    assert!(surface_status(&surface).contains("Esc save"));

    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.surfaces.push(surface);
    let (_, _, _, status) = app.control_lines_with_draft_limit(100, usize::MAX);
    let color_key = status
        .spans
        .iter()
        .find(|span| span.content == "c")
        .expect("statusline color key hint");
    assert!(color_key.style.add_modifier.contains(Modifier::DIM));
    let reset_key = status
        .spans
        .iter()
        .find(|span| span.content == "r")
        .expect("statusline reset key hint");
    assert!(reset_key.style.add_modifier.contains(Modifier::DIM));

    if let Some(Surface::StatusLine { list, .. }) = app.surfaces.last_mut() {
        list.select(1, VISIBLE_MENU_ITEMS);
    }
    let (_, _, _, status) = app.control_lines_with_draft_limit(100, usize::MAX);
    let reset_key = status
        .spans
        .iter()
        .find(|span| span.content == "r")
        .expect("statusline reset key hint");
    assert!(!reset_key.style.add_modifier.contains(Modifier::DIM));

    if let Some(Surface::StatusLine { config, .. }) = app.surfaces.last_mut() {
        config
            .colors
            .insert(StatusLineModule::Agent, StatusLineColor::Rgb(1, 2, 3));
    }
    let (_, _, _, status) = app.control_lines_with_draft_limit(100, usize::MAX);
    let reset_key = status
        .spans
        .iter()
        .find(|span| span.content == "r")
        .expect("statusline reset key hint");
    assert!(!reset_key.style.add_modifier.contains(Modifier::DIM));
}

#[test]
fn search_mcp_and_statusline_use_the_bounded_list_contract() {
    let search_rows = (0..10)
        .map(|index| cagent_agent::web_search::WebSearchProviderStatus {
            provider: if index % 2 == 0 {
                cagent_agent::web_search::WebSearchProvider::Searxng
            } else {
                cagent_agent::web_search::WebSearchProvider::Exa
            },
            active: index == 0,
            ready: true,
        })
        .collect::<Vec<_>>();
    let mcp_rows = (0..10)
        .map(|index| cagent_agent::mcp::McpEffectiveServer {
            name: format!("server-{index}"),
            location: cagent_agent::mcp::McpLocation::project(),
            definition: mcp_test_definition(),
            status: cagent_agent::mcp::McpRuntimeStatus::Connected,
            agents: Vec::new(),
            allowed_for_agent: true,
            overridden: Vec::new(),
            tools: Vec::new(),
            diagnostics: Vec::new(),
            generation: 1,
        })
        .collect::<Vec<_>>();
    let config = StatusLineConfig::default();
    let status_rows = status_line_rows(&config);
    let surfaces = vec![
        Surface::WebSearchPicker {
            list: ListState::selectable(search_rows.len() + 1),
            rows: search_rows,
            query: String::new(),
            query_cursor: 0,
        },
        Surface::McpServers {
            list: ListState::selectable(mcp_rows.len() + 2),
            rows: mcp_rows,
        },
        Surface::StatusLine {
            list: ListState::selectable(status_rows.len()),
            rows: status_rows,
            config,
            mode: StatusLineEditorMode::Modules,
            preview: StatusLineValues::default(),
        },
    ];
    let width: u16 = 52;
    let height: u16 = 24;

    for surface in surfaces {
        let mut app = App::new(
            Path::new("/tmp/project"),
            ("mock".into(), "echo".into(), None),
            ["mock".into()].into_iter().collect(),
            "ask",
            None,
        );
        app.render_width = width;
        app.render_height = height;
        app.surfaces.push(surface);
        let layout = super::surfaces::surface_layout_with_viewport(
            app.surfaces.last().unwrap(),
            &app.workspace,
            width,
            usize::MAX,
        );
        assert_eq!(
            layout
                .hits
                .iter()
                .filter(|hit| matches!(hit, ListHit::Item(_)))
                .count(),
            VISIBLE_MENU_ITEMS
        );
        assert!(layout.lines.iter().all(|line| line.width() <= 52));
        let down = layout
            .hits
            .iter()
            .position(|hit| *hit == ListHit::Down)
            .unwrap();
        let panel_top = height
            .saturating_sub(app.controls_height(width, height))
            .saturating_add(height.saturating_sub(1).min(2));
        app.select_mouse_at(width, height, panel_top + u16::try_from(down).unwrap(), 2);
        let selected = match app.surfaces.last().unwrap() {
            Surface::WebSearchPicker { list, .. }
            | Surface::McpServers { list, .. }
            | Surface::StatusLine { list, .. } => list.selected,
            _ => unreachable!(),
        };
        assert_eq!(selected, Some(VISIBLE_MENU_ITEMS));
    }
}

#[test]
fn web_search_picker_distinguishes_active_from_ready() {
    let surface = Surface::WebSearchPicker {
        rows: vec![
            cagent_agent::web_search::WebSearchProviderStatus {
                provider: cagent_agent::web_search::WebSearchProvider::Searxng,
                active: true,
                ready: false,
            },
            cagent_agent::web_search::WebSearchProviderStatus {
                provider: cagent_agent::web_search::WebSearchProvider::Exa,
                active: false,
                ready: false,
            },
        ],
        list: ListState::selectable(3),
        query: String::new(),
        query_cursor: 0,
    };

    let lines = surface_lines(&surface, Path::new("/tmp/project"), 80);
    let active = lines
        .iter()
        .flat_map(|line| &line.spans)
        .find(|span| span.content == "  active")
        .expect("active web-search status");
    let not_setup = lines
        .iter()
        .flat_map(|line| &line.spans)
        .find(|span| span.content == " · not setup")
        .expect("not-setup web-search status");

    assert_eq!(active.style, ENABLED_STYLE);
    assert_eq!(not_setup.style, ERROR_STYLE);
}

#[test]
fn web_search_picker_dims_unavailable_chatgpt_subscription() {
    let surface = Surface::WebSearchPicker {
        rows: vec![cagent_agent::web_search::WebSearchProviderStatus {
            provider: cagent_agent::web_search::WebSearchProvider::Chatgpt,
            active: false,
            ready: false,
        }],
        list: ListState::selectable(2),
        query: String::new(),
        query_cursor: 0,
    };

    let lines = surface_lines(&surface, Path::new("/tmp/project"), 80);
    let unavailable = lines
        .iter()
        .flat_map(|line| &line.spans)
        .find(|span| span.content == " · subscription required")
        .expect("unavailable ChatGPT status");
    assert_eq!(unavailable.style, DIM_STYLE);
}

#[test]
fn provider_picker_status_colors_distinguish_disabled_from_not_setup() {
    let surface = Surface::Providers {
        rows: vec![
            cagent_agent::presentation::ProviderPickerRow {
                id: "disabled".into(),
                label: "Disabled".into(),
                status: "disabled".into(),
                setup_instructions: None,
                configuration_instructions: "Configure Disabled".into(),
                credential_environment_variable: None,
                enabled: false,
                managed_auth: false,
                api_key_auth: false,
                has_managed_api_key: false,
                auth_flows: Vec::new(),
            },
            cagent_agent::presentation::ProviderPickerRow {
                id: "not-setup".into(),
                label: "Not setup".into(),
                status: "not setup".into(),
                setup_instructions: Some("Configure Not setup".into()),
                configuration_instructions: "Configure Not setup".into(),
                credential_environment_variable: None,
                enabled: false,
                managed_auth: false,
                api_key_auth: false,
                has_managed_api_key: false,
                auth_flows: Vec::new(),
            },
        ],
        list: ListState::selectable(3),
        query: String::new(),
        query_cursor: 0,
        model_configuration_follows: false,
    };

    let lines = surface_lines(&surface, Path::new("/tmp/project"), 80);
    let status_style = |content| {
        lines
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content == content)
            .expect("provider status")
            .style
    };

    assert_eq!(status_style(" · disabled"), NOTICE_STYLE);
    assert_eq!(status_style(" · not setup"), ERROR_STYLE);
}

#[test]
fn statusline_toggle_keeps_module_row_order() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    let config = app.status_line_config.clone();
    let preview = app.status_line_values();
    let rows = status_line_rows(&config);
    let item_count = rows.len();
    app.surfaces.push(Surface::StatusLine {
        rows,
        config,
        list: ListState::selectable(item_count),
        mode: StatusLineEditorMode::Modules,
        preview,
    });
    let height = 24;
    let row = height - app.controls_height(100, height) + 6;

    app.select_mouse_at(100, height, row, 4);

    let Some(Surface::StatusLine { config, rows, .. }) = app.surfaces.last() else {
        panic!("statusline surface should remain open");
    };
    assert!(!config.modules.contains(&StatusLineModule::Mode));
    assert_eq!(rows[0], StatusLineModule::Mode);
}

#[test]
fn statusline_enabling_module_after_hint_moves_hint_behind_it() {
    let mut config = StatusLineConfig {
        modules: vec![StatusLineModule::Mode, StatusLineModule::Hint],
        ..StatusLineConfig::default()
    };
    let mut rows = vec![
        StatusLineModule::Mode,
        StatusLineModule::Hint,
        StatusLineModule::Tokens,
    ];

    set_status_line_module_enabled(&mut config, &mut rows, StatusLineModule::Tokens, true);

    assert_eq!(
        config.modules,
        [
            StatusLineModule::Mode,
            StatusLineModule::Tokens,
            StatusLineModule::Hint,
        ]
    );
    assert_eq!(
        rows,
        [
            StatusLineModule::Mode,
            StatusLineModule::Tokens,
            StatusLineModule::Hint,
        ]
    );

    move_status_line_module(&mut config, &mut rows, StatusLineModule::Hint, true);
    assert_eq!(
        config.modules,
        [
            StatusLineModule::Mode,
            StatusLineModule::Hint,
            StatusLineModule::Tokens,
        ]
    );
}

#[test]
fn statusline_enabling_module_does_not_move_disabled_or_following_hint() {
    let mut disabled_hint = StatusLineConfig {
        modules: vec![StatusLineModule::Mode],
        ..StatusLineConfig::default()
    };
    let mut disabled_rows = vec![
        StatusLineModule::Mode,
        StatusLineModule::Hint,
        StatusLineModule::Tokens,
    ];
    set_status_line_module_enabled(
        &mut disabled_hint,
        &mut disabled_rows,
        StatusLineModule::Tokens,
        true,
    );
    assert_eq!(disabled_rows[1], StatusLineModule::Hint);

    let mut following_hint = StatusLineConfig {
        modules: vec![StatusLineModule::Mode, StatusLineModule::Hint],
        ..StatusLineConfig::default()
    };
    let mut following_rows = vec![
        StatusLineModule::Mode,
        StatusLineModule::Tokens,
        StatusLineModule::Hint,
    ];
    set_status_line_module_enabled(
        &mut following_hint,
        &mut following_rows,
        StatusLineModule::Tokens,
        true,
    );
    assert_eq!(
        following_rows,
        [
            StatusLineModule::Mode,
            StatusLineModule::Tokens,
            StatusLineModule::Hint,
        ]
    );
}

#[test]
fn statusline_mouse_toggle_tracks_module_when_hint_moves() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    let config = StatusLineConfig {
        modules: vec![StatusLineModule::Hint],
        ..StatusLineConfig::default()
    };
    let rows = vec![StatusLineModule::Hint, StatusLineModule::Tokens];
    app.surfaces.push(Surface::StatusLine {
        rows,
        config,
        list: ListState::selectable(2),
        mode: StatusLineEditorMode::Modules,
        preview: app.status_line_values(),
    });
    let height = 24;
    let row = height - app.controls_height(100, height) + 7;

    app.select_mouse_at(100, height, row, 4);

    let Some(Surface::StatusLine {
        config, rows, list, ..
    }) = app.surfaces.last()
    else {
        panic!("statusline surface should remain open");
    };
    assert_eq!(
        config.modules,
        [StatusLineModule::Tokens, StatusLineModule::Hint]
    );
    assert_eq!(rows, &[StatusLineModule::Tokens, StatusLineModule::Hint]);
    assert_eq!(list.selected, Some(0));
}

#[test]
fn statusline_reset_only_restores_the_selected_module() {
    let mut config = StatusLineConfig::default();
    config.modules.swap(0, 1);
    config
        .modules
        .retain(|module| *module != StatusLineModule::Mode);
    config
        .colors
        .insert(StatusLineModule::Mode, StatusLineColor::Rgb(1, 2, 3));
    config
        .colors
        .insert(StatusLineModule::Agent, StatusLineColor::Rgb(4, 5, 6));
    let mut rows = vec![
        StatusLineModule::Agent,
        StatusLineModule::Mode,
        StatusLineModule::Model,
        StatusLineModule::Provider,
        StatusLineModule::Context,
        StatusLineModule::Hint,
    ];

    reset_status_line_module(&mut config, &mut rows, StatusLineModule::Mode);

    assert_eq!(rows[0], StatusLineModule::Mode);
    assert_eq!(config.modules[0], StatusLineModule::Mode);
    assert_eq!(
        config.color(StatusLineModule::Mode),
        StatusLineModule::Mode.default_color()
    );
    assert_eq!(
        config.color(StatusLineModule::Agent),
        StatusLineColor::Rgb(4, 5, 6)
    );
}

#[tokio::test]
async fn statusline_editor_escape_persists_reordering_before_applying_it() {
    let temporary = tempfile::tempdir().unwrap();
    let config_path = temporary.path().join("config.toml");
    std::fs::write(
        &config_path,
        "version = 1\n[providers.mock]\nenabled = true\n",
    )
    .unwrap();
    let config = cagent_agent::config::ConfigStore::open(&config_path).unwrap();
    let runtime = AgentRuntime::open(
        cagent_agent::runtime::RuntimeOptions::new(temporary.path().join("cagent.db"))
            .with_config(config),
    )
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
    let draft = app.status_line_config.clone();
    let preview = app.status_line_values();
    let rows = status_line_rows(&draft);
    let item_count = rows.len();
    app.surfaces.push(Surface::StatusLine {
        rows,
        config: draft,
        list: ListState::selectable(item_count),
        mode: StatusLineEditorMode::Modules,
        preview,
    });

    app.handle_key(&session, KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))
        .await
        .unwrap();
    app.handle_key(&session, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .await
        .unwrap();

    assert!(app.surfaces.is_empty());
    assert!(app.notice.is_none());
    assert_eq!(
        app.status_line_config.modules[..2],
        [StatusLineModule::Agent, StatusLineModule::Mode]
    );
    let persisted = cagent_agent::config::ConfigSnapshot::load(&config_path).unwrap();
    assert_eq!(persisted.status_line(), &app.status_line_config);
}

#[tokio::test]
async fn statusline_editor_enter_toggles_selected_module() {
    let temporary = tempfile::tempdir().unwrap();
    let config_path = temporary.path().join("config.toml");
    std::fs::write(
        &config_path,
        "version = 1\n[providers.mock]\nenabled = true\n",
    )
    .unwrap();
    let config = cagent_agent::config::ConfigStore::open(&config_path).unwrap();
    let runtime = AgentRuntime::open(
        cagent_agent::runtime::RuntimeOptions::new(temporary.path().join("cagent.db"))
            .with_config(config),
    )
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
    let draft = app.status_line_config.clone();
    let preview = app.status_line_values();
    let rows = status_line_rows(&draft);
    let item_count = rows.len();
    let tokens_row = rows
        .iter()
        .position(|module| *module == StatusLineModule::Tokens)
        .expect("tokens row");
    app.surfaces.push(Surface::StatusLine {
        rows,
        config: draft,
        list: ListState::selectable_at(item_count, tokens_row, VISIBLE_MENU_ITEMS),
        mode: StatusLineEditorMode::Modules,
        preview,
    });

    app.handle_key(&session, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();

    let Some(Surface::StatusLine {
        config, rows, list, ..
    }) = app.surfaces.last()
    else {
        panic!("statusline surface should remain open");
    };
    let tokens_row = rows
        .iter()
        .position(|module| *module == StatusLineModule::Tokens)
        .expect("tokens row");
    assert_eq!(rows[tokens_row + 1], StatusLineModule::Hint);
    assert_eq!(list.selected, Some(tokens_row));
    assert!(config.modules.contains(&StatusLineModule::Tokens));
    assert_eq!(app.status_line_config, StatusLineConfig::default());
}

#[test]
fn completion_popup_precedes_composer_and_status() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.insert_text("/mo");
    let (queue, popup, composer, status) = app.control_lines_with_draft_limit(80, usize::MAX);
    assert!(queue.is_empty());
    assert!(
        popup
            .get(1)
            .is_some_and(|line| line.to_string().contains("/model"))
    );
    assert!(
        popup
            .first()
            .is_some_and(|line| line.to_string().starts_with('─'))
    );
    assert_eq!(composer.len(), 2);
    assert!(!composer[0].spans.is_empty());
    assert!(composer[1].spans.is_empty());
    assert!(status.to_string().contains("navigate"));
    assert!(status.to_string().starts_with("  "));
    assert!(status.to_string().ends_with("  "));
    assert_eq!(
        app.control_heights(80),
        (0, u16::try_from(popup.len()).unwrap(), 2, 1)
    );
}

#[test]
fn word_wrapped_composer_rows_drive_height_and_respect_cap() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.insert_text("123456 123456 123456");

    // The composer has ten content columns at this width. Word-aware
    // wrapping produces three rows even though the total display width would
    // fit into two rows if it were divided without considering word breaks.
    assert_eq!(app.control_heights(14).2, 4);

    app.composer_max_rows = Some(2);
    assert_eq!(app.control_heights(14).2, 3);
}

#[test]
fn mouse_selects_completion_below_the_sticky_rows() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.insert_text("/");
    let height = 24;
    let controls_top = height - app.controls_height(80, height);

    // The popup follows the indicator row and directly precedes the composer
    // box. The separator doubles as the permanent top-indicator slot.
    app.select_mouse_at(80, height, controls_top + 3, 1);

    assert_eq!(app.slash_list.selected, Some(1));
}

#[test]
fn queued_messages_stay_on_one_row_and_dim_the_message_text() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.queued.push(QueuedMessage {
        id: cagent_agent::protocol::QueuedMessageId::default(),
        position: 0,
        target: QueueTarget::NextBoundary,
        kind: cagent_agent::protocol::QueuedItemKind::Prompt,
        mode: None,
        command_text: None,
        text: "a very long queued message that must not wrap".into(),
        attachments: Vec::new(),
        images: Vec::new(),
        image_chips: Vec::new(),
        blocked_by_startup: false,
        require_subagent: false,
    });

    let lines = app.queue_lines(24);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].to_string(), "• Queued steering inputs (Enter)");
    assert!(lines[1].to_string().starts_with("  ↳ "));
    assert!(lines[1].width() <= 24);
    assert_eq!(lines[1].spans[2].style, DIM_STYLE);
    assert!(lines.last().is_some_and(|line| !line.spans.is_empty()));

    app.selected_queue = Some(0);
    assert!(app.queue_lines(24)[1].to_string().starts_with("› ↳ "));
    app.queued[0].target = QueueTarget::EndOfTurn;
    assert_eq!(
        app.queue_lines(24)[0].to_string(),
        "• Queued follow-up inputs (Tab)"
    );
}

#[test]
fn queued_commands_render_their_original_slash_text() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.queued = vec![
        QueuedMessage {
            id: cagent_agent::protocol::QueuedMessageId::new(),
            position: 0,
            target: QueueTarget::NextBoundary,
            kind: cagent_agent::protocol::QueuedItemKind::ModePrompt,
            mode: Some("plan".into()),
            command_text: Some("/plan review this design".into()),
            text: "review this design".into(),
            attachments: Vec::new(),
            images: Vec::new(),
            image_chips: Vec::new(),
            blocked_by_startup: false,
            require_subagent: false,
        },
        QueuedMessage {
            id: cagent_agent::protocol::QueuedMessageId::new(),
            position: 1,
            target: QueueTarget::EndOfTurn,
            kind: cagent_agent::protocol::QueuedItemKind::Compact,
            mode: None,
            command_text: None,
            text: "focus on the migration risks".into(),
            attachments: Vec::new(),
            images: Vec::new(),
            image_chips: Vec::new(),
            blocked_by_startup: false,
            require_subagent: false,
        },
    ];

    let lines = app.queue_lines(80);
    assert!(
        lines
            .iter()
            .any(|line| line.to_string() == "  ↳ /plan review this design")
    );
    assert!(
        lines
            .iter()
            .any(|line| line.to_string() == "  ↳ /compact focus on the migration risks")
    );
}

#[test]
fn queued_selection_tracks_message_ids_across_snapshot_refreshes() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    let first = QueuedMessage {
        id: cagent_agent::protocol::QueuedMessageId::new(),
        position: 0,
        target: QueueTarget::NextBoundary,
        kind: cagent_agent::protocol::QueuedItemKind::Prompt,
        mode: None,
        command_text: None,
        text: "first".into(),
        attachments: Vec::new(),
        images: Vec::new(),
        image_chips: Vec::new(),
        blocked_by_startup: false,
        require_subagent: false,
    };
    let selected = QueuedMessage {
        id: cagent_agent::protocol::QueuedMessageId::new(),
        position: 1,
        target: QueueTarget::EndOfTurn,
        kind: cagent_agent::protocol::QueuedItemKind::Prompt,
        mode: None,
        command_text: None,
        text: "selected".into(),
        attachments: Vec::new(),
        images: Vec::new(),
        image_chips: Vec::new(),
        blocked_by_startup: false,
        require_subagent: false,
    };
    app.queued = vec![first.clone(), selected.clone()];
    app.selected_queue = Some(1);

    let mut snapshot = session_snapshot_with_interaction(None);
    snapshot.queue = vec![selected.clone()];
    app.apply_session_snapshot(&snapshot);
    assert_eq!(app.selected_queue, Some(0));

    snapshot.queue = vec![first];
    app.apply_session_snapshot(&snapshot);
    assert!(app.selected_queue.is_none());
}

#[tokio::test]
async fn stale_queued_selection_does_not_panic_during_navigation() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = AgentRuntime::open(cagent_agent::runtime::RuntimeOptions::new(
        temporary.path().join("stale-queue-selection.db"),
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
    app.queued.push(QueuedMessage {
        id: cagent_agent::protocol::QueuedMessageId::new(),
        position: 0,
        target: QueueTarget::NextBoundary,
        kind: cagent_agent::protocol::QueuedItemKind::Prompt,
        mode: None,
        command_text: None,
        text: "queued".into(),
        attachments: Vec::new(),
        images: Vec::new(),
        image_chips: Vec::new(),
        blocked_by_startup: false,
        require_subagent: false,
    });
    app.selected_queue = Some(1);

    app.handle_key(&session, KeyEvent::new(KeyCode::Up, KeyModifiers::ALT))
        .await
        .unwrap();

    assert!(app.selected_queue.is_none());
}

#[test]
fn controls_fit_a_short_terminal_without_losing_composer_padding_or_status() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.insert_text("/model");

    assert_eq!(app.control_heights_within(80, 4), (0, 0, 1, 1));
    assert_eq!(app.controls_height(80, 4), 4);
}

#[test]
fn user_transcript_rows_have_full_width_padding() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.push_user_with_label(None, "hello\nsecond", &[]);
    let history = crate::render::transcript::history_item_rows(&app.history[0], &app.workspace, 20);
    let lines = history
        .iter()
        .map(|row| row.line.clone())
        .skip_while(|line| line.style != USER_STYLE)
        .take(4)
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 4);
    assert!(lines.iter().all(|line| line.width() == 20));
    assert!(lines.iter().all(|line| line.style == USER_STYLE));
}

#[test]
fn labelled_user_messages_render_the_label_before_the_prompt() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.push_user_with_label(Some("Plan"), "review the parser", &[]);
    let cagent_agent::protocol::TranscriptBlockKind::User { label, .. } = &app.history[0].kind
    else {
        panic!("semantic user transcript block");
    };
    assert_eq!(label.as_deref(), Some("Plan"));
    let rows = crate::render::transcript::history_item_rows(&app.history[0], &app.workspace, 80);
    assert!(
        rows.iter()
            .any(|row| row.line.to_string().trim_end() == "› Plan: review the parser")
    );
    let label = rows
        .iter()
        .flat_map(|row| &row.line.spans)
        .find(|span| span.content == "Plan: ")
        .expect("command label span");
    assert_eq!(label.style, Style::default().add_modifier(Modifier::BOLD));
}

#[test]
fn sent_user_messages_keep_confirmed_attachment_chips() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    let attachment = cagent_agent::presentation::SubmittedAttachment {
        start: "inspect ".len(),
        end: "inspect @src/lib.rs".len(),
        spec: cagent_agent::protocol::AttachmentSpec {
            path: "src/lib.rs".into(),
            start_line: Some(1),
            end_line: Some(1),
        },
        kind: cagent_agent::presentation::SubmittedAttachmentKind::File,
    };
    app.push_user_with_label(
        None,
        "inspect @src/lib.rs and leave @missing.rs raw",
        &[attachment],
    );
    let history = crate::render::transcript::history_item_rows(&app.history[0], &app.workspace, 80);
    let line = history
        .iter()
        .map(|row| &row.line)
        .find(|line| line.to_string().contains("[File · src/lib.rs:1-1]"))
        .expect("sent message contains its file chip");
    assert!(line.spans.iter().any(|span| span.style == CHIP_STYLE));
    assert!(line.to_string().contains("@missing.rs raw"));
}

#[test]
fn forked_user_draft_restores_confirmed_attachment_chips() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    let spec = cagent_agent::protocol::AttachmentSpec {
        path: "src/lib.rs".into(),
        start_line: None,
        end_line: None,
    };

    app.replace_draft_with_attachment_specs("inspect @src/lib.rs", std::slice::from_ref(&spec));

    assert_eq!(app.confirmed_attachments().len(), 1);
    assert_eq!(app.confirmed_attachments()[0], spec);
}

#[test]
fn history_navigation_moves_past_the_latest_message_and_reconstructs_attachment_chips() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.history_entries = vec![
        HistoryEntry {
            kind: cagent_agent::protocol::ComposerInputKind::Prompt,
            text: "older @docs ".into(),
            attachment_specs: vec![cagent_agent::protocol::AttachmentSpec {
                path: PathBuf::from("docs"),
                start_line: None,
                end_line: None,
            }],
            images: Vec::new(),
            image_chips: Vec::new(),
        },
        HistoryEntry {
            kind: cagent_agent::protocol::ComposerInputKind::Prompt,
            text: "newer".into(),
            attachment_specs: Vec::new(),
            images: Vec::new(),
            image_chips: Vec::new(),
        },
    ];
    app.recall_previous_history();
    assert_eq!(app.draft, "newer");
    app.recall_previous_history();
    assert_eq!(app.draft, "older @docs ");
    assert_eq!(app.attachments[0].spec.path, PathBuf::from("docs"));
}

#[tokio::test]
async fn composer_vertical_navigation_precedes_history_and_repeats_line_boundaries() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = AgentRuntime::open(cagent_agent::runtime::RuntimeOptions::new(
        temporary.path().join("composer-navigation.db"),
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
    app.history_entries = vec![
        HistoryEntry {
            kind: cagent_agent::protocol::ComposerInputKind::Prompt,
            text: "older history entry".into(),
            attachment_specs: Vec::new(),
            images: Vec::new(),
            image_chips: Vec::new(),
        },
        HistoryEntry {
            kind: cagent_agent::protocol::ComposerInputKind::Prompt,
            text: "history entry\nsecond line".into(),
            attachment_specs: Vec::new(),
            images: Vec::new(),
            image_chips: Vec::new(),
        },
    ];
    app.replace_draft("first\nsecond");

    app.handle_key(&session, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.draft, "first\nsecond");
    assert_eq!(app.cursor, "first".len());

    app.handle_key(&session, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.draft, "first\nsecond");
    assert_eq!(app.cursor, 0);

    app.handle_key(&session, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.draft, "history entry\nsecond line");
    assert_eq!(app.cursor, app.draft.len());

    // A recalled entry focused at its end immediately continues through
    // history rather than moving vertically within that entry.
    app.handle_key(&session, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.draft, "older history entry");

    app.recall_history(1);
    app.move_left();
    app.handle_key(&session, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.draft, "history entry\nsecond line");
    assert_eq!(app.history_index, Some(1));
    assert!(app.cursor <= "history entry".len());

    app.handle_key(&session, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.draft, "history entry\nsecond line");
    assert_eq!(app.cursor, 0);

    app.handle_key(&session, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.draft, "older history entry");

    app.handle_key(&session, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.draft, "history entry\nsecond line");
    assert_eq!(app.cursor, app.draft.len());

    app.cursor = "history entry\nsecond".len();
    app.preferred_column = None;
    app.handle_key(&session, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.draft, "history entry\nsecond line");
    assert_eq!(app.cursor, app.draft.len());

    app.handle_key(&session, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.draft, "first\nsecond");

    app.replace_draft("cleared draft");
    app.clear_draft_for_recall();
    app.handle_key(&session, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.draft, "cleared draft");
    assert_eq!(app.cursor, app.draft.len());

    app.handle_key(&session, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.draft, "history entry\nsecond line");
    assert_eq!(app.cursor, app.draft.len());

    app.replace_draft("first\nsecond\nthird");
    app.cursor = "first\nsecond\n".len();
    app.preferred_column = None;
    app.handle_key(
        &session,
        KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL),
    )
    .await
    .unwrap();
    assert_eq!(app.cursor, "first\n".len());

    app.handle_key(
        &session,
        KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL),
    )
    .await
    .unwrap();
    assert_eq!(app.cursor, "first\nsecond".len());

    app.handle_key(
        &session,
        KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL),
    )
    .await
    .unwrap();
    assert_eq!(app.cursor, "first\nsecond\nthird".len());
}

#[test]
fn prepending_transcript_blocks_preserves_the_visible_row_anchor() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    let make_user = |id: &str, text: &str| cagent_agent::protocol::TranscriptBlock {
        id: cagent_agent::protocol::TranscriptBlockId(id.into()),
        status: cagent_agent::protocol::TranscriptBlockStatus::Completed,
        kind: cagent_agent::protocol::TranscriptBlockKind::User {
            label: None,
            text: text.into(),
            attachments: Vec::new(),
            images: Vec::new(),
            image_chips: Vec::new(),
        },
    };
    let mut snapshot = session_snapshot_with_interaction(None);
    snapshot.transcript = cagent_agent::protocol::TranscriptWindow::new(
        vec![make_user("b", "second"), make_user("c", "third")],
        None,
    );
    app.apply_session_snapshot(&snapshot);
    app.welcome.clear();
    app.render_width = 24;
    app.follow_history_tail = false;
    app.history_scroll = 2;
    app.transcript_prefetch_armed = false;
    app.ensure_history_layout(24);
    let previous_rows = app.history_layout.rendered.as_ref().unwrap().rows.len();
    let viewport_rows = 4;
    let previous_maximum = previous_rows.saturating_sub(viewport_rows);

    snapshot.transcript = cagent_agent::protocol::TranscriptWindow::new(
        vec![
            make_user("a", "first line\nwith a wrapped continuation"),
            make_user("b", "second"),
            make_user("c", "third"),
        ],
        None,
    );
    app.apply_session_snapshot(&snapshot);
    app.ensure_history_layout(24);
    let next_rows = app.history_layout.rendered.as_ref().unwrap().rows.len();
    let added_rows = next_rows - previous_rows;
    assert_eq!(app.history_scroll, 2 + added_rows);
    assert_eq!(
        next_rows.saturating_sub(viewport_rows),
        previous_maximum + added_rows
    );
    assert!(!app.transcript_prefetch_armed);
}

#[tokio::test]
async fn empty_composer_home_and_end_navigate_transcript_boundaries() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = AgentRuntime::open(cagent_agent::runtime::RuntimeOptions::new(
        temporary.path().join("transcript-boundaries.db"),
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
    app.history_scroll = 12;
    app.follow_history_tail = true;
    app.history_layout.start_block = 1;
    app.transcript_older = Some(
        serde_json::from_value(serde_json::json!({
            "conversation_id": session.id(),
            "older_node_id": cagent_agent::protocol::NodeId::new(),
            "newer_node_id": cagent_agent::protocol::NodeId::new(),
        }))
        .unwrap(),
    );

    app.handle_key(&session, KeyEvent::new(KeyCode::Home, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.history_scroll, 0);
    assert_eq!(app.history_layout.start_block, 0);
    assert!(!app.follow_history_tail);
    assert!(app.transcript_home_drain);

    app.handle_key(&session, KeyEvent::new(KeyCode::End, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.history_scroll, usize::MAX);
    assert!(!app.follow_history_tail);
    assert!(!app.transcript_home_drain);

    app.replace_draft("first\nsecond");
    app.cursor = "first\nsecond".len();
    app.handle_key(&session, KeyEvent::new(KeyCode::Home, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.cursor, "first\n".len());
    app.handle_key(&session, KeyEvent::new(KeyCode::End, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.cursor, "first\nsecond".len());
}

#[tokio::test]
async fn observer_empty_composer_home_and_end_navigate_transcript_boundaries() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime = AgentRuntime::open(cagent_agent::runtime::RuntimeOptions::new(
        temporary.path().join("observer-transcript-boundaries.db"),
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
    app.history_scroll = 12;
    app.follow_history_tail = true;

    app.handle_key(&session, KeyEvent::new(KeyCode::Home, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.history_scroll, 0);
    assert!(!app.follow_history_tail);
    app.handle_key(&session, KeyEvent::new(KeyCode::End, KeyModifiers::NONE))
        .await
        .unwrap();
    assert_eq!(app.history_scroll, usize::MAX);
    assert!(!app.follow_history_tail);
}

#[test]
fn history_navigation_collapses_consecutive_duplicate_entries() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.hydrate_composer_history(
        ["a", "a", "b", "b"]
            .into_iter()
            .map(|text| cagent_agent::protocol::ComposerHistoryEntry {
                kind: cagent_agent::protocol::ComposerInputKind::Prompt,
                text: text.into(),
                attachment_specs: Vec::new(),
                images: Vec::new(),
                image_chips: Vec::new(),
            })
            .collect(),
    );
    assert_eq!(app.history_entries.len(), 2);

    app.recall_previous_history();
    assert_eq!(app.draft, "b");
    app.recall_previous_history();
    assert_eq!(app.draft, "a");
}

#[test]
fn resubmitted_history_entry_moves_to_the_latest_position() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.record_history_entry("first".into(), Vec::new());
    app.record_history_entry("second".into(), Vec::new());
    app.record_history_entry("first".into(), Vec::new());

    assert_eq!(
        app.history_entries
            .iter()
            .map(|entry| entry.text.as_str())
            .collect::<Vec<_>>(),
        vec!["second", "first"]
    );
    app.recall_previous_history();
    assert_eq!(app.draft, "first");
}

#[test]
fn bash_history_recall_restores_shell_mode() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.hydrate_composer_history(vec![cagent_agent::protocol::ComposerHistoryEntry {
        kind: cagent_agent::protocol::ComposerInputKind::Bash,
        text: "./scripts/check".into(),
        attachment_specs: Vec::new(),
        images: Vec::new(),
        image_chips: Vec::new(),
    }]);
    app.recall_previous_history();
    assert_eq!(app.composer_mode, ComposerMode::Bash);
    assert_eq!(app.draft, "./scripts/check");
}

#[test]
fn hydrated_composer_history_restores_attachments_and_the_draft_after_navigation() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.hydrate_composer_history(vec![cagent_agent::protocol::ComposerHistoryEntry {
        kind: cagent_agent::protocol::ComposerInputKind::Prompt,
        text: "inspect @src/lib.rs".into(),
        attachment_specs: vec![cagent_agent::protocol::AttachmentSpec {
            path: "src/lib.rs".into(),
            start_line: None,
            end_line: None,
        }],
        images: Vec::new(),
        image_chips: Vec::new(),
    }]);
    app.replace_draft("unfinished draft");

    app.recall_previous_history();
    assert_eq!(app.draft, "inspect @src/lib.rs");
    assert_eq!(app.attachments.len(), 1);
    assert_eq!(app.attachments[0].spec.path, PathBuf::from("src/lib.rs"));

    app.recall_next_history();
    assert_eq!(app.draft, "unfinished draft");
}

#[test]
fn ctrl_c_cleared_draft_is_recalled_before_durable_history() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.history_entries = vec![
        HistoryEntry {
            kind: cagent_agent::protocol::ComposerInputKind::Prompt,
            text: "older prompt".into(),
            attachment_specs: Vec::new(),
            images: Vec::new(),
            image_chips: Vec::new(),
        },
        HistoryEntry {
            kind: cagent_agent::protocol::ComposerInputKind::Prompt,
            text: "newer prompt".into(),
            attachment_specs: Vec::new(),
            images: Vec::new(),
            image_chips: Vec::new(),
        },
    ];
    app.replace_draft("cleared prompt");

    app.clear_draft_for_recall();
    assert!(app.draft.is_empty());

    app.recall_previous_history();
    assert_eq!(app.draft, "cleared prompt");

    app.recall_previous_history();
    assert_eq!(app.draft, "newer prompt");

    app.recall_next_history();
    assert_eq!(app.draft, "cleared prompt");

    app.recall_next_history();
    assert!(app.draft.is_empty());
}

#[test]
fn cleared_draft_recall_is_replaced_and_preserves_composer_chips() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.insert_text("first draft");
    app.clear_draft_for_recall();
    app.insert_text("second ");
    app.insert_paste("draft");
    let paste_count = app.pastes.len();

    app.clear_draft_for_recall();
    app.recall_previous_history();

    assert_eq!(app.draft, "second draft");
    assert_eq!(app.pastes.len(), paste_count);
    assert!(app.cleared_draft.is_some());

    app.discard_cleared_draft();
    app.clear_draft();
    app.recall_previous_history();
    assert!(app.draft.is_empty());
}

#[test]
fn ordinary_draft_clearing_does_not_create_a_recall_entry() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.replace_draft("opened a menu instead");

    app.clear_draft();
    app.recall_previous_history();

    assert!(app.draft.is_empty());
    assert!(app.cleared_draft.is_none());
}

#[test]
fn later_composer_history_snapshot_preserves_draft_and_transcript() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    let mut snapshot = session_snapshot_with_interaction(None);
    snapshot.transcript = vec![cagent_agent::protocol::TranscriptBlock {
        id: cagent_agent::protocol::TranscriptBlockId::derived(
            "tools",
            cagent_agent::protocol::NodeId::new(),
        ),
        status: cagent_agent::protocol::TranscriptBlockStatus::Completed,
        kind: cagent_agent::protocol::TranscriptBlockKind::ToolGroups {
            groups: vec![cagent_agent::presentation::ToolActivityGroup::Bash {
                node_id: Some(cagent_agent::protocol::NodeId::new()),
                terminal_id: None,
                command: "cargo check".into(),
                status: cagent_agent::presentation::ToolActivityStatus::Succeeded,
                output: Some("ok".into()),
                ansi_output: None,
                exit_code: Some(0),
            }],
        },
    }]
    .into();
    app.replace_draft("unfinished draft");
    app.apply_session_snapshot(&snapshot);
    let transcript_len = app.history.len();

    snapshot.composer_history = vec![cagent_agent::protocol::ComposerHistoryEntry {
        kind: cagent_agent::protocol::ComposerInputKind::Prompt,
        text: "hydrated command".into(),
        attachment_specs: Vec::new(),
        images: Vec::new(),
        image_chips: Vec::new(),
    }];
    app.apply_session_snapshot(&snapshot);

    assert_eq!(app.draft, "unfinished draft");
    assert_eq!(app.history.len(), transcript_len);
    assert_eq!(app.history_entries[0].text, "hydrated command");
}

#[test]
fn stale_session_snapshot_does_not_erase_an_accepted_composer_entry() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    let mut snapshot = session_snapshot_with_interaction(None);
    snapshot.composer_history = vec![cagent_agent::protocol::ComposerHistoryEntry {
        kind: cagent_agent::protocol::ComposerInputKind::Prompt,
        text: "older prompt".into(),
        attachment_specs: Vec::new(),
        images: Vec::new(),
        image_chips: Vec::new(),
    }];
    app.apply_session_snapshot(&snapshot);
    app.record_history_entry("just sent".into(), Vec::new());

    // A Working update can be projected before the durable user node has
    // appended the new composer-history row.
    app.apply_session_snapshot(&snapshot);
    app.recall_previous_history();

    assert_eq!(app.draft, "just sent");
}

#[test]
fn stale_session_snapshot_does_not_undo_resubmission_recency() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    let mut snapshot = session_snapshot_with_interaction(None);
    snapshot.composer_history = ["first", "second"]
        .into_iter()
        .map(|text| cagent_agent::protocol::ComposerHistoryEntry {
            kind: cagent_agent::protocol::ComposerInputKind::Prompt,
            text: text.into(),
            attachment_specs: Vec::new(),
            images: Vec::new(),
            image_chips: Vec::new(),
        })
        .collect();
    app.apply_session_snapshot(&snapshot);
    app.record_history_entry("first".into(), Vec::new());

    app.apply_session_snapshot(&snapshot);
    app.recall_previous_history();

    assert_eq!(app.draft, "first");
}

#[test]
fn log_wrapping_keeps_the_message_gutter() {
    let lines = wrap_log_line(&Line::from("› abcdef"), 6);
    assert_eq!(
        lines.iter().map(ToString::to_string).collect::<Vec<_>>(),
        vec!["› abcd", "  ef"]
    );
}

#[test]
fn assistant_wrapping_reserves_space_for_the_gutter() {
    let lines = wrap_log_line(&Line::from("• abcdef"), 6);
    assert_eq!(
        lines.iter().map(ToString::to_string).collect::<Vec<_>>(),
        vec!["• abcd", "  ef"]
    );
}

#[test]
fn assistant_continuation_wrapping_reserves_space_for_the_gutter() {
    let lines = wrap_log_line(&Line::from("  abcdef"), 6);
    assert_eq!(
        lines.iter().map(ToString::to_string).collect::<Vec<_>>(),
        vec!["  abcd", "  ef"]
    );
}

#[test]
fn wrapping_prefers_word_boundaries() {
    let lines = wrap_log_line(&Line::from("• alpha beta gamma"), 12);
    assert_eq!(
        lines.iter().map(ToString::to_string).collect::<Vec<_>>(),
        vec!["• alpha beta", "  gamma"]
    );
}

#[test]
fn assistant_wrapping_handles_narrow_unicode_gutters() {
    for width in 1..=4 {
        let lines = wrap_log_line(&Line::from("• abcdef"), width);
        assert!(!lines.is_empty());
    }
}

#[test]
fn submission_trims_outer_blank_lines_but_preserves_inner_spacing() {
    assert_eq!(
        trim_outer_blank_lines(" \n\n  first  \n\n  second  \n \n"),
        "  first  \n\n  second  "
    );
}

#[test]
fn long_assistant_paragraphs_wrap_inside_the_gutter() {
    let markdown = "This paragraph is intentionally long so its wrapping must leave room for the assistant marker and its continuation padding.";
    let lines = assistant_markdown_rows(&cagent_agent::presentation::parse_markdown(markdown), 32);
    assert!(lines.iter().all(|line| line.line.width() <= 32));
    assert!(
        lines
            .first()
            .is_some_and(|line| line.to_string().starts_with("• "))
    );
    assert!(
        lines
            .iter()
            .skip(1)
            .all(|line| line.to_string().starts_with("  "))
    );
}

#[test]
fn blockquotes_wrap_inside_the_quote_gutter() {
    let markdown = "> This blockquote is intentionally long so its continuation rows must stay inside the quote.\n\n> A second paragraph keeps the quote prefix too.";
    let lines = assistant_markdown_rows(&cagent_agent::presentation::parse_markdown(markdown), 32);

    assert!(lines.iter().all(|line| line.line.width() <= 32));
    assert!(lines.iter().any(|line| line.to_string().contains("┃ ")));
    assert!(
        lines
            .iter()
            .skip(1)
            .any(|line| line.to_string().contains("┃ "))
    );
}

#[test]
fn markdown_link_metadata_does_not_count_as_display_width() {
    let lines = crate::markdown::layout_document(
        &cagent_agent::presentation::parse_markdown(
            "[OpenAI](https://www.openai.com)\n\n| Name | Description |\n| --- | --- |\n| Markdown link | [OpenAI](https://www.openai.com) |",
        ),
        40,
    );
    assert!(lines.iter().all(|line| line.line.width() <= 40));
    let table_row = lines
        .iter()
        .find(|line| line.to_string().contains("Markdown link"))
        .expect("table link row");
    assert!(table_row.line.width() <= 40);
}

#[test]
fn tables_reflow_cells_to_the_available_width() {
    let document = cagent_agent::presentation::parse_markdown(
        "| Name | Description |\n| --- | --- |\n| Alpha | This is deliberately long table content that must wrap. |",
    );
    let wide = crate::markdown::layout_document(&document, 100);
    let wrapped = crate::markdown::layout_document(&document, 32);
    assert!(wrapped.len() > wide.len());
    assert!(wrapped.iter().all(|line| line.line.width() <= 32));
    assert!(wrapped.iter().any(|line| line.to_string().contains("must")));
    assert!(wrapped.iter().any(|line| line.to_string().starts_with('─')));
}

#[test]
fn accepted_path_is_rendered_as_a_file_chip_but_submits_raw_source() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.insert_text("@src/lib.rs");
    let entry = ListEntry {
        path: PathBuf::from("src/lib.rs"),
        kind: ListEntryKind::File,
    };
    app.apply_path_completion(0, &entry);
    assert!(app.rendered_draft().contains("[File · src/lib.rs]"));
    assert_eq!(app.draft, "@src/lib.rs ");
    assert_eq!(app.attachments.len(), 1);
}

#[test]
fn accepted_home_and_absolute_paths_preserve_their_spelling() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );

    app.insert_text("@~/Do");
    app.apply_path_completion(
        0,
        &ListEntry {
            path: PathBuf::from("~/Documents/my notes.md"),
            kind: ListEntryKind::File,
        },
    );
    assert_eq!(app.draft, "@{~/Documents/my notes.md} ");
    assert!(
        app.rendered_draft()
            .contains("[File · ~/Documents/my notes.md]")
    );

    app.replace_draft("");
    app.insert_text("@/var/lo");
    app.apply_path_completion(
        0,
        &ListEntry {
            path: PathBuf::from("/var/log"),
            kind: ListEntryKind::Directory,
        },
    );
    assert_eq!(app.draft, "@/var/log ");
    assert_eq!(app.attachments.len(), 1);
    assert_eq!(app.attachments[0].kind, ListEntryKind::Directory);
}

#[tokio::test]
async fn bare_attachment_completion_lists_root_and_starts_a_path_index() {
    let temporary = tempfile::tempdir().unwrap();
    std::fs::write(temporary.path().join("root.txt"), "").unwrap();
    std::fs::create_dir(temporary.path().join("nested")).unwrap();
    std::fs::write(temporary.path().join("nested/needle.txt"), "").unwrap();
    std::fs::write(temporary.path().join(".hidden.txt"), "").unwrap();
    std::fs::create_dir(temporary.path().join(".hidden-directory")).unwrap();
    let runtime = AgentRuntime::open(cagent_agent::runtime::RuntimeOptions::new(
        temporary.path().join("bare-attachment-completion.db"),
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
    let (sender, mut updates) = tokio::sync::mpsc::channel(32);
    let mut scheduled = None;
    let mut task = None;
    let mut path_completion = None;

    app.insert_text("@");
    schedule_completion(
        &session,
        &app,
        &sender,
        &mut scheduled,
        &mut task,
        &mut path_completion,
    );
    assert!(
        path_completion.is_some(),
        "bare @ should start a recursive index"
    );

    let root_rows = tokio::time::timeout(Duration::from_secs(1), updates.recv())
        .await
        .unwrap()
        .unwrap();
    app.apply_completion(root_rows);
    let root_rows = &app.attachment_completion.as_ref().unwrap().rows;
    assert!(
        root_rows
            .iter()
            .any(|entry| entry.path == Path::new("root.txt"))
    );
    assert!(
        root_rows
            .iter()
            .all(|entry| entry.path != Path::new("nested/needle.txt")),
        "bare @ only lists direct workspace entries"
    );
    assert!(
        root_rows
            .iter()
            .all(|entry| !entry.path.to_string_lossy().starts_with('.'))
    );

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if path_completion
                .as_ref()
                .and_then(|completion| completion.session.complete_cached("needle"))
                .is_some()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    app.insert_text("needle");
    schedule_completion(
        &session,
        &app,
        &sender,
        &mut scheduled,
        &mut task,
        &mut path_completion,
    );
    assert!(
        path_completion.is_some(),
        "a non-empty token reuses the eager index"
    );
    let indexed_rows = tokio::time::timeout(Duration::from_secs(1), updates.recv())
        .await
        .unwrap()
        .unwrap();
    app.apply_completion(indexed_rows);
    assert!(
        app.attachment_completion
            .as_ref()
            .unwrap()
            .rows
            .iter()
            .any(|entry| entry.path == Path::new("nested/needle.txt")),
        "indexed results replace the direct root rows"
    );

    app.attachment_completion = None;
    schedule_completion(
        &session,
        &app,
        &sender,
        &mut scheduled,
        &mut task,
        &mut path_completion,
    );
    assert!(task.is_none());
    assert!(path_completion.is_none());
    assert!(scheduled.is_none());

    std::fs::write(temporary.path().join("nested/fresh-after-reopen.txt"), "").unwrap();
    app.insert_text(" ");
    app.insert_text("@");
    schedule_completion(
        &session,
        &app,
        &sender,
        &mut scheduled,
        &mut task,
        &mut path_completion,
    );
    assert!(
        path_completion.is_some(),
        "reopening @ should start a fresh index"
    );
    let reopened_root = tokio::time::timeout(Duration::from_secs(1), updates.recv())
        .await
        .unwrap()
        .unwrap();
    app.apply_completion(reopened_root);
    assert!(
        app.attachment_completion
            .as_ref()
            .unwrap()
            .rows
            .iter()
            .all(|entry| !entry.path.to_string_lossy().contains("fresh-after-reopen"))
    );

    app.insert_text("fresh-after");
    schedule_completion(
        &session,
        &app,
        &sender,
        &mut scheduled,
        &mut task,
        &mut path_completion,
    );
    let reopened_indexed = tokio::time::timeout(Duration::from_secs(1), updates.recv())
        .await
        .unwrap()
        .unwrap();
    app.apply_completion(reopened_indexed);
    assert!(
        app.attachment_completion
            .as_ref()
            .unwrap()
            .rows
            .iter()
            .any(|entry| entry.path == Path::new("nested/fresh-after-reopen.txt"))
    );
}

#[test]
fn chip_navigation_and_activation_are_atomic() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.insert_text("@TEST.md ");
    app.confirm_attachment(0, 8, ListEntryKind::File);
    app.move_left();
    assert_eq!(app.cursor, 8);
    app.move_left();
    assert_eq!(app.cursor, 0);
    app.move_right();
    assert_eq!(app.cursor, 8);
    app.activate_chip_at_cursor();
    assert!(app.attachment_completion.is_some());
    assert_eq!(app.cursor, 8);
}

#[test]
fn large_paste_collapses_without_changing_its_submitted_text() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    let pasted = (0..21)
        .map(|line| format!("retained line {line}"))
        .collect::<Vec<_>>()
        .join("\n");

    app.insert_paste(&pasted);

    assert_eq!(app.draft, pasted);
    assert_eq!(app.pastes.len(), 1);
    assert!(app.rendered_draft().starts_with("[Pasted text · 21 lines"));
    assert_eq!(app.normalized_submission_text(), pasted);
}

#[test]
fn submission_adds_boundaries_around_attachment_chips() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.draft = "aa@TEST.mdbb".into();
    app.attachments.push(AttachmentChip {
        spec: cagent_agent::protocol::AttachmentSpec {
            path: PathBuf::from("TEST.md"),
            start_line: None,
            end_line: None,
        },
        kind: ListEntryKind::File,
        range: ChipRange::new(2, 10),
    });
    assert_eq!(app.normalized_submission_text(), "aa @TEST.md bb");
}

#[test]
fn rendered_chip_clicks_map_to_attachment_completion() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.insert_text("@TEST.md ");
    app.confirm_attachment(0, 8, ListEntryKind::File);
    app.cursor = app.source_offset_at_rendered_offset(3);
    assert!(app.activate_chip_at_cursor());
    assert!(app.attachment_completion.is_some());
    assert_eq!(app.cursor, 8);
}

#[test]
fn composer_mouse_click_reopens_a_file_chip() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.insert_text("@TEST.md ");
    app.confirm_attachment(0, 8, ListEntryKind::File);
    let height = 24;
    let composer_top = height - app.controls_height(80, height);
    app.select_mouse_at(80, height, composer_top + 2, 3);
    assert!(app.attachment_completion.is_some());
    assert_eq!(app.cursor, 8);
}

#[test]
fn deleting_within_an_edited_chip_keeps_autocomplete_open() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.insert_text("@TEST.md ");
    app.confirm_attachment(0, 8, ListEntryKind::File);
    app.cursor = 8;
    assert!(app.activate_chip_at_cursor());
    app.attachment_completion
        .as_mut()
        .expect("chip edit opened completion")
        .rows
        .push(ListEntry {
            path: PathBuf::from("TEST.md"),
            kind: ListEntryKind::File,
        });
    app.backspace();
    assert_eq!(app.draft, "@TEST.m ");
    let completion = app
        .attachment_completion
        .as_ref()
        .expect("completion remains open");
    assert_eq!(completion.end, 7);
    assert_eq!(completion.rows.len(), 1);
}

#[test]
fn setup_surfaces_keep_padding_above_the_status_line() {
    let surfaces = [
        Surface::WebSearchSetup {
            provider: cagent_agent::web_search::WebSearchProvider::Exa,
            value: String::new(),
            cursor: 0,
        },
        Surface::Rename {
            title: "Conversation".into(),
            cursor: 12,
        },
        Surface::ProvidersRequired,
        Surface::ModelRequired,
        Surface::ModelsUnavailable,
    ];
    for surface in surfaces {
        let lines = surface_lines(&surface, Path::new("/tmp/project"), 80);
        assert_eq!(lines.last().expect("setup surface lines").width(), 0);
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn provider_and_model_surfaces_render_the_bottom_menu_contract() {
    let provider = Surface::Providers {
        rows: vec![cagent_agent::presentation::ProviderPickerRow {
            id: "mock".into(),
            label: "Mock".into(),
            status: "enabled".into(),
            setup_instructions: None,
            configuration_instructions: "Configure Mock".into(),
            credential_environment_variable: None,
            enabled: true,
            managed_auth: false,
            api_key_auth: false,
            has_managed_api_key: false,
            auth_flows: Vec::new(),
        }],
        list: ListState::selectable(2),
        query: String::new(),
        query_cursor: 0,
        model_configuration_follows: false,
    };
    let provider_text = surface_lines(&provider, Path::new("/tmp/project"), 80)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(provider_text.contains("Providers"));
    assert!(provider_text.contains("Mock"));
    assert_eq!(
        surface_status(&provider),
        "↑/↓ navigate · Enter close · Esc back"
    );

    let provider_row = Surface::Providers {
        rows: vec![cagent_agent::presentation::ProviderPickerRow {
            id: "mock".into(),
            label: "Mock".into(),
            status: "enabled".into(),
            setup_instructions: None,
            configuration_instructions: "Configure Mock".into(),
            credential_environment_variable: None,
            enabled: true,
            managed_auth: false,
            api_key_auth: false,
            has_managed_api_key: false,
            auth_flows: Vec::new(),
        }],
        list: ListState {
            selected: Some(1),
            offset: 0,
            item_count: 2,
        },
        query: String::new(),
        query_cursor: 0,
        model_configuration_follows: false,
    };
    assert_eq!(
        surface_status(&provider_row),
        "↑/↓ navigate · Enter toggle · r reconfigure · Esc back"
    );

    let connected_subscription = Surface::Providers {
        rows: vec![cagent_agent::presentation::ProviderPickerRow {
            id: "chatgpt".into(),
            label: "ChatGPT subscription".into(),
            status: "enabled".into(),
            setup_instructions: None,
            configuration_instructions: "Connect ChatGPT".into(),
            credential_environment_variable: None,
            enabled: true,
            managed_auth: true,
            api_key_auth: false,
            has_managed_api_key: false,
            auth_flows: vec![
                cagent_agent::provider::AuthFlow::BrowserPkce,
                cagent_agent::provider::AuthFlow::DeviceCode,
            ],
        }],
        list: ListState {
            selected: Some(1),
            offset: 0,
            item_count: 2,
        },
        query: String::new(),
        query_cursor: 0,
        model_configuration_follows: false,
    };
    assert_eq!(
        surface_status(&connected_subscription),
        "↑/↓ navigate · Enter toggle · r reconnect · d disconnect · Esc back"
    );

    let missing_subscription = Surface::Providers {
        rows: vec![cagent_agent::presentation::ProviderPickerRow {
            id: "chatgpt".into(),
            label: "ChatGPT subscription".into(),
            status: "not setup".into(),
            setup_instructions: Some("Connect ChatGPT".into()),
            configuration_instructions: "Connect ChatGPT".into(),
            credential_environment_variable: None,
            enabled: false,
            managed_auth: true,
            api_key_auth: false,
            has_managed_api_key: false,
            auth_flows: vec![
                cagent_agent::provider::AuthFlow::BrowserPkce,
                cagent_agent::provider::AuthFlow::DeviceCode,
            ],
        }],
        list: ListState {
            selected: Some(1),
            offset: 0,
            item_count: 2,
        },
        query: String::new(),
        query_cursor: 0,
        model_configuration_follows: false,
    };
    assert_eq!(
        surface_status(&missing_subscription),
        "↑/↓ navigate · Enter configure · Esc back"
    );

    let disabled_continue = Surface::Providers {
        rows: Vec::new(),
        list: ListState::selectable(1),
        query: String::new(),
        query_cursor: 0,
        model_configuration_follows: true,
    };
    assert_eq!(
        surface_status(&disabled_continue),
        "↑/↓ navigate · Esc back"
    );

    let enabled_continue = Surface::Providers {
        rows: vec![cagent_agent::presentation::ProviderPickerRow {
            id: "mock".into(),
            label: "Mock".into(),
            status: "enabled".into(),
            setup_instructions: None,
            configuration_instructions: "Configure Mock".into(),
            credential_environment_variable: None,
            enabled: true,
            managed_auth: false,
            api_key_auth: false,
            has_managed_api_key: false,
            auth_flows: Vec::new(),
        }],
        list: ListState::selectable(2),
        query: String::new(),
        query_cursor: 0,
        model_configuration_follows: true,
    };
    let enabled_continue_text = surface_lines(&enabled_continue, Path::new("/tmp/project"), 80)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(enabled_continue_text.contains("Continue"));
    assert_eq!(
        surface_status(&enabled_continue),
        "↑/↓ navigate · Enter continue · Esc back"
    );

    let setup = Surface::ProviderSetup {
        provider_id: "openai".into(),
        provider: "OpenAI".into(),
        instructions: "Set OPENAI_API_KEY in the environment, then restart Cagent.".into(),
        credential_environment_variable: Some("OPENAI_API_KEY".into()),
        auth_challenge: None,
        managed_auth: false,
        api_key_auth: false,
        api_key: String::new(),
        api_key_cursor: 0,
        auth_flows: Vec::new(),
        authenticated: false,
    };
    let setup_text = surface_lines(&setup, Path::new("/tmp/project"), 80)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(setup_text.contains("Configure provider"));
    assert!(setup_text.contains("Set OPENAI_API_KEY"));
    assert!(!setup_text.contains("reopen"));
    assert_eq!(
        surface_lines(&setup, Path::new("/tmp/project"), 80).len(),
        4
    );
    assert_eq!(
        surface_lines(&setup, Path::new("/tmp/project"), 80)[2].spans[1].style,
        ACCENT_STYLE
    );
    assert_eq!(
        surface_lines(&setup, Path::new("/tmp/project"), 80)[3].width(),
        0
    );
    assert_eq!(surface_status(&setup), "Enter/Esc back");

    let credential = "token-e\u{301}-with-a-long-secret";
    let credential_setup = Surface::ProviderSetup {
        provider_id: "openai".into(),
        provider: "OpenAI".into(),
        instructions: String::new(),
        credential_environment_variable: None,
        auth_challenge: None,
        managed_auth: false,
        api_key_auth: true,
        api_key: credential.into(),
        api_key_cursor: credential.len(),
        auth_flows: Vec::new(),
        authenticated: false,
    };
    let credential_lines = surface_lines(&credential_setup, Path::new("/tmp/project"), 20);
    assert!(credential_lines[2].width() <= 20);
    assert!(
        credential_lines[2]
            .spans
            .iter()
            .any(|span| span.style == SEARCH_CURSOR_STYLE)
    );

    let device_only_setup = Surface::ProviderSetup {
        provider_id: "github-copilot".into(),
        provider: "GitHub Copilot".into(),
        instructions: "Connect GitHub Copilot".into(),
        credential_environment_variable: None,
        auth_challenge: None,
        managed_auth: true,
        api_key_auth: false,
        api_key: String::new(),
        api_key_cursor: 0,
        auth_flows: vec![cagent_agent::provider::AuthFlow::DeviceCode],
        authenticated: false,
    };
    assert_eq!(
        surface_status(&device_only_setup),
        "Enter device login · Esc back"
    );

    let copilot_device_setup = Surface::ProviderSetup {
        provider_id: "github-copilot".into(),
        provider: "GitHub Copilot".into(),
        instructions: "Connect GitHub Copilot".into(),
        credential_environment_variable: None,
        auth_challenge: Some(cagent_agent::provider::AuthChallenge::Device {
            verification_url: "https://github.com/login/device".into(),
            user_code: "ABCD-EFGH".into(),
            device_code: "secret-device-code".into(),
            interval_seconds: 5,
        }),
        managed_auth: true,
        api_key_auth: false,
        api_key: String::new(),
        api_key_cursor: 0,
        auth_flows: vec![cagent_agent::provider::AuthFlow::DeviceCode],
        authenticated: false,
    };
    let copilot_device_lines = surface_lines(&copilot_device_setup, Path::new("/tmp/project"), 80);
    assert_eq!(copilot_device_lines.len(), 5);
    assert_eq!(
        copilot_device_lines[3].to_string(),
        "  This will update automatically a few moments after you sign in."
    );

    let device_setup = Surface::ProviderSetup {
        provider_id: "chatgpt".into(),
        provider: "ChatGPT subscription".into(),
        instructions: "Connect ChatGPT".into(),
        credential_environment_variable: None,
        auth_challenge: Some(cagent_agent::provider::AuthChallenge::Device {
            verification_url: "https://auth.openai.com/codex/device".into(),
            user_code: "ABCD-EFGH".into(),
            device_code: "secret-device-code".into(),
            interval_seconds: 5,
        }),
        managed_auth: true,
        api_key_auth: false,
        api_key: String::new(),
        api_key_cursor: 0,
        auth_flows: vec![
            cagent_agent::provider::AuthFlow::BrowserPkce,
            cagent_agent::provider::AuthFlow::DeviceCode,
        ],
        authenticated: false,
    };
    let device_lines = surface_lines(&device_setup, Path::new("/tmp/project"), 80);
    assert_eq!(device_lines.len(), 4);
    assert_eq!(
        device_lines[2].spans[1].content,
        "https://auth.openai.com/codex/device"
    );
    assert!(
        device_lines[2].spans[1]
            .style
            .add_modifier
            .contains(ratatui::style::Modifier::UNDERLINED)
    );
    assert_eq!(device_lines[2].spans[3].content, "ABCD-EFGH");
    assert_eq!(device_lines[2].spans[3].style, ACCENT_STYLE);
    assert_eq!(device_lines[3].width(), 0);
    assert_eq!(
        surface_status(&device_setup),
        "Enter browser login · c copy URL · C copy code · d device login · Esc back"
    );

    let model = Surface::Models {
        rows: vec![
            ModelPickerRow {
                provider: "openrouter".into(),
                id: "openai/gpt-5.6-luna".into(),
                label: "OpenAI: GPT-5.6 Luna".into(),
                efforts: vec!["high".into()],
                reasoning_control: Some(cagent_agent::provider::ReasoningControl::Effort),
                badges: vec!["ctx:1m".into()],
                current: true,
                favourite: true,
                release_date: None,
            },
            ModelPickerRow {
                provider: "anthropic".into(),
                id: "claude-sonnet-5".into(),
                label: "Claude Sonnet 5".into(),
                efforts: vec!["high".into()],
                reasoning_control: Some(cagent_agent::provider::ReasoningControl::Effort),
                badges: vec!["ctx:1m".into()],
                current: false,
                favourite: true,
                release_date: None,
            },
        ],
        list: ListState::selectable(2),
        query: String::new(),
        query_cursor: 0,
        selection_target: ModelSelectionTarget::Conversation,
    };
    let model_text = surface_lines(&model, Path::new("/tmp/project"), 80)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(model_text.contains("Models"));
    assert!(
        model_text.contains(
            "OpenAI: GPT-5.6 Luna  openrouter (openai/gpt-5.6-luna) · ctx:1m · current ★"
        )
    );
    let model_line = &surface_lines(&model, Path::new("/tmp/project"), 80)[2];
    let current = &model_line.spans[model_line.spans.len() - 3];
    assert_eq!(current.content, "current");
    assert_eq!(current.style, ratatui::style::Style::default());
    let separator = &model_line.spans[model_line.spans.len() - 4];
    assert_eq!(separator.content, " · ");
    assert_eq!(separator.style, DIM_STYLE);
    let star = model_line.spans.last().unwrap();
    assert_eq!(star.content, "★");
    assert_eq!(star.style, NOTICE_STYLE);
    let favourite_line = &surface_lines(&model, Path::new("/tmp/project"), 80)[3];
    assert_eq!(favourite_line.spans.last().unwrap().content, "★");
    assert_eq!(favourite_line.spans.last().unwrap().style, NOTICE_STYLE);
    assert_eq!(
        surface_status(&model),
        "↑/↓ navigate · Enter select · f favourite · Ctrl+D agent default · Esc back"
    );
}

#[test]
fn filtered_picker_surfaces_show_a_dim_empty_state_and_cursor() {
    let provider = Surface::Providers {
        rows: vec![cagent_agent::presentation::ProviderPickerRow {
            id: "mock".into(),
            label: "Mock".into(),
            status: "enabled".into(),
            setup_instructions: None,
            configuration_instructions: "Configure Mock".into(),
            credential_environment_variable: None,
            enabled: true,
            managed_auth: false,
            api_key_auth: false,
            has_managed_api_key: false,
            auth_flows: Vec::new(),
        }],
        list: ListState::selectable(2),
        query: "missing".into(),
        query_cursor: 3,
        model_configuration_follows: false,
    };
    let provider_lines = surface_lines(&provider, Path::new("/tmp/project"), 80);
    assert!(provider_lines.iter().any(|line| {
        line.to_string() == "  No providers match your search." && line.style == DIM_STYLE
    }));
    assert!(provider_lines[0].to_string().contains("search: missing"));
    assert!(
        provider_lines[0]
            .spans
            .iter()
            .any(|span| span.style == SEARCH_CURSOR_STYLE)
    );
    assert_eq!(provider_lines.last().expect("provider lines").width(), 0);

    let model = Surface::Models {
        rows: vec![ModelPickerRow {
            provider: "mock".into(),
            id: "echo".into(),
            label: "Echo".into(),
            efforts: Vec::new(),
            reasoning_control: None,
            badges: vec!["tools".into()],
            current: false,
            favourite: false,
            release_date: None,
        }],
        list: ListState::selectable(1),
        query: "missing".into(),
        query_cursor: 7,
        selection_target: ModelSelectionTarget::Conversation,
    };
    let model_lines = surface_lines(&model, Path::new("/tmp/project"), 80);
    assert_eq!(model_lines.last().expect("model lines").width(), 0);
    let model_text = model_lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(model_text.contains("No models match your search."));
    assert!(!model_text.contains("all providers"));
}

#[test]
fn history_scroll_is_clamped_and_tail_following_restored() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.follow_history_tail = false;
    app.history_scroll = 100;
    app.scroll_history_up(10);
    assert_eq!(app.history_scroll, 90);
    app.history_scroll = 0;
    assert!(!app.follow_history_tail);
}

#[test]
fn scrolling_up_an_empty_conversation_keeps_tail_status() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );

    app.scroll_history_up(3);

    assert!(app.follow_history_tail);
    assert!(
        !app.status_line(80)
            .to_string()
            .contains("navigate messages")
    );
}

#[test]
fn scrolling_up_streaming_content_stops_tail_following_before_it_overflows() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.streaming_source = "A response that still fits in the viewport.".into();
    app.history_scroll = 0;

    app.scroll_history_up(3);

    assert_eq!(app.history_scroll, 0);
    assert!(!app.follow_history_tail);
}

#[test]
fn transcript_prefetch_consumes_one_upward_intent_until_a_later_gesture() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    let cursor = || -> cagent_agent::protocol::TranscriptCursor {
        serde_json::from_value(serde_json::json!({
            "conversation_id": cagent_agent::protocol::ConversationId::new(),
            "older_node_id": cagent_agent::protocol::NodeId::new(),
            "newer_node_id": cagent_agent::protocol::NodeId::new(),
        }))
        .unwrap()
    };
    app.transcript_older = Some(cursor());
    app.transcript_viewport_height = 10;
    app.history_scroll = 20;

    // Frontend-local lazy layout is completed before another durable page is
    // requested; it neither consumes nor creates upward intent.
    app.history_layout.start_block = 1;
    assert!(app.take_transcript_page_request().is_none());
    assert!(app.transcript_prefetch_armed);
    app.history_layout.start_block = 0;

    assert!(app.take_transcript_page_request().is_some());
    assert!(app.transcript_page_request.is_some());
    assert!(!app.transcript_prefetch_armed);

    // Page completion and prepend/render reconciliation do not create intent.
    let requested = app.transcript_page_request.clone().unwrap();
    assert!(app.finish_transcript_page_request(&requested));
    app.transcript_older = Some(cursor());
    assert!(app.take_transcript_page_request().is_none());

    // A later PageUp gesture permits exactly the next page.
    app.scroll_history_up(10);
    assert!(app.transcript_prefetch_armed);
    assert!(app.take_transcript_page_request().is_some());
    assert!(app.take_transcript_page_request().is_none());
}

#[test]
fn transcript_prefetch_requires_proximity_and_ignores_input_during_io() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    app.transcript_older = Some(
        serde_json::from_value(serde_json::json!({
            "conversation_id": cagent_agent::protocol::ConversationId::new(),
            "older_node_id": cagent_agent::protocol::NodeId::new(),
            "newer_node_id": cagent_agent::protocol::NodeId::new(),
        }))
        .unwrap(),
    );
    app.transcript_viewport_height = 10;
    app.history_scroll = 21;
    assert!(app.take_transcript_page_request().is_none());

    app.transcript_prefetch_armed = false;
    app.handle_mouse_scroll_within(
        MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        },
        80,
        24,
    );
    assert!(app.transcript_prefetch_armed);
    assert!(app.take_transcript_page_request().is_some());

    // Neither PageUp nor wheel-up queues another request while this one is active.
    app.transcript_prefetch_armed = false;
    app.scroll_history_up(10);
    app.handle_mouse_scroll_within(
        MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        },
        80,
        24,
    );
    assert!(!app.transcript_prefetch_armed);
    let requested = app.transcript_page_request.clone().unwrap();
    assert!(app.finish_transcript_page_request(&requested));
    assert!(app.take_transcript_page_request().is_none());
}

#[test]
fn stale_transcript_page_completion_cannot_clear_the_active_request() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    let cursor = || -> cagent_agent::protocol::TranscriptCursor {
        serde_json::from_value(serde_json::json!({
            "conversation_id": cagent_agent::protocol::ConversationId::new(),
            "older_node_id": cagent_agent::protocol::NodeId::new(),
            "newer_node_id": cagent_agent::protocol::NodeId::new(),
        }))
        .unwrap()
    };
    let stale = cursor();
    let active = cursor();
    app.transcript_page_request = Some(active.clone());

    assert!(!app.finish_transcript_page_request(&stale));
    assert_eq!(app.transcript_page_request.as_ref(), Some(&active));
    assert!(app.finish_transcript_page_request(&active));
    assert!(app.transcript_page_request.is_none());
}

#[test]
fn home_drains_outside_the_prefetch_gate_and_end_cancels_it() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    app.transcript_older = Some(
        serde_json::from_value(serde_json::json!({
            "conversation_id": cagent_agent::protocol::ConversationId::new(),
            "older_node_id": cagent_agent::protocol::NodeId::new(),
            "newer_node_id": cagent_agent::protocol::NodeId::new(),
        }))
        .unwrap(),
    );
    app.transcript_viewport_height = 10;
    app.history_scroll = 100;
    app.transcript_prefetch_armed = false;

    app.scroll_history_to_top();
    assert!(app.take_transcript_page_request().is_some());
    let requested = app.transcript_page_request.clone().unwrap();
    assert!(app.finish_transcript_page_request(&requested));
    assert!(app.take_transcript_page_request().is_some());

    let requested = app.transcript_page_request.clone().unwrap();
    assert!(app.finish_transcript_page_request(&requested));
    app.scroll_history_to_end();
    assert!(!app.transcript_home_drain);
    assert!(app.take_transcript_page_request().is_none());
}

#[test]
fn transcript_scrollbar_track_clicks_and_thumb_drags_update_history() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    app.transcript_scrollbar =
        TranscriptScrollbarLayout::new(ratatui::layout::Rect::new(0, 2, 80, 10), 100, 0);
    let mouse = |kind, row| MouseEvent {
        kind,
        column: 79,
        row,
        modifiers: KeyModifiers::NONE,
    };

    assert!(
        app.handle_transcript_scrollbar_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 7,))
    );
    let clicked = app.history_scroll;
    assert!(clicked > 0);
    assert!(app.transcript_scrollbar_drag.is_some());

    assert!(
        app.handle_transcript_scrollbar_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 10,))
    );
    assert!(app.history_scroll > clicked);
    assert!(
        app.handle_transcript_scrollbar_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 10,))
    );
    assert!(app.transcript_scrollbar_drag.is_none());
}

#[test]
fn transcript_scrollbar_only_upward_gestures_arm_one_prefetch() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    let mouse = |kind, row| MouseEvent {
        kind,
        column: 79,
        row,
        modifiers: KeyModifiers::NONE,
    };

    // A downward track click changes the offset but does not arm prefetch.
    app.transcript_prefetch_armed = false;
    app.transcript_scrollbar =
        TranscriptScrollbarLayout::new(ratatui::layout::Rect::new(0, 2, 80, 10), 100, 50);
    assert!(
        app.handle_transcript_scrollbar_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 10,))
    );
    assert!(!app.transcript_prefetch_armed);
    app.handle_transcript_scrollbar_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 10));

    // An upward track click is a fresh gesture.
    app.history_scroll = 50;
    app.transcript_scrollbar =
        TranscriptScrollbarLayout::new(ratatui::layout::Rect::new(0, 2, 80, 10), 100, 50);
    assert!(
        app.handle_transcript_scrollbar_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 5,))
    );
    assert!(app.transcript_prefetch_armed);
    assert!(
        app.transcript_scrollbar_drag
            .is_some_and(|drag| drag.prefetch_seen)
    );
    app.handle_transcript_scrollbar_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 5));

    // Pressing the thumb without moving is stationary and does not arm.
    app.transcript_prefetch_armed = false;
    app.history_scroll = 50;
    app.transcript_scrollbar =
        TranscriptScrollbarLayout::new(ratatui::layout::Rect::new(0, 2, 80, 10), 100, 50);
    assert!(
        app.handle_transcript_scrollbar_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 7,))
    );
    assert!(!app.transcript_prefetch_armed);

    // The first upward movement arms; later movement in the same drag cannot
    // re-arm after that request is consumed, even if its I/O has completed.
    assert!(
        app.handle_transcript_scrollbar_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 6,))
    );
    assert!(app.transcript_prefetch_armed);
    app.transcript_prefetch_armed = false;
    app.transcript_page_request = Some(
        serde_json::from_value(serde_json::json!({
            "conversation_id": cagent_agent::protocol::ConversationId::new(),
            "older_node_id": cagent_agent::protocol::NodeId::new(),
            "newer_node_id": cagent_agent::protocol::NodeId::new(),
        }))
        .unwrap(),
    );
    app.handle_transcript_scrollbar_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 5));
    app.transcript_page_request = None;
    app.handle_transcript_scrollbar_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 4));
    assert!(!app.transcript_prefetch_armed);
    app.handle_transcript_scrollbar_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 4));
    assert!(app.transcript_scrollbar_drag.is_none());
}

#[test]
fn clicking_transcript_scrollbar_older_indicator_drains_history_to_the_start() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    app.history_scroll = 50;
    app.follow_history_tail = true;
    app.transcript_older = Some(
        serde_json::from_value(serde_json::json!({
            "conversation_id": cagent_agent::protocol::ConversationId::new(),
            "older_node_id": cagent_agent::protocol::NodeId::new(),
            "newer_node_id": cagent_agent::protocol::NodeId::new(),
        }))
        .unwrap(),
    );
    app.transcript_scrollbar =
        TranscriptScrollbarLayout::new(ratatui::layout::Rect::new(0, 2, 80, 10), 100, 50);

    assert!(app.handle_transcript_scrollbar_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 79,
        row: 2,
        modifiers: KeyModifiers::NONE,
    }));
    assert_eq!(app.history_scroll, 0);
    assert!(!app.follow_history_tail);
    assert!(app.transcript_home_drain);
    assert!(app.transcript_scrollbar_drag.is_none());
}

#[test]
fn message_navigation_moves_between_user_cards_and_returns_to_the_tail() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.push_user_with_label(None, "first", &[]);
    app.push_assistant(
        cagent_agent::presentation::parse_markdown("first answer"),
        "first answer".into(),
        None,
    );
    app.push_user_with_label(None, "second", &[]);

    assert!(app.navigate_user_message(false));
    let second = app.history_scroll;
    assert!(!app.follow_history_tail);

    assert!(app.navigate_user_message(false));
    let first = app.history_scroll;
    assert!(first < second);

    assert!(app.navigate_user_message(true));
    assert_eq!(app.history_scroll, second);
    assert!(!app.follow_history_tail);

    assert!(app.navigate_user_message(true));
    assert!(app.follow_history_tail);
    assert!(
        !app.status_line(80)
            .to_string()
            .contains("navigate messages")
    );
}

#[test]
fn scrolling_replaces_background_and_queue_hints_with_message_navigation() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.follow_history_tail = false;
    app.supervised_work
        .rows
        .push(cagent_agent::presentation::SupervisedWork::Terminal {
            terminal: Box::new(cagent_agent::tools::TerminalSnapshot {
                id: cagent_agent::tools::TerminalId::new(),
                owner: cagent_agent::protocol::ConversationId::new(),
                owner_agent_run_id: None,
                tool_call_node_id: None,
                read_safe: Some(false),
                command: "sleep 1".into(),
                status: cagent_agent::tools::TerminalStatus::Running,
                created_at: "0".into(),
                started_at: "0".into(),
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

    let status = app.status_line(80).to_string();
    assert!(status.contains("Ctrl+↑/↓ navigate messages"));
    assert!(!status.contains("Alt+↓ 1 background"));
}

#[test]
fn scrolling_to_transcript_block_leaves_one_row_of_top_context() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.render_width = 40;
    app.push_user_with_label(None, "first", &[]);
    app.push_user_with_label(None, "second message wraps across some terminal rows", &[]);
    let target = app.history[1].id.clone();
    let offset = app.transcript_block_offset(&target).unwrap();

    app.begin_scroll_to_transcript_block(target);

    assert_eq!(app.history_scroll, offset.saturating_sub(1));
    assert!(!app.follow_history_tail);
    assert!(app.pending_transcript_scroll_target.is_none());
}

#[test]
fn clicking_the_scroll_indicator_restores_tail_following() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.follow_history_tail = false;
    let height = 24;
    let row = height - app.controls_height(80, height);

    assert!(!app.scroll_indicator_clicked(80, height, row, 1));
    assert!(app.scroll_indicator_clicked(80, height, row, 2));
    assert!(app.follow_history_tail);
}

#[test]
fn history_blocks_have_one_blank_row_between_them() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );

    app.push_user_with_label(None, "hello", &[]);
    app.push_assistant(
        cagent_agent::presentation::parse_markdown("answer"),
        "answer".into(),
        None,
    );
    app.push_system_message("model changed");
    app.welcome.clear();
    app.ensure_history_layout(80);
    let rows = app
        .history_layout
        .rendered
        .as_ref()
        .unwrap()
        .rows
        .iter()
        .map(|row| row.to_string().trim_end().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        rows,
        vec!["", "› hello", "", "", "• answer", "", "• model changed",]
    );
}

#[test]
fn delegated_activity_and_assistant_reply_have_a_dim_separator_between_them() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.history.push(test_tool_groups(vec![
        cagent_agent::presentation::ToolActivityGroup::Delegate {
            action: "Finished".into(),
            profile: "General".into(),
            task: Some("Run `cargo test`.".into()),
            status: cagent_agent::presentation::ToolActivityStatus::Succeeded,
        },
    ]));
    app.push_assistant(
        cagent_agent::presentation::parse_markdown("cargo test did not pass."),
        "cargo test did not pass.".into(),
        None,
    );
    app.welcome.clear();
    app.ensure_history_layout(80);

    let rows = app
        .history_layout
        .rendered
        .as_ref()
        .unwrap()
        .rows
        .iter()
        .map(|row| row.to_string().trim_end().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        rows,
        vec![
            "• Finished General sub-agent".to_owned(),
            "  └ Run `cargo test`.".to_owned(),
            "".to_owned(),
            "─".repeat(80),
            "".to_owned(),
            "• cargo test did not pass.".to_owned(),
        ]
    );
}

#[test]
fn spawned_delegation_keeps_the_activity_animation_active() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.history.push(test_tool_groups(vec![
        cagent_agent::presentation::ToolActivityGroup::Delegate {
            action: "Running".into(),
            profile: "Explore".into(),
            task: None,
            status: cagent_agent::presentation::ToolActivityStatus::Pending,
        },
    ]));

    assert!(
        !app.active,
        "background delegation pulses with an idle parent"
    );
    assert!(app.bash_activity_animation_visible());
    app.ensure_history_layout(80);
    assert!(app.history_layout.rendered.is_some());
    app.invalidate_activity_layout();
    assert!(app.history_layout.rendered.is_none());

    let cagent_agent::protocol::TranscriptBlockKind::ToolGroups { groups } =
        &mut app.history[0].kind
    else {
        panic!("expected delegation activity");
    };
    let cagent_agent::presentation::ToolActivityGroup::Delegate { action, status, .. } =
        &mut groups[0]
    else {
        panic!("expected delegation group");
    };
    *action = "Finished".into();
    *status = cagent_agent::presentation::ToolActivityStatus::Succeeded;
    assert!(!app.bash_activity_animation_visible());
    app.ensure_history_layout(80);
    app.invalidate_activity_layout();
    assert!(app.history_layout.rendered.is_some());
}

#[test]
fn pending_web_activity_keeps_the_activity_animation_active() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.history.push(test_tool_groups(vec![
        cagent_agent::presentation::ToolActivityGroup::WebSearch {
            provider: "exa".into(),
            query: "rust tui".into(),
            status: cagent_agent::presentation::ToolActivityStatus::Pending,
            result_count: None,
            results: Vec::new(),
        },
        cagent_agent::presentation::ToolActivityGroup::WebFetch {
            node_id: None,
            url: "https://example.com".into(),
            redirected_url: None,
            format: cagent_agent::WebFetchFormat::Markdown,
            content_type: None,
            status: cagent_agent::presentation::ToolActivityStatus::Pending,
            output: None,
        },
    ]));

    assert!(app.bash_activity_animation_visible());
    app.ensure_history_layout(80);
    assert!(app.history_layout.rendered.is_some());
    app.invalidate_activity_layout();
    assert!(app.history_layout.rendered.is_none());
}

#[test]
fn activity_animation_evicts_only_pending_activity_block_layouts() {
    let static_id =
        cagent_agent::protocol::TranscriptBlockId::node(cagent_agent::protocol::NodeId::new());
    let active_id = cagent_agent::protocol::TranscriptBlockId::derived(
        "tools",
        cagent_agent::protocol::NodeId::new(),
    );
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.history = vec![
        test_lines(vec![Line::from("completed activity")]),
        test_tool_groups(vec![
            cagent_agent::presentation::ToolActivityGroup::Bash {
                node_id: None,
                terminal_id: None,
                command: "sleep 1".into(),
                status: cagent_agent::presentation::ToolActivityStatus::Pending,
                output: None,
                ansi_output: None,
                exit_code: None,
            },
            cagent_agent::presentation::ToolActivityGroup::Delegate {
                action: "Running".into(),
                profile: "Explore".into(),
                task: None,
                status: cagent_agent::presentation::ToolActivityStatus::Pending,
            },
        ]),
    ];
    app.history[0].id = static_id.clone();
    app.history[1].id = active_id.clone();

    app.ensure_history_layout(80);
    assert_eq!(app.history_block_layouts.len(), 2);

    app.invalidate_activity_layout();

    assert!(app.history_layout.rendered.is_none());
    assert_eq!(app.history_block_layouts.len(), 1);
    assert!(
        app.history_block_layouts
            .keys()
            .any(|(id, _)| id == &static_id)
    );

    app.ensure_history_layout(80);
    assert_eq!(app.history_block_layouts.len(), 2);
    assert!(
        app.history_block_layouts
            .keys()
            .any(|(id, _)| id == &active_id)
    );
}

#[test]
fn permission_denial_history_row_is_red_and_explains_the_denied_resource() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );

    app.push_permission_denied("edit src/main.rs", "workspace is read-only");

    let lines = crate::render::transcript::history_item_rows(&app.history[0], &app.workspace, 80);
    let line = &lines[0];
    assert_eq!(
        line.to_string(),
        "• Permission denied: edit src/main.rs · workspace is read-only"
    );
    assert_eq!(line.line.spans[0].style, ERROR_STYLE);
    assert_eq!(line.line.spans[1].style, Style::default());
    assert_eq!(line.line.spans[2].style, DIM_STYLE);

    app.push_permission_denied("run cargo test", "too broad\nlimit it to this crate");
    let lines = crate::render::transcript::history_item_rows(&app.history[1], &app.workspace, 80);
    assert_eq!(
        lines[0].to_string(),
        "• Permission denied: run cargo test · too broad"
    );
    assert_eq!(lines[1].to_string(), "  limit it to this crate");
    assert_eq!(lines[1].line.spans[1].style, DIM_STYLE);
}

#[test]
fn large_transcript_uses_only_the_visible_row_window() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.history = vec![test_lines(
        (0..10_000)
            .map(|index| Line::from(format!("history row {index}")))
            .collect(),
    )];
    app.welcome.clear();
    app.ensure_history_layout(80);
    let rows = &app
        .history_layout
        .rendered
        .as_ref()
        .expect("history layout is initialized")
        .rows;
    let mut offset = 5_000;
    let mut remaining = 12;
    let mut visible = Vec::new();
    append_visible_rows(rows, &mut offset, &mut remaining, &mut visible);

    assert_eq!(visible.len(), 12);
    assert_eq!(visible[0].to_string().trim(), "history row 5000");
    assert_eq!(visible[11].to_string().trim(), "history row 5011");
    assert_eq!(remaining, 0);
}

#[test]
fn history_tree_marks_the_selection_and_offers_contextual_fork_controls() {
    let root = cagent_agent::protocol::NodeId::new();
    let message = cagent_agent::protocol::NodeId::new();
    let nodes = vec![
        cagent_agent::protocol::HistoryNode {
            id: root,
            parent_id: None,
            turn_id: None,
            owner_id: None,
            request_index: None,
            kind: NodeKind::ConversationRoot,
            status: "completed".into(),
            role: None,
            summary: None,
            content: serde_json::json!({}),
            composer_text: None,
            created_at: "0".into(),
            completed_at: Some("0".into()),
            active: false,
        },
        cagent_agent::protocol::HistoryNode {
            id: message,
            parent_id: Some(root),
            turn_id: Some(cagent_agent::protocol::TurnId::new()),
            owner_id: None,
            request_index: None,
            kind: NodeKind::UserMessage,
            status: "completed".into(),
            role: Some("user".into()),
            summary: None,
            content: serde_json::json!({"text": format!("branch here {}", "x".repeat(100))}),
            composer_text: None,
            created_at: "1".into(),
            completed_at: Some("1".into()),
            active: true,
        },
    ];

    let browse_lines = history_tree_lines(&nodes, 1, TreePurpose::Browse, 40);
    assert!(browse_lines.iter().all(|line| line.width() <= 40));
    assert_eq!(browse_lines[3].style.fg, None);
    assert_eq!(browse_lines[3].spans[0].style.fg, None);
    assert_eq!(browse_lines[3].spans[1].style.fg, Some(Color::Green));
    assert!(
        browse_lines[3].spans[1]
            .style
            .add_modifier
            .contains(Modifier::BOLD)
    );
    let browse = browse_lines
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(browse.contains("branch here"));
    assert!(browse.contains("◉ user: branch here"));
    assert!(browse.contains('…'));
    assert!(browse.contains("○ root: conversation start"));
    assert!(!browse.contains("Add message"));
    assert!(!browse.contains("Enter switch to"));

    let mut assistant = nodes[1].clone();
    assistant.kind = NodeKind::AssistantMessage;
    assert_eq!(
        history_tree_node_style(&assistant),
        Some(Style::new().fg(Color::Indexed(214)))
    );
    assistant.content = serde_json::json!({
        "history_exploration": true,
        "history_tools": [{"name": "read"}],
    });
    assert_eq!(
        history_tree_node_style(&assistant),
        Some(Style::new().fg(Color::Yellow))
    );

    let mut edit_nodes = nodes.clone();
    edit_nodes[1].kind = NodeKind::AssistantMessage;
    edit_nodes[1].role = Some("assistant".into());
    edit_nodes[1].content = serde_json::json!({"summary": "Edit TEST.md"});
    for purpose in [TreePurpose::Browse, TreePurpose::Fork] {
        let rendered = history_tree_lines(&edit_nodes, 1, purpose, 40)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("◉ assistant: Edit TEST.md"));
    }

    let fork = history_tree_lines(&nodes, 1, TreePurpose::Fork, 40)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(fork.contains("Fork from previous message"));
    assert!(fork.contains("○ root: conversation start"));
    assert!(fork.contains("◉ user: branch here"));
    assert!(!fork.contains("Add message"));
    assert!(!fork.contains("Enter fork"));
}

#[test]
fn history_picker_list_fills_its_full_screen_viewport() {
    let root = cagent_agent::protocol::NodeId::new();
    let mut parent = root;
    let mut nodes = vec![cagent_agent::protocol::HistoryNode {
        id: root,
        parent_id: None,
        turn_id: None,
        owner_id: None,
        request_index: None,
        kind: NodeKind::ConversationRoot,
        status: "completed".into(),
        role: None,
        summary: None,
        content: serde_json::json!({}),
        composer_text: None,
        created_at: "0".into(),
        completed_at: Some("0".into()),
        active: false,
    }];
    for index in 1..30 {
        let id = cagent_agent::protocol::NodeId::new();
        nodes.push(cagent_agent::protocol::HistoryNode {
            id,
            parent_id: Some(parent),
            turn_id: Some(cagent_agent::protocol::TurnId::new()),
            owner_id: None,
            request_index: None,
            kind: NodeKind::UserMessage,
            status: "completed".into(),
            role: Some("user".into()),
            summary: None,
            content: serde_json::json!({"text": format!("message {index}")}),
            composer_text: None,
            created_at: index.to_string(),
            completed_at: Some(index.to_string()),
            active: index == 29,
        });
        parent = id;
    }
    let rows = cagent_agent::presentation::project_history_rows(&nodes);

    for purpose in [TreePurpose::Browse, TreePurpose::Fork] {
        let surface = Surface::HistoryTree {
            list: ListState::selectable(rows.len()),
            rows: rows.clone(),
            purpose,
            loading: false,
            revision: None,
            query: String::new(),
            query_cursor: 0,
            opened_at_millis: 0,
        };
        let layout = super::surfaces::surface_layout_with_viewport(
            &surface,
            Path::new("/workspace"),
            80,
            20,
        );

        assert_eq!(layout.lines.len(), 20);
        assert_eq!(
            layout
                .hits
                .iter()
                .filter(|hit| matches!(hit, ListHit::Item(_)))
                .count(),
            15
        );
    }
}

#[test]
fn history_picker_loading_state_fills_viewport_without_rows() {
    let surface = Surface::HistoryTree {
        list: ListState::selectable(0),
        rows: Vec::new(),
        purpose: TreePurpose::Browse,
        loading: true,
        revision: None,
        query: String::new(),
        query_cursor: 0,
        opened_at_millis: 0,
    };
    let layout =
        super::surfaces::surface_layout_with_viewport(&surface, Path::new("/workspace"), 80, 20);

    assert_eq!(layout.lines.len(), 20);
    assert!(
        layout
            .lines
            .iter()
            .any(|line| line.to_string() == "  Loading history…")
    );
}

#[test]
fn resume_picker_fills_viewport_with_filtered_results() {
    for query in ["", "no matches"] {
        let surface = Surface::Conversations {
            rows: Vec::new(),
            list: ListState::selectable(0),
            query: query.to_owned(),
            query_cursor: query.len(),
            opened_at_millis: 0,
            include_archived: false,
        };
        let layout = super::surfaces::surface_layout_with_viewport(
            &surface,
            Path::new("/workspace"),
            80,
            20,
        );

        assert_eq!(layout.lines.len(), 20);
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn history_tree_labels_a_clean_accepted_plan_as_its_boundary() {
    fn history_node(
        id: cagent_agent::protocol::NodeId,
        parent_id: Option<cagent_agent::protocol::NodeId>,
        kind: NodeKind,
        content: serde_json::Value,
        active: bool,
    ) -> cagent_agent::protocol::HistoryNode {
        cagent_agent::protocol::HistoryNode {
            id,
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
            created_at: String::new(),
            completed_at: Some(String::new()),
            active,
        }
    }

    let root = cagent_agent::protocol::NodeId::new();
    let request = cagent_agent::protocol::NodeId::new();
    let plan = cagent_agent::protocol::NodeId::new();
    let hidden = cagent_agent::protocol::NodeId::new();
    let non_clearing_hidden = cagent_agent::protocol::NodeId::new();
    let clean_user = cagent_agent::protocol::NodeId::new();
    let clean_assistant = cagent_agent::protocol::NodeId::new();
    let fork_user = cagent_agent::protocol::NodeId::new();
    let exploration = cagent_agent::protocol::NodeId::new();
    let fork_assistant = cagent_agent::protocol::NodeId::new();
    let nodes = vec![
        history_node(
            root,
            None,
            NodeKind::ConversationRoot,
            serde_json::json!({}),
            false,
        ),
        history_node(
            request,
            Some(root),
            NodeKind::UserMessage,
            serde_json::json!({"text": "propose a plan with just \"say hi\""}),
            false,
        ),
        history_node(
            plan,
            Some(request),
            NodeKind::AssistantMessage,
            serde_json::json!({"text": "say hi", "flavor": "plan"}),
            false,
        ),
        history_node(
            hidden,
            Some(plan),
            NodeKind::System,
            serde_json::json!({
                "transcript": "hidden",
                "message": "Implement the following plan."
            }),
            false,
        ),
        history_node(
            clean_user,
            Some(hidden),
            NodeKind::AcceptedPlan,
            serde_json::json!({"plan_markdown": "# Say hi", "reset_context": true}),
            false,
        ),
        history_node(
            clean_assistant,
            Some(clean_user),
            NodeKind::AssistantMessage,
            serde_json::json!({"text": "Hi!"}),
            false,
        ),
        history_node(
            non_clearing_hidden,
            Some(plan),
            NodeKind::System,
            serde_json::json!({
                "transcript": "hidden",
                "message": "Implement the following plan."
            }),
            false,
        ),
        history_node(
            fork_user,
            Some(non_clearing_hidden),
            NodeKind::AcceptedPlan,
            serde_json::json!({
                "plan_markdown": "# Say hi",
                "reset_context": false,
                "compact_context": true
            }),
            false,
        ),
        history_node(
            exploration,
            Some(fork_user),
            NodeKind::AssistantMessage,
            serde_json::json!({"summary": "List .", "history_exploration": true}),
            false,
        ),
        history_node(
            fork_assistant,
            Some(exploration),
            NodeKind::AssistantMessage,
            serde_json::json!({"text": "hi"}),
            true,
        ),
    ];

    let rendered = history_tree_lines(&nodes, 4, TreePurpose::Browse, 80)
        .into_iter()
        .skip(2)
        .map(|line| line.to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        rendered,
        [
            "○ root: conversation start",
            "○ user: propose a plan with just \"say hi\"",
            "○ plan: say hi",
            "│",
            "├─ ○ accepted plan (clear): Say hi",
            "│  ◉ assistant: Hi!",
            "│",
            "└─ ○ accepted plan (compact): Say hi",
            "   ○ explore: List .",
            "   ● assistant: hi",
        ]
    );
}

#[test]
fn supervised_wait_turn_uses_waiting_indicator_label() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    let mut snapshot = session_snapshot_with_interaction(None);
    snapshot.turn = cagent_agent::protocol::TurnState::Waiting {
        started_at: "0".into(),
        turn_id: cagent_agent::protocol::TurnId::new(),
    };

    app.apply_session_snapshot(&snapshot);

    assert!(app.waiting_for_work);
    let line = crate::render::working_line(
        app.working_started_at,
        app.last_activity_at,
        app.thinking,
        app.waiting_for_work,
        app.pending_interaction.is_some(),
        app.compacting,
        0,
        None,
    )
    .to_string();
    assert!(line.contains("Waiting"));
    assert!(!line.contains("Working"));
}

#[test]
fn clicking_working_task_count_opens_background_work() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.working_tasks_hit = Some(WorkingTasksHit {
        row: 5,
        columns: 20..27,
    });

    app.select_mouse_at(80, 24, 5, 22);

    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::SupervisedWork { .. })
    ));
}

#[test]
fn compacting_turn_uses_compacting_indicator_label() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    let mut snapshot = session_snapshot_with_interaction(None);
    snapshot.turn = cagent_agent::protocol::TurnState::Compacting {
        started_at: "0".into(),
        turn_id: cagent_agent::protocol::TurnId::new(),
    };

    app.apply_session_snapshot(&snapshot);

    assert!(app.compacting);
    let line = crate::render::working_line(
        app.working_started_at,
        app.last_activity_at,
        app.thinking,
        app.waiting_for_work,
        app.pending_interaction.is_some(),
        app.compacting,
        0,
        None,
    )
    .to_string();
    assert!(line.contains("Compacting"));
    assert!(!line.contains("Working"));
}

#[test]
fn configured_path_actions_disable_launch_or_open_the_builtin_view() {
    let temporary = tempfile::tempdir().unwrap();
    let file = temporary.path().join("main.rs");
    std::fs::write(&file, "fn main() {}\n").unwrap();
    let mut app = App::new(
        temporary.path(),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );

    app.ui_editor = cagent_agent::config::UiEditor::Disabled;
    app.open_path(file.clone());
    assert!(app.surfaces.is_empty());
    assert!(app.pending_action.is_none());

    app.ui_editor = cagent_agent::config::UiEditor::Command {
        command: vec!["zed-preview".into(), "-w".into()],
        line_command: None,
        fallback_executables: Vec::new(),
        mode: cagent_agent::config::UiEditorMode::Background,
    };
    app.open_path(file.clone());
    assert_eq!(
        app.pending_action.take(),
        Some(AppAction::LaunchPath {
            argv_candidates: vec![vec![
                "zed-preview".into(),
                "-w".into(),
                file.to_string_lossy().into_owned(),
            ]],
            mode: cagent_agent::config::UiEditorMode::Background,
            reload_startup_resources: false,
        })
    );

    app.ui_editor = cagent_agent::config::UiEditor::Command {
        command: vec!["nvim".into()],
        line_command: None,
        fallback_executables: Vec::new(),
        mode: cagent_agent::config::UiEditorMode::Foreground,
    };
    app.open_path(file.clone());
    assert_eq!(
        app.pending_action.take(),
        Some(AppAction::LaunchPath {
            argv_candidates: vec![vec!["nvim".into(), file.to_string_lossy().into_owned()]],
            mode: cagent_agent::config::UiEditorMode::Foreground,
            reload_startup_resources: false,
        })
    );

    app.ui_editor = cagent_agent::config::UiEditor::BuiltIn;
    app.open_path(file);
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Expanded {
            view: ExpandedView::File { view },
            ..
        }) if matches!(view.content, cagent_agent::presentation::FileViewContent::Text { .. })
    ));
}

#[test]
fn path_reference_lines_drive_external_argv_and_builtin_file_scroll() {
    let temporary = tempfile::tempdir().unwrap();
    let file = temporary.path().join("main.rs");
    std::fs::write(&file, "one\ntwo\nthree\nfour\n").unwrap();
    let mut app = App::new(
        temporary.path(),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.render_width = 80;
    app.ui_editor = cagent_agent::config::UiEditor::Command {
        command: vec!["my-editor".into()],
        line_command: Some(vec![
            "my-editor".into(),
            "--line".into(),
            "{line}".into(),
            "{path}".into(),
        ]),
        fallback_executables: Vec::new(),
        mode: cagent_agent::config::UiEditorMode::Background,
    };
    app.open_path_at_line(file.clone(), Some(3));
    assert_eq!(
        app.pending_action.take(),
        Some(AppAction::LaunchPath {
            argv_candidates: vec![vec![
                "my-editor".into(),
                "--line".into(),
                "3".into(),
                file.to_string_lossy().into_owned(),
            ]],
            mode: cagent_agent::config::UiEditorMode::Background,
            reload_startup_resources: false,
        })
    );

    app.ui_editor = cagent_agent::config::UiEditor::BuiltIn;
    app.open_path_at_line(file, Some(3));
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Expanded {
            view: ExpandedView::File { .. },
            scroll: 2,
            ..
        })
    ));
}

#[test]
fn builtin_directory_single_click_selects_and_double_click_toggles_lazily() {
    let temporary = tempfile::tempdir().unwrap();
    let nested = temporary.path().join("src");
    std::fs::create_dir(&nested).unwrap();
    std::fs::write(nested.join("lib.rs"), "pub fn value() {}\n").unwrap();
    let mut app = App::new(
        temporary.path(),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );

    app.open_path(temporary.path().to_path_buf());
    let Some(Surface::Expanded {
        view: ExpandedView::Directory { browser },
        ..
    }) = app.surfaces.last()
    else {
        panic!("expected a directory view");
    };
    assert!(matches!(
        browser.tree.rows().as_slice(),
        [cagent_agent::presentation::DirectoryTreeRow {
            kind: cagent_agent::presentation::DirectoryEntryKind::Loading,
            ..
        }]
    ));
    app.complete_directory_loads_for_test();

    let layout = super::surfaces::surface_layout_with_viewport(
        app.surfaces.last().unwrap(),
        temporary.path(),
        80,
        8,
    );
    assert!(layout.surface_hits.contains(&SurfaceHit::DirectoryItem(0)));

    assert!(app.click_expanded_directory_index(0));
    assert!(app.surfaces.last().is_some_and(|surface| matches!(
        surface,
        Surface::Expanded {
            view: ExpandedView::Directory { browser },
            ..
        } if browser.tree.rows().len() == 1
    )));
    assert!(app.click_expanded_directory_index(0));
    assert!(app.surfaces.last().is_some_and(|surface| matches!(
        surface,
        Surface::Expanded {
            view: ExpandedView::Directory { browser },
            ..
        } if browser.tree.rows().iter().any(|row| row.kind == cagent_agent::presentation::DirectoryEntryKind::Loading)
    )));
    app.complete_directory_loads_for_test();
    let Some(Surface::Expanded {
        view: ExpandedView::Directory { browser },
        ..
    }) = app.surfaces.last()
    else {
        panic!("expected a directory view");
    };
    assert_eq!(browser.tree.rows().len(), 2);
    assert_eq!(browser.tree.rows()[1].name, "lib.rs");
    let rendered = surface_lines(app.surfaces.last().unwrap(), temporary.path(), 80)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>();
    assert!(
        rendered
            .iter()
            .any(|line| line.contains("└─") && line.contains("") && line.contains("src"))
    );
    assert!(
        rendered
            .iter()
            .any(|line| line.contains("└─") && line.contains("") && line.contains("lib.rs"))
    );

    assert!(app.click_expanded_directory_index(0));
    assert!(app.click_expanded_directory_index(0));
    let Some(Surface::Expanded {
        view: ExpandedView::Directory { browser },
        ..
    }) = app.surfaces.last()
    else {
        panic!("expected a directory view");
    };
    assert_eq!(browser.tree.rows().len(), 1);
}

#[tokio::test]
async fn directory_keyboard_navigation_keeps_selection_inside_the_viewport() {
    let temporary = tempfile::tempdir().unwrap();
    let viewed = temporary.path().join("view");
    std::fs::create_dir(&viewed).unwrap();
    for index in 0..12 {
        std::fs::write(viewed.join(format!("file-{index:02}.txt")), "text\n").unwrap();
    }
    let runtime = AgentRuntime::open(cagent_agent::runtime::RuntimeOptions::new(
        temporary.path().join("navigation.db"),
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
    app.open_path(viewed);
    app.complete_directory_loads_for_test();
    let Some(Surface::Expanded { viewport_rows, .. }) = app.surfaces.last_mut() else {
        panic!("expected a directory view");
    };
    *viewport_rows = 4;

    app.handle_key(
        &session,
        KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
    )
    .await
    .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Expanded {
            view: ExpandedView::Directory { browser },
            scroll: 1,
            ..
        }) if browser.selected_index() == 4
    ));

    app.handle_key(&session, KeyEvent::new(KeyCode::End, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Expanded {
            view: ExpandedView::Directory { browser },
            scroll: 8,
            ..
        }) if browser.selected_index() == 11
    ));

    app.handle_key(&session, KeyEvent::new(KeyCode::Home, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Expanded {
            view: ExpandedView::Directory { browser },
            scroll: 0,
            ..
        }) if browser.selected_index() == 0
    ));
}

#[test]
fn deleted_image_number_is_reused() {
    let mut app = App::new(
        Path::new("/workspace"),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    let image = |number| cagent_agent::protocol::ImageAttachment {
        id: cagent_agent::protocol::ImageAttachmentId::new(),
        number,
        sha256: format!("hash-{number}"),
        mime_type: "image/png".into(),
        width: 10,
        height: 10,
        size_bytes: 10,
        blob_id: cagent_agent::protocol::BlobId::new(),
    };
    app.insert_image(image(1));
    app.insert_image(image(2));
    app.delete_range(10, 20);

    assert_eq!(app.next_image_number(), 2);
}

#[test]
fn destructive_edits_keep_collapsed_chips_atomic() {
    let new_app = || {
        App::new(
            Path::new("/workspace"),
            ("mock".into(), "echo".into(), None),
            Default::default(),
            "ask",
            None,
        )
    };
    let image = || cagent_agent::protocol::ImageAttachment {
        id: cagent_agent::protocol::ImageAttachmentId::new(),
        number: 1,
        sha256: "hash-1".into(),
        mime_type: "image/png".into(),
        width: 10,
        height: 10,
        size_bytes: 10,
        blob_id: cagent_agent::protocol::BlobId::new(),
    };

    let mut app = new_app();
    app.insert_image(image());
    app.delete_previous_word();
    assert!(app.draft.is_empty());
    assert!(app.images.is_empty());

    app.undo_last_edit();
    app.cursor = 0;
    app.delete_forward();
    assert!(app.draft.is_empty());
    assert!(app.images.is_empty());

    app.undo_last_edit();
    app.delete_range(3, 4);
    assert!(app.draft.is_empty());
    assert!(app.images.is_empty());

    app.undo_last_edit();
    app.cursor = 5;
    app.insert_text("x");
    assert_eq!(app.draft, "[Image #1]x");
    assert_eq!(app.images[0].range.start, 0);
    assert_eq!(app.images[0].range.end, 10);

    app.insert_text(" hello");
    app.move_word_left();
    app.move_word_left();
    assert_eq!(app.cursor, 0);
    app.move_word_right();
    assert_eq!(app.cursor, 10);

    let mut app = new_app();
    app.insert_text("@TEST.md");
    app.confirm_attachment(0, 8, ListEntryKind::File);
    app.delete_previous_word();
    assert!(app.draft.is_empty());
    assert!(app.attachments.is_empty());

    let mut app = new_app();
    let pasted = (0..21)
        .map(|line| format!("pasted line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    app.insert_paste(&pasted);
    app.delete_previous_word();
    assert!(app.draft.is_empty());
    assert!(app.pastes.is_empty());
}

#[test]
fn opening_a_clicked_diff_line_centers_it_in_a_full_file_diff() {
    use cagent_agent::tools::{
        DiffFile, DiffFileKind, DiffHunk, DiffLine, DiffLineKind, SemanticDiff,
    };

    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("sample.rs");
    let source = (1..=100)
        .map(|line| {
            if line == 60 {
                "new value".to_owned()
            } else {
                format!("line {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&path, format!("{source}\n")).unwrap();
    let diff = std::sync::Arc::new(SemanticDiff {
        files: vec![DiffFile {
            old_path: Some(path.clone()),
            new_path: Some(path),
            kind: DiffFileKind::Modified,
            language: Some("rust".into()),
            added_lines: 1,
            removed_lines: 1,
            old_no_final_newline: false,
            new_no_final_newline: false,
            hunks: vec![DiffHunk {
                header: "@@ -60,1 +60,1 @@".into(),
                lines: vec![
                    DiffLine {
                        kind: DiffLineKind::Deletion,
                        old_line: Some(60),
                        new_line: None,
                        text: "old value".into(),
                    },
                    DiffLine {
                        kind: DiffLineKind::Addition,
                        old_line: None,
                        new_line: Some(60),
                        text: "new value".into(),
                    },
                ],
            }],
        }],
    });
    let mut app = App::new(
        temporary.path(),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    app.render_width = 80;
    app.render_height = 24;
    app.diff_hits.push(DiffHit {
        row: 7,
        target: crate::markdown::DiffLineTarget {
            diff,
            file_index: 0,
            old_line: None,
            new_line: Some(60),
        },
    });

    assert!(app.open_diff_at(7));
    let Some(Surface::Expanded {
        view: ExpandedView::Diff { view },
        scroll,
        viewport_rows,
    }) = app.surfaces.last()
    else {
        panic!("expected expanded full-file diff");
    };
    assert_eq!(*viewport_rows, 20);
    let target = super::surfaces::full_file_diff_line_offset(view, 80, None, Some(60));
    let (visible, maximum) = super::surfaces::full_file_diff_scroll_metrics(view, 80, 20);
    assert_eq!(*scroll, target.saturating_sub(visible / 2).min(maximum));
    assert_eq!(view.lines.len(), 101);
}

#[test]
fn provider_auth_update_refreshes_open_provider_menus() {
    let mut app = App::new(
        Path::new("/workspace"),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    app.surfaces.push(Surface::Providers {
        rows: Vec::new(),
        list: ListState::selectable(2),
        query: String::new(),
        query_cursor: 0,
        model_configuration_follows: false,
    });
    app.surfaces.push(Surface::ProviderSetup {
        provider_id: "chatgpt".into(),
        provider: "ChatGPT subscription".into(),
        instructions: "Connect your ChatGPT subscription account.".into(),
        credential_environment_variable: None,
        auth_challenge: Some(cagent_agent::provider::AuthChallenge::Browser {
            authorization_url: "https://auth.invalid".into(),
            state: "state".into(),
            callback_url: "http://localhost:1455/auth/callback".into(),
        }),
        managed_auth: true,
        api_key_auth: false,
        api_key: String::new(),
        api_key_cursor: 0,
        auth_flows: vec![cagent_agent::provider::AuthFlow::BrowserPkce],
        authenticated: false,
    });

    app.apply_provider_auth_update(vec![cagent_agent::provider::ProviderAvailability {
        descriptor: cagent_agent::provider::ProviderDescriptor {
            id: "chatgpt".into(),
            display_name: "ChatGPT subscription".into(),
            default_model_backend: Some(cagent_agent::provider::ModelBackend::OpenAiResponses),
            supported_model_backends: vec![cagent_agent::provider::ModelBackend::OpenAiResponses],
            model_discovery: cagent_agent::provider::ModelDiscoverySource::ModelsDev,
            credential_source: cagent_agent::provider::CredentialSource::Subscription,
            credential_environment_variable: None,
            supports_managed_api_key: false,
            auth_flows: vec![cagent_agent::provider::AuthFlow::BrowserPkce],
        },
        enabled: true,
        auth: cagent_agent::provider::AuthState::Connected {
            detail: "account test".into(),
        },
        has_managed_api_key: false,
    }]);

    assert!(matches!(
        &app.surfaces[0],
        Surface::Providers { rows, .. }
            if rows.len() == 1 && rows[0].id == "chatgpt" && rows[0].status == "enabled"
    ));
    assert!(matches!(
        &app.surfaces[1],
        Surface::ProviderSetup {
            authenticated: true,
            auth_challenge: None,
            instructions,
            ..
        } if instructions == "ChatGPT subscription connected."
    ));
}
