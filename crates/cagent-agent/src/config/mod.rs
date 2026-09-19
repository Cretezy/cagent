use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::RuntimeError;
use crate::provider::ModelCapabilities;

mod agents;
mod effective;
mod instructions;
mod paths;
mod raw;
mod settings;
mod store;
mod system_skills;

pub use agents::{
    AgentAvailability, AgentCatalog, AgentModeOverride, AgentModeOverrideDraft, AgentProfile,
    AgentProfileDraft, AutoLevel, ListPolicy, McpPolicy, ModeProfile, PromptMerge, ReadPolicy,
    RunPolicy, WritePolicy, resolve_modes,
};
use effective::ConfigData;
pub use effective::ConfigSnapshot;
pub use instructions::{
    InstructionSnapshot, InstructionSource, LocalContextPaths, LocalContextSnapshot,
    LocalContextWarning, SkillMetadata, SkillSource,
};
pub use paths::{AppPaths, PathOverrides};
use raw::ProviderConfigFile;
pub use raw::{
    ConfigFile, ProviderKind, ProviderOverrides, ProviderSettings, VertexAuth,
    WebFetchRedirectsConfig,
};
pub use settings::{SettingDefinition, SettingKind, SettingRow, SettingSection};
pub use store::{ConfigChange, ConfigChangeSource, ConfigStore};

pub const CONFIG_VERSION: u16 = 1;
pub const DEFAULT_AGENT_NAME: &str = "general";
pub const DEFAULT_RECAP_IDLE_SECONDS: u64 = 180;

/// Whether a configuration key names a secret-bearing value. Environment
/// variable references are names, not credentials, and remain inspectable.
#[must_use]
pub fn config_key_is_secret(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    !key.ends_with("_env")
        && !key.ends_with("_env_var")
        && ["secret", "password", "token", "api_key", "authorization"]
            .iter()
            .any(|needle| key.contains(needle))
}

/// Returns true when a dotted path or the selected item can disclose a secret.
#[must_use]
pub fn config_selection_is_secret(source: &str, dotted_key: &str) -> bool {
    if dotted_key.split('.').any(config_key_is_secret) {
        return true;
    }
    let Ok(document) = source.parse::<toml_edit::DocumentMut>() else {
        return true;
    };
    let mut item = None;
    let mut table: &dyn toml_edit::TableLike = document.as_table();
    for part in dotted_key.split('.') {
        item = table.get(part);
        let Some(next) = item.and_then(toml_edit::Item::as_table_like) else {
            break;
        };
        table = next;
    }
    fn contains_secret(item: &toml_edit::Item) -> bool {
        item.as_table_like().is_some_and(|table| {
            table
                .iter()
                .any(|(key, child)| config_key_is_secret(key) || contains_secret(child))
        })
    }
    item.is_none_or(contains_secret)
}
pub const BUILTIN_SMALL_MODELS: &[(&str, &str)] = &[
    ("chatgpt/gpt-6-luna", "low"),
    ("github-copilot/gpt-6-luna", "low"),
    ("openai/gpt-6-luna", "low"),
    ("opencode/deepseek-v4-flash", "low"),
    ("anthropic/claude-haiku-4-5", "low"),
    ("openrouter/deepseek/deepseek-v4-flash-latest", "low"),
    ("google/gemini-3.7-flash", "low"),
    ("google-vertex/gemini-3.7-flash", "low"),
];

/// Raw structured model selection used by `config.toml`.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSelectionConfig {
    pub model: Option<String>,
    pub tier: Option<String>,
    pub effort: Option<String>,
}

/// The target half of a validated model selection.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ModelTarget {
    Model(crate::ModelRef),
    Tier(String),
}

/// A validated model target and optional reasoning effort.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ModelSelection {
    pub target: Option<ModelTarget>,
    pub effort: Option<String>,
}

impl ModelSelection {
    fn parse(
        path: &Path,
        field: &str,
        value: ModelSelectionConfig,
        partial: bool,
    ) -> Result<Self, RuntimeError> {
        Self::try_from_config(value, partial).map_err(|message| RuntimeError::Config {
            path: path.to_path_buf(),
            message: format!("{field}: {message}"),
        })
    }

    pub(crate) fn try_from_config(
        value: ModelSelectionConfig,
        partial: bool,
    ) -> Result<Self, String> {
        let model = value
            .model
            .map(|model| {
                crate::ModelRef::parse(&model)
                    .map_err(|error| format!("invalid model {model:?}: {error}"))
            })
            .transpose()?;
        let tier = value
            .tier
            .map(|tier| nonempty_selection_value("tier", tier))
            .transpose()?;
        let effort = value
            .effort
            .map(|effort| nonempty_selection_value("effort", effort))
            .transpose()?;
        let target = match (model, tier) {
            (Some(model), None) => Some(ModelTarget::Model(model)),
            (None, Some(tier)) => Some(ModelTarget::Tier(tier)),
            (None, None) if partial && effort.is_some() => None,
            (None, None) => return Err("must set model or tier".into()),
            (Some(_), Some(_)) => return Err("cannot set both model and tier".into()),
        };
        Ok(Self { target, effort })
    }

    #[must_use]
    pub fn merged_over(&self, fallback: Option<&Self>) -> Self {
        Self {
            target: self
                .target
                .clone()
                .or_else(|| fallback.and_then(|value| value.target.clone())),
            effort: self
                .effort
                .clone()
                .or_else(|| fallback.and_then(|value| value.effort.clone())),
        }
    }
}

impl std::fmt::Display for ModelSelection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.target.as_ref() {
            Some(ModelTarget::Model(model)) => model.fmt(formatter),
            Some(ModelTarget::Tier(tier)) => write!(formatter, "tier:{tier}"),
            None => formatter.write_str("inherited"),
        }
    }
}

fn nonempty_selection_value(field: &str, value: String) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        Err(format!("{field} must not be empty"))
    } else {
        Ok(value.to_owned())
    }
}

/// One concrete candidate in a configurable model tier.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TierCandidate {
    pub model: crate::ModelRef,
    pub effort: Option<String>,
}
const DEFAULT_ATTACHMENT_BYTES: u64 = 256 * 1024;
const DEFAULT_ATTACHMENT_HARD_CAP_BYTES: u64 = 1024 * 1024;
const DEFAULT_RESIZE_IMAGES: bool = true;
const DEFAULT_SUBAGENT_MAX_CONCURRENT: usize = 10;
const DEFAULT_TITLE_GENERATION_TIMEOUT_SECONDS: u64 = 15;
const MAX_TITLE_GENERATION_TIMEOUT_SECONDS: u64 = 300;
pub(crate) const DEFAULT_SHOW_TIPS: bool = true;
pub(crate) const DEFAULT_COLLAPSE_TOOL_ACTIVITY: bool = true;
pub(crate) const DEFAULT_OPEN_LINKS: bool = true;
pub(crate) const DEFAULT_FILE_PICKER_RESPECT_GITIGNORE: bool = true;
pub(crate) const DEFAULT_FILE_PICKER_HIDE_HIDDEN_FILES: bool = true;
pub const DEFAULT_FILES_WIDTH: usize = 32;
const DEFAULT_COMPACTION_THRESHOLD_PERCENT: u8 = 90;
const DEFAULT_CONVERSATION_MAX_SIZE: u64 = 10_000_000_000;
const DEFAULT_CONVERSATION_MAX_AGE_SECONDS: u64 = 365 * 24 * 60 * 60;
pub const DEFAULT_REQUESTED_INPUT_TITLE: &str = "warning";

/// Filesystem action used when a retained-conversation limit is exceeded.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CleanupAction {
    /// Move conversation files to the operating system's trash/recycle bin.
    Trash,
    /// Permanently remove conversation files.
    Delete,
}

/// Effective conversation-retention policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConversationCleanupConfig {
    automatic: bool,
    max_size: Option<u64>,
    max_age: Option<std::time::Duration>,
    max_conversations: Option<usize>,
    action: CleanupAction,
}

impl Default for ConversationCleanupConfig {
    fn default() -> Self {
        Self {
            automatic: true,
            max_size: Some(DEFAULT_CONVERSATION_MAX_SIZE),
            max_age: Some(std::time::Duration::from_secs(
                DEFAULT_CONVERSATION_MAX_AGE_SECONDS,
            )),
            max_conversations: None,
            action: CleanupAction::Trash,
        }
    }
}

impl ConversationCleanupConfig {
    #[must_use]
    pub const fn automatic(self) -> bool {
        self.automatic
    }

    #[must_use]
    pub const fn max_size(self) -> Option<u64> {
        self.max_size
    }

    #[must_use]
    pub const fn max_age(self) -> Option<std::time::Duration> {
        self.max_age
    }

    #[must_use]
    pub const fn max_conversations(self) -> Option<usize> {
        self.max_conversations
    }

    #[must_use]
    pub const fn action(self) -> CleanupAction {
        self.action
    }

    #[cfg(test)]
    pub(crate) const fn for_test(
        max_size: Option<u64>,
        max_age: Option<std::time::Duration>,
        max_conversations: Option<usize>,
        action: CleanupAction,
    ) -> Self {
        Self {
            automatic: false,
            max_size,
            max_age,
            max_conversations,
            action,
        }
    }
}

/// Controls whether an external editor owns the terminal or launches beside the frontend.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UiEditorMode {
    /// Restore the terminal, attach the child, and wait for it to exit.
    Foreground,
    /// Keep the frontend active while the child runs without terminal streams.
    Background,
}

impl UiEditorMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Foreground => "foreground",
            Self::Background => "background",
        }
    }
}

/// Determines what a frontend should do when the user opens a filesystem path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UiEditor {
    Disabled,
    BuiltIn,
    Command {
        command: Vec<String>,
        line_command: Option<Vec<String>>,
        fallback_executables: Vec<String>,
        mode: UiEditorMode,
    },
}

impl UiEditor {
    /// Resolves a configured command and the conventional executable for known editors.
    #[must_use]
    pub fn command(&self) -> Option<&[String]> {
        match self {
            Self::Command { command, .. } => Some(command),
            Self::Disabled | Self::BuiltIn => None,
        }
    }

    #[must_use]
    pub const fn mode(&self) -> Option<UiEditorMode> {
        match self {
            Self::Command { mode, .. } => Some(*mode),
            Self::Disabled | Self::BuiltIn => None,
        }
    }

    #[must_use]
    pub const fn enabled(&self) -> bool {
        !matches!(self, Self::Disabled)
    }

    /// Builds the exact argv used to open a path, without invoking a shell.
    #[must_use]
    pub fn argv(&self, path: &Path, line: Option<usize>) -> Option<Vec<String>> {
        let Self::Command {
            command,
            line_command,
            ..
        } = self
        else {
            return None;
        };
        if let (Some(line), Some(template)) = (line, line_command.as_ref()) {
            let path = path.to_string_lossy();
            let line = line.to_string();
            return Some(
                template
                    .iter()
                    .map(|argument| argument.replace("{path}", &path).replace("{line}", &line))
                    .collect(),
            );
        }
        let mut argv = command.clone();
        argv.push(path.to_string_lossy().into_owned());
        Some(argv)
    }

    /// Builds ordered argv candidates, with preset executable fallbacks after the primary command.
    #[must_use]
    pub fn argv_candidates(&self, path: &Path, line: Option<usize>) -> Vec<Vec<String>> {
        let Self::Command {
            fallback_executables,
            ..
        } = self
        else {
            return Vec::new();
        };
        let Some(primary) = self.argv(path, line) else {
            return Vec::new();
        };
        let mut candidates = Vec::with_capacity(fallback_executables.len() + 1);
        candidates.push(primary.clone());
        for executable in fallback_executables {
            let mut fallback = primary.clone();
            if let Some(first) = fallback.first_mut() {
                first.clone_from(executable);
                candidates.push(fallback);
            }
        }
        candidates
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequestedInputTitle(Vec<String>);

impl Default for RequestedInputTitle {
    fn default() -> Self {
        Self::from_name(DEFAULT_REQUESTED_INPUT_TITLE)
    }
}

impl RequestedInputTitle {
    #[must_use]
    pub fn from_name(name: &str) -> Self {
        match name {
            "warning" => Self(vec!["".into()]),
            "warnings" => Self(vec!["".into(), "".into()]),
            "triangle" => Self(vec!["▲".into()]),
            "triangles" => Self(vec!["▲".into(), "△".into()]),
            "exclamation" => Self(vec!["!".into(), ".".into()]),
            _ => Self(vec![name.into()]),
        }
    }

    #[must_use]
    pub fn frames(&self) -> &[String] {
        &self.0
    }

    #[must_use]
    pub fn frame(&self, index: usize) -> Option<&str> {
        self.0.get(index % self.0.len()).map(String::as_str)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TitleConfig {
    requested_input: RequestedInputTitle,
    progress: ProgressMode,
}

impl TitleConfig {
    #[must_use]
    pub fn requested_input(&self) -> &RequestedInputTitle {
        &self.requested_input
    }

    #[must_use]
    pub fn progress(&self) -> &ProgressMode {
        &self.progress
    }
}

/// Controls automatic branch-local context compaction.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CompactionConfig {
    pub enabled: bool,
    pub threshold_percent: u8,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold_percent: DEFAULT_COMPACTION_THRESHOLD_PERCENT,
        }
    }
}

/// Selects how active work is reported outside the TUI.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum ProgressMode {
    Braille,
    Quadrant,
    Arc,
    Circle,
    Line,
    Block,
    Dots,
    NerdCircle,
    NerdArrow,
    Custom(Vec<String>),
    #[default]
    False,
}

impl ProgressMode {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Braille => "braille",
            Self::Quadrant => "quadrant",
            Self::Arc => "arc",
            Self::Circle => "circle",
            Self::Line => "line",
            Self::Block => "block",
            Self::Dots => "dots",
            Self::NerdCircle => "nerd_circle",
            Self::NerdArrow => "nerd_arrow",
            Self::Custom(_) => "custom",
            Self::False => "false",
        }
    }

    #[must_use]
    pub fn frames(&self) -> Option<&'static [&'static str]> {
        match self {
            Self::Braille => Some(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]),
            Self::Quadrant => Some(&["◰", "◳", "◲", "◱"]),
            Self::Arc => Some(&["◴", "◷", "◶", "◵"]),
            Self::Circle => Some(&["◐", "◓", "◑", "◒"]),
            Self::Line => Some(&["│", "/", "─", "\\"]),
            Self::Block => Some(&[
                "▁", "▂", "▃", "▄", "▅", "▆", "▇", "█", "▇", "▆", "▅", "▄", "▃", "▂",
            ]),
            Self::Dots => Some(&["⣾", "⣷", "⣯", "⣟", "⡿", "⢿", "⣻", "⣽"]),
            Self::NerdCircle => Some(&["󰪞", "󰪟", "󰪠", "󰪡", "󰪢", "󰪣", "󰪤", "󰪥"]),
            Self::NerdArrow => Some(&["󰁔", "󰁍", "󰁅", "󰁝"]),
            Self::Custom(_) => None,
            Self::False => None,
        }
    }

    #[must_use]
    pub fn frame(&self, index: usize) -> Option<&str> {
        if let Self::Custom(frames) = self {
            return (!frames.is_empty())
                .then(|| frames.get(index % frames.len()).map(String::as_str))
                .flatten();
        }
        let frames = self.frames()?;
        frames.get(index % frames.len()).copied()
    }

    #[must_use]
    pub fn is_active(&self) -> bool {
        !matches!(self, Self::False)
    }
}

pub const DEFAULT_SHELL_TIMEOUT_SECONDS: u64 = 600;

/// Selects how a terminal bell notification is emitted.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BellMethod {
    /// Send an OSC 777 desktop notification with a title and description.
    #[default]
    Osc777,
    /// Send one ASCII BEL character.
    Bell,
}

impl BellMethod {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Osc777 => "osc777",
            Self::Bell => "bell",
        }
    }
}

/// Controls terminal notifications for interactive frontends.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BellConfig {
    requested_input: bool,
    completed_turn: bool,
    method: BellMethod,
}

impl Default for BellConfig {
    fn default() -> Self {
        Self {
            requested_input: true,
            completed_turn: true,
            method: BellMethod::default(),
        }
    }
}

impl BellConfig {
    #[must_use]
    pub const fn new(requested_input: bool, completed_turn: bool, method: BellMethod) -> Self {
        Self {
            requested_input,
            completed_turn,
            method,
        }
    }

    #[must_use]
    pub const fn requested_input(self) -> bool {
        self.requested_input
    }

    #[must_use]
    pub const fn completed_turn(self) -> bool {
        self.completed_turn
    }

    #[must_use]
    pub const fn method(self) -> BellMethod {
        self.method
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct ShellConfig {
    pub executable: Option<String>,
    /// Cumulative built-in shell safety threshold. -1 disables classification.
    pub safe_level: i8,
    /// Allow the narrow built-in write grammar when the mode permits edits.
    pub safe_write: bool,
    pub terminal_mode: TerminalMode,
    pub login: bool,
    pub forward_env: Vec<String>,
    pub timeout_seconds: u64,
    pub output_bytes: usize,
    pub buffer_bytes: usize,
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            executable: None,
            safe_level: 3,
            safe_write: false,
            terminal_mode: TerminalMode::Normal,
            login: true,
            forward_env: Vec::new(),
            timeout_seconds: DEFAULT_SHELL_TIMEOUT_SECONDS,
            output_bytes: crate::DEFAULT_MODEL_OUTPUT_BYTES,
            buffer_bytes: crate::DEFAULT_TERMINAL_BUFFER_BYTES,
        }
    }
}

/// Controls terminal capabilities advertised to supervised shell commands.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalMode {
    #[default]
    Normal,
    Dumb,
}

/// Controls how readily primary agents use delegated sub-agents.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegationPolicy {
    /// Disable ordinary delegation; `/spawn` remains a per-turn override.
    Off,
    /// Delegate only when the user clearly asks for it or the task requires it.
    OnDemand,
    /// Keep general implementation primary and delegate independently useful work.
    #[default]
    Complex,
    /// Proactively seek useful independent work to delegate.
    Aggressive,
    /// Reserve substantive work for delegated agents; keep the primary as coordinator.
    Always,
}

impl DelegationPolicy {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::OnDemand => "on_demand",
            Self::Complex => "complex",
            Self::Aggressive => "aggressive",
            Self::Always => "always",
        }
    }
}

// ConfigSnapshot and its effective data live in `effective`; parsing and
// orchestration remain here so frontends retain the historical API.

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeCopy {
    #[default]
    Off,
    Include,
    All,
}

impl<'de> Deserialize<'de> for WorktreeCopy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Value {
            Enabled(bool),
            Mode(String),
        }
        match Value::deserialize(deserializer)? {
            Value::Enabled(false) => Ok(Self::Off),
            Value::Enabled(true) => Err(serde::de::Error::custom(
                "worktree.copy must be false, \"include\", or \"all\"",
            )),
            Value::Mode(value) if value == "include" => Ok(Self::Include),
            Value::Mode(value) if value == "all" => Ok(Self::All),
            Value::Mode(_) => Err(serde::de::Error::custom(
                "worktree.copy must be false, \"include\", or \"all\"",
            )),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WorktreeConfig {
    #[serde(default = "default_worktree_base")]
    pub base: String,
    #[serde(default = "default_worktree_branch_prefix")]
    pub branch_prefix: String,
    #[serde(default)]
    pub copy: WorktreeCopy,
}

impl Default for WorktreeConfig {
    fn default() -> Self {
        Self {
            base: default_worktree_base(),
            branch_prefix: default_worktree_branch_prefix(),
            copy: WorktreeCopy::Off,
        }
    }
}

fn default_worktree_base() -> String {
    "fresh".into()
}

fn default_worktree_branch_prefix() -> String {
    "worktree".into()
}

/// Default source of changes shown by the diff viewer.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum UiDiffMode {
    /// Changes made during the current conversation.
    #[default]
    Conversation,
    /// The repository's current working-copy changes.
    Git,
}

