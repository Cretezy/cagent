#![allow(clippy::type_complexity)] // Store callbacks encode their complete typed request/reply channels.
//! `SQLite` persistence and the runtime's serialized store task.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::de::DeserializeOwned;
use serde_json::json;
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::{
    API_VERSION, AttachmentSpec, AttemptId, BlobId, CapturedAttachment, CompactionSummary,
    ComposerHistoryEntry, ConversationId, DEFAULT_AGENT_NAME, DurableEvent, DurableEventKind,
    EventCursor, MessageRole, ModelInput, ModelRef, ModelRequest, NewSession, NodeId, NodeKind,
    QueueTarget, QueuedMessage, QueuedMessageId, RequestId, ResponseMetadata, RuntimeError, TurnId,
};

mod agent_runs;
mod assistant_reasoning;
mod conversations;
mod global;
mod handle;
mod migrations;
mod patch_diffs;
mod queue;
mod supervised_work;
mod turns;

#[allow(clippy::wildcard_imports)]
use agent_runs::*;
#[allow(clippy::wildcard_imports)]
use conversations::*;
pub(crate) use global::GlobalStore;
#[allow(clippy::wildcard_imports)]
use migrations::*;
#[allow(clippy::wildcard_imports)]
use queue::*;
#[allow(clippy::wildcard_imports)]
use supervised_work::*;
#[allow(clippy::wildcard_imports)]
use turns::*;

const CONVERSATION_SCHEMA: &str = include_str!("../../migrations/conversation/0001_initial.sql");
const IMAGE_BLOB_HASHES_MIGRATION: &str =
    include_str!("../../migrations/conversation/0003_image_blob_hashes.sql");
const GLOBAL_SCHEMA: &str = include_str!("../../migrations/global/0001_initial.sql");
const INLINE_TOOL_OUTPUT_BYTES: usize = 1024 * 1024;

pub(crate) fn read_conversation_paths(
    path: &Path,
) -> Result<
    (
        std::path::PathBuf,
        std::path::PathBuf,
        Option<crate::WorktreeMetadata>,
    ),
    RuntimeError,
> {
    let connection = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    connection.pragma_update(None, "query_only", "ON")?;
    let (project_dir, cwd, worktree_json) = connection.query_row(
        "SELECT project_dir, cwd, worktree_json FROM conversations LIMIT 1",
        [],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        },
    )?;
    let worktree = worktree_json
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(RuntimeError::from)?;
    Ok((project_dir.into(), cwd.into(), worktree))
}

type DispatchedQueued = (NodeId, TurnId, String, Option<String>);
type StoredModelSelection = Option<(String, String, Option<String>, String)>;
type StoredModeSelections = BTreeMap<String, StoredModelSelection>;
type MultiEventResult<T> = Result<(T, Vec<DurableEvent>), RuntimeError>;

fn decode_stored_json<T: DeserializeOwned>(input: impl AsRef<[u8]>) -> Result<T, RuntimeError> {
    let mut input = input.as_ref().to_vec();
    simd_json::serde::from_slice(&mut input).map_err(|error| {
        RuntimeError::EventDecode(serde_json::Error::io(std::io::Error::other(error)))
    })
}

fn save_response_continuation(
    connection: &Connection,
    conversation_id: ConversationId,
    assistant_node_id: NodeId,
    continuation: &StoredResponseContinuation,
) -> Result<(), RuntimeError> {
    connection.execute(
        "INSERT OR REPLACE INTO response_continuations \
         (assistant_node_id, conversation_id, response_id, request_json, incorporated_input_json, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))",
        params![
            assistant_node_id.to_string(),
            conversation_id.to_string(),
            continuation.response_id,
            serde_json::to_string(&continuation.request)?,
            serde_json::to_string(&continuation.incorporated_input)?,
        ],
    )?;
    Ok(())
}

