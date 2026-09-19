#[allow(clippy::wildcard_imports)]
use super::*;
use std::collections::BTreeSet;
use std::time::Duration;

/// Starts the background-terminal event bridge for a session.
///
/// Terminal execution remains owned by the shell supervisor; this bridge is
/// responsible only for durable snapshots, frontend events, and completion
/// notifications consumed by the session loop.
pub(super) fn start_terminal_supervisor(
    session_id: ConversationId,
    shell: &crate::ShellExecutor,
    store: StoreHandle,
    transient: broadcast::Sender<crate::TransientEvent>,
    completions: Arc<tokio::sync::Notify>,
    shutdown: CancellationToken,
) -> crate::TerminalSupervisor {
    let (terminals, mut events) = crate::TerminalSupervisor::new(shell);
    let snapshots = terminals.clone();
    tokio::spawn(
        async move {
            // PTYs may deliver thousands of chunks per second. Persist and publish
            // at a human-visible cadence, but flush an exit immediately so the
            // final durable state always precedes completion notification.
            let mut pending = BTreeSet::new();
            let mut checkpoint = tokio::time::interval(Duration::from_millis(100));
            checkpoint.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let publish = |terminal: crate::TerminalSnapshot, completed: bool| {
                let store = store.clone();
                let transient = transient.clone();
                let completions = completions.clone();
                async move {
                    if let Err(error) = store.upsert_terminal(terminal.clone()).await {
                        tracing::error!(%error, "failed to persist terminal update");
                        return;
                    }
                    let detached = terminal.read_safe.is_none()
                        && terminal.owner_agent_run_id.is_none()
                        && terminal.tool_call_node_id.is_some();
                    let _ = transient.send(crate::TransientEvent::TerminalUpdated {
                        terminal: Box::new(terminal),
                    });
                    if completed && !detached {
                        completions.notify_one();
                    }
                }
            };
            let evict_completed = |id| {
                let snapshots = snapshots.clone();
                tokio::spawn(async move {
                    // Give synchronous waiters a short opportunity to observe
                    // the final in-memory state. Durable reads fall back to the
                    // store after this eviction.
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    snapshots.remove_completed(id);
                });
            };
            loop {
                tokio::select! {
                    () = shutdown.cancelled() => break,
                    maybe_event = events.recv() => {
                        let Some(event) = maybe_event else {
                            for id in std::mem::take(&mut pending) {
                                let _ = snapshots.acknowledge_update(id);
                                if let Ok(terminal) = snapshots.snapshot(id) {
                                    publish(terminal, false).await;
                                }
                            }
                            break;
                        };
                        match event {
                            crate::TerminalSupervisorEvent::Started(terminal) => {
                                publish(terminal, false).await;
                            }
                            crate::TerminalSupervisorEvent::Updated(id) => {
                                pending.insert(id);
                            }
                            crate::TerminalSupervisorEvent::Completed(id) => {
                                pending.remove(&id);
                                match snapshots.snapshot(id) {
                                    Ok(terminal) => publish(terminal, true).await,
                                    Err(error) => tracing::error!(
                                        %id,
                                        %error,
                                        "failed to snapshot completed terminal"
                                    ),
                                }
                                // Eviction must not depend on persistence
                                // succeeding, or a store failure leaks the PTY.
                                evict_completed(id);
                            }
                        }
                    }
                    _ = checkpoint.tick() => {
                        for id in std::mem::take(&mut pending) {
                            // Clear the flag before snapshotting. Output that
                            // arrives concurrently can then enqueue the next
                            // checkpoint without being lost.
                            let _ = snapshots.acknowledge_update(id);
                            if let Ok(terminal) = snapshots.snapshot(id) {
                                publish(terminal, false).await;
                            }
                        }
                    }
                }
            }
        }
        .instrument(tracing::info_span!(
            "agent.session.terminal_updates",
            %session_id
        )),
    );
    terminals
}
