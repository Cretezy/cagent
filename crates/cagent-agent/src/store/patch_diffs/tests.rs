//! Exercise the persistence hooks, not just the diff-recording helper.

#[allow(clippy::wildcard_imports)]
use super::super::*;
use super::{clear_conversation_diff, load_conversation_diff};
use crate::tools::{ConversationDiff, SemanticDiff};

#[test]
fn repeated_edits_reversions_and_file_metadata_preserve_recorded_structure() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let conversation = session(&mut connection, Path::new("."));
    let patches = [
        "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n",
        "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-new\n+old\n",
        "diff --git a/new.rs b/new.rs\nnew file mode 100644\n--- /dev/null\n+++ b/new.rs\n@@ -0,0 +1 @@\n+added\n\\ No newline at end of file\n",
        "diff --git a/new.rs b/new.rs\ndeleted file mode 100644\n--- a/new.rs\n+++ /dev/null\n@@ -1 +0,0 @@\n-added\n\\ No newline at end of file\n",
        "diff --git a/a.rs b/renamed.rs\nsimilarity index 100%\nrename from a.rs\nrename to renamed.rs\n",
    ];
    let diffs: Vec<_> = patches
        .into_iter()
        .map(crate::tools::parse_git_diff)
        .collect();
    for diff in &diffs {
        main_result(
            &mut connection,
            conversation,
            "apply_patch",
            &output(diff),
            false,
        );
    }
    let recorded = load_conversation_diff(&mut connection, conversation)
        .unwrap()
        .diff;
    assert_eq!(recorded.files.len(), 4);
    assert_eq!(
        recorded.files[0].hunks,
        [
            diffs[0].files[0].hunks.clone(),
            diffs[1].files[0].hunks.clone()
        ]
        .concat()
    );
    assert_eq!(recorded.files[0].added_lines, 2);
    assert_eq!(recorded.files[0].removed_lines, 2);
    for (actual, expected) in recorded.files[1..].iter().zip(&diffs[2..]) {
        assert_eq!(actual, &expected.files[0]);
    }
    assert!(recorded.files[1].new_no_final_newline);
    assert!(recorded.files[2].old_no_final_newline);
    assert_eq!(recorded.files[3].kind, crate::tools::DiffFileKind::Renamed);
}

#[test]
fn oversized_diff_is_rejected_before_decode_and_clear_restores_empty_view() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let conversation = session(&mut connection, Path::new("."));
    connection
        .execute(
            "INSERT INTO conversation_patch_diffs(conversation_id, diff_json) VALUES (?1, ?2)",
            params![
                conversation.to_string(),
                "x".repeat(super::MAX_DIFF_BYTES as usize + 1)
            ],
        )
        .unwrap();
    assert!(
        load_conversation_diff(&mut connection, conversation)
            .unwrap_err()
            .to_string()
            .contains("too large")
    );
    clear_conversation_diff(&connection, conversation).unwrap();
    expect_diff(&mut connection, conversation, &[]);
    connection
        .execute(
            "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x < ?2)
         INSERT INTO conversation_patch_diffs(conversation_id, diff_json) SELECT ?1, NULL FROM n",
            params![conversation.to_string(), super::MAX_DIFF_RECORDS + 1],
        )
        .unwrap();
    assert!(
        load_conversation_diff(&mut connection, conversation)
            .unwrap_err()
            .to_string()
            .contains("too large")
    );
}

fn session(connection: &mut Connection, workspace: &Path) -> ConversationId {
    create_session(
        connection,
        &NewSession {
            workspace: workspace.to_path_buf(),
        },
    )
    .unwrap()
}

fn diff(path: &str, text: &str) -> SemanticDiff {
    serde_json::from_value(json!({"files": [{
        "old_path": path,
        "new_path": path,
        "kind": "modified",
        "language": "rust",
        "added_lines": 1,
        "removed_lines": 0,
        "old_no_final_newline": false,
        "new_no_final_newline": false,
        "hunks": [{"header": "@@ -1,0 +1 @@", "lines": [{
            "kind": "addition", "old_line": null, "new_line": 1, "text": text
        }]}]
    }]}))
    .unwrap()
}

fn output(diff: &SemanticDiff) -> serde_json::Value {
    json!({"diff": diff})
}

