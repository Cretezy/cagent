//! Persistence regression tests.

use super::*;

#[test]
fn delegated_log_pages_and_summaries_do_not_reload_old_payloads() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let conversation = create_session(
        &mut connection,
        &NewSession {
            workspace: ".".into(),
        },
    )
    .unwrap();
    let (_, turn) = append_user(&mut connection, conversation, "parent", &[], &[])
        .unwrap()
        .0;
    let run = create_agent_run(
        &mut connection,
        conversation,
        turn,
        "explore",
        &ModelRef::parse("mock/echo").unwrap(),
        None,
        "inspect",
    )
    .unwrap();
    for index in 0..70 {
        let summary = append_agent_run_assistant(
            &mut connection,
            conversation,
            run.id,
            &format!("message {index}"),
        )
        .unwrap();
        assert!(summary.timeline.is_empty());
        assert!(summary.activity.is_empty());
    }
    let first = load_agent_run_log_page(&connection, conversation, run.id, 0).unwrap();
    assert_eq!(first.entries.len(), 64);
    assert!(first.has_more);
    let second =
        load_agent_run_log_page(&connection, conversation, run.id, first.next_after).unwrap();
    assert_eq!(second.entries.len(), 6);
    assert!(!second.has_more);
    assert!(second.next_after > first.next_after);
    assert!(load_agent_run_log_page(&connection, ConversationId::new(), run.id, 0).is_err());

    // A malformed old payload is a deterministic guard against accidental
    // full hydration: summaries, appends, and newer pages must not decode it.
    connection.execute("UPDATE agent_run_events SET content_json = 'invalid json' WHERE run_id = ?1 AND kind = 'assistant' AND sequence <= ?2",
        params![run.id.to_string(), first.next_after]).unwrap();
    assert!(
        list_agent_run_summaries(&connection, conversation).unwrap()[0]
            .timeline
            .is_empty()
    );
    let appended = append_agent_run_activity(
        &mut connection,
        conversation,
        run.id,
        "bash",
        &json!({"command": "ls"}),
        &json!({"output": "ok"}),
        false,
        None,
    )
    .unwrap();
    assert!(appended.timeline.is_empty());
    let appended =
        append_agent_run_assistant(&mut connection, conversation, run.id, "done").unwrap();
    assert!(appended.timeline.is_empty());
    let completed = set_agent_run_status(
        &mut connection,
        conversation,
        run.id,
        crate::AgentRunStatus::Completed,
        Some("done"),
        None,
        None,
    )
    .unwrap();
    assert!(completed.activity.is_empty());
    assert_eq!(
        load_agent_run_log_page(&connection, conversation, run.id, second.next_after)
            .unwrap()
            .entries
            .len(),
        2
    );
    assert!(load_agent_run(&connection, conversation, run.id).is_err());
}

#[test]
fn node_history_schema_has_no_event_log_and_orders_nodes_transactionally() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let conversation_id = create_session(
        &mut connection,
        &NewSession {
            workspace: "/workspace".into(),
        },
    )
    .unwrap();
    let revision_before: i64 = connection
        .query_row(
            "SELECT revision FROM conversations WHERE id = ?1",
            [conversation_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    append_user(&mut connection, conversation_id, "one", &[], &[]).unwrap();
    let sequences = connection
        .prepare("SELECT sequence FROM nodes WHERE conversation_id = ?1 ORDER BY sequence")
        .unwrap()
        .query_map([conversation_id.to_string()], |row| row.get::<_, i64>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(sequences, (1..=sequences.len() as i64).collect::<Vec<_>>());
    let revision_after: i64 = connection
        .query_row(
            "SELECT revision FROM conversations WHERE id = ?1",
            [conversation_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(revision_after > revision_before);
    let event_log_exists: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'event_log')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!event_log_exists);
}

#[test]
fn pre_node_history_schema_is_rejected_without_rewrite() {
    let mut connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL);
             INSERT INTO schema_migrations VALUES (6, 'old');",
        )
        .unwrap();
    let error = migrate(&mut connection).unwrap_err().to_string();
    assert!(error.contains("unsupported conversation schema"));
}

#[test]
fn transcript_pages_cover_the_active_branch_once_and_reject_stale_boundaries() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let conversation_id = create_session(
        &mut connection,
        &NewSession {
            workspace: "/workspace".into(),
        },
    )
    .unwrap();
    let mut expected = Vec::new();
    for index in 0..150 {
        let ((node_id, _), _) = append_user(
            &mut connection,
            conversation_id,
            &format!("message {index}"),
            &[],
            &[],
        )
        .unwrap();
        expected.push(node_id);
    }

    let full_history = load_history(&connection, conversation_id).unwrap();
    let assert_matches_full_history = |page: &[crate::HistoryNode]| {
        for page_node in page {
            let mut expected = full_history
                .iter()
                .find(|node| node.id == page_node.id)
                .cloned()
                .expect("paged node exists in full history");
            // Page activity marks the newest node in that page, whereas full
            // history marks the conversation's durable active tip.
            expected.active = page_node.active;
            assert_eq!(*page_node, expected);
        }
    };
    let tail = load_transcript_page_hydration(&connection, conversation_id, None).unwrap();
    assert_matches_full_history(&tail.history);
    assert!(tail.history.len() <= TRANSCRIPT_PAGE_BLOCKS + 2);
    assert!(
        tail.events
            .iter()
            .all(|event| matches!(event, TranscriptPageEvent::NodeAppended { .. }))
    );
    let stale_candidate = tail.older.clone().expect("large history has an older page");
    let mut pages = vec![
        tail.history
            .iter()
            .filter(|node| node.kind == NodeKind::UserMessage)
            .map(|node| node.id)
            .collect::<Vec<_>>(),
    ];
    let mut cursor = tail.older;
    while let Some(current) = cursor {
        let page =
            load_transcript_page_hydration(&connection, conversation_id, Some(&current)).unwrap();
        assert_matches_full_history(&page.history);
        pages.push(
            page.history
                .iter()
                .filter(|node| node.kind == NodeKind::UserMessage)
                .map(|node| node.id)
                .collect(),
        );
        cursor = page.older;
    }
    pages.reverse();
    assert_eq!(pages.into_iter().flatten().collect::<Vec<_>>(), expected);

    fork(&mut connection, conversation_id, expected[10], &[]).unwrap();
    assert!(matches!(
        load_transcript_page_hydration(&connection, conversation_id, Some(&stale_candidate),),
        Err(RuntimeError::StaleTranscriptCursor)
    ));
}

#[test]
fn transcript_page_includes_durable_notices_attached_to_its_branch() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let conversation_id = create_session(
        &mut connection,
        &NewSession {
            workspace: "/workspace".into(),
        },
    )
    .unwrap();
    let ((parent_id, _), _) =
        append_user(&mut connection, conversation_id, "hello", &[], &[]).unwrap();
    append_transcript_notice(&mut connection, conversation_id, "frontend notice").unwrap();

    let page = load_transcript_page_hydration(&connection, conversation_id, None).unwrap();
    let notice = page
        .history
        .iter()
        .find(|node| {
            node.kind == NodeKind::System
                && node
                    .content
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    == Some("frontend notice")
        })
        .expect("durable transcript notice");
    assert_eq!(notice.parent_id, Some(parent_id));
    assert!(notice.active);
    assert!(page.events.iter().any(|event| matches!(
        event,
        TranscriptPageEvent::NodeAppended { node_id, .. } if *node_id == notice.id
    )));
}

#[test]
fn recap_is_durable_and_rejected_after_the_branch_changes() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let conversation_id = create_session(
        &mut connection,
        &NewSession {
            workspace: "/workspace".into(),
        },
    )
    .unwrap();
    let ((parent_id, _), _) =
        append_user(&mut connection, conversation_id, "hello", &[], &[]).unwrap();
    let (stored, _) = append_recap(
        &mut connection,
        conversation_id,
        parent_id,
        "The session is ready for the next step.",
    )
    .unwrap();
    assert!(stored);

    let page = load_transcript_page_hydration(&connection, conversation_id, None).unwrap();
    let recap = page
        .history
        .iter()
        .find(|node| {
            node.kind == NodeKind::System
                && node
                    .content
                    .get("system_type")
                    .and_then(serde_json::Value::as_str)
                    == Some("recap")
        })
        .expect("durable recap");
    assert_eq!(
        recap
            .content
            .get("text")
            .and_then(serde_json::Value::as_str),
        Some("The session is ready for the next step.")
    );
    assert!(recap.active);
    assert_eq!(
        reconstruct_model_input(&connection, conversation_id, recap.id, false).unwrap(),
        vec![ModelInput::Message {
            role: MessageRole::User,
            content: "hello".into(),
        }]
    );
    assert!(append_recap(&mut connection, conversation_id, parent_id, "stale").is_err());
}

#[test]
fn local_context_markers_are_branch_aware_hidden_model_context() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let conversation_id = create_session(
        &mut connection,
        &NewSession {
            workspace: ".".into(),
        },
    )
    .unwrap();
    let first = crate::LocalContextSnapshot {
        revision: "r1".into(),
        instructions: vec![crate::InstructionSource {
            path: "AGENTS.md".into(),
            content: "first\n".into(),
        }],
        skills: Vec::new(),
        operating_system: "Linux".into(),
        skill_read_roots: Vec::new(),
        skill_read_exclusions: Vec::new(),
        warnings: Vec::new(),
        workspace: None,
        home_dir: None,
    };
    let (initial, _) = synchronize_local_context(&mut connection, conversation_id, &first).unwrap();
    let initial = initial.unwrap();
    let (user_id, _) = append_user(&mut connection, conversation_id, "hello", &[], &[])
        .unwrap()
        .0;
    assert_eq!(
        reconstruct_model_input(&connection, conversation_id, user_id, false).unwrap(),
        vec![
            ModelInput::Message {
                role: MessageRole::System,
                content: initial.message,
            },
            ModelInput::Message {
                role: MessageRole::User,
                content: "hello".into(),
            },
        ]
    );

    let mut second = first.clone();
    second.revision = "r2".into();
    second.instructions[0].content = "second\n".into();
    let (updated, _) =
        synchronize_local_context(&mut connection, conversation_id, &second).unwrap();
    let updated = updated.unwrap();
    assert!(updated.message.contains("-first"));
    assert!(updated.message.contains("+second"));
    assert!(
        synchronize_local_context(&mut connection, conversation_id, &second)
            .unwrap()
            .0
            .is_none()
    );

    append_context_clear(&mut connection, conversation_id, "edit", 8_000).unwrap();
    let (after_clear, _) =
        synchronize_local_context(&mut connection, conversation_id, &second).unwrap();
    assert!(
        after_clear
            .unwrap()
            .message
            .contains("revision=\"complete\"")
    );

    let history = load_history(&connection, conversation_id).unwrap();
    let marker = history
        .iter()
        .find(|node| node.id == initial.node_id)
        .unwrap();
    assert_eq!(marker.kind, NodeKind::System);
    assert_eq!(marker.content["transcript"], "hidden");
    assert_eq!(marker.content["local_context"]["revision"], "r1");
}

