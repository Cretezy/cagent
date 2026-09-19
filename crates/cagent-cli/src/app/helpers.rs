//! Stateless input, tree, clipboard, and terminal-event helpers.

use super::*;

pub(super) fn trim_outer_blank_lines(text: &str) -> String {
    let mut lines = text.split('\n').collect::<Vec<_>>();
    while lines.first().is_some_and(|line| line.trim().is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

pub(super) fn schedule_completion(
    session: &SessionHandle,
    app: &App,
    sender: &tokio::sync::mpsc::Sender<CompletionResult>,
    scheduled: &mut Option<CompletionRequest>,
    completion_task: &mut Option<tokio::task::JoinHandle<()>>,
    path_completion: &mut Option<PathCompletionState>,
) {
    if app.is_observer() {
        clear_scheduled_completion(scheduled, completion_task, path_completion);
        return;
    }
    let Some(completion) = app.attachment_completion.as_ref() else {
        clear_scheduled_completion(scheduled, completion_task, path_completion);
        return;
    };
    if completion.start >= completion.end || completion.end > app.draft.len() {
        clear_scheduled_completion(scheduled, completion_task, path_completion);
        return;
    }
    let raw = app.draft[completion.start..completion.end].to_owned();
    let Some(prefix) = attachment_completion_prefix(&raw) else {
        clear_scheduled_completion(scheduled, completion_task, path_completion);
        return;
    };
    let request = CompletionRequest {
        start: completion.start,
        end: completion.end,
        raw,
        prefix,
    };
    if scheduled.as_ref() == Some(&request) {
        return;
    }
    if let Some(completion_task) = completion_task.take() {
        completion_task.abort();
    }
    *scheduled = Some(request.clone());

    if request.prefix.is_empty() {
        ensure_path_completion(session, request.start, path_completion);
        let session = session.clone();
        let sender = sender.clone();
        *completion_task = Some(tokio::spawn(
            async move {
                let rows = session.directory_completion_listing().await;
                let _ = sender.send(CompletionResult { request, rows }).await;
            }
            .in_current_span(),
        ));
        return;
    }

    ensure_path_completion(session, request.start, path_completion);
    let path_session = path_completion
        .as_ref()
        .expect("path completion was just initialized")
        .session
        .clone();
    if let Some(rows) = path_session.complete_cached(&request.prefix) {
        let _ = sender.try_send(CompletionResult {
            request: request.clone(),
            rows,
        });
        return;
    }
    let sender = sender.clone();
    *completion_task = Some(tokio::spawn(
        async move {
            let rows = path_session.complete(&request.prefix).await;
            let _ = sender.send(CompletionResult { request, rows }).await;
        }
        .in_current_span(),
    ));
}

fn ensure_path_completion(
    session: &SessionHandle,
    start: usize,
    path_completion: &mut Option<PathCompletionState>,
) {
    if path_completion
        .as_ref()
        .is_some_and(|completion| completion.start == start)
    {
        return;
    }
    *path_completion = Some(PathCompletionState::new(session, start));
}

fn clear_scheduled_completion(
    scheduled: &mut Option<CompletionRequest>,
    completion_task: &mut Option<tokio::task::JoinHandle<()>>,
    path_completion: &mut Option<PathCompletionState>,
) {
    if let Some(completion_task) = completion_task.take() {
        completion_task.abort();
    }
    *scheduled = None;
    *path_completion = None;
}

pub(super) fn attachment_completion_prefix(raw: &str) -> Option<String> {
    let token = raw.strip_prefix('@')?;
    if token.starts_with('@') {
        return None;
    }
    if let Ok([reference]) = cagent_agent::parse_attachment_references(raw).as_deref() {
        return Some(reference.spec.path.to_string_lossy().into_owned());
    }
    Some(
        token
            .strip_prefix('{')
            .unwrap_or(token)
            .strip_suffix('}')
            .unwrap_or(token.strip_prefix('{').unwrap_or(token))
            .to_owned(),
    )
}

pub(super) fn split_command(input: &str) -> (&str, Option<&str>) {
    input
        .split_once(char::is_whitespace)
        .map_or((input, None), |(command, argument)| {
            let argument = argument.trim();
            (command, (!argument.is_empty()).then_some(argument))
        })
}

pub(super) fn help_command_rows() -> Vec<(String, String)> {
    let mut rows = SLASH_COMMANDS
        .iter()
        .map(|command| {
            let name = command.argument_hint.map_or_else(
                || command.name.to_owned(),
                |hint| format!("{} {hint}", command.name),
            );
            (name, command.description.to_owned())
        })
        .collect::<Vec<_>>();
    let mode_index = rows
        .iter()
        .position(|(name, _)| name.starts_with("/mode "))
        .map_or(rows.len(), |index| index + 1);
    rows.insert(
        mode_index,
        (
            "/<mode> [message]".into(),
            "switch to an enabled mode and optionally send a message".into(),
        ),
    );
    rows
}

pub(super) fn configured_help_command_rows(
    manual_cleanup_available: bool,
) -> Vec<(String, String)> {
    help_command_rows()
        .into_iter()
        .filter(|(name, _)| manual_cleanup_available || !name.starts_with("/cleanup"))
        .collect()
}

pub(super) fn observer_help_command_rows() -> Vec<(String, String)> {
    let mut rows = SLASH_COMMANDS
        .iter()
        .filter(|command| command.observer_safe)
        .map(|command| {
            if command.name == "/diff" {
                return (
                    "/diff [conversation|git]".into(),
                    "view conversation or repository changes (read-only)".into(),
                );
            }
            let name = command.argument_hint.map_or_else(
                || command.name.to_owned(),
                |hint| format!("{} {hint}", command.name),
            );
            (name, command.description.to_owned())
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| left.0.cmp(&right.0));
    rows
}

#[cfg(test)]
pub(crate) fn history_tree_lines(
    input: &[cagent_agent::protocol::HistoryNode],
    selected: usize,
    purpose: TreePurpose,
    width: u16,
) -> Vec<Line<'static>> {
    let title = if purpose == TreePurpose::Fork {
        "Fork from previous message"
    } else {
        "Conversation tree"
    };
    let mut lines = vec![Line::from(title).style(ACCENT_STYLE), Line::default()];
    let rows = cagent_agent::presentation::project_history_rows(input);
    for (index, row) in rows.into_iter().enumerate() {
        let guide = if purpose == TreePurpose::Browse {
            row.guide.clone()
        } else {
            String::new()
        };
        if let Some(parent_guide) = guide
            .strip_suffix("├─ ")
            .or_else(|| guide.strip_suffix("└─ "))
        {
            lines.push(Line::from(format!("{parent_guide}│")));
        }
        lines.push(history_tree_row_line(
            &row,
            index == selected,
            purpose,
            width,
        ));
    }
    lines
}

pub(crate) fn history_tree_row_line(
    row: &cagent_agent::presentation::HistoryRow,
    selected: bool,
    purpose: TreePurpose,
    width: u16,
) -> Line<'static> {
    let kind = row.label();
    let guide = if purpose == TreePurpose::Browse {
        row.guide.as_str()
    } else {
        ""
    };
    let status = row
        .status
        .as_ref()
        .map_or_else(String::new, |status| format!(" [{status}]"));
    let preview = terminal_safe(&row.preview);
    let marker = if selected {
        "◉"
    } else if row.active {
        "●"
    } else {
        "○"
    };
    let neutral_prefix = format!("{guide}{marker} ");
    let label = format!("{kind}: ");
    let available = usize::from(width)
        .saturating_sub(neutral_prefix.width())
        .saturating_sub(label.width())
        .saturating_sub(status.width());
    let preview = crate::render::truncate_with_ellipsis(&preview, available);
    let role_style = history_tree_row_style(row.kind);
    let text_style = if selected {
        role_style
            .unwrap_or(SELECTED_STYLE)
            .add_modifier(Modifier::BOLD)
    } else {
        role_style.unwrap_or_default()
    };
    let preview_style = if selected {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    Line::from(vec![
        Span::raw(neutral_prefix),
        Span::styled(label, text_style),
        Span::styled(preview, preview_style),
        Span::styled(status, DIM_STYLE),
    ])
}

fn history_tree_row_style(kind: cagent_agent::presentation::HistoryRowKind) -> Option<Style> {
    use cagent_agent::presentation::HistoryRowKind;

    match kind {
        HistoryRowKind::Exploration
        | HistoryRowKind::Run
        | HistoryRowKind::ToolCall
        | HistoryRowKind::ToolResult => Some(Style::new().fg(Color::Yellow)),
        HistoryRowKind::User | HistoryRowKind::AcceptedPlan => Some(Style::new().fg(Color::Green)),
        HistoryRowKind::Assistant | HistoryRowKind::AssistantPlan | HistoryRowKind::Mutation => {
            Some(Style::new().fg(Color::Indexed(214)))
        }
        HistoryRowKind::Compact | HistoryRowKind::Notice => Some(Style::new().fg(Color::DarkGray)),
        _ => None,
    }
}

#[cfg(test)]
pub(super) fn history_tree_node_style(node: &cagent_agent::protocol::HistoryNode) -> Option<Style> {
    cagent_agent::presentation::project_history_rows(std::slice::from_ref(node))
        .first()
        .map(|row| history_tree_row_style(row.kind))
        .flatten()
}

pub(super) fn tree_initial_selection(rows: &[cagent_agent::presentation::HistoryRow]) -> usize {
    rows.iter()
        .position(|row| row.active && tree_node_selectable(row))
        .or_else(|| rows.iter().rposition(tree_node_selectable))
        .unwrap_or_default()
}

pub(super) const fn tree_node_selectable(node: &cagent_agent::presentation::HistoryRow) -> bool {
    node.selectable
}

pub(super) fn copy_dialect(
    argument: Option<&str>,
) -> Result<Option<cagent_agent::presentation::MarkdownDialect>, &'static str> {
    match argument {
        None => Ok(None),
        Some("slack") => Ok(Some(cagent_agent::presentation::MarkdownDialect::Slack)),
        Some("discord") => Ok(Some(cagent_agent::presentation::MarkdownDialect::Discord)),
        Some(_) => Err("usage: /copy [slack|discord]"),
    }
}

pub(super) fn adjust_chip_ranges<T>(
    chips: &mut Vec<T>,
    start: usize,
    end: usize,
    removed: usize,
    range: impl Fn(&mut T) -> &mut ChipRange,
) {
    chips.retain_mut(|chip| {
        let range = range(chip);
        if start < range.end && end > range.start {
            return false;
        }
        if end <= range.start {
            range.start -= removed;
            range.end -= removed;
        }
        true
    });
}

pub(super) fn copy_to_clipboard(text: &str) -> bool {
    #[cfg(target_os = "macos")]
    let candidates: &[(&str, &[&str])] = &[("pbcopy", &[])];
    #[cfg(target_os = "windows")]
    let candidates: &[(&str, &[&str])] = &[("clip", &[])];
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let candidates: &[(&str, &[&str])] =
        &[("wl-copy", &[]), ("xclip", &["-selection", "clipboard"])];
    candidates.iter().any(|(program, args)| {
        let Ok(mut child) = Command::new(program)
            .args(*args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            return false;
        };
        child
            .stdin
            .take()
            .is_some_and(|mut stdin| std::io::Write::write_all(&mut stdin, text.as_bytes()).is_ok())
            && child.wait().is_ok_and(|status| status.success())
    })
}

pub(super) fn open_browser(url: &str) -> bool {
    #[cfg(target_os = "macos")]
    let command = ("open", Vec::<&str>::new());
    #[cfg(target_os = "windows")]
    let command = ("rundll32", vec!["url.dll,FileProtocolHandler"]);
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let command = ("xdg-open", Vec::<&str>::new());
    let Ok(mut child) = Command::new(command.0)
        .args(command.1)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    // Browser launchers can outlive the click handler; reap without blocking input.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    true
}

pub(super) fn is_paste_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('v' | 'V')) && key.modifiers.contains(KeyModifiers::CONTROL)
}

