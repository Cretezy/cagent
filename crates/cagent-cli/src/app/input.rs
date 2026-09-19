//! Input handling split into event and activation boundaries.
//!
//! `core` contains the existing `App` method implementations to preserve
//! their APIs while the focused submodules provide shared routing decisions.
mod content_activation;
mod core;
mod keyboard;
mod mouse;

#[cfg(test)]
pub(crate) use content_activation::status_hint_action;

use super::{ComposerMode, TranscriptScrollbarDrag, file_tree, scroll, surfaces};
