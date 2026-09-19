//! Rendering regression tests (transcript and UI behavior).

use super::*;
use crate::render::transcript;

#[test]
fn crossterm_streaming_and_committed_history_match_test_backend() {
    use std::cell::RefCell;
    use std::io::Write;
    use std::rc::Rc;

    use ratatui::backend::CrosstermBackend;
    use ratatui::{TerminalOptions, Viewport};
    use tui_term::vt100;

    #[derive(Clone, Default)]
    struct Output(Rc<RefCell<Vec<u8>>>);

    impl Write for Output {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn vt_color(color: Color) -> vt100::Color {
        match color {
            Color::Reset => vt100::Color::Default,
            Color::Black => vt100::Color::Idx(0),
            Color::Red => vt100::Color::Idx(1),
            Color::Green => vt100::Color::Idx(2),
            Color::Yellow => vt100::Color::Idx(3),
            Color::Blue => vt100::Color::Idx(4),
            Color::Magenta => vt100::Color::Idx(5),
            Color::Cyan => vt100::Color::Idx(6),
            Color::Gray => vt100::Color::Idx(7),
            Color::DarkGray => vt100::Color::Idx(8),
            Color::LightRed => vt100::Color::Idx(9),
            Color::LightGreen => vt100::Color::Idx(10),
            Color::LightYellow => vt100::Color::Idx(11),
            Color::LightBlue => vt100::Color::Idx(12),
            Color::LightMagenta => vt100::Color::Idx(13),
            Color::LightCyan => vt100::Color::Idx(14),
            Color::White => vt100::Color::Idx(15),
            Color::Indexed(index) => vt100::Color::Idx(index),
            Color::Rgb(r, g, b) => vt100::Color::Rgb(r, g, b),
        }
    }

    for (width, height) in [(80, 24), (37, 12)] {
        let new_app = || {
            App::new(
                Path::new("/workspace"),
                ("mock".into(), "echo".into(), None),
                Default::default(),
                "ask",
                None,
            )
        };
        let mut actual_app = new_app();
        let mut expected_app = new_app();
        expected_app.session_id = actual_app.session_id;
        let output = Output::default();
        // A fixed viewport avoids querying a real TTY, but retains Terminal::draw's
        // real buffer diff, cursor commands, and Crossterm ANSI serialization.
        let mut actual = Terminal::with_options(
            CrosstermBackend::new(output.clone()),
            TerminalOptions {
                viewport: Viewport::Fixed(Rect::new(0, 0, width, height)),
            },
        )
        .unwrap();
        let mut expected = Terminal::new(TestBackend::new(width, height)).unwrap();
        let mut parser = vt100::Parser::new(height, width, 0);
        let mut saw_links = false;
        let mut draw = |actual_app: &mut App, expected_app: &mut App, stage: &str| {
            actual
                .draw(|frame| {
                    // Link text and styling are covered. The separate stdout-only
                    // OSC 8 overlay writer is deliberately not emulated here.
                    saw_links |= !actual_app.render_frame(frame).is_empty();
                })
                .unwrap();
            expected
                .draw(|frame| drop(expected_app.render_frame(frame)))
                .unwrap();
            // Neither terminal nor parser is reset between draws: stale cells,
            // accidental scrolling, and cursor drift must remain observable.
            let bytes = std::mem::take(&mut *output.0.borrow_mut());
            parser.process(&bytes);
            for y in 0..height {
                for x in 0..width {
                    let want = &expected.backend().buffer()[(x, y)];
                    let got = parser.screen().cell(y, x).unwrap();
                    let symbol = if got.contents().is_empty() {
                        " "
                    } else {
                        got.contents()
                    };
                    assert_eq!(
                        (
                            symbol,
                            got.fgcolor(),
                            got.bgcolor(),
                            got.bold(),
                            got.dim(),
                            got.italic(),
                            got.underline(),
                            got.inverse(),
                        ),
                        (
                            want.symbol(),
                            vt_color(want.fg),
                            vt_color(want.bg),
                            want.modifier.contains(Modifier::BOLD),
                            want.modifier.contains(Modifier::DIM),
                            want.modifier.contains(Modifier::ITALIC),
                            want.modifier.contains(Modifier::UNDERLINED),
                            want.modifier.contains(Modifier::REVERSED),
                        ),
                        "{width}x{height}, {stage}, cell ({x}, {y}); ANSI: {:?}\nVT100:\n{}",
                        String::from_utf8_lossy(&bytes),
                        parser.screen().contents(),
                    );
                }
            }
        };

        draw(&mut actual_app, &mut expected_app, "initial welcome");
        for turn in 0..3 {
            for app in [&mut actual_app, &mut expected_app] {
                app.push_user(&format!("Turn {turn}: stream a long response"), &[]);
            }
            let mut source = String::new();
            for chunk in 0..32 {
                source.push_str(&format!(
                    "Turn {turn} chunk {chunk}: **bold** and [linked text](https://example.com/{turn}/{chunk}) with enough words to wrap.\n\n"
                ));
                for app in [&mut actual_app, &mut expected_app] {
                    app.streaming_source.clone_from(&source);
                    app.streaming = cagent_agent::presentation::parse_streaming_markdown(&source);
                    // Match the event path's invalidation when streaming changes.
                    app.streaming_layout = None;
                }
                let stage = format!("turn {turn}, streaming chunk {chunk}");
                draw(&mut actual_app, &mut expected_app, &stage);
                draw(
                    &mut actual_app,
                    &mut expected_app,
                    "unchanged streaming draw",
                );
            }
            for app in [&mut actual_app, &mut expected_app] {
                app.history.push(crate::app::local_transcript_block(
                    cagent_agent::protocol::TranscriptBlockKind::Assistant {
                        document: cagent_agent::presentation::parse_markdown(&source),
                        source: source.clone(),
                        message: None,
                    },
                ));
                app.history_layout.rendered = None;
                app.streaming_source.clear();
                app.streaming.clear();
                app.streaming_layout = None;
            }
            draw(&mut actual_app, &mut expected_app, "committed response");
            draw(
                &mut actual_app,
                &mut expected_app,
                "unchanged committed draw",
            );
            for app in [&mut actual_app, &mut expected_app] {
                app.follow_history_tail = false;
                app.history_scroll = 0;
            }
            draw(&mut actual_app, &mut expected_app, "scroll to welcome");
            for app in [&mut actual_app, &mut expected_app] {
                app.follow_history_tail = true;
                app.history_scroll = usize::MAX;
            }
            draw(&mut actual_app, &mut expected_app, "return to tail");
            draw(
                &mut actual_app,
                &mut expected_app,
                "unchanged returned tail",
            );
        }
        assert!(saw_links, "fixture must exercise hyperlink-bearing rows");
    }
}

#[test]
fn markdown_link_clicks_default_on_and_follow_visible_content() {
    let mut app = App::new(
        Path::new("/workspace"),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    let source = "[site](https://example.com) https://example.org [bad](javascript:alert(1))";
    app.history = vec![crate::app::local_transcript_block(
        cagent_agent::protocol::TranscriptBlockKind::Assistant {
            document: cagent_agent::presentation::parse_markdown(source),
            source: source.into(),
            message: None,
        },
    )];
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| drop(app.render_frame(frame)))
        .unwrap();
    assert!(app.open_links);
    assert_eq!(app.link_hits.len(), 2);
    let destinations: Vec<_> = (0..24)
        .flat_map(|row| (0..80).map(move |column| (row, column)))
        .filter_map(|(row, column)| app.link_at(row, column))
        .collect();
    assert!(destinations.contains(&"https://example.com"));
    assert!(destinations.contains(&"https://example.org"));

    app.open_links = false;
    assert!((0..80).all(|column| app.link_at(0, column).is_none()));
    terminal
        .draw(|frame| {
            // Disabling app clicks retains native terminal hyperlink metadata.
            assert_eq!(app.render_frame(frame).len(), 2);
        })
        .unwrap();
    assert!(app.link_hits.is_empty());

    app.open_links = true;
    app.ui_editor = cagent_agent::config::UiEditor::Disabled;
    app.surfaces.push(Surface::Expanded {
        view: ExpandedView::WebFetch {
            url: "https://example.net".into(),
            redirected_url: None,
            format: cagent_agent::WebFetchFormat::Markdown,
            output: "[expanded](https://expanded.example.com)".into(),
        },
        scroll: 0,
        viewport_rows: 24,
    });
    terminal
        .draw(|frame| drop(app.render_frame(frame)))
        .unwrap();
    assert!((0..24).any(|row| {
        (0..80).any(|column| app.link_at(row, column) == Some("https://expanded.example.com"))
    }));

    app.surfaces.clear();
    app.history.clear();
    app.history_layout = Default::default();
    terminal
        .draw(|frame| drop(app.render_frame(frame)))
        .unwrap();
    assert!(app.link_hits.is_empty());
}

#[test]
fn retried_response_notice_is_bold() {
    let block =
        crate::app::local_transcript_block(cagent_agent::protocol::TranscriptBlockKind::Notice {
            message: "Retried response".into(),
        });
    let rows = transcript::history_item_rows(&block, Path::new("/workspace"), 80);

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].line.to_string().trim_end(), "• Retried response");
    assert!(
        rows[0].line.spans[1]
            .style
            .add_modifier
            .contains(Modifier::BOLD)
    );
}

#[test]
fn collapsible_activity_keeps_first_appearance_order_and_can_expand() {
    let read = crate::app::test_tool_groups(vec![ToolActivityGroup::Exploration {
        active: false,
        activities: vec![ExplorationActivity {
            kind: ExplorationActivityKind::Read,
            targets: vec!["a.rs".into(), "b.rs".into()],
            scopes: Vec::new(),
        }],
    }]);
    let bash = crate::app::test_tool_groups(vec![ToolActivityGroup::Bash {
        node_id: None,
        terminal_id: None,
        command: "cargo check".into(),
        status: ToolActivityStatus::Succeeded,
        output: None,
        ansi_output: None,
        exit_code: Some(0),
    }]);
    let history = vec![read, bash];
    let items = history
        .iter()
        .map(|block| transcript::history_item_rows(block, Path::new("/workspace"), 80))
        .collect::<Vec<_>>();
    let rows = transcript::history_rows_from_items_with_collapsed_activity(
        &[],
        None,
        &history,
        &items,
        Path::new("/workspace"),
        cagent_agent::protocol::ConversationId::new(),
        80,
        true,
        &Default::default(),
    );
    assert_eq!(
        rows[0].line.to_string().trim_end(),
        "• Read 2 files, ran 1 command (click to view)"
    );
    assert_eq!(
        rows[0].line.spans[1].style,
        Style::default().add_modifier(Modifier::BOLD)
    );
    assert_eq!(rows[0].line.spans[2].style, DIM_STYLE);

    let target = crate::markdown::ActivityCollapseTarget::Transcript(history[0].id.clone());
    let rows = transcript::history_rows_from_items_with_collapsed_activity(
        &[],
        None,
        &history,
        &items,
        Path::new("/workspace"),
        cagent_agent::protocol::ConversationId::new(),
        80,
        true,
        &std::collections::HashSet::from([target]),
    );
    assert!(rows[0].line.to_string().contains("click to hide"));
    assert!(rows.iter().skip(1).any(|row| row.line.style == USER_STYLE));

    let diff_style = crate::render::diff::DIFF_ADDITION_STYLE;
    let detail = crate::markdown::DisplayRow::plain(Line::from("+added").style(diff_style));
    let expanded = transcript::expanded_activity_rows(
        &[detail],
        &crate::markdown::ActivityCollapseTarget::Transcript(history[0].id.clone()),
        24,
    );
    assert_eq!(expanded[1].line.style, diff_style);
    assert_eq!(expanded[1].line.width(), 24);
}

#[test]
fn web_search_and_fetch_are_collapsed_and_can_expand() {
    let search = crate::app::test_tool_groups(vec![ToolActivityGroup::WebSearch {
        provider: "exa".into(),
        query: "rust tui".into(),
        status: ToolActivityStatus::Succeeded,
        result_count: Some(2),
        results: Vec::new(),
    }]);
    let fetch = crate::app::test_tool_groups(vec![ToolActivityGroup::WebFetch {
        node_id: None,
        url: "https://example.com".into(),
        redirected_url: None,
        format: cagent_agent::WebFetchFormat::Markdown,
        content_type: Some("text/html".into()),
        status: ToolActivityStatus::Succeeded,
        output: Some("Example page".into()),
    }]);
    let history = vec![search, fetch];
    let items = history
        .iter()
        .map(|block| transcript::history_item_rows(block, Path::new("/workspace"), 80))
        .collect::<Vec<_>>();

    let rows = transcript::history_rows_from_items_with_collapsed_activity(
        &[],
        None,
        &history,
        &items,
        Path::new("/workspace"),
        cagent_agent::protocol::ConversationId::new(),
        80,
        true,
        &Default::default(),
    );
    assert_eq!(
        rows[0].line.to_string().trim_end(),
        "• Ran 1 web search, fetched 1 web page (click to view)"
    );
    assert_eq!(rows.len(), 1);

    let target = crate::markdown::ActivityCollapseTarget::Transcript(history[0].id.clone());
    let rows = transcript::history_rows_from_items_with_collapsed_activity(
        &[],
        None,
        &history,
        &items,
        Path::new("/workspace"),
        cagent_agent::protocol::ConversationId::new(),
        80,
        true,
        &std::collections::HashSet::from([target]),
    );
    assert!(
        rows.iter()
            .any(|row| row.line.to_string().contains("Web search exa"))
    );
    assert!(
        rows.iter()
            .any(|row| row.line.to_string().contains("Web Fetch"))
    );
}

#[test]
fn pending_and_completed_edits_render_as_the_same_collapsed_summary() {
    let pending = crate::app::test_tool_groups(vec![ToolActivityGroup::Tool {
        label: "Edit".into(),
        target: Some("src/main.rs".into()),
        status: ToolActivityStatus::Pending,
    }]);
    let completed = crate::app::test_tool_groups(vec![ToolActivityGroup::Edit {
        diff: cagent_agent::tools::SemanticDiff {
            files: vec![cagent_agent::tools::DiffFile {
                old_path: Some("src/main.rs".into()),
                new_path: Some("src/main.rs".into()),
                kind: cagent_agent::tools::DiffFileKind::Modified,
                language: Some("rust".into()),
                added_lines: 1,
                removed_lines: 1,
                old_no_final_newline: false,
                new_no_final_newline: false,
                hunks: Vec::new(),
            }],
        },
        status: ToolActivityStatus::Succeeded,
    }]);

    for history in [vec![pending], vec![completed]] {
        let items = history
            .iter()
            .map(|block| transcript::history_item_rows(block, Path::new("/workspace"), 80))
            .collect::<Vec<_>>();
        let uncollapsed = transcript::history_rows_from_items_with_collapsed_activity(
            &[],
            None,
            &history,
            &items,
            Path::new("/workspace"),
            cagent_agent::protocol::ConversationId::new(),
            80,
            false,
            &Default::default(),
        );
        assert!(
            uncollapsed
                .iter()
                .all(|row| !row.line.to_string().contains("click to view"))
        );

        let rows = transcript::history_rows_from_items_with_collapsed_activity(
            &[],
            None,
            &history,
            &items,
            Path::new("/workspace"),
            cagent_agent::protocol::ConversationId::new(),
            80,
            true,
            &Default::default(),
        );

        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].line.to_string().trim_end(),
            "• Edited 1 file (click to view)"
        );
    }
}