pub(super) fn is_escape_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Esc | KeyCode::Char('\x1b'))
}

pub(super) fn is_word_backspace(key: KeyEvent) -> bool {
    key.code == KeyCode::Backspace
        && key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        && !key.modifiers.contains(KeyModifiers::SHIFT)
}

pub(super) fn is_word_left(key: KeyEvent) -> bool {
    key.code == KeyCode::Left
        && key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        && !key.modifiers.contains(KeyModifiers::SHIFT)
}

pub(super) fn is_word_right(key: KeyEvent) -> bool {
    key.code == KeyCode::Right
        && key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        && !key.modifiers.contains(KeyModifiers::SHIFT)
}

#[cfg(test)]
pub(super) fn menu_navigation_code(key: KeyEvent) -> KeyCode {
    if key.modifiers == KeyModifiers::CONTROL {
        match key.code {
            KeyCode::Char('k' | 'K') => return KeyCode::Up,
            KeyCode::Char('j' | 'J') => return KeyCode::Down,
            _ => {}
        }
    }
    key.code
}

pub(super) fn permission_rule_edit_details(
    request: &InteractionRequest,
    selected: usize,
    scope: cagent_agent::permissions::PermissionScope,
) -> Option<(
    cagent_agent::permissions::PermissionRule,
    usize,
    cagent_agent::permissions::PermissionScope,
)> {
    let visible = permission_approval_choices(request, scope)
        .get(selected)
        .cloned()?;
    let scope = if visible.supports_session() {
        cagent_agent::permissions::PermissionScope::Conversation
    } else if visible.is_persistent() {
        match scope {
            cagent_agent::permissions::PermissionScope::Conversation => {
                cagent_agent::permissions::PermissionScope::Project
            }
            cagent_agent::permissions::PermissionScope::ConversationGlobal => {
                cagent_agent::permissions::PermissionScope::Global
            }
            scope => scope,
        }
    } else {
        scope
    };
    let mut rule = permission_approval_choices(request, scope)
        .get(selected)
        .and_then(|choice| {
            if visible.supports_session() {
                choice.session_rule.clone()
            } else {
                choice.prepared_rule.clone()
            }
        })?;
    let InteractionRequestKind::PermissionApproval { resource, .. } = &request.kind else {
        return None;
    };
    if rule.tool.as_deref() == Some("bash") && rule.path.is_none() && !resource.command.is_empty() {
        // Persistent approvals start conservatively with the short command
        // pattern, but editing should expose the exact parsed invocation that
        // prompted this request.
        rule.command = Some(resource.command.clone());
        rule.raw_command = None;
    }
    let (pattern, _) = rule.editable_pattern()?;
    Some((rule, pattern.len(), scope))
}