fn main_call(
    connection: &mut Connection,
    conversation: ConversationId,
    tool: &str,
) -> StoredToolCall {
    let ((user, turn), _) = append_user(connection, conversation, "edit", &[], &[]).unwrap();
    let (assistant, _) = start_assistant(connection, conversation, user, turn, None).unwrap();
    complete_assistant(
        connection,
        conversation,
        assistant,
        &ResponseMetadata {
            provider_request_id: None,
            finish_reason: crate::FinishReason::Stop,
            usage: crate::ModelUsage::default(),
        },
        vec![PendingToolCall {
            provider_call_id: assistant.to_string(),
            name: tool.into(),
            // Intent must never be mistaken for successfully persisted output.
            arguments: json!({"patch": "*** Begin Patch\n*** Add File: intent.rs\n+intent\n*** End Patch"}),
            request_index: 0,
            provider_metadata: serde_json::Value::Null,
        }],
    )
    .unwrap()
    .0
    .pop()
    .unwrap()
}

fn main_result(
    connection: &mut Connection,
    conversation: ConversationId,
    tool: &str,
    output: &serde_json::Value,
    is_error: bool,
) -> NodeId {
    let call = main_call(connection, conversation, tool);
    append_tool_result(connection, conversation, &call, output, is_error, 1, None)
        .unwrap()
        .0
}

fn run(connection: &mut Connection, conversation: ConversationId) -> crate::AgentRunId {
    let ((_, turn), _) = append_user(connection, conversation, "delegate", &[], &[]).unwrap();
    create_agent_run(
        connection,
        conversation,
        turn,
        "general",
        &ModelRef::parse("mock/echo").unwrap(),
        None,
        "edit a file",
    )
    .unwrap()
    .id
}

fn record_count(connection: &Connection, conversation: ConversationId) -> i64 {
    connection
        .query_row(
            "SELECT COUNT(*) FROM conversation_patch_diffs WHERE conversation_id = ?1",
            [conversation.to_string()],
            |row| row.get(0),
        )
        .unwrap()
}

fn expect_diff(connection: &mut Connection, conversation: ConversationId, diffs: &[SemanticDiff]) {
    let mut expected = SemanticDiff::default();
    for diff in diffs {
        expected.merge(diff.clone());
    }
    assert_eq!(
        load_conversation_diff(connection, conversation).unwrap(),
        ConversationDiff {
            diff: expected,
            incomplete_history: false,
        }
    );
}

#[test]
fn main_and_delegated_results_record_success_once_and_isolate_conversations() {
    let mut connection = Connection::open_in_memory().unwrap();
    configure(&connection).unwrap();
    migrate(&mut connection).unwrap();
    let conversation = session(&mut connection, Path::new("."));
    let other = session(&mut connection, Path::new("."));
    let delegated = run(&mut connection, conversation);
    let main = diff("shared.rs", "main edit");
    let delegated_diff = diff("shared.rs", "delegated edit");
    let excluded = diff("excluded.rs", "must not appear");

    main_result(
        &mut connection,
        conversation,
        "apply_patch",
        &output(&main),
        false,
    );
    append_agent_run_activity(
        &mut connection,
        conversation,
        delegated,
        "apply_patch",
        &json!({}),
        &output(&delegated_diff),
        false,
        None,
    )
    .unwrap();
    for (tool, is_error) in [("apply_patch", true), ("bash", false)] {
        main_result(
            &mut connection,
            conversation,
            tool,
            &output(&excluded),
            is_error,
        );
        append_agent_run_activity(
            &mut connection,
            conversation,
            delegated,
            tool,
            &json!({}),
            &output(&excluded),
            is_error,
            None,
        )
        .unwrap();
    }
    // Merely completing a call with patch arguments does not record a diff.
    main_call(&mut connection, conversation, "apply_patch");
    main_result(
        &mut connection,
        other,
        "apply_patch",
        &output(&excluded),
        false,
    );
    let other_run = run(&mut connection, other);
    append_agent_run_activity(
        &mut connection,
        other,
        other_run,
        "apply_patch",
        &json!({}),
        &output(&excluded),
        false,
        None,
    )
    .unwrap();
    assert!(
        append_agent_run_activity(
            &mut connection,
            conversation,
            other_run,
            "apply_patch",
            &json!({}),
            &output(&excluded),
            false,
            None,
        )
        .is_err()
    );

    for _ in 0..2 {
        expect_diff(
            &mut connection,
            conversation,
            &[main.clone(), delegated_diff.clone()],
        );
        expect_diff(
            &mut connection,
            other,
            &[excluded.clone(), excluded.clone()],
        );
        assert_eq!(record_count(&connection, conversation), 2);
        assert_eq!(record_count(&connection, other), 2);
    }
}

