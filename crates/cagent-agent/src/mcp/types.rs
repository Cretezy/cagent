use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A configured MCP environment variable or HTTP header. The description is
/// setup guidance and is never sent to the server.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum McpConfiguredValue {
    String(String),
    Described {
        value: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
}

impl McpConfiguredValue {
    #[must_use]
    pub fn value(&self) -> &str {
        match self {
            Self::String(value) | Self::Described { value, .. } => value,
        }
    }

    #[must_use]
    pub fn description(&self) -> Option<&str> {
        match self {
            Self::String(_) => None,
            Self::Described { description, .. } => description.as_deref(),
        }
    }
}

impl From<String> for McpConfiguredValue {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<&str> for McpConfiguredValue {
    fn from(value: &str) -> Self {
        Self::String(value.into())
    }
}

/// Small, deterministic value vocabulary used by package setup parameters.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum McpParameterValue {
    String(String),
    Integer(i64),
    Boolean(bool),
    Strings(Vec<String>),
}

impl std::fmt::Display for McpParameterValue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::String(value) => formatter.write_str(value),
            Self::Integer(value) => value.fmt(formatter),
            Self::Boolean(value) => value.fmt(formatter),
            Self::Strings(values) => formatter.write_str(&values.join(",")),
        }
    }
}

/// Host process or OCI-container execution for a stdio MCP server.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpRunner {
    #[default]
    Host,
    Oci,
}

/// OCI restrictions applied by both Docker and Podman runners.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct McpOciConfig {
    #[serde(default)]
    pub network: bool,
    #[serde(default = "default_true")]
    pub read_only_root: bool,
    #[serde(default = "default_oci_memory")]
    pub memory: String,
    #[serde(default = "default_oci_pids")]
    pub pids: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
}

impl Default for McpOciConfig {
    fn default() -> Self {
        Self {
            network: false,
            read_only_root: true,
            memory: default_oci_memory(),
            pids: default_oci_pids(),
            runtime: None,
        }
    }
}

fn default_oci_memory() -> String {
    "512m".into()
}

const fn default_oci_pids() -> u32 {
    128
}

fn default_true() -> bool {
    true
}

fn default_startup_timeout() -> u64 {
    10
}

fn default_request_timeout() -> u64 {
    60
}

/// Configuration layer that owns an MCP server definition.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpScope {
    Global,
    Project,
    /// Retained only so callers compiled against the pre-shared-catalog API get a
    /// useful migration error instead of silently writing the removed layout.
    Agent,
}

/// A concrete configuration location. Agent scope requires `agent`.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct McpLocation {
    pub scope: McpScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

impl McpLocation {
    #[must_use]
    pub const fn global() -> Self {
        Self {
            scope: McpScope::Global,
            agent: None,
        }
    }

    #[must_use]
    pub const fn project() -> Self {
        Self {
            scope: McpScope::Project,
            agent: None,
        }
    }

    #[must_use]
    pub fn agent(name: impl Into<String>) -> Self {
        Self {
            scope: McpScope::Agent,
            agent: Some(name.into()),
        }
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        match (self.scope, self.agent.as_deref()) {
            (McpScope::Agent, _) => Err(
                "agent-scoped MCP definitions were removed; define [mcp.servers.<name>] and set agents = [\"...\"] instead".into(),
            ),
            (_, None) => Ok(()),
            (_, Some(_)) => Err("only agent MCP scope accepts an agent name".into()),
        }
    }
}

/// Supported external MCP transports.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "transport", rename_all = "snake_case")]
pub enum McpTransportConfig {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default)]
        env: BTreeMap<String, McpConfiguredValue>,
        #[serde(default)]
        env_remove: Vec<String>,
        #[serde(default = "default_true")]
        inherit_env: bool,
    },
    #[serde(rename = "http", alias = "streamable_http", alias = "streamable-http")]
    StreamableHttp {
        url: String,
        #[serde(default)]
        headers: BTreeMap<String, McpConfiguredValue>,
        #[serde(default)]
        allow_insecure: bool,
    },
}

