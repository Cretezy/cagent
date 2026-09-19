//! Shared scrolling for visual-row content.
//!
//! `ListState` owns selection and therefore knows about logical items.  A
//! scroll view is deliberately different: its rows are already prepared for
//! display and the state only tracks a top-origin offset into those rows.

use ratatui::text::Line;

use super::{DIM_STYLE, list::ListAction};

pub(crate) fn move_scroll_offset(offset: &mut usize, maximum: usize, amount: isize) {
    *offset = if amount.is_negative() {
        offset.saturating_sub(amount.unsigned_abs())
    } else {
        offset.saturating_add(amount.unsigned_abs()).min(maximum)
    };
}

pub(crate) fn scroll_window_metrics(content_rows: usize, capacity: usize) -> (usize, usize) {
    let metrics = ScrollViewMetrics::new(content_rows, capacity);
    (
        metrics.content_rows.min(metrics.capacity),
        metrics.maximum_offset,
    )
}

/// Keeps a focused row visible with one row of context on either side when
/// the content and viewport have room for it. Lists and editable scroll views
/// share this rule so moving a selection or caret produces the same window.
pub(crate) fn reconcile_focused_offset(
    offset: usize,
    focused: usize,
    content_rows: usize,
    capacity: usize,
) -> usize {
    let capacity = capacity.max(1).min(content_rows.max(1));
    let maximum = content_rows.saturating_sub(capacity);
    let mut offset = offset.min(maximum);
    let focused = focused.min(content_rows.saturating_sub(1));

    if focused < offset {
        offset = focused.saturating_sub(1);
    } else if focused >= offset.saturating_add(capacity) {
        offset = focused
            .saturating_add(2)
            .saturating_sub(capacity)
            .min(maximum);
    } else if focused == offset && offset > 0 {
        offset = focused.saturating_sub(1);
    } else if focused.saturating_add(1) == offset.saturating_add(capacity)
        && focused.saturating_add(1) < content_rows
    {
        offset = focused
            .saturating_add(2)
            .saturating_sub(capacity)
            .min(maximum);
    }
    offset
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScrollViewAction {
    Previous,
    Next,
    PagePrevious,
    PageNext,
    WheelPrevious,
    WheelNext,
    Home,
    End,
}

/// Geometry for a visual-row scroll region. Rendering and input both consume
/// this value, so indicator placement and movement share one definition of a
/// viewport and its last valid offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScrollViewMetrics {
    pub(crate) content_rows: usize,
    pub(crate) capacity: usize,
    pub(crate) maximum_offset: usize,
}

impl ScrollViewMetrics {
    pub(crate) const fn new(content_rows: usize, capacity: usize) -> Self {
        Self {
            content_rows,
            capacity,
            maximum_offset: content_rows.saturating_sub(capacity),
        }
    }

    pub(crate) fn visible_range(self, offset: usize) -> std::ops::Range<usize> {
        let start = offset.min(self.maximum_offset);
        let end = start.saturating_add(self.capacity).min(self.content_rows);
        start..end
    }
}