#[test]
fn sqlite_connections_use_wal_foreign_keys_and_a_cross_process_busy_timeout() {
    let temporary = tempfile::tempdir().unwrap();
    let connection = Connection::open(temporary.path().join("cagent.db")).unwrap();
    configure(&connection).unwrap();

    let journal_mode: String = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    let busy_timeout: i64 = connection
        .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
        .unwrap();
    let foreign_keys: i64 = connection
        .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
        .unwrap();

    assert_eq!(journal_mode, "wal");
    assert_eq!(busy_timeout, 30_000);
    assert_eq!(foreign_keys, 1);
}

#[tokio::test]
async fn global_reads_are_read_only_and_do_not_create_or_migrate_the_cache() {
    let temporary = tempfile::TempDir::new().unwrap();
    let path = temporary.path().join("global.db");
    let global = GlobalStore::open(temporary.path()).await.unwrap();

    assert!(
        global
            .conversations(crate::ConversationQuery::default())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(!path.exists(), "a cache read must not create global.db");

    Connection::open(&path).unwrap();
    assert!(
        global
            .conversations(crate::ConversationQuery::default())
            .await
            .unwrap()
            .is_empty()
    );
    let connection = Connection::open(path).unwrap();
    let migrations: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'schema_migrations'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(migrations, 0, "a cache read must not migrate global.db");

    let writer = Connection::open(temporary.path().join("global.db")).unwrap();
    configure_global(&writer).unwrap();
    let query_only: i64 = writer
        .query_row("PRAGMA query_only", [], |row| row.get(0))
        .unwrap();
    assert_eq!(query_only, 0);
    let read_only = Connection::open_with_flags(
        temporary.path().join("global.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    read_only.pragma_update(None, "query_only", "ON").unwrap();
    assert_eq!(
        read_only
            .query_row("PRAGMA query_only", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert!(
        read_only
            .execute("CREATE TABLE should_not_exist(id)", [])
            .is_err()
    );
}

#[test]
fn global_writer_connections_use_the_five_second_busy_timeout() {
    let temporary = tempfile::tempdir().unwrap();
    let connection = Connection::open(temporary.path().join("global.db")).unwrap();
    configure_global(&connection).unwrap();
    let busy_timeout: i64 = connection
        .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
        .unwrap();
    assert_eq!(busy_timeout, 5_000);
}

#[test]
fn composer_history_persists_attachments_slash_commands_and_collapses_exact_duplicates() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let conversation_id = create_session(
        &mut connection,
        &NewSession {
            workspace: "/workspace".into(),
        },
    )
    .unwrap();
    let spec = AttachmentSpec {
        path: "src/lib.rs".into(),
        start_line: Some(4),
        end_line: Some(8),
    };
    append_user(
        &mut connection,
        conversation_id,
        "inspect @src/lib.rs",
        &[],
        std::slice::from_ref(&spec),
    )
    .unwrap();
    append_user(
        &mut connection,
        conversation_id,
        "inspect @src/lib.rs",
        &[],
        std::slice::from_ref(&spec),
    )
    .unwrap();
    record_slash_command(
        &mut connection,
        conversation_id,
        "/model mock/echo",
        None,
        true,
    )
    .unwrap();

    assert_eq!(
        load_composer_history(&connection, conversation_id, false).unwrap(),
        vec![
            ComposerHistoryEntry {
                kind: crate::ComposerInputKind::Prompt,
                text: "inspect @src/lib.rs".into(),
                attachment_specs: vec![spec],
                images: Vec::new(),
                image_chips: Vec::new(),
            },
            ComposerHistoryEntry {
                kind: crate::ComposerInputKind::Prompt,
                text: "/model mock/echo".into(),
                attachment_specs: Vec::new(),
                images: Vec::new(),
                image_chips: Vec::new(),
            },
        ]
    );
}

#[test]
fn detached_bash_history_preserves_input_kind_and_branch_tip() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let conversation_id = create_session(
        &mut connection,
        &NewSession {
            workspace: "/workspace".into(),
        },
    )
    .unwrap();
    let before: String = connection
        .query_row(
            "SELECT active_node_id FROM conversations WHERE id = ?1",
            [conversation_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    let anchor = record_bash_command(&mut connection, conversation_id, "./scripts/check").unwrap();
    assert_eq!(anchor.to_string(), before);
    let after: String = connection
        .query_row(
            "SELECT active_node_id FROM conversations WHERE id = ?1",
            [conversation_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(after, before);
    assert_eq!(
        load_composer_history(&connection, conversation_id, false).unwrap(),
        [ComposerHistoryEntry {
            kind: crate::ComposerInputKind::Bash,
            text: "./scripts/check".into(),
            attachment_specs: Vec::new(),
            images: Vec::new(),
            image_chips: Vec::new(),
        }]
    );
}

#[test]
fn queued_prompt_enters_composer_history_immediately() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let conversation_id = create_session(
        &mut connection,
        &NewSession {
            workspace: "/workspace".into(),
        },
    )
    .unwrap();

    queue_input(
        &mut connection,
        conversation_id,
        "follow up",
        crate::QueueTarget::NextBoundary,
        &[],
        &[],
        &[],
        false,
        false,
    )
    .unwrap();

    assert_eq!(
        load_composer_history(&connection, conversation_id, false).unwrap(),
        [ComposerHistoryEntry {
            kind: crate::ComposerInputKind::Prompt,
            text: "follow up".into(),
            attachment_specs: Vec::new(),
            images: Vec::new(),
            image_chips: Vec::new(),
        }]
    );
}

#[test]
fn history_nodes_restore_the_full_slash_command_for_forked_drafts() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let conversation_id = create_session(
        &mut connection,
        &NewSession {
            workspace: "/workspace".into(),
        },
    )
    .unwrap();

    for (command, text) in [
        ("/plan propose an example plan", "propose an example plan"),
        (
            "/spawn inspect the relevant files",
            "inspect the relevant files",
        ),
    ] {
        record_slash_command(&mut connection, conversation_id, command, Some(text), true).unwrap();
        append_user(&mut connection, conversation_id, text, &[], &[]).unwrap();
    }

    let user_nodes = load_history(&connection, conversation_id)
        .unwrap()
        .into_iter()
        .filter(|node| node.kind == NodeKind::UserMessage)
        .collect::<Vec<_>>();
    assert_eq!(user_nodes.len(), 2);
    assert_eq!(user_nodes[0].content["display_label"], "Plan");
    assert!(user_nodes[0].content.get("display_text").is_none());
    assert_eq!(user_nodes[1].content["display_label"], "Spawn");
    assert!(user_nodes[1].content.get("display_text").is_none());
    assert_eq!(
        reconstruct_model_input(&connection, conversation_id, user_nodes[1].id, false).unwrap(),
        vec![
            ModelInput::Message {
                role: MessageRole::User,
                content: "propose an example plan".into(),
            },
            ModelInput::Message {
                role: MessageRole::User,
                content: "inspect the relevant files".into(),
            },
        ]
    );
    assert_eq!(
        crate::history_user_draft(&user_nodes[0]).unwrap().0,
        "/plan propose an example plan"
    );
    assert_eq!(
        crate::history_user_draft(&user_nodes[1]).unwrap().0,
        "/spawn inspect the relevant files"
    );
}

#[test]
fn queued_slash_command_uses_its_user_facing_label_when_dispatched() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let conversation_id = create_session(
        &mut connection,
        &NewSession {
            workspace: "/workspace".into(),
        },
    )
    .unwrap();
    record_slash_command(
        &mut connection,
        conversation_id,
        "/search rust sqlite",
        Some("Search rust sqlite"),
        true,
    )
    .unwrap();
    let (queued, _) = queue_input(
        &mut connection,
        conversation_id,
        "Search rust sqlite",
        QueueTarget::EndOfTurn,
        &[],
        &[],
        &[],
        false,
        false,
    )
    .unwrap();
    let ((node_id, _, _, _), _) =
        dispatch_queued(&mut connection, conversation_id, &queued, &[], &[], &[]).unwrap();
    let history = load_history(&connection, conversation_id).unwrap();
    let user = history.iter().find(|node| node.id == node_id).unwrap();
    assert_eq!(user.content["display_label"], "Search");
    assert_eq!(user.content["display_text"], "rust sqlite");
    assert_eq!(
        reconstruct_model_input(&connection, conversation_id, node_id, false).unwrap(),
        vec![ModelInput::Message {
            role: MessageRole::User,
            content: "Search rust sqlite".into(),
        }]
    );
}

#[test]
fn queued_mode_command_defers_its_mode_and_preserves_its_command_text() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let conversation_id = create_session(
        &mut connection,
        &NewSession {
            workspace: "/workspace".into(),
        },
    )
    .unwrap();
    record_slash_command(
        &mut connection,
        conversation_id,
        "/plan review this design",
        Some("review this design"),
        true,
    )
    .unwrap();
    let (queued, _) = queue_mode_input(
        &mut connection,
        conversation_id,
        "plan",
        "/plan review this design",
        "review this design",
        QueueTarget::NextBoundary,
        &[],
    )
    .unwrap();

    assert_eq!(queued.kind, crate::QueuedItemKind::ModePrompt);
    assert_eq!(queued.mode.as_deref(), Some("plan"));
    assert_eq!(
        queued.command_text.as_deref(),
        Some("/plan review this design")
    );
    assert_eq!(
        load_active_profiles(&connection, conversation_id)
            .unwrap()
            .1,
        "ask"
    );

    let ((node_id, _, prompt, mode), events) =
        dispatch_queued(&mut connection, conversation_id, &queued, &[], &[], &[]).unwrap();
    assert_eq!(prompt, "review this design");
    assert_eq!(mode.as_deref(), Some("plan"));
    assert_eq!(
        load_active_profiles(&connection, conversation_id)
            .unwrap()
            .1,
        "plan"
    );
    assert!(matches!(
        events.first().map(|event| &event.kind),
        Some(DurableEventKind::ModeChanged { mode, pending: true }) if mode == "plan"
    ));
    assert_eq!(
        reconstruct_model_input(&connection, conversation_id, node_id, false).unwrap(),
        vec![ModelInput::Message {
            role: MessageRole::User,
            content: "review this design".into(),
        }]
    );
    let user = load_history(&connection, conversation_id)
        .unwrap()
        .into_iter()
        .find(|node| node.id == node_id)
        .unwrap();
    assert_eq!(
        crate::history_user_draft(&user).unwrap().0,
        "/plan review this design"
    );
}

#[test]
fn new_session_composer_seed_is_workspace_scoped_and_limited_to_100_prompts() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let workspace = "/workspace";
    let first = create_session(
        &mut connection,
        &NewSession {
            workspace: workspace.into(),
        },
    )
    .unwrap();
    for number in 0..101 {
        append_user(
            &mut connection,
            first,
            &format!("prompt {number}"),
            &[],
            &[],
        )
        .unwrap();
    }
    record_slash_command(&mut connection, first, "/plan xxx", Some("xxx"), true).unwrap();
    append_user(&mut connection, first, "xxx", &[], &[]).unwrap();
    let other = create_session(
        &mut connection,
        &NewSession {
            workspace: "/workspace-child".into(),
        },
    )
    .unwrap();
    append_user(&mut connection, other, "must not leak", &[], &[]).unwrap();
    let new_session = create_session(
        &mut connection,
        &NewSession {
            workspace: workspace.into(),
        },
    )
    .unwrap();

    let entries = load_composer_history(&connection, new_session, true).unwrap();
    assert_eq!(entries.len(), 100);
    assert_eq!(entries.first().unwrap().text, "prompt 2");
    assert_eq!(entries.last().unwrap().text, "/plan xxx");
    assert!(entries.iter().any(|entry| entry.text == "/plan xxx"));
    assert!(entries.iter().all(|entry| entry.text != "xxx"));
    assert!(entries.iter().all(|entry| entry.text != "must not leak"));
}

#[test]
fn conversation_schema_migrations_are_versioned_and_idempotent() {
    let mut connection = Connection::open_in_memory().unwrap();

    migrate(&mut connection).unwrap();
    let changes_after_first_migration = connection.total_changes();
    migrate(&mut connection).unwrap();
    assert_eq!(connection.total_changes(), changes_after_first_migration);

    let schema_migrations: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'schema_migrations'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(schema_migrations, 1);
    let versions: i64 = connection
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(versions, 5);
    let composer_history: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'composer_history'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(composer_history, 1);
    let conversation_permissions: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'conversation_permissions'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(conversation_permissions, 1);
    let image_blob_hashes: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'image_blob_hashes'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(image_blob_hashes, 1);
}

#[test]
fn conversation_schema_version_seven_adds_conversation_permissions() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    connection
        .execute_batch(
            "DROP TABLE conversation_permissions;
             DELETE FROM schema_migrations WHERE version = 13;
             INSERT OR IGNORE INTO schema_migrations(version, applied_at) VALUES (7, 'old');",
        )
        .unwrap();

    migrate(&mut connection).unwrap();

    let table_exists: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'conversation_permissions')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let version_exists: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version = 13)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(table_exists);
    assert!(version_exists);
}