/// Host-managed OAuth configuration for a Streamable HTTP MCP server.
/// Tokens and dynamic client registrations are stored in managed credential
/// storage, never in the server definition.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct McpOAuthConfig {
    /// Optional managed bearer token fallback (for example a PAT). OAuth
    /// credentials take precedence when both are configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bearer_token: Option<McpConfiguredValue>,
    /// Optional pre-registered public client ID. If omitted, standards-based
    /// client metadata or dynamic registration discovered from the server is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<McpConfiguredValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<McpConfiguredValue>,
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Optional device authorization endpoint for providers that require a
    /// device-code flow for distributed public clients.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_authorization_endpoint: Option<String>,
    /// Static metadata is an interoperability escape hatch for authorization
    /// servers (notably GitHub OAuth Apps) that do not publish RFC 8414 metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issuer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorization_endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_endpoint: Option<String>,
}

/// One canonical MCP server definition as stored in TOML or portable JSON.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct McpServerDefinition {
    #[serde(flatten)]
    pub transport: McpTransportConfig,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Profiles that may activate this server. An empty list means all profiles.
    #[serde(default)]
    pub agents: Vec<String>,
    /// Kept for source compatibility with callers while the TOML/JSON setting is
    /// intentionally rejected as part of the shared-catalog migration.
    #[serde(skip_serializing, default)]
    pub eager: bool,
    #[serde(default = "default_startup_timeout")]
    pub startup_timeout_seconds: u64,
    #[serde(default = "default_request_timeout")]
    pub request_timeout_seconds: u64,
    /// Explicit user attestation; server annotations never add authority.
    #[serde(default)]
    pub read_only_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth: Option<McpOAuthConfig>,
    /// Stdio execution policy. HTTP definitions always use the host HTTP client.
    #[serde(default, skip_serializing_if = "is_host_runner")]
    pub runner: McpRunner,
    /// Required when `runner = "oci"`; ignored for host and HTTP definitions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "is_default_oci")]
    pub oci: McpOciConfig,
    /// Catalog source (`builtin:<id>` or an HTTPS URL). A loaded definition
    /// retains its resolved MCP fields in memory while serializing this reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// Required for URL packages; built-ins are versioned with Cagent itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_digest: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub parameters: BTreeMap<String, McpParameterValue>,
}

impl Default for McpServerDefinition {
    fn default() -> Self {
        Self {
            transport: McpTransportConfig::Stdio {
                command: String::new(),
                args: Vec::new(),
                cwd: None,
                env: BTreeMap::new(),
                env_remove: Vec::new(),
                inherit_env: true,
            },
            enabled: true,
            agents: Vec::new(),
            eager: false,
            startup_timeout_seconds: default_startup_timeout(),
            request_timeout_seconds: default_request_timeout(),
            read_only_tools: Vec::new(),
            oauth: None,
            runner: McpRunner::Host,
            image: None,
            oci: McpOciConfig::default(),
            package: None,
            package_digest: None,
            parameters: BTreeMap::new(),
        }
    }
}

const fn is_host_runner(runner: &McpRunner) -> bool {
    matches!(runner, McpRunner::Host)
}

fn is_default_oci(config: &McpOciConfig) -> bool {
    config == &McpOciConfig::default()
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Deserialize)]
struct McpServerDefinitionWire {
    #[serde(default)]
    transport: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, McpConfiguredValue>,
    #[serde(default)]
    env_remove: Vec<String>,
    #[serde(default = "default_true")]
    inherit_env: bool,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    headers: BTreeMap<String, McpConfiguredValue>,
    #[serde(default)]
    allow_insecure: bool,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    agents: Vec<String>,
    #[serde(default = "default_startup_timeout")]
    startup_timeout_seconds: u64,
    #[serde(default = "default_request_timeout")]
    request_timeout_seconds: u64,
    #[serde(default)]
    read_only_tools: Vec<String>,
    #[serde(default)]
    oauth: Option<McpOAuthConfig>,
    #[serde(default)]
    runner: McpRunner,
    #[serde(default)]
    image: Option<String>,
    #[serde(default)]
    oci: McpOciConfig,
    #[serde(default)]
    package: Option<String>,
    #[serde(default)]
    package_digest: Option<String>,
    #[serde(default)]
    parameters: BTreeMap<String, McpParameterValue>,
}

impl<'de> Deserialize<'de> for McpServerDefinition {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error as _;

