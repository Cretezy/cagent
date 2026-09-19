use super::events::follow_expanded_scroll;
use super::*;
use cagent_agent::protocol::{AgentRunId, AgentRunLogPage, RuntimeError};

/// Detail reads are independent of ordinary session snapshots. Only the open
/// log is hydrated, one bounded page at a time; later updates fetch appends.
pub(crate) struct AgentLogLoad {
    id: AgentRunId,
    after: u64,
    loading: bool,
    dirty: bool,
}

impl App {
    pub(super) fn invalidate_open_agent_log(&mut self, id: Option<AgentRunId>) {
        if let Some(load) = &mut self.agent_log_load
            && id.is_none_or(|id| id == load.id)
        {
            load.dirty = true;
        }
    }

    pub(super) fn take_agent_log_request(&mut self) -> Option<(AgentRunId, u64)> {
        let run = self
            .surfaces
            .iter()
            .rev()
            .find_map(|surface| match surface {
                Surface::Expanded {
                    view: ExpandedView::AgentLog { run, .. },
                    ..
                } => Some(run),
                _ => None,
            });
        let Some(run) = run else {
            self.agent_log_load = None;
            return None;
        };
        if self
            .agent_log_load
            .as_ref()
            .is_none_or(|load| load.id != run.id)
        {
            self.agent_log_load = Some(AgentLogLoad {
                id: run.id,
                after: 0,
                loading: false,
                dirty: true,
            });
        }
        let load = self.agent_log_load.as_mut()?;
        if load.loading || !load.dirty {
            return None;
        }
        load.loading = true;
        load.dirty = false;
        Some((load.id, load.after))
    }

    pub(super) fn apply_agent_log_page(
        &mut self,
        id: AgentRunId,
        after: u64,
        result: Result<AgentRunLogPage, RuntimeError>,
    ) {
        let Some(load) = &mut self.agent_log_load else {
            return;
        };
        if load.id != id || load.after != after || !load.loading {
            return;
        }
        load.loading = false;
        let page = match result {
            Ok(page) => page,
            Err(error) => {
                self.set_notice(format!("could not load delegated log · {error}"));
                return;
            }
        };
        load.after = page.next_after;
        load.dirty |= page.has_more;
        if page.entries.is_empty() {
            return;
        }
        let (_, _, panel_rows, _) =
            self.control_heights_within(self.render_width, self.render_height);
        for surface in &mut self.surfaces {
            let Surface::Expanded {
                view,
                scroll,
                viewport_rows,
            } = surface
            else {
                continue;
            };
            let ExpandedView::AgentLog {
                run, max_scroll, ..
            } = view
            else {
                continue;
            };
            if run.id != id {
                continue;
            }
            let previous = max_scroll.get().unwrap_or(0);
            // The first page replaces any compatibility detail supplied by a
            // caller. Subsequent pages only append; no old payload is cloned.
            if after == 0 {
                run.timeline.clear();
                run.activity.clear();
            }
            run.timeline.extend(page.entries);
            self.expanded_text_render_cache.get_mut().take();
            let next = surfaces::cached_expanded_scroll_metrics(
                &self.expanded_text_render_cache,
                view,
                *viewport_rows,
                &self.workspace,
                self.render_width,
                usize::from(panel_rows),
            )
            .1;
            *scroll = follow_expanded_scroll(*scroll, previous, next);
            break;
        }
    }
}