pub(super) fn permission_submission_choice(
    request: &InteractionRequest,
    selected: usize,
    scope: cagent_agent::permissions::PermissionScope,
    force_global: bool,
) -> Option<cagent_agent::presentation::PermissionApprovalChoice> {
    let choice = permission_approval_choices(request, scope)
        .get(selected)
        .cloned()?;
    let scope = if force_global && choice.supports_session() {
        cagent_agent::permissions::PermissionScope::Conversation
    } else if force_global {
        cagent_agent::permissions::PermissionScope::Global
    } else {
        scope
    };
    permission_approval_choices(request, scope)
        .get(selected)
        .cloned()
}

pub(super) fn read_from_clipboard() -> Option<String> {
    #[cfg(target_os = "macos")]
    let candidates: &[(&str, &[&str])] = &[("pbpaste", &[])];
    #[cfg(target_os = "windows")]
    let candidates: &[(&str, &[&str])] =
        &[("powershell", &["-NoProfile", "-Command", "Get-Clipboard"])];
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let candidates: &[(&str, &[&str])] = &[
        ("wl-paste", &["--no-newline"]),
        ("xclip", &["-selection", "clipboard", "-o"]),
    ];

    candidates.iter().find_map(|(program, args)| {
        let output = Command::new(program).args(*args).output().ok()?;
        if !output.status.success() {
            return None;
        }
        String::from_utf8(output.stdout).ok()
    })
}