#[test]
fn delegated_diff_is_retained_before_activity_output_is_truncated() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let conversation = session(&mut connection, Path::new("."));
    let delegated = run(&mut connection, conversation);
    let full = diff("large.rs", &"complete delegated output".repeat(1024));
    append_agent_run_activity(
        &mut connection,
        conversation,
        delegated,
        "apply_patch",
        &json!({}),
        &output(&full),
        false,
        None,
    )
    .unwrap();

    let activity = load_agent_run(&connection, conversation, delegated).unwrap();
    assert_eq!(activity.activity.len(), 1);
    assert_eq!(activity.activity[0].output["truncated"], true);
    assert!(activity.activity[0].output.get("diff").is_none());
    expect_diff(&mut connection, conversation, &[full]);
    assert_eq!(record_count(&connection, conversation), 1);
}

// Simulate the preceding schema without changing historical tool payloads.
fn mark_previous_schema(connection: &Connection) {
    connection
        .execute_batch(
            "DELETE FROM schema_migrations WHERE version >= 12;
         INSERT OR IGNORE INTO schema_migrations(version, applied_at) VALUES (11, 'legacy');",
        )
        .unwrap();
}

#[test]
fn migration_never_uses_a_compact_main_preview_when_full_output_is_missing() {
    let mut connection = Connection::open_in_memory().unwrap();
    migrate(&mut connection).unwrap();
    let conversation = session(&mut connection, Path::new("."));
    let full = diff("lost.rs", &"x".repeat(INLINE_TOOL_OUTPUT_BYTES + 1));
    for remove_reference in [false, true] {
        let node = main_result(
            &mut connection,
            conversation,
            "apply_patch",
            &output(&full),
            false,
        );
        if remove_reference {
            connection.execute(
                "UPDATE nodes SET content_json = json_remove(content_json, '$.output_blob_id') WHERE id = ?1",
                [node.to_string()],
            ).unwrap();
        }
    }
    connection.execute("DELETE FROM blobs", []).unwrap();
    connection
        .execute("DROP TABLE conversation_patch_diffs", [])
        .unwrap();
    mark_previous_schema(&connection);
    migrate(&mut connection).unwrap();
    let loaded = load_conversation_diff(&mut connection, conversation).unwrap();
    assert!(loaded.diff.files.is_empty());
    assert!(loaded.incomplete_history);
    assert_eq!(record_count(&connection, conversation), 2);
    clear_conversation_diff(&connection, conversation).unwrap();
    migrate(&mut connection).unwrap();
    expect_diff(&mut connection, conversation, &[]);
}

#[test]
fn clear_preserves_history_and_files_across_reopen_migration_and_new_results() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("conversation.sqlite3");
    let file = directory.path().join("edited.rs");
    std::fs::write(&file, "already edited\n").unwrap();
    let mut connection = Connection::open(&path).unwrap();
    configure(&connection).unwrap();
    migrate(&mut connection).unwrap();
    let conversation = session(&mut connection, directory.path());
    let other = session(&mut connection, directory.path());
    let old = diff("edited.rs", "already edited");
    main_result(
        &mut connection,
        conversation,
        "apply_patch",
        &output(&old),
        false,
    );
    let delegated = run(&mut connection, conversation);
    append_agent_run_activity(
        &mut connection,
        conversation,
        delegated,
        "apply_patch",
        &json!({}),
        &output(&old),
        false,
        None,
    )
    .unwrap();
    main_result(&mut connection, other, "apply_patch", &output(&old), false);
    // This tool started before clear; persistence order, not start time, decides its period.
    let in_flight = main_call(&mut connection, conversation, "apply_patch");
    let history = format!("{:?}", load_history(&connection, conversation).unwrap());
    let activity = format!(
        "{:?}",
        load_agent_run(&connection, conversation, delegated).unwrap()
    );

    clear_conversation_diff(&connection, conversation).unwrap();
    clear_conversation_diff(&connection, conversation).unwrap();
    expect_diff(&mut connection, conversation, &[]);
    drop(connection);

    let mut connection = Connection::open(&path).unwrap();
    configure(&connection).unwrap();
    migrate(&mut connection).unwrap();
    expect_diff(&mut connection, conversation, &[]);
    // Even an actual migration pass must respect the existing, cleared table.
    mark_previous_schema(&connection);
    migrate(&mut connection).unwrap();
    expect_diff(&mut connection, conversation, &[]);
    expect_diff(&mut connection, other, &[old]);
    assert_eq!(
        format!("{:?}", load_history(&connection, conversation).unwrap()),
        history
    );
    assert_eq!(
        format!(
            "{:?}",
            load_agent_run(&connection, conversation, delegated).unwrap()
        ),
        activity
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "already edited\n");

    let later_main = diff("edited.rs", "later main edit");
    let later_delegated = diff("edited.rs", "later delegated edit");
    append_tool_result(
        &mut connection,
        conversation,
        &in_flight,
        &output(&later_main),
        false,
        1,
        None,
    )
    .unwrap();
    append_agent_run_activity(
        &mut connection,
        conversation,
        delegated,
        "apply_patch",
        &json!({}),
        &output(&later_delegated),
        false,
        None,
    )
    .unwrap();
    drop(connection);
    let mut connection = Connection::open(&path).unwrap();
    migrate(&mut connection).unwrap();
    expect_diff(
        &mut connection,
        conversation,
        &[later_main, later_delegated],
    );
    assert_eq!(record_count(&connection, conversation), 2);
}