#[test]
fn active_collapsed_tail_uses_a_pulsing_dot() {
    let activity = crate::app::test_tool_groups(vec![ToolActivityGroup::Exploration {
        active: true,
        activities: vec![ExplorationActivity {
            kind: ExplorationActivityKind::Read,
            targets: vec!["src/main.rs".into()],
            scopes: Vec::new(),
        }],
    }]);
    let items = vec![transcript::history_item_rows(
        &activity,
        Path::new("/workspace"),
        80,
    )];
    let rows = transcript::history_rows_from_items_with_collapsed_activity(
        &[],
        None,
        std::slice::from_ref(&activity),
        &items,
        Path::new("/workspace"),
        cagent_agent::protocol::ConversationId::new(),
        80,
        true,
        &Default::default(),
    );

    let Some(Color::Rgb(red, green, blue)) = rows[0].line.spans[0].style.fg else {
        panic!("active collapsed tail dot should use a grayscale RGB pulse");
    };
    assert_eq!(red, green);
    assert_eq!(green, blue);
    assert!((110..=255).contains(&red));
}

#[test]
fn recap_blocks_render_as_one_dimmed_assistant_style_line() {
    let block =
        crate::app::local_transcript_block(cagent_agent::protocol::TranscriptBlockKind::Recap {
            text: "Implementation is complete. Run the final checks next.".into(),
        });
    let rows = transcript::history_item_rows(&block, Path::new("/workspace"), 40);
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].line.to_string(),
        "• Recap: Implementation is complete. Ru…"
    );
    assert!(
        rows[0].line.spans[0]
            .style
            .add_modifier
            .contains(Modifier::DIM)
    );
    assert!(
        rows[0].line.spans[1]
            .style
            .add_modifier
            .contains(Modifier::DIM)
    );
    assert!(transcript::is_non_message_block(&block));
}

#[test]
fn interruption_blocks_style_the_whole_line_by_reason() {
    for (queued_steering, expected_text, expected_style) in [
        (false, "■ Conversation interrupted", crate::app::ERROR_STYLE),
        (
            true,
            "• Model interrupted to submit steering instructions",
            crate::app::NOTICE_STYLE,
        ),
    ] {
        let block = crate::app::local_transcript_block(
            cagent_agent::protocol::TranscriptBlockKind::Interrupt { queued_steering },
        );
        let rows = transcript::history_item_rows(&block, Path::new("/workspace"), 80);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].line.to_string(), expected_text);
        assert_eq!(rows[0].line.spans[0].content, expected_text);
        assert_eq!(rows[0].line.spans[0].style, expected_style);
    }
}

#[test]
fn completed_edit_groups_expose_existing_file_paths_for_clicks() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("test.md");
    std::fs::write(&path, "# Test\n").unwrap();
    let diff = cagent_agent::tools::SemanticDiff {
        files: vec![cagent_agent::tools::DiffFile {
            old_path: Some(PathBuf::from("test.md")),
            new_path: Some(PathBuf::from("test.md")),
            kind: cagent_agent::tools::DiffFileKind::Modified,
            language: Some("markdown".into()),
            added_lines: 98,
            removed_lines: 0,
            old_no_final_newline: false,
            new_no_final_newline: false,
            hunks: vec![cagent_agent::tools::DiffHunk {
                header: "@@ -1,1 +1,1 @@".into(),
                lines: vec![cagent_agent::tools::DiffLine {
                    kind: cagent_agent::tools::DiffLineKind::Addition,
                    old_line: None,
                    new_line: Some(1),
                    text: "# Test".into(),
                }],
            }],
        }],
    };
    let rows = crate::render::transcript::tool_group_rows(
        &[ToolActivityGroup::Edit {
            diff: diff.clone(),
            status: ToolActivityStatus::Succeeded,
        }],
        temporary.path(),
        80,
    );
    let summary = rows
        .iter()
        .find(|row| row.line.to_string().contains("Edited test.md (+98 -0)"))
        .expect("edit summary");
    assert!(
        summary
            .paths()
            .iter()
            .any(|segment| segment.target.path == path)
    );
    assert!(rows.iter().any(|row| {
        row.diff_line()
            .is_some_and(|target| target.file_index == 0 && target.new_line == Some(1))
    }));

    let mut hits = Vec::new();
    append_display_row_path_hits(&mut hits, summary, 4, 0, 80);
    assert!(hits.iter().any(|hit| hit.path == path));

    let mut app = App::new(
        temporary.path(),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    app.history = vec![crate::app::test_tool_groups(vec![
        ToolActivityGroup::Edit {
            diff,
            status: ToolActivityStatus::Succeeded,
        },
    ])];
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| drop(app.render_frame(frame)))
        .unwrap();
    assert!(app.path_hits.iter().any(|hit| hit.path == path));
    assert!(
        app.diff_hits
            .iter()
            .any(|hit| hit.target.new_line == Some(1))
    );

    app.ui_editor = cagent_agent::config::UiEditor::Disabled;
    terminal
        .draw(|frame| drop(app.render_frame(frame)))
        .unwrap();
    assert!(app.path_hits.is_empty());
    assert!(
        app.diff_hits
            .iter()
            .any(|hit| hit.target.new_line == Some(1))
    );

    let deleted = cagent_agent::tools::SemanticDiff {
        files: vec![cagent_agent::tools::DiffFile {
            old_path: Some(PathBuf::from("test.md")),
            new_path: None,
            kind: cagent_agent::tools::DiffFileKind::Deleted,
            language: Some("markdown".into()),
            added_lines: 0,
            removed_lines: 1,
            old_no_final_newline: false,
            new_no_final_newline: false,
            hunks: Vec::new(),
        }],
    };
    let deleted_rows = crate::render::transcript::tool_group_rows(
        &[ToolActivityGroup::Edit {
            diff: deleted,
            status: ToolActivityStatus::Succeeded,
        }],
        temporary.path(),
        80,
    );
    assert_eq!(deleted_rows.iter().flat_map(|row| row.paths()).count(), 0);
}

#[test]
fn assistant_plan_and_clear_accepted_plan_references_produce_line_aware_hits() {
    let temporary = tempfile::tempdir().unwrap();
    let file = temporary.path().join("src.rs");
    std::fs::write(&file, "a\nb\n").unwrap();
    let source = "Open @src.rs:2";
    let document = cagent_agent::presentation::parse_markdown(source);
    let blocks = [
        crate::app::local_transcript_block(
            cagent_agent::protocol::TranscriptBlockKind::Assistant {
                document: document.clone(),
                source: source.into(),
                message: None,
            },
        ),
        crate::app::local_transcript_block(cagent_agent::protocol::TranscriptBlockKind::Plan {
            document: document.clone(),
            source: source.into(),
        }),
        crate::app::local_transcript_block(
            cagent_agent::protocol::TranscriptBlockKind::AcceptedPlan {
                document,
                source: source.into(),
                clear_context: true,
                compact_context: false,
                compaction_summary: None,
            },
        ),
    ];
    for block in &blocks {
        let rows = crate::render::transcript::history_item_rows_with_editor(
            block,
            temporary.path(),
            24,
            true,
        );
        assert!(
            rows.iter()
                .flat_map(|row| row.paths())
                .any(|segment| { segment.target.path == file && segment.target.line == Some(2) })
        );
        let rendered = rows
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("Open src.rs:2"));
        assert!(!rendered.contains("@src.rs"));
    }

    let streaming = crate::render::transcript::streaming_rows(
        &cagent_agent::presentation::parse_streaming_markdown(source),
        None,
        24,
        temporary.path(),
        true,
    );
    assert!(
        streaming
            .iter()
            .flat_map(|row| row.paths())
            .any(|segment| { segment.target.path == file && segment.target.line == Some(2) })
    );
    assert!(!streaming.iter().any(|row| row.to_string().contains('@')));
    let disabled = crate::render::transcript::streaming_rows(
        &cagent_agent::presentation::parse_streaming_markdown(source),
        None,
        24,
        temporary.path(),
        false,
    );
    assert_eq!(disabled.iter().flat_map(|row| row.paths()).count(), 0);
}

fn test_session_id() -> cagent_agent::protocol::ConversationId {
    "cagent-0.1.0-260214.1432.950-1ae287ab"
        .parse()
        .expect("valid test session ID")
}

#[test]
fn frame_layout_uses_the_draw_time_size_and_keeps_newline_context() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    app.render_width = 64;
    app.render_height = 33;
    app.surfaces.push(Surface::AgentWizard {
        name: "review".into(),
        description: String::new(),
        parent: String::new(),
        parents: vec!["None".into()],
        parent_list: crate::app::list::ListState::selectable(1),
        prompt: MultilineInput::new("aaaaa\n".into(), 6),
        availability: 0,
        step: AgentWizardStep::Prompt,
        cursor: 6,
        editing: false,
        original_name: None,
    });
    let mut terminal = Terminal::new(TestBackend::new(63, 33)).unwrap();

    terminal
        .draw(|frame| {
            drop(app.render_frame(frame));
        })
        .unwrap();

    assert_eq!((app.render_width, app.render_height), (63, 33));
    let buffer = terminal.backend().buffer();
    let visible = (0..33)
        .map(|y| (0..63).map(|x| buffer[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(visible.contains("aaaaa"));
}

#[test]
fn frame_layout_grows_to_show_every_word_wrapped_composer_row() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    app.draft = "123456 123456 123456".into();
    app.cursor = app.draft.len();
    let mut terminal = Terminal::new(TestBackend::new(14, 12)).unwrap();

    terminal
        .draw(|frame| {
            drop(app.render_frame(frame));
        })
        .unwrap();

    let buffer = terminal.backend().buffer();
    let visible = (0..12)
        .map(|y| (0..14).map(|x| buffer[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>();
    assert_eq!(
        visible
            .iter()
            .filter(|line| line.contains("123456"))
            .count(),
        3
    );
}

#[test]
fn transcript_end_offset_is_clamped_and_restores_tail_following() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    for index in 0..40 {
        app.push_user(&format!("transcript row {index}"), &[]);
    }
    app.follow_history_tail = false;
    app.history_scroll = usize::MAX;
    let mut terminal = Terminal::new(TestBackend::new(80, 16)).unwrap();

    terminal
        .draw(|frame| {
            drop(app.render_frame(frame));
        })
        .unwrap();

    assert!(app.history_scroll > 0);
    assert!(app.follow_history_tail);
}

fn transcript_scrollbar_thumb_rows(app: &mut App, width: u16, height: u16) -> Vec<u16> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| {
            drop(app.render_frame(frame));
        })
        .unwrap();
    let buffer = terminal.backend().buffer();
    (0..height)
        .filter(|&row| buffer[(width.saturating_sub(1), row)].symbol() == "█")
        .collect()
}

#[test]
fn transcript_scrollbar_is_visible_only_above_the_tail() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    app.welcome.clear();
    for index in 0..40 {
        app.push_user(&format!("transcript row {index}"), &[]);
    }

    assert!(transcript_scrollbar_thumb_rows(&mut app, 60, 16).is_empty());

    app.follow_history_tail = false;
    app.history_scroll = app.history_scroll.saturating_sub(8);
    let thumb_rows = transcript_scrollbar_thumb_rows(&mut app, 60, 16);
    assert!(!thumb_rows.is_empty());
    let mut terminal = Terminal::new(TestBackend::new(60, 16)).unwrap();
    terminal
        .draw(|frame| drop(app.render_frame(frame)))
        .unwrap();
    assert_eq!(
        terminal.backend().buffer()[(59, thumb_rows[0])].fg,
        Color::Reset
    );
    let track_row = (0..16)
        .find(|row| !thumb_rows.contains(row))
        .expect("scrollbar track row");
    assert_eq!(
        terminal.backend().buffer()[(59, track_row)].fg,
        Color::Reset
    );
    assert!(
        terminal.backend().buffer()[(59, track_row)]
            .modifier
            .contains(Modifier::DIM)
    );

    app.history_scroll = usize::MAX;
    assert!(transcript_scrollbar_thumb_rows(&mut app, 60, 16).is_empty());
    assert!(app.follow_history_tail);
}

#[test]
fn transcript_reflow_clamps_scroll_without_restoring_tail_following() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    app.welcome.clear();
    for index in 0..40 {
        app.push_user(&format!("transcript row {index}"), &[]);
    }
    // Isolate viewport clamping from tail-first prefix materialization, which
    // legitimately increases the numeric offset to preserve the visible row.
    app.ensure_history_layout(80);
    let mut terminal = Terminal::new(TestBackend::new(80, 16)).unwrap();
    terminal
        .draw(|frame| drop(app.render_frame(frame)))
        .unwrap();
    app.follow_history_tail = false;
    let previous_scroll = app.history_scroll;
    terminal.backend_mut().resize(80, 20);

    terminal
        .draw(|frame| drop(app.render_frame(frame)))
        .unwrap();

    assert!(app.history_scroll < previous_scroll);
    assert!(!app.follow_history_tail);
}

#[test]
fn transcript_scrollbar_marks_unloaded_older_messages() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    app.welcome.clear();
    for index in 0..40 {
        app.push_user(&format!("transcript row {index}"), &[]);
    }
    app.follow_history_tail = false;
    app.transcript_older = Some(
        serde_json::from_value(serde_json::json!({
            "conversation_id": cagent_agent::protocol::ConversationId::new(),
            "older_node_id": cagent_agent::protocol::NodeId::new(),
            "newer_node_id": cagent_agent::protocol::NodeId::new(),
        }))
        .unwrap(),
    );

    let mut terminal = Terminal::new(TestBackend::new(60, 16)).unwrap();
    terminal
        .draw(|frame| drop(app.render_frame(frame)))
        .unwrap();
    let indicator = &terminal.backend().buffer()[(59, 0)];
    assert_eq!(indicator.symbol(), "↑");
    assert_eq!(indicator.fg, Color::Reset);
    assert!(indicator.modifier.contains(Modifier::DIM));
}

#[test]
fn transcript_scrollbar_thumb_tracks_loaded_row_offset() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    app.welcome.clear();
    for index in 0..60 {
        app.push_user(&format!("transcript row {index}"), &[]);
    }
    app.follow_history_tail = false;
    app.ensure_history_layout(40);
    let max_scroll = app
        .history_layout
        .rendered
        .as_ref()
        .expect("history layout")
        .rows
        .len()
        .saturating_sub(14);

    app.history_scroll = 0;
    let top = transcript_scrollbar_thumb_rows(&mut app, 40, 14);
    app.history_scroll = max_scroll / 3;
    let middle = transcript_scrollbar_thumb_rows(&mut app, 40, 14);
    app.history_scroll = max_scroll.saturating_mul(2) / 3;
    let near_tail = transcript_scrollbar_thumb_rows(&mut app, 40, 14);

    assert!(!top.is_empty());
    assert!(!middle.is_empty());
    assert!(!near_tail.is_empty());
    assert!(top[0] < middle[0]);
    assert!(middle[0] < near_tail[0]);
}

#[test]
fn transcript_scrollbar_handles_narrow_and_short_viewports() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    app.welcome.clear();
    for index in 0..20 {
        app.push_user(&format!("row {index}"), &[]);
    }
    app.follow_history_tail = false;
    app.history_scroll = 1;

    let _ = transcript_scrollbar_thumb_rows(&mut app, 8, 6);
}

