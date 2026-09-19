//! Protocol error values.

use std::path::PathBuf;

use super::session::{ConversationId, InteractionRequestId, NodeId, QueuedMessageId};

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RuntimeError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("runtime task stopped")]
    RuntimeStopped,
    #[error("conversation {0} does not exist")]
    ConversationNotFound(ConversationId),
    #[error("conversation {0} is open read-only because another process owns its writer lock")]
    ReadOnlyObserver(ConversationId),
    #[error("queued message {0} does not exist in conversation {1}")]
    QueuedMessageNotFound(QueuedMessageId, ConversationId),
    #[error("queued message {0} changed before it could be dispatched")]
    QueuedMessageChanged(QueuedMessageId),
    #[error("node {0} does not exist in conversation {1} or is not in the required state")]
    NodeNotFoundOrInvalidState(NodeId, ConversationId),
    #[error("node {0} is not the active parent of conversation {1}")]
    StaleActiveParent(NodeId, ConversationId),
    #[error("transcript cursor no longer belongs to the active conversation branch")]
    StaleTranscriptCursor,
    #[error("queued input must not be empty")]
    EmptyQueuedInput,
    #[error("unsupported session command in this implementation phase")]
    UnsupportedCommand,
    #[error("interaction {0} must be resolved before the operation can continue")]
    InteractionPending(InteractionRequestId),
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    #[error("MCP login required")]
    McpAuthenticationRequired,
    #[error("invalid runtime option: {0}")]
    InvalidOption(String),
    #[error("unsupported protocol version {actual}; this runtime supports {supported}")]
    UnsupportedVersion { actual: u16, supported: u16 },
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("stored event could not be decoded: {0}")]
    EventDecode(#[from] serde_json::Error),
    #[error("platform configuration or data directories are unavailable")]
    PlatformDirectoriesUnavailable,
    #[error("configuration error in {path}: {message}")]
    Config { path: PathBuf, message: String },
    #[error("instruction error in {path}: {message}")]
    Instructions { path: PathBuf, message: String },
    #[error("permissions error in {path}: {message}")]
    Permissions { path: PathBuf, message: String },
}