        let wire = McpServerDefinitionWire::deserialize(deserializer)?;
        let transport_name = wire.transport.as_deref().ok_or_else(|| {
            D::Error::custom("MCP definition requires transport unless it is a resolved package")
        })?;
        let transport = match transport_name {
            "stdio" => {
                if wire.url.is_some() || !wire.headers.is_empty() || wire.allow_insecure {
                    return Err(D::Error::custom(
                        "stdio MCP definitions cannot contain HTTP fields",
                    ));
                }
                McpTransportConfig::Stdio {
                    command: wire
                        .command
                        .ok_or_else(|| D::Error::missing_field("command"))?,
                    args: wire.args,
                    cwd: wire.cwd,
                    env: wire.env,
                    env_remove: wire.env_remove,
                    inherit_env: wire.inherit_env,
                }
            }
            "http" | "streamable_http" | "streamable-http" => {
                if wire.command.is_some()
                    || !wire.args.is_empty()
                    || wire.cwd.is_some()
                    || !wire.env.is_empty()
                    || !wire.env_remove.is_empty()
                    || !wire.inherit_env
                {
                    return Err(D::Error::custom(
                        "HTTP MCP definitions cannot contain stdio fields",
                    ));
                }
                McpTransportConfig::StreamableHttp {
                    url: wire.url.ok_or_else(|| D::Error::missing_field("url"))?,
                    headers: wire.headers,
                    allow_insecure: wire.allow_insecure,
                }
            }
            "sse" | "oauth" => {
                return Err(D::Error::custom(format!(
                    "unsupported MCP transport: {}",
                    transport_name
                )));
            }
            _ => {
                return Err(D::Error::unknown_variant(
                    transport_name,
                    &["stdio", "http", "streamable_http", "streamable-http"],
                ));
            }
        };
        Ok(Self {
            transport,
            enabled: wire.enabled,
            agents: wire.agents,
            eager: false,
            startup_timeout_seconds: wire.startup_timeout_seconds,
            request_timeout_seconds: wire.request_timeout_seconds,
            read_only_tools: wire.read_only_tools,
            oauth: wire.oauth,
            runner: wire.runner,
            image: wire.image,
            oci: wire.oci,
            package: wire.package,
            package_digest: wire.package_digest,
            parameters: wire.parameters,
        })
    }
}

impl McpServerDefinition {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.startup_timeout_seconds == 0 {
            return Err("startup_timeout_seconds must be at least 1".into());
        }
        if self.request_timeout_seconds == 0 {
            return Err("request_timeout_seconds must be at least 1".into());
        }
        match self.runner {
            McpRunner::Host if self.image.is_some() => {
                return Err("image is only valid when runner = \"oci\"".into());
            }
            McpRunner::Oci if self.image.as_deref().is_none_or(str::is_empty) => {
                return Err("OCI MCP definitions require image".into());
            }
            McpRunner::Oci if !matches!(self.transport, McpTransportConfig::Stdio { .. }) => {
                return Err("OCI execution is only valid for stdio MCP servers".into());
            }
            McpRunner::Host | McpRunner::Oci => {}
        }
        if self.oauth.is_some()
            && !matches!(self.transport, McpTransportConfig::StreamableHttp { .. })
        {
            return Err("oauth is only valid for Streamable HTTP MCP servers".into());
        }
        if let Some(oauth) = &self.oauth {
            let static_fields = [
                oauth.issuer.as_ref(),
                oauth.authorization_endpoint.as_ref(),
                oauth.token_endpoint.as_ref(),
            ];
            if static_fields.iter().any(|value| value.is_some())
                && static_fields.iter().any(|value| value.is_none())
            {
                return Err(
                    "oauth static metadata requires issuer, authorization_endpoint, and token_endpoint"
                        .into(),
                );
            }
            if oauth.client_secret.is_some() && oauth.client_id.is_none() {
                return Err("oauth.client_secret requires oauth.client_id".into());
            }
            if let Some(endpoint) = &oauth.device_authorization_endpoint {
                super::validate_http_url(endpoint, false)?;
                if oauth.client_id.is_none() || oauth.token_endpoint.is_none() {
                    return Err(
                        "oauth.device_authorization_endpoint requires oauth.client_id and oauth.token_endpoint"
                            .into(),
                    );
                }
            }
        }
        if self.oci.pids == 0 {
            return Err("oci.pids must be at least 1".into());
        }
        let mut seen = std::collections::BTreeSet::new();
        if self
            .agents
            .iter()
            .any(|agent| agent.trim().is_empty() || !seen.insert(agent))
        {
            return Err("agents must contain unique, non-empty profile names".into());
        }
        match &self.transport {
            McpTransportConfig::Stdio { command, .. } if command.trim().is_empty() => {
                Err("stdio command must not be empty".into())
            }
            McpTransportConfig::StreamableHttp {
                url,
                allow_insecure,
                ..
            } if url.contains("${") => Ok(()),
            McpTransportConfig::StreamableHttp {
                url,
                allow_insecure,
                ..
            } => super::validate_http_url(url, *allow_insecure),
            McpTransportConfig::Stdio { .. } => Ok(()),
        }
    }
}