#[test]
fn files_sidebar_uses_shared_tree_and_shifts_the_wide_main_pane() {
    let temporary = tempfile::tempdir().unwrap();
    std::fs::write(temporary.path().join("main.rs"), "fn main() {}\n").unwrap();
    let mut app = App::new(
        temporary.path(),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    app.toggle_files_sidebar();
    assert!(
        app.files_sidebar
            .tree
            .as_ref()
            .unwrap()
            .browser
            .tree
            .rows()
            .iter()
            .any(|row| row.kind == cagent_agent::presentation::DirectoryEntryKind::Loading)
    );
    app.complete_directory_loads_for_test();
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();

    terminal
        .draw(|frame| drop(app.render_frame(frame)))
        .unwrap();

    assert_eq!(app.render_width, 88);
    assert_eq!(app.files_sidebar.area.unwrap().width, 32);
    assert_eq!(terminal.backend().buffer()[(31, 0)].symbol(), "│");
    assert_eq!(terminal.backend().buffer()[(31, 23)].symbol(), "│");
    let visible = (0..24)
        .map(|y| {
            (0..120)
                .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(visible.contains("Files"));
    assert!(visible.contains("main.rs"));
    assert_eq!(terminal.backend().buffer()[(0, 0)].bg, Color::Reset);
    assert_eq!(terminal.backend().buffer()[(0, 23)].bg, Color::Reset);
    assert_eq!(
        terminal.backend().buffer()[(0, 1)].bg,
        COMPOSER_STYLE.bg.unwrap()
    );
    assert!(
        (0..32)
            .map(|x| terminal.backend().buffer()[(x, 2)].symbol())
            .collect::<String>()
            .contains("Files")
    );

    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    assert!(app.handle_files_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 31,
        row: 4,
        modifiers: KeyModifiers::NONE,
    }));
    assert!(app.handle_files_mouse(MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: 44,
        row: 4,
        modifiers: KeyModifiers::NONE,
    }));
    assert!(app.handle_files_mouse(MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: 44,
        row: 4,
        modifiers: KeyModifiers::NONE,
    }));
    terminal
        .draw(|frame| drop(app.render_frame(frame)))
        .unwrap();
    assert_eq!(app.files_sidebar.area.unwrap().width, 45);
    assert_eq!(app.render_width, 75);
    assert!(app.handle_files_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Esc,
        KeyModifiers::NONE,
    )));
    assert!(!app.files_sidebar.visible);
}

#[test]
fn files_sidebar_uses_surface_background_and_project_relative_directory() {
    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir(project.path().join(".git")).unwrap();
    let workspace = project.path().join("src");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(workspace.join("main.rs"), "fn main() {}\n").unwrap();
    let mut app = App::new(
        &workspace,
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    app.toggle_files_sidebar();
    app.complete_directory_loads_for_test();
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();

    terminal
        .draw(|frame| drop(app.render_frame(frame)))
        .unwrap();

    assert_eq!(terminal.backend().buffer()[(0, 0)].bg, Color::Reset);
    assert_eq!(terminal.backend().buffer()[(0, 1)].bg, Color::Indexed(236));
    assert_eq!(terminal.backend().buffer()[(31, 1)].bg, Color::Reset);
    let header = (0..32)
        .map(|x| terminal.backend().buffer()[(x, 2)].symbol())
        .collect::<String>();
    let project_name = project.path().file_name().unwrap().to_string_lossy();
    assert_eq!(workspace_display_path(project.path()), project_name);
    assert!(header.contains(&format!("{project_name}/src")));
}

#[test]
fn narrow_files_sidebar_owns_the_screen_without_a_composer() {
    let temporary = tempfile::tempdir().unwrap();
    std::fs::write(temporary.path().join("README.md"), "hello\n").unwrap();
    let mut app = App::new(
        temporary.path(),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    app.toggle_files_sidebar();
    app.complete_directory_loads_for_test();
    app.onboarding = true;
    let prompt_layout = app.files_pane_layout(ratatui::layout::Rect::new(0, 0, 50, 16));
    assert!(prompt_layout.sidebar.is_none());
    assert_eq!(prompt_layout.main.width, 50);
    app.onboarding = false;
    let mut terminal = Terminal::new(TestBackend::new(50, 16)).unwrap();

    terminal
        .draw(|frame| drop(app.render_frame(frame)))
        .unwrap();

    assert_eq!(app.files_sidebar.area.unwrap().width, 50);
    let visible = (0..16)
        .map(|y| {
            (0..50)
                .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(visible.contains("README.md"));
    assert!(!visible.contains(COMPOSER_PLACEHOLDER));
}

#[test]
fn escape_returns_from_a_sidebar_file_to_the_tree_before_closing_it() {
    let temporary = tempfile::tempdir().unwrap();
    let file = temporary.path().join("README.md");
    std::fs::write(&file, "hello\n").unwrap();
    let mut app = App::new(
        temporary.path(),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    app.toggle_files_sidebar();
    app.complete_directory_loads_for_test();

    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    assert!(app.handle_files_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
    assert!(!app.files_sidebar.focused);
    assert!(app.filesystem_expanded());

    assert!(app.handle_files_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
    assert!(app.files_sidebar.focused);
    assert!(!app.filesystem_expanded());
    let narrow = app.files_pane_layout(ratatui::layout::Rect::new(0, 0, 50, 16));
    assert_eq!(narrow.main.width, 0);
    assert!(narrow.sidebar.is_some());

    assert!(app.handle_files_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
    assert!(!app.files_sidebar.visible);
    assert!(!app.filesystem_expanded());

    app.toggle_files_sidebar();
    app.complete_directory_loads_for_test();
    let mut terminal = Terminal::new(TestBackend::new(50, 16)).unwrap();
    terminal
        .draw(|frame| drop(app.render_frame(frame)))
        .unwrap();
    let row = app.files_sidebar.row_hits[0].0;
    assert!(app.handle_files_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 2,
        row,
        modifiers: KeyModifiers::NONE,
    }));
    assert!(app.files_sidebar.focused);
    assert!(!app.filesystem_expanded());
    assert!(app.handle_files_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 2,
        row,
        modifiers: KeyModifiers::NONE,
    }));
    assert!(!app.files_sidebar.focused);
    assert!(app.filesystem_expanded());
    assert!(app.handle_files_mouse(MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: 2,
        row,
        modifiers: KeyModifiers::NONE,
    }));
    assert!(!app.files_sidebar.focused);
    assert!(app.handle_files_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
    assert!(app.files_sidebar.focused);
    assert!(!app.filesystem_expanded());
}

#[test]
fn narrow_file_view_covers_sidebar_and_replaces_the_previous_path_view() {
    let temporary = tempfile::tempdir().unwrap();
    let first = temporary.path().join("first.txt");
    let second = temporary.path().join("second.txt");
    std::fs::write(&first, "first\n").unwrap();
    std::fs::write(&second, "second\n").unwrap();
    let mut app = App::new(
        temporary.path(),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    app.toggle_files_sidebar();
    app.complete_directory_loads_for_test();
    let _ = app.open_builtin_path(&first, true);
    let _ = app.open_builtin_path(&second, true);
    app.files_sidebar.focused = false;
    assert_eq!(app.surfaces.len(), 1);
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Expanded {
            view: ExpandedView::File { view },
            ..
        }) if view.path == second
    ));

    let mut terminal = Terminal::new(TestBackend::new(50, 16)).unwrap();
    terminal
        .draw(|frame| drop(app.render_frame(frame)))
        .unwrap();
    assert!(app.files_sidebar.area.is_none());
    let visible = (0..16)
        .map(|y| {
            (0..50)
                .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(visible.contains("second.txt"));
    assert!(!visible.contains("Files"));
}

#[test]
fn switching_from_an_image_clears_the_rendered_image_state() {
    let temporary = tempfile::tempdir().unwrap();
    let image_path = temporary.path().join("image.png");
    let text_path = temporary.path().join("text.txt");
    image::DynamicImage::new_rgb8(2, 2)
        .save(&image_path)
        .unwrap();
    std::fs::write(&text_path, "text after image\n").unwrap();

    let mut app = App::new(
        temporary.path(),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    app.open_builtin_path(&image_path, true);
    let mut terminal = Terminal::new(TestBackend::new(60, 20)).unwrap();
    terminal
        .draw(|frame| drop(app.render_frame(frame)))
        .unwrap();

    assert!(
        app.image_preview
            .as_ref()
            .is_some_and(|preview| preview.protocol.is_some())
    );
    assert!(app.rendered_image.is_some());

    app.open_builtin_path(&text_path, true);
    terminal
        .draw(|frame| drop(app.render_frame(frame)))
        .unwrap();

    assert!(app.image_preview.is_none());
    assert!(app.rendered_image.is_some());
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Expanded {
            view: ExpandedView::File { view },
            ..
        }) if matches!(view.content, cagent_agent::presentation::FileViewContent::Text { .. })
    ));
}

#[test]
fn image_render_area_uses_the_protocol_size_instead_of_the_viewport() {
    let mut picker = Picker::halfblocks();
    picker.set_protocol_type(ProtocolType::Kitty);
    let protocol = picker
        .new_protocol(
            image::DynamicImage::new_rgb8(20, 20),
            Size::new(100, 40),
            Resize::Fit(None),
        )
        .unwrap();
    let viewport = Rect::new(30, 4, 100, 40);

    let area = image_render_area(viewport, &protocol);

    assert_eq!(area, Rect::new(30, 4, 2, 1));
}

#[test]
fn image_render_area_clips_a_cached_protocol_to_a_smaller_viewport() {
    let mut picker = Picker::halfblocks();
    picker.set_protocol_type(ProtocolType::Kitty);
    let protocol = picker
        .new_protocol(
            image::DynamicImage::new_rgb8(400, 400),
            Size::new(80, 40),
            Resize::Fit(None),
        )
        .unwrap();

    let area = image_render_area(Rect::new(30, 4, 10, 5), &protocol);

    assert_eq!(area, Rect::new(30, 4, 10, 5));
}

#[test]
fn small_kitty_image_does_not_claim_the_sidebar_or_unused_file_background() {
    let temporary = tempfile::tempdir().unwrap();
    let image_path = temporary.path().join("small.png");
    image::DynamicImage::new_rgb8(20, 20)
        .save(&image_path)
        .unwrap();

    let mut app = App::new(
        temporary.path(),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    app.toggle_files_sidebar();
    app.complete_directory_loads_for_test();
    app.open_builtin_path(&image_path, true);
    app.image_picker.set_protocol_type(ProtocolType::Kitty);

    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal
        .draw(|frame| drop(app.render_frame(frame)))
        .unwrap();

    let protocol_size = app
        .image_preview
        .as_ref()
        .and_then(|preview| preview.protocol.as_ref())
        .map(|(_, protocol)| protocol.size())
        .expect("image protocol should be prepared");
    let rendered = app
        .rendered_image
        .as_ref()
        .expect("image should have an occupied region");
    assert_eq!(rendered.area.as_size(), protocol_size);

    let sidebar = app.files_sidebar.area.expect("sidebar should be visible");
    let sidebar_background = terminal.backend().buffer()[(
        sidebar.right().saturating_sub(2),
        sidebar.bottom().saturating_sub(2),
    )]
        .bg;
    assert_eq!(sidebar_background, COMPOSER_STYLE.bg.unwrap());

    let unused_background =
        terminal.backend().buffer()[(rendered.area.right(), rendered.area.y)].bg;
    assert_eq!(unused_background, COMPOSER_STYLE.bg.unwrap());

    assert!(
        sidebar
            .positions()
            .all(|position| terminal.backend().buffer()[position].diff_option
                == CellDiffOption::AlwaysUpdate)
    );
    assert!(!app.files_sidebar.redraw_after_image_update);
}

#[test]
fn cleanup_redraw_preserves_image_diff_cells_and_forces_surrounding_cells() {
    let temporary = tempfile::tempdir().unwrap();
    let image_path = temporary.path().join("small.png");
    image::DynamicImage::new_rgb8(20, 20)
        .save(&image_path)
        .unwrap();

    let mut app = App::new(
        temporary.path(),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    app.open_builtin_path(&image_path, true);
    app.image_picker.set_protocol_type(ProtocolType::Kitty);
    let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
    terminal
        .draw(|frame| drop(app.render_frame(frame)))
        .unwrap();
    let image_area = app.rendered_image.as_ref().unwrap().area;
    app.image_redraw_area = Some(Rect::new(
        image_area.x,
        image_area.y,
        image_area.width.saturating_add(1),
        image_area.height,
    ));

    let mut frame = terminal.get_frame();
    drop(app.render_frame(&mut frame));
    let image_cell = &frame.buffer_mut()[(image_area.x, image_area.y)];
    assert!(matches!(
        image_cell.diff_option,
        CellDiffOption::ForcedWidth(_)
    ));
    let background_cell = &frame.buffer_mut()[(image_area.right(), image_area.y)];
    assert_eq!(background_cell.diff_option, CellDiffOption::AlwaysUpdate);
}

#[test]
fn image_replacement_commits_background_before_rendering_the_new_protocol() {
    let temporary = tempfile::tempdir().unwrap();
    let image_path = temporary.path().join("image.png");
    image::DynamicImage::new_rgb8(320, 160)
        .save(&image_path)
        .unwrap();

    let mut app = App::new(
        temporary.path(),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    app.open_builtin_path(&image_path, true);
    app.image_picker.set_protocol_type(ProtocolType::Kitty);
    let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
    terminal
        .draw(|frame| drop(app.render_frame(frame)))
        .unwrap();
    let old_area = app.rendered_image.as_ref().unwrap().area;

    app.image_preview.as_mut().unwrap().protocol = None;
    app.rendered_image = None;
    app.image_redraw_area = Some(old_area);
    terminal
        .draw(|frame| drop(app.render_frame_with_image(frame, false)))
        .unwrap();

    assert!(
        app.image_preview
            .as_ref()
            .is_some_and(|preview| preview.protocol.is_none()),
        "the cleanup frame must not prepare or place the replacement image"
    );
    assert!(old_area.positions().all(|position| {
        let cell = &terminal.backend().buffer()[position];
        cell.bg == COMPOSER_STYLE.bg.unwrap() && cell.diff_option == CellDiffOption::AlwaysUpdate
    }));

    terminal
        .draw(|frame| drop(app.render_frame_with_image(frame, true)))
        .unwrap();
    assert!(
        app.image_preview
            .as_ref()
            .is_some_and(|preview| preview.protocol.is_some())
    );
    assert!(app.rendered_image.is_some());
}

#[test]
fn sidebar_resize_keeps_image_and_repaints_sidebar_after_drag() {
    let temporary = tempfile::tempdir().unwrap();
    let image_path = temporary.path().join("image.png");
    image::DynamicImage::new_rgb8(32, 16)
        .save(&image_path)
        .unwrap();

    let mut app = App::new(
        temporary.path(),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    app.toggle_files_sidebar();
    app.complete_directory_loads_for_test();
    app.open_builtin_path(&image_path, true);

    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal
        .draw(|frame| drop(app.render_frame(frame)))
        .unwrap();
    let initial_size = app
        .image_preview
        .as_ref()
        .and_then(|preview| preview.protocol.as_ref())
        .map(|(size, _)| *size)
        .expect("image protocol should be prepared");
    let divider = app.files_sidebar.divider_column.expect("sidebar divider");

    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    assert!(app.handle_files_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: divider,
        row: 4,
        modifiers: KeyModifiers::NONE,
    }));
    assert!(
        app.image_preview
            .as_ref()
            .is_some_and(|preview| preview.protocol.is_some())
    );
    assert!(app.handle_files_mouse(MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: divider.saturating_add(12),
        row: 4,
        modifiers: KeyModifiers::NONE,
    }));
    terminal
        .draw(|frame| drop(app.render_frame(frame)))
        .unwrap();

    let size_during_drag = app
        .image_preview
        .as_ref()
        .and_then(|preview| preview.protocol.as_ref())
        .map(|(size, _)| *size)
        .expect("drag should keep the existing image protocol");
    assert_eq!(size_during_drag, initial_size);

    assert!(app.handle_files_mouse(MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: divider.saturating_add(12),
        row: 4,
        modifiers: KeyModifiers::NONE,
    }));
    assert!(
        app.image_preview
            .as_ref()
            .is_some_and(|preview| preview.protocol.is_none())
    );
    assert!(app.files_sidebar.redraw_after_image_update);
    let mut frame = terminal.get_frame();
    drop(app.render_frame(&mut frame));
    let sidebar = app
        .files_sidebar
        .area
        .expect("sidebar should remain visible");
    assert!(
        sidebar.positions().all(
            |position| frame.buffer_mut()[position].diff_option == CellDiffOption::AlwaysUpdate
        )
    );
    assert!(!app.files_sidebar.redraw_after_image_update);

    let final_size = app
        .image_preview
        .as_ref()
        .and_then(|preview| preview.protocol.as_ref())
        .map(|(size, _)| *size)
        .expect("image protocol should be rebuilt after the drag");
    assert_ne!(final_size, initial_size);
    let final_protocol_size = app
        .image_preview
        .as_ref()
        .and_then(|preview| preview.protocol.as_ref())
        .map(|(_, protocol)| protocol.size())
        .unwrap();
    assert_eq!(
        app.rendered_image.as_ref().unwrap().area.as_size(),
        final_protocol_size
    );
}

#[test]
fn closing_sidebar_invalidates_and_rebuilds_the_image_for_the_full_viewport() {
    let temporary = tempfile::tempdir().unwrap();
    let image_path = temporary.path().join("wide.png");
    image::DynamicImage::new_rgb8(1_000, 100)
        .save(&image_path)
        .unwrap();

    let mut app = App::new(
        temporary.path(),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    app.toggle_files_sidebar();
    app.complete_directory_loads_for_test();
    app.open_builtin_path(&image_path, true);
    app.image_picker.set_protocol_type(ProtocolType::Kitty);

    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal
        .draw(|frame| drop(app.render_frame(frame)))
        .unwrap();
    let sidebar_viewport = app
        .image_preview
        .as_ref()
        .and_then(|preview| preview.protocol.as_ref())
        .map(|(size, _)| *size)
        .expect("image should be prepared with the sidebar visible");
    assert!(app.rendered_image.is_some());

    app.toggle_files_sidebar();

    assert!(!app.files_sidebar.visible);
    assert!(
        app.image_preview
            .as_ref()
            .is_some_and(|preview| preview.protocol.is_none())
    );
    assert!(
        app.rendered_image.is_some(),
        "the render loop still needs the old placement geometry for cleanup"
    );

    terminal
        .draw(|frame| drop(app.render_frame(frame)))
        .unwrap();
    let full_viewport = app
        .image_preview
        .as_ref()
        .and_then(|preview| preview.protocol.as_ref())
        .map(|(size, _)| *size)
        .expect("image should be rebuilt after the sidebar closes");
    assert!(full_viewport.width > sidebar_viewport.width);
    assert_eq!(
        app.rendered_image.as_ref().unwrap().area.as_size(),
        app.image_preview
            .as_ref()
            .and_then(|preview| preview.protocol.as_ref())
            .map(|(_, protocol)| protocol.size())
            .unwrap()
    );
}

#[test]
fn visible_file_and_sidebar_tree_refresh_in_place() {
    let temporary = tempfile::tempdir().unwrap();
    let file = temporary.path().join("live.txt");
    std::fs::write(&file, "before\n").unwrap();
    let mut app = App::new(
        temporary.path(),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    app.toggle_files_sidebar();
    app.complete_directory_loads_for_test();
    let initial_tree = app.files_sidebar.tree.as_ref().unwrap();
    let selected_path = initial_tree.browser.tree.rows()[initial_tree.selected_index()]
        .path
        .clone();
    let added = temporary.path().join("added.rs");
    std::fs::write(&added, "fn added() {}\n").unwrap();
    app.refresh_filesystem_views(std::slice::from_ref(&added));
    app.complete_directory_loads_for_test();
    let sidebar = app.files_sidebar.tree.as_ref().unwrap();
    assert_eq!(
        sidebar.browser.tree.rows()[sidebar.selected_index()].path,
        selected_path
    );
    assert!(
        sidebar
            .browser
            .tree
            .rows()
            .iter()
            .any(|row| row.path == added)
    );

    let _ = app.open_builtin_path(&file, true);
    std::fs::write(&file, "after\n").unwrap();
    app.refresh_filesystem_views(std::slice::from_ref(&file));
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Expanded {
            view:
                ExpandedView::File {
                    view: cagent_agent::presentation::FileView {
                        content: cagent_agent::presentation::FileViewContent::Text { lines, .. },
                        ..
                    }
                },
            ..
        }) if lines.first().is_some_and(|line| line.tokens.iter().any(|token| token.text == "after"))
    ));

    std::fs::remove_file(&file).unwrap();
    app.refresh_filesystem_views(std::slice::from_ref(&file));
    assert!(matches!(
        app.surfaces.last(),
        Some(Surface::Expanded {
            view: ExpandedView::File {
                view: cagent_agent::presentation::FileView {
                    content: cagent_agent::presentation::FileViewContent::Unavailable { .. },
                    ..
                }
            },
            ..
        })
    ));
}

#[test]
fn duplicate_web_fetch_urls_open_the_matching_rendered_card() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    app.history = vec![crate::app::test_tool_groups(vec![
        ToolActivityGroup::WebFetch {
            node_id: Some(cagent_agent::protocol::NodeId::new()),
            url: "https://example.com".into(),
            redirected_url: None,
            format: cagent_agent::WebFetchFormat::Text,
            content_type: Some("text/html".into()),
            status: ToolActivityStatus::Succeeded,
            output: None,
        },
        ToolActivityGroup::WebFetch {
            node_id: Some(cagent_agent::protocol::NodeId::new()),
            url: "https://example.com".into(),
            redirected_url: None,
            format: cagent_agent::WebFetchFormat::Markdown,
            content_type: Some("text/html".into()),
            status: ToolActivityStatus::Succeeded,
            output: Some("# Markdown result\nfirst fetch".into()),
        },
        ToolActivityGroup::WebFetch {
            node_id: Some(cagent_agent::protocol::NodeId::new()),
            url: "https://example.com".into(),
            redirected_url: None,
            format: cagent_agent::WebFetchFormat::Html,
            content_type: Some("text/html".into()),
            status: ToolActivityStatus::Succeeded,
            output: Some("<main>HTML result</main>".into()),
        },
    ])];
    let mut terminal = Terminal::new(TestBackend::new(80, 32)).unwrap();

    terminal
        .draw(|frame| {
            drop(app.render_frame(frame));
        })
        .unwrap();

    let html_hit = app
        .web_fetch_output_hits
        .iter()
        .find(|hit| hit.format == cagent_agent::WebFetchFormat::Html)
        .expect("HTML card should have a distinct click hit");
    assert_eq!(html_hit.output, "<main>HTML result</main>");
    assert!(
        app.web_fetch_output_hits
            .iter()
            .any(|hit| hit.format == cagent_agent::WebFetchFormat::Markdown)
    );
}

