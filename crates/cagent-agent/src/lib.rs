//! UI-independent runtime for Cagent.

pub mod agent_control;
pub mod attachments;
pub mod cleanup;
pub mod config;
pub mod frontend;
pub mod images;
pub mod mcp;
pub mod permissions;
pub mod presentation;
mod prompts;
pub mod protocol;
pub mod provider;
pub mod runtime;
mod scratchpad;
mod store;
pub mod tools;
pub mod web_fetch;
pub mod web_search;
pub use runtime::WorktreeInfo;
pub use runtime::workspace::{WorkspaceEntry, WorkspaceEntryKind};
pub use web_fetch::{WebFetchError, WebFetchFormat, WebFetchRequest, WebFetchResponse};

pub use agent_control::{
    QUESTION_NONE_OF_THE_ABOVE, QuestionAnswer, QuestionOption, QuestionPrompt, QuestionRequest,
    QuestionResult,
};
pub use attachments::{
    AttachmentReference, history_user_draft, parse_attachment_references, parse_attachment_specs,
};
pub use protocol::{UsageBreakdown, UsageOverview};

#[allow(clippy::wildcard_imports)]
pub(crate) use config::*;
#[allow(clippy::wildcard_imports)]
pub(crate) use mcp::*;
#[allow(clippy::wildcard_imports)]
pub(crate) use permissions::*;
#[allow(clippy::wildcard_imports)]
pub(crate) use presentation::*;
#[allow(clippy::wildcard_imports)]
pub(crate) use protocol::*;
#[allow(clippy::wildcard_imports)]
pub(crate) use provider::*;
#[allow(clippy::wildcard_imports)]
pub(crate) use tools::*;
#[allow(clippy::wildcard_imports)]
pub(crate) use web_search::*;
