#[allow(clippy::wildcard_imports)]
use super::*;

/// Keep read-only proof attached to the segment that owns each operand. A
/// sibling reading or writing the same path must not inherit that proof.
pub(super) fn permission_paths<'a>(
    analysis: &'a crate::ShellAnalysis,
    safe: Option<&'a crate::SafeShellClassification>,
    authorization: Option<&'a crate::SafeBashAuthorization>,
) -> Vec<(&'a str, crate::ShellPathAccess, bool)> {
    if let Some(safe) = safe {
        return safe
            .paths
            .iter()
            .map(|path| (path.as_str(), crate::ShellPathAccess::Read, true))
            .collect();
    }
    let mut paths = analysis
        .paths
        .iter()
        .filter(|path| {
            !path.dynamic
                && !path.segment_index.is_some_and(|index| {
                    authorization.is_some_and(|proof| proof.segment(index).is_some())
                })
        })
        .map(|path| (path.value.as_str(), path.access, false))
        .collect::<Vec<_>>();
    if let Some(authorization) = authorization {
        // Use the classifier's semantic operands, including expanded paths
        // and commands not covered by the fallback argument collector.
        paths.extend(authorization.segments.values().flat_map(|proof| {
            proof
                .paths
                .iter()
                .map(|path| (path.as_str(), crate::ShellPathAccess::Read, true))
        }));
    }
    paths
}

/// Executes an authorized Bash call through the terminal supervisor.
#[allow(clippy::too_many_arguments)]
#[tracing::instrument(
    level = "trace",
    name = "agent.tools.bash",
    skip_all,
    fields(session_id = %conversation_id, node_id = %node_id)
)]
pub(super) async fn execute_bash(
    runtime: &ToolRuntime,
    request: crate::BashRequest,
    node_id: crate::NodeId,
    tool_call_id: &str,
    conversation_id: ConversationId,
    mode: &str,
    agent: &str,
    primary_selection: &SessionSelection,
    classifier_input: &[ModelInput],
    cancellation: &CancellationToken,
    audits: &mut Vec<crate::PermissionAudit>,
) -> Result<serde_json::Value, String> {
    let workspace = runtime.workspace_state();
    let analysis = crate::analyze_shell(&request.command).map_err(|error| error.to_string())?;
    workspace
        .shell
        .wait_for_inventory(cancellation)
        .await
        .map_err(|error| error.to_string())?;
    let authorization = crate::authorize_available_safe_bash_segments(
        &request,
        workspace.shell.inventory(),
        &analysis,
        runtime.config().shell().safe_level,
        runtime.config().shell().safe_write,
    );
    let safe = authorization
        .as_ref()
        .and_then(|authorization| authorization.whole.as_ref());
    let values = super::authorize_bash(
        runtime,
        &analysis,
        &request,
        mode,
        agent,
        primary_selection,
        classifier_input,
        safe,
        authorization.as_ref(),
        cancellation,
    )
    .await;
    match values {
        Ok(values) => audits.extend(values),
        Err((values, message)) => {
            audits.extend(values);
            return Err(message);
        }
    }

    let mut execution_request = request;
    if let Some(safe) = &safe
        && let Some(command) = &safe.hardened_command
    {
        execution_request.command = command.clone();
    }
    let frontend_terminal = runtime
        .frontend_terminal
        .read()
        .map_err(|_| "frontend terminal lock poisoned".to_owned())?
        .clone();
    if let Some(frontend_terminal) = frontend_terminal.filter(|_| execution_request.wait) {
        let cwd = execution_request
            .cwd
            .as_ref()
            .map_or_else(|| workspace.cwd.clone(), |cwd| workspace.cwd.join(cwd));
        let output_limit = u64::try_from(workspace.shell.model_output_bytes()).unwrap_or(u64::MAX);
        let result = frontend_terminal
            .execute(
                crate::frontend::FrontendTerminalRequest {
                    tool_call_id: tool_call_id.to_owned(),
                    command: execution_request.command.clone(),
                    cwd,
                    env: execution_request.env,
                    output_byte_limit: output_limit,
                },
                cancellation.clone(),
            )
            .await?;
        return Ok(serde_json::json!({
            "type": "terminal_completion",
            "command": execution_request.command,
            "status": if result.exit_code == Some(0) { "exited" } else { "failed" },
            "termination": result.signal.as_ref().map_or("exited", |_| "cancelled"),
            "exit_code": result.exit_code,
            "signal": result.signal,
            "output": result.output,
            "discarded_bytes": 0,
            "truncated": result.truncated,
        }));
    }
    let started = runtime
        .terminals
        .start_classified(conversation_id, node_id, &execution_request, safe.is_some())
        .map_err(|error| error.to_string())?;
    let snapshot = runtime
        .terminals
        .snapshot(started.id)
        .map_err(|error| error.to_string())?;
    runtime
        .store
        .upsert_terminal(snapshot)
        .await
        .map_err(|error| error.to_string())?;
    if !execution_request.wait {
        return serde_json::to_value(started).map_err(|error| error.to_string());
    }
    let completed = match runtime.terminals.wait(started.id, cancellation).await {
        Ok(completed) => completed,
        Err(crate::ShellError::Cancelled) => {
            // The cancelling wait has requested graceful termination. Keep the
            // foreground tool call alive until the final snapshot is durable
            // and claimed, otherwise its completion can start a new turn just
            // after the parent interruption finishes.
            let completed = runtime
                .terminals
                .wait(started.id, &CancellationToken::new())
                .await
                .map_err(|error| error.to_string())?;
            runtime
                .store
                .upsert_terminal(completed)
                .await
                .map_err(|error| error.to_string())?;
            let _ = runtime
                .store
                .claim_completion(
                    conversation_id,
                    "terminal",
                    started.id.to_string(),
                    "interrupted_wait",
                )
                .await
                .map_err(|error| error.to_string())?;
            return Err(crate::ShellError::Cancelled.to_string());
        }
        Err(error) => return Err(error.to_string()),
    };
    runtime
        .store
        .upsert_terminal(completed.clone())
        .await
        .map_err(|error| error.to_string())?;
    let _ = runtime
        .store
        .claim_completion(
            conversation_id,
            "terminal",
            started.id.to_string(),
            "bash_wait",
        )
        .await
        .map_err(|error| error.to_string())?;
    Ok(super::terminal_completion_envelope(
        &completed,
        workspace.shell.model_output_bytes(),
    ))
}