#[test]
fn onboarding_surface_has_welcome_setup_and_help_copy() {
    let surface = crate::app::Surface::Onboarding;
    let lines = surface_lines(&surface, Path::new("/tmp/project"), 80);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    assert!(text.contains("Welcome"));
    assert!(text.contains("Welcome to Cagent!"));
    assert_eq!(text.matches("Welcome to Cagent!").count(), 1);
    assert!(text.contains("provider and model"));
    assert!(text.contains("Run /help for more help."));
    let setup_index = lines
        .iter()
        .position(|line| line.to_string().contains("Press Enter"))
        .expect("setup guidance");
    let setup = &lines[setup_index];
    assert_eq!(setup.spans[1].content, "Enter");
    assert_eq!(setup.spans[1].style, ACCENT_STYLE);
    assert_eq!(setup.spans[3].content, "Esc");
    assert_eq!(setup.spans[3].style, DIM_STYLE);
    assert_eq!(lines[setup_index + 1].width(), 0);
    let help = lines
        .iter()
        .find(|line| line.to_string().contains("Run /help"))
        .expect("help guidance");
    assert_eq!(help.spans[1].content, "/help");
    assert_eq!(help.spans[1].style, ACCENT_STYLE);
    assert_eq!(lines.last().expect("onboarding lines").width(), 0);
    assert_eq!(surface_status(&surface), "Enter setup · Esc dismiss");
}

#[test]
fn waiting_indicator_uses_waiting_label() {
    let line = working_line(None, None, false, true, false, false, 0, None).to_string();
    assert!(line.contains("Waiting"));
    assert!(!line.contains("Working"));
}

#[test]
fn waiting_indicator_shows_dim_task_hint() {
    let line = working_line(None, None, false, true, false, false, 1, None);

    assert_eq!(
        line.to_string(),
        "• Waiting (0s • esc to interrupt) • 1 task"
    );
    assert_eq!(line.spans.last().expect("task hint").style, DIM_STYLE);
}

#[test]
fn waiting_indicator_pluralizes_task_hint() {
    let line = working_line(None, None, false, true, false, false, 2, None);

    assert!(line.to_string().ends_with("2 tasks"));
}

#[test]
fn thinking_indicator_uses_thinking_label() {
    let line = working_line(None, None, true, false, false, false, 1, None).to_string();
    assert!(line.contains("Thinking"));
    assert!(!line.contains("Working"));
    assert!(line.ends_with("1 task"));
}

#[test]
fn working_indicator_shows_task_hint() {
    let line = working_line(None, None, false, false, false, false, 2, None).to_string();

    assert!(line.ends_with("2 tasks"));
}

#[test]
fn working_indicator_omits_task_hint_without_background_work() {
    let line = working_line(None, None, false, false, false, false, 0, None).to_string();

    assert_eq!(line, "• Working (0s • esc to interrupt)");
}

#[test]
fn working_indicator_shows_reconnect_countdown_and_attempt() {
    let reconnect = crate::app::ReconnectStatus {
        next_attempt: 2,
        max_attempts: 4,
        reason: "connection_failure".into(),
        deadline: Instant::now() + Duration::from_millis(2_500),
    };

    let line =
        working_line(None, None, false, false, false, false, 0, Some(&reconnect)).to_string();

    assert!(line.contains("Working (0s • esc to interrupt)"));
    assert!(line.contains("Reconnecting in 3s • attempt 2/4 • connection failure"));
}

#[test]
fn working_indicator_omits_expired_reconnect_status() {
    let reconnect = crate::app::ReconnectStatus {
        next_attempt: 3,
        max_attempts: 4,
        reason: "timeout".into(),
        deadline: Instant::now() - Duration::from_millis(1),
    };

    let line =
        working_line(None, None, false, false, false, false, 0, Some(&reconnect)).to_string();

    assert!(!line.contains("Reconnecting"));
}

