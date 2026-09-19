use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use std::time::{SystemTime, UNIX_EPOCH};

use cagent_agent::presentation::{
    EffortPickerFlow, ModelPickerRow, StatusLineColor, StatusLineConfig, StatusLineModule,
    StatusLineValues, effort_picker_flow, fallback_model_effort, filter_model_picker_rows,
    filter_provider_picker_rows, filter_settings_rows, permission_approval_choices,
    prioritize_current_model, project_provider_picker, uses_file_permission_choices,
    uses_list_permission_choices,
};
#[cfg(test)]
use cagent_agent::protocol::{DurableEventKind, NodeKind};
use cagent_agent::protocol::{
    InteractionRequest, InteractionRequestKind, QueueTarget, QueuedMessage, SessionAccess,
    SessionAction, SessionCommand,
};
use cagent_agent::runtime::{AgentRuntime, SessionHandle};
use cagent_agent::{
    QuestionAnswer, WorkspaceEntry as ListEntry, WorkspaceEntryKind as ListEntryKind,
};
use crossterm::event::{
    Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use futures_util::StreamExt;
use ratatui::DefaultTerminal;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use tracing::Instrument as _;
use unicode_width::UnicodeWidthStr as _;

mod agent_log;
use agent_log::AgentLogLoad;
mod commands;
#[allow(clippy::double_ended_iterator_last)] // Composer scans backward for the most recent trigger.
mod composer;
pub(crate) mod controller;
mod draft;
mod editor;
pub(crate) mod events;
pub(crate) mod file_tree;
pub(crate) mod helpers;
pub(crate) mod input;
mod input_syntax;
mod keybindings;
pub(crate) mod list;
mod multiline_input;
mod progress;
pub(crate) mod scroll;
mod single_line_input;
mod status_line;
mod supervised_work;
pub(crate) mod surface_control;
mod surface_expanded;
pub(crate) mod surfaces;
mod transcript;

#[derive(Clone, Debug)]
pub(crate) struct ReconnectStatus {
    pub(crate) next_attempt: u32,
    pub(crate) max_attempts: u32,
    pub(crate) reason: String,
    pub(crate) deadline: Instant,
}

pub(crate) use status_line::*;

use crate::render::transcript::{BlockLayoutCache, HistoryLayoutState, WrappedLayout};
#[cfg(test)]
use crate::render::transcript::{append_visible_rows, assistant_markdown_rows};
#[cfg(test)]
use crate::render::wrap_log_line;
use crate::render::{
    status_line_background_hint_at, status_line_module_at, terminal_safe, welcome_lines,
};
#[allow(unused_imports)]
pub(crate) use composer::{
    cursor_at_end, next_word_boundary, previous_grapheme, previous_word_boundary,
};
pub(crate) use controller::{StartupSurface, run};
use editor::{ComposerEditor, edit_draft_in_system_editor};
#[cfg(test)]
use helpers::help_command_rows;
#[cfg(test)]
use helpers::history_tree_lines;
#[cfg(test)]
use helpers::menu_navigation_code;
#[allow(clippy::wildcard_imports)]
use helpers::{
    adjust_chip_ranges, configured_help_command_rows, copy_dialect, copy_to_clipboard,
    is_escape_key, is_paste_key, is_word_backspace, is_word_left, is_word_right,
    observer_help_command_rows, open_browser, permission_rule_edit_details,
    permission_submission_choice, read_from_clipboard, read_image_from_clipboard,
    schedule_completion, split_command, tree_initial_selection, trim_outer_blank_lines,
};
use list::{ListAction, ListMode, ListState};
use multiline_input::is_control_enter;
pub(crate) use multiline_input::{
    InteractionNote, InteractionNoteOutcome, InteractionNoteView, MultilineInput,
};
use progress::{ProgressGuard, wait_for_keepalive};
use scroll::{ScrollViewAction, ScrollViewState, move_scroll_offset};
use single_line_input::SingleLineInput;
use supervised_work::SupervisedWorkState;
#[cfg(test)]
use surfaces::{surface_lines, surface_lines_with_viewport, surface_status};
pub(crate) use transcript::{TranscriptBlock, local_transcript_block};
#[cfg(test)]
pub(crate) use transcript::{test_edits, test_lines, test_tool_groups};
pub(crate) const COMPOSER_STYLE: Style = Style::new().bg(Color::Indexed(236));
pub(crate) const DIM_STYLE: Style = Style::new().add_modifier(Modifier::DIM);
pub(crate) const ACCENT_STYLE: Style = Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD);
pub(crate) const BASH_STYLE: Style = Style::new()
    .fg(Color::Indexed(215))
    .add_modifier(Modifier::BOLD);
pub(crate) const CHIP_STYLE: Style = Style::new().fg(Color::Cyan);
pub(crate) const COMMAND_STYLE: Style = Style::new().fg(Color::Cyan);
pub(crate) const ENABLED_STYLE: Style = Style::new().fg(Color::Green);
pub(crate) const ERROR_STYLE: Style = Style::new().fg(Color::LightRed);
pub(crate) const NOTICE_STYLE: Style = Style::new().fg(Color::Yellow);
pub(crate) const SELECTED_STYLE: Style = Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD);
pub(crate) const MENU_TITLE_STYLE: Style =
    Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD);
pub(crate) const MENU_DETAIL_STYLE: Style = Style::new().add_modifier(Modifier::DIM);
pub(crate) const SEARCH_CURSOR_STYLE: Style = Style::new().fg(Color::Black).bg(Color::White);
pub(crate) const USER_STYLE: Style = Style::new().bg(Color::Indexed(236));
pub(crate) const NOTICE_DURATION: Duration = Duration::from_secs(1);
const EXIT_NOTICE: &str = "press Ctrl+C again to exit";
const WORKING_ANIMATION_INTERVAL: Duration = Duration::from_millis(75);
const REQUESTED_INPUT_TITLE_FRAME_TICKS: usize = 2;
const INTERACTION_INPUT_SETTLE_DELAY: Duration = Duration::from_millis(750);
pub(crate) const WORKING_SHIMMER_PERIOD: Duration = Duration::from_secs(2);
/// Logical items shown between the two permanent list indicator rows.
pub(crate) const VISIBLE_MENU_ITEMS: usize = 8;

fn mcp_server_menu_item_count(server: &cagent_agent::mcp::McpEffectiveServer) -> usize {
    6 + usize::from(matches!(
        &server.definition.transport,
        cagent_agent::mcp::McpTransportConfig::StreamableHttp { .. }
    )) + usize::from(server.definition.package.is_some())
}

pub(crate) fn settings_item_capacity(query: &str) -> usize {
    VISIBLE_MENU_ITEMS + usize::from(!query.is_empty())
}
/// One-line Help entries shown between the two permanent indicator rows.
pub(crate) const VISIBLE_HELP_ITEMS: usize = 12;
pub(crate) const COMPOSER_PLACEHOLDER: &str = "Ask Cagent anything";
pub(crate) const READ_ONLY_COMPOSER_PLACEHOLDER: &str = "Viewing conversation in read-only mode";

pub(crate) fn picker_opened_at_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

pub(crate) fn safe_terminal_title(title: &str) -> String {
    let title = title
        .chars()
        .filter(|character| !character.is_control())
        .collect::<String>();
    let title = title.trim();
    if title.is_empty() {
        "Cagent".into()
    } else {
        title.into()
    }
}

#[allow(dead_code)]
pub(crate) fn session_terminal_title(
    title: Option<&str>,
    waiting_for_user: bool,
    requested_input_title: &cagent_agent::config::RequestedInputTitle,
) -> String {
    session_terminal_title_with_progress(
        title,
        waiting_for_user,
        requested_input_title,
        &cagent_agent::config::ProgressMode::default(),
        false,
        0,
    )
}

pub(crate) fn session_terminal_title_with_progress(
    title: Option<&str>,
    waiting_for_user: bool,
    requested_input_title: &cagent_agent::config::RequestedInputTitle,
    progress_mode: &cagent_agent::config::ProgressMode,
    progress_active: bool,
    progress_frame: usize,
) -> String {
    let title = title
        .filter(|title| !title.trim().is_empty())
        .unwrap_or("Cagent");
    if waiting_for_user {
        requested_input_title
            .frame(progress_frame / REQUESTED_INPUT_TITLE_FRAME_TICKS)
            .filter(|frame| !frame.is_empty())
            .map_or_else(|| title.to_owned(), |frame| format!("{frame} {title}"))
    } else if progress_active && let Some(frame) = progress_mode.frame(progress_frame) {
        format!("{frame} {title}")
    } else {
        title.to_owned()
    }
}

pub(crate) fn set_terminal_title(title: &str) -> std::io::Result<()> {
    crossterm::execute!(
        std::io::stdout(),
        crossterm::terminal::SetTitle(safe_terminal_title(title))
    )
}

pub(crate) trait SelectionInput {
    fn into_selection(self) -> Option<(String, String, Option<String>)>;
}

impl SelectionInput for Option<(String, String, Option<String>)> {
    fn into_selection(self) -> Option<(String, String, Option<String>)> {
        self
    }
}

impl SelectionInput for (String, String, Option<String>) {
    fn into_selection(self) -> Option<(String, String, Option<String>)> {
        Some(self)
    }
}

