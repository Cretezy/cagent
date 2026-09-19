//! Surface rendering and interaction entry points.
//!
//! The implementation lives in sibling modules so this module remains the
//! stable boundary used by the rest of the application.

#[cfg(test)]
pub(crate) use super::surface_expanded::surface_settings::setting_input_description;
pub(crate) use super::surface_expanded::*;