#[test]
fn working_indicator_warns_after_ninety_seconds_without_activity() {
    let now = Instant::now();
    let line = working_line(
        Some(now - Duration::from_secs(91)),
        Some(now - Duration::from_secs(90)),
        false,
        false,
        false,
        false,
        0,
        None,
    );

    assert_eq!(
        line.to_string(),
        "• Working (1m 31s • esc to interrupt) No activity in past 1m 30s"
    );
    assert!(line.spans[2].style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(
        line.spans.last().expect("warning span").style,
        crate::app::NOTICE_STYLE.add_modifier(Modifier::BOLD)
    );
}

#[test]
fn working_indicator_does_not_warn_before_the_threshold_or_while_waiting() {
    let now = Instant::now();
    let active = working_line(
        Some(now - Duration::from_secs(100)),
        Some(now - Duration::from_secs(60)),
        false,
        false,
        false,
        false,
        0,
        None,
    );
    assert!(!active.to_string().contains("No activity"));

    let waiting = working_line(
        Some(now - Duration::from_secs(100)),
        Some(now - Duration::from_secs(100)),
        false,
        true,
        false,
        false,
        0,
        None,
    );
    assert!(!waiting.to_string().contains("No activity"));
}

#[test]
fn worked_line_fills_the_terminal_width() {
    let line = worked_line("1m 48s", 80);

    assert_eq!(line.width(), 80);
    assert_eq!(
        line.to_string(),
        format!("─ Worked for 1m 48s {}", "─".repeat(60))
    );
    assert_eq!(line.spans[0].style, DIM_STYLE);

    let narrow = worked_line("1m 48s", 10);
    assert_eq!(narrow.width(), 10);
}

#[test]
fn compacted_line_fills_the_terminal_width_responsively() {
    let line = compacted_line(40);
    assert_eq!(line.width(), 40);
    assert_eq!(line.to_string(), format!("─ Compacted {}", "─".repeat(28)));
    assert_eq!(line.spans[0].style, DIM_STYLE);

    let narrow = compacted_line(7);
    assert_eq!(narrow.width(), 7);
    assert_eq!(narrow.to_string(), "─ Compa");
}

#[test]
fn compacted_dividers_carry_their_summary_activation_target() {
    let standalone = crate::app::local_transcript_block(
        cagent_agent::protocol::TranscriptBlockKind::Compacted {
            summary: "Standalone summary".into(),
        },
    );
    let standalone_rows =
        transcript::history_item_rows_with_editor(&standalone, Path::new("/workspace"), 40, false);
    assert_eq!(
        standalone_rows[0]
            .compaction()
            .map(|target| target.summary.as_ref()),
        Some("Standalone summary")
    );

    let accepted = crate::app::local_transcript_block(
        cagent_agent::protocol::TranscriptBlockKind::AcceptedPlan {
            source: "# Plan".into(),
            document: cagent_agent::presentation::parse_markdown("# Plan"),
            clear_context: false,
            compact_context: true,
            compaction_summary: Some("Accepted plan summary".into()),
        },
    );
    let accepted_rows =
        transcript::history_item_rows_with_editor(&accepted, Path::new("/workspace"), 40, false);
    assert_eq!(
        accepted_rows[2]
            .compaction()
            .map(|target| target.summary.as_ref()),
        Some("Accepted plan summary")
    );
}

#[test]
fn status_line_hit_testing_matches_rendered_module_bounds() {
    let config = cagent_agent::presentation::StatusLineConfig {
        modules: vec![
            cagent_agent::presentation::StatusLineModule::Mode,
            cagent_agent::presentation::StatusLineModule::Model,
            cagent_agent::presentation::StatusLineModule::Provider,
        ],
        ..Default::default()
    };
    let values = cagent_agent::presentation::StatusLineValues {
        mode: "ask".into(),
        model: Some("echo".into()),
        provider: Some("mock".into()),
        ..Default::default()
    };

    assert_eq!(
        status_line_module_at(&config, &values, 80, 2),
        Some(StatusLineModule::Mode)
    );
    assert_eq!(
        status_line_module_at(&config, &values, 80, 8),
        Some(StatusLineModule::Model)
    );
    assert_eq!(
        status_line_module_at(&config, &values, 80, 15),
        Some(StatusLineModule::Provider)
    );
    assert_eq!(status_line_module_at(&config, &values, 80, 5), None);
    assert_eq!(status_line_module_at(&config, &values, 80, 1), None);
}

#[test]
fn status_line_renders_effort_without_bold_weight() {
    let config = cagent_agent::presentation::StatusLineConfig {
        modules: vec![cagent_agent::presentation::StatusLineModule::Model],
        ..Default::default()
    };
    let values = cagent_agent::presentation::StatusLineValues {
        model: Some("echo".into()),
        effort: Some("high".into()),
        ..Default::default()
    };

    let line = status_line_content(&config, &values, 80);

    assert_eq!(line.to_string(), "echo high");
    assert_eq!(line.spans[0].content, "echo");
    assert!(line.spans[0].style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(line.spans[1].content, " high");
    assert!(!line.spans[1].style.add_modifier.contains(Modifier::BOLD));
}

#[test]
fn status_line_renders_tokens_and_cache_as_separate_modules() {
    let config = cagent_agent::presentation::StatusLineConfig {
        modules: vec![
            cagent_agent::presentation::StatusLineModule::Tokens,
            cagent_agent::presentation::StatusLineModule::CacheTokens,
            cagent_agent::presentation::StatusLineModule::Cache,
        ],
        ..Default::default()
    };
    let mut values = cagent_agent::presentation::StatusLineValues::default();
    values.usage.add(&cagent_agent::provider::ModelUsage {
        input_tokens: Some(40_000),
        non_cached_input_tokens: Some(10_000),
        cache_read_input_tokens: Some(30_000),
        cache_write_input_tokens: Some(5_000),
        output_tokens: Some(8_000),
        ..Default::default()
    });

    let line = status_line_content(&config, &values, 80);

    assert_eq!(
        line.to_string(),
        "in:10k out:8k · cache r:30k/w:5k · 85% cached"
    );
    assert_eq!(line.spans[0].style.fg, Some(Color::Gray));
    assert_eq!(line.spans[1].content, " · ");
    assert_eq!(line.spans[2].content, "cache r:30k/w:5k");
    assert_eq!(line.spans[2].style.fg, Some(Color::Gray));
    assert_eq!(line.spans[4].content, "85% cached");
    assert_eq!(line.spans[4].style.fg, Some(Color::Blue));
    assert!(line.spans[0].style.add_modifier.contains(Modifier::BOLD));
    assert!(!line.spans[1].style.add_modifier.contains(Modifier::BOLD));
    assert!(line.spans[2].style.add_modifier.contains(Modifier::BOLD));
}

#[test]
fn status_line_background_hint_hit_testing_matches_rendered_text() {
    let config = cagent_agent::presentation::StatusLineConfig {
        modules: vec![cagent_agent::presentation::StatusLineModule::Hint],
        ..Default::default()
    };
    let values = cagent_agent::presentation::StatusLineValues {
        background: 1,
        ..Default::default()
    };

    assert!(status_line_background_hint_at(&config, &values, 80, 5));
    assert!(!status_line_background_hint_at(&config, &values, 80, 1));
    assert!(!status_line_background_hint_at(&config, &values, 80, 20));

    let values = cagent_agent::presentation::StatusLineValues {
        background: 1,
        hints: vec!["Alt+↑/↓ queued".into()],
        ..Default::default()
    };
    assert!(!status_line_background_hint_at(&config, &values, 80, 5));
    assert!(status_line_background_hint_at(&config, &values, 80, 20));
}

#[test]
fn pending_interaction_uses_waiting_for_user_label() {
    let line = working_line(None, None, false, true, true, false, 1, None).to_string();
    assert!(line.contains("Waiting for user"));
    assert!(!line.contains("Working"));
    assert!(line.ends_with("1 task"));
}

#[test]
fn question_transcript_emphasizes_prompts_answers_and_notes() {
    let lines = question_transcript_lines(&cagent_agent::presentation::QuestionTranscript {
        answered: 3,
        total: 3,
        cancelled: false,
        entries: vec![
            cagent_agent::presentation::QuestionTranscriptEntry {
                header: "Goal".into(),
                question: "Which part of the project should we focus on?".into(),
                answer: Some("Ratatui UI (Recommended)".into()),
                note: None,
            },
            cagent_agent::presentation::QuestionTranscriptEntry {
                header: "Work".into(),
                question: "What kind of work do you have in mind?".into(),
                answer: Some("New feature (Recommended)".into()),
                note: Some("Keep the current interactions.\nPreserve the shortcuts.".into()),
            },
        ],
    });

    assert_eq!(lines[0].to_string(), "• Questions 3/3 answered");
    assert_eq!(lines[0].spans[0].style, Style::default());
    assert!(
        lines[0].spans[1]
            .style
            .add_modifier
            .contains(Modifier::BOLD)
    );
    assert_eq!(lines[0].spans[2].style, Style::default());
    assert_eq!(
        lines[0].spans[3].style,
        DIM_STYLE.add_modifier(Modifier::BOLD)
    );
    assert_eq!(
        lines[1].to_string(),
        "  • Which part of the project should we focus on?"
    );
    assert_eq!(lines[1].spans[1].style, Style::default());
    assert_eq!(lines[2].spans[0].style, DIM_STYLE);
    assert_eq!(lines[2].spans[1].style, CHIP_STYLE);
    assert_eq!(lines[5].spans[0].style, DIM_STYLE);
    assert_eq!(lines[5].spans[1].style, Style::default());
    assert_eq!(lines[6].to_string(), "          Preserve the shortcuts.");
    assert_eq!(lines[6].spans[0].style, DIM_STYLE);
    assert_eq!(lines[6].spans[1].style, Style::default());
    assert!(lines.iter().all(|line| !line.to_string().contains("Goal")));
}

#[test]
fn cancelled_question_renders_as_a_system_message() {
    let lines = render_tool_groups(
        &[ToolActivityGroup::Question {
            transcript: cagent_agent::presentation::QuestionTranscript {
                answered: 0,
                total: 1,
                cancelled: true,
                entries: Vec::new(),
            },
        }],
        80,
    );

    assert_eq!(lines[0].to_string(), "• Question not answered");
}

#[test]
fn transcript_scroll_indicator_is_dimmed_for_queue_or_scrolling() {
    let empty_at_tail = scroll_indicator_line(false, 12, false, false);
    assert!(empty_at_tail.spans.is_empty());

    let queued_at_tail = scroll_indicator_line(false, 12, true, false);
    assert_eq!(queued_at_tail.to_string(), "────────────");
    assert!(
        queued_at_tail
            .spans
            .iter()
            .all(|span| span.style == DIM_STYLE)
    );

    let queued_scrolled = scroll_indicator_line(true, 12, true, false);
    assert_eq!(queued_scrolled.to_string(), "─ ↓ ────────");
    assert!(
        queued_scrolled
            .spans
            .iter()
            .all(|span| span.style == DIM_STYLE)
    );

    let empty_scrolled = scroll_indicator_line(true, 12, false, false);
    assert_eq!(empty_scrolled.to_string(), "─ ↓ ────────");

    let expanded_view = scroll_indicator_line(true, 12, true, true);
    assert!(expanded_view.spans.is_empty());
}

#[test]
fn log_rendering_removes_trailing_blank_rows() {
    let rows = vec![
        crate::markdown::DisplayRow::plain(Line::from("message")),
        crate::markdown::DisplayRow::plain(Line::default()),
        crate::markdown::DisplayRow::plain(Line::default()),
    ];

    let trimmed = trim_trailing_log_blank_rows(&rows);
    assert_eq!(trimmed.len(), 1);
    assert_eq!(trimmed[0].to_string(), "message");
}

#[test]
fn log_rendering_keeps_trailing_user_message_padding() {
    let rows = vec![
        crate::markdown::DisplayRow::plain(Line::from("user message").style(USER_STYLE)),
        crate::markdown::DisplayRow::plain(Line::default().style(USER_STYLE)),
        crate::markdown::DisplayRow::plain(Line::default().style(USER_STYLE)),
        crate::markdown::DisplayRow::plain(Line::default()),
    ];

    let trimmed = trim_trailing_log_blank_rows(&rows);
    assert_eq!(trimmed.len(), 3);
    assert!(trimmed.iter().all(|row| row.line.style == USER_STYLE));
}

#[test]
fn assistant_message_marker_uses_default_color() {
    let document = cagent_agent::presentation::parse_markdown("reply");
    let rows = transcript::assistant_markdown_rows(&document, 80);

    assert_eq!(rows[0].line.spans[0].style, Style::default());
}

#[test]
fn transcript_sections_have_one_blank_row_between_completed_and_live_content() {
    let history = vec![crate::markdown::DisplayRow::plain(Line::from(
        "• Explored files",
    ))];
    let streaming = vec![
        crate::markdown::DisplayRow::plain(Line::from("• Assistant reply")),
        crate::markdown::DisplayRow::plain(Line::default()),
    ];
    let working = vec![
        crate::markdown::DisplayRow::plain(Line::from("• Working")),
        crate::markdown::DisplayRow::plain(Line::default()),
    ];

    let rows = join_transcript_sections(&[&history, &streaming, &working], 80, false);
    let text = rows.iter().map(ToString::to_string).collect::<Vec<_>>();
    assert_eq!(
        text,
        vec!["• Explored files", "", "• Assistant reply", "", "• Working",]
    );
}

#[test]
fn active_plan_renders_semantic_statuses_and_wraps() {
    let plan = cagent_agent::protocol::UpdatePlanArgs {
        explanation: Some("Proceeding in order".into()),
        plan: vec![
            cagent_agent::protocol::PlanItemArg {
                step: "Inspect the existing implementation".into(),
                status: cagent_agent::protocol::PlanStepStatus::Completed,
            },
            cagent_agent::protocol::PlanItemArg {
                step: "Implement the change".into(),
                status: cagent_agent::protocol::PlanStepStatus::InProgress,
            },
            cagent_agent::protocol::PlanItemArg {
                step: "Test".into(),
                status: cagent_agent::protocol::PlanStepStatus::Pending,
            },
        ],
    };
    let lines = active_plan_lines(&plan, 28);
    let text = lines.iter().map(ToString::to_string).collect::<Vec<_>>();
    assert_eq!(text[0], "• Plan");
    assert!(text.iter().any(|line| line.contains("✔ Inspect")));
    assert!(text.iter().any(|line| line.contains("□ Implement")));
    assert!(text.iter().all(|line| line.width() <= 28));
    let completed = lines
        .iter()
        .find(|line| line.to_string().contains("✔ Inspect"))
        .unwrap();
    assert!(
        completed
            .spans
            .last()
            .unwrap()
            .style
            .add_modifier
            .contains(Modifier::CROSSED_OUT)
    );
    let in_progress = lines
        .iter()
        .find(|line| line.to_string().contains("□ Implement"))
        .unwrap();
    assert_eq!(
        in_progress.spans.last().unwrap().style.fg,
        Some(Color::Cyan)
    );
}

#[test]
fn empty_active_plan_has_an_explicit_placeholder() {
    let lines = active_plan_lines(
        &cagent_agent::protocol::UpdatePlanArgs {
            explanation: None,
            plan: Vec::new(),
        },
        80,
    );
    assert_eq!(lines[1].to_string(), "  └ (no steps provided)");
}

#[test]
fn active_plan_is_rendered_below_working_at_the_transcript_tail() {
    let mut app = App::new(
        Path::new("/workspace"),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    app.active = true;
    app.working_started_at = Some(Instant::now());
    app.active_plan = Some(cagent_agent::protocol::UpdatePlanArgs {
        explanation: None,
        plan: vec![cagent_agent::protocol::PlanItemArg {
            step: "Implement".into(),
            status: cagent_agent::protocol::PlanStepStatus::InProgress,
        }],
    });
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| drop(app.render_frame(frame)))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let rows = (0..24)
        .map(|y| (0..80).map(|x| buffer[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>();
    let working = rows.iter().position(|row| row.contains("Working")).unwrap();
    let plan = rows.iter().position(|row| row.contains("• Plan")).unwrap();
    assert!(plan > working);
    assert!(rows.iter().any(|row| row.contains("□ Implement")));
}

#[test]
fn activity_before_assistant_uses_a_full_width_dim_separator() {
    let history = vec![
        crate::app::test_tool_groups(vec![ToolActivityGroup::Exploration {
            active: false,
            activities: vec![ExplorationActivity {
                kind: ExplorationActivityKind::Read,
                targets: vec!["crates/cagent-agent/src/runtime/mod.rs".into()],
                scopes: Vec::new(),
            }],
        }]),
        crate::app::local_transcript_block(
            cagent_agent::protocol::TranscriptBlockKind::Assistant {
                document: cagent_agent::presentation::parse_markdown("Yes."),
                source: "Yes.".into(),
                message: None,
            },
        ),
    ];

    let rows = transcript::history_rows(
        &[],
        None,
        &history,
        Path::new("/workspace"),
        test_session_id(),
        24,
        false,
    );
    let separator_index = rows
        .iter()
        .position(|row| row.to_string() == "─".repeat(24))
        .expect("activity/assistant separator");

    assert!(rows[separator_index - 1].to_string().is_empty());
    assert_eq!(rows[separator_index].line.style, DIM_STYLE);
    assert!(rows[separator_index + 1].to_string().is_empty());
    assert_eq!(rows[separator_index + 2].to_string(), "• Yes.");
}

#[test]
fn streaming_assistant_after_activity_gets_a_spaced_dim_separator() {
    let history = vec![crate::markdown::DisplayRow::plain(Line::from("• Explored"))];
    let streaming = vec![
        crate::markdown::DisplayRow::plain(Line::from("• Yes.")),
        crate::markdown::DisplayRow::plain(Line::default()),
    ];

    let rows = join_transcript_sections(&[&history, &streaming], 12, true);
    let text = rows.iter().map(ToString::to_string).collect::<Vec<_>>();

    assert_eq!(text, vec!["• Explored", "", "────────────", "", "• Yes."]);
}

#[test]
fn virtual_transcript_sections_preserve_boundary_windows() {
    let history = vec![crate::markdown::DisplayRow::plain(Line::from("history"))];
    let streaming = vec![
        crate::markdown::DisplayRow::plain(Line::from("streaming")),
        crate::markdown::DisplayRow::plain(Line::default()),
    ];
    let sections = [&history[..], &streaming[..]];
    let expected = ["history", "", "────────", "", "streaming"];

    assert_eq!(transcript_sections_len(&sections, true), expected.len());
    for offset in 0..=expected.len() {
        let mut visible = Vec::new();
        append_visible_transcript_sections(&sections, 8, true, offset, 2, &mut visible);
        assert_eq!(
            visible.iter().map(ToString::to_string).collect::<Vec<_>>(),
            expected
                .iter()
                .skip(offset)
                .take(2)
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            "offset {offset}",
        );
    }
}

#[test]
fn virtual_transcript_sections_clone_only_the_requested_viewport() {
    let history = (0..10_000)
        .map(|index| crate::markdown::DisplayRow::plain(Line::from(format!("row-{index}"))))
        .collect::<Vec<_>>();
    let sections = [&history[..]];
    let mut visible = Vec::new();

    assert_eq!(transcript_sections_len(&sections, false), 10_000);
    append_visible_transcript_sections(&sections, 80, false, 9_000, 20, &mut visible);

    assert_eq!(visible.len(), 20);
    assert_eq!(visible.first().unwrap().to_string(), "row-9000");
    assert_eq!(visible.last().unwrap().to_string(), "row-9019");
}

#[test]
fn delegated_activity_nests_the_task_under_the_running_subagent() {
    let lines = render_tool_groups(
        &[ToolActivityGroup::Delegate {
            action: "Running".into(),
            profile: "Explore".into(),
            task: Some("Summarize PHASES.md".into()),
            status: ToolActivityStatus::Pending,
        }],
        80,
    );
    assert_eq!(lines[0].to_string(), "• Running Explore sub-agent");
    assert_eq!(lines[1].to_string(), "  └ Summarize PHASES.md");
}

#[test]
fn terminal_write_and_kill_show_the_command_and_written_data() {
    let lines = render_tool_groups(
        &[
            ToolActivityGroup::TerminalWrite {
                terminal_id: None,
                command: "cat".into(),
                data: "hello".into(),
                status: ToolActivityStatus::Succeeded,
            },
            ToolActivityGroup::TerminalKill {
                command: "cat".into(),
                status: ToolActivityStatus::Succeeded,
            },
        ],
        80,
    );
    assert_eq!(lines[0].to_string(), "• Terminal Write: cat");
    assert_eq!(lines[1].to_string(), "  └  hello");
    assert_eq!(lines[2].to_string(), "• Terminal Kill: cat");
    let bash_style = bash_command_spans("cat")[0].style;
    assert_eq!(lines[0].spans[2].style, bash_style);
    assert_eq!(lines[2].spans[2].style, bash_style);
}

#[test]
fn terminal_write_data_wraps_under_its_tree_elbow() {
    let lines = render_tool_groups(
        &[ToolActivityGroup::TerminalWrite {
            terminal_id: None,
            command: "cat".into(),
            data: "a long line that needs wrapping in the terminal transcript".into(),
            status: ToolActivityStatus::Succeeded,
        }],
        24,
    );
    assert!(lines.iter().skip(1).all(|line| line.width() <= 24));
    assert_eq!(
        lines[1].to_string().chars().take(5).collect::<String>(),
        "  └  "
    );
}

#[test]
fn terminal_write_data_registers_an_expanded_bash_output_hit() {
    let terminal_id = cagent_agent::tools::TerminalId::new();
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    app.history = vec![crate::app::test_tool_groups(vec![
        ToolActivityGroup::Bash {
            node_id: None,
            terminal_id: Some(terminal_id),
            command: "cat".into(),
            status: ToolActivityStatus::Pending,
            output: Some("hello\n".into()),
            ansi_output: Some("hello\n".into()),
            exit_code: None,
        },
        ToolActivityGroup::TerminalWrite {
            terminal_id: Some(terminal_id),
            command: "cat".into(),
            data: "hello\n".into(),
            status: ToolActivityStatus::Succeeded,
        },
    ])];
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| {
            drop(app.render_frame(frame));
        })
        .unwrap();

    let write_hit = app
        .bash_output_hits
        .last()
        .expect("terminal write data should be clickable");
    assert_eq!(write_hit.terminal_id, Some(terminal_id));
    assert_eq!(write_hit.command, "cat");
    assert_eq!(write_hit.output, "hello\n");
}

#[test]
fn running_delegation_uses_the_active_label_and_pulsing_bullet() {
    let lines = render_tool_groups(
        &[ToolActivityGroup::Delegate {
            action: "Running".into(),
            profile: "Explore".into(),
            task: Some("Summarize PHASES.md".into()),
            status: ToolActivityStatus::Pending,
        }],
        80,
    );
    assert_eq!(lines[0].to_string(), "• Running Explore sub-agent");
    let Some(Color::Rgb(red, green, blue)) = lines[0].spans[0].style.fg else {
        panic!("running delegation dot should use a grayscale RGB pulse");
    };
    assert_eq!(red, green);
    assert_eq!(green, blue);
    assert!((110..=255).contains(&red));
}

#[test]
fn web_activity_uses_pulsing_and_completed_bullets() {
    let pending = render_tool_groups(
        &[
            ToolActivityGroup::WebSearch {
                provider: "exa".into(),
                query: "rust tui".into(),
                status: ToolActivityStatus::Pending,
                result_count: None,
                results: Vec::new(),
            },
            ToolActivityGroup::WebFetch {
                node_id: None,
                url: "https://example.com".into(),
                redirected_url: None,
                format: cagent_agent::WebFetchFormat::Markdown,
                content_type: None,
                status: ToolActivityStatus::Pending,
                output: None,
            },
        ],
        80,
    );
    for line in [&pending[0], &pending[2]] {
        let Some(Color::Rgb(red, green, blue)) = line.spans[0].style.fg else {
            panic!("pending web activity dot should use a grayscale RGB pulse");
        };
        assert_eq!(red, green);
        assert_eq!(green, blue);
        assert!((110..=255).contains(&red));
    }

    let completed = render_tool_groups(
        &[
            ToolActivityGroup::WebSearch {
                provider: "exa".into(),
                query: "rust tui".into(),
                status: ToolActivityStatus::Succeeded,
                result_count: Some(0),
                results: Vec::new(),
            },
            ToolActivityGroup::WebFetch {
                node_id: None,
                url: "https://example.com".into(),
                redirected_url: None,
                format: cagent_agent::WebFetchFormat::Markdown,
                content_type: None,
                status: ToolActivityStatus::Succeeded,
                output: None,
            },
        ],
        80,
    );
    assert_eq!(completed[0].spans[0].style.fg, Some(Color::Green));
    assert_eq!(completed[2].spans[0].style.fg, Some(Color::Green));
}

#[test]
fn terminal_delegation_uses_the_finished_label() {
    let lines = render_tool_groups(
        &[ToolActivityGroup::Delegate {
            action: "Finished".into(),
            profile: "Explore".into(),
            task: None,
            status: ToolActivityStatus::Succeeded,
        }],
        80,
    );
    assert_eq!(lines[0].to_string(), "• Finished Explore sub-agent");
    assert_eq!(lines[0].spans[0].style, DIM_STYLE);
}

#[test]
fn delegated_task_wraps_beyond_its_tree_elbow() {
    let lines = render_tool_groups(
        &[ToolActivityGroup::Delegate {
            action: "Running".into(),
            profile: "Explore".into(),
            task: Some("Read the detailed PHASES.md migration plan".into()),
            status: ToolActivityStatus::Pending,
        }],
        30,
    );
    let wrapped = wrap_log_lines(&lines, 30)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>();

    assert!(wrapped.iter().any(|line| line.starts_with("  └ Read")));
    assert!(wrapped.iter().any(|line| line.starts_with("    PHASES.md")));
    assert!(
        wrapped.iter().any(|line| line.starts_with("    ")),
        "{wrapped:?}"
    );
}

#[test]
fn mcp_activity_shows_identity_duration_and_dim_parameters_on_one_line() {
    let lines = render_tool_groups(
        &[ToolActivityGroup::Mcp {
            call: cagent_agent::presentation::McpCall {
                node_id: None,
                server: "weather\u{1b}[31m".into(),
                tool: "forecast".into(),
                status: ToolActivityStatus::Succeeded,
                duration_millis: Some(42),
                parameters: serde_json::json!({"city": "Toronto"}),
                output: Some(serde_json::json!({"content": "hidden output"})),
            },
        }],
        80,
    );
    assert_eq!(
        lines[0].to_string(),
        "• MCP weather\\u{1b}[31m · forecast · 42 ms · {\"city\":\"Toronto\"}"
    );
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].spans[0].style.fg, Some(Color::Green));
    assert_eq!(lines[0].spans.last().unwrap().style, DIM_STYLE);
    assert!(
        lines
            .iter()
            .all(|line| !line.to_string().contains('\u{1b}'))
    );
}

#[test]
fn welcome_card_is_the_first_scrollable_history_block() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.push_system_message("model changed");
    app.ensure_history_layout(80);

    let rows = &app
        .history_layout
        .rendered
        .as_ref()
        .expect("history layout is initialized")
        .rows;
    assert!(rows[0].to_string().starts_with('╭'));
    assert!(rows.iter().any(|row| row.to_string().contains("Cagent")));

    let intro_rows = welcome_card_rows(&app.welcome, &app.workspace, app.session_id, 80).len();
    let mut visible = Vec::new();
    let mut offset = intro_rows.saturating_add(1);
    let mut remaining = 1;
    transcript::append_visible_rows(rows, &mut offset, &mut remaining, &mut visible);
    assert_eq!(visible[0].to_string(), "• model changed");
}

#[test]
fn welcome_stays_scrolled_past_when_composer_shrinks() {
    let mut app = App::new(
        Path::new("/workspace"),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    app.history.push(crate::app::test_lines(
        (0..20)
            .map(|row| {
                Line::from(if row == 19 {
                    "[history 19](https://example.com)".to_owned()
                } else {
                    format!("history {row}")
                })
            })
            .collect(),
    ));
    app.composer_max_rows = None;
    app.draft = vec!["draft"; 24].join("\n");
    app.cursor = app.draft.len();
    let mut terminal = Terminal::new(TestBackend::new(80, 45)).unwrap();
    terminal
        .draw(|frame| drop(app.render_frame(frame)))
        .unwrap();
    assert!(app.welcome_scrolled_past);

    app.draft.clear();
    app.cursor = 0;
    for _ in 0..2 {
        terminal
            .draw(|frame| drop(app.render_frame(frame)))
            .unwrap();
        assert!(app.follow_history_tail);
        assert!(app.history_scroll > 0);
        assert!(app.transcript_scrollbar.is_none());
        assert_welcome_hidden_and_tail_bottom_aligned(&app, &terminal, "history 19");
        let last_row = 45 - app.controls_height(80, 45) - 1;
        assert!((0..80).any(|x| app.link_at(last_row, x) == Some("https://example.com")));
    }

    app.ensure_history_layout(80);
    app.follow_history_tail = false;
    app.history_scroll = 0;
    terminal
        .draw(|frame| drop(app.render_frame(frame)))
        .unwrap();
    assert_eq!(app.history_scroll, 0);
    let scrollbar = app.transcript_scrollbar.expect("intro remains scrollable");
    assert!(scrollbar.area.y > 0);
    assert_eq!(
        terminal.backend().buffer()[(0, scrollbar.area.y)].symbol(),
        "╭"
    );

    app.history_scroll = usize::MAX;
    terminal
        .draw(|frame| drop(app.render_frame(frame)))
        .unwrap();
    assert_welcome_hidden_and_tail_bottom_aligned(&app, &terminal, "history 19");
}

fn assert_welcome_hidden_and_tail_bottom_aligned(
    app: &App,
    terminal: &Terminal<TestBackend>,
    tail: &str,
) {
    let buffer = terminal.backend().buffer();
    let lines = (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    let bottom = buffer.area.height - app.controls_height(buffer.area.width, buffer.area.height);
    assert!(
        !lines[..usize::from(bottom)]
            .iter()
            .any(|line| line.contains("Cagent") || line.starts_with('╭')),
        "{lines:#?}"
    );
    assert!(lines[usize::from(bottom - 1)].contains(tail), "{lines:#?}");
}

#[test]
fn welcome_stays_scrolled_past_after_resize_and_live_rows_disappear() {
    let mut app = App::new(
        Path::new("/workspace"),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    app.push_system_message("retained content");
    app.streaming_source = (0..30).map(|row| format!("live {row}\n\n")).collect();
    app.streaming = cagent_agent::presentation::parse_streaming_markdown(&app.streaming_source);
    let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
    terminal
        .draw(|frame| drop(app.render_frame(frame)))
        .unwrap();
    assert!(app.welcome_scrolled_past);

    terminal.backend_mut().resize(100, 100);
    terminal
        .draw(|frame| drop(app.render_frame(frame)))
        .unwrap();
    assert_welcome_hidden_and_tail_bottom_aligned(&app, &terminal, "live 29");

    app.streaming.clear();
    app.streaming_source.clear();
    app.streaming_layout = None;
    terminal
        .draw(|frame| drop(app.render_frame(frame)))
        .unwrap();
    assert_welcome_hidden_and_tail_bottom_aligned(&app, &terminal, "retained content");
    assert!(app.follow_history_tail);
}

#[test]
fn welcome_initial_layout_and_zero_height_panel_do_not_count_as_scrolling() {
    let mut app = App::new(
        Path::new("/workspace"),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| drop(app.render_frame(frame)))
        .unwrap();
    assert!(!app.welcome_scrolled_past);
    assert_eq!(terminal.backend().buffer()[(0, 0)].symbol(), "╭");

    terminal.backend_mut().resize(80, 1);
    terminal
        .draw(|frame| drop(app.render_frame(frame)))
        .unwrap();
    assert!(!app.welcome_scrolled_past);
    terminal.backend_mut().resize(80, 24);
    terminal
        .draw(|frame| drop(app.render_frame(frame)))
        .unwrap();
    assert_eq!(terminal.backend().buffer()[(0, 0)].symbol(), "╭");
}

#[test]
fn welcome_stays_scrolled_past_when_a_lazy_prefix_is_materialized() {
    let mut app = App::new(
        Path::new("/workspace"),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    for index in 0..20 {
        app.push_user(&format!("message {index}"), &[]);
    }
    let mut terminal = Terminal::new(TestBackend::new(80, 16)).unwrap();
    terminal
        .draw(|frame| drop(app.render_frame(frame)))
        .unwrap();
    assert!(app.history_layout_incomplete());
    assert!(app.welcome_scrolled_past);

    terminal.backend_mut().resize(80, 200);
    terminal
        .draw(|frame| drop(app.render_frame(frame)))
        .unwrap();
    assert!(!app.history_layout_incomplete());
    // User messages have one intentional styled blank row after the text.
    assert_welcome_hidden_and_tail_bottom_aligned(&app, &terminal, "");
    let bottom = 200 - app.controls_height(80, 200);
    let row = (0..80)
        .map(|x| terminal.backend().buffer()[(x, bottom - 2)].symbol())
        .collect::<String>();
    assert!(row.contains("message 19"));
}

#[test]
fn mcp_rows_truncate_and_use_pulsing_success_and_error_dots() {
    for status in [
        ToolActivityStatus::Pending,
        ToolActivityStatus::Succeeded,
        ToolActivityStatus::Failed,
    ] {
        let call = cagent_agent::presentation::McpCall {
            node_id: None,
            server: "github".into(),
            tool: "get_latest_release".into(),
            status,
            duration_millis: Some(813),
            parameters: serde_json::json!({"repo": "界🌍".repeat(100), "owner": "openai"}),
            output: None,
        };
        let group = ToolActivityGroup::Mcp { call };
        for width in [1, 12, 60, 100] {
            let rows = crate::render::transcript::tool_group_rows(
                std::slice::from_ref(&group),
                Path::new("/workspace"),
                width,
            );
            assert_eq!(rows.len(), 1);
            assert!(rows[0].line.width() <= usize::from(width));
            assert!(rows[0].mcp_call().is_some());
        }
        let lines = render_tool_groups(&[group], 100);
        match status {
            ToolActivityStatus::Pending => {
                let Some(Color::Rgb(r, g, b)) = lines[0].spans[0].style.fg else {
                    panic!("expected pulse")
                };
                assert_eq!((r, r), (g, b));
                assert!((110..=255).contains(&r));
            }
            ToolActivityStatus::Succeeded => {
                assert_eq!(lines[0].spans[0].style.fg, Some(Color::Green))
            }
            ToolActivityStatus::Failed => assert_eq!(lines[0].spans[0].style, ERROR_STYLE),
        }
    }
}

#[test]
fn clicking_duplicate_mcp_calls_opens_the_exact_call_as_highlighted_json() {
    let mut app = App::new(
        Path::new("/workspace"),
        ("mock".into(), "echo".into(), None),
        Default::default(),
        "ask",
        None,
    );
    app.history = vec![crate::app::test_tool_groups((1..=2).map(|n| ToolActivityGroup::Mcp {
        call: cagent_agent::presentation::McpCall {
            node_id: Some(cagent_agent::protocol::NodeId::new()),
            server: "github".into(),
            tool: "get_latest_release".into(),
            status: ToolActivityStatus::Succeeded,
            duration_millis: Some(813),
            parameters: serde_json::json!({"repo": "codex"}),
            output: Some(serde_json::json!({"content": [{"type": "text", "text": format!("{{\"release\":{n}}}") }]})),
        },
    }).collect())];
    let mut terminal = Terminal::new(TestBackend::new(90, 32)).unwrap();
    terminal
        .draw(|frame| {
            drop(app.render_frame(frame));
        })
        .unwrap();
    assert_eq!(app.mcp_call_hits.len(), 2);
    let (row, expected) = app.mcp_call_hits[1].clone();
    app.select_mouse_at(90, 32, row, 85);
    let Some(
        surface @ Surface::Expanded {
            view: ExpandedView::Mcp { call },
            scroll,
            ..
        },
    ) = app.surfaces.last()
    else {
        panic!("MCP row must open expanded details");
    };
    assert_eq!(call.as_ref(), expected.as_ref());
    assert_eq!(*scroll, 0);
    let lines = surface_lines(surface, Path::new("/workspace"), 90);
    let text = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("Parameters:"), "{text}");
    assert!(text.contains("Output:"), "{text}");
    assert!(text.contains("\"release\": 2"), "{text}");
    let json_line = lines
        .iter()
        .find(|line| line.to_string().contains("\"release\""))
        .unwrap();
    assert!(
        json_line
            .spans
            .iter()
            .any(|span| span.style.fg.is_some() && span.style != DIM_STYLE)
    );
}

#[test]
fn welcome_card_grows_for_a_long_model_and_separates_its_details() {
    let model = format!("openrouter/{}", "long-model-name-".repeat(4));
    let lines = welcome_lines(&model, Some("high"), true, 120);
    let rows = welcome_card_rows(&lines, Path::new("/tmp/project"), test_session_id(), 120);
    let rendered = rows.iter().map(ToString::to_string).collect::<Vec<_>>();
    let width = rendered[0].width();

    assert!(width > 60);
    assert!(width <= 120);
    assert!(rendered.iter().all(|row| row.width() == width));
    assert!(rendered[5].contains("model:"));
    assert!(rendered[5].contains("/model to change"));
    assert!(!rendered[6].contains("Effort:"));
    assert!(!rendered[6].contains("Provider:"));
    assert!(rendered[6].contains("high · openrouter"));
    assert!(rendered[6].contains("Or Alt+P"));
    assert!(rendered.iter().any(|row| row.contains("directory:")));
    assert_eq!(lines[2].spans[2].content, " ");
    assert_eq!(lines[2].spans[4].content, "  ");
    assert_eq!(lines[3].spans[2].content, " ");
    assert_eq!(lines[3].spans[4].content, "  ");
    assert_eq!(
        lines[2].spans[3].width(),
        cagent_agent::provider::ModelRef::parse(&model)
            .unwrap()
            .model
            .width()
    );
    assert_eq!(lines[2].width(), lines[3].width());
    assert_eq!(lines[2].width() + 4, width);
    assert_eq!(
        lines[2].spans.last().unwrap().content.trim(),
        "/model to change"
    );
    assert!(
        !lines[2]
            .spans
            .last()
            .unwrap()
            .style
            .add_modifier
            .contains(Modifier::BOLD)
    );
    assert_eq!(lines[3].spans.last().unwrap().content.trim(), "↳ Or Alt+P");
}

#[test]
fn welcome_card_moves_model_setup_into_the_value_column_without_a_selection() {
    let lines = welcome_lines("", None, true, 80);
    let rendered = welcome_card_rows(&lines, Path::new("/tmp/project"), test_session_id(), 80)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();

    assert!(rendered[5].contains("None, set with /model"));
    assert!(rendered[6].contains("Or Alt+P"));
    assert!(rendered.iter().any(|row| row.contains("directory:")));
    assert_eq!(lines[2].spans.len(), 6);
    assert_eq!(lines[3].spans.len(), 4);
    let value_column = |line: &Line<'_>| line.spans[..3].iter().map(Span::width).sum::<usize>();
    assert_eq!(value_column(&lines[2]), value_column(&lines[3]));
    assert_eq!(lines[2].spans[3].content, "None, ");
    assert_eq!(lines[2].spans[4].content, "set with /model");
    assert_eq!(lines[2].spans[4].style, ACCENT_STYLE);
    assert_eq!(lines[3].spans[3].content.trim(), "↳ Or Alt+P");
}

#[test]
fn welcome_card_grows_for_the_directory_up_to_seventy_columns() {
    let welcome = welcome_lines("mock/echo", None, true, 80);
    let workspace = Path::new("/tmp/a-very-long-directory-that-must-expand-the-model-card");
    let card = welcome_card_rows(&welcome, workspace, test_session_id(), 80);

    assert!(
        card.iter()
            .any(|row| row.to_string().contains("directory:"))
    );
    assert!(card.iter().any(|row| row.to_string().contains("session:")));
    assert!(
        card[3]
            .to_string()
            .contains(&workspace.display().to_string())
    );
    assert_eq!(card[0].width(), card[3].width());
}

#[test]
fn welcome_card_directory_width_is_capped_and_respects_terminal_width() {
    let welcome = welcome_lines("mock/echo", None, true, 100);
    let workspace = PathBuf::from(format!("/tmp/{}", "directory/".repeat(20)));
    let wide_card = welcome_card_rows(&welcome, &workspace, test_session_id(), 100);
    let narrow_card = welcome_card_rows(&welcome, &workspace, test_session_id(), 60);

    assert_eq!(wide_card[0].width(), 85);
    assert!(wide_card[3].to_string().contains('…'));
    assert_eq!(narrow_card[0].width(), 60);
    assert!(narrow_card[3].to_string().contains('…'));
}

#[test]
fn welcome_directory_precedes_aligned_model_metadata() {
    let lines = welcome_lines("openai/gpt-5.6-luna", Some("high"), true, 80);
    let rendered = welcome_card_rows(
        &lines,
        Path::new("/home/charles/Projects/coding-agent"),
        test_session_id(),
        80,
    )
    .iter()
    .map(ToString::to_string)
    .collect::<Vec<_>>();
    assert!(rendered[3].contains("directory: ~/Projects/coding-agent"));
    assert!(rendered[4].contains("session:   cagent-0.1.0-260214.1432.950-1ae287ab"));
    assert!(rendered[5].contains("model:     gpt-5.6-luna"));
    assert!(rendered[6].contains("            high · openai"));
    assert_eq!(
        super::welcome::welcome_session_line(test_session_id(), 80).spans[3].style,
        DIM_STYLE
    );
    assert_eq!(
        rendered[3].find("~/Projects").unwrap(),
        rendered[5].find("gpt-5.6").unwrap()
    );
}

#[test]
fn fresh_session_tip_is_below_the_card_and_dark_gray() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.welcome_tip = Some(super::welcome::WELCOME_TIPS[0]);
    app.ensure_history_layout(80);

    let rows = &app
        .history_layout
        .rendered
        .as_ref()
        .expect("history layout is initialized")
        .rows;
    let card_rows = welcome_card_rows(&app.welcome, &app.workspace, app.session_id, 80).len();
    assert!(rows[card_rows].line.to_string().is_empty());
    assert_eq!(
        rows[card_rows + 1].line.to_string(),
        "  Tip: Use /help to see commands and keybindings."
    );
    assert_eq!(rows[card_rows + 1].line.spans[0].style, DIM_STYLE);

    app.push_system_message("model changed");
    app.ensure_history_layout(80);
    let rows = &app
        .history_layout
        .rendered
        .as_ref()
        .expect("history layout is initialized")
        .rows;
    assert!(
        !rows
            .iter()
            .any(|row| row.line.to_string().contains("Tip: "))
    );
}

#[test]
fn welcome_tips_are_documented_and_nonempty() {
    assert!((10..=20).contains(&super::welcome::WELCOME_TIPS.len()));
    assert!(
        super::welcome::WELCOME_TIPS
            .iter()
            .all(|tip| !tip.trim().is_empty())
    );
}

#[test]
fn exploration_groups_use_tree_branches_for_multiple_actions() {
    let lines = render_tool_groups(
        &[ToolActivityGroup::Exploration {
            active: false,
            activities: vec![
                ExplorationActivity {
                    kind: ExplorationActivityKind::List,
                    targets: vec![".".into()],
                    scopes: Vec::new(),
                },
                ExplorationActivity {
                    kind: ExplorationActivityKind::Read,
                    targets: vec!["TEST.md".into()],
                    scopes: Vec::new(),
                },
            ],
        }],
        80,
    );

    assert_eq!(
        lines.iter().map(ToString::to_string).collect::<Vec<_>>(),
        ["• Explored", "  │ List .", "  └ Read TEST.md"]
    );
}

#[test]
fn active_exploration_group_uses_exploring_heading() {
    let lines = render_tool_groups(
        &[ToolActivityGroup::Exploration {
            active: true,
            activities: vec![ExplorationActivity {
                kind: ExplorationActivityKind::List,
                targets: vec![".".into()],
                scopes: Vec::new(),
            }],
        }],
        80,
    );

    assert_eq!(
        lines.iter().map(ToString::to_string).collect::<Vec<_>>(),
        ["• Exploring", "  └ List ."]
    );
    let Some(Color::Rgb(red, green, blue)) = lines[0].spans[0].style.fg else {
        panic!("active exploration dot should use a grayscale RGB pulse");
    };
    assert_eq!(red, green);
    assert_eq!(green, blue);
    assert!((110..=255).contains(&red));
}

#[test]
fn long_exploration_groups_show_live_head_tail_preview() {
    let activities = (1..=6)
        .map(|index| ExplorationActivity {
            kind: ExplorationActivityKind::Read,
            targets: vec![format!("file-{index}.rs")],
            scopes: Vec::new(),
        })
        .collect();
    let lines = render_tool_groups(
        &[ToolActivityGroup::Exploration {
            active: true,
            activities,
        }],
        80,
    );

    assert_eq!(
        lines.iter().map(ToString::to_string).collect::<Vec<_>>(),
        [
            "• Exploring",
            "  │ Read file-1.rs",
            "  │ Read file-2.rs",
            "  │ … +2 items (click to expand)",
            "  │ Read file-5.rs",
            "  └ Read file-6.rs",
        ]
    );
}

#[test]
fn expanded_exploration_keeps_inline_collapse_control() {
    let block_id = cagent_agent::protocol::TranscriptBlockId::derived(
        "tools",
        cagent_agent::protocol::NodeId::new(),
    );
    let target = crate::markdown::ExplorationToggleTarget::Transcript {
        block_id: block_id.clone(),
        group_index: 0,
    };
    let expanded = [target.clone()].into_iter().collect();
    let group = ToolActivityGroup::Exploration {
        active: false,
        activities: (1..=5)
            .map(|index| ExplorationActivity {
                kind: ExplorationActivityKind::Read,
                targets: vec![format!("file-{index}.rs")],
                scopes: Vec::new(),
            })
            .collect(),
    };
    let rows = crate::render::transcript::history_item_rows_with_explorations(
        &crate::app::test_tool_groups(vec![group]),
        Path::new("/workspace"),
        80,
        true,
        Some(&block_id),
        &expanded,
    );

    assert_eq!(
        rows.iter().map(ToString::to_string).collect::<Vec<_>>(),
        [
            "• Explored",
            "  │ Read file-1.rs",
            "  │ Read file-2.rs",
            "  │ Read file-3.rs",
            "  │ Read file-4.rs",
            "  │ Read file-5.rs",
            "  └ … click to collapse",
        ]
    );
    assert_eq!(
        rows.iter()
            .filter_map(|row| row.exploration_toggle())
            .collect::<Vec<_>>(),
        [&target]
    );
}

#[test]
fn list_activity_renders_targets_and_scopes() {
    let lines = render_tool_groups(
        &[ToolActivityGroup::Exploration {
            active: false,
            activities: vec![ExplorationActivity {
                kind: ExplorationActivityKind::List,
                targets: vec![
                    "SPEC.md".into(),
                    "AGENTS.md".into(),
                    "Cargo.toml".into(),
                    "*.rs".into(),
                ],
                scopes: vec![".".into()],
            }],
        }],
        100,
    );

    assert_eq!(
        lines.iter().map(ToString::to_string).collect::<Vec<_>>(),
        [
            "• Explored",
            "  └ List SPEC.md, AGENTS.md, Cargo.toml, *.rs in ."
        ]
    );
}

#[test]
fn exploration_target_separators_are_dimmed() {
    let lines = render_tool_groups(
        &[ToolActivityGroup::Exploration {
            active: false,
            activities: vec![
                ExplorationActivity {
                    kind: ExplorationActivityKind::Read,
                    targets: vec!["one.rs".into(), "two.rs".into()],
                    scopes: Vec::new(),
                },
                ExplorationActivity {
                    kind: ExplorationActivityKind::List,
                    targets: vec!["src".into(), "tests".into()],
                    scopes: Vec::new(),
                },
                ExplorationActivity {
                    kind: ExplorationActivityKind::Search,
                    targets: vec!["needle".into()],
                    scopes: vec!["src".into(), "tests".into()],
                },
            ],
        }],
        80,
    );

    let separators = lines
        .iter()
        .flat_map(|line| &line.spans)
        .filter(|span| matches!(span.content.as_ref(), ", " | " in "))
        .collect::<Vec<_>>();
    assert_eq!(
        separators
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<Vec<_>>(),
        [", ", ", ", " in ", ", "]
    );
    assert!(separators.iter().all(|span| span.style == DIM_STYLE));
}

#[test]
fn rich_exploration_labels_and_relations_render_with_dimmed_relation() {
    let lines = render_tool_groups(
        &[ToolActivityGroup::Exploration {
            active: false,
            activities: vec![ExplorationActivity {
                kind: ExplorationActivityKind::Read,
                targets: vec!["\u{1f}label:Read jj file".into(), "SPEC.md".into()],
                scopes: vec!["\u{1f}relation:in".into(), "@".into()],
            }],
        }],
        80,
    );

    assert_eq!(lines[1].to_string(), "  └ Read jj file SPEC.md in @");
    let relation = lines[1]
        .spans
        .iter()
        .find(|span| span.content == " in ")
        .unwrap();
    assert_eq!(relation.style, DIM_STYLE);
}

#[test]
fn bash_activity_renders_state_exit_color_and_head_tail_output() {
    let lines = render_tool_groups(
        &[ToolActivityGroup::Bash {
            node_id: None,
            terminal_id: None,
            command: "cargo test -p cagent-agent --lib".into(),
            status: ToolActivityStatus::Succeeded,
            output: Some("one\ntwo\nthree\nfour\nfive\nsix\n".into()),
            ansi_output: None,
            exit_code: Some(0),
        }],
        100,
    );
    let rendered = lines.iter().map(ToString::to_string).collect::<Vec<_>>();

    assert_eq!(rendered[0], "• Ran cargo test -p cagent-agent --lib");
    assert_eq!(rendered[1], "  │  one");
    assert_eq!(rendered[2], "  │  two");
    assert_eq!(rendered[3], "  │  … +2 lines (click to view full output)");
    assert_eq!(rendered[4], "  │  five");
    assert_eq!(rendered[5], "  └  six");
    assert_eq!(lines[0].spans[0].style.fg, Some(Color::Green));

    let failed = render_tool_groups(
        &[ToolActivityGroup::Bash {
            node_id: None,
            terminal_id: None,
            command: "false".into(),
            status: ToolActivityStatus::Succeeded,
            output: None,
            ansi_output: None,
            exit_code: Some(1),
        }],
        80,
    );
    assert_eq!(failed[0].spans[0].style, ERROR_STYLE);

    let running = render_tool_groups(
        &[ToolActivityGroup::Bash {
            node_id: None,
            terminal_id: None,
            command: "echo waiting".into(),
            status: ToolActivityStatus::Pending,
            output: None,
            ansi_output: None,
            exit_code: None,
        }],
        80,
    );
    let Some(Color::Rgb(red, green, blue)) = running[0].spans[0].style.fg else {
        panic!("running Bash dot should use a grayscale RGB pulse");
    };
    assert_eq!(red, green);
    assert_eq!(green, blue);
    assert!((110..=255).contains(&red));
}

#[test]
fn failed_transition_activity_uses_standard_error_and_path_styles() {
    let lines = render_tool_groups(
        &[ToolActivityGroup::Tool {
            label: "Change Working Directory".into(),
            target: Some("../missing".into()),
            status: ToolActivityStatus::Failed,
        }],
        80,
    );

    assert_eq!(
        lines[0].to_string(),
        "• Change Working Directory ../missing"
    );
    assert_eq!(lines[0].spans[0].style, ERROR_STYLE);
    assert_eq!(
        lines[0].spans[1].style,
        Style::default().add_modifier(Modifier::BOLD)
    );
    assert_eq!(lines[0].spans[2].style, Style::default());
}

#[test]
fn background_bash_with_no_preview_output_gets_a_heading_hit() {
    let terminal_id = cagent_agent::tools::TerminalId::new();
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.history = vec![crate::app::test_tool_groups(vec![
        ToolActivityGroup::Bash {
            node_id: Some(cagent_agent::protocol::NodeId::new()),
            terminal_id: Some(terminal_id),
            command: "cargo test".into(),
            status: ToolActivityStatus::Pending,
            output: None,
            ansi_output: None,
            exit_code: None,
        },
    ])];

    let candidates = bash_output_candidates(&app);
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].terminal_id, Some(terminal_id));
    assert!(candidates[0].output.is_empty());
    assert_eq!(bash_hit_rows(3, 0, 10), vec![3]);
}

#[test]
fn bash_hit_rows_include_heading_once_before_preview_rows() {
    assert_eq!(bash_hit_rows(3, 2, 10), vec![3, 4, 5]);
    assert_eq!(bash_hit_rows(8, 4, 10), vec![8, 9]);
}

#[test]
fn bash_output_preview_stays_inside_the_available_width() {
    let lines = render_tool_groups(
        &[ToolActivityGroup::Bash {
            node_id: None,
            terminal_id: None,
            command: "cargo test".into(),
            status: ToolActivityStatus::Pending,
            output: Some(
                "Compiling a very long dependency name that exceeds the card\nsecond long output line that also exceeds the card\nthird\nfourth long output line\nfifth long output line"
                    .into(),
            ),
            ansi_output: None,
            exit_code: None,
        }],
        28,
    );

    assert!(lines.iter().all(|line| line.width() <= 28));
    assert!(lines[1].to_string().ends_with('…'));
    assert!(lines[3].to_string().contains("… +1 lines"));
    assert!(lines[3].to_string().ends_with('…'));
}

#[test]
fn collapsed_bash_output_keeps_only_sanitized_head_and_tail_lines() {
    let mut output = String::from("first\nsecond\n");
    for index in 0..10_000 {
        output.push_str(&format!("hidden-{index}\u{1b}[31m\n"));
    }
    output.push_str("penultimate\nlast\n");
    let lines = render_tool_groups(
        &[ToolActivityGroup::Bash {
            node_id: None,
            terminal_id: None,
            command: "large-output".into(),
            status: ToolActivityStatus::Succeeded,
            output: Some(output),
            ansi_output: None,
            exit_code: Some(0),
        }],
        100,
    );
    let rendered = lines.iter().map(ToString::to_string).collect::<Vec<_>>();

    assert_eq!(rendered[1], "  │  first");
    assert_eq!(rendered[2], "  │  second");
    assert_eq!(
        rendered[3],
        "  │  … +10000 lines (click to view full output)"
    );
    assert_eq!(rendered[4], "  │  penultimate");
    assert_eq!(rendered[5], "  └  last");
}

#[test]
fn edited_bash_lines_use_shell_syntax_colors() {
    let diff = cagent_agent::tools::SemanticDiff {
        files: vec![cagent_agent::tools::DiffFile {
            old_path: Some(Path::new("/workspace/script.sh").into()),
            new_path: Some(Path::new("/workspace/script.sh").into()),
            kind: cagent_agent::tools::DiffFileKind::Modified,
            language: Some("bash".into()),
            added_lines: 1,
            removed_lines: 0,
            old_no_final_newline: false,
            new_no_final_newline: false,
            hunks: vec![cagent_agent::tools::DiffHunk {
                header: "@@ -0,0 +1 @@".into(),
                lines: vec![cagent_agent::tools::DiffLine {
                    kind: cagent_agent::tools::DiffLineKind::Addition,
                    old_line: None,
                    new_line: Some(1),
                    text: "if true; then echo \"$HOME\"; fi".into(),
                }],
            }],
        }],
    };

    let lines = render_edit_history(&diff, Path::new("/workspace"), 100);
    let command = lines
        .iter()
        .find(|line| line.to_string().contains("if true"))
        .unwrap();
    assert!(command.spans.iter().any(|span| {
        span.style
            == crate::markdown::code_style(cagent_agent::presentation::CodeTokenKind::Keyword)
    }));
}

#[test]
fn completed_edit_activity_renders_the_diff_below_its_path() {
    let path =
        PathBuf::from(std::env::var_os("HOME").expect("test environment has a home directory"))
            .join(".agents/skills/agent-browser/SKILL.md");
    let diff = cagent_agent::tools::SemanticDiff {
        files: vec![cagent_agent::tools::DiffFile {
            old_path: Some(path.clone()),
            new_path: Some(path),
            kind: cagent_agent::tools::DiffFileKind::Modified,
            language: Some("markdown".into()),
            added_lines: 1,
            removed_lines: 1,
            old_no_final_newline: false,
            new_no_final_newline: false,
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
    let lines = render_tool_groups_with_workspace(
        &[ToolActivityGroup::Edit {
            diff,
            status: ToolActivityStatus::Succeeded,
        }],
        Path::new("/workspace"),
        100,
    );
    let rendered = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    assert!(rendered.contains("Edited ~/.agents/skills/agent-browser/SKILL.md"));
    assert!(rendered.contains("-old"));
    assert!(rendered.contains("+new"));
}

#[test]
fn wrapped_transcript_diff_rows_keep_their_background_to_the_right_edge() {
    let diff = cagent_agent::tools::SemanticDiff {
        files: vec![cagent_agent::tools::DiffFile {
            old_path: Some(Path::new("/workspace/example.md").into()),
            new_path: Some(Path::new("/workspace/example.md").into()),
            kind: cagent_agent::tools::DiffFileKind::Modified,
            language: Some("markdown".into()),
            added_lines: 1,
            removed_lines: 1,
            old_no_final_newline: false,
            new_no_final_newline: false,
            hunks: vec![cagent_agent::tools::DiffHunk {
                header: "@@ -1 +1 @@".into(),
                lines: vec![
                    cagent_agent::tools::DiffLine {
                        kind: cagent_agent::tools::DiffLineKind::Deletion,
                        old_line: Some(1),
                        new_line: None,
                        text: "the old line is long enough to wrap".into(),
                    },
                    cagent_agent::tools::DiffLine {
                        kind: cagent_agent::tools::DiffLineKind::Addition,
                        old_line: None,
                        new_line: Some(1),
                        text: "the new line is long enough to wrap".into(),
                    },
                ],
            }],
        }],
    };

    let rows = crate::render::transcript::history_item_rows(
        &crate::app::test_edits(diff),
        Path::new("/workspace"),
        24,
    );
    let diff_rows = rows
        .iter()
        .filter(|row| row.line.style.bg.is_some())
        .collect::<Vec<_>>();

    assert!(diff_rows.len() > 2, "diff lines should wrap: {diff_rows:?}");
    assert!(diff_rows.iter().all(|row| row.line.width() == 24));
}

#[test]
fn permission_diff_preview_renders_only_the_requested_window() {
    let diff = cagent_agent::tools::SemanticDiff {
        files: vec![cagent_agent::tools::DiffFile {
            old_path: Some(Path::new("/workspace/large.rs").into()),
            new_path: Some(Path::new("/workspace/large.rs").into()),
            kind: cagent_agent::tools::DiffFileKind::Modified,
            language: Some("rust".into()),
            added_lines: 10_000,
            removed_lines: 0,
            old_no_final_newline: false,
            new_no_final_newline: false,
            hunks: vec![cagent_agent::tools::DiffHunk {
                header: "@@ -1,10000 +1,10000 @@".into(),
                lines: (0..10_000)
                    .map(|number| cagent_agent::tools::DiffLine {
                        kind: cagent_agent::tools::DiffLineKind::Context,
                        old_line: Some(number + 1),
                        new_line: Some(number + 1),
                        text: format!("line {number}"),
                    })
                    .collect(),
            }],
        }],
    };

    assert_eq!(diff_preview_line_count(&diff, 80), 10_000);
    let rows = render_diff_preview_window(&diff, 80, 9_990, 3);
    assert_eq!(rows.len(), 3);
    assert!(rows[0].to_string().contains("line 9990"));
    assert!(rows[2].to_string().contains("line 9992"));

    let mut rendered_source_lines = 0;
    let rows =
        super::diff::render_diff_preview_window_with(&diff, 80, 9_990, 3, |_, line, _, _, _| {
            rendered_source_lines += 1;
            vec![ratatui::text::Line::from(line.text.clone())]
        });
    assert_eq!(rendered_source_lines, 3);
    assert_eq!(rows.len(), 3);
}

#[test]
fn permission_diff_preview_wraps_long_content() {
    let diff = cagent_agent::tools::SemanticDiff {
        files: vec![cagent_agent::tools::DiffFile {
            old_path: Some(Path::new("/workspace/example.rs").into()),
            new_path: Some(Path::new("/workspace/example.rs").into()),
            kind: cagent_agent::tools::DiffFileKind::Modified,
            language: Some("rust".into()),
            added_lines: 1,
            removed_lines: 0,
            old_no_final_newline: false,
            new_no_final_newline: false,
            hunks: vec![cagent_agent::tools::DiffHunk {
                header: "@@ -1 +1 @@".into(),
                lines: vec![cagent_agent::tools::DiffLine {
                    kind: cagent_agent::tools::DiffLineKind::Addition,
                    old_line: None,
                    new_line: Some(1),
                    text: "let long_value = \"abcdefghijklmnopqrstuvwxyz0123456789\";".into(),
                }],
            }],
        }],
    };

    let rows = render_diff_preview_window(&diff, 24, 0, 10);
    assert!(rows.len() > 1);
    assert!(rows.iter().all(|line| line.width() <= 24));
    assert!(
        rows.iter()
            .any(|line| line.to_string().contains("long_value"))
    );
}

#[test]
fn permission_diff_row_count_matches_render_wrapping() {
    for text in [
        "",
        "short",
        "words wrap at spaces",
        "abcdefghijklmnopqrstuvwxyz",
        "emoji 🧑‍💻 and 漢字 stay whole",
        "first\nsecond\n",
    ] {
        for width in 1..=24 {
            assert_eq!(
                super::diff::wrapped_row_count(text, width),
                wrap_ranges(text, width).len(),
                "text={text:?}, width={width}",
            );
        }
    }
}

#[test]
fn completed_tool_groups_reflow_when_the_terminal_width_changes() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.history.push(crate::app::test_tool_groups(vec![
        ToolActivityGroup::Exploration {
            active: false,
            activities: vec![ExplorationActivity {
                kind: ExplorationActivityKind::Read,
                targets: vec!["a/very/long/path/that/must/wrap/at/narrow/width.md".into()],
                scopes: Vec::new(),
            }],
        },
    ]));

    app.ensure_history_layout(80);
    let wide = app.history_layout.rendered.as_ref().unwrap().rows.len();
    app.ensure_history_layout(16);
    let narrow = app.history_layout.rendered.as_ref().unwrap().rows.len();

    assert!(narrow > wide);
    assert_eq!(app.history_layout.rendered.as_ref().unwrap().width, 16);
}

#[test]
fn fresh_transcript_layout_materializes_the_tail_then_preserves_anchor_while_expanding() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.welcome.clear();
    for block in 0..10 {
        app.history.push(crate::app::test_lines(
            (0..10)
                .map(|row| Line::from(format!("block-{block}-row-{row}")))
                .collect(),
        ));
        app.history.last_mut().unwrap().id =
            cagent_agent::protocol::TranscriptBlockId(format!("block-{block}"));
    }

    app.ensure_history_tail_layout(80, 5);

    assert_eq!(app.history_layout.start_block, 9);
    assert_eq!(app.history_block_layouts.len(), 1);
    app.ensure_history_tail_layout(80, 5);
    assert_eq!(app.history_layout.start_block, 9);
    assert_eq!(app.history_block_layouts.len(), 1);
    let previous = app.history_layout.rendered.as_ref().unwrap().rows.clone();
    app.follow_history_tail = false;
    app.history_scroll = 3;
    app.prepare_history_scroll_up(3);
    let expanded = &app.history_layout.rendered.as_ref().unwrap().rows;
    let added = expanded.len() - previous.len();
    assert_eq!(app.history_scroll, 3 + added);
    assert_eq!(expanded[app.history_scroll].line, previous[3].line);

    while app.history_layout_incomplete() {
        app.materialize_older_history_layout();
    }
    assert_eq!(app.history_layout.start_block, 0);
    assert_eq!(app.history_block_layouts.len(), 10);

    // A resize starts another bounded width-specific tail; a caller that
    // needs the exact beginning can still complete it synchronously.
    app.ensure_history_tail_layout(40, 5);
    assert_eq!(app.history_layout.start_block, 9);
    assert_eq!(app.history_layout.width, Some(40));
    app.ensure_history_layout(40);
    assert_eq!(app.history_layout.start_block, 0);
}

#[test]
fn tail_layout_expands_and_preserves_its_anchor_when_the_viewport_grows() {
    let mut app = App::new(
        Path::new("/tmp/project"),
        ("mock".into(), "echo".into(), None),
        ["mock".into()].into_iter().collect(),
        "ask",
        None,
    );
    app.welcome.clear();
    for block in 0..10 {
        app.history.push(crate::app::test_lines(
            (0..10)
                .map(|row| Line::from(format!("block-{block}-row-{row}")))
                .collect(),
        ));
        app.history.last_mut().unwrap().id =
            cagent_agent::protocol::TranscriptBlockId(format!("block-{block}"));
    }

    app.ensure_history_tail_layout(80, 5);
    let previous = app.history_layout.rendered.as_ref().unwrap().rows.clone();
    assert_eq!(app.history_layout.start_block, 9);
    app.follow_history_tail = false;
    app.history_scroll = 3;

    app.ensure_history_tail_layout(80, 15);

    let expanded = &app.history_layout.rendered.as_ref().unwrap().rows;
    assert!(app.history_layout.start_block < 9);
    assert!(expanded.len() >= 30);
    assert_eq!(
        app.history_scroll,
        3 + expanded.len().saturating_sub(previous.len())
    );
    assert_eq!(expanded[app.history_scroll].line, previous[3].line);
}

#[test]
fn composer_styles_only_a_valid_leading_command() {
    let spans = styled_draft_slice("/model gpt-5", &[], 0, Some("/model".len()));

    assert_eq!(spans[0].content, "/model");
    assert_eq!(spans[0].style.fg, Some(Color::Cyan));
    assert_eq!(spans[1].content, " gpt-5");
    assert_eq!(spans[1].style.fg, None);
}