/// Reads raster clipboard bytes before text. Platform commands are kept in
/// the frontend; decoding and durable normalization belong to cagent-agent.
pub(super) fn read_image_from_clipboard() -> Option<Vec<u8>> {
    #[cfg(target_os = "macos")]
    let candidates: &[(&str, &[&str])] = &[("pngpaste", &["-"])];
    #[cfg(target_os = "windows")]
    let candidates: &[(&str, &[&str])] = &[(
        "powershell",
        &[
            "-NoProfile",
            "-STA",
            "-Command",
            "Add-Type -AssemblyName System.Windows.Forms; Add-Type -AssemblyName System.Drawing; $image=[Windows.Forms.Clipboard]::GetImage(); if ($null -eq $image) { exit 1 }; $image.Save([Console]::OpenStandardOutput(), [Drawing.Imaging.ImageFormat]::Png)",
        ],
    )];
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let candidates: &[(&str, &[&str])] = &[
        ("wl-paste", &["--no-newline", "--type", "image/png"]),
        (
            "xclip",
            &["-selection", "clipboard", "-target", "image/png", "-o"],
        ),
    ];
    if let Some(bytes) = candidates.iter().find_map(|(program, args)| {
        let output = Command::new(program).args(*args).output().ok()?;
        (output.status.success()
            && !output.stdout.is_empty()
            && image::guess_format(&output.stdout).is_ok())
        .then_some(output.stdout)
    }) {
        return Some(bytes);
    }

    clipboard_image_file().and_then(|path| {
        const MAX_CLIPBOARD_IMAGE_BYTES: u64 = 32 * 1024 * 1024;
        let metadata = std::fs::metadata(&path).ok()?;
        if metadata.len() > MAX_CLIPBOARD_IMAGE_BYTES {
            return None;
        }
        let bytes = std::fs::read(path).ok()?;
        image::guess_format(&bytes).is_ok().then_some(bytes)
    })
}

fn clipboard_image_file() -> Option<std::path::PathBuf> {
    #[cfg(target_os = "windows")]
    {
        let output = Command::new("powershell")
            .args([
                "-NoProfile",
                "-STA",
                "-Command",
                "Add-Type -AssemblyName System.Windows.Forms; $files=[Windows.Forms.Clipboard]::GetFileDropList(); if ($files.Count -eq 0) { exit 1 }; $files[0]",
            ])
            .output()
            .ok()?;
        let path = String::from_utf8(output.stdout).ok()?;
        let path = std::path::PathBuf::from(path.trim());
        return path.is_file().then_some(path);
    }
    #[cfg(not(target_os = "windows"))]
    {
        let candidates: &[(&str, &[&str])] = &[
            ("wl-paste", &["--no-newline", "--type", "text/uri-list"]),
            ("pbpaste", &[]),
        ];
        candidates.iter().find_map(|(program, args)| {
            let output = Command::new(program).args(*args).output().ok()?;
            let text = String::from_utf8(output.stdout).ok()?;
            let uri = text.lines().find(|line| line.starts_with("file://"))?;
            let path = url::Url::parse(uri.trim()).ok()?.to_file_path().ok()?;
            path.is_file().then_some(path)
        })
    }
}
