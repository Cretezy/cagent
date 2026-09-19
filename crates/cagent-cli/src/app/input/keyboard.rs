//! Keyboard and composer input boundaries.
//!
//! This module owns event-shape decisions shared by keyboard routing and the
//! composer. Stateful editing continues to use `App` methods.

use crossterm::event::{KeyCode, KeyEvent};

pub(super) fn is_page_navigation(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::PageUp | KeyCode::PageDown)
}

pub(super) fn page_navigation_action(
    key: KeyEvent,
) -> Option<super::super::scroll::ScrollViewAction> {
    match key.code {
        KeyCode::PageUp => Some(super::super::scroll::ScrollViewAction::PagePrevious),
        KeyCode::PageDown => Some(super::super::scroll::ScrollViewAction::PageNext),
        _ => None,
    }
}
