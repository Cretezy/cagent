//! Application-state regression tests, grouped by user-visible behavior.

use super::*;
use background_and_tools::mcp_test_definition;
use interaction_surfaces::{
    edit_diff, permission_request, question_request, session_snapshot_with_interaction,
};

#[path = "background_and_tools.rs"]
mod background_and_tools;
#[path = "expanded_views.rs"]
mod expanded_views;
#[path = "interaction_surfaces.rs"]
mod interaction_surfaces;
#[path = "observer_and_history.rs"]
mod observer_and_history;
#[path = "permissions_and_commands.rs"]
mod permissions_and_commands;
#[path = "transcript_and_navigation.rs"]
mod transcript_and_navigation;