impl UiDiffMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Conversation => "conversation",
            Self::Git => "git",
        }
    }
}

/// Built-in terminal UI color scheme.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum UiTheme {
    #[default]
    Dark,
    Light,
}

impl UiTheme {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }
}

/// Resolved semantic colors used by terminal frontends.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UiColors {
    pub background: Option<crate::StatusLineColor>,
    pub foreground: crate::StatusLineColor,
    pub surface: crate::StatusLineColor,
    pub highlight: crate::StatusLineColor,
    pub bash: crate::StatusLineColor,
    pub muted: crate::StatusLineColor,
    pub diff_added_background: crate::StatusLineColor,
    pub diff_removed_background: crate::StatusLineColor,
}

impl UiColors {
    #[must_use]
    pub const fn for_theme(theme: UiTheme) -> Self {
        match theme {
            UiTheme::Dark => Self {
                background: None,
                foreground: crate::StatusLineColor::Rgb(229, 231, 235),
                surface: crate::StatusLineColor::Rgb(48, 48, 48),
                highlight: crate::StatusLineColor::Cyan,
                bash: crate::StatusLineColor::Rgb(255, 165, 0),
                muted: crate::StatusLineColor::Rgb(156, 163, 175),
                diff_added_background: crate::StatusLineColor::Rgb(33, 58, 43),
                diff_removed_background: crate::StatusLineColor::Rgb(74, 34, 29),
            },
            UiTheme::Light => Self {
                background: None,
                foreground: crate::StatusLineColor::Rgb(31, 41, 55),
                surface: crate::StatusLineColor::Rgb(218, 221, 226),
                highlight: crate::StatusLineColor::Cyan,
                bash: crate::StatusLineColor::Rgb(180, 83, 9),
                muted: crate::StatusLineColor::Rgb(107, 114, 128),
                diff_added_background: crate::StatusLineColor::Rgb(209, 245, 221),
                diff_removed_background: crate::StatusLineColor::Rgb(250, 213, 213),
            },
        }
    }
}

/*
#[derive(Clone, Debug)]
struct ConfigData {
    machine_fingerprint: bool,
    default_agent: String,
    default_mode: String,
    default_model: Option<crate::ModelRef>,
    default_effort: Option<String>,
    fast_model: Option<String>,
    preferred_small_models: Vec<String>,
    title_generation_enabled: bool,
    #[cfg(test)]
    title_generation_explicit: bool,
    title_generation_timeout_seconds: u64,
    delegation: DelegationPolicy,
    favourite_models: BTreeSet<crate::ModelRef>,
    status_line: crate::StatusLineConfig,
    agents: crate::AgentCatalog,
    modes: BTreeMap<String, crate::ModeProfile>,
    permission_rules: BTreeMap<(String, String), Vec<crate::PermissionRule>>,
    key_bindings: BTreeMap<String, Option<Vec<Option<String>>>>,
    attachment_bytes: u64,
    attachment_hard_cap_bytes: u64,
    resize_images: bool,
    scrollback_reflow_rows: usize,
    composer_max_rows: Option<usize>,
    files_width: usize,
    diff_context_lines: usize,
    show_tips: bool,
    ui_theme: UiTheme,
    ui_colors: UiColors,
    file_picker_respect_gitignore: bool,
    file_picker_hide_hidden_files: bool,
    editor: UiEditor,
    title: TitleConfig,
    bell: BellConfig,
    progress_osc: bool,
    providers: BTreeMap<String, ProviderSettings>,
    mcp: crate::McpGlobalConfig,
    web_search: crate::WebSearchConfig,
    web_fetch_redirects: WebFetchRedirectsConfig,
    subagent_max_concurrent: usize,
    shell: ShellConfig,
    external_agents: bool,
    bundled_skills: bool,
    disabled_skills: BTreeSet<String>,
    compaction: CompactionConfig,
    conversation_cleanup: ConversationCleanupConfig,
    worktree: WorktreeConfig,
}
*/

/*
    pub version: u16,
    #[serde(default = "default_true")]
    pub machine_fingerprint: bool,
    pub default_agent: Option<String>,
    pub default_mode: Option<String>,
    pub default_model: Option<String>,
    pub default_effort: Option<String>,
    pub fast_model: Option<String>,
    #[serde(default)]
    pub preferred_small_models: Vec<String>,
    #[serde(default)]
    title_generation: TitleGenerationConfigFile,
    #[serde(default)]
    subagents: SubagentConfigFile,
    #[serde(default)]
    pub favourite_models: BTreeSet<String>,
    #[serde(default)]
    providers: ProviderConfigFile,
    #[serde(default)]
    web_search: Option<crate::web_search::WebSearchFile>,
    #[serde(default)]
    web_fetch: WebFetchConfigFile,
    #[serde(default)]
    pub shell: ShellConfig,
    #[serde(default)]
    skills: SkillsConfigFile,
    #[serde(default)]
    compatibility: CompatibilityConfigFile,
    #[serde(default)]
    worktree: WorktreeConfig,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct SkillsConfigFile {
    #[serde(default)]
    bundled: BundledSkillsConfigFile,
    #[serde(default)]
    disabled: BTreeSet<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct BundledSkillsConfigFile {
    #[serde(default = "default_true")]
    enabled: bool,
}

impl Default for BundledSkillsConfigFile {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct CompatibilityConfigFile {
    #[serde(default = "default_true")]
    external_agents: bool,
}

impl Default for CompatibilityConfigFile {
    fn default() -> Self {
        Self {
            external_agents: true,
        }
    }
}

const fn default_true() -> bool {
    true
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WebFetchRedirectsConfig {
    generally_safe: bool,
    same_site: bool,
}

impl Default for WebFetchRedirectsConfig {
    fn default() -> Self {
        Self {
            generally_safe: true,
            same_site: true,
        }
    }
}

impl WebFetchRedirectsConfig {
    #[must_use]
    pub const fn generally_safe(self) -> bool {
        self.generally_safe
    }

    #[must_use]
    pub const fn same_site(self) -> bool {
        self.same_site
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
struct WebFetchConfigFile {
    redirects: WebFetchRedirectsConfigFile,
}

impl Default for WebFetchConfigFile {
    fn default() -> Self {
        Self {
            redirects: WebFetchRedirectsConfigFile::default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
struct WebFetchRedirectsConfigFile {
    generally_safe: bool,
    same_site: bool,
}

impl Default for WebFetchRedirectsConfigFile {
    fn default() -> Self {
        Self {
            generally_safe: true,
            same_site: true,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct SubagentConfigFile {
    strategy: Option<DelegationPolicy>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct TitleGenerationConfigFile {
    enabled: Option<bool>,
    timeout_seconds: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct ProviderConfigFile {
    #[serde(default)]
    custom: BTreeMap<String, ProviderOverrides>,
    #[serde(flatten)]
    built_in: BTreeMap<String, toml::Value>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    Mock,
    Chatgpt,
    #[serde(rename = "github-copilot")]
    GithubCopilot,
    Openai,
    Opencode,
    Anthropic,
    Openrouter,
    Google,
    #[serde(rename = "google-vertex")]
    GoogleVertex,
    #[serde(rename = "openai-compatible")]
    OpenAiCompatible,
}

impl std::fmt::Display for ProviderKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Mock => "mock",
            Self::Chatgpt => "chatgpt",
            Self::GithubCopilot => "github-copilot",
            Self::Openai => "openai",
            Self::Opencode => "opencode",
            Self::Anthropic => "anthropic",
            Self::Openrouter => "openrouter",
            Self::Google => "google",
            Self::GoogleVertex => "google-vertex",
            Self::OpenAiCompatible => "openai-compatible",
        })
    }
}

/// Raw optional provider overrides from the file schema.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ProviderOverrides {
    #[serde(rename = "type")]
    pub kind: Option<ProviderKind>,
    pub enabled: Option<bool>,
    pub api_key_env: Option<String>,
    pub display_name: Option<String>,
    pub base_url: Option<String>,
    pub protocol: Option<crate::ProviderProtocol>,
    pub models_endpoint: Option<String>,
    pub fast_model: Option<String>,
    /// Stable provider-owned usage window ID shown by the statusline.
    pub usage_limit: Option<String>,
    pub models: Option<Vec<String>>,
    pub aliases: Option<BTreeMap<String, String>>,
    pub capability_overrides: Option<BTreeMap<String, ModelCapabilities>>,
    pub auth: Option<VertexAuth>,
    pub project: Option<String>,
    pub location: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum VertexAuth {
    Adc,
    ApiKey,
}

/// Fully resolved effective provider settings used at runtime.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProviderSettings {
    pub kind: ProviderKind,
    pub enabled: bool,
    pub api_key_env: Option<String>,
    /// User-visible provider name. Built-ins supply their own name when omitted.
    pub display_name: Option<String>,
    /// API root for custom OpenAI-compatible providers. Secrets must never appear here.
    pub base_url: Option<String>,
    /// Request protocol used by custom providers.
    pub protocol: Option<crate::ProviderProtocol>,
    /// Relative or absolute model-list endpoint. `None` disables provider-owned discovery for
    /// custom providers; built-ins use their canonical endpoint.
    pub models_endpoint: Option<String>,
    /// Low-latency model used by classification and `model_tier = "fast"` agents.
    pub fast_model: Option<String>,
    pub usage_limit: Option<String>,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub aliases: BTreeMap<String, String>,
    #[serde(default)]
    pub capability_overrides: BTreeMap<String, ModelCapabilities>,
    pub auth: VertexAuth,
    pub project: Option<String>,
    pub location: Option<String>,
}

impl Default for ProviderSettings {
    fn default() -> Self {
        Self {
            kind: ProviderKind::Mock,
            enabled: false,
            api_key_env: None,
            display_name: None,
            base_url: None,
            protocol: None,
            models_endpoint: None,
            fast_model: None,
            usage_limit: None,
            models: Vec::new(),
            aliases: BTreeMap::new(),
            capability_overrides: BTreeMap::new(),
            auth: VertexAuth::Adc,
            project: None,
            location: None,
        }
    }
}

impl ProviderSettings {
    fn apply(&mut self, overrides: ProviderOverrides) {
        if let Some(kind) = overrides.kind {
            self.kind = kind;
        }
        if let Some(enabled) = overrides.enabled {
            self.enabled = enabled;
        }
        if overrides.api_key_env.is_some() {
            self.api_key_env = overrides.api_key_env;
        }
        if overrides.display_name.is_some() {
            self.display_name = overrides.display_name;
        }
        if overrides.base_url.is_some() {
            self.base_url = overrides.base_url;
        }
        if overrides.protocol.is_some() {
            self.protocol = overrides.protocol;
        }
        if overrides.models_endpoint.is_some() {
            self.models_endpoint = overrides.models_endpoint;
        }
        if overrides.fast_model.is_some() {
            self.fast_model = overrides.fast_model;
        }
        if overrides.usage_limit.is_some() {
            self.usage_limit = overrides.usage_limit;
        }
        if let Some(models) = overrides.models {
            self.models = models;
        }
        if let Some(aliases) = overrides.aliases {
            self.aliases = aliases;
        }
        if let Some(overrides) = overrides.capability_overrides {
            self.capability_overrides = overrides;
        }
        if let Some(auth) = overrides.auth {
            self.auth = auth;
        }
        if overrides.project.is_some() {
            self.project = overrides.project;
        }
        if overrides.location.is_some() {
            self.location = overrides.location;
        }
    }
}
*/

struct ParsedConfig {
    editable: toml_edit::DocumentMut,
    value: toml::Value,
    file: ConfigFile,
}

impl ParsedConfig {
    fn parse(path: &Path, source: &str) -> Result<Self, RuntimeError> {
        let editable = source
            .parse::<toml_edit::DocumentMut>()
            .map_err(|error| config_error(path, error))?;
        let value = toml_edit::de::from_document::<toml::Value>(editable.clone())
            .map_err(|error| config_error(path, error))?;
        if let Some(table) = value.as_table() {
            for removed in ["fast_model", "preferred_small_models", "default_effort"] {
                if table.contains_key(removed) {
                    return Err(RuntimeError::Config {
                        path: path.to_path_buf(),
                        message: format!(
                            "removed configuration key `{removed}`; use structured model selections and `[tiers]`"
                        ),
                    });
                }
            }
            if table
                .get("providers")
                .and_then(toml::Value::as_table)
                .is_some_and(|providers| {
                    providers.iter().any(|(name, provider)| {
                        if name == "custom" {
                            provider.as_table().is_some_and(|custom| {
                                custom.values().any(|entry| {
                                    entry
                                        .as_table()
                                        .is_some_and(|entry| entry.contains_key("fast_model"))
                                })
                            })
                        } else {
                            provider
                                .as_table()
                                .is_some_and(|entry| entry.contains_key("fast_model"))
                        }
                    })
                })
            {
                return Err(RuntimeError::Config {
                    path: path.to_path_buf(),
                    message: "removed provider key `fast_model`; configure `[tiers].small` instead"
                        .into(),
                });
            }
        }
        let file = toml_edit::de::from_document::<ConfigFile>(editable.clone())
            .map_err(|error| config_error(path, error))?;
        Ok(Self {
            editable,
            value,
            file,
        })
    }

    fn validate_version(&self, path: &Path) -> Result<(), RuntimeError> {
        if self.file.version == CONFIG_VERSION {
            Ok(())
        } else {
            Err(RuntimeError::Config {
                path: path.to_path_buf(),
                message: format!(
                    "unsupported config version {}; expected {CONFIG_VERSION}",
                    self.file.version
                ),
            })
        }
    }
}

struct BasicOptions {
    default_agent: String,
    default_mode: String,
    default_plan_exit_mode: Option<String>,
    default_model: Option<ModelSelection>,
    tiers: BTreeMap<String, Vec<TierCandidate>>,
    fast: bool,
    delegation: DelegationPolicy,
    favourite_models: BTreeSet<crate::ModelRef>,
}

