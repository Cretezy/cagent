use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

use super::{DIM_STYLE, scroll::reconcile_focused_offset};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ListMode {
    Selectable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ListAction {
    Previous,
    Next,
    PagePrevious,
    PageNext,
    WheelPrevious,
    WheelNext,
    Home,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ListState {
    pub(crate) selected: Option<usize>,
    pub(crate) offset: usize,
    pub(crate) item_count: usize,
}

impl ListState {
    pub(crate) fn selectable(item_count: usize) -> Self {
        Self {
            selected: (item_count > 0).then_some(0),
            offset: 0,
            item_count,
        }
    }

    pub(crate) fn selectable_at(item_count: usize, selected: usize, capacity: usize) -> Self {
        let mut state = Self::selectable(item_count);
        state.select(selected, capacity);
        state
    }

    pub(crate) fn reset(&mut self, mode: ListMode, item_count: usize) {
        *self = match mode {
            ListMode::Selectable => Self::selectable(item_count),
        };
    }

    pub(crate) fn reconcile(&mut self, mode: ListMode, item_count: usize, capacity: usize) {
        self.item_count = item_count;
        match mode {
            ListMode::Selectable => {
                self.selected = if item_count == 0 {
                    None
                } else {
                    Some(self.selected.unwrap_or(0).min(item_count - 1))
                };
                self.reconcile_selectable_window(capacity);
            }
        }
    }

    pub(crate) fn select(&mut self, index: usize, capacity: usize) -> bool {
        if self.item_count == 0 {
            return false;
        }
        let next = index.min(self.item_count - 1);
        let changed = self.selected != Some(next);
        self.selected = Some(next);
        self.reconcile_selectable_window(capacity);
        changed
    }

    pub(crate) fn step_up(&mut self, mode: ListMode, capacity: usize) -> bool {
        self.move_by(mode, -1, capacity)
    }

    pub(crate) fn step_down(&mut self, mode: ListMode, capacity: usize) -> bool {
        self.move_by(mode, 1, capacity)
    }

    pub(crate) fn page_up(&mut self, mode: ListMode, capacity: usize) -> bool {
        self.move_by(mode, -(capacity.max(1) as isize), capacity)
    }

    pub(crate) fn page_down(&mut self, mode: ListMode, capacity: usize) -> bool {
        self.move_by(mode, capacity.max(1) as isize, capacity)
    }

    pub(crate) fn wheel_up(&mut self, mode: ListMode, capacity: usize) -> bool {
        self.move_by(mode, -3, capacity)
    }

    pub(crate) fn wheel_down(&mut self, mode: ListMode, capacity: usize) -> bool {
        self.move_by(mode, 3, capacity)
    }

    pub(crate) fn home(&mut self, mode: ListMode, capacity: usize) -> bool {
        match mode {
            ListMode::Selectable => self.select(0, capacity),
        }
    }

    pub(crate) fn end(&mut self, mode: ListMode, capacity: usize) -> bool {
        match mode {
            ListMode::Selectable => self.select(self.item_count.saturating_sub(1), capacity),
        }
    }

    pub(crate) fn apply(&mut self, action: ListAction, mode: ListMode, capacity: usize) -> bool {
        match action {
            ListAction::Previous => self.step_up(mode, capacity),
            ListAction::Next => self.step_down(mode, capacity),
            ListAction::PagePrevious => self.page_up(mode, capacity),
            ListAction::PageNext => self.page_down(mode, capacity),
            ListAction::WheelPrevious => self.wheel_up(mode, capacity),
            ListAction::WheelNext => self.wheel_down(mode, capacity),
            ListAction::Home => self.home(mode, capacity),
            ListAction::End => self.end(mode, capacity),
        }
    }

    pub(crate) fn has_above(self) -> bool {
        self.offset > 0
    }

    pub(crate) fn has_below(self, capacity: usize) -> bool {
        self.offset.saturating_add(capacity.max(1)) < self.item_count
    }

    pub(crate) fn visible_range(self, capacity: usize) -> std::ops::Range<usize> {
        let end = self
            .offset
            .saturating_add(capacity.max(1))
            .min(self.item_count);
        self.offset..end
    }

    fn move_by(&mut self, mode: ListMode, amount: isize, capacity: usize) -> bool {
        match mode {
            ListMode::Selectable => self.move_selectable_by(amount, capacity),
        }
    }

    fn move_selectable_by(&mut self, amount: isize, capacity: usize) -> bool {
        let Some(selected) = self.selected else {
            return false;
        };
        let next = if amount.is_negative() {
            selected.saturating_sub(amount.unsigned_abs())
        } else {
            selected
                .saturating_add(amount as usize)
                .min(self.item_count.saturating_sub(1))
        };
        self.select(next, capacity)
    }

    fn reconcile_selectable_window(&mut self, capacity: usize) {
        let Some(selected) = self.selected else {
            self.offset = 0;
            return;
        };
        self.offset = reconcile_focused_offset(self.offset, selected, self.item_count, capacity);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ListHit {
    Up,
    Item(usize),
    Down,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum LinePolicy {
    Preserve,
    Wrap,
    Truncate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ListLayout {
    pub(crate) lines: Vec<Line<'static>>,
    pub(crate) hits: Vec<ListHit>,
}

impl ListLayout {
    pub(crate) fn hit(&self, row: usize) -> ListHit {
        self.hits.get(row).copied().unwrap_or(ListHit::None)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ListWidget {
    width: u16,
    item_capacity: usize,
    top_indicator: bool,
    bottom_indicator: bool,
    empty_lines: Vec<Line<'static>>,
    fill_capacity: bool,
    leading_spacing: usize,
    trailing_spacing: usize,
    policy: LinePolicy,
}

impl ListWidget {
    pub(crate) fn new(width: u16, item_capacity: usize) -> Self {
        Self {
            width,
            item_capacity: item_capacity.max(1),
            top_indicator: true,
            bottom_indicator: true,
            empty_lines: Vec::new(),
            fill_capacity: false,
            leading_spacing: 0,
            trailing_spacing: 0,
            policy: LinePolicy::Preserve,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn indicators(mut self, top: bool, bottom: bool) -> Self {
        self.top_indicator = top;
        self.bottom_indicator = bottom;
        self
    }

    pub(crate) fn empty_lines(mut self, lines: Vec<Line<'static>>) -> Self {
        self.empty_lines = lines;
        self
    }

    pub(crate) fn fill_capacity(mut self) -> Self {
        self.fill_capacity = true;
        self
    }

    #[allow(dead_code)]
    pub(crate) fn spacing(mut self, leading: usize, trailing: usize) -> Self {
        self.leading_spacing = leading;
        self.trailing_spacing = trailing;
        self
    }

    pub(crate) fn policy(mut self, policy: LinePolicy) -> Self {
        self.policy = policy;
        self
    }

    pub(crate) fn render<F>(&self, state: &ListState, mut render_item: F) -> ListLayout
    where
        F: FnMut(usize, u16, bool) -> Vec<Line<'static>>,
    {
        let mut lines = Vec::new();
        let mut hits = Vec::new();
        push_blank_rows(&mut lines, &mut hits, self.leading_spacing);

        if self.top_indicator {
            let active = state.has_above();
            lines.push(indicator_line(true, active));
            hits.push(if active { ListHit::Up } else { ListHit::None });
        }
        if state.item_count == 0 {
            for line in &self.empty_lines {
                let rendered = apply_policy(line.clone(), self.width, self.policy);
                for line in rendered {
                    lines.push(line);
                    hits.push(ListHit::None);
                }
            }
        } else {
            for index in state.visible_range(self.item_capacity) {
                let selected = state.selected == Some(index);
                let rendered = render_item(index, self.width, selected);
                for source in rendered {
                    for line in apply_policy(source, self.width, self.policy) {
                        lines.push(line);
                        hits.push(ListHit::Item(index));
                    }
                }
            }
        }
        if self.fill_capacity {
            let occupied = if state.item_count == 0 {
                self.empty_lines.len()
            } else {
                state.visible_range(self.item_capacity).len()
            };
            push_blank_rows(
                &mut lines,
                &mut hits,
                self.item_capacity.saturating_sub(occupied),
            );
        }
        if self.bottom_indicator {
            let active = state.has_below(self.item_capacity);
            lines.push(indicator_line(false, active));
            hits.push(if active { ListHit::Down } else { ListHit::None });
        }
        push_blank_rows(&mut lines, &mut hits, self.trailing_spacing);
        debug_assert_eq!(lines.len(), hits.len());
        ListLayout { lines, hits }
    }

    /// Builds the row hit map without rendering item content. This is useful
    /// when a surrounding surface owns the title and other non-list rows but
    /// still needs the list's exact logical row spans.
    pub(crate) fn hits<F>(&self, state: &ListState, mut item_rows: F) -> Vec<ListHit>
    where
        F: FnMut(usize) -> usize,
    {
        let mut hits = vec![ListHit::None; self.leading_spacing];
        if self.top_indicator {
            hits.push(if state.has_above() {
                ListHit::Up
            } else {
                ListHit::None
            });
        }
        if state.item_count == 0 {
            hits.extend(self.empty_lines.iter().flat_map(|line| {
                std::iter::repeat_n(
                    ListHit::None,
                    apply_policy(line.clone(), self.width, self.policy).len(),
                )
            }));
        } else {
            for index in state.visible_range(self.item_capacity) {
                hits.extend(std::iter::repeat_n(ListHit::Item(index), item_rows(index)));
            }
        }
        if self.fill_capacity {
            let occupied = if state.item_count == 0 {
                self.empty_lines.len()
            } else {
                state.visible_range(self.item_capacity).len()
            };
            hits.extend(std::iter::repeat_n(
                ListHit::None,
                self.item_capacity.saturating_sub(occupied),
            ));
        }
        if self.bottom_indicator {
            hits.push(if state.has_below(self.item_capacity) {
                ListHit::Down
            } else {
                ListHit::None
            });
        }
        hits.extend(std::iter::repeat_n(ListHit::None, self.trailing_spacing));
        hits
    }
}

fn push_blank_rows(lines: &mut Vec<Line<'static>>, hits: &mut Vec<ListHit>, count: usize) {
    for _ in 0..count {
        lines.push(Line::default());
        hits.push(ListHit::None);
    }
}

fn indicator_line(up: bool, active: bool) -> Line<'static> {
    if active {
        Line::from(if up { "  ↑" } else { "  ↓" }).style(DIM_STYLE)
    } else {
        Line::default()
    }
}

fn apply_policy(line: Line<'static>, width: u16, policy: LinePolicy) -> Vec<Line<'static>> {
    match policy {
        LinePolicy::Preserve => vec![line],
        LinePolicy::Truncate => vec![truncate_line(line, usize::from(width))],
        LinePolicy::Wrap => wrap_line(line, usize::from(width)),
    }
}

pub(crate) fn truncate_line(line: Line<'static>, width: usize) -> Line<'static> {
    if line.width() <= width {
        return line;
    }
    if width == 0 {
        return Line::default();
    }
    let target = width.saturating_sub(1);
    let mut spans = Vec::new();
    let mut used: usize = 0;
    let mut last_style = Style::default();
    'outer: for span in line.spans {
        last_style = span.style;
        let mut value = String::new();
        for character in span.content.chars() {
            let character_width = character.width().unwrap_or(0);
            if used.saturating_add(character_width) > target {
                if !value.is_empty() {
                    spans.push(Span::styled(value, span.style));
                }
                break 'outer;
            }
            value.push(character);
            used += character_width;
        }
        if !value.is_empty() {
            spans.push(Span::styled(value, span.style));
        }
    }
    spans.push(Span::styled("…", last_style));
    Line::from(spans)
}

fn wrap_line(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    if width == 0 {
        return vec![Line::default()];
    }
    let mut rows = vec![Vec::new()];
    let mut used: usize = 0;
    for span in line.spans {
        let mut value = String::new();
        for character in span.content.chars() {
            let character_width = character.width().unwrap_or(0);
            if used > 0 && used.saturating_add(character_width) > width {
                if !value.is_empty() {
                    rows.last_mut()
                        .expect("wrap always has a row")
                        .push(Span::styled(std::mem::take(&mut value), span.style));
                }
                rows.push(Vec::new());
                used = 0;
            }
            value.push(character);
            used += character_width;
        }
        if !value.is_empty() {
            rows.last_mut()
                .expect("wrap always has a row")
                .push(Span::styled(value, span.style));
        }
    }
    rows.into_iter().map(Line::from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selectable_state_preserves_middle_window_and_padding() {
        let mut state = ListState {
            selected: Some(5),
            offset: 2,
            item_count: 20,
        };
        assert!(state.step_down(ListMode::Selectable, 8));
        assert_eq!((state.selected, state.offset), (Some(6), 2));
        assert!(state.step_down(ListMode::Selectable, 8));
        assert_eq!((state.selected, state.offset), (Some(7), 2));
        assert!(state.step_down(ListMode::Selectable, 8));
        assert_eq!((state.selected, state.offset), (Some(8), 2));
        assert!(state.step_down(ListMode::Selectable, 8));
        assert_eq!((state.selected, state.offset), (Some(9), 3));
        assert!(state.step_up(ListMode::Selectable, 8));
        assert_eq!((state.selected, state.offset), (Some(8), 3));
    }

    #[test]
    fn state_handles_pages_wheels_edges_empty_and_repopulation() {
        let mut state = ListState::selectable(20);
        state.page_down(ListMode::Selectable, 8);
        assert_eq!(state.selected, Some(8));
        state.wheel_down(ListMode::Selectable, 8);
        assert_eq!(state.selected, Some(11));
        state.end(ListMode::Selectable, 8);
        assert_eq!((state.selected, state.offset), (Some(19), 12));
        state.page_down(ListMode::Selectable, 8);
        assert_eq!(state.selected, Some(19));
        state.home(ListMode::Selectable, 8);
        assert_eq!((state.selected, state.offset), (Some(0), 0));
        state.reconcile(ListMode::Selectable, 0, 8);
        assert_eq!(state, ListState::selectable(0));
        state.reconcile(ListMode::Selectable, 3, 8);
        assert_eq!(state.selected, Some(0));
    }

    #[test]
    fn widget_has_permanent_indicators_and_row_hits() {
        let state = ListState::selectable(10);
        let layout = ListWidget::new(20, 2).render(&state, |index, _, selected| {
            vec![Line::from(format!(
                "{} {index}",
                if selected { '›' } else { ' ' }
            ))]
        });
        assert_eq!(layout.lines.len(), 4);
        assert_eq!(layout.hit(0), ListHit::None);
        assert_eq!(layout.hit(1), ListHit::Item(0));
        assert_eq!(layout.hit(2), ListHit::Item(1));
        assert_eq!(layout.hit(3), ListHit::Down);
    }

    #[test]
    fn widget_maps_every_visual_item_row_and_empty_spacing() {
        let state = ListState::selectable(1);
        let layout = ListWidget::new(8, 8)
            .spacing(1, 1)
            .policy(LinePolicy::Truncate)
            .render(&state, |_, _, _| {
                vec![
                    Line::from("first row"),
                    Line::from("a very long second row"),
                ]
            });
        assert_eq!(layout.lines.len(), 6);
        assert_eq!(layout.hit(2), ListHit::Item(0));
        assert_eq!(layout.hit(3), ListHit::Item(0));
        assert!(layout.lines[3].width() <= 8);

        let empty = ListWidget::new(20, 8)
            .empty_lines(vec![Line::from("  Nothing here")])
            .render(&ListState::selectable(0), |_, _, _| Vec::new());
        assert_eq!(empty.lines.len(), 3);
        assert_eq!(empty.hits, vec![ListHit::None; 3]);
    }

    #[test]
    fn widget_wraps_styled_lines() {
        let layout = ListWidget::new(4, 1)
            .indicators(false, false)
            .policy(LinePolicy::Wrap)
            .render(&ListState::selectable(1), |_, _, _| {
                vec![Line::from(vec![Span::styled("abcdef", Style::new())])]
            });
        assert_eq!(layout.lines.len(), 2);
        assert_eq!(layout.hits, vec![ListHit::Item(0); 2]);
    }

    #[test]
    fn hit_only_layout_matches_rendered_row_spans() {
        let state = ListState {
            selected: Some(3),
            offset: 2,
            item_count: 7,
        };
        let widget = ListWidget::new(20, 3).spacing(1, 1);
        let rendered = widget.render(&state, |index, _, _| {
            vec![Line::from(format!("item {index}")); 1 + index % 2]
        });
        assert_eq!(widget.hits(&state, |index| 1 + index % 2), rendered.hits);
    }

    #[test]
    fn selectable_surfaces_share_identical_navigation_transitions() {
        let mut states = [ListState::selectable(20); 5];
        for state in &mut states {
            state.page_down(ListMode::Selectable, 8);
            state.step_down(ListMode::Selectable, 8);
            state.wheel_down(ListMode::Selectable, 8);
            state.step_up(ListMode::Selectable, 8);
        }
        assert!(states.windows(2).all(|pair| pair[0] == pair[1]));
        assert_eq!(
            states[0],
            ListState {
                selected: Some(11),
                offset: 6,
                item_count: 20
            }
        );
    }
}