/// Lifecycle state shared by all frontends.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum McpRuntimeStatus {
    Disabled,
    #[serde(rename = "inactive")]
    NotStarted,
    #[serde(rename = "loading")]
    Starting,
    #[serde(rename = "active")]
    Connected,
    Restarting,
    AuthenticationRequired,
    Failed {
        message: String,
    },
    Stopped,
}

impl std::fmt::Display for McpRuntimeStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disabled => formatter.write_str("Disabled"),
            Self::NotStarted => formatter.write_str("Not started"),
            Self::Starting => formatter.write_str("Starting"),
            Self::Connected => formatter.write_str("Connected"),
            Self::Restarting => formatter.write_str("Restarting"),
            Self::AuthenticationRequired => formatter.write_str("Login required"),
            Self::Failed { message } => write!(formatter, "Failed · {message}"),
            Self::Stopped => formatter.write_str("Stopped"),
        }
    }
}

/// One discovered MCP tool with both original and provider-safe identities.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct McpToolSummary {
    pub server: String,
    pub name: String,
    pub provider_name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub configured_read_only: bool,
}

/// Picker/CLI projection of a resolved server.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct McpEffectiveServer {
    pub name: String,
    pub location: McpLocation,
    pub definition: McpServerDefinition,
    pub status: McpRuntimeStatus,
    /// Assigned profiles in deterministic order. An empty list means all profiles.
    #[serde(default)]
    pub agents: Vec<String>,
    /// Compatibility projection for callers asking about one profile.
    pub allowed_for_agent: bool,
    #[serde(default)]
    pub overridden: Vec<McpLocation>,
    #[serde(default)]
    pub tools: Vec<McpToolSummary>,
    #[serde(default)]
    pub diagnostics: Vec<String>,
    pub generation: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpImportAction {
    Add,
    Replace,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct McpImportEntry {
    pub name: String,
    pub definition: McpServerDefinition,
    pub action: McpImportAction,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct McpImportPreview {
    pub location: McpLocation,
    pub entries: Vec<McpImportEntry>,
    pub requires_confirmation: bool,
}

/// One atomic management operation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)] // Mutation payloads mirror the public TOML management API.
pub enum McpMutation {
    Put {
        location: McpLocation,
        name: String,
        definition: McpServerDefinition,
    },
    Remove {
        location: McpLocation,
        name: String,
    },
    Move {
        from: McpLocation,
        name: String,
        to: McpLocation,
        new_name: String,
    },
}

/// Consequences shown before a management batch is committed.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct McpMutationPreview {
    pub mutations: Vec<McpMutation>,
    pub affected: Vec<String>,
    pub revealed: Vec<McpEffectiveServer>,
    pub agent_visibility: BTreeMap<String, Vec<String>>,
    pub touched_permission_rules: Vec<String>,
    pub touched_agent_policies: Vec<String>,
    pub replacements: Vec<String>,
    pub touched_files: Vec<PathBuf>,
    pub requires_confirmation: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceTrust {
    Trusted,
    UntrustedForRun,
    Unseen,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectControlStatus {
    pub workspace: PathBuf,
    pub trust: WorkspaceTrust,
    pub project_config_exists: bool,
    pub project_config_loaded: bool,
}

/// Durable external MCP result retained independently of its bounded summary.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct McpToolResult {
    pub server: String,
    pub tool: String,
    pub provider_name: String,
    pub content: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<serde_json::Value>,
    pub is_error: bool,
    pub duration_millis: u64,
    pub summary: String,
}