impl From<ListAction> for ScrollViewAction {
    fn from(action: ListAction) -> Self {
        match action {
            ListAction::Previous => Self::Previous,
            ListAction::Next => Self::Next,
            ListAction::PagePrevious => Self::PagePrevious,
            ListAction::PageNext => Self::PageNext,
            ListAction::WheelPrevious => Self::WheelPrevious,
            ListAction::WheelNext => Self::WheelNext,
            ListAction::Home => Self::Home,
            ListAction::End => Self::End,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ScrollViewState {
    pub(crate) offset: usize,
    pub(crate) content_rows: usize,
}

impl ScrollViewState {
    pub(crate) const fn new(content_rows: usize) -> Self {
        Self {
            offset: 0,
            content_rows,
        }
    }

    pub(crate) fn reset(&mut self, content_rows: usize) {
        *self = Self::new(content_rows);
    }

    /// Reconciles the offset after content or the viewport changes.  The
    /// optional `follow_end` flag is evaluated against the old state, so a
    /// growing live view follows only when it was already at its end.
    pub(crate) fn reconcile(
        &mut self,
        content_rows: usize,
        capacity: usize,
        follow_end: bool,
    ) -> bool {
        self.reconcile_with_previous_capacity(content_rows, capacity, capacity, follow_end)
    }

    pub(crate) fn reconcile_focus(
        &mut self,
        content_rows: usize,
        focused: usize,
        capacity: usize,
    ) -> bool {
        self.content_rows = content_rows;
        let next = reconcile_focused_offset(self.offset, focused, content_rows, capacity);
        let changed = self.offset != next;
        self.offset = next;
        changed
    }

    /// Reconciles after a reflow whose old and new capacities differ. Live
    /// panels pass their previously rendered capacity so tail following is
    /// determined before the resize, not against the newly enlarged window.
    pub(crate) fn reconcile_with_previous_capacity(
        &mut self,
        content_rows: usize,
        previous_capacity: usize,
        capacity: usize,
        follow_end: bool,
    ) -> bool {
        let was_at_end = self.at_end(previous_capacity);
        self.content_rows = content_rows;
        let metrics = ScrollViewMetrics::new(content_rows, capacity);
        let next = if follow_end && was_at_end {
            metrics.maximum_offset
        } else {
            self.offset.min(metrics.maximum_offset)
        };
        let changed = self.offset != next;
        self.offset = next;
        changed
    }

    pub(crate) fn maximum_offset(self, capacity: usize) -> usize {
        ScrollViewMetrics::new(self.content_rows, capacity).maximum_offset
    }

    pub(crate) fn at_end(self, capacity: usize) -> bool {
        self.offset >= self.maximum_offset(capacity)
    }

    pub(crate) fn has_above(self) -> bool {
        self.offset > 0
    }

    pub(crate) fn has_below(self, capacity: usize) -> bool {
        self.offset < self.maximum_offset(capacity)
    }

    pub(crate) fn apply(&mut self, action: ScrollViewAction, capacity: usize) -> bool {
        let maximum = ScrollViewMetrics::new(self.content_rows, capacity).maximum_offset;
        let next = match action {
            ScrollViewAction::Previous => self.offset.saturating_sub(1),
            ScrollViewAction::Next => self.offset.saturating_add(1).min(maximum),
            ScrollViewAction::PagePrevious => self.offset.saturating_sub(capacity.max(1)),
            ScrollViewAction::PageNext => self.offset.saturating_add(capacity.max(1)).min(maximum),
            ScrollViewAction::WheelPrevious => self.offset.saturating_sub(3),
            ScrollViewAction::WheelNext => self.offset.saturating_add(3).min(maximum),
            ScrollViewAction::Home => 0,
            ScrollViewAction::End => maximum,
        };
        let changed = self.offset != next;
        self.offset = next;
        changed
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScrollViewHit {
    Indicator(ScrollViewAction),
    Content(usize),
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScrollViewLayout {
    pub(crate) lines: Vec<Line<'static>>,
    pub(crate) hits: Vec<ScrollViewHit>,
    pub(crate) metrics: ScrollViewMetrics,
}

impl ScrollViewLayout {
    #[cfg(test)]
    pub(crate) fn hit(&self, row: usize) -> ScrollViewHit {
        self.hits.get(row).copied().unwrap_or(ScrollViewHit::None)
    }
}

/// Renders a bounded range of already-prepared visual rows. Indicator slots
/// are always present, even when their actions are currently inactive; this
/// keeps keyboard, mouse, and resize geometry stable.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ScrollViewWidget {
    content_capacity: usize,
    top_action: ScrollViewAction,
    bottom_action: ScrollViewAction,
}

impl ScrollViewWidget {
    pub(crate) fn new(content_capacity: usize) -> Self {
        Self {
            content_capacity,
            top_action: ScrollViewAction::PagePrevious,
            bottom_action: ScrollViewAction::PageNext,
        }
    }

    pub(crate) fn indicators(mut self, top: ScrollViewAction, bottom: ScrollViewAction) -> Self {
        self.top_action = top;
        self.bottom_action = bottom;
        self
    }

    pub(crate) fn render<F>(&self, state: &ScrollViewState, mut render_row: F) -> ScrollViewLayout
    where
        F: FnMut(usize) -> Line<'static>,
    {
        let metrics = ScrollViewMetrics::new(state.content_rows, self.content_capacity);
        let layout_capacity = metrics.content_rows.min(metrics.capacity).saturating_add(2);
        let mut lines = Vec::with_capacity(layout_capacity);
        let mut hits = Vec::with_capacity(layout_capacity);
        let top_active = state.has_above();
        lines.push(indicator_line(true, top_active));
        hits.push(if top_active {
            ScrollViewHit::Indicator(self.top_action)
        } else {
            ScrollViewHit::None
        });
        for row in metrics.visible_range(state.offset) {
            lines.push(render_row(row));
            hits.push(ScrollViewHit::Content(row));
        }
        let bottom_active = state.has_below(self.content_capacity);
        lines.push(indicator_line(false, bottom_active));
        hits.push(if bottom_active {
            ScrollViewHit::Indicator(self.bottom_action)
        } else {
            ScrollViewHit::None
        });
        debug_assert_eq!(lines.len(), hits.len());
        ScrollViewLayout {
            lines,
            hits,
            metrics,
        }
    }
}

fn indicator_line(up: bool, active: bool) -> Line<'static> {
    if active {
        Line::from(if up { "  ↑" } else { "  ↓" }).style(DIM_STYLE)
    } else {
        Line::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn movement_is_top_origin_and_clamped() {
        let mut state = ScrollViewState::new(30);
        assert!(state.apply(ScrollViewAction::PageNext, 12));
        assert_eq!(state.offset, 12);
        assert!(state.apply(ScrollViewAction::WheelNext, 12));
        assert_eq!(state.offset, 15);
        assert!(state.apply(ScrollViewAction::End, 12));
        assert_eq!(state.offset, 18);
        assert!(state.apply(ScrollViewAction::Home, 12));
        assert_eq!(state.offset, 0);
    }

    #[test]
    fn reconcile_clamps_and_follows_only_from_the_end() {
        let mut state = ScrollViewState::new(30);
        state.apply(ScrollViewAction::End, 8);
        assert_eq!(state.offset, 22);
        state.reconcile(40, 8, true);
        assert_eq!(state.offset, 32);
        state.apply(ScrollViewAction::Previous, 8);
        state.reconcile(50, 8, true);
        assert_eq!(state.offset, 31);
        state.reconcile(4, 8, false);
        assert_eq!(state.offset, 0);
    }

    #[test]
    fn reflow_follows_using_the_previous_capacity() {
        let mut state = ScrollViewState::new(30);
        state.apply(ScrollViewAction::End, 10);
        state.reconcile_with_previous_capacity(30, 10, 5, true);
        assert_eq!(state.offset, 25);

        state.apply(ScrollViewAction::Previous, 5);
        state.reconcile_with_previous_capacity(40, 5, 10, true);
        assert_eq!(state.offset, 24);
    }

    #[test]
    fn widget_has_permanent_indicators_and_lazy_content_hits() {
        let mut state = ScrollViewState::new(20);
        state.offset = 4;
        let layout =
            ScrollViewWidget::new(3).render(&state, |row| Line::from(format!("row {row}")));
        assert_eq!(layout.lines.len(), 5);
        assert_eq!(
            layout.hit(0),
            ScrollViewHit::Indicator(ScrollViewAction::PagePrevious)
        );
        assert_eq!(layout.hit(1), ScrollViewHit::Content(4));
        assert_eq!(layout.hit(3), ScrollViewHit::Content(6));
        assert_eq!(
            layout.hit(4),
            ScrollViewHit::Indicator(ScrollViewAction::PageNext)
        );
    }

    #[test]
    fn empty_and_short_views_keep_the_indicator_slots() {
        let state = ScrollViewState::new(0);
        let layout = ScrollViewWidget::new(12).render(&state, |_| unreachable!());
        assert_eq!(layout.lines.len(), 2);
        assert!(layout.hits.iter().all(|hit| *hit == ScrollViewHit::None));
    }

    #[test]
    fn metrics_clamp_the_visible_range_after_reflow() {
        let metrics = ScrollViewMetrics::new(10, 4);
        assert_eq!(metrics.maximum_offset, 6);
        assert_eq!(metrics.visible_range(3), 3..7);
        assert_eq!(metrics.visible_range(usize::MAX), 6..10);
    }

    #[test]
    fn focused_rows_keep_one_row_of_context() {
        let mut state = ScrollViewState::new(20);
        state.reconcile_focus(20, 0, 5);
        assert_eq!(state.offset, 0);
        state.reconcile_focus(20, 3, 5);
        assert_eq!(state.offset, 0);
        state.reconcile_focus(20, 4, 5);
        assert_eq!(state.offset, 1);
        state.reconcile_focus(20, 5, 5);
        assert_eq!(state.offset, 2);
        state.reconcile_focus(20, 4, 5);
        assert_eq!(state.offset, 2);
        state.reconcile_focus(20, 2, 5);
        assert_eq!(state.offset, 1);
    }
}
