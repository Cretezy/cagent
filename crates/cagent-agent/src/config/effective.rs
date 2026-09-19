use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use super::{
    BellConfig, CompactionConfig, ConversationCleanupConfig, DelegationPolicy, ProviderSettings,
    ShellConfig, TitleConfig, UiColors, UiDiffMode, UiEditor, UiTheme, WebFetchRedirectsConfig,
    WorktreeConfig,
};

use crate::{
    AgentCatalog, McpGlobalConfig, ModeProfile, PermissionRule, StatusLineConfig, WebSearchConfig,
};

/// A validated, immutable view of the effective configuration.
#[derive(Clone, Debug)]
pub struct ConfigSnapshot(pub(crate) Arc<ConfigData>);

#[derive(Clone, Debug)]
pub(crate) struct ConfigData {
    pub(crate) machine_fingerprint: bool,
    pub(crate) default_agent: String,
    pub(crate) default_mode: String,
    pub(crate) default_plan_exit_mode: String,
    pub(crate) default_model: Option<super::ModelSelection>,
    pub(crate) tiers: BTreeMap<String, Vec<super::TierCandidate>>,
    pub(crate) fast: bool,
    pub(crate) title_generation_enabled: bool,
    #[cfg(test)]
    pub(crate) title_generation_explicit: bool,
    pub(crate) title_generation_timeout_seconds: u64,
    pub(crate) automatic_recaps: bool,
    pub(crate) recap_idle_seconds: u64,
    pub(crate) delegation: DelegationPolicy,
    pub(crate) favourite_models: BTreeSet<crate::ModelRef>,
    pub(crate) status_line: StatusLineConfig,
    pub(crate) agents: AgentCatalog,
    pub(crate) modes: BTreeMap<String, ModeProfile>,
    pub(crate) permission_rules: BTreeMap<(String, String), Vec<PermissionRule>>,
    pub(crate) key_bindings: BTreeMap<String, Option<Vec<Option<String>>>>,
    pub(crate) attachment_bytes: u64,
    pub(crate) attachment_hard_cap_bytes: u64,
    pub(crate) resize_images: bool,
    pub(crate) scrollback_reflow_rows: usize,
    pub(crate) composer_max_rows: Option<usize>,
    pub(crate) files_width: usize,
    pub(crate) diff_context_lines: usize,
    pub(crate) diff_mode: UiDiffMode,
    pub(crate) show_tips: bool,
    pub(crate) collapse_tool_activity: bool,
    pub(crate) open_links: bool,
    pub(crate) ui_theme: UiTheme,
    pub(crate) ui_colors: UiColors,
    pub(crate) file_picker_respect_gitignore: bool,
    pub(crate) file_picker_hide_hidden_files: bool,
    pub(crate) editor: UiEditor,
    pub(crate) title: TitleConfig,
    pub(crate) bell: BellConfig,
    pub(crate) progress_osc: bool,
    pub(crate) providers: BTreeMap<String, ProviderSettings>,
    pub(crate) mcp: McpGlobalConfig,
    pub(crate) web_search: WebSearchConfig,
    pub(crate) web_fetch_redirects: WebFetchRedirectsConfig,
    pub(crate) subagent_max_concurrent: usize,
    pub(crate) shell: ShellConfig,
    pub(crate) external_agents: bool,
    pub(crate) bundled_skills: bool,
    pub(crate) disabled_skills: BTreeSet<String>,
    pub(crate) compaction: CompactionConfig,
    pub(crate) conversation_cleanup: ConversationCleanupConfig,
    pub(crate) worktree: WorktreeConfig,
}