#[test]
fn terminal_preview_migration_backfills_retained_output() {
    let mut connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE background_terminals (
                id TEXT PRIMARY KEY, conversation_id TEXT NOT NULL,
                tool_call_node_id TEXT, command TEXT NOT NULL, status TEXT NOT NULL,
                created_at TEXT NOT NULL, started_at TEXT NOT NULL, completed_at TEXT,
                exit_code INTEGER, output_base INTEGER NOT NULL DEFAULT 0,
                output_cursor INTEGER NOT NULL DEFAULT 0, output_bytes INTEGER NOT NULL DEFAULT 0,
                discarded_bytes INTEGER NOT NULL DEFAULT 0, truncated INTEGER NOT NULL DEFAULT 0,
                output BLOB NOT NULL DEFAULT X'', ansi_output BLOB NOT NULL DEFAULT X''
             );
             INSERT INTO background_terminals (
                id, conversation_id, command, status, created_at, started_at, output
             ) VALUES ('terminal', 'conversation', 'printf lots', 'exited', '1', '1',
                       CAST('one\ntwo\nthree\nfour\nfive' AS BLOB));",
        )
        .unwrap();

    migrate(&mut connection).unwrap();

    let preview: String = connection
        .query_row(
            "SELECT preview_output FROM background_terminals WHERE id = 'terminal'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(preview, "one\ntwo\n… 1 lines omitted …\nfour\nfive");
}

#[tokio::test]
async fn global_schema_is_independently_versioned_and_created_by_writers() {
    let temporary = tempfile::TempDir::new().unwrap();
    let global = GlobalStore::open(temporary.path()).await.unwrap();
    let path = temporary.path().join("global.db");
    assert!(!path.exists());

    assert!(
        global
            .conversations(crate::ConversationQuery::default())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(!path.exists());
    global.reconcile().await.unwrap();
    assert!(path.is_file());
    let connection = Connection::open(path).unwrap();
    let versions: i64 = connection
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(versions, 1);
    let conversation_tables: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'conversations'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(conversation_tables, 0);
}

#[tokio::test]
async fn cleanup_removes_all_global_conversation_projections() {
    let temporary = tempfile::TempDir::new().unwrap();
    let global = GlobalStore::open(temporary.path()).await.unwrap();
    global.reconcile().await.unwrap();
    let id = crate::ConversationId::new();
    let path = temporary.path().join("global.db");
    let connection = Connection::open(&path).unwrap();
    connection
        .execute(
            "INSERT INTO conversation_index(
            conversation_id, workspace, title, title_search, created_at, updated_at,
            active_node_id, active_status, active_branch_preview, active_branch_message_count,
            agent, mode, provider, model, archived, source_revision
         ) VALUES (?1, '', '', '', '', '', '', '', '', 0, '', '', NULL, NULL, 0, 1)",
            [id.to_string()],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO conversation_search_messages VALUES (?1, 'node', 'text')",
            [id.to_string()],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO composer_recent VALUES ('entry', ?1, 'user_message', 'prompt', 'text', '[]', '')",
            [id.to_string()],
        )
        .unwrap();
    drop(connection);

    global
        .remove_conversation_projections(vec![id])
        .await
        .unwrap();
    let connection = Connection::open(path).unwrap();
    for table in [
        "conversation_index",
        "conversation_search_messages",
        "composer_recent",
    ] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0, "{table}");
    }
}

#[tokio::test]
async fn global_cache_reads_fall_back_while_the_disposable_database_is_locked() {
    let temporary = tempfile::TempDir::new().unwrap();
    let global = GlobalStore::open(temporary.path()).await.unwrap();
    global.reconcile().await.unwrap();
    let blocker = Connection::open(temporary.path().join("global.db")).unwrap();
    blocker.execute_batch("BEGIN EXCLUSIVE").unwrap();

    let started = std::time::Instant::now();
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            global.load_composer_seed()
        )
        .await
        .unwrap()
        .unwrap()
        .is_empty()
    );
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            global.load_model_catalog("mock")
        )
        .await
        .unwrap()
        .unwrap()
        .is_none()
    );
    assert!(started.elapsed() < std::time::Duration::from_millis(250));

    blocker.execute_batch("COMMIT").unwrap();
}

#[test]
fn schema_initialization_adds_the_title_generation_state_to_existing_databases() {
    let mut connection = Connection::open_in_memory().unwrap();
    let old_schema = CONVERSATION_SCHEMA
        .replace(
            "  title_generation_state TEXT NOT NULL DEFAULT 'complete',\n",
            "",
        )
        .replace("  slash_command_user_text TEXT,\n", "")
        .replace("  context_window_tokens INTEGER,\n", "");
    connection.execute_batch(&old_schema).unwrap();

    migrate(&mut connection).unwrap();

    let columns = connection
        .prepare("PRAGMA table_info(conversations)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(
        columns
            .iter()
            .any(|column| column == "title_generation_state")
    );
    let composer_columns = connection
        .prepare("PRAGMA table_info(composer_history)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(
        composer_columns
            .iter()
            .any(|column| column == "slash_command_user_text")
    );
    let node_columns = connection
        .prepare("PRAGMA table_info(nodes)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(
        node_columns
            .iter()
            .any(|name| name == "context_window_tokens")
    );
}

#[test]
fn active_context_uses_latest_response_and_stops_at_clear() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let conversation_id = create_session(
        &mut connection,
        &NewSession {
            workspace: "/workspace".into(),
        },
    )
    .unwrap();
    let (user_id, turn_id) = append_user(&mut connection, conversation_id, "hello", &[], &[])
        .unwrap()
        .0;
    let attempt = ModelAttemptSnapshot {
        request_id: RequestId::new(),
        attempt_id: AttemptId::new(),
        model: ModelRef::parse("mock/echo").unwrap(),
        effort: None,
        agent: "default".into(),
        mode: "ask".into(),
        context_window: 1_000,
    };
    let (assistant_id, _) = start_assistant(
        &mut connection,
        conversation_id,
        user_id,
        turn_id,
        Some(&attempt),
    )
    .unwrap();
    connection
        .execute(
            "INSERT INTO model_usage (node_id, input_tokens, output_tokens, total_tokens)
             VALUES (?1, 30, 7, NULL)",
            [assistant_id.to_string()],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE nodes SET status = 'completed', completed_at = datetime('now') WHERE id = ?1",
            [assistant_id.to_string()],
        )
        .unwrap();
    assert_eq!(
        load_active_context(&connection, conversation_id).unwrap(),
        Some(crate::ContextUsage {
            used_tokens: 37,
            context_window: 1_000,
        })
    );

    append_context_clear(&mut connection, conversation_id, "ask", 2_000).unwrap();
    assert_eq!(
        load_active_context(&connection, conversation_id).unwrap(),
        Some(crate::ContextUsage {
            used_tokens: 0,
            context_window: 2_000,
        })
    );

    fork(&mut connection, conversation_id, assistant_id, &[]).unwrap();
    assert_eq!(
        load_active_context(&connection, conversation_id)
            .unwrap()
            .unwrap()
            .used_tokens,
        37
    );
}

#[test]
fn compact_plan_source_skips_transcript_notice_between_plan_and_acceptance() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let conversation_id = create_session(
        &mut connection,
        &NewSession {
            workspace: "/workspace".into(),
        },
    )
    .unwrap();
    let (user_id, turn_id) =
        append_user(&mut connection, conversation_id, "propose a plan", &[], &[])
            .unwrap()
            .0;
    let (plan_id, _) =
        start_assistant(&mut connection, conversation_id, user_id, turn_id, None).unwrap();
    connection
        .execute(
            "UPDATE nodes SET status = 'completed', content_json = ?1, completed_at = datetime('now') WHERE id = ?2",
            params![json!({ "text": "# Plan", "flavor": "plan" }).to_string(), plan_id.to_string()],
        )
        .unwrap();
    append_transcript_notice(&mut connection, conversation_id, "Worked for 1s").unwrap();
    let (accepted_id, _) = append_accepted_plan(
        &mut connection,
        conversation_id,
        "# Plan",
        None,
        false,
        true,
        32_768,
    )
    .unwrap()
    .0;

    let source =
        load_compaction_source_with_image_resize(&connection, conversation_id, false, true)
            .unwrap();

    assert_eq!(source.excluded_node_id, Some(plan_id));
    assert_eq!(source.reapplied_node_id, Some(accepted_id));
    assert_eq!(source.source_tip_node_id, user_id);
}

