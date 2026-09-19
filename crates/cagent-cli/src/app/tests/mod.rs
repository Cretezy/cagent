//! Application-state regression tests, grouped under `tests/` by behavior.

use super::events::follow_expanded_scroll;
use super::helpers::history_tree_node_style;
use super::input::status_hint_action;
use super::list::ListHit;
use super::scroll::ScrollViewHit;
use super::surface_control::{agent_menu_row, concise_setting_error};
use super::surfaces::{
    SurfaceHit, expanded_scroll_metrics, note_input_layout, permission_surface_line_count,
    web_fetch_content_lines, web_search_result_lines,
};
use super::*;
use std::fmt::Write as _;
use unicode_width::UnicodeWidthStr;

// Keep the app-level module paths available to the behavior files. This also
// leaves each test with the same imports it had before the organizational
// split.
pub(crate) use super::{
    controller, events, helpers, input, list, scroll, surface_control, surfaces,
};

mod behavior;