fn parse_basic_options(path: &Path, file: &mut ConfigFile) -> Result<BasicOptions, RuntimeError> {
    let default_agent = validate_nonempty(
        path,
        "default_agent",
        file.default_agent.as_deref().unwrap_or(DEFAULT_AGENT_NAME),
    )?;
    let default_mode = validate_nonempty(
        path,
        "default_mode",
        file.default_mode.as_deref().unwrap_or("edit"),
    )?;
    let default_plan_exit_mode = validate_optional(
        path,
        "default_plan_exit_mode",
        file.default_plan_exit_mode.take(),
    )?;
    let default_model = file
        .default_model
        .take()
        .map(|selection| ModelSelection::parse(path, "default_model", selection, false))
        .transpose()?;
    let mut tiers = BTreeMap::new();
    for (name, candidates) in std::mem::take(&mut file.tiers) {
        let name = validate_nonempty(path, "tier name", &name)?;
        let candidates = candidates
            .into_iter()
            .enumerate()
            .map(|(index, candidate)| {
                let field = format!("tiers.{name}[{index}]");
                let selection = ModelSelection::parse(path, &field, candidate, false)?;
                let Some(ModelTarget::Model(model)) = selection.target else {
                    return Err(RuntimeError::Config {
                        path: path.to_path_buf(),
                        message: format!("{field} must contain a concrete model, not a tier"),
                    });
                };
                Ok(TierCandidate {
                    model,
                    effort: selection.effort,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        tiers.insert(name, candidates);
    }
    tiers.entry("small".into()).or_default();
    let fast = file.fast;
    let delegation = file.subagents.strategy.unwrap_or_default();
    if !(1..=600).contains(&file.shell.timeout_seconds)
        || file.shell.output_bytes == 0
        || file.shell.buffer_bytes == 0
        || file.shell.output_bytes > file.shell.buffer_bytes
    {
        return Err(RuntimeError::Config {
            path: path.to_path_buf(),
            message: "invalid shell limits".into(),
        });
    }
    if !(-1..=3).contains(&file.shell.safe_level) {
        return Err(RuntimeError::Config {
            path: path.to_path_buf(),
            message: "shell.safe_level must be an integer from -1 to 3".into(),
        });
    }
    let favourite_models = std::mem::take(&mut file.favourite_models)
        .into_iter()
        .map(|model| {
            crate::ModelRef::parse(&model).map_err(|error| RuntimeError::Config {
                path: path.to_path_buf(),
                message: format!("invalid favourite_models entry {model:?}: {error}"),
            })
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    Ok(BasicOptions {
        default_agent,
        default_mode,
        default_plan_exit_mode,
        default_model,
        tiers,
        fast,
        delegation,
        favourite_models,
    })
}

fn parse_providers(
    path: &Path,
    config: &mut ProviderConfigFile,
) -> Result<BTreeMap<String, ProviderSettings>, RuntimeError> {
    let mut providers: BTreeMap<String, ProviderSettings> = BTreeMap::new();
    for (id, value) in std::mem::take(&mut config.built_in) {
        if let Some(definition) = builtin_provider_definition(&id) {
            let overrides = value.try_into().map_err(|error| RuntimeError::Config {
                path: path.to_path_buf(),
                message: format!("invalid providers.{id}: {error}"),
            })?;
            let mut settings = provider_settings(definition);
            settings.apply(overrides);
            providers.insert(id, settings);
        }
        // Provider namespaces are extensible. Only built-ins and providers explicitly
        // declared under `providers.custom` are owned by Cagent; leave other entries
        // available for another frontend or a future built-in.
    }
    for (id, overrides) in std::mem::take(&mut config.custom) {
        if builtin_provider_definition(&id).is_some() {
            return Err(RuntimeError::Config {
                path: path.to_path_buf(),
                message: format!(
                    "providers.custom.{id} conflicts with the built-in provider providers.{id}"
                ),
            });
        }
        let Some(kind) = overrides.kind else {
            return Err(RuntimeError::Config {
                path: path.to_path_buf(),
                message: format!("providers.custom.{id}.type is required for custom providers"),
            });
        };
        if kind != ProviderKind::OpenAiCompatible {
            return Err(RuntimeError::Config {
                path: path.to_path_buf(),
                message: format!("providers.custom.{id}.type must be openai-compatible"),
            });
        }
        let mut settings = ProviderSettings::default();
        settings.apply(overrides);
        settings.kind = kind;
        providers.insert(id, settings);
    }
    for definition in crate::builtin_provider_definitions() {
        providers
            .entry(definition.id.into())
            .or_insert_with(|| provider_settings(definition));
    }
    validate_providers(path, &providers)?;
    Ok(providers)
}

struct UiOptions {
    status_line: crate::StatusLineConfig,
    attachment_bytes: u64,
    attachment_hard_cap_bytes: u64,
    resize_images: bool,
    scrollback_reflow_rows: usize,
    composer_max_rows: Option<usize>,
    files_width: usize,
    diff_context_lines: usize,
    diff_mode: UiDiffMode,
    show_tips: bool,
    collapse_tool_activity: bool,
    open_links: bool,
    theme: UiTheme,
    colors: UiColors,
    file_picker_respect_gitignore: bool,
    file_picker_hide_hidden_files: bool,
    editor: UiEditor,
    title: TitleConfig,
    bell: BellConfig,
    progress_osc: bool,
}

fn parse_ui_diff_mode(path: &Path, document: &toml::Value) -> Result<UiDiffMode, RuntimeError> {
    match document
        .get("ui")
        .and_then(|ui| ui.get("diff_mode"))
        .map(toml::Value::as_str)
    {
        None => Ok(UiDiffMode::default()),
        Some(Some("conversation")) => Ok(UiDiffMode::Conversation),
        Some(Some("git")) => Ok(UiDiffMode::Git),
        _ => Err(RuntimeError::Config {
            path: path.to_path_buf(),
            message: "ui.diff_mode must be conversation or git".into(),
        }),
    }
}

fn parse_ui_theme(path: &Path, document: &toml::Value) -> Result<UiTheme, RuntimeError> {
    match document
        .get("ui")
        .and_then(|ui| ui.get("theme"))
        .map(toml::Value::as_str)
    {
        None => Ok(UiTheme::default()),
        Some(Some("dark")) => Ok(UiTheme::Dark),
        Some(Some("light")) => Ok(UiTheme::Light),
        _ => Err(RuntimeError::Config {
            path: path.to_path_buf(),
            message: "ui.theme must be \"dark\" or \"light\"".into(),
        }),
    }
}

fn parse_ui_colors(
    path: &Path,
    document: &toml::Value,
    theme: UiTheme,
) -> Result<UiColors, RuntimeError> {
    let mut colors = UiColors::for_theme(theme);
    let Some(value) = document.get("ui").and_then(|ui| ui.get("colors")) else {
        return Ok(colors);
    };
    let table = value.as_table().ok_or_else(|| RuntimeError::Config {
        path: path.to_path_buf(),
        message: "ui.colors must be a table".into(),
    })?;
    if let Some(value) = table.get("background") {
        let source = value.as_str().ok_or_else(|| RuntimeError::Config {
            path: path.to_path_buf(),
            message: "ui.colors.background must be a color string".into(),
        })?;
        colors.background = Some(source.parse().map_err(|message| RuntimeError::Config {
            path: path.to_path_buf(),
            message: format!("invalid ui.colors.background: {message}"),
        })?);
    }
    for (key, target) in [
        ("foreground", &mut colors.foreground),
        ("surface", &mut colors.surface),
        ("highlight", &mut colors.highlight),
        ("bash", &mut colors.bash),
        ("muted", &mut colors.muted),
        ("diff_added_background", &mut colors.diff_added_background),
        (
            "diff_removed_background",
            &mut colors.diff_removed_background,
        ),
    ] {
        let Some(value) = table.get(key) else {
            continue;
        };
        let source = value.as_str().ok_or_else(|| RuntimeError::Config {
            path: path.to_path_buf(),
            message: format!("ui.colors.{key} must be a color string"),
        })?;
        *target = source.parse().map_err(|message| RuntimeError::Config {
            path: path.to_path_buf(),
            message: format!("invalid ui.colors.{key}: {message}"),
        })?;
    }
    Ok(colors)
}

fn parse_compaction(path: &Path, document: &toml::Value) -> Result<CompactionConfig, RuntimeError> {
    let Some(value) = document.get("compaction") else {
        return Ok(CompactionConfig::default());
    };
    let table = value.as_table().ok_or_else(|| RuntimeError::Config {
        path: path.to_path_buf(),
        message: "compaction must be a table".into(),
    })?;
    if table.get("enabled").is_some_and(|value| !value.is_bool()) {
        return Err(RuntimeError::Config {
            path: path.to_path_buf(),
            message: "compaction.enabled must be a boolean".into(),
        });
    }
    let enabled = table
        .get("enabled")
        .and_then(toml::Value::as_bool)
        .unwrap_or(true);
    let threshold = match table.get("threshold_percent") {
        None => i64::from(DEFAULT_COMPACTION_THRESHOLD_PERCENT),
        Some(value) => value.as_integer().ok_or_else(|| RuntimeError::Config {
            path: path.to_path_buf(),
            message: "compaction.threshold_percent must be an integer from 1 to 100".into(),
        })?,
    };
    let threshold_percent = u8::try_from(threshold)
        .ok()
        .filter(|value| (1..=100).contains(value))
        .ok_or_else(|| RuntimeError::Config {
            path: path.to_path_buf(),
            message: "compaction.threshold_percent must be an integer from 1 to 100".into(),
        })?;
    Ok(CompactionConfig {
        enabled,
        threshold_percent,
    })
}

fn parse_conversation_cleanup(
    path: &Path,
    document: &toml::Value,
) -> Result<ConversationCleanupConfig, RuntimeError> {
    let Some(value) = document.get("conversation_cleanup") else {
        return Ok(ConversationCleanupConfig::default());
    };
    let table = value.as_table().ok_or_else(|| RuntimeError::Config {
        path: path.to_path_buf(),
        message: "conversation_cleanup must be a table".into(),
    })?;
    let automatic = boolean_or_default(
        path,
        table.get("automatic"),
        "conversation_cleanup.automatic",
        true,
    )?;
    let max_size = parse_optional_human_limit(
        path,
        table.get("max_size"),
        "conversation_cleanup.max_size",
        DEFAULT_CONVERSATION_MAX_SIZE,
        parse_size,
    )?;
    let max_age_seconds = parse_optional_human_limit(
        path,
        table.get("max_age"),
        "conversation_cleanup.max_age",
        DEFAULT_CONVERSATION_MAX_AGE_SECONDS,
        parse_duration,
    )?;
    let max_conversations = match table.get("max_conversations") {
        None => None,
        Some(toml::Value::Boolean(false)) => None,
        Some(toml::Value::Integer(value)) if *value > 0 => {
            Some(usize::try_from(*value).map_err(|_| RuntimeError::Config {
                path: path.to_path_buf(),
                message: "conversation_cleanup.max_conversations is too large".into(),
            })?)
        }
        _ => {
            return Err(RuntimeError::Config {
                path: path.to_path_buf(),
                message:
                    "conversation_cleanup.max_conversations must be a positive integer or false"
                        .into(),
            });
        }
    };
    let action = match table.get("action") {
        None => CleanupAction::Trash,
        Some(toml::Value::String(value)) if value == "trash" => CleanupAction::Trash,
        Some(toml::Value::String(value)) if value == "delete" => CleanupAction::Delete,
        _ => {
            return Err(RuntimeError::Config {
                path: path.to_path_buf(),
                message: "conversation_cleanup.action must be \"trash\" or \"delete\"".into(),
            });
        }
    };
    Ok(ConversationCleanupConfig {
        automatic,
        max_size,
        max_age: max_age_seconds.map(std::time::Duration::from_secs),
        max_conversations,
        action,
    })
}

fn parse_optional_human_limit(
    path: &Path,
    value: Option<&toml::Value>,
    name: &str,
    default: u64,
    parser: fn(&str) -> Option<u64>,
) -> Result<Option<u64>, RuntimeError> {
    match value {
        None => Ok(Some(default)),
        Some(toml::Value::Boolean(false)) => Ok(None),
        Some(toml::Value::String(value)) => {
            parser(value).map(Some).ok_or_else(|| RuntimeError::Config {
                path: path.to_path_buf(),
                message: format!("{name} must be a positive human-readable value or false"),
            })
        }
        _ => Err(RuntimeError::Config {
            path: path.to_path_buf(),
            message: format!("{name} must be a positive human-readable value or false"),
        }),
    }
}

fn parse_scaled(value: &str, units: &[(&str, f64)]) -> Option<u64> {
    let normalized = value.trim().to_ascii_lowercase();
    let (number, multiplier) = units.iter().find_map(|(suffix, multiplier)| {
        normalized
            .strip_suffix(suffix)
            .map(|number| (number.trim(), *multiplier))
    })?;
    let number = number.parse::<f64>().ok()?;
    let scaled = number * multiplier;
    (number.is_finite() && number > 0.0 && scaled <= u64::MAX as f64)
        .then(|| scaled.round() as u64)
        .filter(|value| *value > 0)
}

fn parse_size(value: &str) -> Option<u64> {
    parse_scaled(
        value,
        &[
            ("tb", 1_000_000_000_000.0),
            ("gb", 1_000_000_000.0),
            ("mb", 1_000_000.0),
            ("kb", 1_000.0),
            ("b", 1.0),
        ],
    )
}

fn parse_duration(value: &str) -> Option<u64> {
    parse_scaled(
        value,
        &[
            ("y", 365.0 * 24.0 * 60.0 * 60.0),
            ("w", 7.0 * 24.0 * 60.0 * 60.0),
            ("d", 24.0 * 60.0 * 60.0),
            ("h", 60.0 * 60.0),
            ("m", 60.0),
            ("s", 1.0),
        ],
    )
}

fn parse_ui_options(path: &Path, document: &toml::Value) -> Result<UiOptions, RuntimeError> {
    let (attachment_bytes, attachment_hard_cap_bytes) = attachment_limits(path, document)?;
    let (scrollback_reflow_rows, composer_max_rows, files_width, diff_context_lines) =
        ui_limits(path, document)?;
    let title = title_or_default(path, document)?;
    let bell = bell_or_default(path, document)?;
    let progress_osc = progress_osc_or_default(path, document)?;
    let theme = parse_ui_theme(path, document)?;
    Ok(UiOptions {
        status_line: parse_status_line(path, document)?,
        attachment_bytes,
        attachment_hard_cap_bytes,
        resize_images: boolean_or_default(
            path,
            document
                .get("limits")
                .and_then(|limits| limits.get("resize_images")),
            "limits.resize_images",
            DEFAULT_RESIZE_IMAGES,
        )?,
        scrollback_reflow_rows,
        composer_max_rows,
        files_width,
        diff_context_lines,
        diff_mode: parse_ui_diff_mode(path, document)?,
        show_tips: boolean_or_default(
            path,
            document.get("ui").and_then(|ui| ui.get("show_tips")),
            "ui.show_tips",
            DEFAULT_SHOW_TIPS,
        )?,
        collapse_tool_activity: boolean_or_default(
            path,
            document
                .get("ui")
                .and_then(|ui| ui.get("collapse_tool_activity")),
            "ui.collapse_tool_activity",
            DEFAULT_COLLAPSE_TOOL_ACTIVITY,
        )?,
        open_links: boolean_or_default(
            path,
            document.get("ui").and_then(|ui| ui.get("open_links")),
            "ui.open_links",
            DEFAULT_OPEN_LINKS,
        )?,
        theme,
        colors: parse_ui_colors(path, document, theme)?,
        file_picker_respect_gitignore: boolean_or_default(
            path,
            document
                .get("ui")
                .and_then(|ui| ui.get("file_picker"))
                .and_then(|picker| picker.get("respect_gitignore")),
            "ui.file_picker.respect_gitignore",
            DEFAULT_FILE_PICKER_RESPECT_GITIGNORE,
        )?,
        file_picker_hide_hidden_files: boolean_or_default(
            path,
            document
                .get("ui")
                .and_then(|ui| ui.get("file_picker"))
                .and_then(|picker| picker.get("hide_hidden_files")),
            "ui.file_picker.hide_hidden_files",
            DEFAULT_FILE_PICKER_HIDE_HIDDEN_FILES,
        )?,
        editor: parse_ui_editor(path, document)?,
        title,
        bell,
        progress_osc,
    })
}

fn parse_ui_editor(path: &Path, document: &toml::Value) -> Result<UiEditor, RuntimeError> {
    let Some(value) = document.get("ui").and_then(|ui| ui.get("editor")) else {
        return Ok(UiEditor::BuiltIn);
    };
    if value.as_bool() == Some(false) {
        return Ok(UiEditor::Disabled);
    }
    if let Some(name) = value.as_str() {
        let (executable, fallback_executables, mode, line_arguments): (
            &str,
            &[&str],
            UiEditorMode,
            &[&str],
        ) = match name.to_ascii_lowercase().as_str() {
            "builtin" => return Ok(UiEditor::BuiltIn),
            "vscode" | "code" => (
                "code",
                &[],
                UiEditorMode::Background,
                &["--goto", "{path}:{line}"],
            ),
            "vscode-insiders" | "code-insiders" => (
                "code-insiders",
                &[],
                UiEditorMode::Background,
                &["--goto", "{path}:{line}"],
            ),
            "cursor" => (
                "cursor",
                &[],
                UiEditorMode::Background,
                &["--goto", "{path}:{line}"],
            ),
            "zed" => (
                "zed",
                &["zeditor"],
                UiEditorMode::Background,
                &["{path}:{line}"],
            ),
            "zed-preview" => (
                "zed-preview",
                &[],
                UiEditorMode::Background,
                &["{path}:{line}"],
            ),
            "intellij" | "idea" => (
                "idea",
                &[],
                UiEditorMode::Background,
                &["--line", "{line}", "{path}"],
            ),
            "webstorm" => (
                "webstorm",
                &[],
                UiEditorMode::Background,
                &["--line", "{line}", "{path}"],
            ),
            "pycharm" => (
                "pycharm",
                &[],
                UiEditorMode::Background,
                &["--line", "{line}", "{path}"],
            ),
            "rustrover" => (
                "rustrover",
                &[],
                UiEditorMode::Background,
                &["--line", "{line}", "{path}"],
            ),
            "goland" => (
                "goland",
                &[],
                UiEditorMode::Background,
                &["--line", "{line}", "{path}"],
            ),
            "clion" => (
                "clion",
                &[],
                UiEditorMode::Background,
                &["--line", "{line}", "{path}"],
            ),
            "rider" => (
                "rider",
                &[],
                UiEditorMode::Background,
                &["--line", "{line}", "{path}"],
            ),
            "fleet" => ("fleet", &[], UiEditorMode::Background, &["{path}:{line}"]),
            "sublime" | "subl" => ("subl", &[], UiEditorMode::Background, &["{path}:{line}"]),
            "lapce" => ("lapce", &[], UiEditorMode::Background, &["{path}:{line}"]),
            "emacs" => (
                "emacs",
                &[],
                UiEditorMode::Background,
                &["+{line}", "{path}"],
            ),
            "neovim" | "nvim" => (
                "nvim",
                &[],
                UiEditorMode::Foreground,
                &["+{line}", "{path}"],
            ),
            "vim" => ("vim", &[], UiEditorMode::Foreground, &["+{line}", "{path}"]),
            "helix" | "hx" => ("hx", &[], UiEditorMode::Foreground, &["+{line}", "{path}"]),
            _ => {
                return Err(RuntimeError::Config {
                    path: path.to_path_buf(),
                    message: format!(
                        "ui.editor names an unknown editor: {name}; use a command array or table for custom programs"
                    ),
                });
            }
        };
        return Ok(UiEditor::Command {
            command: vec![executable.into()],
            line_command: Some(
                std::iter::once(executable.to_owned())
                    .chain(line_arguments.iter().map(|argument| (*argument).to_owned()))
                    .collect(),
            ),
            fallback_executables: fallback_executables
                .iter()
                .map(|executable| (*executable).to_owned())
                .collect(),
            mode,
        });
    }
    if let Some(values) = value.as_array() {
        return Ok(UiEditor::Command {
            command: parse_ui_editor_command(path, values, "ui.editor")?,
            line_command: None,
            fallback_executables: Vec::new(),
            mode: UiEditorMode::Foreground,
        });
    }
    let Some(table) = value.as_table() else {
        return Err(RuntimeError::Config {
            path: path.to_path_buf(),
            message: "ui.editor must be false, a known editor name, a non-empty command array, or a { command, mode } table".into(),
        });
    };
    if let Some(key) = table
        .keys()
        .find(|key| !matches!(key.as_str(), "command" | "line_command" | "mode"))
    {
        return Err(RuntimeError::Config {
            path: path.to_path_buf(),
            message: format!("ui.editor has an unknown field: {key}"),
        });
    }
    let command_values = table
        .get("command")
        .and_then(toml::Value::as_array)
        .ok_or_else(|| RuntimeError::Config {
            path: path.to_path_buf(),
            message: "ui.editor.command must be a non-empty array of strings".into(),
        })?;
    let mode = match table.get("mode").and_then(toml::Value::as_str) {
        Some("foreground") => UiEditorMode::Foreground,
        Some("background") => UiEditorMode::Background,
        Some(mode) => {
            return Err(RuntimeError::Config {
                path: path.to_path_buf(),
                message: format!(
                    "ui.editor.mode must be \"foreground\" or \"background\", not {mode:?}"
                ),
            });
        }
        None => {
            return Err(RuntimeError::Config {
                path: path.to_path_buf(),
                message: "ui.editor.mode must be \"foreground\" or \"background\"".into(),
            });
        }
    };
    let line_command = table
        .get("line_command")
        .map(|value| {
            let values = value.as_array().ok_or_else(|| RuntimeError::Config {
                path: path.to_path_buf(),
                message: "ui.editor.line_command must be a non-empty array of strings".into(),
            })?;
            let command = parse_ui_editor_command(path, values, "ui.editor.line_command")?;
            if !command.iter().any(|argument| argument.contains("{path}"))
                || !command.iter().any(|argument| argument.contains("{line}"))
            {
                return Err(RuntimeError::Config {
                    path: path.to_path_buf(),
                    message: "ui.editor.line_command must contain both {path} and {line}".into(),
                });
            }
            Ok(command)
        })
        .transpose()?;
    Ok(UiEditor::Command {
        command: parse_ui_editor_command(path, command_values, "ui.editor.command")?,
        line_command,
        fallback_executables: Vec::new(),
        mode,
    })
}

fn parse_ui_editor_command(
    path: &Path,
    values: &[toml::Value],
    key: &str,
) -> Result<Vec<String>, RuntimeError> {
    let command = values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            value
                .as_str()
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| RuntimeError::Config {
                    path: path.to_path_buf(),
                    message: format!("{key}[{index}] must be a non-empty string"),
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    if command.is_empty() {
        return Err(RuntimeError::Config {
            path: path.to_path_buf(),
            message: format!("{key} must not be empty"),
        });
    }
    Ok(command)
}

fn parse_profiles_and_modes(
    path: &Path,
    parsed: &ParsedConfig,
    basic: &BasicOptions,
) -> Result<(crate::AgentCatalog, BTreeMap<String, crate::ModeProfile>), RuntimeError> {
    let agents = crate::AgentCatalog::from_document(&parsed.value).map_err(|error| {
        RuntimeError::Config {
            path: path.to_path_buf(),
            message: error.to_string(),
        }
    })?;
    if agents.get(&basic.default_agent).is_none() {
        return Err(RuntimeError::Config {
            path: path.to_path_buf(),
            message: format!(
                "default_agent names an unknown profile: {}",
                basic.default_agent
            ),
        });
    }
    if !agents
        .get(&basic.default_agent)
        .is_some_and(|agent| agent.enabled && agent.availability.user_selectable())
    {
        return Err(RuntimeError::Config {
            path: path.to_path_buf(),
            message: format!(
                "default_agent must be enabled and user-selectable: {}",
                basic.default_agent
            ),
        });
    }
    let declared_mode_order = configured_mode_order(&parsed.editable);
    let explicit_mode_order =
        crate::config::agents::mode_order(&parsed.value).map_err(|error| RuntimeError::Config {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
    let modes = crate::config::agents::resolve_modes_ordered(
        &parsed.value,
        &declared_mode_order,
        explicit_mode_order.as_deref(),
    )
    .map_err(|error| RuntimeError::Config {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;
    if !modes
        .get(&basic.default_mode)
        .is_some_and(|mode| mode.enabled)
    {
        return Err(RuntimeError::Config {
            path: path.to_path_buf(),
            message: format!(
                "default_mode must name an enabled mode: {}",
                basic.default_mode
            ),
        });
    }
    Ok((agents, modes))
}

fn resolve_default_plan_exit_mode(
    path: &Path,
    basic: &BasicOptions,
    modes: &BTreeMap<String, crate::ModeProfile>,
) -> Result<String, RuntimeError> {
    let eligible = |mode: &crate::ModeProfile| mode.enabled && mode.cycleable && !mode.plan;
    if let Some(name) = &basic.default_plan_exit_mode {
        if modes.get(name).is_some_and(eligible) {
            return Ok(name.clone());
        }
        return Err(RuntimeError::Config {
            path: path.to_path_buf(),
            message: format!(
                "default_plan_exit_mode must name an enabled, cycleable non-planning mode: {name}"
            ),
        });
    }
    if modes.get(&basic.default_mode).is_some_and(eligible) {
        return Ok(basic.default_mode.clone());
    }
    modes
        .values()
        .filter(|mode| eligible(mode))
        .min_by_key(|mode| mode.order)
        .map(|mode| mode.name.clone())
        .ok_or_else(|| RuntimeError::Config {
            path: path.to_path_buf(),
            message: "a plan exit requires an enabled, cycleable non-planning mode".into(),
        })
}

impl ConfigSnapshot {
    /// Loads and validates a versioned TOML configuration snapshot.
    ///
    /// A missing or empty file produces the built-in defaults. Any other read or parse failure
    /// names the source path and leaves snapshot replacement to the caller.
    ///
    /// # Errors
    ///
    /// Returns an error for unreadable files, invalid TOML, unsupported versions, or an unknown
    /// default mode.
    pub fn load(path: &Path) -> Result<Self, RuntimeError> {
        match std::fs::read_to_string(path) {
            Ok(source) => Self::parse(path, &source),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(RuntimeError::Config {
                path: path.to_path_buf(),
                message: error.to_string(),
            }),
        }
    }

    /// Parses and validates a TOML snapshot attributed to `path`.
    ///
    /// # Errors
    ///
    /// Returns a path-qualified configuration error when parsing or validation fails.
    pub fn parse(path: &Path, source: &str) -> Result<Self, RuntimeError> {
        if source.trim().is_empty() {
            return Ok(Self::default());
        }
        let mut parsed = ParsedConfig::parse(path, source)?;
        parsed.validate_version(path)?;
        let shell = parsed.file.shell.clone();
        let basic = parse_basic_options(path, &mut parsed.file)?;
        let providers = parse_providers(path, &mut parsed.file.providers)?;
        let title_generation = std::mem::take(&mut parsed.file.title_generation);
        #[cfg(test)]
        let title_generation_explicit =
            title_generation.enabled.is_some() || title_generation.timeout_seconds.is_some();
        let title_generation_enabled = title_generation.enabled.unwrap_or(true);
        let title_generation_timeout_seconds = title_generation
            .timeout_seconds
            .unwrap_or(DEFAULT_TITLE_GENERATION_TIMEOUT_SECONDS);
        if !(1..=MAX_TITLE_GENERATION_TIMEOUT_SECONDS).contains(&title_generation_timeout_seconds) {
            return Err(RuntimeError::Config {
                path: path.to_path_buf(),
                message: format!(
                    "title_generation.timeout_seconds must be between 1 and {MAX_TITLE_GENERATION_TIMEOUT_SECONDS}"
                ),
            });
        }
        let automatic_recaps = parsed.file.automatic_recaps;
        let recap_idle_seconds = parsed.file.recap_idle_seconds;
        if recap_idle_seconds == 0 {
            return Err(RuntimeError::Config {
                path: path.to_path_buf(),
                message: "recap_idle_seconds must be a positive integer".into(),
            });
        }
        let document = &parsed.value;
        let ui = parse_ui_options(path, document)?;
        let (agents, modes) = parse_profiles_and_modes(path, &parsed, &basic)?;
        let default_plan_exit_mode = resolve_default_plan_exit_mode(path, &basic, &modes)?;
        if let Some(selection) = basic.default_model.as_ref() {
            validate_model_selection(path, "default_model", selection, &providers, &basic.tiers)?;
        }
        for (tier, candidates) in &basic.tiers {
            for (index, candidate) in candidates.iter().enumerate() {
                validate_provider_reference(
                    path,
                    &format!("tiers.{tier}[{index}].model"),
                    &candidate.model,
                    &providers,
                )?;
            }
        }
        validate_profile_model_references(path, &agents, &modes, &providers, &basic.tiers)?;
        let permission_rules = resolve_all_permission_rules(path, document, &agents, &modes)?;
        let key_bindings = parse_key_bindings(document);
        let mcp = crate::global_config_from_document(path, document)?;
        let web_search = crate::WebSearchConfig::parse(path, parsed.file.web_search)?;
        let web_fetch_redirects = WebFetchRedirectsConfig {
            generally_safe: parsed.file.web_fetch.redirects.generally_safe,
            same_site: parsed.file.web_fetch.redirects.same_site,
        };
        let subagent_max_concurrent = nonnegative_usize_or_default(
            path,
            document
                .get("subagents")
                .and_then(|subagents| subagents.get("max_concurrent")),
            "subagents.max_concurrent",
            DEFAULT_SUBAGENT_MAX_CONCURRENT,
        )?;
        let compaction = parse_compaction(path, document)?;
        let conversation_cleanup = parse_conversation_cleanup(path, document)?;
        Ok(Self(Arc::new(ConfigData {
            machine_fingerprint: parsed.file.machine_fingerprint,
            default_agent: basic.default_agent,
            default_mode: basic.default_mode,
            default_plan_exit_mode,
            default_model: basic.default_model,
            tiers: basic.tiers,
            fast: basic.fast,
            title_generation_enabled,
            #[cfg(test)]
            title_generation_explicit,
            title_generation_timeout_seconds,
            automatic_recaps,
            recap_idle_seconds,
            delegation: basic.delegation,
            favourite_models: basic.favourite_models,
            status_line: ui.status_line,
            agents,
            modes,
            permission_rules,
            key_bindings,
            attachment_bytes: ui.attachment_bytes,
            attachment_hard_cap_bytes: ui.attachment_hard_cap_bytes,
            resize_images: ui.resize_images,
            scrollback_reflow_rows: ui.scrollback_reflow_rows,
            composer_max_rows: ui.composer_max_rows,
            files_width: ui.files_width,
            diff_context_lines: ui.diff_context_lines,
            diff_mode: ui.diff_mode,
            show_tips: ui.show_tips,
            collapse_tool_activity: ui.collapse_tool_activity,
            open_links: ui.open_links,
            ui_theme: ui.theme,
            ui_colors: ui.colors,
            file_picker_respect_gitignore: ui.file_picker_respect_gitignore,
            file_picker_hide_hidden_files: ui.file_picker_hide_hidden_files,
            editor: ui.editor,
            title: ui.title,
            bell: ui.bell,
            progress_osc: ui.progress_osc,
            providers,
            mcp,
            web_search,
            web_fetch_redirects,
            subagent_max_concurrent,
            shell,
            external_agents: parsed.file.compatibility.external_agents,
            bundled_skills: parsed.file.skills.bundled.enabled,
            disabled_skills: parsed.file.skills.disabled,
            compaction,
            conversation_cleanup,
            worktree: parsed.file.worktree,
        })))
    }

    #[must_use]
    pub fn default_agent(&self) -> &str {
        &self.0.default_agent
    }

    /// Returns whether new conversation IDs should include the hashed machine fingerprint.
    #[must_use]
    pub fn machine_fingerprint(&self) -> bool {
        self.0.machine_fingerprint
    }

    #[must_use]
    pub fn shell(&self) -> &ShellConfig {
        &self.0.shell
    }

    #[must_use]
    pub fn external_agents_compatibility(&self) -> bool {
        self.0.external_agents
    }

    #[must_use]
    pub fn bundled_skills_enabled(&self) -> bool {
        self.0.bundled_skills
    }

    #[must_use]
    pub fn disabled_skills(&self) -> &BTreeSet<String> {
        &self.0.disabled_skills
    }

    #[must_use]
    pub fn compaction(&self) -> CompactionConfig {
        self.0.compaction
    }

    #[must_use]
    pub fn show_tips(&self) -> bool {
        self.0.show_tips
    }

    #[must_use]
    pub fn collapse_tool_activity(&self) -> bool {
        self.0.collapse_tool_activity
    }

    #[must_use]
    pub fn open_links(&self) -> bool {
        self.0.open_links
    }

    #[must_use]
    pub fn ui_theme(&self) -> UiTheme {
        self.0.ui_theme
    }

    #[must_use]
    pub fn ui_colors(&self) -> UiColors {
        self.0.ui_colors
    }

    #[must_use]
    pub fn file_picker_respect_gitignore(&self) -> bool {
        self.0.file_picker_respect_gitignore
    }

    #[must_use]
    pub fn file_picker_hide_hidden_files(&self) -> bool {
        self.0.file_picker_hide_hidden_files
    }

    #[must_use]
    pub fn editor(&self) -> &UiEditor {
        &self.0.editor
    }

    #[must_use]
    pub fn default_mode(&self) -> &str {
        &self.0.default_mode
    }

    /// Returns the initial implementation mode selected after completing a plan.
    #[must_use]
    pub fn default_plan_exit_mode(&self) -> &str {
        &self.0.default_plan_exit_mode
    }

    #[must_use]
    pub fn worktree(&self) -> &WorktreeConfig {
        &self.0.worktree
    }

    #[must_use]
    pub fn default_model(&self) -> Option<&ModelSelection> {
        self.0.default_model.as_ref()
    }

    #[must_use]
    pub fn tier(&self, name: &str) -> Option<&[TierCandidate]> {
        self.0.tiers.get(name).map(Vec::as_slice)
    }

    #[must_use]
    pub fn tiers(&self) -> &BTreeMap<String, Vec<TierCandidate>> {
        &self.0.tiers
    }

    /// Returns whether the catalog-advertised Fast service tier is requested.
    #[must_use]
    pub fn fast(&self) -> bool {
        self.0.fast
    }

    /// Returns whether automatic conversation-title refinement is enabled.
    #[must_use]
    pub fn title_generation_enabled(&self) -> bool {
        self.0.title_generation_enabled
    }

    #[cfg(test)]
    pub(crate) fn disable_implicit_title_generation(self) -> Self {
        if self.0.title_generation_explicit {
            return self;
        }
        let mut data = (*self.0).clone();
        data.title_generation_enabled = false;
        Self(Arc::new(data))
    }

    /// Returns the conversation-title refinement deadline in seconds.
    #[must_use]
    pub fn title_generation_timeout_seconds(&self) -> u64 {
        self.0.title_generation_timeout_seconds
    }

    /// Whether completed conversations produce a short recap after becoming idle.
    #[must_use]
    pub fn automatic_recaps(&self) -> bool {
        self.0.automatic_recaps
    }

    /// Number of idle seconds before an automatic recap is generated.
    #[must_use]
    pub fn recap_idle_seconds(&self) -> u64 {
        self.0.recap_idle_seconds
    }

    /// Returns the effective policy for autonomous sub-agent delegation.
    #[must_use]
    pub fn delegation_policy(&self) -> DelegationPolicy {
        self.0.delegation
    }

    /// Returns the maximum number of delegated agents that may run at once.
    #[must_use]
    pub fn subagent_max_concurrent(&self) -> usize {
        self.0.subagent_max_concurrent
    }

    /// Returns whether native sub-agent tools are available to primary agents.
    #[must_use]
    pub fn subagents_enabled(&self) -> bool {
        self.subagent_max_concurrent() > 0
    }

    #[must_use]
    pub fn favourite_models(&self) -> &BTreeSet<crate::ModelRef> {
        &self.0.favourite_models
    }

    #[must_use]
    pub fn status_line(&self) -> &crate::StatusLineConfig {
        &self.0.status_line
    }

    /// Returns the fully resolved built-in and configured agent profiles.
    ///
    /// # Errors
    /// Returns an error if the immutable document is not a valid agent graph.
    pub fn agent_catalog(&self) -> Result<crate::AgentCatalog, RuntimeError> {
        Ok(self.0.agents.clone())
    }

    /// Returns built-in and custom modes with configured policy overrides.
    ///
    /// # Errors
    /// Returns an error if a configured model reference is invalid.
    pub fn modes(&self) -> Result<BTreeMap<String, crate::ModeProfile>, RuntimeError> {
        Ok(self.0.modes.clone())
    }

    /// Returns one enabled mode, rejecting unknown and disabled names consistently.
    ///
    /// # Errors
    /// Returns an error when mode configuration is invalid, the name is unknown, or the mode is
    /// disabled.
    pub fn enabled_mode(&self, name: &str) -> Result<crate::ModeProfile, RuntimeError> {
        let mode = self
            .modes()?
            .remove(name)
            .ok_or_else(|| RuntimeError::InvalidOption(format!("unknown mode: {name}")))?;
        if !mode.enabled {
            return Err(RuntimeError::InvalidOption(format!(
                "mode is disabled: {name}"
            )));
        }
        Ok(mode)
    }

    /// Returns enabled modes in picker and cycling order.
    ///
    /// # Errors
    /// Returns an error when mode configuration is invalid.
    pub fn enabled_modes(&self) -> Result<Vec<crate::ModeProfile>, RuntimeError> {
        let mut modes = self
            .modes()?
            .into_values()
            .filter(|mode| mode.enabled)
            .collect::<Vec<_>>();
        modes.sort_by_key(|mode| mode.order);
        Ok(modes)
    }

    #[must_use]
    pub fn attachment_bytes(&self) -> u64 {
        self.0.attachment_bytes
    }

    #[must_use]
    pub fn attachment_hard_cap_bytes(&self) -> u64 {
        self.0.attachment_hard_cap_bytes
    }

    #[must_use]
    pub fn resize_images(&self) -> bool {
        self.0.resize_images
    }

    /// Returns configured stable action IDs and their array-valued chord overrides.
    ///
    /// The outer `None` preserves a scalar or other non-array value. Inner `None` entries preserve
    /// non-string array elements so the shared resolver can report each invalid chord.
    #[must_use]
    pub fn key_bindings(&self) -> BTreeMap<String, Option<Vec<Option<String>>>> {
        self.0.key_bindings.clone()
    }

    #[must_use]
    pub fn scrollback_reflow_rows(&self) -> usize {
        self.0.scrollback_reflow_rows
    }

    #[must_use]
    pub fn composer_max_rows(&self) -> Option<usize> {
        self.0.composer_max_rows
    }

    #[must_use]
    pub fn conversation_cleanup(&self) -> ConversationCleanupConfig {
        self.0.conversation_cleanup
    }

    #[must_use]
    pub fn files_width(&self) -> usize {
        self.0.files_width
    }

    #[must_use]
    pub fn diff_context_lines(&self) -> usize {
        self.0.diff_context_lines
    }

    /// Default source of changes for the diff viewer.
    #[must_use]
    pub fn diff_mode(&self) -> UiDiffMode {
        self.0.diff_mode
    }

    #[must_use]
    pub fn title(&self) -> &TitleConfig {
        &self.0.title
    }

    #[must_use]
    pub fn bell(&self) -> BellConfig {
        self.0.bell
    }

    #[must_use]
    pub fn progress_osc(&self) -> bool {
        self.0.progress_osc
    }

    #[must_use]
    pub fn providers(&self) -> &BTreeMap<String, ProviderSettings> {
        &self.0.providers
    }

    #[must_use]
    pub fn provider(&self, id: &str) -> Option<&ProviderSettings> {
        self.0.providers.get(id)
    }

    #[must_use]
    pub fn web_search(&self) -> &crate::WebSearchConfig {
        &self.0.web_search
    }

    #[must_use]
    pub fn web_fetch_redirects(&self) -> WebFetchRedirectsConfig {
        self.0.web_fetch_redirects
    }

    pub(crate) fn mcp_config(&self) -> &crate::McpGlobalConfig {
        &self.0.mcp
    }

    #[must_use]
    pub fn provider_enabled(&self, id: &str) -> bool {
        self.provider(id).is_some_and(|provider| provider.enabled)
    }

    /// Environment variables stripped from child shells unless explicitly forwarded.
    #[must_use]
    pub fn provider_credential_variables(&self) -> Vec<String> {
        let mut variables = BTreeSet::from([
            "OPENAI_API_KEY".to_owned(),
            "ANTHROPIC_API_KEY".to_owned(),
            "OPENROUTER_API_KEY".to_owned(),
            "OPENCODE_API_KEY".to_owned(),
        ]);
        variables.extend(
            self.0
                .providers
                .values()
                .filter_map(|provider| provider.api_key_env.clone()),
        );
        variables.extend(self.0.web_search.credential_variables());
        variables.into_iter().collect()
    }

    /// Resolves inline permission rules for one configured agent or mode.
    ///
    /// # Errors
    ///
    /// Returns a path-qualified configuration error for malformed inline rules.
    pub fn permission_rules(
        &self,
        section: &str,
        name: &str,
    ) -> Result<Vec<crate::PermissionRule>, RuntimeError> {
        if !matches!(section, "agents" | "modes") {
            return Err(RuntimeError::InvalidOption(format!(
                "permission section must be agents or modes, got {section}"
            )));
        }
        Ok(self
            .0
            .permission_rules
            .get(&(section.to_owned(), name.to_owned()))
            .cloned()
            .unwrap_or_default())
    }
}

fn builtin_provider_definition(id: &str) -> Option<&'static crate::ProviderDefinition> {
    crate::builtin_provider_definitions()
        .iter()
        .find(|definition| definition.id == id)
}

fn provider_config_path(id: &str) -> String {
    if builtin_provider_definition(id).is_some() {
        format!("providers.{id}")
    } else {
        format!("providers.custom.{id}")
    }
}

fn provider_settings(definition: &crate::ProviderDefinition) -> ProviderSettings {
    ProviderSettings {
        kind: definition.kind,
        enabled: false,
        api_key_env: definition
            .credential_environment_variable
            .map(str::to_owned),
        display_name: None,
        base_url: None,
        protocol: None,
        models_endpoint: None,
        usage_limit: (definition.id == "chatgpt").then(|| "codex:weekly".into()),
        models: Vec::new(),
        aliases: BTreeMap::new(),
        capability_overrides: BTreeMap::new(),
        auth: VertexAuth::Adc,
        project: None,
        location: None,
    }
}

impl Default for ConfigSnapshot {
    fn default() -> Self {
        Self::parse(Path::new("config.toml"), "version = 1\n").expect("built-in config is valid")
    }
}

#[cfg(test)]
impl ConfigSnapshot {
    pub(crate) fn test_default() -> Self {
        Self::parse(
            Path::new("runtime-test-config.toml"),
            "version = 1\ndefault_mode = 'edit'\ndefault_model = { model = 'mock/echo' }\n[title_generation]\nenabled = false\n[providers.mock]\ntype = 'mock'\nenabled = true\n",
        )
        .expect("test runtime config is valid")
    }
}

fn attachment_limits(path: &Path, document: &toml::Value) -> Result<(u64, u64), RuntimeError> {
    let limits = document.get("limits");
    let implicit = positive_limit(
        path,
        limits.and_then(|limits| limits.get("attachment_bytes")),
        "limits.attachment_bytes",
        DEFAULT_ATTACHMENT_BYTES,
    )?;
    let hard = positive_limit(
        path,
        limits.and_then(|limits| limits.get("attachment_hard_cap_bytes")),
        "limits.attachment_hard_cap_bytes",
        DEFAULT_ATTACHMENT_HARD_CAP_BYTES,
    )?;
    if implicit > hard {
        return Err(RuntimeError::Config {
            path: path.to_path_buf(),
            message: "limits.attachment_bytes must not exceed limits.attachment_hard_cap_bytes"
                .into(),
        });
    }
    Ok((implicit, hard))
}

fn positive_limit(
    path: &Path,
    value: Option<&toml::Value>,
    name: &str,
    default: u64,
) -> Result<u64, RuntimeError> {
    match value {
        None => Ok(default),
        Some(toml::Value::Integer(value)) if *value > 0 => {
            u64::try_from(*value).map_err(|error| config_error(path, error))
        }
        Some(_) => Err(RuntimeError::Config {
            path: path.to_path_buf(),
            message: format!("{name} must be a positive integer"),
        }),
    }
}

fn ui_limits(
    path: &Path,
    document: &toml::Value,
) -> Result<(usize, Option<usize>, usize, usize), RuntimeError> {
    let ui = document.get("ui");
    let scrollback = positive_usize_or_default(
        path,
        ui.and_then(|value| value.get("scrollback_reflow_rows")),
        "ui.scrollback_reflow_rows",
        2_000,
    )?;
    let composer = optional_positive_usize(
        path,
        ui.and_then(|value| value.get("composer_max_rows")),
        "ui.composer_max_rows",
    )?;
    let files_width = positive_usize_or_default(
        path,
        ui.and_then(|value| value.get("files"))
            .and_then(|value| value.get("width")),
        "ui.files.width",
        DEFAULT_FILES_WIDTH,
    )?;
    let diff = positive_usize_or_default(
        path,
        ui.and_then(|value| value.get("diff_context_lines")),
        "ui.diff_context_lines",
        3,
    )?;
    Ok((scrollback, composer, files_width, diff))
}

fn bell_or_default(path: &Path, document: &toml::Value) -> Result<BellConfig, RuntimeError> {
    let ui = document.get("ui");
    let legacy_method = ui.and_then(|ui| ui.get("terminal_notification"));
    let Some(value) = ui.and_then(|ui| ui.get("bell")) else {
        return Ok(BellConfig {
            method: legacy_method
                .map(|value| bell_method(path, "ui.terminal_notification", value))
                .transpose()?
                .unwrap_or_default(),
            ..BellConfig::default()
        });
    };
    let table = value.as_table().ok_or_else(|| RuntimeError::Config {
        path: path.to_path_buf(),
        message: "ui.bell must be a table".into(),
    })?;
    let requested_input = boolean_or_default(
        path,
        table.get("requested_input"),
        "ui.bell.requested_input",
        true,
    )?;
    let completed_turn = boolean_or_default(
        path,
        table.get("completed_turn"),
        "ui.bell.completed_turn",
        true,
    )?;
    let method = match table.get("method") {
        Some(value) => bell_method(path, "ui.bell.method", value)?,
        None => legacy_method
            .map(|value| bell_method(path, "ui.terminal_notification", value))
            .transpose()?
            .unwrap_or_default(),
    };
    Ok(BellConfig {
        requested_input,
        completed_turn,
        method,
    })
}

fn bell_method(path: &Path, name: &str, value: &toml::Value) -> Result<BellMethod, RuntimeError> {
    match value {
        toml::Value::String(value) => match value.as_str() {
            "osc777" => Ok(BellMethod::Osc777),
            "bell" => Ok(BellMethod::Bell),
            _ => Err(RuntimeError::Config {
                path: path.to_path_buf(),
                message: format!("{name} must be \"osc777\" or \"bell\""),
            }),
        },
        _ => Err(RuntimeError::Config {
            path: path.to_path_buf(),
            message: format!("{name} must be a string"),
        }),
    }
}

fn progress_osc_or_default(path: &Path, document: &toml::Value) -> Result<bool, RuntimeError> {
    match document.get("ui").and_then(|ui| ui.get("progress_osc")) {
        None => Ok(true),
        Some(toml::Value::Boolean(value)) => Ok(*value),
        Some(_) => Err(RuntimeError::Config {
            path: path.to_path_buf(),
            message: "ui.progress_osc must be a boolean".into(),
        }),
    }
}

fn title_or_default(path: &Path, document: &toml::Value) -> Result<TitleConfig, RuntimeError> {
    let Some(title) = document.get("ui").and_then(|ui| ui.get("title")) else {
        return Ok(TitleConfig::default());
    };
    let table = title.as_table().ok_or_else(|| RuntimeError::Config {
        path: path.to_path_buf(),
        message: "ui.title must be a table".into(),
    })?;
    let requested_input = match table.get("requested_input") {
        None => RequestedInputTitle::default(),
        Some(toml::Value::String(value)) => RequestedInputTitle::from_name(value),
        Some(toml::Value::Array(values)) if !values.is_empty() => {
            let frames = values
                .iter()
                .enumerate()
                .map(|(index, value)| {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| RuntimeError::Config {
                            path: path.to_path_buf(),
                            message: format!("ui.title.requested_input[{index}] must be a string"),
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            RequestedInputTitle(frames)
        }
        Some(toml::Value::Array(_)) => {
            return Err(RuntimeError::Config {
                path: path.to_path_buf(),
                message: "ui.title.requested_input must not be empty".into(),
            });
        }
        Some(_) => {
            return Err(RuntimeError::Config {
                path: path.to_path_buf(),
                message: "ui.title.requested_input must be a string or array of strings".into(),
            });
        }
    };
    Ok(TitleConfig {
        requested_input,
        progress: progress_or_default(path, table.get("progress"))?,
    })
}

fn progress_or_default(
    path: &Path,
    value: Option<&toml::Value>,
) -> Result<ProgressMode, RuntimeError> {
    let Some(value) = value else {
        return Ok(ProgressMode::default());
    };
    match value {
        toml::Value::Boolean(false) => Ok(ProgressMode::False),
        toml::Value::String(value) => match value.as_str() {
            "braille" => Ok(ProgressMode::Braille),
            "quadrant" => Ok(ProgressMode::Quadrant),
            "arc" => Ok(ProgressMode::Arc),
            "circle" => Ok(ProgressMode::Circle),
            "line" => Ok(ProgressMode::Line),
            "block" => Ok(ProgressMode::Block),
            "dots" => Ok(ProgressMode::Dots),
            "nerd_circle" => Ok(ProgressMode::NerdCircle),
            "nerd_arrow" => Ok(ProgressMode::NerdArrow),
            _ => Err(RuntimeError::Config {
                path: path.to_path_buf(),
                message: "ui.title.progress must be false, braille, quadrant, arc, circle, line, block, dots, nerd_circle, or nerd_arrow".into(),
            }),
        },
        toml::Value::Array(values) if !values.is_empty() => {
            let frames = values
                .iter()
                .enumerate()
                .map(|(index, value)| {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| RuntimeError::Config {
                            path: path.to_path_buf(),
                            message: format!("ui.title.progress[{index}] must be a string"),
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(ProgressMode::Custom(frames))
        }
        toml::Value::Array(_) => Err(RuntimeError::Config {
            path: path.to_path_buf(),
            message: "ui.title.progress must not be empty".into(),
        }),
        _ => Err(RuntimeError::Config {
            path: path.to_path_buf(),
            message: "ui.title.progress must be false, a spinner style string, or an array of strings".into(),
        }),
    }
}

fn positive_usize_or_default(
    path: &Path,
    value: Option<&toml::Value>,
    name: &str,
    default: usize,
) -> Result<usize, RuntimeError> {
    value.map_or(Ok(default), |value| positive_usize_value(path, value, name))
}

fn boolean_or_default(
    path: &Path,
    value: Option<&toml::Value>,
    name: &str,
    default: bool,
) -> Result<bool, RuntimeError> {
    match value {
        None => Ok(default),
        Some(toml::Value::Boolean(value)) => Ok(*value),
        Some(_) => Err(RuntimeError::Config {
            path: path.to_path_buf(),
            message: format!("{name} must be a boolean"),
        }),
    }
}

fn nonnegative_usize_or_default(
    path: &Path,
    value: Option<&toml::Value>,
    name: &str,
    default: usize,
) -> Result<usize, RuntimeError> {
    match value {
        None => Ok(default),
        Some(toml::Value::Integer(value)) if *value >= 0 => {
            usize::try_from(*value).map_err(|error| config_error(path, error))
        }
        Some(_) => Err(RuntimeError::Config {
            path: path.to_path_buf(),
            message: format!("{name} must be a non-negative integer"),
        }),
    }
}

fn positive_usize_value(
    path: &Path,
    value: &toml::Value,
    name: &str,
) -> Result<usize, RuntimeError> {
    match value {
        toml::Value::Integer(value) if *value > 0 => {
            usize::try_from(*value).map_err(|error| config_error(path, error))
        }
        _ => Err(RuntimeError::Config {
            path: path.to_path_buf(),
            message: format!("{name} must be a positive integer"),
        }),
    }
}

fn optional_positive_usize(
    path: &Path,
    value: Option<&toml::Value>,
    name: &str,
) -> Result<Option<usize>, RuntimeError> {
    value
        .map(|value| positive_usize_value(path, value, name))
        .transpose()
}

fn validate_profile_model_references(
    path: &Path,
    agents: &crate::AgentCatalog,
    modes: &BTreeMap<String, crate::ModeProfile>,
    providers: &BTreeMap<String, ProviderSettings>,
    tiers: &BTreeMap<String, Vec<TierCandidate>>,
) -> Result<(), RuntimeError> {
    for (name, agent) in agents.iter() {
        if let Some(model) = agent.model.as_ref() {
            validate_model_selection(
                path,
                &format!("agents.{name}.model"),
                model,
                providers,
                tiers,
            )?;
        }
        for (mode, override_settings) in &agent.mode_overrides {
            if !modes.contains_key(mode) {
                return Err(RuntimeError::Config {
                    path: path.to_path_buf(),
                    message: format!("agents.{name}.modes names an unknown mode: {mode}"),
                });
            }
            if let Some(model) = override_settings.model.as_ref() {
                validate_model_selection(
                    path,
                    &format!("agents.{name}.modes.{mode}.model"),
                    model,
                    providers,
                    tiers,
                )?;
            }
        }
        if let Some(mode) = agent.mode.as_deref()
            && !modes.contains_key(mode)
        {
            return Err(RuntimeError::Config {
                path: path.to_path_buf(),
                message: format!("agents.{name}.mode names an unknown mode: {mode}"),
            });
        }
    }
    for (name, mode) in modes {
        if let Some(model) = mode.model.as_ref() {
            validate_model_selection(
                path,
                &format!("modes.{name}.model"),
                model,
                providers,
                tiers,
            )?;
        }
    }
    Ok(())
}

fn validate_model_selection(
    path: &Path,
    field: &str,
    selection: &ModelSelection,
    providers: &BTreeMap<String, ProviderSettings>,
    tiers: &BTreeMap<String, Vec<TierCandidate>>,
) -> Result<(), RuntimeError> {
    match selection.target.as_ref() {
        Some(ModelTarget::Model(model)) => {
            validate_provider_reference(path, field, model, providers)
        }
        Some(ModelTarget::Tier(tier)) if tiers.contains_key(tier) => Ok(()),
        Some(ModelTarget::Tier(tier)) => Err(RuntimeError::Config {
            path: path.to_path_buf(),
            message: format!("{field} names an unknown tier: {tier}"),
        }),
        None => Ok(()),
    }
}

fn validate_provider_reference(
    path: &Path,
    field: &str,
    model: &crate::ModelRef,
    providers: &BTreeMap<String, ProviderSettings>,
) -> Result<(), RuntimeError> {
    if providers.contains_key(&model.provider) {
        Ok(())
    } else {
        Err(RuntimeError::Config {
            path: path.to_path_buf(),
            message: format!("{field} references an unknown provider: {}", model.provider),
        })
    }
}

fn parse_key_bindings(document: &toml::Value) -> BTreeMap<String, Option<Vec<Option<String>>>> {
    document
        .get("keys")
        .and_then(toml::Value::as_table)
        .map(|table| {
            table
                .iter()
                .map(|(id, value)| {
                    (
                        id.clone(),
                        value.as_array().map(|values| {
                            values
                                .iter()
                                .map(|value| value.as_str().map(str::to_owned))
                                .collect()
                        }),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

fn resolve_all_permission_rules(
    path: &Path,
    document: &toml::Value,
    agents: &crate::AgentCatalog,
    modes: &BTreeMap<String, crate::ModeProfile>,
) -> Result<BTreeMap<(String, String), Vec<crate::PermissionRule>>, RuntimeError> {
    let mut resolved = BTreeMap::new();
    for name in agents.iter().map(|(name, _)| name.as_str()) {
        resolved.insert(
            ("agents".into(), name.into()),
            resolve_permission_rules(path, document, "agents", name)?,
        );
    }
    for name in modes.keys() {
        resolved.insert(
            ("modes".into(), name.clone()),
            resolve_permission_rules(path, document, "modes", name)?,
        );
    }
    Ok(resolved)
}

fn resolve_permission_rules(
    path: &Path,
    document: &toml::Value,
    section: &str,
    name: &str,
) -> Result<Vec<crate::PermissionRule>, RuntimeError> {
    let mut lineage = vec![name];
    if section == "agents" {
        let mut current = name;
        while let Some(parent) = document
            .get("agents")
            .and_then(|agents| agents.get(current))
            .and_then(|agent| agent.get("extends"))
            .and_then(toml::Value::as_str)
        {
            lineage.push(parent);
            current = parent;
        }
        lineage.reverse();
    }
    let mut rules = Vec::new();
    for profile in lineage {
        let Some(definition) = document.get(section).and_then(|value| value.get(profile)) else {
            continue;
        };
        if definition
            .get("permissions_replace")
            .and_then(toml::Value::as_bool)
            == Some(true)
        {
            rules.clear();
        }
        let Some(values) = definition
            .get("permissions")
            .and_then(toml::Value::as_array)
        else {
            continue;
        };
        for (index, value) in values.iter().enumerate() {
            let mut value = value.clone();
            let table = value.as_table_mut().ok_or_else(|| RuntimeError::Config {
                path: path.to_path_buf(),
                message: format!("{section}.{profile}.permissions[{index}] must be a table"),
            })?;
            table
                .entry("id")
                .or_insert_with(|| toml::Value::String(format!("{section}:{profile}:{index}")));
            table
                .entry("source")
                .or_insert_with(|| toml::Value::String("config".into()));
            rules.push(value.try_into().map_err(|error| RuntimeError::Config {
                path: path.to_path_buf(),
                message: format!("invalid {section}.{profile}.permissions[{index}]: {error}"),
            })?);
        }
    }
    Ok(rules)
}

fn validate_providers(
    path: &Path,
    providers: &BTreeMap<String, ProviderSettings>,
) -> Result<(), RuntimeError> {
    for (id, provider) in providers {
        let provider_path = provider_config_path(id);
        validate_nonempty(path, "provider ID", id)?;
        if let Some(variable) = provider.api_key_env.as_deref() {
            validate_environment_variable(path, id, variable)?;
        }
        if let Some(name) = provider.display_name.as_deref() {
            validate_nonempty(path, &format!("{provider_path}.display_name"), name)?;
        }
        if let Some(base_url) = provider.base_url.as_deref() {
            validate_nonempty(path, &format!("{provider_path}.base_url"), base_url)?;
            if reqwest::Url::parse(base_url).is_err()
                || (!base_url.starts_with("https://") && !base_url.starts_with("http://"))
            {
                return Err(RuntimeError::Config {
                    path: path.to_path_buf(),
                    message: format!("{provider_path}.base_url must be an HTTP(S) URL"),
                });
            }
        }
        if let Some(limit) = provider.usage_limit.as_deref() {
            validate_nonempty(path, &format!("{provider_path}.usage_limit"), limit)?;
        }
        if provider.kind == ProviderKind::GoogleVertex {
            if let Some(project) = provider.project.as_deref() {
                validate_nonempty(path, &format!("{provider_path}.project"), project)?;
            }
            if let Some(location) = provider.location.as_deref() {
                validate_nonempty(path, &format!("{provider_path}.location"), location)?;
            }
            if provider.enabled && provider.auth == VertexAuth::Adc && provider.project.is_none() {
                return Err(RuntimeError::Config {
                    path: path.to_path_buf(),
                    message: format!("{provider_path}.project is required when auth is adc"),
                });
            }
        }
        if provider.kind == ProviderKind::OpenAiCompatible
            && provider.protocol == Some(crate::ProviderProtocol::Messages)
        {
            return Err(RuntimeError::Config {
                path: path.to_path_buf(),
                message: format!(
                    "{provider_path}.protocol must be responses or open_ai_compatible"
                ),
            });
        }
        if provider.kind == ProviderKind::OpenAiCompatible {
            if provider.base_url.is_none() {
                return Err(RuntimeError::Config {
                    path: path.to_path_buf(),
                    message: format!("{provider_path}.base_url is required"),
                });
            }
            if provider.api_key_env.is_none() {
                return Err(RuntimeError::Config {
                    path: path.to_path_buf(),
                    message: format!("{provider_path}.api_key_env is required"),
                });
            }
        }
        if let Some(endpoint) = provider.models_endpoint.as_deref() {
            validate_nonempty(path, &format!("{provider_path}.models_endpoint"), endpoint)?;
            if (endpoint.starts_with("http://") || endpoint.starts_with("https://"))
                && reqwest::Url::parse(endpoint).is_err()
            {
                return Err(RuntimeError::Config {
                    path: path.to_path_buf(),
                    message: format!(
                        "{provider_path}.models_endpoint must be a valid URL or relative path"
                    ),
                });
            }
        }
        for model in &provider.models {
            validate_nonempty(path, &format!("{provider_path}.models entry"), model)?;
        }
        for (alias, model) in &provider.aliases {
            validate_nonempty(path, &format!("{provider_path}.aliases key"), alias)?;
            validate_nonempty(path, &format!("{provider_path}.aliases.{alias}"), model)?;
            if alias == model {
                return Err(RuntimeError::Config {
                    path: path.to_path_buf(),
                    message: format!("{provider_path}.aliases.{alias} must not refer to itself"),
                });
            }
        }
        for model in provider.capability_overrides.keys() {
            validate_nonempty(
                path,
                &format!("{provider_path}.capability_overrides key"),
                model,
            )?;
        }
    }
    Ok(())
}

fn validate_environment_variable(
    path: &Path,
    provider: &str,
    variable: &str,
) -> Result<(), RuntimeError> {
    let valid = !variable.is_empty()
        && variable.bytes().enumerate().all(|(index, byte)| {
            byte == b'_' || byte.is_ascii_alphabetic() || (index > 0 && byte.is_ascii_digit())
        });
    if valid {
        Ok(())
    } else {
        Err(RuntimeError::Config {
            path: path.to_path_buf(),
            message: format!(
                "{}.api_key_env is not a valid environment variable name",
                provider_config_path(provider)
            ),
        })
    }
}

fn parse_status_line(
    path: &Path,
    document: &toml::Value,
) -> Result<crate::StatusLineConfig, RuntimeError> {
    let mut config = crate::StatusLineConfig::default();
    let Some(statusline) = document.get("ui").and_then(|ui| ui.get("statusline")) else {
        return Ok(config);
    };
    let table = statusline.as_table().ok_or_else(|| RuntimeError::Config {
        path: path.to_path_buf(),
        message: "ui.statusline must be a table".into(),
    })?;
    if let Some(modules) = table.get("modules") {
        let modules = modules.as_array().ok_or_else(|| RuntimeError::Config {
            path: path.to_path_buf(),
            message: "ui.statusline.modules must be an array of module IDs".into(),
        })?;
        let mut seen = BTreeSet::new();
        config.modules = modules
            .iter()
            .enumerate()
            .map(|(index, value)| {
                let id = value.as_str().ok_or_else(|| RuntimeError::Config {
                    path: path.to_path_buf(),
                    message: format!("ui.statusline.modules[{index}] must be a string"),
                })?;
                let module = id.parse::<crate::StatusLineModule>().map_err(|error| {
                    RuntimeError::Config {
                        path: path.to_path_buf(),
                        message: format!("ui.statusline.modules[{index}]: {error}"),
                    }
                })?;
                if !seen.insert(module) {
                    return Err(RuntimeError::Config {
                        path: path.to_path_buf(),
                        message: format!("ui.statusline.modules contains duplicate {id:?}"),
                    });
                }
                Ok(module)
            })
            .collect::<Result<Vec<_>, _>>()?;
    }
    if let Some(colors) = table.get("colors") {
        let colors = colors.as_table().ok_or_else(|| RuntimeError::Config {
            path: path.to_path_buf(),
            message: "ui.statusline.colors must be a table".into(),
        })?;
        for (id, value) in colors {
            let module =
                id.parse::<crate::StatusLineModule>()
                    .map_err(|error| RuntimeError::Config {
                        path: path.to_path_buf(),
                        message: format!("ui.statusline.colors.{id}: {error}"),
                    })?;
            let value = value.as_str().ok_or_else(|| RuntimeError::Config {
                path: path.to_path_buf(),
                message: format!("ui.statusline.colors.{id} must be a color string"),
            })?;
            let color =
                value
                    .parse::<crate::StatusLineColor>()
                    .map_err(|error| RuntimeError::Config {
                        path: path.to_path_buf(),
                        message: format!("ui.statusline.colors.{id}: {error}"),
                    })?;
            config.colors.insert(module, color);
        }
    }
    Ok(config)
}

pub(crate) fn implicit_table() -> toml_edit::Table {
    let mut table = toml_edit::Table::new();
    table.set_implicit(true);
    table
}

fn ensure_table(document: &mut toml_edit::DocumentMut, key: &str) -> Result<(), RuntimeError> {
    if !document.contains_key(key) {
        document.insert(key, toml_edit::Item::Table(implicit_table()));
    }
    document
        .get(key)
        .and_then(toml_edit::Item::as_table_like)
        .map(|_| ())
        .ok_or_else(|| RuntimeError::InvalidOption(format!("{key} must be a table")))
}

fn ensure_child_table(
    parent: &mut dyn toml_edit::TableLike,
    key: &str,
) -> Result<(), RuntimeError> {
    if !parent.contains_key(key) {
        parent.insert(key, toml_edit::Item::Table(implicit_table()));
    }
    parent
        .get(key)
        .and_then(toml_edit::Item::as_table_like)
        .map(|_| ())
        .ok_or_else(|| RuntimeError::InvalidOption(format!("ui.{key} must be a table")))
}

fn validate_nonempty(path: &Path, field: &str, value: &str) -> Result<String, RuntimeError> {
    let value = value.trim();
    if value.is_empty() {
        Err(RuntimeError::Config {
            path: path.to_path_buf(),
            message: format!("{field} must not be empty"),
        })
    } else {
        Ok(value.into())
    }
}

fn validate_optional(
    path: &Path,
    field: &str,
    value: Option<String>,
) -> Result<Option<String>, RuntimeError> {
    value
        .map(|value| validate_nonempty(path, field, &value))
        .transpose()
}

fn config_error(path: &Path, error: impl std::fmt::Display) -> RuntimeError {
    RuntimeError::Config {
        path: path.to_path_buf(),
        message: error.to_string(),
    }
}

fn configured_mode_order(document: &toml_edit::DocumentMut) -> Vec<String> {
    document
        .get("modes")
        .and_then(toml_edit::Item::as_table_like)
        .map(|table| table.iter().map(|(name, _)| name.to_owned()).collect())
        .unwrap_or_default()
}

/// Replaces a top-level string while retaining the original value's whitespace and comments.
///
/// # Errors
///
/// Returns an error when the key is missing or is not a scalar TOML value.
pub fn set_config_string_preserving_comments(
    document: &mut toml_edit::DocumentMut,
    key: &str,
    value: &str,
) -> Result<(), RuntimeError> {
    let decor = document
        .get(key)
        .and_then(toml_edit::Item::as_value)
        .map(|value| value.decor().clone())
        .ok_or_else(|| RuntimeError::InvalidOption(format!("config key {key} is not a value")))?;
    let mut replacement = toml_edit::Value::from(value);
    *replacement.decor_mut() = decor;
    document[key] = toml_edit::Item::Value(replacement);
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    #[test]
    fn structured_model_tiers_parse_and_legacy_keys_are_rejected() {
        let snapshot = ConfigSnapshot::parse(
            Path::new("tiers.toml"),
            r#"
                version = 1
                default_model = { model = "openai/gpt-main", effort = "high" }
                [tiers]
                small = [{ model = "anthropic/claude-haiku-4-5", effort = "low" }]
                powerful = [{ model = "openai/gpt-powerful", effort = "high" }]
                [providers.openai]
                enabled = true
                [providers.anthropic]
                enabled = true
            "#,
        )
        .unwrap();
        assert_eq!(
            snapshot.default_model().unwrap().to_string(),
            "openai/gpt-main"
        );
        assert_eq!(
            snapshot.default_model().unwrap().effort.as_deref(),
            Some("high")
        );
        assert_eq!(
            snapshot.tier("small").unwrap()[0].model.to_string(),
            "anthropic/claude-haiku-4-5"
        );
        assert_eq!(
            snapshot.tier("powerful").unwrap()[0].effort.as_deref(),
            Some("high")
        );

        for source in [
            "version = 1\nfast_model = 'openai/old'\n",
            "version = 1\npreferred_small_models = ['openai/old']\n",
            "version = 1\ndefault_effort = 'high'\n",
            "version = 1\ndefault_model = 'openai/old'\n",
            "version = 1\n[providers.openai]\nfast_model = 'old'\n",
        ] {
            assert!(ConfigSnapshot::parse(Path::new("legacy.toml"), source).is_err());
        }
    }

    #[test]
    fn tier_candidates_must_be_concrete_and_named_tiers_must_exist() {
        assert!(
            ConfigSnapshot::parse(
                Path::new("tier-reference.toml"),
                "version = 1\n[tiers]\nsmall = [{ tier = 'other' }]\n",
            )
            .is_err()
        );
        assert!(
            ConfigSnapshot::parse(
                Path::new("unknown-tier.toml"),
                "version = 1\n[agents.explore]\nmodel = { tier = 'missing' }\n",
            )
            .is_err()
        );
    }

    use super::*;

    #[test]
    fn overrides_resolve_and_directories_are_created() {
        let temporary = TempDir::new().unwrap();
        let config_file = temporary.path().join("portable/config.toml");
        let data_dir = temporary.path().join("portable/data");
        let paths = AppPaths::resolve(PathOverrides {
            config_file: Some(config_file.clone()),
            data_dir: Some(data_dir.clone()),
        })
        .unwrap();
        paths.create_directories().unwrap();
        assert_eq!(paths.config_file, config_file);
        assert_eq!(
            paths.permissions_file,
            temporary.path().join("portable/permissions.toml")
        );
        assert_eq!(
            paths.global_instruction_dir,
            temporary.path().join("portable")
        );
        assert!(paths.config_file.parent().unwrap().is_dir());
        assert!(paths.data_dir.is_dir());
    }

    #[test]
    fn missing_config_uses_immutable_defaults() {
        let temporary = TempDir::new().unwrap();
        let snapshot = ConfigSnapshot::load(&temporary.path().join("missing.toml")).unwrap();
        assert!(snapshot.machine_fingerprint());
        assert_eq!(snapshot.default_agent(), "general");
        assert_eq!(snapshot.default_mode(), "edit");
        assert_eq!(snapshot.delegation_policy(), DelegationPolicy::Complex);
        assert_eq!(snapshot.subagent_max_concurrent(), 10);
        assert!(snapshot.tier("small").unwrap().is_empty());
        assert!(snapshot.title_generation_enabled());
        assert!(snapshot.automatic_recaps());
        assert_eq!(snapshot.recap_idle_seconds(), 180);
        assert_eq!(snapshot.title_generation_timeout_seconds(), 15);
        assert!(snapshot.web_fetch_redirects().generally_safe());
        assert!(snapshot.web_fetch_redirects().same_site());
        assert!(!snapshot.provider_enabled("mock"));
        assert!(!snapshot.provider_enabled("openai"));
        assert!(!snapshot.provider_enabled("opencode"));
        assert_eq!(
            snapshot.provider("openai").unwrap().api_key_env.as_deref(),
            Some("OPENAI_API_KEY")
        );
        assert_eq!(snapshot.tier("small"), Some(&[][..]));
    }

    #[test]
    fn machine_fingerprint_can_be_disabled_in_top_level_config() {
        let snapshot = ConfigSnapshot::parse(
            Path::new("config.toml"),
            "version = 1\nmachine_fingerprint = false\n",
        )
        .unwrap();
        assert!(!snapshot.machine_fingerprint());
    }

    #[test]
    fn vertex_provider_requires_a_project_for_enabled_adc_and_supports_express() {
        let path = Path::new("config.toml");
        let missing_project = "version = 1\n[providers.google-vertex]\nenabled = true\n";
        assert!(ConfigSnapshot::parse(path, missing_project).is_err());

        let express = ConfigSnapshot::parse(
            path,
            "version = 1\n[providers.google-vertex]\nenabled = true\nauth = \"api-key\"\napi_key_env = \"VERTEX_EXPRESS_KEY\"\n",
        )
        .unwrap();
        let vertex = express.provider("google-vertex").unwrap();
        assert_eq!(vertex.auth, VertexAuth::ApiKey);
        assert_eq!(vertex.api_key_env.as_deref(), Some("VERTEX_EXPRESS_KEY"));
        assert_eq!(vertex.location.as_deref(), None);
    }

    #[test]
    fn delegation_policy_defaults_parses_and_rejects_invalid_values() {
        let path = Path::new("config.toml");
        assert_eq!(
            ConfigSnapshot::parse(path, "version = 1\n")
                .unwrap()
                .delegation_policy(),
            DelegationPolicy::Complex
        );
        for (value, expected) in [
            ("off", DelegationPolicy::Off),
            ("on_demand", DelegationPolicy::OnDemand),
            ("complex", DelegationPolicy::Complex),
            ("aggressive", DelegationPolicy::Aggressive),
            ("always", DelegationPolicy::Always),
        ] {
            let source = format!("version = 1\n[subagents]\nstrategy = \"{value}\"\n");
            assert_eq!(
                ConfigSnapshot::parse(path, &source)
                    .unwrap()
                    .delegation_policy(),
                expected
            );
        }
        for value in ["", "sometimes"] {
            let source = format!("version = 1\n[subagents]\nstrategy = \"{value}\"\n");
            assert!(ConfigSnapshot::parse(path, &source).is_err());
        }
    }

    #[test]
    fn subagent_concurrency_limit_defaults_and_accepts_zero_as_disabled() {
        let path = Path::new("config.toml");
        assert_eq!(
            ConfigSnapshot::parse(path, "version = 1\n")
                .unwrap()
                .subagent_max_concurrent(),
            10
        );
        assert_eq!(
            ConfigSnapshot::parse(path, "version = 1\n[subagents]\nmax_concurrent = 3\n")
                .unwrap()
                .subagent_max_concurrent(),
            3
        );
        let disabled =
            ConfigSnapshot::parse(path, "version = 1\n[subagents]\nmax_concurrent = 0\n").unwrap();
        assert_eq!(disabled.subagent_max_concurrent(), 0);
        assert!(!disabled.subagents_enabled());
        for value in ["-1", "\"many\""] {
            let source = format!("version = 1\n[subagents]\nmax_concurrent = {value}\n");
            assert!(ConfigSnapshot::parse(path, &source).is_err());
        }
    }

    #[test]
    fn empty_config_uses_immutable_defaults() {
        let path = Path::new("empty.toml");
        let empty = ConfigSnapshot::parse(path, "").unwrap();
        let whitespace = ConfigSnapshot::parse(path, " \n\t").unwrap();

        assert_eq!(empty.default_agent(), "general");
        assert_eq!(empty.default_mode(), "edit");
        assert_eq!(empty.default_plan_exit_mode(), "edit");
        assert_eq!(empty.default_agent(), whitespace.default_agent());
        assert_eq!(empty.default_mode(), whitespace.default_mode());
        assert!(!empty.provider_enabled("openai"));
    }

    #[test]
    fn default_plan_exit_mode_inherits_overrides_and_validates() {
        let path = Path::new("config.toml");
        let inherited = ConfigSnapshot::parse(path, "version = 1\ndefault_mode = 'auto'").unwrap();
        assert_eq!(inherited.default_plan_exit_mode(), "auto");

        let planning_default =
            ConfigSnapshot::parse(path, "version = 1\ndefault_mode = 'plan'").unwrap();
        assert_eq!(planning_default.default_plan_exit_mode(), "edit");

        let overridden = ConfigSnapshot::parse(
            path,
            "version = 1\ndefault_mode = 'edit'\ndefault_plan_exit_mode = 'auto'",
        )
        .unwrap();
        assert_eq!(overridden.default_plan_exit_mode(), "auto");

        for source in [
            "version = 1\ndefault_plan_exit_mode = 'missing'",
            "version = 1\ndefault_plan_exit_mode = 'plan'",
            "version = 1\ndefault_plan_exit_mode = 'read'",
            "version = 1\ndefault_plan_exit_mode = 'auto'\n[modes.auto]\nenabled = false",
        ] {
            let error = ConfigSnapshot::parse(path, source).unwrap_err();
            assert!(error.to_string().contains(
                "default_plan_exit_mode must name an enabled, cycleable non-planning mode"
            ));
        }
    }

    #[test]
    fn removed_fast_model_is_rejected() {
        assert!(
            ConfigSnapshot::parse(
                Path::new("config.toml"),
                "version = 1\nfast_model = 'openrouter/deepseek/deepseek-v4-flash-latest'\n",
            )
            .is_err()
        );
    }

    #[test]
    fn fast_defaults_off_and_parses_top_level_value() {
        assert!(!ConfigSnapshot::default().fast());
        let snapshot =
            ConfigSnapshot::parse(Path::new("config.toml"), "version = 1\nfast = true\n").unwrap();
        assert!(snapshot.fast());
    }

    #[test]
    fn title_generation_options_default_and_validate() {
        let path = Path::new("config.toml");
        let configured = ConfigSnapshot::parse(
            path,
            "version = 1\n[title_generation]\nenabled = false\ntimeout_seconds = 42\n",
        )
        .unwrap();
        assert!(!configured.title_generation_enabled());
        assert_eq!(configured.title_generation_timeout_seconds(), 42);

        for value in ["0", "301", "-1", "\"slow\""] {
            let source = format!("version = 1\n[title_generation]\ntimeout_seconds = {value}\n");
            assert!(ConfigSnapshot::parse(path, &source).is_err());
        }
    }

    #[test]
    fn recap_options_default_configure_and_validate() {
        let path = Path::new("config.toml");
        let configured = ConfigSnapshot::parse(
            path,
            "version = 1\nautomatic_recaps = false\nrecap_idle_seconds = 30\n",
        )
        .unwrap();
        assert!(!configured.automatic_recaps());
        assert_eq!(configured.recap_idle_seconds(), 30);
        assert!(ConfigSnapshot::parse(path, "version = 1\nrecap_idle_seconds = 0\n").is_err());
    }

    #[test]
    fn small_tier_candidates_are_ordered() {
        let snapshot = ConfigSnapshot::parse(
            Path::new("config.toml"),
            "version = 1\n[tiers]\nsmall = [{ model = 'openai/custom-small' }, { model = 'openai/gpt-5.6-luna' }]\n[providers.openai]\nenabled = true\n",
        )
        .unwrap();

        assert_eq!(
            snapshot
                .tier("small")
                .unwrap()
                .iter()
                .map(|candidate| candidate.model.to_string())
                .collect::<Vec<_>>(),
            ["openai/custom-small", "openai/gpt-5.6-luna"]
        );
    }

    #[test]
    fn config_is_parsed_and_validated_as_one_snapshot() {
        let snapshot = ConfigSnapshot::parse(
            Path::new("test-config.toml"),
            r#"
                version = 1
                default_agent = "review"
                default_mode = "auto"
                default_model = { model = "openai/test", effort = "high" }

                [limits]
                read_bytes = 42

                [agents.review]
                description = "Review code"
                availability = "user"
            "#,
        )
        .unwrap();
        assert_eq!(snapshot.default_agent(), "review");
        assert_eq!(snapshot.default_mode(), "auto");
        assert_eq!(
            snapshot.default_model().map(ToString::to_string).as_deref(),
            Some("openai/test")
        );
        assert_eq!(
            snapshot.default_model().unwrap().effort.as_deref(),
            Some("high")
        );
        assert_eq!(snapshot.attachment_bytes(), DEFAULT_ATTACHMENT_BYTES);
        assert_eq!(
            snapshot.attachment_hard_cap_bytes(),
            DEFAULT_ATTACHMENT_HARD_CAP_BYTES
        );
        assert_eq!(snapshot.scrollback_reflow_rows(), 2_000);
    }

    #[test]
    fn agent_mode_overrides_validate_mode_names_and_provider_references() {
        let valid = ConfigSnapshot::parse(
            Path::new("config.toml"),
            r#"
            version = 1
            [providers.mock]
            type = "mock"
            enabled = true
            [agents.smart]
            model = { model = "mock/normal" }
            [agents.smart.modes.review]
            model = { model = "mock/review", effort = "high" }
            [modes.review]
            enabled = true
            "#,
        )
        .unwrap();
        let catalog = valid.agent_catalog().unwrap();
        let smart = catalog.get("smart").unwrap();
        assert_eq!(
            smart.mode_overrides["review"]
                .model
                .as_ref()
                .and_then(|selection| selection.effort.as_deref()),
            Some("high")
        );

        let unknown_mode = ConfigSnapshot::parse(
            Path::new("config.toml"),
            "version = 1\n[agents.smart.modes.missing]\nmodel = { effort = 'high' }\n",
        )
        .unwrap_err();
        assert!(unknown_mode.to_string().contains("unknown mode"));

        let unknown_provider = ConfigSnapshot::parse(
            Path::new("config.toml"),
            "version = 1\n[agents.smart.modes.edit]\nmodel = { model = 'other/model' }\n",
        )
        .unwrap_err();
        assert!(unknown_provider.to_string().contains("unknown provider"));
    }

    #[test]
    fn web_search_configuration_is_optional_and_provider_specific() {
        let absent = ConfigSnapshot::parse(Path::new("config.toml"), "version = 1\n").unwrap();
        assert_eq!(absent.web_search().provider(), None);
        assert!(absent.web_search().resolve().is_none());

        let searx = ConfigSnapshot::parse(
            Path::new("config.toml"),
            "version = 1\n[web_search]\nprovider = 'searxng'\n[web_search.searxng]\nurl = 'http://127.0.0.1:8080'\n",
        )
        .unwrap();
        assert_eq!(
            searx.web_search().provider(),
            Some(crate::WebSearchProvider::Searxng)
        );
        assert!(searx.web_search().resolve().is_some());

        let exa = ConfigSnapshot::parse(
            Path::new("config.toml"),
            "version = 1\n[web_search]\nprovider = 'exa'\n[web_search.exa]\napi_key_env_var = 'EXA_API_KEY'\n",
        )
        .unwrap();
        assert_eq!(
            exa.web_search().provider(),
            Some(crate::WebSearchProvider::Exa)
        );
        assert_eq!(
            exa.web_search().provider_statuses()[0].provider,
            crate::WebSearchProvider::Exa
        );
        assert!(exa.web_search().resolve().is_none());
        assert!(
            exa.web_search()
                .provider_statuses()
                .iter()
                .any(|status| status.provider == crate::WebSearchProvider::Exa && !status.ready)
        );
    }

    #[test]
    fn web_fetch_redirects_default_and_accept_independent_overrides() {
        let path = Path::new("config.toml");
        let defaults = ConfigSnapshot::parse(path, "version = 1\n").unwrap();
        assert!(defaults.web_fetch_redirects().generally_safe());
        assert!(defaults.web_fetch_redirects().same_site());

        let configured = ConfigSnapshot::parse(
            path,
            "version = 1\n[web_fetch.redirects]\ngenerally_safe = false\nsame_site = false\n",
        )
        .unwrap();
        assert!(!configured.web_fetch_redirects().generally_safe());
        assert!(!configured.web_fetch_redirects().same_site());
        assert!(
            ConfigSnapshot::parse(path, "version = 1\n[web_fetch.redirects]\nunknown = true\n",)
                .is_ok()
        );
    }

    #[test]
    fn ui_editor_defaults_and_resolves_known_and_custom_commands() {
        let path = Path::new("config.toml");
        let defaults = ConfigSnapshot::parse(path, "version = 1\n").unwrap();
        assert_eq!(defaults.editor(), &UiEditor::BuiltIn);

        let disabled = ConfigSnapshot::parse(path, "version = 1\n[ui]\neditor = false\n").unwrap();
        assert_eq!(disabled.editor(), &UiEditor::Disabled);

        let vscode = ConfigSnapshot::parse(path, "version = 1\n[ui]\neditor = 'vscode'\n").unwrap();
        assert_eq!(
            vscode.editor(),
            &UiEditor::Command {
                command: vec!["code".into()],
                line_command: Some(vec!["code".into(), "--goto".into(), "{path}:{line}".into()]),
                fallback_executables: Vec::new(),
                mode: UiEditorMode::Background,
            }
        );

        let neovim = ConfigSnapshot::parse(path, "version = 1\n[ui]\neditor = 'nvim'\n").unwrap();
        assert_eq!(
            neovim.editor(),
            &UiEditor::Command {
                command: vec!["nvim".into()],
                line_command: Some(vec!["nvim".into(), "+{line}".into(), "{path}".into()]),
                fallback_executables: Vec::new(),
                mode: UiEditorMode::Foreground,
            }
        );

        let custom =
            ConfigSnapshot::parse(path, "version = 1\n[ui]\neditor = ['zed-preview', '-w']\n")
                .unwrap();
        assert_eq!(
            custom.editor(),
            &UiEditor::Command {
                command: vec!["zed-preview".into(), "-w".into()],
                line_command: None,
                fallback_executables: Vec::new(),
                mode: UiEditorMode::Foreground,
            }
        );

        let custom_background = ConfigSnapshot::parse(
            path,
            "version = 1\n[ui]\neditor = { command = ['my-editor', '--reuse-window'], mode = 'background' }\n",
        )
        .unwrap();
        assert_eq!(
            custom_background.editor(),
            &UiEditor::Command {
                command: vec!["my-editor".into(), "--reuse-window".into()],
                line_command: None,
                fallback_executables: Vec::new(),
                mode: UiEditorMode::Background,
            }
        );

        let custom_line = ConfigSnapshot::parse(
            path,
            "version = 1\n[ui]\neditor = { command = ['my-editor'], line_command = ['my-editor', '--line', '{line}', '{path}'], mode = 'background' }\n",
        )
        .unwrap();
        let selected = Path::new("/tmp/selected file.rs");
        assert_eq!(
            custom_line.editor().argv(selected, Some(17)).unwrap(),
            vec!["my-editor", "--line", "17", "/tmp/selected file.rs"]
        );
        assert_eq!(
            custom_line.editor().argv(selected, None).unwrap(),
            vec!["my-editor", "/tmp/selected file.rs"]
        );

        for invalid in [
            "{ command = ['e'], line_command = ['e', '{path}'], mode = 'background' }",
            "{ command = ['e'], line_command = ['e', '{line}'], mode = 'background' }",
            "{ command = ['e'], line_command = 'bad', mode = 'background' }",
        ] {
            let source = format!("version = 1\n[ui]\neditor = {invalid}\n");
            assert!(ConfigSnapshot::parse(path, &source).is_err());
        }

        assert_eq!(
            vscode.editor().argv(selected, Some(12)).unwrap(),
            vec!["code", "--goto", "/tmp/selected file.rs:12"]
        );
        let zed = ConfigSnapshot::parse(path, "version = 1\n[ui]\neditor = 'zed'\n").unwrap();
        assert_eq!(
            zed.editor().argv_candidates(selected, Some(12)),
            vec![
                vec!["zed", "/tmp/selected file.rs:12"],
                vec!["zeditor", "/tmp/selected file.rs:12"],
            ]
        );

        for (preset, expected) in [
            ("zed", vec!["zed", "/tmp/selected file.rs:12"]),
            (
                "intellij",
                vec!["idea", "--line", "12", "/tmp/selected file.rs"],
            ),
            ("sublime", vec!["subl", "/tmp/selected file.rs:12"]),
            ("fleet", vec!["fleet", "/tmp/selected file.rs:12"]),
            ("neovim", vec!["nvim", "+12", "/tmp/selected file.rs"]),
            ("emacs", vec!["emacs", "+12", "/tmp/selected file.rs"]),
        ] {
            let source = format!("version = 1\n[ui]\neditor = {preset:?}\n");
            let config = ConfigSnapshot::parse(path, &source).unwrap();
            assert_eq!(config.editor().argv(selected, Some(12)).unwrap(), expected);
        }

        for name in [
            "vscode",
            "vscode-insiders",
            "cursor",
            "zed",
            "zed-preview",
            "intellij",
            "webstorm",
            "pycharm",
            "rustrover",
            "goland",
            "clion",
            "rider",
            "fleet",
            "sublime",
            "lapce",
            "emacs",
        ] {
            let parsed =
                ConfigSnapshot::parse(path, &format!("version = 1\n[ui]\neditor = {name:?}\n"))
                    .unwrap();
            assert_eq!(
                parsed.editor().mode(),
                Some(UiEditorMode::Background),
                "{name}"
            );
        }
        for name in ["neovim", "vim", "helix"] {
            let parsed =
                ConfigSnapshot::parse(path, &format!("version = 1\n[ui]\neditor = {name:?}\n"))
                    .unwrap();
            assert_eq!(
                parsed.editor().mode(),
                Some(UiEditorMode::Foreground),
                "{name}"
            );
        }

        for invalid in [
            "editor = true",
            "editor = 'unknown-editor'",
            "editor = []",
            "editor = ['zed', '']",
            "editor = { command = ['zed'] }",
            "editor = { command = [], mode = 'background' }",
            "editor = { command = ['zed'], mode = 'detached' }",
            "editor = { command = ['zed'], mode = 'background', wait = true }",
        ] {
            assert!(
                ConfigSnapshot::parse(path, &format!("version = 1\n[ui]\n{invalid}\n")).is_err()
            );
        }
    }

    #[test]
    fn web_search_rejects_invalid_urls_and_allows_inactive_provider_settings() {
        assert!(
            ConfigSnapshot::parse(
                Path::new("config.toml"),
                "version = 1\n[web_search]\nprovider = 'searxng'\n[web_search.searxng]\nurl = 'ftp://example.com'\n",
            )
            .is_err()
        );
        assert!(ConfigSnapshot::parse(
            Path::new("config.toml"),
            "version = 1\n[web_search]\nprovider = 'exa'\n[web_search.searxng]\nurl = 'http://example.com'\n",
        )
        .is_ok());
    }

    #[test]
    fn documented_config_is_copyable() {
        let docs = include_str!("../../../../docs/src/content/docs/configuration.md");
        let example = docs
            .split_once("## Full example")
            .and_then(|(_, section)| section.split_once("```toml\n"))
            .and_then(|(_, fenced)| fenced.split_once("\n```"))
            .map(|(source, _)| source)
            .expect("configuration guide contains its TOML example");

        let snapshot = ConfigSnapshot::parse(Path::new("documented-config.toml"), example)
            .expect("documented config is accepted by the shipped parser");
        assert_eq!(snapshot.default_agent(), "general");
        assert_eq!(snapshot.default_mode(), "edit");
        assert_eq!(
            snapshot.default_model().map(ToString::to_string).as_deref(),
            Some("openai/gpt-5.6-luna")
        );
        assert_eq!(
            snapshot.default_model().unwrap().effort.as_deref(),
            Some("high")
        );
        assert!(snapshot.provider_enabled("openai"));
        assert_eq!(snapshot.attachment_bytes(), 262_144);
        assert_eq!(snapshot.attachment_hard_cap_bytes(), 1_048_576);
        assert_eq!(snapshot.scrollback_reflow_rows(), 2_000);
        assert_eq!(snapshot.diff_context_lines(), 3);
        assert!(snapshot.modes().unwrap().contains_key("edit"));
        for section in [
            "## Core options",
            "## Search and fetch",
            "## Providers and models",
            "## Agents",
            "## Modes",
            "### Inline permission rules",
            "## Context, retention, skills, and worktrees",
            "## Interface and files",
            "### Keybindings",
        ] {
            assert!(
                docs.contains(section),
                "missing configuration section {section}"
            );
        }
    }

    #[test]
    fn scrollback_reflow_cap_is_configurable() {
        let snapshot = ConfigSnapshot::parse(
            Path::new("ui.toml"),
            "version = 1\n[ui]\nscrollback_reflow_rows = 321\n",
        )
        .unwrap();
        assert_eq!(snapshot.scrollback_reflow_rows(), 321);
    }

    #[test]
    fn composer_height_is_configurable() {
        let snapshot = ConfigSnapshot::parse(
            Path::new("ui.toml"),
            "version = 1\n[ui]\ncomposer_max_rows = 12\n",
        )
        .unwrap();
        assert_eq!(snapshot.composer_max_rows(), Some(12));
        assert_eq!(ConfigSnapshot::default().composer_max_rows(), None);
    }

    #[test]
    fn files_sidebar_width_is_configurable() {
        let snapshot = ConfigSnapshot::parse(
            Path::new("ui.toml"),
            "version = 1\n[ui.files]\nwidth = 41\n",
        )
        .unwrap();
        assert_eq!(snapshot.files_width(), 41);
        assert_eq!(ConfigSnapshot::default().files_width(), DEFAULT_FILES_WIDTH);
        assert!(
            ConfigSnapshot::parse(
                Path::new("invalid-ui.toml"),
                "version = 1\n[ui.files]\nwidth = 0\n",
            )
            .is_err()
        );
    }

    #[test]
    fn ui_limits_use_defaults_and_reject_non_positive_values() {
        let defaults = ConfigSnapshot::parse(Path::new("ui.toml"), "version = 1\n").unwrap();
        assert_eq!(defaults.scrollback_reflow_rows(), 2_000);
        assert_eq!(defaults.diff_context_lines(), 3);
        assert_eq!(defaults.composer_max_rows(), None);
        assert_eq!(defaults.files_width(), DEFAULT_FILES_WIDTH);
        assert_eq!(defaults.title().requested_input().frames(), [""]);
        assert!(defaults.bell().requested_input());
        assert!(defaults.bell().completed_turn());
        assert_eq!(defaults.bell().method(), BellMethod::Osc777);

        for field in [
            "scrollback_reflow_rows",
            "composer_max_rows",
            "diff_context_lines",
        ] {
            let source = format!("version = 1\n[ui]\n{field} = 0\n");
            assert!(ConfigSnapshot::parse(Path::new("invalid-ui.toml"), &source).is_err());
        }
    }

    #[test]
    fn open_links_defaults_on_and_requires_a_boolean() {
        let path = Path::new("ui.toml");
        assert!(
            ConfigSnapshot::parse(path, "version = 1\n")
                .unwrap()
                .open_links()
        );

        for enabled in [true, false] {
            let source = format!("version = 1\n[ui]\nopen_links = {enabled}\neditor = false\n");
            let snapshot = ConfigSnapshot::parse(path, &source).unwrap();
            assert_eq!(snapshot.open_links(), enabled);
            assert!(!snapshot.editor().enabled());
        }

        for value in ["'true'", "'false'", "1", "[]", "{}"] {
            let source = format!("version = 1\n[ui]\nopen_links = {value}\n");
            let error = ConfigSnapshot::parse(path, &source).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("ui.open_links must be a boolean")
            );
        }
    }

    #[test]
    fn bell_settings_accept_valid_values_and_reject_invalid_values() {
        let configured = ConfigSnapshot::parse(
            Path::new("ui.toml"),
            "version = 1\n[ui.bell]\nrequested_input = false\ncompleted_turn = false\nmethod = 'bell'\n",
        )
        .unwrap();
        assert!(!configured.bell().requested_input());
        assert!(!configured.bell().completed_turn());
        assert_eq!(configured.bell().method(), BellMethod::Bell);
        assert!(
            ConfigSnapshot::parse(
                Path::new("invalid-ui.toml"),
                "version = 1\n[ui.bell]\nrequested_input = 'yes'\n",
            )
            .is_err()
        );
        assert!(
            ConfigSnapshot::parse(
                Path::new("invalid-ui.toml"),
                "version = 1\n[ui.bell]\nmethod = 'desktop'\n",
            )
            .is_err()
        );
        let legacy = ConfigSnapshot::parse(
            Path::new("legacy-ui.toml"),
            "version = 1\n[ui]\nterminal_notification = 'bell'\n",
        )
        .unwrap();
        assert_eq!(legacy.bell().method(), BellMethod::Bell);
    }

    #[test]
    fn progress_settings_accept_osc_and_spinner_styles() {
        let styles = [
            ("braille", ProgressMode::Braille),
            ("quadrant", ProgressMode::Quadrant),
            ("arc", ProgressMode::Arc),
            ("circle", ProgressMode::Circle),
            ("line", ProgressMode::Line),
            ("block", ProgressMode::Block),
            ("dots", ProgressMode::Dots),
            ("nerd_circle", ProgressMode::NerdCircle),
            ("nerd_arrow", ProgressMode::NerdArrow),
        ];
        for (value, expected) in styles {
            let config = ConfigSnapshot::parse(
                Path::new("ui.toml"),
                &format!("version = 1\n[ui.title]\nprogress = '{value}'\n"),
            )
            .unwrap();
            assert_eq!(config.title().progress(), &expected);
        }
        let disabled = ConfigSnapshot::parse(
            Path::new("ui.toml"),
            "version = 1\n[ui.title]\nprogress = false\n",
        )
        .unwrap();
        assert_eq!(disabled.title().progress(), &ProgressMode::False);
        let osc = ConfigSnapshot::parse(
            Path::new("ui.toml"),
            "version = 1\n[ui]\nprogress_osc = false\n[ui.title]\nprogress = false\n",
        )
        .unwrap();
        assert!(!osc.progress_osc());
        assert_eq!(osc.title().progress(), &ProgressMode::False);
        let custom = ConfigSnapshot::parse(
            Path::new("ui.toml"),
            "version = 1\n[ui.title]\nprogress = ['.', '..', '...']\n",
        )
        .unwrap();
        assert_eq!(custom.title().progress().frame(0), Some("."));
        assert_eq!(custom.title().progress().frame(3), Some("."));
        assert!(ConfigSnapshot::default().progress_osc());
        assert_eq!(
            ConfigSnapshot::default().title().progress(),
            &ProgressMode::False
        );
    }

    #[test]
    fn progress_settings_reject_invalid_values() {
        for value in ["osc", "osc123", "", "spinner"] {
            assert!(
                ConfigSnapshot::parse(
                    Path::new("invalid-ui.toml"),
                    &format!("version = 1\n[ui.title]\nprogress = '{value}'\n"),
                )
                .is_err()
            );
        }
        assert!(
            ConfigSnapshot::parse(
                Path::new("invalid-ui.toml"),
                "version = 1\n[ui.title]\nprogress = true\n",
            )
            .is_err()
        );
        for value in ["[]", "[1]"] {
            assert!(
                ConfigSnapshot::parse(
                    Path::new("invalid-ui.toml"),
                    &format!("version = 1\n[ui.title]\nprogress = {value}\n"),
                )
                .is_err()
            );
        }
        assert!(
            ConfigSnapshot::parse(
                Path::new("invalid-ui.toml"),
                "version = 1\n[ui]\nprogress_osc = 'true'\n",
            )
            .is_err()
        );
    }

    #[test]
    fn progress_frames_cycle_without_byte_indexing() {
        let mode = ProgressMode::Braille;
        let frames = mode.frames().unwrap();
        assert_eq!(frames.len(), 10);
        assert_eq!(mode.frame(0), Some("⠋"));
        assert_eq!(mode.frame(frames.len()), Some("⠋"));
        assert_eq!(mode.frame(9), Some("⠏"));
        assert_eq!(ProgressMode::False.frame(0), None);
    }

    #[test]
    fn dots_progress_clockwise() {
        assert_eq!(
            ProgressMode::Dots.frames().unwrap(),
            &["⣾", "⣷", "⣯", "⣟", "⡿", "⢿", "⣻", "⣽"]
        );
    }

    #[test]
    fn requested_input_title_accepts_presets_strings_and_arrays() {
        assert_eq!(RequestedInputTitle::from_name("warning").frames(), [""]);
        assert_eq!(
            RequestedInputTitle::from_name("warnings").frames(),
            ["", ""]
        );
        assert_eq!(RequestedInputTitle::from_name("triangle").frames(), ["▲"]);
        assert_eq!(
            RequestedInputTitle::from_name("exclamation").frames(),
            ["!", "."]
        );
        let snapshot = ConfigSnapshot::parse(
            Path::new("ui.toml"),
            "version = 1\n[ui.title]\nrequested_input = 'triangles'\n",
        )
        .unwrap();
        assert_eq!(snapshot.title().requested_input().frames(), ["▲", "△"]);

        let custom = ConfigSnapshot::parse(
            Path::new("ui.toml"),
            "version = 1\n[ui.title]\nrequested_input = ['!', '!!']\n",
        )
        .unwrap();
        assert_eq!(custom.title().requested_input().frame(0), Some("!"));
        assert_eq!(custom.title().requested_input().frame(2), Some("!"));
        assert_eq!(custom.title().requested_input().frame(3), Some("!!"));
        assert!(
            ConfigSnapshot::parse(
                Path::new("invalid-ui.toml"),
                "version = 1\n[ui.title]\nrequested_input = []\n",
            )
            .is_err()
        );
    }

    #[test]
    fn statusline_layout_and_colors_are_validated() {
        let snapshot = ConfigSnapshot::parse(
            Path::new("ui.toml"),
            r##"
                version = 1
                [ui.statusline]
                modules = ["provider_usage", "token_rate", "model", "mode"]
                [ui.statusline.colors]
                model = "#7aA2f7"
                mode = "light-magenta"
            "##,
        )
        .unwrap();
        assert_eq!(
            snapshot.status_line().modules,
            [
                crate::StatusLineModule::ProviderUsage,
                crate::StatusLineModule::TokenRate,
                crate::StatusLineModule::Model,
                crate::StatusLineModule::Mode
            ]
        );
        assert_eq!(
            snapshot.status_line().color(crate::StatusLineModule::Model),
            crate::StatusLineColor::Rgb(122, 162, 247)
        );

        for invalid in [
            "modules = ['mode', 'mode']",
            "modules = ['unknown']",
            "modules = 'mode'",
            "modules = ['mode']\n[ui.statusline.colors]\nmode = '#bad'",
            "modules = ['mode']\n[ui.statusline.colors]\nunknown = 'red'",
        ] {
            let source = format!("version = 1\n[ui.statusline]\n{invalid}\n");
            assert!(ConfigSnapshot::parse(Path::new("invalid-ui.toml"), &source).is_err());
        }
    }

    #[test]
    fn chatgpt_defaults_to_the_normal_weekly_usage_limit_and_allows_an_override() {
        let default = ConfigSnapshot::default();
        assert_eq!(
            default
                .provider("chatgpt")
                .and_then(|provider| provider.usage_limit.as_deref()),
            Some("codex:weekly")
        );

        let configured = ConfigSnapshot::parse(
            Path::new("usage-limit.toml"),
            "version = 1\n[providers.chatgpt]\nusage_limit = 'codex_bengalfox:weekly'\n",
        )
        .unwrap();
        assert_eq!(
            configured
                .provider("chatgpt")
                .and_then(|provider| provider.usage_limit.as_deref()),
            Some("codex_bengalfox:weekly")
        );
    }

    #[test]
    fn empty_statusline_layout_is_distinct_from_the_default() {
        let snapshot = ConfigSnapshot::parse(
            Path::new("ui.toml"),
            "version = 1\n[ui.statusline]\nmodules = []\n",
        )
        .unwrap();
        assert!(snapshot.status_line().modules.is_empty());
        assert_eq!(
            ConfigSnapshot::default().status_line().modules,
            vec![
                crate::StatusLineModule::Mode,
                crate::StatusLineModule::Agent,
                crate::StatusLineModule::Model,
                crate::StatusLineModule::Provider,
                crate::StatusLineModule::Fast,
                crate::StatusLineModule::Context,
                crate::StatusLineModule::ProviderUsage,
                crate::StatusLineModule::Cost,
                crate::StatusLineModule::Hint,
            ]
        );
    }

    #[test]
    fn attachment_limits_are_configurable_and_validated_together() {
        let snapshot = ConfigSnapshot::parse(
            Path::new("limits.toml"),
            "version = 1\n[limits]\nattachment_bytes = 12\nattachment_hard_cap_bytes = 34\n",
        )
        .unwrap();
        assert_eq!(snapshot.attachment_bytes(), 12);
        assert_eq!(snapshot.attachment_hard_cap_bytes(), 34);

        for invalid in [
            "attachment_bytes = 0\nattachment_hard_cap_bytes = 34",
            "attachment_bytes = 35\nattachment_hard_cap_bytes = 34",
            "attachment_bytes = 'large'\nattachment_hard_cap_bytes = 34",
        ] {
            let source = format!("version = 1\n[limits]\n{invalid}\n");
            assert!(ConfigSnapshot::parse(Path::new("invalid-limits.toml"), &source).is_err());
        }
    }

    #[test]
    fn editable_copy_preserves_comments() {
        let mut editable = "# user comment\nversion = 1\ndefault_mode = 'read' # keep me\n"
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        set_config_string_preserving_comments(&mut editable, "default_mode", "auto").unwrap();
        let rendered = editable.to_string();
        assert!(rendered.contains("# user comment"));
        assert!(rendered.contains("# keep me"));
    }

    #[test]
    fn model_defaults_persist_atomically_without_losing_comments() {
        let temporary = TempDir::new().unwrap();
        let path = temporary.path().join("config.toml");
        std::fs::write(
            &path,
            "# keep me\nversion = 1\ndefault_mode = \"read\" # inline\n",
        )
        .unwrap();
        let store = ConfigStore::open(&path).unwrap();
        store
            .persist_model_defaults("openai/gpt-test", Some("high"))
            .unwrap();
        let persisted = std::fs::read_to_string(&path).unwrap();
        assert!(persisted.contains("# keep me"));
        assert!(persisted.contains("# inline"));
        let reloaded = ConfigSnapshot::load(&path).unwrap();
        assert_eq!(
            reloaded.default_model().map(ToString::to_string).as_deref(),
            Some("openai/gpt-test")
        );
        assert_eq!(
            reloaded.default_model().unwrap().effort.as_deref(),
            Some("high")
        );
    }

    #[test]
    fn model_favourites_round_trip_as_full_model_references() {
        let temporary = TempDir::new().unwrap();
        let path = temporary.path().join("config.toml");
        std::fs::write(&path, "version = 1\n").unwrap();
        let store = ConfigStore::open(&path).unwrap();
        let favourites = BTreeSet::from([
            crate::ModelRef::parse("openai/gpt-test").unwrap(),
            crate::ModelRef::parse("openrouter/openai/gpt-test").unwrap(),
        ]);

        store.persist_model_favourites(&favourites).unwrap();

        let reloaded = ConfigSnapshot::load(&path).unwrap();
        assert_eq!(reloaded.favourite_models(), &favourites);
    }

    #[test]
    fn statusline_persistence_preserves_comments_and_disabled_colors() {
        let temporary = TempDir::new().unwrap();
        let path = temporary.path().join("config.toml");
        std::fs::write(
            &path,
            "# keep me\nversion = 1\n[ui]\ncomposer_max_rows = 8\n",
        )
        .unwrap();
        let store = ConfigStore::open(&path).unwrap();
        let mut statusline = crate::StatusLineConfig {
            modules: vec![crate::StatusLineModule::Mode],
            ..crate::StatusLineConfig::default()
        };
        statusline.colors.insert(
            crate::StatusLineModule::Provider,
            crate::StatusLineColor::Rgb(1, 2, 3),
        );
        statusline.colors.insert(
            crate::StatusLineModule::Mode,
            crate::StatusLineColor::Rgb(4, 5, 6),
        );

        store.persist_status_line(&statusline).unwrap();

        let persisted = std::fs::read_to_string(&path).unwrap();
        assert!(persisted.contains("# keep me"));
        assert!(persisted.contains("composer_max_rows = 8"));
        assert!(persisted.contains("\n[ui]\n"));
        let reloaded = ConfigSnapshot::load(&path).unwrap();
        assert_eq!(
            reloaded.status_line().modules,
            [crate::StatusLineModule::Mode]
        );
        assert_eq!(
            reloaded
                .status_line()
                .color(crate::StatusLineModule::Provider),
            crate::StatusLineColor::Rgb(1, 2, 3)
        );
        assert_eq!(
            reloaded.status_line().color(crate::StatusLineModule::Mode),
            crate::StatusLineColor::White
        );

        statusline.modules = vec![crate::StatusLineModule::Mode, crate::StatusLineModule::Mode];
        assert!(store.persist_status_line(&statusline).is_err());
    }

    #[test]
    fn provider_enablement_materializes_an_implicit_builtin_atomically() {
        let temporary = TempDir::new().unwrap();
        let path = temporary.path().join("config.toml");
        let store = ConfigStore::open(&path).unwrap();

        store.persist_provider_enabled("openai", true).unwrap();

        let persisted = std::fs::read_to_string(&path).unwrap();
        assert!(persisted.contains("[providers.openai]"));
        assert!(!persisted.contains("[providers]\n"));
        assert!(!persisted.contains("type ="));
        assert!(!persisted.contains("api_key_env"));
        let reloaded = ConfigSnapshot::load(&path).unwrap();
        let openai = reloaded.provider("openai").unwrap();
        assert_eq!(openai.kind, ProviderKind::Openai);
        assert!(openai.enabled);
        assert_eq!(openai.api_key_env.as_deref(), Some("OPENAI_API_KEY"));

        store.persist_provider_enabled("opencode", true).unwrap();
        let reloaded = ConfigSnapshot::load(&path).unwrap();
        let opencode = reloaded.provider("opencode").unwrap();
        assert_eq!(opencode.kind, ProviderKind::Opencode);
        assert!(opencode.enabled);
        assert_eq!(opencode.api_key_env.as_deref(), Some("OPENCODE_API_KEY"));

        store
            .persist_provider_enabled("github-copilot", true)
            .unwrap();
        let reloaded = ConfigSnapshot::load(&path).unwrap();
        let copilot = reloaded.provider("github-copilot").unwrap();
        assert_eq!(copilot.kind, ProviderKind::GithubCopilot);
        assert!(copilot.enabled);
        assert_eq!(copilot.api_key_env, None);
    }

    #[test]
    fn provider_enablement_cleans_redundant_builtin_table_and_type() {
        let temporary = TempDir::new().unwrap();
        let path = temporary.path().join("config.toml");
        std::fs::write(
            &path,
            "version = 1\n[providers]\n[providers.openai]\ntype = 'openai'\nenabled = false\n",
        )
        .unwrap();
        let store = ConfigStore::open(&path).unwrap();

        store.persist_provider_enabled("openai", true).unwrap();

        let persisted = std::fs::read_to_string(&path).unwrap();
        assert!(persisted.contains("[providers]\n"));
        assert!(!persisted.contains("type ="));
        assert!(persisted.contains("[providers.openai]"));
        assert!(
            ConfigSnapshot::load(&path)
                .unwrap()
                .provider_enabled("openai")
        );
    }

    #[test]
    fn unsupported_version_and_invalid_mode_are_rejected() {
        let path = Path::new("bad.toml");
        assert!(ConfigSnapshot::parse(path, "version = 2").is_err());
        assert!(ConfigSnapshot::parse(path, "version = 1\ndefault_mode = 'unsafe'").is_err());
        let edit = ConfigSnapshot::parse(path, "version = 1\ndefault_mode = 'edit'").unwrap();
        assert_eq!(edit.default_mode(), "edit");
        assert!(edit.modes().unwrap().contains_key("edit"));
    }

    #[test]
    fn providers_require_explicit_enablement_and_preserve_catalog_customization() {
        let snapshot = ConfigSnapshot::parse(
            Path::new("providers.toml"),
            r#"
                version = 1

                [providers.openai]
                type = "openai"
                enabled = false
                api_key_env = "OPENAI_API_KEY"
                models = ["gpt-manual"]

                [providers.openai.aliases]
                daily = "gpt-discovered"

                [providers.openai.capability_overrides.gpt-manual]
                context_window = 12345
                supports_tools = true
            "#,
        )
        .unwrap();
        let provider = snapshot.provider("openai").unwrap();
        assert!(!provider.enabled);
        assert_eq!(provider.models, ["gpt-manual"]);
        assert_eq!(provider.aliases["daily"], "gpt-discovered");
        assert_eq!(
            provider.capability_overrides["gpt-manual"].context_window,
            Some(12_345)
        );
    }

    #[test]
    fn builtin_providers_are_top_level_and_custom_providers_are_nested() {
        let builtin = ConfigSnapshot::parse(
            Path::new("providers.toml"),
            "version = 1\n[providers.chatgpt]\nenabled = true\n",
        )
        .unwrap();
        assert_eq!(
            builtin.provider("chatgpt").unwrap().kind,
            ProviderKind::Chatgpt
        );

        let custom = "version = 1\n[providers.custom.local]\ntype = 'openai-compatible'\nenabled = true\nbase_url = 'http://localhost/v1'\napi_key_env = 'LOCAL_KEY'\n";
        let custom = ConfigSnapshot::parse(Path::new("providers.toml"), custom).unwrap();
        assert_eq!(
            custom.provider("local").unwrap().kind,
            ProviderKind::OpenAiCompatible
        );

        let old_shape =
            "version = 1\n[providers.local]\ntype = 'openai-compatible'\nenabled = true\n";
        let old_shape = ConfigSnapshot::parse(Path::new("providers.toml"), old_shape).unwrap();
        assert!(old_shape.provider("local").is_none());

        let missing_type = "version = 1\n[providers.custom.local]\nenabled = true\n";
        let error = ConfigSnapshot::parse(Path::new("providers.toml"), missing_type).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("providers.custom.local.type is required")
        );
    }

    #[test]
    fn unknown_config_fields_and_removed_model_keys_are_rejected() {
        assert!(
            ConfigSnapshot::parse(
                Path::new("forward-compatible.toml"),
                r#"
                version = 1
                default_model = "google/gemini-future"
                fast_model = "google/gemini-fast"
                preferred_small_models = ["google/gemini-small"]
                favourite_models = ["google/gemini-future"]
                future_top_level = true

                [providers.google]
                type = "future-provider"
                endpoint = "https://example.com"

                [web_fetch.redirects]
                future_redirect_setting = true
            "#,
            )
            .is_err()
        );
    }

    #[test]
    fn custom_provider_enablement_stays_in_the_custom_namespace() {
        let temporary = TempDir::new().unwrap();
        let path = temporary.path().join("config.toml");
        std::fs::write(
            &path,
            "version = 1\n[providers.custom.local]\ntype = 'openai-compatible'\nenabled = false\nbase_url = 'http://localhost/v1'\napi_key_env = 'LOCAL_KEY'\n",
        )
        .unwrap();
        let store = ConfigStore::open(&path).unwrap();

        store.persist_provider_enabled("local", true).unwrap();

        let persisted = std::fs::read_to_string(&path).unwrap();
        assert!(persisted.contains("[providers.custom.local]"));
        assert!(!persisted.contains("[providers.local]"));
        assert!(
            ConfigSnapshot::load(&path)
                .unwrap()
                .provider_enabled("local")
        );
    }

    #[test]
    fn invalid_provider_environment_variable_is_rejected() {
        let source = r#"
            version = 1
            [providers.openai]
            type = "openai"
            api_key_env = "NOT-VALID"
        "#;
        assert!(ConfigSnapshot::parse(Path::new("bad-provider.toml"), source).is_err());
    }

    #[test]
    fn inline_mode_and_agent_permission_rules_receive_stable_ids() {
        let snapshot = ConfigSnapshot::parse(
            Path::new("config.toml"),
            "version = 1\n[[modes.read.permissions]]\neffect = 'deny'\ntool = 'apply_patch'\n[[agents.general.permissions]]\neffect = 'allow'\ntool = 'read'\n[agents.child]\nextends = 'general'\n[[agents.child.permissions]]\neffect = 'deny'\ntool = 'bash'\n",
        ).unwrap();
        let mode = snapshot.permission_rules("modes", "read").unwrap();
        let agent = snapshot.permission_rules("agents", "general").unwrap();
        assert_eq!(mode[0].id, "modes:read:0");
        assert_eq!(agent[0].id, "agents:general:0");
        let child = snapshot.permission_rules("agents", "child").unwrap();
        assert_eq!(
            child
                .iter()
                .map(|rule| rule.id.as_str())
                .collect::<Vec<_>>(),
            vec!["agents:general:0", "agents:child:0"]
        );
    }

    #[test]
    fn inherited_agent_permissions_append_or_replace_in_parent_first_order() {
        let appended = ConfigSnapshot::parse(
            Path::new("permissions-inheritance.toml"),
            "version = 1\n[agents.base]\n[[agents.base.permissions]]\neffect = 'deny'\ntool = 'bash'\n[agents.child]\nextends = 'base'\n[[agents.child.permissions]]\neffect = 'allow'\ntool = 'read'\n",
        )
        .unwrap();
        let rules = appended.permission_rules("agents", "child").unwrap();
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].id, "agents:base:0");
        assert_eq!(rules[1].id, "agents:child:0");

        let replaced = ConfigSnapshot::parse(
            Path::new("permissions-replacement.toml"),
            "version = 1\n[agents.base]\n[[agents.base.permissions]]\neffect = 'deny'\ntool = 'bash'\n[agents.child]\nextends = 'base'\npermissions_replace = true\n[[agents.child.permissions]]\neffect = 'allow'\ntool = 'read'\n",
        )
        .unwrap();
        let rules = replaced.permission_rules("agents", "child").unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].id, "agents:child:0");
    }

    #[test]
    fn shell_safety_defaults_and_validation() {
        let default = ConfigSnapshot::parse(Path::new("default.toml"), "version = 1").unwrap();
        assert_eq!(default.shell().safe_level, 3);
        assert!(!default.shell().safe_write);
        let enabled = ConfigSnapshot::parse(
            Path::new("enabled.toml"),
            "version = 1\n[shell]\nsafe_level = 3\nsafe_write = true\n",
        )
        .unwrap();
        assert_eq!(enabled.shell().safe_level, 3);
        assert!(enabled.shell().safe_write);
        for level in [-2, 4] {
            assert!(
                ConfigSnapshot::parse(
                    Path::new("invalid.toml"),
                    &format!("version = 1\n[shell]\nsafe_level = {level}\n"),
                )
                .is_err()
            );
        }
    }

    #[test]
    fn bundled_skills_default_on_and_can_be_disabled() {
        let default = ConfigSnapshot::parse(Path::new("default.toml"), "version = 1").unwrap();
        assert!(default.bundled_skills_enabled());
        let disabled = ConfigSnapshot::parse(
            Path::new("disabled.toml"),
            "version = 1\n[skills.bundled]\nenabled = false\n",
        )
        .unwrap();
        assert!(!disabled.bundled_skills_enabled());
    }

    #[test]
    fn shell_terminal_mode_defaults_to_normal_and_accepts_dumb() {
        let default = ConfigSnapshot::parse(Path::new("default.toml"), "version = 1").unwrap();
        assert_eq!(default.shell().terminal_mode, TerminalMode::Normal);
        let dumb = ConfigSnapshot::parse(
            Path::new("dumb.toml"),
            "version = 1\n[shell]\nterminal_mode = 'dumb'\n",
        )
        .unwrap();
        assert_eq!(dumb.shell().terminal_mode, TerminalMode::Dumb);
    }
    #[test]
    fn conversation_cleanup_defaults_match_retention_policy() {
        let cleanup = ConfigSnapshot::default().conversation_cleanup();
        assert!(cleanup.automatic());
        assert_eq!(cleanup.max_size(), Some(10_000_000_000));
        assert_eq!(
            cleanup.max_age(),
            Some(std::time::Duration::from_secs(365 * 86_400))
        );
        assert_eq!(cleanup.max_conversations(), None);
        assert_eq!(cleanup.action(), CleanupAction::Trash);
    }

    #[test]
    fn conversation_cleanup_limits_can_be_configured_or_disabled_independently() {
        let snapshot = ConfigSnapshot::parse(
            Path::new("cleanup.toml"),
            "version = 1\n[conversation_cleanup]\nautomatic = false\nmax_size = '1.5gb'\nmax_age = false\nmax_conversations = 42\naction = 'delete'\n",
        ).unwrap();
        let cleanup = snapshot.conversation_cleanup();
        assert!(!cleanup.automatic());
        assert_eq!(cleanup.max_size(), Some(1_500_000_000));
        assert_eq!(cleanup.max_age(), None);
        assert_eq!(cleanup.max_conversations(), Some(42));
        assert_eq!(cleanup.action(), CleanupAction::Delete);

        let disabled = ConfigSnapshot::parse(
            Path::new("cleanup.toml"),
            "version = 1\n[conversation_cleanup]\nmax_size = false\nmax_age = '2w'\nmax_conversations = false\n",
        ).unwrap().conversation_cleanup();
        assert_eq!(disabled.max_size(), None);
        assert_eq!(
            disabled.max_age(),
            Some(std::time::Duration::from_secs(14 * 86_400))
        );
        assert_eq!(disabled.max_conversations(), None);
    }

    #[test]
    fn conversation_cleanup_rejects_invalid_limits_and_actions() {
        for setting in [
            "max_size = '0gb'",
            "max_size = '-1gb'",
            "max_size = 'wat'",
            "max_age = '0d'",
            "max_age = '-1d'",
            "max_age = 4",
            "max_conversations = 0",
            "max_conversations = -1",
            "action = 'archive'",
        ] {
            let source = format!("version = 1\n[conversation_cleanup]\n{setting}\n");
            assert!(
                ConfigSnapshot::parse(Path::new("bad-cleanup.toml"), &source).is_err(),
                "{setting}"
            );
        }
    }

    #[test]
    fn ui_themes_supply_tokens_and_allow_color_overrides() {
        let dark = ConfigSnapshot::parse(Path::new("dark.toml"), "version = 1").unwrap();
        assert_eq!(dark.ui_theme(), UiTheme::Dark);
        assert_eq!(dark.ui_colors(), UiColors::for_theme(UiTheme::Dark));

        let light = ConfigSnapshot::parse(
            Path::new("light.toml"),
            "version = 1\n[ui]\ntheme = 'light'\n[ui.colors]\nbackground = '#FAFAFA'\nhighlight = '#123456'\nmuted = 'magenta'\n",
        )
        .unwrap();
        assert_eq!(light.ui_theme(), UiTheme::Light);
        assert_eq!(
            light.ui_colors().highlight,
            crate::StatusLineColor::Rgb(18, 52, 86)
        );
        assert_eq!(light.ui_colors().muted, crate::StatusLineColor::Magenta);
        assert_eq!(
            light.ui_colors().background,
            Some(crate::StatusLineColor::Rgb(250, 250, 250))
        );
        assert_eq!(
            light.ui_colors().surface,
            UiColors::for_theme(UiTheme::Light).surface
        );
    }

    #[test]
    fn ui_diff_mode_defaults_to_conversation_and_accepts_both_modes() {
        let defaults = ConfigSnapshot::parse(Path::new("config.toml"), "version = 1").unwrap();
        assert_eq!(defaults.diff_mode(), UiDiffMode::Conversation);
        assert_eq!(UiDiffMode::default(), UiDiffMode::Conversation);

        for mode in [UiDiffMode::Conversation, UiDiffMode::Git] {
            let source = format!("version = 1\n[ui]\ndiff_mode = '{}'\n", mode.as_str());
            let snapshot = ConfigSnapshot::parse(Path::new("config.toml"), &source).unwrap();
            assert_eq!(snapshot.diff_mode(), mode);
            let serialized = serde_json::to_string(&mode).unwrap();
            assert_eq!(serialized, format!("\"{}\"", mode.as_str()));
            assert_eq!(
                serde_json::from_str::<UiDiffMode>(&serialized).unwrap(),
                mode
            );
        }
    }

    #[test]
    fn ui_diff_mode_rejects_unknown_strings_and_non_strings() {
        let path = Path::new("bad-diff-mode.toml");
        for value in ["'auto'", "'Git'", "''", "false", "1", "[]", "{}"] {
            let source = format!("version = 1\n[ui]\ndiff_mode = {value}\n");
            let error = ConfigSnapshot::parse(path, &source).unwrap_err();
            assert!(
                matches!(error, RuntimeError::Config { path: error_path, message }
                    if error_path == path && message == "ui.diff_mode must be conversation or git"),
                "{value}"
            );
        }
    }

    #[test]
    fn invalid_ui_theme_and_colors_are_rejected() {
        assert!(
            ConfigSnapshot::parse(Path::new("bad.toml"), "version = 1\n[ui]\ntheme = 'auto'\n")
                .is_err()
        );
        assert!(
            ConfigSnapshot::parse(
                Path::new("bad.toml"),
                "version = 1\n[ui.colors]\nhighlight = 'chartreuse'\n",
            )
            .is_err()
        );
    }
}