#[test]
fn active_context_treats_a_clear_accepted_plan_as_a_durable_reset() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let conversation_id = create_session(
        &mut connection,
        &NewSession {
            workspace: "/workspace".into(),
        },
    )
    .unwrap();
    let (user_id, turn_id) = append_user(&mut connection, conversation_id, "hello", &[], &[])
        .unwrap()
        .0;
    let attempt = ModelAttemptSnapshot {
        request_id: RequestId::new(),
        attempt_id: AttemptId::new(),
        model: ModelRef::parse("mock/echo").unwrap(),
        effort: None,
        agent: "default".into(),
        mode: "ask".into(),
        context_window: 1_000,
    };
    let (assistant_id, _) = start_assistant(
        &mut connection,
        conversation_id,
        user_id,
        turn_id,
        Some(&attempt),
    )
    .unwrap();
    connection
        .execute(
            "INSERT INTO model_usage (node_id, input_tokens, output_tokens, total_tokens)
             VALUES (?1, 30, 7, NULL)",
            [assistant_id.to_string()],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE nodes SET status = 'completed', completed_at = datetime('now') WHERE id = ?1",
            [assistant_id.to_string()],
        )
        .unwrap();

    let (accepted_plan_id, accepted_turn_id) = append_accepted_plan(
        &mut connection,
        conversation_id,
        "Implement it",
        None,
        true,
        false,
        2_000,
    )
    .unwrap()
    .0;
    let history = load_history(&connection, conversation_id).unwrap();
    let accepted = history
        .iter()
        .find(|node| node.id == accepted_plan_id)
        .unwrap();
    let payload: crate::AcceptedPlanPayload =
        serde_json::from_value(accepted.content.clone()).unwrap();
    assert_eq!(payload.instruction, "Implement the following plan.");
    assert_eq!(payload.plan_markdown, "Implement it");
    assert!(payload.reset_context);
    assert_eq!(accepted.parent_id, Some(assistant_id));
    assert!(!history.iter().any(|node| {
        node.kind == NodeKind::System
            && node
                .content
                .get("message")
                .and_then(serde_json::Value::as_str)
                == Some("Implement the following plan.")
    }));
    assert_eq!(
        load_active_context(&connection, conversation_id).unwrap(),
        Some(crate::ContextUsage {
            used_tokens: 0,
            context_window: 2_000,
        })
    );
    assert_eq!(
        reconstruct_model_input(&connection, conversation_id, accepted_plan_id, false).unwrap(),
        vec![
            ModelInput::Message {
                role: MessageRole::System,
                content: "Implement the following plan.".into(),
            },
            ModelInput::Message {
                role: MessageRole::User,
                content: "Implement it".into(),
            },
        ]
    );

    let (implementation_id, _) = start_assistant(
        &mut connection,
        conversation_id,
        accepted_plan_id,
        accepted_turn_id,
        Some(&ModelAttemptSnapshot {
            request_id: RequestId::new(),
            attempt_id: AttemptId::new(),
            model: ModelRef::parse("mock/echo").unwrap(),
            effort: None,
            agent: "default".into(),
            mode: "edit".into(),
            context_window: 2_000,
        }),
    )
    .unwrap();
    connection
        .execute(
            "UPDATE nodes SET status = 'completed', completed_at = datetime('now') WHERE id = ?1",
            [implementation_id.to_string()],
        )
        .unwrap();

    assert_eq!(
        load_active_context(&connection, conversation_id).unwrap(),
        Some(crate::ContextUsage {
            used_tokens: 0,
            context_window: 2_000,
        })
    );
}

#[test]
fn background_terminal_retains_separate_safe_and_ansi_output() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let conversation_id = create_session(
        &mut connection,
        &NewSession {
            workspace: ".".into(),
        },
    )
    .unwrap();
    let (node_id, _) = append_user(&mut connection, conversation_id, "work", &[], &[])
        .unwrap()
        .0;
    let mut terminal = crate::TerminalSnapshot {
        id: crate::TerminalId::new(),
        owner: conversation_id,
        owner_agent_run_id: None,
        tool_call_node_id: Some(node_id),
        read_safe: Some(true),
        command: "cargo test".into(),
        status: crate::TerminalStatus::Running,
        created_at: "1".into(),
        started_at: "1".into(),
        completed_at: None,
        exit_code: None,
        output_base: 0,
        output_cursor: 3,
        output_bytes: 3,
        discarded_bytes: 0,
        truncated: false,
        ansi_output: "\x1b[31mone\ntwo\nthree\nfour\nfive\x1b[0m".into(),
        output: "one\ntwo\nthree\nfour\nfive".into(),
    };
    upsert_terminal(&mut connection, &terminal).unwrap();
    let loaded = load_terminal(&connection, conversation_id, terminal.id).unwrap();
    assert_eq!(loaded.tool_call_node_id, Some(node_id));
    assert_eq!(loaded.read_safe, Some(true));
    assert_eq!(loaded.output, "one\ntwo\nthree\nfour\nfive");
    assert_eq!(
        loaded.ansi_output,
        "\x1b[31mone\ntwo\nthree\nfour\nfive\x1b[0m"
    );
    let preview = list_terminal_previews(&connection, conversation_id).unwrap();
    assert_eq!(preview.len(), 1);
    assert_eq!(
        preview[0].output,
        "one\ntwo\n… 1 lines omitted …\nfour\nfive"
    );
    assert_eq!(preview[0].ansi_output, preview[0].output);

    terminal.status = crate::TerminalStatus::Terminating;
    upsert_terminal(&mut connection, &terminal).unwrap();
    assert_eq!(
        load_terminal(&connection, conversation_id, terminal.id)
            .unwrap()
            .status,
        crate::TerminalStatus::Terminating
    );
    let terminal_completion_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM completion_mailbox WHERE work_kind = 'terminal'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(terminal_completion_count, 0);

    terminal.status = crate::TerminalStatus::Exited;
    terminal.completed_at = Some("2".into());
    terminal.exit_code = Some(0);
    upsert_terminal(&mut connection, &terminal).unwrap();
    let terminal_completion_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM completion_mailbox WHERE work_kind = 'terminal'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(terminal_completion_count, 1);
}

#[test]
fn killed_and_orphaned_terminals_do_not_wake_the_model() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let conversation_id = create_session(
        &mut connection,
        &NewSession {
            workspace: ".".into(),
        },
    )
    .unwrap();
    let (node_id, _) = append_user(&mut connection, conversation_id, "work", &[], &[])
        .unwrap()
        .0;
    let terminal = crate::TerminalSnapshot {
        id: crate::TerminalId::new(),
        owner: conversation_id,
        owner_agent_run_id: None,
        tool_call_node_id: Some(node_id),
        read_safe: None,
        command: "cargo test".into(),
        status: crate::TerminalStatus::Killed,
        created_at: "1".into(),
        started_at: "1".into(),
        completed_at: Some("2".into()),
        exit_code: Some(1),
        output_base: 0,
        output_cursor: 0,
        output_bytes: 0,
        discarded_bytes: 0,
        truncated: false,
        ansi_output: String::new(),
        output: String::new(),
    };
    upsert_terminal(&mut connection, &terminal).unwrap();

    let mut orphaned = terminal.clone();
    orphaned.id = crate::TerminalId::new();
    orphaned.status = crate::TerminalStatus::Orphaned;
    orphaned.exit_code = None;
    upsert_terminal(&mut connection, &orphaned).unwrap();

    let terminal_completion_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM completion_mailbox WHERE work_kind = 'terminal'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(terminal_completion_count, 0);
    assert_eq!(
        load_terminal(&connection, conversation_id, terminal.id)
            .unwrap()
            .status,
        crate::TerminalStatus::Killed
    );
    assert_eq!(
        load_terminal(&connection, conversation_id, orphaned.id)
            .unwrap()
            .status,
        crate::TerminalStatus::Orphaned
    );
}

