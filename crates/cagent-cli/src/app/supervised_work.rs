use std::collections::BTreeMap;

use cagent_agent::presentation::SupervisedWork;
use cagent_agent::protocol::AgentRunId;

/// Stores the agent-owned work projection used by the supervised-work browser.
#[derive(Default)]
pub(crate) struct SupervisedWorkState {
    pub(crate) rows: Vec<SupervisedWork>,
    terminals: BTreeMap<cagent_agent::tools::TerminalId, cagent_agent::tools::TerminalSnapshot>,
    pub(crate) agent_run_live: BTreeMap<AgentRunId, cagent_agent::presentation::DelegatedRunLive>,
}

impl SupervisedWorkState {
    pub(crate) fn hydrate(
        &mut self,
        rows: Vec<SupervisedWork>,
        terminals: Vec<cagent_agent::tools::TerminalSnapshot>,
    ) {
        let agents = rows
            .into_iter()
            .filter_map(|work| match work {
                SupervisedWork::Agent { run } => Some(SupervisedWork::Agent { run }),
                SupervisedWork::Terminal { terminal } => {
                    self.merge_snapshot(*terminal);
                    None
                }
            })
            .collect::<Vec<_>>();
        for terminal in terminals {
            self.merge_snapshot(terminal);
        }
        self.rows = agents;
        self.rebuild_rows();
    }

    pub(crate) fn hydrate_live(&mut self, live: Vec<cagent_agent::presentation::DelegatedRunLive>) {
        self.agent_run_live = live.into_iter().map(|state| (state.id, state)).collect();
    }

    pub(crate) fn upsert_agent(&mut self, run: cagent_agent::protocol::AgentRun) {
        if let Some(existing) = self
            .rows
            .iter_mut()
            .find(|work| matches!(work, SupervisedWork::Agent { run: item } if item.id == run.id))
        {
            *existing = SupervisedWork::Agent { run: Box::new(run) };
        } else {
            self.rows.push(SupervisedWork::Agent { run: Box::new(run) });
        }
        cagent_agent::presentation::sort_supervised_work_newest_first(&mut self.rows);
    }

    pub(crate) fn upsert_live(&mut self, live: cagent_agent::presentation::DelegatedRunLive) {
        self.agent_run_live.insert(live.id, live);
    }

    pub(crate) fn remove_live(&mut self, id: AgentRunId) {
        self.agent_run_live.remove(&id);
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    /// Inserts the latest terminal update while keeping all supervised work
    /// newest-first. Incremental updates are ordered by the runtime,
    /// so they are authoritative even when a later full snapshot is stale.
    pub(crate) fn upsert_terminal(&mut self, terminal: cagent_agent::tools::TerminalSnapshot) {
        self.terminals.insert(terminal.id, terminal);
        self.rebuild_rows();
    }

    #[must_use]
    pub(crate) fn is_delegated_terminal(
        &self,
        terminal: &cagent_agent::tools::TerminalSnapshot,
    ) -> bool {
        terminal.owner_agent_run_id.is_some()
            || self.rows.iter().any(|work| {
                let SupervisedWork::Agent { run } = work else {
                    return false;
                };
                cagent_agent::presentation::terminal_belongs_to_agent_run(terminal, run)
            })
    }

    fn merge_snapshot(&mut self, terminal: cagent_agent::tools::TerminalSnapshot) {
        let replace = self.terminals.get(&terminal.id).is_none_or(|current| {
            // A completed terminal must not regress to Running when a stale
            // snapshot arrives. Output cursors provide the ordering for
            // snapshots that are otherwise in the same lifecycle state.
            !(current.status.is_final() && terminal.status.is_active())
                && terminal.output_cursor >= current.output_cursor
        });
        if replace {
            self.terminals.insert(terminal.id, terminal);
        }
    }

    fn rebuild_rows(&mut self) {
        self.rows
            .retain(|work| matches!(work, SupervisedWork::Agent { .. }));
        self.rows
            .extend(
                self.terminals
                    .values()
                    .cloned()
                    .map(|terminal| SupervisedWork::Terminal {
                        terminal: Box::new(terminal),
                    }),
            );
        cagent_agent::presentation::sort_supervised_work_newest_first(&mut self.rows);
    }

    /// Returns all retained terminals owned by a delegated run, including
    /// terminals received before the corresponding supervised-work row was
    /// hydrated.
    #[must_use]
    pub(crate) fn terminals_for_agent(
        &self,
        run: &cagent_agent::protocol::AgentRun,
    ) -> Vec<cagent_agent::tools::TerminalSnapshot> {
        let mut terminals = self
            .terminals
            .values()
            .filter(|terminal| {
                cagent_agent::presentation::terminal_belongs_to_agent_run(terminal, run)
            })
            .cloned()
            .collect::<Vec<_>>();
        for work in &self.rows {
            let SupervisedWork::Terminal { terminal } = work else {
                continue;
            };
            if cagent_agent::presentation::terminal_belongs_to_agent_run(terminal, run)
                && !terminals.iter().any(|item| item.id == terminal.id)
            {
                terminals.push((**terminal).clone());
            }
        }
        terminals.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        terminals
    }

    #[must_use]
    pub(crate) fn rows(&self) -> Vec<SupervisedWork> {
        self.rows
            .iter()
            .filter(|work| match work {
                SupervisedWork::Agent { .. } => true,
                SupervisedWork::Terminal { terminal } => {
                    cagent_agent::presentation::terminal_visible_as_task(terminal)
                }
            })
            .cloned()
            .collect()
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn has_work(&self) -> bool {
        !self.rows().is_empty()
    }

    #[must_use]
    pub(crate) fn has_delayed_read(&self) -> bool {
        self.rows.iter().any(|work| {
            matches!(work, SupervisedWork::Terminal { terminal }
                if terminal.status.is_active()
                    && terminal.read_safe == Some(true)
                    && !cagent_agent::presentation::terminal_visible_as_task(terminal))
        })
    }

    #[must_use]
    pub(crate) fn running_terminal_count(&self) -> usize {
        self.rows
            .iter()
            .filter(|work| {
                matches!(
                    work,
                    SupervisedWork::Terminal { terminal }
                        if terminal.status.is_active()
                            && cagent_agent::presentation::terminal_visible_as_task(terminal)
                )
            })
            .count()
    }

    #[must_use]
    pub(crate) fn running_agent_count(&self) -> usize {
        self.rows
            .iter()
            .filter(|work| {
                matches!(
                    work,
                    SupervisedWork::Agent { run }
                        if matches!(
                            run.status,
                            cagent_agent::protocol::AgentRunStatus::Queued
                                | cagent_agent::protocol::AgentRunStatus::Running
                        )
                )
            })
            .count()
    }
}
