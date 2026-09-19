//! Mouse input boundaries.
//!
//! Mouse routing and scrolling decisions live here; the `App` methods retain
//! their public API and perform the state mutation.

use super::super::scroll::ScrollViewAction;
use crossterm::event::MouseEventKind;

pub(super) fn is_wheel(kind: MouseEventKind) -> bool {
    matches!(kind, MouseEventKind::ScrollUp | MouseEventKind::ScrollDown)
}

pub(super) fn wheel_action(kind: MouseEventKind) -> Option<ScrollViewAction> {
    match kind {
        MouseEventKind::ScrollUp => Some(ScrollViewAction::PagePrevious),
        MouseEventKind::ScrollDown => Some(ScrollViewAction::PageNext),
        _ => None,
    }
}