#[test]
fn delegated_terminal_with_null_node_id_round_trips_and_does_not_notify_parent() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let conversation_id = create_session(
        &mut connection,
        &NewSession {
            workspace: ".".into(),
        },
    )
    .unwrap();
    let (_, turn_id) = append_user(&mut connection, conversation_id, "work", &[], &[])
        .unwrap()
        .0;
    let run = create_agent_run(
        &mut connection,
        conversation_id,
        turn_id,
        "explore",
        &ModelRef::parse("mock/echo").unwrap(),
        Some("high"),
        "run a command",
    )
    .unwrap();
    assert_eq!(run.effort.as_deref(), Some("high"));
    let terminal = crate::TerminalSnapshot {
        id: crate::TerminalId::new(),
        owner: conversation_id,
        owner_agent_run_id: Some(run.id),
        tool_call_node_id: None,
        read_safe: None,
        command: "echo delegated".into(),
        status: crate::TerminalStatus::Exited,
        created_at: "1".into(),
        started_at: "1".into(),
        completed_at: Some("2".into()),
        exit_code: Some(0),
        output_base: 0,
        output_cursor: 10,
        output_bytes: 10,
        discarded_bytes: 0,
        truncated: false,
        ansi_output: "delegated\n".into(),
        output: "delegated\n".into(),
    };

    upsert_terminal(&mut connection, &terminal).unwrap();
    let stored_node: Option<String> = connection
        .query_row(
            "SELECT tool_call_node_id FROM background_terminals WHERE id = ?1",
            [terminal.id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored_node, None);

    let loaded = load_terminal(&connection, conversation_id, terminal.id).unwrap();
    assert_eq!(loaded.tool_call_node_id, None);
    assert_eq!(loaded.owner_agent_run_id, Some(run.id));
    assert_eq!(loaded.output, "delegated\n");

    let terminal_completion_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM completion_mailbox WHERE work_kind = 'terminal'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(terminal_completion_count, 0);

    set_agent_run_status(
        &mut connection,
        conversation_id,
        run.id,
        crate::AgentRunStatus::Completed,
        Some("finished"),
        None,
        None,
    )
    .unwrap();
    let (notice_id, _) = append_pending_completion_notice(&mut connection, conversation_id)
        .unwrap()
        .unwrap();
    let content: String = connection
        .query_row(
            "SELECT content_json FROM nodes WHERE id = ?1",
            [notice_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    let content: serde_json::Value = serde_json::from_str(&content).unwrap();
    let envelopes = content["envelopes"].as_array().unwrap();
    assert_eq!(envelopes.len(), 1);
    assert_eq!(envelopes[0]["type"], "sub_agent_completion");
    assert_eq!(envelopes[0]["id"], run.id.to_string());
}

#[test]
fn completion_mailbox_claim_is_exactly_once() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let conversation_id = create_session(
        &mut connection,
        &NewSession {
            workspace: ".".into(),
        },
    )
    .unwrap();
    let (_, turn_id) = append_user(&mut connection, conversation_id, "work", &[], &[])
        .unwrap()
        .0;
    let run = create_agent_run(
        &mut connection,
        conversation_id,
        turn_id,
        "explore",
        &ModelRef::parse("mock/echo").unwrap(),
        None,
        "inspect",
    )
    .unwrap();
    set_agent_run_status(
        &mut connection,
        conversation_id,
        run.id,
        crate::AgentRunStatus::Completed,
        Some("done"),
        None,
        None,
    )
    .unwrap();

    assert!(
        claim_completion(
            &mut connection,
            conversation_id,
            "agent",
            &run.id.to_string(),
            "wait_join",
        )
        .unwrap()
    );
    assert!(
        !claim_completion(
            &mut connection,
            conversation_id,
            "agent",
            &run.id.to_string(),
            "wait_join",
        )
        .unwrap()
    );
}

#[test]
fn pending_completions_coalesce_into_hidden_notice_order() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let conversation_id = create_session(
        &mut connection,
        &NewSession {
            workspace: ".".into(),
        },
    )
    .unwrap();
    let (_, turn_id) = append_user(&mut connection, conversation_id, "work", &[], &[])
        .unwrap()
        .0;
    let mut ids = Vec::new();
    for task in ["first", "second"] {
        let run = create_agent_run(
            &mut connection,
            conversation_id,
            turn_id,
            "explore",
            &ModelRef::parse("mock/echo").unwrap(),
            None,
            task,
        )
        .unwrap();
        set_agent_run_status(
            &mut connection,
            conversation_id,
            run.id,
            crate::AgentRunStatus::Completed,
            Some(task),
            None,
            None,
        )
        .unwrap();
        ids.push(run.id.to_string());
    }

    let (notice_id, _) = append_pending_completion_notice(&mut connection, conversation_id)
        .unwrap()
        .unwrap();
    let content: String = connection
        .query_row(
            "SELECT content_json FROM nodes WHERE id = ?1",
            [notice_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    let content: serde_json::Value = serde_json::from_str(&content).unwrap();
    let delivered = content["envelopes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|envelope| envelope["id"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(delivered, ids);
    assert_eq!(content["system_type"], "completion_envelopes");
}

#[test]
fn interrupt_notice_is_not_rehydrated_into_model_context() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let conversation_id = create_session(
        &mut connection,
        &NewSession {
            workspace: ".".into(),
        },
    )
    .unwrap();

    append_user(&mut connection, conversation_id, "before", &[], &[]).unwrap();
    append_interrupt_notice(&mut connection, conversation_id, true).unwrap();
    let (after_id, _) = append_user(&mut connection, conversation_id, "after", &[], &[])
        .unwrap()
        .0;

    let input = reconstruct_model_input(&connection, conversation_id, after_id, false).unwrap();

    assert_eq!(
        input,
        vec![
            ModelInput::Message {
                role: MessageRole::User,
                content: "before".into(),
            },
            ModelInput::Message {
                role: MessageRole::User,
                content: "after".into(),
            },
        ]
    );
}

fn test_compaction(
    source_tip_node_id: NodeId,
    retained_from_node_id: Option<NodeId>,
    previous_checkpoint_id: Option<NodeId>,
    summary: &str,
) -> CompactionSummary {
    CompactionSummary {
        version: 1,
        summary: summary.into(),
        retained_from_node_id,
        trigger: crate::CompactionTrigger::Manual,
        estimated_input_tokens: 100,
        context_window_tokens: 1_000,
        threshold_percent: 80,
        summary_model: crate::CompactionModelSettings {
            provider: "mock".into(),
            model: "echo".into(),
            effort: Some("high".into()),
        },
        agent: "general".into(),
        mode: "ask".into(),
        source_tip_node_id,
        excluded_node_id: None,
        reapplied_node_id: None,
        previous_checkpoint_id,
        last_summarized_node_id: None,
        retained_token_estimate: None,
        retained_token_target: None,
    }
}

fn test_response_metadata() -> ResponseMetadata {
    ResponseMetadata {
        provider_request_id: Some("summary-response".into()),
        finish_reason: crate::FinishReason::Stop,
        usage: crate::ModelUsage {
            input_tokens: Some(10),
            output_tokens: Some(5),
            total_tokens: Some(15),
            ..crate::ModelUsage::default()
        },
    }
}

fn test_provider_reasoning(ciphertext: &str) -> ModelInput {
    ModelInput::ProviderReasoning {
        source: ModelRef::parse("chatgpt/test-model").unwrap(),
        item: serde_json::from_value(json!({
            "type": "reasoning",
            "id": "rs_test",
            "encrypted_content": ciphertext,
            "summary": [],
        }))
        .unwrap(),
    }
}

fn test_reasoning_assistant(connection: &mut Connection) -> (ConversationId, NodeId) {
    migrate(connection).unwrap();
    let conversation = create_session(
        connection,
        &NewSession {
            workspace: ".".into(),
        },
    )
    .unwrap();
    let ((user, turn), _) = append_user(connection, conversation, "prompt", &[], &[]).unwrap();
    let (assistant, _) = start_assistant(connection, conversation, user, turn, None).unwrap();
    (conversation, assistant)
}

#[test]
fn assistant_reasoning_reopens_in_order_without_entering_transcript_or_events() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("reasoning.db");
    let mut connection = Connection::open(&path).unwrap();
    configure(&connection).unwrap();
    let (conversation, assistant) = test_reasoning_assistant(&mut connection);
    let reasoning = vec![
        test_provider_reasoning("ciphertext-one"),
        test_provider_reasoning("ciphertext-two"),
    ];
    append_assistant_delta(&mut connection, conversation, assistant, "answer", "answer").unwrap();
    let (_, events) = complete_assistant_with_reasoning(
        &mut connection,
        conversation,
        assistant,
        &test_response_metadata(),
        vec![],
        reasoning.clone(),
    )
    .unwrap();
    assert!(
        !serde_json::to_string(&events)
            .unwrap()
            .contains("ciphertext")
    );
    drop(connection);

    let mut connection = Connection::open(&path).unwrap();
    configure(&connection).unwrap();
    migrate(&mut connection).unwrap();
    let input = reconstruct_model_input(&connection, conversation, assistant, false).unwrap();
    assert_eq!(&input[1..3], reasoning.as_slice());
    assert_eq!(
        input[3],
        ModelInput::Message {
            role: MessageRole::Assistant,
            content: "answer".into()
        }
    );
    let stored: String = connection
        .query_row(
            "SELECT reasoning_json FROM assistant_reasoning WHERE node_id = ?1",
            [assistant.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Vec<ModelInput>>(&stored).unwrap(),
        reasoning
    );
    let contents: Vec<String> = connection
        .prepare("SELECT content_json FROM nodes")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(
        contents
            .iter()
            .all(|content| !content.contains("ciphertext"))
    );

    // These are the store-owned inputs to transcript snapshots and event replay.
    let page = load_transcript_page_hydration(&connection, conversation, None).unwrap();
    assert!(
        !serde_json::to_string(&page.history)
            .unwrap()
            .contains("ciphertext")
    );
    for event in page.events {
        let TranscriptPageEvent::NodeAppended { cursor, node_id } = event;
        let node = page.history.iter().find(|node| node.id == node_id).unwrap();
        let event = node.clone().into_appended_event(conversation, cursor);
        assert!(
            !serde_json::to_string(&event)
                .unwrap()
                .contains("ciphertext")
        );
    }

    // Hard forks disable foreign keys while pruning, so sidecars must be pruned explicitly.
    let fork_id = ConversationId::new();
    let fork_path = temporary.path().join("fork.db");
    let user = page
        .history
        .iter()
        .find(|node| node.kind == NodeKind::UserMessage)
        .unwrap()
        .id;
    hard_fork(
        &mut connection,
        conversation,
        fork_id,
        user,
        &fork_path,
        &[],
    )
    .unwrap();
    let copy = Connection::open(fork_path).unwrap();
    assert_eq!(
        copy.query_row("SELECT COUNT(*) FROM assistant_reasoning", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn assistant_reasoning_empty_text_precedes_tools_and_survives_compaction_tail() {
    let mut connection = Connection::open_in_memory().unwrap();
    let (conversation, assistant) = test_reasoning_assistant(&mut connection);
    let reasoning = test_provider_reasoning("retained-ciphertext");
    let (calls, _) = complete_assistant_with_reasoning(
        &mut connection,
        conversation,
        assistant,
        &test_response_metadata(),
        vec![PendingToolCall {
            provider_call_id: "call-1".into(),
            name: "bash".into(),
            arguments: json!({"command": "pwd"}),
            request_index: 0,
            provider_metadata: serde_json::Value::Null,
        }],
        vec![reasoning.clone()],
    )
    .unwrap();
    let (result, _) = append_tool_result(
        &mut connection,
        conversation,
        &calls[0],
        &json!({"output": "."}),
        false,
        1,
        None,
    )
    .unwrap();
    let input = reconstruct_model_input(&connection, conversation, result, false).unwrap();
    assert_eq!(input.len(), 4);
    assert_eq!(input[1], reasoning);
    assert!(matches!(&input[2], ModelInput::ToolCall { call_id, .. } if call_id == "call-1"));
    assert!(matches!(&input[3], ModelInput::ToolResult { call_id, .. } if call_id == "call-1"));
    let source =
        load_compaction_source_with_image_resize(&connection, conversation, false, false).unwrap();
    assert_eq!(source.effective_input, input);
    assert_eq!(
        source
            .tail_candidates
            .iter()
            .map(|item| item.input.clone())
            .collect::<Vec<_>>(),
        input
    );
    assert_eq!(source.tail_candidates[1].node_id, assistant);
    assert!(
        source.tail_candidates[1..]
            .iter()
            .all(|item| item.group_id == assistant)
    );

    let mut summary = test_compaction(result, Some(assistant), None, "prefix summary");
    summary.version = 2;
    let (checkpoint, _) = append_compaction_summary(
        &mut connection,
        conversation,
        result,
        &summary,
        &test_response_metadata(),
    )
    .unwrap();
    let compacted = reconstruct_model_input(&connection, conversation, checkpoint, false).unwrap();
    assert_eq!(&compacted[1..], &input[1..]);
    let source =
        load_compaction_source_with_image_resize(&connection, conversation, false, false).unwrap();
    assert_eq!(source.effective_input, compacted);
    assert_eq!(source.tail_candidates[0].input, reasoning);
    assert!(
        source
            .tail_candidates
            .iter()
            .all(|item| item.group_id == assistant)
    );
    let ((later, _), _) = append_user(&mut connection, conversation, "later", &[], &[]).unwrap();
    let later_input = reconstruct_model_input(&connection, conversation, later, false).unwrap();
    assert_eq!(&later_input[..compacted.len()], compacted.as_slice());
}

#[test]
fn assistant_reasoning_completion_validates_and_rolls_back_atomically() {
    let mut connection = Connection::open_in_memory().unwrap();
    let (conversation, assistant) = test_reasoning_assistant(&mut connection);
    let reasoning = test_provider_reasoning("atomic-ciphertext");
    let before = load_history(&connection, conversation).unwrap();
    for invalid_item in [
        ModelInput::Message {
            role: MessageRole::Assistant,
            content: "not reasoning".into(),
        },
        ModelInput::ConfigurationUpdate {
            effort: "high".into(),
        },
    ] {
        assert!(
            complete_assistant_with_reasoning(
                &mut connection,
                conversation,
                assistant,
                &test_response_metadata(),
                vec![],
                vec![reasoning.clone(), invalid_item],
            )
            .is_err()
        );
    }
    // Fail after both the reasoning INSERT and node completion UPDATE have executed.
    assert!(
        complete_assistant_with_reasoning(
            &mut connection,
            conversation,
            assistant,
            &test_response_metadata(),
            vec![PendingToolCall {
                provider_call_id: "bad-index".into(),
                name: "bash".into(),
                arguments: json!({}),
                request_index: u64::MAX,
                provider_metadata: serde_json::Value::Null,
            }],
            vec![reasoning.clone()],
        )
        .is_err()
    );
    assert_eq!(load_history(&connection, conversation).unwrap(), before);
    for table in ["assistant_reasoning", "model_usage"] {
        assert_eq!(
            connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    complete_assistant_with_reasoning(
        &mut connection,
        conversation,
        assistant,
        &test_response_metadata(),
        vec![],
        vec![reasoning.clone()],
    )
    .unwrap();
    assert!(
        complete_assistant_with_reasoning(
            &mut connection,
            conversation,
            assistant,
            &test_response_metadata(),
            vec![],
            vec![test_provider_reasoning("replacement")],
        )
        .is_err()
    );
    assert_eq!(
        super::assistant_reasoning::load(&connection, assistant).unwrap(),
        vec![reasoning]
    );
}

#[test]
fn assistant_reasoning_incomplete_and_failed_nodes_never_replay() {
    for status in ["streaming", "failed", "cancelled"] {
        let mut connection = Connection::open_in_memory().unwrap();
        let (conversation, assistant) = test_reasoning_assistant(&mut connection);
        // Simulate stale/corrupt private state: projection must still gate on node status.
        connection
            .execute(
                "INSERT INTO assistant_reasoning(node_id, reasoning_json) VALUES (?1, ?2)",
                params![
                    assistant.to_string(),
                    serde_json::to_string(&vec![test_provider_reasoning("incomplete-ciphertext")])
                        .unwrap()
                ],
            )
            .unwrap();
        if status != "streaming" {
            finish_assistant_with_status(&mut connection, conversation, assistant, status).unwrap();
        }
        for include_partial in [false, true] {
            let input =
                reconstruct_model_input(&connection, conversation, assistant, include_partial)
                    .unwrap();
            assert!(
                !input
                    .iter()
                    .any(|item| matches!(item, ModelInput::ProviderReasoning { .. }))
            );
        }
        let source =
            load_compaction_source_with_image_resize(&connection, conversation, false, false)
                .unwrap();
        assert!(
            !source
                .tail_candidates
                .iter()
                .any(|item| matches!(item.input, ModelInput::ProviderReasoning { .. }))
        );
    }
}

#[test]
fn assistant_reasoning_migration_upgrades_v12_and_is_idempotent() {
    let mut connection = Connection::open_in_memory().unwrap();
    let (conversation, assistant) = test_reasoning_assistant(&mut connection);
    let before = load_history(&connection, conversation).unwrap();
    connection
        .execute_batch(
            "DROP TABLE assistant_reasoning;
         DELETE FROM schema_migrations WHERE version = 13;
         INSERT OR IGNORE INTO schema_migrations VALUES (12, 'old');",
        )
        .unwrap();
    migrate(&mut connection).unwrap();
    let changes = connection.total_changes();
    migrate(&mut connection).unwrap();
    assert_eq!(connection.total_changes(), changes);
    assert_eq!(load_history(&connection, conversation).unwrap(), before);
    assert!(
        super::assistant_reasoning::load(&connection, assistant)
            .unwrap()
            .is_empty()
    );
    configure(&connection).unwrap();
    assert!(
        connection
            .execute(
                "INSERT INTO assistant_reasoning(node_id, reasoning_json) VALUES ('missing', '[]')",
                [],
            )
            .is_err()
    );
}

#[test]
fn v2_compaction_projects_summary_before_exact_suffix_immediately_and_later() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let conversation_id = create_session(
        &mut connection,
        &NewSession {
            workspace: ".".into(),
        },
    )
    .unwrap();
    let (_first, _) = append_user(&mut connection, conversation_id, "first", &[], &[])
        .unwrap()
        .0;
    let (retained, _) = append_user(&mut connection, conversation_id, "retained", &[], &[])
        .unwrap()
        .0;
    let mut content = test_compaction(retained, Some(retained), None, "prefix summary");
    content.version = 2;
    content.retained_token_estimate = Some(2);
    content.retained_token_target = Some(20_000);
    let (checkpoint, _) = append_compaction_summary(
        &mut connection,
        conversation_id,
        retained,
        &content,
        &test_response_metadata(),
    )
    .unwrap();

    let immediate =
        reconstruct_model_input(&connection, conversation_id, checkpoint, false).unwrap();
    assert!(
        matches!(&immediate[0], ModelInput::Message { role: MessageRole::System, content } if content.contains("prefix summary"))
    );
    assert!(
        matches!(&immediate[1], ModelInput::Message { role: MessageRole::User, content } if content == "retained")
    );

    let (later, _) = append_user(&mut connection, conversation_id, "later", &[], &[])
        .unwrap()
        .0;
    let resumed = reconstruct_model_input(&connection, conversation_id, later, false).unwrap();
    assert!(
        matches!(&resumed[0], ModelInput::Message { role: MessageRole::System, content } if content.contains("prefix summary"))
    );
    assert!(
        matches!(&resumed[1], ModelInput::Message { role: MessageRole::User, content } if content == "retained")
    );
    assert!(
        matches!(&resumed[2], ModelInput::Message { role: MessageRole::User, content } if content == "later")
    );
}

#[test]
fn compaction_projection_replays_tail_summary_and_later_nodes() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let conversation_id = create_session(
        &mut connection,
        &NewSession {
            workspace: ".".into(),
        },
    )
    .unwrap();
    let (first, _) = append_user(&mut connection, conversation_id, "first", &[], &[])
        .unwrap()
        .0;
    let (second, _) = append_user(&mut connection, conversation_id, "second", &[], &[])
        .unwrap()
        .0;
    let (checkpoint, _) = append_compaction_summary(
        &mut connection,
        conversation_id,
        second,
        &test_compaction(second, Some(second), None, "earlier work is summarized"),
        &test_response_metadata(),
    )
    .unwrap();
    let (later, _) = append_user(&mut connection, conversation_id, "later", &[], &[])
        .unwrap()
        .0;

    let input = reconstruct_model_input(&connection, conversation_id, later, false).unwrap();
    assert_eq!(input.len(), 3);
    assert!(
        matches!(&input[0], ModelInput::Message { role: MessageRole::User, content } if content == "second")
    );
    assert!(
        matches!(&input[1], ModelInput::Message { role: MessageRole::System, content } if content.contains("earlier work is summarized"))
    );
    assert!(
        matches!(&input[2], ModelInput::Message { role: MessageRole::User, content } if content == "later")
    );

    let history = load_history(&connection, conversation_id).unwrap();
    assert!(history.iter().any(|node| {
        node.id == checkpoint
            && node.kind == NodeKind::CompactionSummary
            && node.parent_id == Some(second)
    }));
    assert!(history.iter().any(|node| node.id == first));
    let stats = load_session_stats(&connection, conversation_id).unwrap();
    assert_eq!(stats.usage.output_tokens, Some(5));
}

#[test]
fn repeated_compaction_uses_only_the_newest_checkpoint_and_clear_is_stronger() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let conversation_id = create_session(
        &mut connection,
        &NewSession {
            workspace: ".".into(),
        },
    )
    .unwrap();
    let (first, _) = append_user(&mut connection, conversation_id, "first", &[], &[])
        .unwrap()
        .0;
    let (checkpoint_one, _) = append_compaction_summary(
        &mut connection,
        conversation_id,
        first,
        &test_compaction(first, Some(first), None, "old checkpoint"),
        &test_response_metadata(),
    )
    .unwrap();
    let (later, _) = append_user(&mut connection, conversation_id, "later", &[], &[])
        .unwrap()
        .0;
    let (checkpoint_two, _) = append_compaction_summary(
        &mut connection,
        conversation_id,
        later,
        &test_compaction(later, Some(later), Some(checkpoint_one), "new checkpoint"),
        &test_response_metadata(),
    )
    .unwrap();
    let input =
        reconstruct_model_input(&connection, conversation_id, checkpoint_two, false).unwrap();
    let encoded = serde_json::to_string(&input).unwrap();
    assert!(encoded.contains("new checkpoint"));
    assert!(!encoded.contains("later"));
    assert!(!encoded.contains("old checkpoint"));

    append_context_clear(&mut connection, conversation_id, "ask", 32_768).unwrap();
    let (after_clear, _) = append_user(&mut connection, conversation_id, "fresh", &[], &[])
        .unwrap()
        .0;
    assert_eq!(
        reconstruct_model_input(&connection, conversation_id, after_clear, false).unwrap(),
        vec![
            ModelInput::Message {
                role: MessageRole::System,
                content: "Implement the following plan.".into(),
            },
            ModelInput::Message {
                role: MessageRole::User,
                content: String::new(),
            },
            ModelInput::Message {
                role: MessageRole::User,
                content: "fresh".into(),
            },
        ]
    );
}

#[test]
fn compaction_checkpoint_is_branch_local() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let conversation_id = create_session(
        &mut connection,
        &NewSession {
            workspace: ".".into(),
        },
    )
    .unwrap();
    let (_base, _) = append_user(&mut connection, conversation_id, "base", &[], &[])
        .unwrap()
        .0;
    let (tip, _) = append_user(&mut connection, conversation_id, "tip", &[], &[])
        .unwrap()
        .0;
    append_compaction_summary(
        &mut connection,
        conversation_id,
        tip,
        &test_compaction(tip, Some(tip), None, "checkpoint branch"),
        &test_response_metadata(),
    )
    .unwrap();

    fork(&mut connection, conversation_id, tip, &[]).unwrap();
    let (sibling, _) = append_user(&mut connection, conversation_id, "sibling", &[], &[])
        .unwrap()
        .0;
    let encoded = serde_json::to_string(
        &reconstruct_model_input(&connection, conversation_id, sibling, false).unwrap(),
    )
    .unwrap();
    assert!(encoded.contains("base"));
    assert!(encoded.contains("sibling"));
    assert!(!encoded.contains("checkpoint branch"));
}

#[test]
fn queued_compaction_preserves_order_and_can_be_promoted_deleted_but_not_edited() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let conversation_id = create_session(
        &mut connection,
        &NewSession {
            workspace: ".".into(),
        },
    )
    .unwrap();
    let (first, _) = queue_input(
        &mut connection,
        conversation_id,
        "first",
        QueueTarget::NextBoundary,
        &[],
        &[],
        &[],
        false,
        false,
    )
    .unwrap();
    let (compact, _) = queue_compact(
        &mut connection,
        conversation_id,
        QueueTarget::EndOfTurn,
        Some("Keep the API migration details prominent."),
    )
    .unwrap();
    let (last, _) = queue_input(
        &mut connection,
        conversation_id,
        "last",
        QueueTarget::EndOfTurn,
        &[],
        &[],
        &[],
        false,
        false,
    )
    .unwrap();
    assert!(first.position < compact.position && compact.position < last.position);
    assert_eq!(compact.kind, crate::QueuedItemKind::Compact);
    assert_eq!(compact.text, "Keep the API migration details prominent.");
    assert!(
        replace_queued(
            &mut connection,
            conversation_id,
            compact.id,
            "message",
            None,
            &[],
            &[],
            &[],
        )
        .unwrap_err()
        .to_string()
        .contains("cannot be edited")
    );
    let (promoted, _) = promote_queued(&mut connection, conversation_id, compact.id).unwrap();
    assert_eq!(promoted.kind, crate::QueuedItemKind::Compact);
    assert_eq!(promoted.target, QueueTarget::NextBoundary);
    assert_eq!(promoted.text, "Keep the API migration details prominent.");
    assert_eq!(
        peek_next_queued(&connection, conversation_id, QueueTarget::NextBoundary)
            .unwrap()
            .unwrap()
            .id,
        first.id
    );
    begin_editing_queued(&connection, conversation_id, first.id).unwrap();
    assert!(
        peek_next_queued(&connection, conversation_id, QueueTarget::EndOfTurn)
            .unwrap()
            .is_none(),
        "an edited item blocks every later queued item"
    );
    replace_queued(
        &mut connection,
        conversation_id,
        first.id,
        "edited first",
        None,
        &[],
        &[],
        &[],
    )
    .unwrap();
    assert_eq!(
        peek_next_queued(&connection, conversation_id, QueueTarget::NextBoundary)
            .unwrap()
            .unwrap()
            .id,
        first.id
    );
    delete_queued(&mut connection, conversation_id, compact.id).unwrap();
    assert_eq!(
        peek_next_queued(&connection, conversation_id, QueueTarget::EndOfTurn)
            .unwrap()
            .unwrap()
            .id,
        last.id
    );
}

#[test]
fn generated_title_is_persisted_without_a_length_cap() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let conversation_id = create_session(
        &mut connection,
        &NewSession {
            workspace: ".".into(),
        },
    )
    .unwrap();
    let long_prompt = "x".repeat(200);
    append_user(&mut connection, conversation_id, &long_prompt, &[], &[]).unwrap();

    let claim = claim_title_generation(&mut connection, conversation_id)
        .unwrap()
        .unwrap();
    assert_eq!(claim.prompt, long_prompt);
    let generated = "A deliberately long generated title that remains intact";
    let ((), event) = finish_title_generation(&mut connection, conversation_id, Some(generated))
        .unwrap()
        .unwrap();

    assert_eq!(
        load_conversation_title(&connection, conversation_id).unwrap(),
        Some(generated.into())
    );
    assert!(matches!(
        event.kind,
        DurableEventKind::ConversationTitleChanged {
            title,
            source: crate::ConversationTitleSource::Generated,
            ..
        } if title == generated
    ));
}

#[test]
fn manual_rename_wins_a_pending_title_generation() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let conversation_id = create_session(
        &mut connection,
        &NewSession {
            workspace: ".".into(),
        },
    )
    .unwrap();
    append_user(&mut connection, conversation_id, "first prompt", &[], &[]).unwrap();
    assert!(
        claim_title_generation(&mut connection, conversation_id)
            .unwrap()
            .is_some()
    );

    rename_conversation(&mut connection, conversation_id, "My chosen name").unwrap();
    assert!(
        finish_title_generation(&mut connection, conversation_id, Some("Generated name"))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        load_conversation_title(&connection, conversation_id).unwrap(),
        Some("My chosen name".into())
    );
}