fn load_response_continuation(
    connection: &Connection,
    conversation_id: ConversationId,
    branch_tip: NodeId,
) -> Result<Option<StoredResponseContinuation>, RuntimeError> {
    // Only accept a snapshot anchored on the active ancestry. This naturally
    // excludes siblings after a fork and is safe when no provider state exists.
    connection
        .query_row(
            "WITH RECURSIVE ancestry(id, parent_id) AS (
                SELECT id, parent_id FROM nodes WHERE id = ?1 AND conversation_id = ?2
                UNION ALL
                SELECT n.id, n.parent_id FROM nodes n JOIN ancestry a ON n.id = a.parent_id
             )
             SELECT response_id, request_json, incorporated_input_json
             FROM response_continuations
             WHERE conversation_id = ?2 AND assistant_node_id IN (SELECT id FROM ancestry)
             ORDER BY created_at DESC LIMIT 1",
            params![branch_tip.to_string(), conversation_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?
        .map(|(response_id, request, incorporated_input)| {
            Ok(StoredResponseContinuation {
                response_id,
                request: decode_stored_json(request)?,
                incorporated_input: decode_stored_json(incorporated_input)?,
            })
        })
        .transpose()
}

#[derive(Clone, Debug)]
pub(crate) struct PendingToolCall {
    pub(crate) provider_call_id: String,
    pub(crate) name: String,
    pub(crate) arguments: serde_json::Value,
    pub(crate) request_index: u64,
    pub(crate) provider_metadata: serde_json::Value,
}

#[derive(Clone, Debug)]
pub(crate) struct StoredToolCall {
    pub(crate) node_id: NodeId,
    pub(crate) provider_call_id: String,
    pub(crate) name: String,
    pub(crate) arguments: serde_json::Value,
    pub(crate) request_index: u64,
    pub(crate) provider_metadata: serde_json::Value,
}

#[derive(Clone, Debug)]
pub(crate) struct ModelAttemptSnapshot {
    pub(crate) request_id: RequestId,
    pub(crate) attempt_id: AttemptId,
    pub(crate) model: ModelRef,
    pub(crate) effort: Option<String>,
    pub(crate) agent: String,
    pub(crate) mode: String,
    pub(crate) context_window: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct RetryContext {
    pub(crate) parent_id: NodeId,
    pub(crate) turn_id: TurnId,
    pub(crate) input: Vec<ModelInput>,
}

#[derive(Clone, Debug)]
pub(crate) struct CompactionPathItem {
    pub(crate) node_id: NodeId,
    /// Atomic protocol group. Standalone messages use their own node ID;
    /// calls/results from one assistant response share its assistant node ID.
    pub(crate) group_id: NodeId,
    pub(crate) input: ModelInput,
}

#[derive(Clone, Debug)]
pub(crate) struct CompactionSource {
    pub(crate) source_tip_node_id: NodeId,
    pub(crate) checkpoint_parent_id: NodeId,
    pub(crate) excluded_node_id: Option<NodeId>,
    pub(crate) reapplied_node_id: Option<NodeId>,
    pub(crate) previous_checkpoint: Option<(NodeId, CompactionSummary)>,
    pub(crate) effective_input: Vec<ModelInput>,
    pub(crate) tail_candidates: Vec<CompactionPathItem>,
}

#[derive(Clone, Debug)]
pub(crate) struct StoredResponseContinuation {
    pub(crate) response_id: String,
    pub(crate) request: ModelRequest,
    pub(crate) incorporated_input: Vec<ModelInput>,
}

pub(crate) struct StoredSessionStats {
    pub(crate) title: Option<String>,
    pub(crate) message_count: u64,
    pub(crate) usage: crate::SessionUsage,
}

/// Store-owned inputs required to project an attached session. This keeps
/// snapshot hydration to one request through the serialized store task.
pub(crate) struct SessionHydration {
    pub(crate) cursor: Option<EventCursor>,
    pub(crate) durable_turn: crate::TurnState,
    pub(crate) queue: Vec<QueuedMessage>,
    pub(crate) active_profiles: (String, String),
    pub(crate) usage: crate::SessionUsage,
    pub(crate) context: Option<crate::ContextUsage>,
    pub(crate) terminals: Vec<crate::TerminalSnapshot>,
    pub(crate) title: Option<String>,
    pub(crate) model_selection: StoredModelSelection,
    pub(crate) agent_runs: Vec<crate::AgentRun>,
}

pub(crate) struct HistoryHydration {
    pub(crate) revision: Option<EventCursor>,
    pub(crate) history: Vec<crate::HistoryNode>,
    pub(crate) workspace: std::path::PathBuf,
    pub(crate) terminals: Vec<crate::TerminalSnapshot>,
    pub(crate) agent_runs: Vec<crate::AgentRun>,
}

/// Bounded durable inputs for one transcript page.
pub(crate) struct TranscriptPageHydration {
    pub(crate) active_node_id: NodeId,
    pub(crate) history: Vec<crate::HistoryNode>,
    pub(crate) events: Vec<TranscriptPageEvent>,
    pub(crate) older: Option<crate::TranscriptCursor>,
    pub(crate) workspace: std::path::PathBuf,
    pub(crate) terminals: Vec<crate::TerminalSnapshot>,
    pub(crate) agent_runs: Vec<crate::AgentRun>,
}

pub(crate) enum TranscriptPageEvent {
    NodeAppended {
        cursor: EventCursor,
        node_id: NodeId,
    },
}

pub(crate) struct CreatedSession {
    pub(crate) id: ConversationId,
}

#[derive(Clone, Debug)]
pub(crate) struct ComposerHistoryRecord {
    pub(crate) entry_id: String,
    pub(crate) text: String,
    pub(crate) kind: crate::ComposerInputKind,
    pub(crate) attachment_specs: Vec<AttachmentSpec>,
    pub(crate) images: Vec<crate::ImageAttachment>,
    pub(crate) image_chips: Vec<crate::ImageChipRange>,
    pub(crate) created_at: String,
}

impl ComposerHistoryRecord {
    pub(crate) fn into_entry(self) -> ComposerHistoryEntry {
        ComposerHistoryEntry {
            kind: self.kind,
            text: self.text,
            attachment_specs: self.attachment_specs,
            images: self.images,
            image_chips: self.image_chips,
        }
    }
}

/// Store-owned state needed to resume a durable session in one request.
pub(crate) struct ResumedSession {
    pub(crate) project_dir: std::path::PathBuf,
    pub(crate) cwd: std::path::PathBuf,
    pub(crate) worktree: Option<crate::WorktreeMetadata>,
    pub(crate) model_selection: StoredModelSelection,
    pub(crate) plan_model_selection: StoredModelSelection,
    pub(crate) mode_selections: StoredModeSelections,
    pub(crate) active_profiles: (String, String),
    pub(crate) composer_history: Vec<ComposerHistoryRecord>,
    pub(crate) completed_active_plan: Option<String>,
}

#[derive(Clone)]
pub(crate) struct StoreHandle {
    tx: mpsc::Sender<StoreCommand>,
    live_events: broadcast::Sender<DurableEvent>,
    database_path: Option<std::path::PathBuf>,
    _writer_lock: Option<std::sync::Arc<std::fs::File>>,
    _open_lock: Option<std::sync::Arc<std::fs::File>>,
}

struct Subscribers {
    live: broadcast::Sender<DurableEvent>,
}

#[allow(clippy::large_enum_variant)] // Commands carry complete typed persistence requests.
enum StoreCommand {
    LoadConversationDiff {
        conversation_id: ConversationId,
        reply: oneshot::Sender<Result<crate::tools::ConversationDiff, RuntimeError>>,
    },
    ClearConversationDiff {
        conversation_id: ConversationId,
        reply: oneshot::Sender<Result<(), RuntimeError>>,
    },
    #[allow(dead_code)]
    CreateSession {
        options: NewSession,
        reply: oneshot::Sender<Result<ConversationId, RuntimeError>>,
    },
    CreateInitializedSession {
        conversation_id: ConversationId,
        options: NewSession,
        name: Option<String>,
        agent: String,
        mode: String,
        model_selection: Option<(String, String, Option<String>)>,
        plan_model_selection: Option<(String, String, Option<String>)>,
        mode_selections: Vec<(String, String, String, Option<String>)>,
        reply: oneshot::Sender<Result<CreatedSession, RuntimeError>>,
    },
    LoadResumedSession {
        conversation_id: ConversationId,
        reply: oneshot::Sender<Result<ResumedSession, RuntimeError>>,
    },
    AppendConfigurationChanged {
        conversation_id: ConversationId,
        path: Option<std::path::PathBuf>,
        reply: oneshot::Sender<Result<(), RuntimeError>>,
    },
    AppendTranscriptNotice {
        conversation_id: ConversationId,
        message: String,
        reply: oneshot::Sender<Result<(), RuntimeError>>,
    },
    AppendRecap {
        conversation_id: ConversationId,
        parent_id: NodeId,
        text: String,
        reply: oneshot::Sender<Result<bool, RuntimeError>>,
    },
    AppendWorkspaceTransition {
        conversation_id: ConversationId,
        transition: crate::runtime::worktrees::WorkspaceTransition,
        reply: oneshot::Sender<Result<(), RuntimeError>>,
    },
    SynchronizeLocalContext {
        conversation_id: ConversationId,
        snapshot: crate::LocalContextSnapshot,
        reply: oneshot::Sender<Result<Option<LocalContextSync>, RuntimeError>>,
    },
    #[allow(dead_code)]
    ListConversations {
        workspace: Option<std::path::PathBuf>,
        reply: oneshot::Sender<Result<Vec<crate::ConversationSummary>, RuntimeError>>,
    },
    SetConversationArchived {
        conversation_id: ConversationId,
        archived: bool,
        reply: oneshot::Sender<Result<(), RuntimeError>>,
    },
    SetConversationFavourite {
        conversation_id: ConversationId,
        favourite: bool,
        reply: oneshot::Sender<Result<(), RuntimeError>>,
    },
    LoadComposerHistory {
        conversation_id: ConversationId,
        new_session_seed: bool,
        reply: oneshot::Sender<Result<Vec<ComposerHistoryEntry>, RuntimeError>>,
    },
    RecordSlashCommand {
        conversation_id: ConversationId,
        text: String,
        user_text: Option<String>,
        publish_to_global: bool,
        reply: oneshot::Sender<Result<(), RuntimeError>>,
    },
    RecordBashCommand {
        conversation_id: ConversationId,
        command: String,
        reply: oneshot::Sender<Result<NodeId, RuntimeError>>,
    },
    LoadConversationTitle {
        conversation_id: ConversationId,
        reply: oneshot::Sender<Result<Option<String>, RuntimeError>>,
    },
    RenameConversation {
        conversation_id: ConversationId,
        title: String,
        reply: oneshot::Sender<Result<String, RuntimeError>>,
    },
    ClaimTitleGeneration {
        conversation_id: ConversationId,
        reply: oneshot::Sender<Result<Option<TitleGenerationClaim>, RuntimeError>>,
    },
    FinishTitleGeneration {
        conversation_id: ConversationId,
        title: Option<String>,
        reply: oneshot::Sender<Result<bool, RuntimeError>>,
    },
    LoadHistory {
        conversation_id: ConversationId,
        reply: oneshot::Sender<Result<Vec<crate::HistoryNode>, RuntimeError>>,
    },
    LoadDurableCursor {
        conversation_id: ConversationId,
        reply: oneshot::Sender<Result<Option<EventCursor>, RuntimeError>>,
    },
    LoadSessionHydration {
        conversation_id: ConversationId,
        reply: oneshot::Sender<Result<SessionHydration, RuntimeError>>,
    },
    LoadTranscriptPage {
        conversation_id: ConversationId,
        cursor: Option<crate::TranscriptCursor>,
        reply: oneshot::Sender<Result<TranscriptPageHydration, RuntimeError>>,
    },
    LoadSessionStats {
        conversation_id: ConversationId,
        reply: oneshot::Sender<Result<StoredSessionStats, RuntimeError>>,
    },
    Fork {
        conversation_id: ConversationId,
        at: NodeId,
        planning_modes: Vec<String>,
        reply: oneshot::Sender<Result<NodeId, RuntimeError>>,
    },
    HardFork {
        conversation_id: ConversationId,
        new_conversation_id: ConversationId,
        at: NodeId,
        destination: std::path::PathBuf,
        planning_modes: Vec<String>,
        reply: oneshot::Sender<Result<NodeId, RuntimeError>>,
    },
    LoadWorkspace {
        id: ConversationId,
        reply: oneshot::Sender<Result<std::path::PathBuf, RuntimeError>>,
    },
    LoadModelSelection {
        id: ConversationId,
        reply: oneshot::Sender<Result<StoredModelSelection, RuntimeError>>,
    },
    LoadPlanModelSelection {
        id: ConversationId,
        reply: oneshot::Sender<Result<StoredModelSelection, RuntimeError>>,
    },
    LoadModeSelections {
        id: ConversationId,
        reply: oneshot::Sender<Result<StoredModeSelections, RuntimeError>>,
    },
    LoadActiveProfiles {
        id: ConversationId,
        reply: oneshot::Sender<Result<(String, String), RuntimeError>>,
    },
    SetActiveProfile {
        conversation_id: ConversationId,
        agent: Option<String>,
        mode: Option<String>,
        pending: bool,
        reply: oneshot::Sender<Result<(), RuntimeError>>,
    },
    CreateAgentRun {
        conversation_id: ConversationId,
        parent_turn_id: TurnId,
        profile: String,
        model: ModelRef,
        effort: Option<String>,
        task: String,
        reply: oneshot::Sender<Result<crate::AgentRun, RuntimeError>>,
    },
    SetAgentRunStatus {
        conversation_id: ConversationId,
        id: crate::AgentRunId,
        status: crate::AgentRunStatus,
        result: Option<String>,
        error: Option<String>,
        usage: Option<crate::ModelUsage>,
        reply: oneshot::Sender<Result<crate::AgentRun, RuntimeError>>,
    },
    LoadAgentRun {
        conversation_id: ConversationId,
        id: crate::AgentRunId,
        reply: oneshot::Sender<Result<crate::AgentRun, RuntimeError>>,
    },
    LoadAgentRunLogPage {
        conversation_id: ConversationId,
        id: crate::AgentRunId,
        after: u64,
        reply: oneshot::Sender<Result<crate::AgentRunLogPage, RuntimeError>>,
    },
    ListAgentRuns {
        conversation_id: ConversationId,
        reply: oneshot::Sender<Result<Vec<crate::AgentRun>, RuntimeError>>,
    },
    AppendAgentRunActivity {
        conversation_id: ConversationId,
        id: crate::AgentRunId,
        tool: String,
        arguments: serde_json::Value,
        output: serde_json::Value,
        is_error: bool,
        permission_audit: Option<Box<crate::PermissionAudit>>,
        reply: oneshot::Sender<Result<crate::AgentRun, RuntimeError>>,
    },
    AppendAgentRunAssistant {
        conversation_id: ConversationId,
        id: crate::AgentRunId,
        text: String,
        reply: oneshot::Sender<Result<crate::AgentRun, RuntimeError>>,
    },
    UpsertTerminal {
        terminal: crate::TerminalSnapshot,
        reply: oneshot::Sender<Result<(), RuntimeError>>,
    },
    LoadTerminal {
        conversation_id: ConversationId,
        id: crate::TerminalId,
        reply: oneshot::Sender<Result<crate::TerminalSnapshot, RuntimeError>>,
    },
    ListTerminals {
        conversation_id: ConversationId,
        reply: oneshot::Sender<Result<Vec<crate::TerminalSnapshot>, RuntimeError>>,
    },
    ClaimCompletion {
        conversation_id: ConversationId,
        kind: String,
        id: String,
        claim_kind: String,
        reply: oneshot::Sender<Result<bool, RuntimeError>>,
    },
    #[allow(dead_code)]
    ClaimAgentCompletions {
        conversation_id: ConversationId,
        ids: Vec<String>,
        claim_kind: String,
        reply: oneshot::Sender<Result<(), RuntimeError>>,
    },
    AppendPendingCompletionNotice {
        conversation_id: ConversationId,
        reply: oneshot::Sender<Result<Option<(NodeId, TurnId)>, RuntimeError>>,
    },
    AppendInterruptNotice {
        conversation_id: ConversationId,
        queued_steering: bool,
        reply: oneshot::Sender<Result<Option<NodeId>, RuntimeError>>,
    },
    #[allow(dead_code)]
    InitializeModelSelection {
        conversation_id: ConversationId,
        provider: String,
        model: String,
        effort: Option<String>,
        plan: bool,
        reply: oneshot::Sender<Result<(), RuntimeError>>,
    },
    InitializeModeModelSelection {
        conversation_id: ConversationId,
        mode: String,
        provider: String,
        model: String,
        effort: Option<String>,
        reply: oneshot::Sender<Result<(), RuntimeError>>,
    },
    LoadRetryContext {
        id: ConversationId,
        reply: oneshot::Sender<Result<RetryContext, RuntimeError>>,
    },
    ReconstructModelInput {
        conversation_id: ConversationId,
        node_id: NodeId,
        resize_images: bool,
        reply: oneshot::Sender<Result<Vec<ModelInput>, RuntimeError>>,
    },
    LoadCompactionSource {
        conversation_id: ConversationId,
        resize_images: bool,
        exclude_active_tip: bool,
        reply: oneshot::Sender<Result<CompactionSource, RuntimeError>>,
    },
    AppendCompactionSummary {
        conversation_id: ConversationId,
        parent_id: NodeId,
        content: CompactionSummary,
        metadata: ResponseMetadata,
        reply: oneshot::Sender<Result<NodeId, RuntimeError>>,
    },
    #[allow(dead_code)]
    LoadRecentModels {
        provider: String,
        limit: usize,
        reply: oneshot::Sender<Result<Vec<String>, RuntimeError>>,
    },
    LoadBlob {
        id: crate::BlobId,
        reply: oneshot::Sender<Result<Vec<u8>, RuntimeError>>,
    },
    StoreImageBlob {
        id: crate::BlobId,
        sha256: String,
        bytes: Vec<u8>,
        reply: oneshot::Sender<Result<crate::BlobId, RuntimeError>>,
    },
    AppendUser {
        conversation_id: ConversationId,
        text: String,
        attachments: Vec<CapturedAttachment>,
        attachment_specs: Vec<AttachmentSpec>,
        deferred_attachment_paths: Vec<std::path::PathBuf>,
        images: Vec<crate::ImageAttachment>,
        image_chips: Vec<crate::ImageChipRange>,
        reply: oneshot::Sender<Result<(NodeId, TurnId), RuntimeError>>,
    },
    #[allow(dead_code)]
    SetModelSelection {
        conversation_id: ConversationId,
        provider: String,
        model: String,
        effort: Option<String>,
        pending: bool,
        plan: bool,
        reply: oneshot::Sender<Result<(), RuntimeError>>,
    },
    SetModeModelSelection {
        conversation_id: ConversationId,
        mode: String,
        provider: String,
        model: String,
        effort: Option<String>,
        pending: bool,
        reply: oneshot::Sender<Result<(), RuntimeError>>,
    },
    QueueInput {
        conversation_id: ConversationId,
        text: String,
        target: QueueTarget,
        attachments: Vec<AttachmentSpec>,
        images: Vec<crate::ImageAttachment>,
        image_chips: Vec<crate::ImageChipRange>,
        blocked_by_startup: bool,
        require_subagent: bool,
        reply: oneshot::Sender<Result<QueuedMessage, RuntimeError>>,
    },
    QueueCompact {
        conversation_id: ConversationId,
        target: QueueTarget,
        instructions: Option<String>,
        reply: oneshot::Sender<Result<QueuedMessage, RuntimeError>>,
    },
    QueueModeInput {
        conversation_id: ConversationId,
        mode: String,
        command_text: String,
        text: String,
        target: QueueTarget,
        attachments: Vec<AttachmentSpec>,
        reply: oneshot::Sender<Result<QueuedMessage, RuntimeError>>,
    },
    BeginEditingQueued {
        conversation_id: ConversationId,
        id: QueuedMessageId,
        reply: oneshot::Sender<Result<(), RuntimeError>>,
    },
    ReplaceQueued {
        conversation_id: ConversationId,
        id: QueuedMessageId,
        text: String,
        target: Option<QueueTarget>,
        attachments: Vec<AttachmentSpec>,
        images: Vec<crate::ImageAttachment>,
        image_chips: Vec<crate::ImageChipRange>,
        reply: oneshot::Sender<Result<QueuedMessage, RuntimeError>>,
    },
    ReplaceQueuedModeInput {
        conversation_id: ConversationId,
        id: QueuedMessageId,
        mode: String,
        command_text: String,
        text: String,
        target: Option<QueueTarget>,
        attachments: Vec<AttachmentSpec>,
        reply: oneshot::Sender<Result<QueuedMessage, RuntimeError>>,
    },
    DeleteQueued {
        conversation_id: ConversationId,
        id: QueuedMessageId,
        reply: oneshot::Sender<Result<(), RuntimeError>>,
    },
    PromoteQueued {
        conversation_id: ConversationId,
        id: QueuedMessageId,
        reply: oneshot::Sender<Result<QueuedMessage, RuntimeError>>,
    },
    DispatchQueued {
        conversation_id: ConversationId,
        expected: QueuedMessage,
        attachments: Vec<CapturedAttachment>,
        attachment_specs: Vec<AttachmentSpec>,
        deferred_attachment_paths: Vec<std::path::PathBuf>,
        reply: oneshot::Sender<Result<(NodeId, TurnId, String, Option<String>), RuntimeError>>,
    },
    PeekNextQueued {
        conversation_id: ConversationId,
        target: QueueTarget,
        reply: oneshot::Sender<Result<Option<QueuedMessage>, RuntimeError>>,
    },
    PeekNextStartupQueued {
        conversation_id: ConversationId,
        reply: oneshot::Sender<Result<Option<QueuedMessage>, RuntimeError>>,
    },
    StartAssistant {
        conversation_id: ConversationId,
        parent_id: NodeId,
        turn_id: TurnId,
        attempt: Option<ModelAttemptSnapshot>,
        reply: oneshot::Sender<Result<NodeId, RuntimeError>>,
    },
    AppendAssistantDelta {
        conversation_id: ConversationId,
        node_id: NodeId,
        text: String,
        delta: String,
        reply: oneshot::Sender<Result<(), RuntimeError>>,
    },
    StartPlan {
        conversation_id: ConversationId,
        node_id: NodeId,
        reply: oneshot::Sender<Result<(), RuntimeError>>,
    },
    AppendPlanDelta {
        conversation_id: ConversationId,
        node_id: NodeId,
        text: String,
        delta: String,
        reply: oneshot::Sender<Result<(), RuntimeError>>,
    },
    #[allow(dead_code)]
    AppendContextClear {
        conversation_id: ConversationId,
        mode: String,
        context_window: u64,
        reply: oneshot::Sender<Result<NodeId, RuntimeError>>,
    },
    AppendAcceptedPlan {
        conversation_id: ConversationId,
        plan: String,
        note: Option<String>,
        clear_context: bool,
        compact_context: bool,
        context_window: u64,
        reply: oneshot::Sender<Result<(NodeId, TurnId), RuntimeError>>,
    },
    CompleteAssistant {
        conversation_id: ConversationId,
        node_id: NodeId,
        metadata: ResponseMetadata,
        tool_calls: Vec<PendingToolCall>,
        reasoning: Vec<ModelInput>,
        reply: oneshot::Sender<Result<Vec<StoredToolCall>, RuntimeError>>,
    },
    SaveResponseContinuation {
        conversation_id: ConversationId,
        assistant_node_id: NodeId,
        continuation: StoredResponseContinuation,
        reply: oneshot::Sender<Result<(), RuntimeError>>,
    },
    LoadResponseContinuation {
        conversation_id: ConversationId,
        branch_tip: NodeId,
        reply: oneshot::Sender<Result<Option<StoredResponseContinuation>, RuntimeError>>,
    },
    AppendToolResult {
        conversation_id: ConversationId,
        tool_call: StoredToolCall,
        output: serde_json::Value,
        is_error: bool,
        duration_millis: u64,
        transition: Option<crate::runtime::worktrees::WorkspaceTransition>,
        reply: oneshot::Sender<Result<NodeId, RuntimeError>>,
    },
    AppendPermissionDecision {
        conversation_id: ConversationId,
        tool_call: StoredToolCall,
        audit: Box<crate::PermissionAudit>,
        reply: oneshot::Sender<Result<NodeId, RuntimeError>>,
    },
    CancelAssistant {
        conversation_id: ConversationId,
        node_id: NodeId,
        reply: oneshot::Sender<Result<(), RuntimeError>>,
    },
    FailAssistant {
        conversation_id: ConversationId,
        node_id: NodeId,
        status: &'static str,
        rewind_to_parent: bool,
        code: String,
        message: String,
        retryable: bool,
        reply: oneshot::Sender<Result<(), RuntimeError>>,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct LocalContextSync {
    pub(crate) node_id: NodeId,
    pub(crate) message: String,
    pub(crate) warnings: Vec<crate::LocalContextWarning>,
}

#[allow(clippy::too_many_lines)]
async fn run_store(
    mut connection: Connection,
    mut rx: mpsc::Receiver<StoreCommand>,
    live: broadcast::Sender<DurableEvent>,
) {
    if let Err(error) = prune_unreferenced_image_blobs(&connection) {
        tracing::warn!(%error, "could not prune unreferenced image blobs");
    }
    let mut subscribers = Subscribers { live };
    while let Some(command) = rx.recv().await {
        let command = {
            let _command_span = tracing::trace_span!("store.command").entered();
            let Some(command) = handle_tool_command(command, &mut connection, &mut subscribers)
            else {
                continue;
            };
            let Some(command) = handle_queue_command(command, &mut connection, &mut subscribers)
            else {
                continue;
            };
            command
        };
        match command {
            StoreCommand::CreateSession { options, reply } => {
                let _ = reply.send(create_session(&mut connection, &options));
            }
            StoreCommand::CreateInitializedSession {
                conversation_id,
                options,
                name,
                agent,
                mode,
                model_selection,
                plan_model_selection,
                mode_selections,
                reply,
            } => {
                let result = {
                    let _span = tracing::trace_span!("store.session.create_initialized").entered();
                    (|| {
                        let id = {
                            let _span = tracing::trace_span!("store.session.persist_initial_state")
                                .entered();
                            let transaction = connection.transaction()?;
                            let id = create_session_in_transaction_with_title(
                                &transaction,
                                conversation_id,
                                &options,
                                name.as_deref(),
                            )?;
                            initialize_active_profiles_in_transaction(
                                &transaction,
                                id,
                                &agent,
                                &mode,
                            )?;
                            for (provider, model, effort, plan) in model_selection
                                .into_iter()
                                .map(|(provider, model, effort)| (provider, model, effort, false))
                                .chain(plan_model_selection.into_iter().map(
                                    |(provider, model, effort)| (provider, model, effort, true),
                                ))
                            {
                                conversations::initialize_model_selection_in_transaction(
                                    &transaction,
                                    id,
                                    &provider,
                                    &model,
                                    effort.as_deref(),
                                    plan,
                                )?;
                            }
                            for (mode, provider, model, effort) in &mode_selections {
                                initialize_mode_model_selection_in_transaction(
                                    &transaction,
                                    id,
                                    mode,
                                    provider,
                                    model,
                                    effort.as_deref(),
                                )?;
                            }
                            transaction.commit()?;
                            id
                        };

                        Ok(CreatedSession { id })
                    })()
                };
                let _ = reply.send(result);
            }
            StoreCommand::HardFork {
                conversation_id,
                new_conversation_id,
                at,
                destination,
                planning_modes,
                reply,
            } => {
                let result = hard_fork(
                    &mut connection,
                    conversation_id,
                    new_conversation_id,
                    at,
                    &destination,
                    &planning_modes,
                );
                let _ = reply.send(result);
            }
            StoreCommand::SaveResponseContinuation {
                conversation_id,
                assistant_node_id,
                continuation,
                reply,
            } => {
                let result = save_response_continuation(
                    &connection,
                    conversation_id,
                    assistant_node_id,
                    &continuation,
                );
                let _ = reply.send(result);
            }
            StoreCommand::LoadResponseContinuation {
                conversation_id,
                branch_tip,
                reply,
            } => {
                let _ = reply.send(load_response_continuation(
                    &connection,
                    conversation_id,
                    branch_tip,
                ));
            }
            StoreCommand::LoadResumedSession {
                conversation_id,
                reply,
            } => {
                let _span = tracing::trace_span!(
                    "store.session.resume",
                    session_id = %conversation_id
                )
                .entered();
                let result = (|| {
                    let (project_dir, cwd, worktree) =
                        load_session_paths(&connection, conversation_id)?;
                    Ok(ResumedSession {
                        model_selection: load_model_selection(&connection, conversation_id)?,
                        mode_selections: load_mode_selections(&connection, conversation_id)?,
                        plan_model_selection: load_plan_model_selection(
                            &connection,
                            conversation_id,
                        )?,
                        active_profiles: load_active_profiles(&connection, conversation_id)?,
                        composer_history: load_composer_history_records(
                            &connection,
                            conversation_id,
                        )?,
                        completed_active_plan: connection
                            .query_row(
                                "WITH RECURSIVE resumable_tip(id, parent_id, kind, status, content_json, depth) AS (
                                     SELECT n.id, n.parent_id, n.kind, n.status, n.content_json, 0
                                     FROM conversations c
                                     JOIN nodes n ON n.id = c.active_node_id
                                     WHERE c.id = ?1
                                     UNION ALL
                                     SELECT parent.id, parent.parent_id, parent.kind, parent.status,
                                            parent.content_json, tip.depth + 1
                                     FROM resumable_tip tip
                                     JOIN nodes parent ON parent.id = tip.parent_id
                                     WHERE tip.kind = 'system'
                                 )
                                 SELECT json_extract(content_json, '$.text')
                                 FROM resumable_tip
                                 WHERE kind = 'assistant_message' AND status = 'completed'
                                   AND json_extract(content_json, '$.flavor') = 'plan'
                                 ORDER BY depth LIMIT 1",
                                [conversation_id.to_string()],
                                |row| row.get::<_, String>(0),
                            )
                            .optional()?,
                        project_dir,
                        cwd,
                        worktree,
                    })
                })();
                let _ = reply.send(result);
            }
            StoreCommand::AppendConfigurationChanged {
                conversation_id,
                path,
                reply,
            } => {
                let result = append_configuration_changed(&mut connection, conversation_id, path);
                publish_result(&mut subscribers, &result);
                let _ = reply.send(result.map(|_| ()));
            }
            StoreCommand::AppendTranscriptNotice {
                conversation_id,
                message,
                reply,
            } => {
                let result = append_transcript_notice(&mut connection, conversation_id, &message);
                publish_result(&mut subscribers, &result);
                let _ = reply.send(result.map(|_| ()));
            }
            StoreCommand::AppendRecap {
                conversation_id,
                parent_id,
                text,
                reply,
            } => {
                let result = append_recap(&mut connection, conversation_id, parent_id, &text);
                publish_result(&mut subscribers, &result);
                let _ = reply.send(result.map(|(stored, _)| stored));
            }
            StoreCommand::AppendWorkspaceTransition {
                conversation_id,
                transition,
                reply,
            } => {
                let result =
                    append_workspace_transition(&mut connection, conversation_id, &transition);
                publish_result(&mut subscribers, &result);
                let _ = reply.send(result.map(|_| ()));
            }
            StoreCommand::SynchronizeLocalContext {
                conversation_id,
                snapshot,
                reply,
            } => {
                let result = synchronize_local_context(&mut connection, conversation_id, &snapshot);
                if let Ok((_, Some(event))) = &result {
                    publish_event(&mut subscribers, event);
                }
                let _ = reply.send(result.map(|(sync, _)| sync));
            }
            StoreCommand::ListConversations { workspace, reply } => {
                let _ = reply.send(list_conversations(&connection, workspace.as_deref()));
            }
            StoreCommand::SetConversationArchived {
                conversation_id,
                archived,
                reply,
            } => {
                let result = set_conversation_archived(&mut connection, conversation_id, archived);
                let _ = reply.send(result);
            }
            StoreCommand::SetConversationFavourite {
                conversation_id,
                favourite,
                reply,
            } => {
                let result =
                    set_conversation_favourite(&mut connection, conversation_id, favourite);
                let _ = reply.send(result);
            }
            StoreCommand::LoadComposerHistory {
                conversation_id,
                new_session_seed,
                reply,
            } => {
                let _ = reply.send(load_composer_history(
                    &connection,
                    conversation_id,
                    new_session_seed,
                ));
            }
            StoreCommand::RecordSlashCommand {
                conversation_id,
                text,
                user_text,
                publish_to_global,
                reply,
            } => {
                let _ = reply.send(record_slash_command(
                    &mut connection,
                    conversation_id,
                    &text,
                    user_text.as_deref(),
                    publish_to_global,
                ));
            }
            StoreCommand::LoadConversationTitle {
                conversation_id,
                reply,
            } => {
                let _ = reply.send(load_conversation_title(&connection, conversation_id));
            }
            StoreCommand::RenameConversation {
                conversation_id,
                title,
                reply,
            } => {
                let result = rename_conversation(&mut connection, conversation_id, &title);
                publish_result(&mut subscribers, &result);
                let _ = reply.send(result.map(|(title, _)| title));
            }
            StoreCommand::ClaimTitleGeneration {
                conversation_id,
                reply,
            } => {
                let _ = reply.send(claim_title_generation(&mut connection, conversation_id));
            }
            StoreCommand::FinishTitleGeneration {
                conversation_id,
                title,
                reply,
            } => {
                let result =
                    finish_title_generation(&mut connection, conversation_id, title.as_deref());
                if let Ok(Some(((), event))) = &result {
                    publish_event(&mut subscribers, event);
                }
                let _ = reply.send(result.map(|result| result.is_some()));
            }
            StoreCommand::LoadHistory {
                conversation_id,
                reply,
            } => {
                let _ = reply.send(load_history(&connection, conversation_id));
            }
            StoreCommand::LoadDurableCursor {
                conversation_id,
                reply,
            } => {
                let result = (|| {
                    ensure_session(&connection, conversation_id)?;
                    connection
                        .query_row(
                            "SELECT revision FROM conversations WHERE id = ?1",
                            [conversation_id.to_string()],
                            |row| row.get::<_, i64>(0).map(Some),
                        )?
                        .map(|value| {
                            value.try_into().map(EventCursor).map_err(|_| {
                                RuntimeError::InvalidOption("negative stored event sequence".into())
                            })
                        })
                        .transpose()
                })();
                let _ = reply.send(result);
            }
            StoreCommand::RecordBashCommand {
                conversation_id,
                command,
                reply,
            } => {
                let _ = reply.send(record_bash_command(
                    &mut connection,
                    conversation_id,
                    &command,
                ));
            }
            StoreCommand::LoadSessionHydration {
                conversation_id,
                reply,
            } => {
                let result = (|| {
                    ensure_session(&connection, conversation_id)?;
                    let cursor = connection
                        .query_row(
                            "SELECT revision FROM conversations WHERE id = ?1",
                            [conversation_id.to_string()],
                            |row| row.get::<_, i64>(0).map(Some),
                        )?
                        .map(|value| -> Result<EventCursor, RuntimeError> {
                            Ok(EventCursor(value.try_into().map_err(|_| {
                                RuntimeError::InvalidOption("negative stored event sequence".into())
                            })?))
                        })
                        .transpose()?;
                    let durable_turn = connection
                        .query_row(
                            "WITH RECURSIVE active(id, parent_id, turn_id, status, created_at, depth) AS (
                                 SELECT n.id, n.parent_id, n.turn_id, n.status, n.created_at, 0
                                 FROM conversations c JOIN nodes n ON n.id = c.active_node_id
                                 WHERE c.id = ?1
                                 UNION ALL
                                 SELECT n.id, n.parent_id, n.turn_id, n.status, n.created_at, active.depth + 1
                                 FROM nodes n JOIN active ON n.id = active.parent_id
                             )
                             SELECT turn_id, created_at FROM active
                             WHERE turn_id IS NOT NULL AND status IN ('pending', 'running', 'streaming')
                             ORDER BY depth LIMIT 1",
                            [conversation_id.to_string()],
                            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                        )
                        .optional()?
                        .map(|(turn_id, started_at)| -> Result<crate::TurnState, RuntimeError> {
                            Ok(crate::TurnState::Working {
                                started_at,
                                turn_id: parse_stored_id(Some(turn_id), "active turn")?,
                            })
                        })
                        .transpose()?
                        .unwrap_or(crate::TurnState::Idle);
                    let active_profiles = load_active_profiles(&connection, conversation_id)?;
                    let model_selection = load_active_model_selection(
                        &connection,
                        conversation_id,
                        &active_profiles.1,
                    )?;
                    Ok(SessionHydration {
                        cursor,
                        durable_turn,
                        queue: list_queued_messages(&connection, conversation_id)?,
                        active_profiles,
                        usage: load_session_stats(&connection, conversation_id)?.usage,
                        context: load_active_context(&connection, conversation_id)?,
                        terminals: list_terminal_previews(&connection, conversation_id)?,
                        title: load_conversation_title(&connection, conversation_id)?,
                        model_selection,
                        agent_runs: list_agent_run_summaries(&connection, conversation_id)?,
                    })
                })();
                let _ = reply.send(result);
            }
            StoreCommand::LoadTranscriptPage {
                conversation_id,
                cursor,
                reply,
            } => {
                let _ = reply.send(load_transcript_page_hydration(
                    &connection,
                    conversation_id,
                    cursor.as_ref(),
                ));
            }
            StoreCommand::LoadSessionStats {
                conversation_id,
                reply,
            } => {
                let _ = reply.send(load_session_stats(&connection, conversation_id));
            }
            StoreCommand::Fork {
                conversation_id,
                at,
                planning_modes,
                reply,
            } => {
                let result = fork(&mut connection, conversation_id, at, &planning_modes);
                publish_result(&mut subscribers, &result);
                let _ = reply.send(result.map(|(node_id, _)| node_id));
            }
            StoreCommand::LoadWorkspace { id, reply } => {
                let _ = reply.send(load_workspace(&connection, id));
            }
            StoreCommand::LoadModelSelection { id, reply } => {
                let _ = reply.send(load_model_selection(&connection, id));
            }
            StoreCommand::LoadPlanModelSelection { id, reply } => {
                let _ = reply.send(load_plan_model_selection(&connection, id));
            }
            StoreCommand::LoadModeSelections { id, reply } => {
                let _ = reply.send(load_mode_selections(&connection, id));
            }
            StoreCommand::LoadActiveProfiles { id, reply } => {
                let _ = reply.send(load_active_profiles(&connection, id));
            }
            StoreCommand::SetActiveProfile {
                conversation_id,
                agent,
                mode,
                pending,
                reply,
            } => {
                let result = set_active_profile(
                    &mut connection,
                    conversation_id,
                    agent.as_deref(),
                    mode.as_deref(),
                    pending,
                );
                publish_result(&mut subscribers, &result);
                let _ = reply.send(result.map(|_| ()));
            }
            StoreCommand::CreateAgentRun {
                conversation_id,
                parent_turn_id,
                profile,
                model,
                effort,
                task,
                reply,
            } => {
                let _ = reply.send(create_agent_run(
                    &mut connection,
                    conversation_id,
                    parent_turn_id,
                    &profile,
                    &model,
                    effort.as_deref(),
                    &task,
                ));
            }
            StoreCommand::SetAgentRunStatus {
                conversation_id,
                id,
                status,
                result,
                error,
                usage,
                reply,
            } => {
                let _ = reply.send(set_agent_run_status(
                    &mut connection,
                    conversation_id,
                    id,
                    status,
                    result.as_deref(),
                    error.as_deref(),
                    usage.as_ref(),
                ));
            }
            StoreCommand::LoadAgentRun {
                conversation_id,
                id,
                reply,
            } => {
                let _ = reply.send(load_agent_run(&connection, conversation_id, id));
            }
            StoreCommand::LoadConversationDiff {
                conversation_id,
                reply,
            } => {
                let _ = reply.send(patch_diffs::load_conversation_diff(
                    &mut connection,
                    conversation_id,
                ));
            }
            StoreCommand::ClearConversationDiff {
                conversation_id,
                reply,
            } => {
                let _ = reply.send(patch_diffs::clear_conversation_diff(
                    &connection,
                    conversation_id,
                ));
            }
            StoreCommand::ListAgentRuns {
                conversation_id,
                reply,
            } => {
                let _ = reply.send(list_agent_runs(&connection, conversation_id));
            }
            StoreCommand::LoadAgentRunLogPage {
                conversation_id,
                id,
                after,
                reply,
            } => {
                let _ = reply.send(load_agent_run_log_page(
                    &connection,
                    conversation_id,
                    id,
                    after,
                ));
            }
            StoreCommand::AppendAgentRunActivity {
                conversation_id,
                id,
                tool,
                arguments,
                output,
                is_error,
                permission_audit,
                reply,
            } => {
                let result = append_agent_run_activity(
                    &mut connection,
                    conversation_id,
                    id,
                    &tool,
                    &arguments,
                    &output,
                    is_error,
                    permission_audit.as_deref(),
                );
                let _ = reply.send(result);
            }
            StoreCommand::AppendAgentRunAssistant {
                conversation_id,
                id,
                text,
                reply,
            } => {
                let result =
                    append_agent_run_assistant(&mut connection, conversation_id, id, &text);
                let _ = reply.send(result);
            }
            StoreCommand::UpsertTerminal { terminal, reply } => {
                let _ = reply.send(upsert_terminal(&mut connection, &terminal));
            }
            StoreCommand::LoadTerminal {
                conversation_id,
                id,
                reply,
            } => {
                let _ = reply.send(load_terminal(&connection, conversation_id, id));
            }
            StoreCommand::ListTerminals {
                conversation_id,
                reply,
            } => {
                let _ = reply.send(list_terminals(&connection, conversation_id));
            }
            StoreCommand::ClaimCompletion {
                conversation_id,
                kind,
                id,
                claim_kind,
                reply,
            } => {
                let _ = reply.send(claim_completion(
                    &mut connection,
                    conversation_id,
                    &kind,
                    &id,
                    &claim_kind,
                ));
            }
            StoreCommand::ClaimAgentCompletions {
                conversation_id,
                ids,
                claim_kind,
                reply,
            } => {
                let _ = reply.send(claim_agent_completions(
                    &mut connection,
                    conversation_id,
                    &ids,
                    &claim_kind,
                ));
            }
            StoreCommand::AppendPendingCompletionNotice {
                conversation_id,
                reply,
            } => {
                let _ = reply.send(append_pending_completion_notice(
                    &mut connection,
                    conversation_id,
                ));
            }
            StoreCommand::AppendInterruptNotice {
                conversation_id,
                queued_steering,
                reply,
            } => {
                let result =
                    append_interrupt_notice(&mut connection, conversation_id, queued_steering);
                publish_result(&mut subscribers, &result);
                let _ = reply.send(result.map(|(node_id, _)| node_id));
            }
            StoreCommand::InitializeModelSelection {
                conversation_id,
                provider,
                model,
                effort,
                plan,
                reply,
            } => {
                let result = initialize_model_selection(
                    &mut connection,
                    conversation_id,
                    &provider,
                    &model,
                    effort.as_deref(),
                    plan,
                );
                let _ = reply.send(result);
            }
            StoreCommand::InitializeModeModelSelection {
                conversation_id,
                mode,
                provider,
                model,
                effort,
                reply,
            } => {
                let result = initialize_mode_model_selection(
                    &mut connection,
                    conversation_id,
                    &mode,
                    &provider,
                    &model,
                    effort.as_deref(),
                );
                let _ = reply.send(result);
            }
            StoreCommand::LoadRetryContext { id, reply } => {
                let _ = reply.send(load_retry_context(&mut connection, id));
            }
            StoreCommand::ReconstructModelInput {
                conversation_id,
                node_id,
                resize_images,
                reply,
            } => {
                let _ = reply.send(reconstruct_model_input_with_image_resize(
                    &connection,
                    conversation_id,
                    node_id,
                    false,
                    resize_images,
                ));
            }
            StoreCommand::LoadCompactionSource {
                conversation_id,
                resize_images,
                exclude_active_tip,
                reply,
            } => {
                let _ = reply.send(load_compaction_source_with_image_resize(
                    &connection,
                    conversation_id,
                    resize_images,
                    exclude_active_tip,
                ));
            }
            StoreCommand::LoadRecentModels {
                provider,
                limit,
                reply,
            } => {
                let _ = reply.send(load_recent_models(&connection, &provider, limit));
            }
            StoreCommand::LoadBlob { id, reply } => {
                let result = connection
                    .query_row(
                        "SELECT bytes FROM blobs WHERE id = ?1",
                        [id.to_string()],
                        |row| row.get(0),
                    )
                    .map_err(RuntimeError::from);
                let _ = reply.send(result);
            }
            StoreCommand::StoreImageBlob {
                id,
                sha256,
                bytes,
                reply,
            } => {
                let result = store_image_blob(&mut connection, id, &sha256, bytes);
                let _ = reply.send(result);
            }
            StoreCommand::AppendUser {
                conversation_id,
                text,
                attachments,
                attachment_specs,
                deferred_attachment_paths,
                images,
                image_chips,
                reply,
            } => {
                let result = append_user_with_images(
                    &mut connection,
                    conversation_id,
                    &text,
                    &attachments,
                    &attachment_specs,
                    &deferred_attachment_paths,
                    &images,
                    &image_chips,
                );
                publish_result(&mut subscribers, &result);
                let _ = reply.send(result.map(|((node, turn), _)| (node, turn)));
            }
            StoreCommand::AppendCompactionSummary {
                conversation_id,
                parent_id,
                content,
                metadata,
                reply,
            } => {
                let result = append_compaction_summary(
                    &mut connection,
                    conversation_id,
                    parent_id,
                    &content,
                    &metadata,
                );
                publish_results(&mut subscribers, &result);
                let _ = reply.send(result.map(|(node, _)| node));
            }
            StoreCommand::SetModelSelection {
                conversation_id,
                provider,
                model,
                effort,
                pending,
                plan,
                reply,
            } => {
                let result = set_model_selection(
                    &mut connection,
                    conversation_id,
                    &provider,
                    &model,
                    effort.as_deref(),
                    pending,
                    plan,
                );
                publish_result(&mut subscribers, &result);
                let _ = reply.send(result.map(|_| ()));
            }
            StoreCommand::SetModeModelSelection {
                conversation_id,
                mode,
                provider,
                model,
                effort,
                pending,
                reply,
            } => {
                let result = set_mode_model_selection(
                    &mut connection,
                    conversation_id,
                    &mode,
                    &provider,
                    &model,
                    effort.as_deref(),
                    pending,
                );
                publish_result(&mut subscribers, &result);
                let _ = reply.send(result.map(|_| ()));
            }
            StoreCommand::StartAssistant {
                conversation_id,
                parent_id,
                turn_id,
                attempt,
                reply,
            } => {
                let result = start_assistant(
                    &mut connection,
                    conversation_id,
                    parent_id,
                    turn_id,
                    attempt.as_ref(),
                );
                publish_result(&mut subscribers, &result);
                let _ = reply.send(result.map(|(node, _)| node));
            }
            StoreCommand::AppendAssistantDelta {
                conversation_id,
                node_id,
                text,
                delta,
                reply,
            } => {
                let result = append_assistant_delta(
                    &mut connection,
                    conversation_id,
                    node_id,
                    &text,
                    &delta,
                );
                publish_result(&mut subscribers, &result);
                let _ = reply.send(result.map(|_| ()));
            }
            StoreCommand::StartPlan {
                conversation_id,
                node_id,
                reply,
            } => {
                let result = start_plan(&mut connection, conversation_id, node_id);
                publish_result(&mut subscribers, &result);
                let _ = reply.send(result.map(|_| ()));
            }
            StoreCommand::AppendPlanDelta {
                conversation_id,
                node_id,
                text,
                delta,
                reply,
            } => {
                let result =
                    append_plan_delta(&mut connection, conversation_id, node_id, &text, &delta);
                publish_result(&mut subscribers, &result);
                let _ = reply.send(result.map(|_| ()));
            }
            StoreCommand::AppendContextClear {
                conversation_id,
                mode,
                context_window,
                reply,
            } => {
                let result =
                    append_context_clear(&mut connection, conversation_id, &mode, context_window);
                publish_result(&mut subscribers, &result);
                let _ = reply.send(result.map(|(node, _)| node));
            }
            StoreCommand::AppendAcceptedPlan {
                conversation_id,
                plan,
                note,
                clear_context,
                compact_context,
                context_window,
                reply,
            } => {
                let result = append_accepted_plan(
                    &mut connection,
                    conversation_id,
                    &plan,
                    note.as_deref(),
                    clear_context,
                    compact_context,
                    context_window,
                );
                publish_results(&mut subscribers, &result);
                let _ = reply.send(result.map(|(ids, _)| ids));
            }
            StoreCommand::QueueInput { .. }
            | StoreCommand::QueueCompact { .. }
            | StoreCommand::QueueModeInput { .. }
            | StoreCommand::BeginEditingQueued { .. }
            | StoreCommand::ReplaceQueued { .. }
            | StoreCommand::ReplaceQueuedModeInput { .. }
            | StoreCommand::DeleteQueued { .. }
            | StoreCommand::PromoteQueued { .. }
            | StoreCommand::DispatchQueued { .. }
            | StoreCommand::PeekNextQueued { .. }
            | StoreCommand::PeekNextStartupQueued { .. }
            | StoreCommand::CompleteAssistant { .. }
            | StoreCommand::AppendToolResult { .. }
            | StoreCommand::AppendPermissionDecision { .. }
            | StoreCommand::CancelAssistant { .. }
            | StoreCommand::FailAssistant { .. } => unreachable!("handled above"),
        }
    }
}

fn store_image_blob(
    connection: &mut Connection,
    id: BlobId,
    sha256: &str,
    bytes: Vec<u8>,
) -> Result<BlobId, RuntimeError> {
    let transaction = connection.transaction()?;
    let existing = transaction
        .query_row(
            "SELECT blob_id FROM image_blob_hashes WHERE sha256 = ?1",
            [sha256],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let blob_id = if let Some(existing) = existing {
        existing.parse().map_err(|error| {
            RuntimeError::InvalidOption(format!("invalid stored image blob ID: {error}"))
        })?
    } else {
        transaction.execute(
            "INSERT INTO blobs (id, codec, bytes) VALUES (?1, 'png', ?2)",
            params![id.to_string(), bytes],
        )?;
        transaction.execute(
            "INSERT INTO image_blob_hashes (sha256, blob_id) VALUES (?1, ?2)",
            params![sha256, id.to_string()],
        )?;
        id
    };
    transaction.commit()?;
    Ok(blob_id)
}

fn prune_unreferenced_image_blobs(connection: &Connection) -> Result<usize, RuntimeError> {
    connection
        .execute(
            "DELETE FROM blobs
             WHERE codec = 'png'
               AND id NOT IN (
                 SELECT value FROM nodes, json_tree(nodes.content_json) WHERE key = 'blob_id'
                 UNION
                 SELECT value FROM queued_messages, json_tree(queued_messages.content_json) WHERE key = 'blob_id'
                 UNION
                 SELECT value FROM composer_history, json_tree(composer_history.images_json) WHERE key = 'blob_id'
               )",
            [],
        )
        .map_err(RuntimeError::from)
}

fn handle_tool_command(
    command: StoreCommand,
    connection: &mut Connection,
    subscribers: &mut Subscribers,
) -> Option<StoreCommand> {
    match command {
        StoreCommand::CompleteAssistant {
            conversation_id,
            node_id,
            metadata,
            tool_calls,
            reasoning,
            reply,
        } => {
            let result = complete_assistant_with_reasoning(
                connection,
                conversation_id,
                node_id,
                &metadata,
                tool_calls,
                reasoning,
            );
            publish_results(subscribers, &result);
            let _ = reply.send(result.map(|(calls, _)| calls));
        }
        StoreCommand::AppendToolResult {
            conversation_id,
            tool_call,
            output,
            is_error,
            duration_millis,
            transition,
            reply,
        } => {
            let result = append_tool_result(
                connection,
                conversation_id,
                &tool_call,
                &output,
                is_error,
                duration_millis,
                transition.as_ref(),
            );
            publish_results(subscribers, &result);
            let _ = reply.send(result.map(|(node_id, _)| node_id));
        }
        StoreCommand::AppendPermissionDecision {
            conversation_id,
            tool_call,
            audit,
            reply,
        } => {
            let result =
                append_permission_decision(connection, conversation_id, &tool_call, &audit);
            publish_result(subscribers, &result);
            let _ = reply.send(result.map(|(node_id, _)| node_id));
        }
        command => return Some(command),
    }
    None
}

fn handle_queue_command(
    command: StoreCommand,
    connection: &mut Connection,
    subscribers: &mut Subscribers,
) -> Option<StoreCommand> {
    match command {
        StoreCommand::QueueInput {
            conversation_id,
            text,
            target,
            attachments,
            images,
            image_chips,
            blocked_by_startup,
            require_subagent,
            reply,
        } => {
            let result = queue_input(
                connection,
                conversation_id,
                &text,
                target,
                &attachments,
                &images,
                &image_chips,
                blocked_by_startup,
                require_subagent,
            );
            publish_result(subscribers, &result);
            let _ = reply.send(result.map(|(message, _)| message));
        }
        StoreCommand::QueueCompact {
            conversation_id,
            target,
            instructions,
            reply,
        } => {
            let result =
                queue_compact(connection, conversation_id, target, instructions.as_deref());
            publish_result(subscribers, &result);
            let _ = reply.send(result.map(|(message, _)| message));
        }
        StoreCommand::QueueModeInput {
            conversation_id,
            mode,
            command_text,
            text,
            target,
            attachments,
            reply,
        } => {
            let result = queue_mode_input(
                connection,
                conversation_id,
                &mode,
                &command_text,
                &text,
                target,
                &attachments,
            );
            publish_result(subscribers, &result);
            let _ = reply.send(result.map(|(message, _)| message));
        }
        StoreCommand::BeginEditingQueued {
            conversation_id,
            id,
            reply,
        } => {
            let _ = reply.send(begin_editing_queued(connection, conversation_id, id));
        }
        StoreCommand::ReplaceQueued {
            conversation_id,
            id,
            text,
            target,
            attachments,
            images,
            image_chips,
            reply,
        } => {
            let result = replace_queued(
                connection,
                conversation_id,
                id,
                &text,
                target,
                &attachments,
                &images,
                &image_chips,
            );
            publish_result(subscribers, &result);
            let _ = reply.send(result.map(|(message, _)| message));
        }
        StoreCommand::ReplaceQueuedModeInput {
            conversation_id,
            id,
            mode,
            command_text,
            text,
            target,
            attachments,
            reply,
        } => {
            let result = replace_queued_mode_input(
                connection,
                conversation_id,
                id,
                &mode,
                &command_text,
                &text,
                target,
                &attachments,
            );
            publish_result(subscribers, &result);
            let _ = reply.send(result.map(|(message, _)| message));
        }
        StoreCommand::DeleteQueued {
            conversation_id,
            id,
            reply,
        } => {
            let result = delete_queued(connection, conversation_id, id);
            publish_result(subscribers, &result);
            let _ = reply.send(result.map(|_| ()));
        }
        StoreCommand::PromoteQueued {
            conversation_id,
            id,
            reply,
        } => {
            let result = promote_queued(connection, conversation_id, id);
            publish_result(subscribers, &result);
            let _ = reply.send(result.map(|(message, _)| message));
        }
        StoreCommand::DispatchQueued {
            conversation_id,
            expected,
            attachments,
            attachment_specs,
            deferred_attachment_paths,
            reply,
        } => {
            let result = dispatch_queued(
                connection,
                conversation_id,
                &expected,
                &attachments,
                &attachment_specs,
                &deferred_attachment_paths,
            );
            publish_results(subscribers, &result);
            let _ = reply.send(result.map(|(dispatched, _)| dispatched));
        }
        StoreCommand::PeekNextStartupQueued {
            conversation_id,
            reply,
        } => {
            let _ = reply.send(peek_next_startup_queued(connection, conversation_id));
        }
        StoreCommand::PeekNextQueued {
            conversation_id,
            target,
            reply,
        } => {
            let _ = reply.send(peek_next_queued(connection, conversation_id, target));
        }
        StoreCommand::CancelAssistant {
            conversation_id,
            node_id,
            reply,
        } => {
            let result =
                finish_assistant_with_status(connection, conversation_id, node_id, "cancelled");
            publish_result(subscribers, &result);
            let _ = reply.send(result.map(|_| ()));
        }
        StoreCommand::FailAssistant {
            conversation_id,
            node_id,
            status,
            rewind_to_parent,
            code,
            message,
            retryable,
            reply,
        } => {
            let result = fail_assistant(
                connection,
                conversation_id,
                node_id,
                status,
                rewind_to_parent,
                &code,
                &message,
                retryable,
            );
            publish_results(subscribers, &result);
            let _ = reply.send(result.map(|_| ()));
        }
        command => return Some(command),
    }
    None
}

fn publish_result<T>(
    subscribers: &mut Subscribers,
    result: &Result<(T, DurableEvent), RuntimeError>,
) {
    if let Ok((_, event)) = result {
        publish_event(subscribers, event);
    }
}

fn publish_event(subscribers: &mut Subscribers, event: &DurableEvent) {
    let _ = subscribers.live.send(event.clone());
}

fn publish_results<T>(subscribers: &mut Subscribers, result: &MultiEventResult<T>) {
    if let Ok((_, events)) = result {
        for event in events {
            let _ = subscribers.live.send(event.clone());
        }
    }
}

fn recover(connection: &mut Connection) -> Result<(), RuntimeError> {
    // Healthy conversations are overwhelmingly the common resume path. Avoid
    // opening and committing a write transaction (and running several UPDATE
    // statements) unless persisted state actually needs crash recovery.
    let recovery_needed = connection.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM conversations WHERE title_generation_state = 'generating'
             UNION ALL
             SELECT 1 FROM nodes
              WHERE kind = 'assistant_message' AND status = 'streaming'
             UNION ALL
             SELECT 1 FROM agent_runs WHERE status IN ('queued', 'running')
             UNION ALL
             SELECT 1 FROM background_terminals WHERE status IN ('running', 'terminating')
             UNION ALL
             SELECT 1 FROM queued_messages
              WHERE status IN ('editing', 'editing_blocked_by_startup')
             UNION ALL
             SELECT 1 FROM nodes
              WHERE kind = 'tool_result'
                AND json_extract(content_json, '$.name') = 'bash'
                AND json_extract(content_json, '$.output.status') IN ('running', 'terminating')
         )",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    if !recovery_needed {
        return Ok(());
    }
    let transaction = connection.transaction()?;
    transaction.execute(
        "UPDATE queued_messages
         SET status = CASE status
             WHEN 'editing_blocked_by_startup' THEN 'blocked_by_startup'
             ELSE 'queued'
         END
         WHERE status IN ('editing', 'editing_blocked_by_startup')",
        [],
    )?;
    transaction.execute(
        "UPDATE conversations SET title_generation_state = 'complete'
         WHERE title_generation_state = 'generating'",
        [],
    )?;
    let now = now();
    transaction.execute(
        "UPDATE nodes SET status = 'interrupted', completed_at = ?1
         WHERE kind = 'assistant_message' AND status = 'streaming'",
        [&now],
    )?;
    transaction.execute(
        "UPDATE conversations
         SET active_node_id = (
             SELECT parent_id FROM nodes WHERE nodes.id = conversations.active_node_id
         ), updated_at = ?1
         WHERE active_node_id IN (
             SELECT id FROM nodes WHERE kind = 'assistant_message' AND status = 'interrupted'
         )",
        [&now],
    )?;
    transaction.execute(
        "INSERT INTO agent_run_events (run_id, kind, content_json, created_at)
         SELECT id, 'interrupted', '{\"error\":\"Cagent stopped before this delegated run completed\"}', ?1
         FROM agent_runs WHERE status IN ('queued', 'running')",
        [&now],
    )?;
    transaction.execute(
        "UPDATE agent_runs SET status = 'interrupted', error = 'Cagent stopped before this delegated run completed', completed_at = ?1
         WHERE status IN ('queued', 'running')",
        [&now],
    )?;
    let orphaned_terminals = transaction.execute(
        "UPDATE background_terminals SET status = 'orphaned', completed_at = ?1
         WHERE status IN ('running', 'terminating')",
        [&now],
    )?;
    // A crash can occur between committing a Bash result node and its
    // terminal row. The node is the sole conversation-history authority.
    transaction.execute(
        "UPDATE nodes
         SET content_json = json_set(content_json, '$.output.status', 'orphaned')
         WHERE kind = 'tool_result'
           AND json_extract(content_json, '$.name') = 'bash'
           AND json_extract(content_json, '$.output.status') IN ('running', 'terminating')",
        [],
    )?;
    let _ = orphaned_terminals;
    transaction.commit()?;
    Ok(())
}

fn now() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .to_string()
}

#[cfg(test)]
mod tests;
