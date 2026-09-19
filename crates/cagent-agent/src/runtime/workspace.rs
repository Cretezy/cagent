//! Internal workspace services shared by attachment capture and path completion.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A path candidate exposed to frontend path completion.
///
/// Workspace queries return workspace-relative paths. Home and absolute
/// queries preserve their `~/` or absolute display spelling.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkspaceEntry {
    pub path: PathBuf,
    pub kind: WorkspaceEntryKind,
}

/// Filesystem kind for a [`WorkspaceEntry`].
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceEntryKind {
    File,
    Directory,
    Symlink,
}