const SLASH_COMMANDS: [SlashCommand; 35] = [
    SlashCommand {
        name: "/providers",
        alias: None,
        argument_hint: None,
        insert_argument_hint: false,
        observer_safe: true,
        description: "choose and configure providers",
    },
    SlashCommand {
        name: "/model",
        alias: None,
        argument_hint: Some("[query]"),
        insert_argument_hint: false,
        observer_safe: false,
        description: "choose a model and effort",
    },
    SlashCommand {
        name: "/fast",
        alias: None,
        argument_hint: Some("[on|off]"),
        insert_argument_hint: false,
        observer_safe: false,
        description: "toggle the model's Fast service tier",
    },
    SlashCommand {
        name: "/dir",
        alias: None,
        argument_hint: Some("[path]"),
        insert_argument_hint: false,
        observer_safe: false,
        description: "show or change the working directory",
    },
    SlashCommand {
        name: "/agent",
        alias: None,
        argument_hint: Some("[name]"),
        insert_argument_hint: false,
        observer_safe: false,
        description: "choose an agent profile",
    },
    SlashCommand {
        name: "/skills",
        alias: None,
        argument_hint: None,
        insert_argument_hint: false,
        observer_safe: false,
        description: "manage project and global skills",
    },
    SlashCommand {
        name: "/mode",
        alias: None,
        argument_hint: Some("[name] [message]"),
        insert_argument_hint: false,
        observer_safe: false,
        description: "choose a permission and behavior mode",
    },
    SlashCommand {
        name: "/permissions",
        alias: None,
        argument_hint: Some("[simulate <bash command>]"),
        insert_argument_hint: false,
        observer_safe: false,
        description: "manage permissions or simulate a Bash decision",
    },
    SlashCommand {
        name: "/spawn",
        alias: None,
        argument_hint: Some("<message>"),
        insert_argument_hint: false,
        observer_safe: false,
        description: "send a message that requires delegated work",
    },
    SlashCommand {
        name: "/search",
        alias: None,
        argument_hint: Some("[query]"),
        insert_argument_hint: false,
        observer_safe: true,
        description: "configure web search providers or send a search query",
    },
    SlashCommand {
        name: "/statusline",
        alias: None,
        argument_hint: None,
        insert_argument_hint: false,
        observer_safe: true,
        description: "customize statusline modules and colors",
    },
    SlashCommand {
        name: "/settings",
        alias: None,
        argument_hint: None,
        insert_argument_hint: false,
        observer_safe: false,
        description: "edit global UI, limits, mode, and keybindings",
    },
    SlashCommand {
        name: "/usage",
        alias: None,
        argument_hint: None,
        insert_argument_hint: false,
        observer_safe: false,
        description: "view and reset global usage and cost",
    },
    SlashCommand {
        name: "/mcp",
        alias: None,
        argument_hint: None,
        insert_argument_hint: false,
        observer_safe: true,
        description: "manage external MCP servers",
    },
    SlashCommand {
        name: "/help",
        alias: None,
        argument_hint: None,
        insert_argument_hint: false,
        observer_safe: true,
        description: "show commands and keybindings",
    },
    SlashCommand {
        name: "/background",
        alias: None,
        argument_hint: None,
        insert_argument_hint: false,
        observer_safe: true,
        description: "show active and past background work",
    },
    SlashCommand {
        name: "/copy",
        alias: None,
        argument_hint: Some("[slack|discord]"),
        insert_argument_hint: false,
        observer_safe: true,
        description: "copy the last response for Markdown or chat",
    },
    SlashCommand {
        name: "/diff",
        alias: None,
        argument_hint: Some("[conversation|git|clear]"),
        insert_argument_hint: false,
        observer_safe: true,
        description: "view conversation or repository changes, or clear tracking",
    },
    SlashCommand {
        name: "/new",
        alias: None,
        argument_hint: Some("[title]"),
        insert_argument_hint: false,
        observer_safe: true,
        description: "start a new conversation",
    },
    SlashCommand {
        name: "/tree",
        alias: None,
        argument_hint: None,
        insert_argument_hint: false,
        observer_safe: false,
        description: "show the durable conversation tree",
    },
    SlashCommand {
        name: "/files",
        alias: None,
        argument_hint: None,
        insert_argument_hint: false,
        observer_safe: true,
        description: "toggle the workspace file tree",
    },
    SlashCommand {
        name: "/worktree",
        alias: None,
        argument_hint: Some("[worktree] [base]"),
        insert_argument_hint: false,
        observer_safe: false,
        description: "enter or create a repository worktree",
    },
    SlashCommand {
        name: "/fork",
        alias: None,
        argument_hint: None,
        insert_argument_hint: false,
        observer_safe: false,
        description: "select a message to fork from",
    },
    SlashCommand {
        name: "/resume",
        alias: None,
        argument_hint: Some("[conversation-id]"),
        insert_argument_hint: false,
        observer_safe: true,
        description: "resume a durable conversation",
    },
    SlashCommand {
        name: "/continue",
        alias: None,
        argument_hint: None,
        insert_argument_hint: false,
        observer_safe: false,
        description: "resume the latest workspace conversation",
    },
    SlashCommand {
        name: "/rename",
        alias: None,
        argument_hint: Some("[name]"),
        insert_argument_hint: false,
        observer_safe: false,
        description: "rename the current conversation",
    },
    SlashCommand {
        name: "/archive",
        alias: None,
        argument_hint: None,
        insert_argument_hint: false,
        observer_safe: false,
        description: "archive this conversation and exit",
    },
    SlashCommand {
        name: "/delete",
        alias: None,
        argument_hint: None,
        insert_argument_hint: false,
        observer_safe: false,
        description: "permanently delete this conversation and exit",
    },
    SlashCommand {
        name: "/retry",
        alias: None,
        argument_hint: None,
        insert_argument_hint: false,
        observer_safe: false,
        description: "wake the model from safe history",
    },
    SlashCommand {
        name: "/compact",
        alias: None,
        argument_hint: Some("[instructions]"),
        insert_argument_hint: false,
        observer_safe: false,
        description: "compact the active branch context with optional instructions",
    },
    SlashCommand {
        name: "/recap",
        alias: None,
        argument_hint: None,
        insert_argument_hint: false,
        observer_safe: false,
        description: "generate a recap of the recent conversation",
    },
    SlashCommand {
        name: "/context",
        alias: None,
        argument_hint: Some("save"),
        insert_argument_hint: true,
        observer_safe: false,
        description: "save the latest model context request as JSON",
    },
    SlashCommand {
        name: "/cleanup",
        alias: None,
        argument_hint: None,
        insert_argument_hint: false,
        observer_safe: false,
        description: "clean up conversations using the configured retention policy",
    },
    SlashCommand {
        name: "/reload",
        alias: None,
        argument_hint: None,
        insert_argument_hint: false,
        observer_safe: false,
        description: "reload configuration and refresh model catalogs",
    },
    SlashCommand {
        name: "/quit",
        alias: None,
        argument_hint: None,
        insert_argument_hint: false,
        observer_safe: true,
        description: "exit Cagent",
    },
];

#[derive(Clone, Copy)]
pub(crate) struct SlashCommand {
    pub(crate) name: &'static str,
    pub(crate) alias: Option<&'static str>,
    pub(crate) argument_hint: Option<&'static str>,
    insert_argument_hint: bool,
    pub(crate) observer_safe: bool,
    pub(crate) description: &'static str,
}

#[derive(Clone)]
pub(crate) struct SlashSuggestion {
    pub(crate) name: String,
    pub(crate) alias: Option<&'static str>,
    pub(crate) argument_hint: Option<&'static str>,
    insert_argument_hint: bool,
    pub(crate) description: String,
}

impl From<SlashCommand> for SlashSuggestion {
    fn from(command: SlashCommand) -> Self {
        Self {
            name: command.name.into(),
            alias: command.alias,
            argument_hint: command.argument_hint,
            insert_argument_hint: command.insert_argument_hint,
            description: command.description.into(),
        }
    }
}