#[test]
fn manual_name_before_first_message_prevents_title_generation() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let conversation_id = create_session(
        &mut connection,
        &NewSession {
            workspace: ".".into(),
        },
    )
    .unwrap();

    rename_conversation(&mut connection, conversation_id, "My chosen name").unwrap();
    append_user(&mut connection, conversation_id, "first prompt", &[], &[]).unwrap();

    assert!(
        claim_title_generation(&mut connection, conversation_id)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        load_conversation_title(&connection, conversation_id).unwrap(),
        Some("My chosen name".into())
    );
}

#[test]
fn title_updates_preserve_pending_and_streaming_assistant_parents() {
    for source in [
        crate::ConversationTitleSource::Generated,
        crate::ConversationTitleSource::Fallback,
        crate::ConversationTitleSource::Manual,
    ] {
        for during_stream in [false, true] {
            let mut connection = Connection::open_in_memory().unwrap();
            migrate(&mut connection).unwrap();
            let conversation_id = create_session(
                &mut connection,
                &NewSession {
                    workspace: ".".into(),
                },
            )
            .unwrap();
            let ((user, turn), _) =
                append_user(&mut connection, conversation_id, "prompt", &[], &[]).unwrap();
            let streaming = during_stream.then(|| {
                start_assistant(&mut connection, conversation_id, user, turn, None)
                    .unwrap()
                    .0
            });
            let anchor = streaming.unwrap_or(user);
            let event = if source == crate::ConversationTitleSource::Manual {
                rename_conversation(&mut connection, conversation_id, "Manual title")
                    .unwrap()
                    .1
            } else {
                claim_title_generation(&mut connection, conversation_id).unwrap();
                finish_title_generation(
                    &mut connection,
                    conversation_id,
                    (source == crate::ConversationTitleSource::Generated)
                        .then_some("Generated title"),
                )
                .unwrap()
                .unwrap()
                .1
            };
            let history = load_history(&connection, conversation_id).unwrap();
            assert_eq!(history.iter().find(|node| node.active).unwrap().id, anchor);
            let title = history.last().unwrap();
            assert_eq!(title.parent_id, Some(anchor));
            assert_eq!(title.content["system_type"], "title_change");
            assert!(!title.active);
            assert_eq!(
                title
                    .clone()
                    .into_appended_event(conversation_id, event.cursor)
                    .kind,
                event.kind,
                "live and replayed title events must have the same anchor"
            );

            let assistant = streaming.unwrap_or_else(|| {
                start_assistant(&mut connection, conversation_id, user, turn, None)
                    .unwrap()
                    .0
            });
            complete_assistant(
                &mut connection,
                conversation_id,
                assistant,
                &test_response_metadata(),
                vec![],
            )
            .unwrap();
            let history = load_history(&connection, conversation_id).unwrap();
            let active = history.iter().find(|node| node.active).unwrap();
            assert_eq!(active.id, assistant);
            assert_eq!(active.status, crate::NodeStatus::Completed);
            assert!(active.completed_at.is_some());

            // Metadata is harmless, but a genuine branch move must still reject
            // a response prepared for the old execution tip.
            fork(&mut connection, conversation_id, user, &[]).unwrap();
            assert!(matches!(
                start_assistant(&mut connection, conversation_id, assistant, turn, None),
                Err(RuntimeError::StaleActiveParent(id, conversation))
                    if id == assistant && conversation == conversation_id
            ));
        }
    }
}