#[test]
fn migration_recovers_main_blob_and_marks_truncated_delegation_incomplete_once() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("legacy.sqlite3");
    let mut connection = Connection::open(&path).unwrap();
    configure(&connection).unwrap();
    migrate(&mut connection).unwrap();
    let conversation = session(&mut connection, directory.path());
    let other = session(&mut connection, directory.path());
    let main = diff("blob.rs", &"x".repeat(INLINE_TOOL_OUTPUT_BYTES + 1));
    let inline = diff("inline.rs", "inline main output");
    let small_delegated = diff("delegated.rs", "inline delegated output");
    let truncated = diff("unrecoverable.rs", &"y".repeat(20 * 1024));
    let node = main_result(
        &mut connection,
        conversation,
        "apply_patch",
        &output(&main),
        false,
    );
    let content: String = connection
        .query_row(
            "SELECT content_json FROM nodes WHERE id = ?1",
            [node.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    let content: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert!(content["output_blob_id"].is_string());
    assert_ne!(content["output"]["diff"], output(&main)["diff"]);
    main_result(
        &mut connection,
        conversation,
        "apply_patch",
        &output(&inline),
        false,
    );
    let delegated = run(&mut connection, conversation);
    for diff in [&small_delegated, &truncated] {
        append_agent_run_activity(
            &mut connection,
            conversation,
            delegated,
            "apply_patch",
            &json!({"patch": "intent only"}),
            &output(diff),
            false,
            None,
        )
        .unwrap();
    }
    // Failed and unrelated records must not produce either diffs or missing-history markers.
    for (tool, is_error) in [("apply_patch", true), ("bash", false)] {
        main_result(&mut connection, other, tool, &output(&inline), is_error);
        let other_run = run(&mut connection, other);
        append_agent_run_activity(
            &mut connection,
            other,
            other_run,
            tool,
            &json!({}),
            &output(&truncated),
            is_error,
            None,
        )
        .unwrap();
    }
    main_call(&mut connection, other, "apply_patch");
    connection
        .execute_batch("DROP TABLE conversation_patch_diffs;")
        .unwrap();
    mark_previous_schema(&connection);
    drop(connection);

    let mut connection = Connection::open(&path).unwrap();
    configure(&connection).unwrap();
    migrate(&mut connection).unwrap();
    let mut expected = main;
    expected.merge(inline);
    expected.merge(small_delegated);
    for _ in 0..2 {
        assert_eq!(
            load_conversation_diff(&mut connection, conversation).unwrap(),
            ConversationDiff {
                diff: expected.clone(),
                incomplete_history: true,
            }
        );
        assert_eq!(record_count(&connection, conversation), 4);
        expect_diff(&mut connection, other, &[]);
        assert_eq!(record_count(&connection, other), 0);
        migrate(&mut connection).unwrap();
    }
    clear_conversation_diff(&connection, conversation).unwrap();
    drop(connection);
    let mut connection = Connection::open(&path).unwrap();
    migrate(&mut connection).unwrap();
    expect_diff(&mut connection, conversation, &[]);
}