/// A side effect requested by input handling and performed by the runtime loop.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum AppAction {
    NewSession(Option<String>),
    Resume(Option<cagent_agent::protocol::ConversationId>),
    Archive,
    Delete,
    Quit,
    EditDraft,
    LaunchPath {
        argv_candidates: Vec<Vec<String>>,
        mode: cagent_agent::config::UiEditorMode,
        reload_startup_resources: bool,
    },
    RefreshFork,
    HardFork(cagent_agent::protocol::NodeId),
    LoadHistory(TreePurpose),
    SwitchWorkspace(PathBuf),
    Submit(DeferredSubmission),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DeferredSubmission {
    pub(crate) command: SessionCommand,
    pub(crate) draft: cagent_agent::protocol::UserDraft,
    pub(crate) optimistic_queue_id: Option<cagent_agent::protocol::QueuedMessageId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OnboardingOutcome {
    Dismissed,
    Completed,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum HelpTab {
    #[default]
    Commands,
    Keybindings,
}

impl HelpTab {
    const fn previous(self) -> Self {
        match self {
            Self::Commands => Self::Keybindings,
            Self::Keybindings => Self::Commands,
        }
    }

    const fn next(self) -> Self {
        self.previous()
    }
}

#[derive(Clone)]
pub(crate) struct ChipRange {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) expanded: bool,
}

impl ChipRange {
    pub(crate) fn new(start: usize, end: usize) -> Self {
        Self {
            start,
            end,
            expanded: false,
        }
    }
}

#[derive(Clone)]
pub(crate) struct AttachmentChip {
    pub(crate) spec: cagent_agent::protocol::AttachmentSpec,
    pub(crate) kind: ListEntryKind,
    pub(crate) range: ChipRange,
}

#[derive(Clone)]
pub(crate) struct PasteChip {
    pub(crate) range: ChipRange,
    pub(crate) lines: usize,
    pub(crate) bytes: usize,
}

#[derive(Clone)]
pub(crate) struct ImageChip {
    pub(crate) image: cagent_agent::protocol::ImageAttachment,
    pub(crate) range: ChipRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HistoryEntry {
    kind: cagent_agent::protocol::ComposerInputKind,
    text: String,
    attachment_specs: Vec<cagent_agent::protocol::AttachmentSpec>,
    images: Vec<cagent_agent::protocol::ImageAttachment>,
    image_chips: Vec<cagent_agent::protocol::ImageChipRange>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ComposerMode {
    #[default]
    Prompt,
    Bash,
}

#[derive(Clone)]
pub(crate) struct DraftSnapshot {
    mode: ComposerMode,
    draft: String,
    cursor: usize,
    attachments: Vec<AttachmentChip>,
    pastes: Vec<PasteChip>,
    images: Vec<ImageChip>,
}

#[derive(Clone)]
pub(crate) struct AttachmentCompletion {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) rows: Vec<ListEntry>,
    pub(crate) list: ListState,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CompletionRequest {
    start: usize,
    end: usize,
    raw: String,
    prefix: String,
}

struct PathCompletionState {
    start: usize,
    session: cagent_agent::runtime::PathCompletionSession,
    warm_task: tokio::task::JoinHandle<()>,
}

impl PathCompletionState {
    fn new(session: &cagent_agent::runtime::SessionHandle, start: usize) -> Self {
        let path_session = session.start_path_completion();
        let warming = path_session.clone();
        let warm_task = tokio::spawn(
            async move {
                let _ = warming.warm().await;
            }
            .in_current_span(),
        );
        Self {
            start,
            session: path_session,
            warm_task,
        }
    }
}

impl Drop for PathCompletionState {
    fn drop(&mut self) {
        self.warm_task.abort();
    }
}

struct CompletionResult {
    request: CompletionRequest,
    rows: Result<Vec<ListEntry>, cagent_agent::runtime::RuntimeError>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AgentWizardStep {
    Name,
    Description,
    Parent,
    Prompt,
    Availability,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SkillWizardStep {
    Scope,
    Name,
    Description,
    Content,
}

impl AgentWizardStep {
    pub(crate) fn next(self) -> Self {
        match self {
            Self::Name => Self::Description,
            Self::Description => Self::Parent,
            Self::Parent => Self::Prompt,
            Self::Prompt | Self::Availability => Self::Availability,
        }
    }

    pub(crate) fn index(self) -> usize {
        match self {
            Self::Name => 0,
            Self::Description => 1,
            Self::Parent => 2,
            Self::Prompt => 3,
            Self::Availability => 4,
        }
    }

    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Description => "description",
            Self::Parent => "parent",
            Self::Prompt => "prompt",
            Self::Availability => "availability",
        }
    }

    pub(crate) fn input_cursor(self, name: &str, description: &str) -> usize {
        match self {
            Self::Name => cursor_at_end(name),
            Self::Description => cursor_at_end(description),
            Self::Parent | Self::Prompt | Self::Availability => 0,
        }
    }
}

fn interaction_question_count(kind: &InteractionRequestKind) -> usize {
    match kind {
        InteractionRequestKind::Question { questions } => questions.len(),
        InteractionRequestKind::PermissionApproval { .. }
        | InteractionRequestKind::PlanCompletion { .. } => 0,
    }
}

pub(crate) enum Surface {
    Onboarding,
    Question {
        request: Box<InteractionRequest>,
        question_index: usize,
        option_index: usize,
        answers: Vec<QuestionAnswer>,
        answered: Vec<bool>,
        editing_note: bool,
        note_cursor: usize,
    },
    Permission {
        request: Box<InteractionRequest>,
        selected: usize,
        scope: cagent_agent::permissions::PermissionScope,
        diff_scroll: usize,
        denial_note: String,
        editing_note: bool,
        note_cursor: usize,
    },
    PermissionRuleEdit {
        request: Box<InteractionRequest>,
        rule: cagent_agent::permissions::PermissionRule,
        pattern: String,
        cursor: usize,
        scope: cagent_agent::permissions::PermissionScope,
    },
    Permissions {
        rows: Vec<PermissionMenuRow>,
        list: ListState,
    },
    PersistentPermissionEdit {
        rule: cagent_agent::permissions::PermissionRule,
        pattern: String,
        cursor: usize,
        scope: cagent_agent::permissions::PermissionScope,
        selected: usize,
    },
    PlanCompletion {
        request: Box<InteractionRequest>,
        selected: usize,
        note: String,
        editing_note: bool,
        note_cursor: usize,
        implementation_mode_index: usize,
        implementation_mode_colors: Vec<StatusLineColor>,
        implementation_model: String,
        /// Current active-branch context use, kept in sync with the statusline.
        context_percent: Option<u64>,
    },
    Rename {
        title: String,
        cursor: usize,
    },
    WebSearchPicker {
        rows: Vec<cagent_agent::web_search::WebSearchProviderStatus>,
        list: ListState,
        query: String,
        query_cursor: usize,
    },
    Worktrees {
        rows: Vec<cagent_agent::WorktreeInfo>,
        list: ListState,
    },
    WorktreeNew {
        name: String,
        base: String,
        cursor: usize,
        editing_base: bool,
    },
    WebSearchSetup {
        provider: cagent_agent::web_search::WebSearchProvider,
        value: String,
        cursor: usize,
    },
    McpServers {
        rows: Vec<cagent_agent::mcp::McpEffectiveServer>,
        list: ListState,
    },
    McpCatalog {
        rows: Vec<cagent_agent::mcp::McpPackage>,
        list: ListState,
    },
    McpServer {
        server: cagent_agent::mcp::McpEffectiveServer,
        selected: usize,
        oauth_connected: bool,
    },
    McpOAuth {
        server: String,
        label: String,
        attempt: cagent_agent::mcp::McpOAuthAttempt,
    },
    McpPackageSetup {
        draft: Box<McpPackageDraft>,
        selected: usize,
    },
    McpPackageValueEdit {
        draft: Box<McpPackageDraft>,
        field: McpPackageField,
        value: String,
        cursor: usize,
    },
    McpTools {
        server: String,
        tools: Vec<cagent_agent::mcp::McpToolSummary>,
        list: ListState,
        loading: bool,
        failure: Option<String>,
    },
    McpRemoveConfirm {
        server: cagent_agent::mcp::McpEffectiveServer,
        selected: usize,
    },
    McpAddScope {
        selected: usize,
    },
    McpTransport {
        location: cagent_agent::mcp::McpLocation,
        selected: usize,
    },
    McpForm {
        draft: Box<McpFormDraft>,
        list: ListState,
    },
    McpFieldEdit {
        draft: Box<McpFormDraft>,
        field: McpFormField,
        value: String,
        cursor: usize,
    },
    McpJsonEdit {
        location: cagent_agent::mcp::McpLocation,
        separate_name: Option<String>,
        editor: MultilineInput,
    },
    McpMutationPreview {
        preview: cagent_agent::mcp::McpMutationPreview,
        list: ListState,
        details: ScrollViewState,
    },
    ProvidersRequired,
    ModelRequired,
    ModelsUnavailable,
    ProviderSetup {
        provider_id: String,
        provider: String,
        instructions: String,
        credential_environment_variable: Option<String>,
        auth_challenge: Option<cagent_agent::provider::AuthChallenge>,
        managed_auth: bool,
        api_key_auth: bool,
        api_key: String,
        api_key_cursor: usize,
        auth_flows: Vec<cagent_agent::provider::AuthFlow>,
        authenticated: bool,
    },
    Providers {
        rows: Vec<cagent_agent::presentation::ProviderPickerRow>,
        list: ListState,
        query: String,
        query_cursor: usize,
        model_configuration_follows: bool,
    },
    Models {
        rows: Vec<ModelPickerRow>,
        list: ListState,
        query: String,
        query_cursor: usize,
        selection_target: ModelSelectionTarget,
    },
    Effort {
        provider: String,
        model: String,
        rows: Vec<String>,
        reasoning_control: Option<cagent_agent::provider::ReasoningControl>,
        list: ListState,
        selection_target: ModelSelectionTarget,
    },
    Profiles {
        kind: ProfileKind,
        rows: Vec<(String, String, bool)>,
        list: ListState,
    },
    /// Small, staged editor used to create a profile without forcing callers
    /// to understand the raw TOML shape.
    AgentWizard {
        name: String,
        description: String,
        parent: String,
        parents: Vec<String>,
        parent_list: ListState,
        prompt: MultilineInput,
        availability: usize,
        step: AgentWizardStep,
        cursor: usize,
        editing: bool,
        original_name: Option<String>,
    },
    AgentEdit {
        name: String,
        list: ListState,
    },
    Skills {
        rows: Vec<cagent_agent::config::SkillMetadata>,
        list: ListState,
    },
    SkillDeleteConfirm {
        skill: cagent_agent::config::SkillMetadata,
        selected: usize,
    },
    SkillWizard {
        project: bool,
        scope_list: ListState,
        name: SingleLineInput,
        description: SingleLineInput,
        content: MultilineInput,
        step: SkillWizardStep,
    },
    StatusLine {
        config: StatusLineConfig,
        rows: Vec<StatusLineModule>,
        list: ListState,
        mode: StatusLineEditorMode,
        preview: StatusLineValues,
    },
    Settings {
        rows: Vec<cagent_agent::config::SettingRow>,
        section: cagent_agent::config::SettingSection,
        list: ListState,
        query: String,
        query_cursor: usize,
    },
    Usage {
        overview: cagent_agent::UsageOverview,
        list: ListState,
    },
    UsageBreakdown {
        kind: UsageBreakdownKind,
        rows: Vec<cagent_agent::UsageBreakdown>,
        list: ListState,
    },
    UsageResetConfirm {
        selected: usize,
    },
    SettingChoices {
        setting: cagent_agent::config::SettingDefinition,
        choices: Vec<(String, String)>,
        list: ListState,
        custom_value: Option<String>,
    },
    SettingInput {
        setting: cagent_agent::config::SettingDefinition,
        value: String,
        cursor: usize,
    },
    SupervisedWork {
        rows: Vec<cagent_agent::presentation::SupervisedWork>,
        list: ListState,
        show_past: bool,
    },
    KillSupervisedWork {
        target: cagent_agent::runtime::SupervisedWorkTarget,
        selected: usize,
    },
    Expanded {
        view: ExpandedView,
        scroll: usize,
        viewport_rows: usize,
    },
    Paths {
        rows: Vec<ListEntry>,
        list: ListState,
        token_start: usize,
    },
    Help {
        tab: HelpTab,
        command_rows: Vec<(String, String)>,
        key_rows: Vec<(String, String)>,
        list: ScrollViewState,
    },
    HistoryTree {
        rows: Vec<cagent_agent::presentation::HistoryRow>,
        list: ListState,
        purpose: TreePurpose,
        loading: bool,
        revision: Option<cagent_agent::protocol::EventCursor>,
        query: String,
        query_cursor: usize,
        opened_at_millis: u128,
    },
    Conversations {
        rows: Vec<cagent_agent::protocol::ConversationSummary>,
        list: ListState,
        query: String,
        query_cursor: usize,
        opened_at_millis: u128,
        include_archived: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UsageBreakdownKind {
    Project,
    Model,
}

impl Surface {
    pub(crate) fn interaction_note(&self) -> Option<InteractionNoteView<'_>> {
        match self {
            Self::Question {
                question_index,
                answers,
                editing_note,
                note_cursor,
                ..
            } => Some(InteractionNoteView {
                text: answers
                    .get(*question_index)?
                    .note
                    .as_deref()
                    .unwrap_or_default(),
                active: *editing_note,
                cursor: *note_cursor,
            }),
            Self::Permission {
                denial_note,
                editing_note,
                note_cursor,
                ..
            } => Some(InteractionNoteView {
                text: denial_note,
                active: *editing_note,
                cursor: *note_cursor,
            }),
            Self::PlanCompletion {
                note,
                editing_note,
                note_cursor,
                ..
            } => Some(InteractionNoteView {
                text: note,
                active: *editing_note,
                cursor: *note_cursor,
            }),
            _ => None,
        }
    }

    /// Returns the note editor for interaction surfaces that currently expose
    /// one. Question notes are stored directly in their durable answer draft;
    /// the adapter keeps that protocol detail out of input handling.
    pub(crate) fn interaction_note_mut(&mut self) -> Option<InteractionNote<'_>> {
        match self {
            Self::Question {
                question_index,
                answers,
                editing_note,
                note_cursor,
                ..
            } => InteractionNote::optional(
                &mut answers.get_mut(*question_index)?.note,
                editing_note,
                note_cursor,
            )
            .into(),
            Self::Permission {
                denial_note,
                editing_note,
                note_cursor,
                ..
            } => Some(InteractionNote::required(
                denial_note,
                editing_note,
                note_cursor,
            )),
            Self::PlanCompletion {
                note,
                editing_note,
                note_cursor,
                ..
            } => Some(InteractionNote::required(note, editing_note, note_cursor)),
            _ => None,
        }
    }

    pub(crate) fn is_editing_interaction_note(&self) -> bool {
        self.interaction_note().is_some_and(|note| note.active)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PermissionMenuRow {
    pub(crate) scope: cagent_agent::permissions::PermissionScope,
    pub(crate) rule: cagent_agent::permissions::PermissionRule,
}

pub(crate) enum ExpandedView {
    Mcp {
        call: std::sync::Arc<cagent_agent::presentation::McpCall>,
    },
    Compaction {
        summary: String,
    },
    RepositoryDiff {
        diff: cagent_agent::tools::SemanticDiff,
        title: &'static str,
    },
    Terminal {
        terminal_id: Option<cagent_agent::tools::TerminalId>,
        command: String,
        output: String,
        ansi_output: String,
        completion: Option<String>,
        started_at: Option<String>,
        completed_at: Option<String>,
    },
    WebFetch {
        url: String,
        redirected_url: Option<String>,
        format: cagent_agent::WebFetchFormat,
        output: String,
    },
    WebSearch {
        provider: String,
        query: String,
        results: Vec<cagent_agent::web_search::WebSearchResult>,
    },
    AgentLog {
        run: Box<cagent_agent::protocol::AgentRun>,
        streaming: String,
        terminals: Vec<cagent_agent::tools::TerminalSnapshot>,
        expanded_explorations: std::collections::HashSet<usize>,
        expanded_activity_runs: std::collections::HashSet<usize>,
        collapse_tool_activity: bool,
        max_scroll: Cell<Option<usize>>,
    },
    File {
        view: cagent_agent::presentation::FileView,
    },
    Diff {
        view: cagent_agent::presentation::FullFileDiffView,
    },
    Image {
        metadata: cagent_agent::protocol::ImageAttachment,
        png: Vec<u8>,
    },
    Directory {
        browser: file_tree::DirectoryBrowserState,
    },
}

impl ExpandedView {
    pub(super) fn fills_bottom_row(&self) -> bool {
        matches!(
            self,
            Self::Compaction { .. }
                | Self::Mcp { .. }
                | Self::RepositoryDiff { .. }
                | Self::WebFetch { .. }
                | Self::WebSearch { .. }
                | Self::AgentLog { .. }
                | Self::File { .. }
                | Self::Diff { .. }
                | Self::Directory { .. }
        )
    }
}

#[derive(Clone)]
pub(crate) struct McpFormDraft {
    pub(crate) original: Option<(cagent_agent::mcp::McpLocation, String)>,
    pub(crate) location: cagent_agent::mcp::McpLocation,
    pub(crate) name: String,
    pub(crate) definition: cagent_agent::mcp::McpServerDefinition,
}

#[derive(Clone)]
pub(crate) struct McpPackageDraft {
    pub(crate) package: cagent_agent::mcp::McpPackage,
    pub(crate) server_name: String,
    pub(crate) location: cagent_agent::mcp::McpLocation,
    pub(crate) parameters: std::collections::BTreeMap<String, cagent_agent::mcp::McpParameterValue>,
    pub(crate) secret_statuses:
        std::collections::BTreeMap<String, cagent_agent::mcp::McpSecretStatus>,
    pub(crate) secret_updates: std::collections::BTreeMap<String, Option<String>>,
    pub(crate) installed: bool,
}

#[derive(Clone)]
pub(crate) enum McpPackageField {
    Parameter(String),
    Secret(String),
}

#[derive(Clone, Copy)]
pub(crate) enum McpFormField {
    Name,
    Agents,
    StartupTimeout,
    RequestTimeout,
    ReadOnlyTools,
    Command,
    Arguments,
    Cwd,
    Environment,
    RemovedEnvironment,
    Url,
    Headers,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum TreePurpose {
    Browse,
    Fork,
}

pub(crate) struct HistoryPreviewState {
    pub(crate) latest_live: cagent_agent::protocol::SessionSnapshot,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum ProfileKind {
    Agent,
    Mode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TranscriptScrollbarLayout {
    pub(crate) area: ratatui::layout::Rect,
    pub(crate) maximum: usize,
    pub(crate) thumb_start: usize,
    pub(crate) thumb_len: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TranscriptScrollbarDrag {
    pub(crate) grab_offset: usize,
    pub(crate) prefetch_seen: bool,
}

impl TranscriptScrollbarLayout {
    pub(crate) fn new(
        area: ratatui::layout::Rect,
        content_rows: usize,
        offset: usize,
    ) -> Option<Self> {
        let viewport = usize::from(area.height);
        let maximum = content_rows.saturating_sub(viewport);
        if area.width == 0 || viewport == 0 || maximum == 0 {
            return None;
        }
        let thumb_len = viewport
            .saturating_mul(viewport)
            .saturating_add(content_rows / 2)
            .checked_div(content_rows)
            .unwrap_or_default()
            .clamp(1, viewport);
        let travel = viewport.saturating_sub(thumb_len);
        let thumb_start = offset
            .min(maximum)
            .saturating_mul(travel)
            .saturating_add(maximum / 2)
            / maximum;
        Some(Self {
            area,
            maximum,
            thumb_start,
            thumb_len,
        })
    }

    pub(crate) fn offset_for_thumb_start(self, thumb_start: usize) -> usize {
        let travel = usize::from(self.area.height).saturating_sub(self.thumb_len);
        if travel == 0 {
            return 0;
        }
        thumb_start
            .min(travel)
            .saturating_mul(self.maximum)
            .saturating_add(travel / 2)
            / travel
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ModelSelectionTarget {
    Conversation,
    Mode(String),
    Setting(cagent_agent::config::SettingDefinition),
}

#[allow(clippy::struct_excessive_bools)]
pub(crate) struct App {
    pub(crate) workspace: PathBuf,
    pub(crate) session_id: cagent_agent::protocol::ConversationId,
    pub(crate) access: SessionAccess,
    pub(crate) welcome: Vec<Line<'static>>,
    pub(crate) welcome_tip: Option<&'static str>,
    pub(crate) history: Vec<TranscriptBlock>,
    pub(crate) transcript_older: Option<cagent_agent::protocol::TranscriptCursor>,
    pub(crate) transcript_page_request: Option<cagent_agent::protocol::TranscriptCursor>,
    pub(crate) transcript_home_drain: bool,
    pub(crate) pending_transcript_scroll_target: Option<cagent_agent::protocol::TranscriptBlockId>,
    /// A fresh upward gesture may start one ordinary older-page request.
    pub(crate) transcript_prefetch_armed: bool,
    pub(crate) transcript_viewport_height: usize,
    /// The loaded rows do not yet fill the live viewport.
    pub(crate) transcript_fill_needed: bool,
    /// Do not retry a failed automatic fill on every animation frame.
    pub(crate) transcript_fill_failed: Option<cagent_agent::protocol::TranscriptCursor>,
    pub(crate) supervised_work: SupervisedWorkState,
    pub(crate) streaming: cagent_agent::presentation::MarkdownDocument,
    pub(crate) streaming_source: String,
    pub(crate) streaming_status: Option<Line<'static>>,
    pub(crate) streaming_plan: cagent_agent::presentation::MarkdownDocument,
    pub(crate) streaming_plan_source: String,
    pub(crate) active_plan: Option<cagent_agent::protocol::UpdatePlanArgs>,
    pub(crate) queued: Vec<QueuedMessage>,
    pub(crate) selected_queue: Option<usize>,
    pub(crate) editing_queue: Option<QueuedMessage>,
    pub(crate) draft: String,
    pub(crate) composer_mode: ComposerMode,
    pub(crate) last_composer_input_at: Option<Instant>,
    pub(crate) cursor: usize,
    pub(crate) preferred_column: Option<usize>,
    pub(crate) attachments: Vec<AttachmentChip>,
    pub(crate) pastes: Vec<PasteChip>,
    pub(crate) images: Vec<ImageChip>,
    pub(crate) attachment_completion: Option<AttachmentCompletion>,
    pub(crate) history_entries: Vec<HistoryEntry>,
    pub(crate) history_index: Option<usize>,
    pub(crate) history_scratch: Option<DraftSnapshot>,
    /// The most recent non-empty draft cleared with the cancel key. This is
    /// frontend-local and is offered before durable composer history.
    pub(crate) cleared_draft: Option<DraftSnapshot>,
    pub(crate) recalling_cleared_draft: bool,
    pub(crate) undo_stack: Vec<DraftSnapshot>,
    pub(crate) active: bool,
    /// The active turn is waiting on supervised work rather than producing
    /// model output. User interactions have their own, more specific label.
    pub(crate) waiting_for_work: bool,
    /// The provider exposed reasoning activity for the current request.
    pub(crate) thinking: bool,
    pub(crate) compacting: bool,
    pub(crate) working_started_at: Option<Instant>,
    pub(crate) last_activity_at: Option<Instant>,
    pub(crate) reconnect_status: Option<ReconnectStatus>,
    /// Hides the terminal-local work decoration between an interrupt request
    /// and the agent snapshot that confirms cancellation.
    pub(crate) hide_working_indicator: bool,
    pub(crate) local_interrupt_pending: bool,
    pub(crate) context_percent: Option<u64>,
    pub(crate) context_used_tokens: Option<u64>,
    pub(crate) context_window: Option<u64>,
    pub(crate) session_usage: cagent_agent::protocol::SessionUsage,
    pub(crate) token_rate_tracker: cagent_agent::presentation::TokenRateTracker,
    pub(crate) provider_usage: Option<cagent_agent::provider::ProviderUsageReport>,
    pub(crate) provider_model: String,
    pub(crate) effort: Option<String>,
    pub(crate) fast: bool,
    pub(crate) fast_effective: bool,
    pub(crate) mode: String,
    pub(crate) agent: String,
    pub(crate) mode_colors: BTreeMap<String, StatusLineColor>,
    pub(crate) status_line_config: StatusLineConfig,
    pub(crate) theme: crate::theme::Theme,
    pub(crate) enabled_providers: BTreeSet<String>,
    pub(crate) notice: Option<String>,
    pub(crate) notice_deadline: Option<Instant>,
    pub(crate) notice_muted: bool,
    pub(crate) surfaces: Vec<Surface>,
    pub(crate) onboarding: bool,
    pub(crate) onboarding_outcome: Option<OnboardingOutcome>,
    pub(crate) recent_commands: Vec<String>,
    pub(crate) mode_commands: Vec<String>,
    pub(crate) skill_commands: Vec<(String, String)>,
    pub(crate) slash_list: ListState,
    pub(crate) slash_dismissed: bool,
    pub(crate) manual_cleanup_available: bool,
    observer_command_editor: Option<ComposerEditor>,
    pub(crate) observer_command_list: ListState,
    observer_command_dismissed: bool,
    pub(crate) pending_interaction: Option<InteractionRequest>,
    pub(crate) interaction_clears_draft: bool,
    pub(crate) permission_decisions:
        BTreeMap<cagent_agent::protocol::NodeId, cagent_agent::permissions::PermissionEffect>,
    pub(crate) approved_permission_requests: Vec<InteractionRequest>,
    pub(crate) plan_transition_pending: bool,
    pub(crate) pending_action: Option<AppAction>,
    pub(crate) pending_fork_draft: Option<(String, Vec<cagent_agent::protocol::AttachmentSpec>)>,
    pub(crate) exit_armed: Option<Instant>,
    pub(crate) composer_max_rows: Option<usize>,
    pub(crate) composer_scroll: ScrollViewState,
    pub(crate) history_scroll: usize,
    pub(crate) history_preview: Option<HistoryPreviewState>,
    pub(crate) follow_history_tail: bool,
    /// Once the welcome card leaves the viewport, extra space belongs above
    /// the transcript, not below it. Explicit scrolling can still reveal it.
    pub(crate) welcome_scrolled_past: bool,
    pub(crate) transcript_scrollbar: Option<TranscriptScrollbarLayout>,
    pub(crate) transcript_scrollbar_drag: Option<TranscriptScrollbarDrag>,
    /// A bounded transcript tail plus its width and first materialized block.
    pub(crate) history_layout: HistoryLayoutState,
    pub(crate) history_block_layouts: BlockLayoutCache,
    pub(crate) streaming_layout: Option<WrappedLayout>,
    pub(crate) streaming_plan_layout: Option<WrappedLayout>,
    pub(crate) bash_output_hits: Vec<BashOutputHit>,
    pub(crate) exploration_toggle_hits: Vec<ExplorationToggleHit>,
    pub(crate) expanded_explorations:
        std::collections::HashSet<crate::markdown::ExplorationToggleTarget>,
    pub(crate) activity_collapse_hits: Vec<ActivityCollapseHit>,
    pub(crate) expanded_activity_runs:
        std::collections::HashSet<crate::markdown::ActivityCollapseTarget>,
    pub(crate) collapse_tool_activity: bool,
    pub(crate) web_fetch_output_hits: Vec<WebFetchOutputHit>,
    pub(crate) mcp_call_hits: Vec<(u16, std::sync::Arc<cagent_agent::presentation::McpCall>)>,
    pub(crate) pending_mcp_detail_load: Option<cagent_agent::protocol::NodeId>,
    pub(crate) agent_log_load: Option<AgentLogLoad>,
    pub(crate) compaction_hits: Vec<CompactionHit>,
    pub(crate) web_search_result_hits: Vec<WebSearchResultHit>,
    pub(crate) agent_log_hits: Vec<AgentLogHit>,
    pub(crate) path_hits: Vec<PathHit>,
    pub(crate) link_hits: Vec<crate::markdown::HyperlinkOverlay>,
    pub(crate) open_links: bool,
    pub(crate) diff_hits: Vec<DiffHit>,
    pub(crate) image_hits: Vec<ImageHit>,
    pub(crate) working_tasks_hit: Option<WorkingTasksHit>,
    pub(crate) files_sidebar: file_tree::FilesSidebar,
    pub(crate) ui_editor: cagent_agent::config::UiEditor,
    pub(crate) image_picker: ratatui_image::picker::Picker,
    pub(crate) image_preview: Option<ImagePreview>,
    pub(crate) rendered_image: Option<RenderedImage>,
    pub(crate) image_redraw_area: Option<ratatui::layout::Rect>,
    pub(crate) pending_terminal_title: Option<String>,
    pub(crate) conversation_title: Option<String>,
    pub(crate) requested_input_title: cagent_agent::config::RequestedInputTitle,
    pub(crate) progress_mode: cagent_agent::config::ProgressMode,
    pub(crate) progress_osc: bool,
    pub(crate) progress_frame: usize,
    pub(crate) render_width: u16,
    pub(crate) render_height: u16,
    /// The currently displayed expanded terminal is parsed once per output or
    /// viewport change rather than once per frame.
    pub(crate) terminal_render_cache: Option<crate::render::TerminalRenderCache>,
    /// Prepared visual rows for the currently displayed text-backed expanded
    /// view. Rendering clones only the visible rows from this cache.
    pub(crate) expanded_text_render_cache: RefCell<Option<surfaces::ExpandedTextRenderCache>>,
    /// Width-dependent row index for the active permission diff. Input and
    /// rendering share it so wheel events never rewrap the complete diff.
    pub(crate) permission_diff_render_cache: RefCell<Option<surfaces::PermissionDiffRenderCache>>,
    pub(crate) key_bindings: cagent_agent::presentation::ResolvedKeyBindings,
    pub(crate) last_picker_click: Option<(Instant, PickerClickTarget)>,
    pub(crate) last_directory_click: Option<(Instant, u64, PathBuf)>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PickerClickTarget {
    Surface {
        kind: std::mem::Discriminant<Surface>,
        item: usize,
    },
    Attachment(usize),
    Slash(usize),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PickerClickKind {
    Surface,
    Attachment,
    Slash,
}

#[derive(Clone, Debug)]
pub(crate) struct BashOutputHit {
    pub(crate) row: u16,
    pub(crate) terminal_id: Option<cagent_agent::tools::TerminalId>,
    pub(crate) command: String,
    pub(crate) output: String,
    pub(crate) ansi_output: String,
    pub(crate) completion: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct ExplorationToggleHit {
    pub(crate) row: u16,
    pub(crate) target: crate::markdown::ExplorationToggleTarget,
}

#[derive(Clone, Debug)]
pub(crate) struct ActivityCollapseHit {
    pub(crate) row: u16,
    pub(crate) target: crate::markdown::ActivityCollapseTarget,
}

#[derive(Clone, Debug)]
pub(crate) struct WebFetchOutputHit {
    pub(crate) row: u16,
    pub(crate) url: String,
    pub(crate) redirected_url: Option<String>,
    pub(crate) format: cagent_agent::WebFetchFormat,
    pub(crate) output: String,
}

#[derive(Clone, Debug)]
pub(crate) struct CompactionHit {
    pub(crate) row: u16,
    pub(crate) summary: String,
}

#[derive(Clone, Debug)]
pub(crate) struct WebSearchResultHit {
    pub(crate) row: u16,
    pub(crate) provider: String,
    pub(crate) query: String,
    pub(crate) results: Vec<cagent_agent::web_search::WebSearchResult>,
}

#[derive(Clone, Debug)]
pub(crate) struct AgentLogHit {
    pub(crate) row: u16,
    pub(crate) run_id: cagent_agent::protocol::AgentRunId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PathHit {
    pub(crate) row: u16,
    pub(crate) columns: std::ops::Range<u16>,
    pub(crate) path: PathBuf,
    pub(crate) line: Option<usize>,
    pub(crate) directory: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct DiffHit {
    pub(crate) row: u16,
    pub(crate) target: crate::markdown::DiffLineTarget,
}

#[derive(Clone, Debug)]
pub(crate) struct ImageHit {
    pub(crate) row: u16,
    pub(crate) columns: std::ops::Range<u16>,
    pub(crate) image: cagent_agent::protocol::ImageAttachment,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WorkingTasksHit {
    pub(crate) row: u16,
    pub(crate) columns: std::ops::Range<u16>,
}

pub(crate) struct ImagePreview {
    pub(crate) key: String,
    pub(crate) image: Option<image::DynamicImage>,
    pub(crate) protocol: Option<(ratatui::layout::Size, ratatui_image::protocol::Protocol)>,
    pub(crate) error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RenderedImage {
    pub(crate) key: String,
    pub(crate) area: ratatui::layout::Rect,
    pub(crate) protocol: ImageProtocol,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ImageProtocol {
    Halfblocks,
    Sixel,
    Kitty,
    Iterm2,
}

impl App {
    pub(crate) fn is_observer(&self) -> bool {
        matches!(self.access, SessionAccess::Observer { .. })
    }

    pub(crate) fn is_read_only_view(&self) -> bool {
        self.is_observer() || self.history_preview.is_some()
    }

    pub(crate) fn history_preview_showing(&self) -> bool {
        self.history_preview.is_some()
            && self
                .surfaces
                .last()
                .is_some_and(|surface| matches!(surface, Surface::HistoryTree { .. }))
    }

    pub(crate) fn observer_command_active(&self) -> bool {
        self.is_read_only_view() && self.observer_command_editor.is_some()
    }

    pub(crate) fn observer_command_text(&self) -> Option<&str> {
        self.observer_command_editor
            .as_ref()
            .map(ComposerEditor::text)
    }

    pub(crate) fn observer_command_cursor(&self) -> usize {
        self.observer_command_editor
            .as_ref()
            .map_or(0, ComposerEditor::cursor)
    }

    pub(crate) fn observer_surface_allowed(surface: &Surface) -> bool {
        matches!(
            surface,
            Surface::Expanded { .. }
                | Surface::SupervisedWork { .. }
                | Surface::Help { .. }
                | Surface::Conversations { .. }
                | Surface::Providers { .. }
                | Surface::ProviderSetup { .. }
                | Surface::WebSearchPicker { .. }
                | Surface::WebSearchSetup { .. }
                | Surface::McpServers { .. }
                | Surface::McpServer { .. }
                | Surface::McpOAuth { .. }
                | Surface::McpPackageSetup { .. }
                | Surface::McpPackageValueEdit { .. }
                | Surface::McpTools { .. }
                | Surface::McpRemoveConfirm { .. }
                | Surface::McpAddScope { .. }
                | Surface::McpTransport { .. }
                | Surface::McpForm { .. }
                | Surface::McpFieldEdit { .. }
                | Surface::McpJsonEdit { .. }
                | Surface::McpMutationPreview { .. }
                | Surface::StatusLine { .. }
        )
    }

    pub(crate) fn new<S: SelectionInput>(
        workspace: &Path,
        selection: S,
        enabled_providers: BTreeSet<String>,
        mode: &str,
        composer_max_rows: Option<usize>,
    ) -> Self {
        let mut app = Self {
            workspace: workspace.to_path_buf(),
            session_id: cagent_agent::protocol::ConversationId::new(),
            access: SessionAccess::Owner,
            welcome: Vec::new(),
            welcome_tip: None,
            history: Vec::new(),
            transcript_older: None,
            transcript_page_request: None,
            transcript_home_drain: false,
            pending_transcript_scroll_target: None,
            transcript_prefetch_armed: true,
            transcript_viewport_height: 0,
            transcript_fill_needed: false,
            transcript_fill_failed: None,
            supervised_work: SupervisedWorkState::default(),
            streaming: cagent_agent::presentation::MarkdownDocument::default(),
            streaming_source: String::new(),
            streaming_status: None,
            streaming_plan: cagent_agent::presentation::MarkdownDocument::default(),
            streaming_plan_source: String::new(),
            active_plan: None,
            queued: Vec::new(),
            selected_queue: None,
            editing_queue: None,
            draft: String::new(),
            composer_mode: ComposerMode::Prompt,
            last_composer_input_at: None,
            cursor: 0,
            preferred_column: None,
            attachments: Vec::new(),
            pastes: Vec::new(),
            images: Vec::new(),
            attachment_completion: None,
            history_entries: Vec::new(),
            history_index: None,
            history_scratch: None,
            cleared_draft: None,
            recalling_cleared_draft: false,
            undo_stack: Vec::new(),
            active: false,
            waiting_for_work: false,
            thinking: false,
            compacting: false,
            working_started_at: None,
            last_activity_at: None,
            reconnect_status: None,
            hide_working_indicator: false,
            local_interrupt_pending: false,
            context_percent: None,
            context_used_tokens: None,
            context_window: None,
            session_usage: cagent_agent::protocol::SessionUsage::default(),
            token_rate_tracker: cagent_agent::presentation::TokenRateTracker::default(),
            provider_usage: None,
            provider_model: String::new(),
            effort: None,
            fast: false,
            fast_effective: false,
            mode: mode.to_owned(),
            agent: cagent_agent::config::DEFAULT_AGENT_NAME.into(),
            mode_colors: BTreeMap::new(),
            status_line_config: StatusLineConfig::default(),
            theme: crate::theme::Theme::dark(),
            enabled_providers,
            notice: None,
            notice_deadline: None,
            notice_muted: false,
            surfaces: Vec::new(),
            onboarding: false,
            onboarding_outcome: None,
            recent_commands: Vec::new(),
            mode_commands: Vec::new(),
            skill_commands: Vec::new(),
            slash_list: ListState::selectable(0),
            slash_dismissed: false,
            manual_cleanup_available: false,
            observer_command_editor: None,
            observer_command_list: ListState::selectable(0),
            observer_command_dismissed: false,
            pending_interaction: None,
            interaction_clears_draft: false,
            permission_decisions: BTreeMap::new(),
            approved_permission_requests: Vec::new(),
            plan_transition_pending: false,
            pending_action: None,
            pending_fork_draft: None,
            exit_armed: None,
            composer_max_rows,
            composer_scroll: ScrollViewState::default(),
            history_scroll: 0,
            history_preview: None,
            follow_history_tail: true,
            welcome_scrolled_past: false,
            transcript_scrollbar: None,
            transcript_scrollbar_drag: None,
            history_layout: HistoryLayoutState::default(),
            history_block_layouts: BlockLayoutCache::default(),
            streaming_layout: None,
            streaming_plan_layout: None,
            bash_output_hits: Vec::new(),
            exploration_toggle_hits: Vec::new(),
            expanded_explorations: std::collections::HashSet::new(),
            activity_collapse_hits: Vec::new(),
            expanded_activity_runs: std::collections::HashSet::new(),
            collapse_tool_activity: false,
            web_fetch_output_hits: Vec::new(),
            mcp_call_hits: Vec::new(),
            pending_mcp_detail_load: None,
            agent_log_load: None,
            compaction_hits: Vec::new(),
            web_search_result_hits: Vec::new(),
            agent_log_hits: Vec::new(),
            path_hits: Vec::new(),
            link_hits: Vec::new(),
            open_links: true,
            diff_hits: Vec::new(),
            image_hits: Vec::new(),
            working_tasks_hit: None,
            files_sidebar: file_tree::FilesSidebar::default(),
            ui_editor: cagent_agent::config::UiEditor::BuiltIn,
            image_picker: ratatui_image::picker::Picker::halfblocks(),
            image_preview: None,
            rendered_image: None,
            image_redraw_area: None,
            pending_terminal_title: None,
            conversation_title: None,
            requested_input_title: cagent_agent::config::RequestedInputTitle::default(),
            progress_mode: cagent_agent::config::ProgressMode::default(),
            progress_osc: true,
            progress_frame: 0,
            render_width: 80,
            render_height: 24,
            terminal_render_cache: None,
            expanded_text_render_cache: RefCell::new(None),
            permission_diff_render_cache: RefCell::new(None),
            key_bindings: cagent_agent::presentation::ResolvedKeyBindings::default(),
            last_picker_click: None,
            last_directory_click: None,
        };
        app.set_selection(selection.into_selection());
        app.welcome = welcome_lines(
            &app.provider_model,
            app.effort.as_deref(),
            !app.enabled_providers.is_empty(),
            80,
        );
        app
    }

    pub(crate) fn set_requested_input_title(
        &mut self,
        title: &cagent_agent::config::RequestedInputTitle,
    ) {
        self.requested_input_title = title.clone();
        if self.conversation_title.is_some() || self.pending_interaction.is_some() {
            self.pending_terminal_title = Some(self.current_terminal_title());
        }
    }

    pub(crate) fn set_progress_mode(&mut self, mode: &cagent_agent::config::ProgressMode) {
        self.progress_mode = mode.clone();
        self.progress_frame = 0;
        self.pending_terminal_title = Some(self.current_terminal_title());
    }

    pub(crate) fn set_progress_osc(&mut self, enabled: bool) {
        self.progress_osc = enabled;
    }

    pub(crate) fn advance_progress_frame(&mut self) {
        let requested_input_animated = !self.is_observer()
            && self.pending_interaction.is_some()
            && self.requested_input_title.frames().len() > 1;
        if self.progress_mode.is_active() || requested_input_animated {
            self.progress_frame = self.progress_frame.wrapping_add(1);
            self.pending_terminal_title = Some(self.current_terminal_title());
        }
    }

    pub(crate) fn current_terminal_title(&self) -> String {
        session_terminal_title_with_progress(
            self.conversation_title.as_deref(),
            !self.is_observer() && self.pending_interaction.is_some(),
            &self.requested_input_title,
            &self.progress_mode,
            self.working_indicator_visible(),
            self.progress_frame,
        )
    }

    /// Whether the terminal should report this turn as actively progressing.
    /// A pending interaction keeps the turn active, but work is paused until
    /// the user responds.
    pub(crate) fn osc_progress_active(&self) -> bool {
        self.active && self.pending_interaction.is_none()
    }

    pub(crate) fn begin_onboarding(&mut self) {
        self.onboarding = true;
        self.onboarding_outcome = None;
        self.surfaces.push(Surface::Onboarding);
    }

    pub(crate) fn dismiss_onboarding(&mut self) {
        if self.onboarding {
            self.onboarding = false;
            self.onboarding_outcome = Some(OnboardingOutcome::Dismissed);
        }
    }

    pub(crate) fn complete_onboarding(&mut self) {
        if self.onboarding {
            self.onboarding = false;
            self.onboarding_outcome = Some(OnboardingOutcome::Completed);
        }
    }

    pub(crate) fn take_onboarding_outcome(&mut self) -> Option<OnboardingOutcome> {
        self.onboarding_outcome.take()
    }

    fn reset_for_new_session(
        &mut self,
        selection: Option<(String, String, Option<String>)>,
        enabled_providers: BTreeSet<String>,
        mode: &str,
        welcome_tip: Option<&'static str>,
        width: u16,
    ) {
        self.history.clear();
        self.history_block_layouts.clear();
        self.welcome_tip = welcome_tip;
        self.supervised_work.clear();
        self.streaming.clear();
        self.streaming_source.clear();
        self.streaming_status = None;
        self.streaming_layout = None;
        self.streaming_plan.clear();
        self.streaming_plan_source.clear();
        self.streaming_plan_layout = None;
        self.active_plan = None;
        self.bash_output_hits.clear();
        self.exploration_toggle_hits.clear();
        self.expanded_explorations.clear();
        self.web_fetch_output_hits.clear();
        self.mcp_call_hits.clear();
        self.pending_mcp_detail_load = None;
        self.agent_log_load = None;
        self.compaction_hits.clear();
        self.web_search_result_hits.clear();
        self.path_hits.clear();
        self.link_hits.clear();
        self.image_preview = None;
        self.rendered_image = None;
        self.image_redraw_area = None;
        self.pending_terminal_title = None;
        self.pending_interaction = None;
        self.permission_decisions.clear();
        self.approved_permission_requests.clear();
        self.clear_notice();
        self.surfaces.clear();
        self.onboarding = false;
        self.onboarding_outcome = None;
        self.history_layout.clear();
        self.active = false;
        self.working_started_at = None;
        self.hide_working_indicator = false;
        self.local_interrupt_pending = false;
        self.context_percent = None;
        self.context_used_tokens = None;
        self.context_window = None;
        self.session_usage = cagent_agent::protocol::SessionUsage::default();
        self.token_rate_tracker = cagent_agent::presentation::TokenRateTracker::default();
        self.provider_usage = None;
        self.queued.clear();
        self.selected_queue = None;
        self.editing_queue = None;
        self.pending_action = None;
        self.plan_transition_pending = false;
        self.exit_armed = None;
        self.history_entries.clear();
        self.recent_commands.clear();
        self.history_index = None;
        self.history_scratch = None;
        self.cleared_draft = None;
        self.recalling_cleared_draft = false;
        self.clear_observer_command();
        self.enabled_providers = enabled_providers;
        self.set_selection(selection);
        mode.clone_into(&mut self.mode);
        self.clear_draft();
        self.welcome = welcome_lines(
            &self.provider_model,
            self.effort.as_deref(),
            !self.enabled_providers.is_empty(),
            width,
        );
        self.history_scroll = 0;
        self.follow_history_tail = true;
        self.welcome_scrolled_past = false;
        self.transcript_older = None;
        self.transcript_page_request = None;
        self.transcript_home_drain = false;
        self.transcript_fill_needed = false;
        self.transcript_fill_failed = None;
        self.pending_transcript_scroll_target = None;
        self.transcript_prefetch_armed = true;
        self.transcript_scrollbar_drag = None;
    }

    pub(super) fn arm_transcript_prefetch_if_idle(&mut self) {
        if self.transcript_page_request.is_none() {
            self.transcript_prefetch_armed = true;
            self.transcript_fill_failed = None;
        }
    }

    /// Takes the next older-page request allowed by the current navigation intent.
    ///
    /// Home-draining deliberately bypasses the ordinary gesture gate. Ordinary
    /// prefetch consumes its intent here, before I/O starts, so rendering or a
    /// prepend cannot cascade into another request.
    pub(crate) fn take_transcript_page_request(
        &mut self,
    ) -> Option<cagent_agent::protocol::TranscriptCursor> {
        if self.transcript_page_request.is_some() {
            return None;
        }
        // Materialize the already loaded prefix before asking the agent for
        // another durable page. Layout completion is frontend-local and must
        // not consume or manufacture pagination intent.
        if self.history_layout.start_block > 0 {
            return None;
        }
        let near_loaded_beginning =
            self.history_scroll <= self.transcript_viewport_height.saturating_mul(2);
        let fill_viewport = self.follow_history_tail
            && self.transcript_fill_needed
            && self.transcript_older != self.transcript_fill_failed;
        if !self.transcript_home_drain
            && !fill_viewport
            && !(self.transcript_prefetch_armed && near_loaded_beginning)
        {
            return None;
        }
        let cursor = self.transcript_older.clone()?;
        self.transcript_page_request = Some(cursor.clone());
        if !self.transcript_home_drain {
            self.transcript_prefetch_armed = false;
        }
        Some(cursor)
    }

    /// Completes only the request that is still active. Responses from a
    /// previous session or superseded request must not disturb current I/O.
    pub(crate) fn finish_transcript_page_request(
        &mut self,
        cursor: &cagent_agent::protocol::TranscriptCursor,
    ) -> bool {
        if self.transcript_page_request.as_ref() != Some(cursor) {
            return false;
        }
        self.transcript_page_request = None;
        true
    }

    /// Resets frontend-local state and hydrates the agent-owned session state
    /// from one attachment snapshot.
    pub(super) fn reset_for_session_snapshot(
        &mut self,
        snapshot: &cagent_agent::protocol::SessionSnapshot,
        welcome_tip: Option<&'static str>,
        width: u16,
    ) {
        self.session_id = snapshot.conversation_id;
        self.reset_for_new_session(
            snapshot.model_selection.clone(),
            snapshot.enabled_providers.iter().cloned().collect(),
            &snapshot.active_mode,
            welcome_tip,
            width,
        );
        self.apply_session_snapshot(snapshot);
    }

    pub(crate) fn hydrate_composer_history(
        &mut self,
        entries: Vec<cagent_agent::protocol::ComposerHistoryEntry>,
    ) {
        self.history_entries =
            cagent_agent::presentation::collapse_consecutive_composer_entries(entries)
                .into_iter()
                .map(|entry| HistoryEntry {
                    kind: entry.kind,
                    text: entry.text,
                    attachment_specs: entry.attachment_specs,
                    images: entry.images,
                    image_chips: entry.image_chips,
                })
                .collect();
    }

    fn set_selection(&mut self, selection: Option<(String, String, Option<String>)>) {
        let Some((provider, model, effort)) = selection else {
            self.provider_model.clear();
            self.effort = None;
            return;
        };
        if self.enabled_providers.contains(&provider) {
            self.provider_model = format!("{provider}/{model}");
            self.effort = effort;
        } else {
            self.provider_model.clear();
            self.effort = None;
        }
    }

    fn latest_assistant_source(&self) -> Option<&str> {
        self.history.iter().rev().find_map(|item| match &item.kind {
            cagent_agent::protocol::TranscriptBlockKind::Assistant { source, .. }
            | cagent_agent::protocol::TranscriptBlockKind::Plan { source, .. }
            | cagent_agent::protocol::TranscriptBlockKind::AcceptedPlan { source, .. } => {
                Some(source.as_str())
            }
            _ => None,
        })
    }

    #[cfg(test)]
    fn push_edit_history(&mut self, diff: cagent_agent::tools::SemanticDiff) {
        if let Some(TranscriptBlock {
            kind: cagent_agent::protocol::TranscriptBlockKind::Edits { diff: group },
            ..
        }) = self.history.last_mut()
        {
            group.merge(diff);
        } else {
            self.history.push(transcript::local_transcript_block(
                cagent_agent::protocol::TranscriptBlockKind::Edits { diff },
            ));
        }
        self.history_layout.rendered = None;
    }

    async fn handle_pending_interaction_key(
        &mut self,
        session: &SessionHandle,
        key: KeyEvent,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        if self.is_observer() {
            return Ok(false);
        }
        let Some(request) = self.pending_interaction.take() else {
            return Ok(false);
        };
        if let InteractionRequestKind::Question { questions } = &request.kind {
            if is_escape_key(key) {
                let answers = vec![QuestionAnswer::default(); questions.len()];
                self.pending_interaction = Some(request.clone());
                self.respond_to_question(session, request.id, &answers, None, true)
                    .await?;
                self.finish_interaction_response("cancel");
            } else {
                self.pending_interaction = Some(request);
            }
            return Ok(false);
        }
        if matches!(&request.kind, InteractionRequestKind::PlanCompletion { .. }) {
            if is_escape_key(key) {
                self.respond_to_plan(session, request.id, None, "keep", None)
                    .await?;
            } else {
                self.pending_interaction = Some(request);
            }
            return Ok(false);
        }
        let decision = if is_escape_key(key) {
            Some("deny")
        } else if uses_list_permission_choices(&request) {
            match key.code {
                KeyCode::Char('y' | 'Y') => Some("allow_once"),
                KeyCode::Char('a' | 'A') => Some("allow_directory"),
                KeyCode::Char('n' | 'N') => Some("deny"),
                _ => None,
            }
        } else if uses_file_permission_choices(&request) {
            match key.code {
                KeyCode::Char('y' | 'Y') => Some("allow_once"),
                KeyCode::Char('d' | 'D') => Some("allow_containing_directory"),
                KeyCode::Char('f' | 'F') => Some("allow_file"),
                KeyCode::Char('n' | 'N') => Some("deny"),
                _ => None,
            }
        } else {
            match key.code {
                KeyCode::Char('y' | 'Y') => Some("allow_once"),
                KeyCode::Char('p' | 'P') => Some("allow_project"),
                KeyCode::Char('g' | 'G') => Some("allow_global"),
                KeyCode::Char('n' | 'N') => Some("deny"),
                _ => None,
            }
        };
        if let Some(decision) = decision {
            self.respond_to_permission(session, request.id, decision, None)
                .await?;
            if decision != "deny" {
                self.approve_permission_request(&request);
            }
            self.finish_interaction_response(decision);
        } else {
            self.pending_interaction = Some(request);
        }
        Ok(false)
    }

    fn finish_interaction_response(&mut self, decision: &str) {
        self.pending_interaction = None;
        self.permission_diff_render_cache.get_mut().take();
        if decision == "deny" {
            self.interaction_clears_draft = false;
        }
    }

    async fn respond_to_permission(
        &self,
        session: &SessionHandle,
        request_id: cagent_agent::protocol::InteractionRequestId,
        decision: &str,
        reason: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        session
            .submit(SessionCommand::new(SessionAction::RespondToInteraction {
                request_id,
                response: match reason {
                    Some(reason) => serde_json::json!({ "decision": decision, "reason": reason }),
                    None => serde_json::json!({ "decision": decision }),
                },
            }))
            .await?;
        Ok(())
    }

    async fn respond_to_question(
        &self,
        session: &SessionHandle,
        request_id: cagent_agent::protocol::InteractionRequestId,
        answers: &[QuestionAnswer],
        answered: Option<&[bool]>,
        cancelled: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let questions = self
            .pending_interaction
            .as_ref()
            .and_then(|request| match &request.kind {
                InteractionRequestKind::Question { questions } => Some(questions),
                _ => None,
            });
        let answers = questions
            .into_iter()
            .flatten()
            .zip(answers)
            .enumerate()
            .filter(|(index, _)| answered.is_none_or(|answered| answered[*index]))
            .map(|(_, (question, answer))| (question.id.clone(), answer.clone()))
            .collect::<BTreeMap<_, _>>();
        session
            .submit(SessionCommand::new(SessionAction::RespondToInteraction {
                request_id,
                response: serde_json::json!({ "answers": answers, "cancelled": cancelled }),
            }))
            .await?;
        Ok(())
    }

    async fn respond_to_permission_rule(
        &self,
        session: &SessionHandle,
        request_id: cagent_agent::protocol::InteractionRequestId,
        decision: &str,
        rule: &cagent_agent::permissions::PermissionRule,
    ) -> Result<(), Box<dyn std::error::Error>> {
        session
            .submit(SessionCommand::new(SessionAction::RespondToInteraction {
                request_id,
                response: serde_json::json!({ "decision": decision, "rule": rule }),
            }))
            .await?;
        Ok(())
    }

    async fn respond_to_plan(
        &self,
        session: &SessionHandle,
        request_id: cagent_agent::protocol::InteractionRequestId,
        mode: Option<&str>,
        context_action: &str,
        note: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let response = mode.map_or_else(
            || serde_json::json!({ "decision": "keep_planning" }),
            |mode| {
                serde_json::json!({
                    "decision": "implement",
                    "mode": mode,
                    "context_action": context_action,
                    "note": note,
                })
            },
        );
        session
            .submit(SessionCommand::new(SessionAction::RespondToInteraction {
                request_id,
                response,
            }))
            .await?;
        Ok(())
    }

    pub(super) fn open_pending_interaction(&mut self) {
        if self.is_observer() {
            return;
        }
        let Some(request) = self.pending_interaction.clone() else {
            return;
        };
        if !self.surfaces.is_empty() && !self.sidebar_file_is_open() {
            return;
        }
        if self.interaction_presentation_is_deferred() {
            return;
        }
        if let InteractionRequestKind::PlanCompletion { plan, .. } = &request.kind
            && !self.history.iter().any(
                |block| matches!(&block.kind, cagent_agent::protocol::TranscriptBlockKind::Plan { source, .. } if source == plan),
            )
        {
            // Durable and interaction events travel over separate channels. Keep
            // the decision pending until the authoritative plan node has reached
            // the transcript, regardless of which channel wins the race.
            return;
        }
        if self.surfaces.iter().any(|surface| match surface {
            Surface::Permission {
                request: visible, ..
            }
            | Surface::PermissionRuleEdit {
                request: visible, ..
            }
            | Surface::PlanCompletion {
                request: visible, ..
            }
            | Surface::Question {
                request: visible, ..
            } => visible.id == request.id,
            _ => false,
        }) {
            return;
        }
        self.interaction_clears_draft = matches!(
            &request.kind,
            InteractionRequestKind::PermissionApproval {
                queued_message_id: None,
                ..
            }
        );
        let implementation_mode_index = match &request.kind {
            InteractionRequestKind::PlanCompletion {
                implementation_modes,
                default_mode,
                ..
            } => implementation_modes
                .iter()
                .position(|mode| mode == default_mode)
                .unwrap_or(0),
            InteractionRequestKind::PermissionApproval { .. }
            | InteractionRequestKind::Question { .. } => 0,
        };
        let question_count = interaction_question_count(&request.kind);
        self.surfaces.push(match &request.kind {
            InteractionRequestKind::Question { .. } => Surface::Question {
                request: Box::new(request),
                question_index: 0,
                option_index: 0,
                answers: vec![QuestionAnswer::default(); question_count],
                answered: vec![false; question_count],
                editing_note: false,
                note_cursor: 0,
            },
            InteractionRequestKind::PermissionApproval { .. } => Surface::Permission {
                scope: cagent_agent::presentation::default_permission_approval_scope(&request),
                request: Box::new(request),
                selected: 0,
                diff_scroll: 0,
                denial_note: String::new(),
                editing_note: false,
                note_cursor: 0,
            },
            InteractionRequestKind::PlanCompletion { .. } => Surface::PlanCompletion {
                implementation_mode_colors: match &request.kind {
                    InteractionRequestKind::PlanCompletion {
                        implementation_modes,
                        ..
                    } => implementation_modes
                        .iter()
                        .map(|mode| {
                            self.mode_colors
                                .get(mode)
                                .copied()
                                .unwrap_or(StatusLineColor::Cyan)
                        })
                        .collect(),
                    InteractionRequestKind::PermissionApproval { .. }
                    | InteractionRequestKind::Question { .. } => unreachable!(),
                },
                request: Box::new(request),
                selected: 0,
                note: String::new(),
                editing_note: false,
                note_cursor: 0,
                implementation_mode_index,
                implementation_model: self.provider_model.clone(),
                context_percent: self.context_percent,
            },
        });
    }

    #[cfg(test)]
    fn set_pending_interaction(&mut self, request: InteractionRequest) {
        self.pending_interaction = Some(request);
        self.active = true;
        self.working_started_at.get_or_insert_with(Instant::now);
        self.open_pending_interaction();
    }

    pub(super) fn approve_permission_request(&mut self, request: &InteractionRequest) {
        if !self
            .approved_permission_requests
            .iter()
            .any(|existing| existing.id == request.id)
        {
            self.approved_permission_requests.push(request.clone());
        }
    }

    pub(super) fn interaction_presentation_is_deferred(&self) -> bool {
        self.pending_interaction.is_some()
            && self.surfaces.is_empty()
            && !self.draft.is_empty()
            && self
                .last_composer_input_at
                .is_some_and(|input| input.elapsed() < INTERACTION_INPUT_SETTLE_DELAY)
    }
}

#[cfg(test)]
fn tool_output_max_scroll(output: &str, viewport_rows: usize) -> usize {
    output.lines().count().saturating_sub(viewport_rows)
}

#[cfg(test)]
mod tests;