#[test]
fn title_sidecars_replay_in_order_and_follow_their_branch_on_fork() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let conversation_id = create_session(
        &mut connection,
        &NewSession {
            workspace: ".".into(),
        },
    )
    .unwrap();
    let ((first_user, first_turn), _) =
        append_user(&mut connection, conversation_id, "first", &[], &[]).unwrap();
    let (first, _) = start_assistant(
        &mut connection,
        conversation_id,
        first_user,
        first_turn,
        None,
    )
    .unwrap();
    complete_assistant(
        &mut connection,
        conversation_id,
        first,
        &test_response_metadata(),
        vec![],
    )
    .unwrap();
    claim_title_generation(&mut connection, conversation_id).unwrap();
    finish_title_generation(&mut connection, conversation_id, Some("Generated title")).unwrap();
    rename_conversation(&mut connection, conversation_id, "First rename").unwrap();
    let ((second_user, second_turn), _) =
        append_user(&mut connection, conversation_id, "second", &[], &[]).unwrap();
    let (second, _) = start_assistant(
        &mut connection,
        conversation_id,
        second_user,
        second_turn,
        None,
    )
    .unwrap();
    complete_assistant(
        &mut connection,
        conversation_id,
        second,
        &test_response_metadata(),
        vec![],
    )
    .unwrap();
    rename_conversation(&mut connection, conversation_id, "Second rename").unwrap();

    for (target, expected) in [
        (
            second,
            vec!["Generated title", "First rename", "Second rename"],
        ),
        (first, vec!["Generated title", "First rename"]),
    ] {
        fork(&mut connection, conversation_id, target, &[]).unwrap();
        let page = load_transcript_page_hydration(&connection, conversation_id, None).unwrap();
        let titles = page
            .events
            .iter()
            .filter_map(|event| {
                let TranscriptPageEvent::NodeAppended { cursor, node_id } = event;
                let node = page
                    .history
                    .iter()
                    .find(|node| node.id == *node_id)
                    .unwrap();
                match node
                    .clone()
                    .into_appended_event(conversation_id, *cursor)
                    .kind
                {
                    DurableEventKind::ConversationTitleChanged { title, .. } => Some(title),
                    _ => None,
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(titles, expected);
        assert_eq!(
            load_conversation_title(&connection, conversation_id)
                .unwrap()
                .as_deref(),
            Some("Second rename")
        );
    }
    assert_eq!(
        reconstruct_model_input(&connection, conversation_id, first_user, false).unwrap(),
        vec![ModelInput::Message {
            role: MessageRole::User,
            content: "first".into()
        }]
    );
    let temporary = tempfile::TempDir::new().unwrap();
    let destination = temporary.path().join("title-fork.db");
    let fork_id = ConversationId::new();
    hard_fork(
        &mut connection,
        conversation_id,
        fork_id,
        first,
        &destination,
        &[],
    )
    .unwrap();
    let mut copy = Connection::open(&destination).unwrap();
    let copied_history = load_history(&copy, fork_id).unwrap();
    let copied_titles = copied_history
        .iter()
        .filter(|node| node.content["system_type"] == "title_change")
        .map(|node| node.content["title"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(copied_titles, ["Generated title", "First rename"]);
    assert_eq!(
        load_conversation_title(&copy, fork_id).unwrap().as_deref(),
        Some("Second rename")
    );
    assert_eq!(
        rename_conversation(&mut copy, fork_id, "").unwrap().0,
        "Generated title"
    );

    let (restored, _) = rename_conversation(&mut connection, conversation_id, "").unwrap();
    assert_eq!(restored, "Generated title");
}

#[test]
fn manual_rename_is_an_ordered_typed_system_node() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let conversation_id = create_session(
        &mut connection,
        &NewSession {
            workspace: ".".into(),
        },
    )
    .unwrap();
    append_user(&mut connection, conversation_id, "first prompt", &[], &[]).unwrap();
    let nodes_before = load_history(&connection, conversation_id).unwrap();

    let (_, rename) =
        rename_conversation(&mut connection, conversation_id, "Manual title").unwrap();

    let nodes = load_history(&connection, conversation_id).unwrap();
    assert_eq!(nodes.len(), nodes_before.len() + 1);
    let title = nodes.last().unwrap();
    assert_eq!(title.kind, NodeKind::System);
    assert_eq!(title.content["system_type"], "title_change");
    assert_eq!(title.content["title"], "Manual title");
    assert!(matches!(
        rename.kind,
        DurableEventKind::ConversationTitleChanged { .. }
    ));
    assert!(rename.cursor.0 > 0);
}

#[test]
fn fork_preserves_the_conversation_title_when_selecting_an_older_branch() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let conversation_id = create_session(
        &mut connection,
        &NewSession {
            workspace: ".".into(),
        },
    )
    .unwrap();
    let ((first, _), _) =
        append_user(&mut connection, conversation_id, "first prompt", &[], &[]).unwrap();
    claim_title_generation(&mut connection, conversation_id).unwrap();
    finish_title_generation(&mut connection, conversation_id, Some("Generated title")).unwrap();
    let ((renamed_at, _), _) =
        append_user(&mut connection, conversation_id, "second prompt", &[], &[]).unwrap();
    let (_, rename) =
        rename_conversation(&mut connection, conversation_id, "Manual title").unwrap();
    assert!(matches!(
        rename.kind,
        DurableEventKind::ConversationTitleChanged {
            node_id: Some(node_id),
            ..
        } if node_id == renamed_at
    ));
    let ((after_rename, _), _) =
        append_user(&mut connection, conversation_id, "third prompt", &[], &[]).unwrap();

    fork(&mut connection, conversation_id, after_rename, &[]).unwrap();
    assert_eq!(
        load_conversation_title(&connection, conversation_id).unwrap(),
        Some("Manual title".into())
    );

    fork(&mut connection, conversation_id, renamed_at, &[]).unwrap();
    assert_eq!(
        load_conversation_title(&connection, conversation_id).unwrap(),
        Some("Manual title".into())
    );
    assert!(
        load_history(&connection, conversation_id)
            .unwrap()
            .iter()
            .any(|node| {
                node.parent_id == Some(first)
                    && node.content["system_type"] == "title_change"
                    && node.content["source"] == "generated"
            })
    );
}

#[test]
fn fork_restores_profiles_from_selected_nodes() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let conversation_id = create_session(
        &mut connection,
        &NewSession {
            workspace: ".".into(),
        },
    )
    .unwrap();
    let ((_before_changes, _), _) =
        append_user(&mut connection, conversation_id, "first", &[], &[]).unwrap();
    set_model_selection(
        &mut connection,
        conversation_id,
        "mock",
        "echo-slow",
        None,
        false,
        false,
    )
    .unwrap();
    let ((changed_at, _), _) =
        append_user(&mut connection, conversation_id, "second", &[], &[]).unwrap();
    set_model_selection(
        &mut connection,
        conversation_id,
        "mock",
        "echo-fast",
        None,
        false,
        false,
    )
    .unwrap();
    rename_conversation(&mut connection, conversation_id, "Changed here").unwrap();
    let ((after_changes, _), _) =
        append_user(&mut connection, conversation_id, "third", &[], &[]).unwrap();

    fork(&mut connection, conversation_id, after_changes, &[]).unwrap();
    assert_eq!(
        load_model_selection(&connection, conversation_id)
            .unwrap()
            .unwrap()
            .1,
        "echo-fast"
    );

    fork(&mut connection, conversation_id, changed_at, &[]).unwrap();
    assert_eq!(
        load_model_selection(&connection, conversation_id)
            .unwrap()
            .unwrap()
            .1,
        "echo-slow"
    );
    assert_eq!(
        load_conversation_title(&connection, conversation_id)
            .unwrap()
            .as_deref(),
        Some("Changed here")
    );
}

#[test]
fn model_and_agent_switch_notices_skip_empty_conversations_and_update_consecutive_switches() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let conversation_id = create_session(
        &mut connection,
        &NewSession {
            workspace: ".".into(),
        },
    )
    .unwrap();

    set_model_selection(
        &mut connection,
        conversation_id,
        "mock",
        "first",
        None,
        false,
        false,
    )
    .unwrap();
    assert_eq!(load_history(&connection, conversation_id).unwrap().len(), 1);

    append_user(&mut connection, conversation_id, "hello", &[], &[]).unwrap();
    set_model_selection(
        &mut connection,
        conversation_id,
        "mock",
        "second",
        None,
        false,
        false,
    )
    .unwrap();
    set_model_selection(
        &mut connection,
        conversation_id,
        "mock",
        "third",
        None,
        false,
        false,
    )
    .unwrap();
    set_active_profile(
        &mut connection,
        conversation_id,
        Some("review"),
        None,
        false,
    )
    .unwrap();
    set_active_profile(
        &mut connection,
        conversation_id,
        Some("general"),
        None,
        false,
    )
    .unwrap();

    let notices = load_history(&connection, conversation_id)
        .unwrap()
        .into_iter()
        .filter(|node| node.kind == NodeKind::System)
        .collect::<Vec<_>>();
    assert_eq!(notices.len(), 2);
    assert_eq!(notices[0].content["message"], "Changed model to mock/third");
    assert_eq!(notices[1].content["message"], "Changed agent to general");
    assert!(
        notices
            .iter()
            .all(|node| node.content["transcript"] == "notice")
    );
}

