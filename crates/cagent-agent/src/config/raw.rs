use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{
    DelegationPolicy, ModelCapabilities, ModelSelectionConfig, ShellConfig, WorktreeConfig,
};

/// Optional values exactly as represented by `config.toml` version 1.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ConfigFile {
    pub version: u16,
    #[serde(default = "default_true")]
    pub machine_fingerprint: bool,
    pub default_agent: Option<String>,
    pub default_mode: Option<String>,
    pub default_plan_exit_mode: Option<String>,
    #[serde(default)]
    pub default_model: Option<ModelSelectionConfig>,
    #[serde(default)]
    pub tiers: BTreeMap<String, Vec<ModelSelectionConfig>>,
    #[serde(default)]
    pub fast: bool,
    #[serde(default)]
    pub(crate) title_generation: TitleGenerationConfigFile,
    #[serde(default = "default_true")]
    pub automatic_recaps: bool,
    #[serde(default = "default_recap_idle_seconds")]
    pub recap_idle_seconds: u64,
    #[serde(default)]
    pub(crate) subagents: SubagentConfigFile,
    #[serde(default)]
    pub favourite_models: BTreeSet<String>,
    #[serde(default)]
    pub(crate) providers: ProviderConfigFile,
    #[serde(default)]
    pub(crate) web_search: Option<crate::web_search::WebSearchFile>,
    #[serde(default)]
    pub(crate) web_fetch: WebFetchConfigFile,
    #[serde(default)]
    pub(crate) shell: ShellConfig,
    #[serde(default)]
    pub(crate) skills: SkillsConfigFile,
    #[serde(default)]
    pub(crate) compatibility: CompatibilityConfigFile,
    #[serde(default)]
    pub(crate) worktree: WorktreeConfig,
}

const fn default_recap_idle_seconds() -> u64 {
    180
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct SkillsConfigFile {
    #[serde(default)]
    pub bundled: BundledSkillsConfigFile,
    #[serde(default)]
    pub disabled: BTreeSet<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct BundledSkillsConfigFile {
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for BundledSkillsConfigFile {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct CompatibilityConfigFile {
    #[serde(default = "default_true")]
    pub external_agents: bool,
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
    pub(crate) generally_safe: bool,
    pub(crate) same_site: bool,
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

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct WebFetchConfigFile {
    pub redirects: WebFetchRedirectsConfigFile,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct WebFetchRedirectsConfigFile {
    pub generally_safe: bool,
    pub same_site: bool,
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
pub(crate) struct SubagentConfigFile {
    pub strategy: Option<DelegationPolicy>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct TitleGenerationConfigFile {
    pub enabled: Option<bool>,
    pub timeout_seconds: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct ProviderConfigFile {
    #[serde(default)]
    pub custom: BTreeMap<String, ProviderOverrides>,
    #[serde(flatten)]
    pub built_in: BTreeMap<String, toml::Value>,
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
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
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
    pub usage_limit: Option<String>,
    pub models: Option<Vec<String>>,
    pub aliases: Option<BTreeMap<String, String>>,
    pub capability_overrides: Option<BTreeMap<String, ModelCapabilities>>,
    pub auth: Option<VertexAuth>,
    pub project: Option<String>,
    pub location: Option<String>,
}

/// Fully resolved effective provider settings used at runtime.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProviderSettings {
    pub kind: ProviderKind,
    pub enabled: bool,
    pub api_key_env: Option<String>,
    pub display_name: Option<String>,
    pub base_url: Option<String>,
    pub protocol: Option<crate::ProviderProtocol>,
    pub models_endpoint: Option<String>,
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum VertexAuth {
    Adc,
    ApiKey,
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
    pub(crate) fn apply(&mut self, o: ProviderOverrides) {
        if let Some(v) = o.kind {
            self.kind = v;
        }
        if let Some(v) = o.enabled {
            self.enabled = v;
        }
        if o.api_key_env.is_some() {
            self.api_key_env = o.api_key_env;
        }
        if o.display_name.is_some() {
            self.display_name = o.display_name;
        }
        if o.base_url.is_some() {
            self.base_url = o.base_url;
        }
        if o.protocol.is_some() {
            self.protocol = o.protocol;
        }
        if o.models_endpoint.is_some() {
            self.models_endpoint = o.models_endpoint;
        }
        if o.usage_limit.is_some() {
            self.usage_limit = o.usage_limit;
        }
        if let Some(v) = o.models {
            self.models = v;
        }
        if let Some(v) = o.aliases {
            self.aliases = v;
        }
        if let Some(v) = o.capability_overrides {
            self.capability_overrides = v;
        }
        if let Some(v) = o.auth {
            self.auth = v;
        }
        if o.project.is_some() {
            self.project = o.project;
        }
        if o.location.is_some() {
            self.location = o.location;
        }
    }
}