#[test]
fn empty_rename_restores_the_generated_title() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let conversation_id = create_session(
        &mut connection,
        &NewSession {
            workspace: ".".into(),
        },
    )
    .unwrap();
    append_user(&mut connection, conversation_id, "first prompt", &[], &[]).unwrap();
    claim_title_generation(&mut connection, conversation_id).unwrap();
    finish_title_generation(&mut connection, conversation_id, Some("Generated name")).unwrap();
    rename_conversation(&mut connection, conversation_id, "Manual name").unwrap();

    let (title, event) = rename_conversation(&mut connection, conversation_id, "   ").unwrap();

    assert_eq!(title, "Generated name");
    assert_eq!(
        load_conversation_title(&connection, conversation_id).unwrap(),
        Some("Generated name".into())
    );
    assert!(matches!(
        event.kind,
        DurableEventKind::ConversationTitleChanged {
            title,
            source: crate::ConversationTitleSource::Generated,
            ..
        } if title == "Generated name"
    ));
}

#[test]
fn empty_rename_uses_the_first_prompt_when_generation_never_finished() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let conversation_id = create_session(
        &mut connection,
        &NewSession {
            workspace: ".".into(),
        },
    )
    .unwrap();
    append_user(
        &mut connection,
        conversation_id,
        "A useful fallback title\nwith more detail",
        &[],
        &[],
    )
    .unwrap();
    rename_conversation(&mut connection, conversation_id, "Manual name").unwrap();

    let (title, event) = rename_conversation(&mut connection, conversation_id, "").unwrap();

    assert_eq!(title, "A useful fallback title");
    assert!(matches!(
        event.kind,
        DurableEventKind::ConversationTitleChanged {
            source: crate::ConversationTitleSource::Fallback,
            ..
        }
    ));
}

#[test]
fn conversation_titles_reject_empty_multiline_and_control_text() {
    for invalid in ["", "   ", "two\nlines", "escape\u{1b}sequence"] {
        assert!(
            normalize_conversation_title(invalid).is_err(),
            "{invalid:?}"
        );
    }
    assert_eq!(
        normalize_conversation_title("  Unicode title 🦀  ").unwrap(),
        "Unicode title 🦀"
    );
}

#[test]
fn fork_updates_the_active_mode_selection_without_losing_its_source() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let conversation_id = create_session(
        &mut connection,
        &NewSession {
            workspace: ".".into(),
        },
    )
    .unwrap();
    let ((node_id, _), _) =
        append_user(&mut connection, conversation_id, "review", &[], &[]).unwrap();
    set_mode_model_selection(
        &mut connection,
        conversation_id,
        "review",
        "mock",
        "current",
        Some("low"),
        false,
    )
    .unwrap();
    connection
        .execute(
            "UPDATE nodes SET mode = 'review', provider = 'mock', model = 'forked', effort = 'high'
             WHERE id = ?1",
            [node_id.to_string()],
        )
        .unwrap();

    fork(&mut connection, conversation_id, node_id, &[]).unwrap();

    assert_eq!(
        load_active_profiles(&connection, conversation_id).unwrap(),
        ("general".into(), "review".into())
    );
    assert_eq!(
        load_mode_selections(&connection, conversation_id)
            .unwrap()
            .get("review")
            .cloned()
            .flatten(),
        Some((
            "mock".into(),
            "forked".into(),
            Some("high".into()),
            "explicit".into(),
        ))
    );
}

#[test]
fn forked_history_omits_tool_calls_without_results() {
    let mut input = vec![
        ModelInput::ToolCall {
            call_id: "kept".into(),
            name: "apply_patch".into(),
            arguments: json!({"patch": "first"}),
            provider_metadata: serde_json::Value::Null,
        },
        ModelInput::ToolCall {
            call_id: "forked-away".into(),
            name: "apply_patch".into(),
            arguments: json!({"patch": "second"}),
            provider_metadata: serde_json::Value::Null,
        },
        ModelInput::ToolResult {
            call_id: "kept".into(),
            output: json!({"changed": true}),
            is_error: false,
        },
    ];

    retain_complete_tool_exchanges(&mut input);

    assert_eq!(input.len(), 2);
    assert!(input.iter().all(|item| !matches!(
        item,
        ModelInput::ToolCall { call_id, .. } if call_id == "forked-away"
    )));
}

#[test]
fn recovery_interrupts_delegated_work_without_restarting_it() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    connection.execute("INSERT INTO conversations (id, workspace, project_dir, cwd, created_at, updated_at, active_agent, active_mode) VALUES ('conversation', '.', '.', '.', '0', '0', 'default', 'ask')", []).unwrap();
    connection.execute("INSERT INTO agent_runs (id, conversation_id, parent_turn_id, sequence, profile, provider, model, task, status, created_at) VALUES ('queued', 'conversation', 'turn', 0, 'explore', 'mock', 'echo', 'queued task', 'queued', '0'), ('running', 'conversation', 'turn', 1, 'explore', 'mock', 'echo', 'running task', 'running', '0')", []).unwrap();

    recover(&mut connection).unwrap();

    let rows = connection
        .prepare("SELECT status, error, completed_at FROM agent_runs ORDER BY sequence")
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(rows.iter().all(|(status, error, completed)| {
        status == "interrupted"
            && error
                .as_deref()
                .is_some_and(|value| value.contains("stopped"))
            && completed.is_some()
    }));
}

#[test]
fn recovery_does_not_write_a_healthy_conversation() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    create_session(
        &mut connection,
        &NewSession {
            workspace: ".".into(),
        },
    )
    .unwrap();
    let changes = connection.total_changes();

    recover(&mut connection).unwrap();

    assert_eq!(connection.total_changes(), changes);
}

#[test]
fn recovery_marks_persisted_running_terminals_orphaned() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    connection.execute("INSERT INTO conversations (id, workspace, project_dir, cwd, created_at, updated_at, active_agent, active_mode) VALUES ('conversation', '.', '.', '.', '0', '0', 'default', 'ask')", []).unwrap();
    connection.execute("INSERT INTO nodes (id, conversation_id, kind, status, role, content_json, created_at) VALUES ('root', 'conversation', 'conversation_root', 'completed', 'system', '{}', '0')", []).unwrap();
    let content = json!({"name": "bash", "output": {"id": "01900000-0000-7000-8000-000000000000", "status": "running"}});
    connection.execute("INSERT INTO nodes (id, conversation_id, parent_id, turn_id, kind, status, role, content_json, created_at) VALUES ('terminal', 'conversation', 'root', 'turn', 'tool_result', 'completed', 'tool', ?1, '0')", [content.to_string()]).unwrap();
    recover(&mut connection).unwrap();

    let node_status: String = connection
        .query_row(
            "SELECT json_extract(content_json, '$.output.status') FROM nodes WHERE id = 'terminal'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(node_status, "orphaned");
}

#[test]
fn recovery_marks_persisted_terminating_terminals_orphaned() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let conversation_id = create_session(
        &mut connection,
        &NewSession {
            workspace: ".".into(),
        },
    )
    .unwrap();
    let terminal = crate::TerminalSnapshot {
        id: crate::TerminalId::new(),
        owner: conversation_id,
        owner_agent_run_id: None,
        tool_call_node_id: None,
        read_safe: None,
        command: "sleep 30".into(),
        status: crate::TerminalStatus::Terminating,
        created_at: "1".into(),
        started_at: "1".into(),
        completed_at: None,
        exit_code: None,
        output_base: 0,
        output_cursor: 0,
        output_bytes: 0,
        discarded_bytes: 0,
        truncated: false,
        ansi_output: String::new(),
        output: String::new(),
    };
    upsert_terminal(&mut connection, &terminal).unwrap();

    recover(&mut connection).unwrap();

    let recovered = load_terminal(&connection, conversation_id, terminal.id).unwrap();
    assert_eq!(recovered.status, crate::TerminalStatus::Orphaned);
    assert!(recovered.completed_at.is_some());
}

#[test]
fn image_blobs_are_deduplicated_and_unreferenced_blobs_are_pruned() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let first = BlobId::new();
    let duplicate = BlobId::new();

    assert_eq!(
        store_image_blob(&mut connection, first, "same-image", vec![1, 2, 3]).unwrap(),
        first
    );
    assert_eq!(
        store_image_blob(&mut connection, duplicate, "same-image", vec![1, 2, 3]).unwrap(),
        first
    );
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM blobs WHERE codec = 'png'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);

    assert_eq!(prune_unreferenced_image_blobs(&connection).unwrap(), 1);
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM blobs WHERE codec = 'png'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}
