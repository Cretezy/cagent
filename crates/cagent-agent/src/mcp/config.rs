use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use fs2::FileExt as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::{
    McpEffectiveServer, McpImportAction, McpImportEntry, McpImportPreview, McpLocation,
    McpMutation, McpMutationPreview, McpRuntimeStatus, McpScope, McpServerDefinition,
    McpTransportConfig, ProjectControlStatus, WorkspaceTrust,
};
use crate::RuntimeError;
use crate::config::implicit_table;

const TRANSACTION_FILE: &str = ".mcp-transaction.json";

#[derive(Clone, Debug, Default)]
struct ConfigLayers {
    global: BTreeMap<String, McpServerDefinition>,
    project: BTreeMap<String, McpServerDefinition>,
    session: BTreeMap<String, McpServerDefinition>,
    known_agents: BTreeSet<String>,
}

/// Global and agent-scoped MCP configuration resolved with the main configuration snapshot.
#[derive(Clone, Debug, Default)]
pub(crate) struct McpGlobalConfig {
    global: BTreeMap<String, McpServerDefinition>,
    known_agents: BTreeSet<String>,
}

impl ConfigLayers {
    fn apply_global(&mut self, global: &McpGlobalConfig) {
        self.global.clone_from(&global.global);
        self.known_agents.clone_from(&global.known_agents);
    }
}

/// Comment-preserving, serialized MCP configuration service for one workspace.
#[derive(Clone, Debug)]
pub struct McpConfigService {
    config_store: crate::ConfigStore,
    global_path: PathBuf,
    project_path: PathBuf,
    permissions_path: PathBuf,
    workspace: PathBuf,
    trusted: bool,
    layers: Arc<RwLock<ConfigLayers>>,
    project_observed: Arc<Mutex<Option<String>>>,
    writer: Arc<Mutex<()>>,
}

impl McpConfigService {
    /// Adds non-persistent server definitions for the lifetime of this service clone.
    /// Session definitions shadow project and global definitions with the same name.
    pub fn with_session_servers(
        self,
        servers: BTreeMap<String, McpServerDefinition>,
    ) -> Result<Self, RuntimeError> {
        for (name, definition) in &servers {
            validate_name(name)?;
            definition.validate().map_err(RuntimeError::InvalidOption)?;
        }
        self.layers
            .write()
            .map_err(|_| RuntimeError::RuntimeStopped)?
            .session = servers;
        Ok(self)
    }
    #[must_use]
    pub(crate) fn trusted(&self) -> bool {
        self.trusted
    }
    pub(crate) fn for_workspace(
        &self,
        workspace: &Path,
        trusted: bool,
    ) -> Result<Self, RuntimeError> {
        Self::from_store(
            self.config_store.clone(),
            self.global_path.clone(),
            workspace,
            trusted,
        )
    }
    /// Loads the global file and, when trusted, the workspace project file.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid paths, configuration, or interrupted-transaction recovery.
    pub fn load(
        global_path: PathBuf,
        workspace: &Path,
        trusted: bool,
    ) -> Result<Self, RuntimeError> {
        let config_store = crate::ConfigStore::open(&global_path)?;
        Self::from_store(config_store, global_path, workspace, trusted)
    }

    /// Uses an existing file-backed main configuration store for global and agent MCP settings.
    ///
    /// # Errors
    ///
    /// Returns an error for an in-memory store or invalid workspace/project configuration.
    pub fn from_config_store(
        config_store: crate::ConfigStore,
        workspace: &Path,
        trusted: bool,
    ) -> Result<Self, RuntimeError> {
        let global_path = config_store.path().map(Path::to_path_buf).ok_or_else(|| {
            RuntimeError::InvalidOption(
                "MCP configuration management requires a file-backed ConfigStore".into(),
            )
        })?;
        Self::from_store(config_store, global_path, workspace, trusted)
    }

    pub(crate) fn from_store(
        config_store: crate::ConfigStore,
        fallback_global_path: PathBuf,
        workspace: &Path,
        trusted: bool,
    ) -> Result<Self, RuntimeError> {
        let workspace = workspace.canonicalize()?;
        let global_path = config_store
            .path()
            .map_or(fallback_global_path, Path::to_path_buf);
        let project_path = workspace.join(".cagent/config.toml");
        let permissions_path = global_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("permissions.toml");
        let service = Self {
            config_store,
            global_path,
            project_path,
            permissions_path,
            workspace,
            trusted,
            layers: Arc::new(RwLock::new(ConfigLayers::default())),
            project_observed: Arc::new(Mutex::new(None)),
            writer: Arc::new(Mutex::new(())),
        };
        if service.recover_transaction()? && service.config_store.path().is_some() {
            service.config_store.reload_from_disk()?;
        }
        let layers = service.read_layers()?;
        *service
            .layers
            .write()
            .map_err(|_| RuntimeError::RuntimeStopped)? = layers;
        *service
            .project_observed
            .lock()
            .map_err(|_| RuntimeError::RuntimeStopped)? =
            Some(document_fingerprint(&service.project_path)?);
        Ok(service)
    }

    #[must_use]
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    #[must_use]
    pub fn global_path(&self) -> &Path {
        &self.global_path
    }

    /// Returns the configuration file that owns a location. Package caches use
    /// this same anchor so global and project URL packages resolve consistently.
    pub fn path_for_location(&self, location: &McpLocation) -> Result<PathBuf, RuntimeError> {
        self.path_for(location)
    }

    #[must_use]
    pub fn project_control_status(&self) -> ProjectControlStatus {
        ProjectControlStatus {
            workspace: self.workspace.clone(),
            trust: if self.trusted {
                WorkspaceTrust::Trusted
            } else {
                WorkspaceTrust::UntrustedForRun
            },
            project_config_exists: self.project_path.exists(),
            project_config_loaded: self.trusted && self.project_path.exists(),
        }
    }

    /// Lists the shared catalog in stable name order. Project definitions shadow global ones.
    ///
    /// # Errors
    ///
    /// Returns an error if the shared catalog lock is unavailable.
    pub fn list_effective(&self, agent: &str) -> Result<Vec<McpEffectiveServer>, RuntimeError> {
        self.sync_project()?;
        self.sync_global()?;
        let layers = self
            .layers
            .read()
            .map_err(|_| RuntimeError::RuntimeStopped)?;
        Ok(resolve_effective(&layers, agent))
    }

    /// Returns one resolved server for an agent.
    ///
    /// # Errors
    ///
    /// Returns an error if the catalog cannot be read.
    pub fn get_effective(
        &self,
        agent: &str,
        name: &str,
    ) -> Result<Option<McpEffectiveServer>, RuntimeError> {
        Ok(self
            .list_effective(agent)?
            .into_iter()
            .find(|server| server.name == name))
    }

    /// Returns every stored definition, including overridden layers.
    ///
    /// # Errors
    ///
    /// Returns an error if the catalog cannot be read.
    pub fn list_all(
        &self,
    ) -> Result<Vec<(McpLocation, String, McpServerDefinition)>, RuntimeError> {
        self.sync_project()?;
        self.sync_global()?;
        let layers = self
            .layers
            .read()
            .map_err(|_| RuntimeError::RuntimeStopped)?;
        let mut output = Vec::new();
        output.extend(
            layers.global.iter().map(|(name, definition)| {
                (McpLocation::global(), name.clone(), definition.clone())
            }),
        );
        output.extend(
            layers.project.iter().map(|(name, definition)| {
                (McpLocation::project(), name.clone(), definition.clone())
            }),
        );
        Ok(output)
    }

    /// Lists every stored definition, including definitions shadowed by another layer.
    ///
    /// # Errors
    ///
    /// Returns an error if the catalog cannot be read.
    pub fn list_all_effective(&self, agent: &str) -> Result<Vec<McpEffectiveServer>, RuntimeError> {
        self.list_all().map(|servers| {
            servers
                .into_iter()
                .map(|(location, name, definition)| {
                    let agents = definition.agents.clone();
                    McpEffectiveServer {
                        allowed_for_agent: agents.is_empty()
                            || agents.iter().any(|assigned| assigned == agent),
                        status: if definition.enabled {
                            McpRuntimeStatus::NotStarted
                        } else {
                            McpRuntimeStatus::Disabled
                        },
                        name,
                        location,
                        definition,
                        agents,
                        overridden: Vec::new(),
                        tools: Vec::new(),
                        diagnostics: Vec::new(),
                        generation: 0,
                    }
                })
                .collect()
        })
    }

    /// Parses and validates portable JSON without changing configuration.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid input, scope, or project trust.
    pub fn preview_import_json(
        &self,
        location: McpLocation,
        source: &str,
        separate_name: Option<&str>,
    ) -> Result<McpImportPreview, RuntimeError> {
        location.validate().map_err(RuntimeError::InvalidOption)?;
        self.ensure_location_allowed(&location)?;
        let definitions = parse_mcp_json(source, separate_name)?;
        let existing = self.definitions_at(&location)?;
        let entries = definitions
            .into_iter()
            .map(|(name, definition)| McpImportEntry {
                action: if existing.contains_key(&name) {
                    McpImportAction::Replace
                } else {
                    McpImportAction::Add
                },
                name,
                definition,
            })
            .collect::<Vec<_>>();
        Ok(McpImportPreview {
            location,
            requires_confirmation: entries
                .iter()
                .any(|entry| entry.action == McpImportAction::Replace),
            entries,
        })
    }

    /// Validates a mutation batch and describes its effects without writing files.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid definitions, references, scopes, or trust.
    #[allow(clippy::too_many_lines)]
    pub fn preview_mutations(
        &self,
        agent: &str,
        mutations: Vec<McpMutation>,
    ) -> Result<McpMutationPreview, RuntimeError> {
        self.sync_project()?;
        self.sync_global()?;
        if mutations.is_empty() {
            return Err(RuntimeError::InvalidOption(
                "an MCP mutation batch must not be empty".into(),
            ));
        }
        let current = self
            .layers
            .read()
            .map_err(|_| RuntimeError::RuntimeStopped)?
            .clone();
        let before = resolve_effective(&current, agent);
        let mut candidate = current.clone();
        let mut replacements = Vec::new();
        let mut affected = BTreeSet::new();
        let mut touched_files = BTreeSet::new();
        for mutation in &mutations {
            validate_mutation(mutation)?;
            if let McpMutation::Put { definition, .. } = mutation {
                validate_assignments(definition, &candidate.known_agents)?;
            }
            for location in mutation_locations(mutation) {
                self.ensure_location_allowed(location)?;
                touched_files.insert(self.path_for(location)?);
            }
            match mutation {
                McpMutation::Put {
                    location,
                    name,
                    definition,
                } => {
                    if definitions_at_mut(&mut candidate, location).contains_key(name) {
                        replacements.push(name.clone());
                    }
                    definitions_at_mut(&mut candidate, location)
                        .insert(name.clone(), definition.clone());
                    affected.insert(name.clone());
                }
                McpMutation::Remove { location, name } => {
                    if definitions_at_mut(&mut candidate, location)
                        .remove(name)
                        .is_none()
                    {
                        return Err(RuntimeError::InvalidOption(format!(
                            "MCP server {name} does not exist at the requested scope"
                        )));
                    }
                    affected.insert(name.clone());
                }
                McpMutation::Move {
                    from,
                    name,
                    to,
                    new_name,
                } => {
                    let definition = definitions_at_mut(&mut candidate, from)
                        .remove(name)
                        .ok_or_else(|| {
                            RuntimeError::InvalidOption(format!(
                                "MCP server {name} does not exist at the source scope"
                            ))
                        })?;
                    if definitions_at_mut(&mut candidate, to).contains_key(new_name) {
                        replacements.push(new_name.clone());
                    }
                    definitions_at_mut(&mut candidate, to).insert(new_name.clone(), definition);
                    affected.insert(name.clone());
                    affected.insert(new_name.clone());
                    if name != new_name {
                        touched_files.insert(self.permissions_path.clone());
                    }
                }
            }
        }
        let after = resolve_effective(&candidate, agent);
        let before_by_name = before
            .into_iter()
            .map(|server| (server.name.clone(), server))
            .collect::<BTreeMap<_, _>>();
        let revealed = after
            .iter()
            .filter(|server| {
                before_by_name
                    .get(&server.name)
                    .is_some_and(|old| old.location != server.location)
            })
            .cloned()
            .collect();
        let agents = candidate.known_agents.clone();
        let agent_visibility = agents
            .into_iter()
            .map(|name| {
                let visible = resolve_effective(&candidate, &name)
                    .into_iter()
                    .filter(|server| server.allowed_for_agent && server.definition.enabled)
                    .map(|server| server.name)
                    .collect();
                (name, visible)
            })
            .collect();
        let renamed = mutations.iter().find_map(|mutation| match mutation {
            McpMutation::Move { name, new_name, .. } if name != new_name => {
                Some((name.as_str(), new_name.as_str()))
            }
            _ => None,
        });
        let (touched_agent_policies, touched_permission_rules) = renamed.map_or_else(
            || (Vec::new(), Vec::new()),
            |(old, _)| self.reference_preview(old),
        );
        Ok(McpMutationPreview {
            mutations,
            affected: affected.into_iter().collect(),
            revealed,
            agent_visibility,
            touched_permission_rules,
            touched_agent_policies,
            requires_confirmation: !replacements.is_empty(),
            replacements,
            touched_files: touched_files.into_iter().collect(),
        })
    }

    /// Persists a fully validated batch as one recoverable multi-file transaction.
    ///
    /// # Errors
    ///
    /// Returns an error if validation, locking, staging, persistence, or reload fails.
    #[allow(clippy::needless_pass_by_value)]
    pub fn apply_mutations(
        &self,
        agent: &str,
        preview: McpMutationPreview,
        confirmed: bool,
    ) -> Result<Vec<McpEffectiveServer>, RuntimeError> {
        if preview.requires_confirmation && !confirmed {
            return Err(RuntimeError::InvalidOption(
                "MCP replacements require explicit confirmation".into(),
            ));
        }
        let checked = self.preview_mutations(agent, preview.mutations.clone())?;
        let changes_global = checked.mutations.iter().any(|mutation| {
            mutation_locations(mutation)
                .into_iter()
                .any(|location| location.scope != McpScope::Project)
        });
        let _guard = self
            .writer
            .lock()
            .map_err(|_| RuntimeError::RuntimeStopped)?;
        self.persist_batch(&checked.mutations)?;
        if changes_global {
            self.config_store.reload_from_disk()?;
        }
        let layers = self.read_layers()?;
        *self
            .layers
            .write()
            .map_err(|_| RuntimeError::RuntimeStopped)? = layers;
        self.list_effective(agent)
    }

    /// Applies a previously previewed JSON import.
    ///
    /// # Errors
    ///
    /// Returns an error if confirmation, validation, persistence, or reload fails.
    pub fn apply_import(
        &self,
        agent: &str,
        preview: McpImportPreview,
        confirmed: bool,
    ) -> Result<Vec<McpEffectiveServer>, RuntimeError> {
        let mutations = preview
            .entries
            .into_iter()
            .map(|entry| McpMutation::Put {
                location: preview.location.clone(),
                name: entry.name,
                definition: entry.definition,
            })
            .collect();
        let mutation_preview = self.preview_mutations(agent, mutations)?;
        self.apply_mutations(agent, mutation_preview, confirmed)
    }

    /// Changes the enabled state of one definition at an exact location.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown definition or failed persistence.
    pub fn set_enabled(
        &self,
        agent: &str,
        location: McpLocation,
        name: &str,
        enabled: bool,
    ) -> Result<Vec<McpEffectiveServer>, RuntimeError> {
        let mut definition = self
            .definitions_at(&location)?
            .get(name)
            .cloned()
            .ok_or_else(|| RuntimeError::InvalidOption(format!("unknown MCP server: {name}")))?;
        definition.enabled = enabled;
        let preview = self.preview_mutations(
            agent,
            vec![McpMutation::Put {
                location,
                name: name.into(),
                definition,
            }],
        )?;
        self.apply_mutations(agent, preview, true)
    }

    fn ensure_location_allowed(&self, location: &McpLocation) -> Result<(), RuntimeError> {
        if location.scope == McpScope::Project && !self.trusted {
            return Err(RuntimeError::InvalidOption(
                "project MCP changes require persisted workspace trust".into(),
            ));
        }
        Ok(())
    }

    fn path_for(&self, location: &McpLocation) -> Result<PathBuf, RuntimeError> {
        location.validate().map_err(RuntimeError::InvalidOption)?;
        Ok(match location.scope {
            McpScope::Global | McpScope::Agent => self.global_path.clone(),
            McpScope::Project => self.project_path.clone(),
        })
    }

    fn definitions_at(
        &self,
        location: &McpLocation,
    ) -> Result<BTreeMap<String, McpServerDefinition>, RuntimeError> {
        self.sync_project()?;
        self.sync_global()?;
        let layers = self
            .layers
            .read()
            .map_err(|_| RuntimeError::RuntimeStopped)?;
        Ok(match location.scope {
            McpScope::Global => layers.global.clone(),
            McpScope::Project => layers.project.clone(),
            McpScope::Agent => BTreeMap::new(),
        })
    }

    fn read_layers(&self) -> Result<ConfigLayers, RuntimeError> {
        let project = if self.trusted {
            read_document(&self.project_path)?
        } else {
            toml::Value::Table(toml::map::Map::new())
        };
        let mut layers = ConfigLayers {
            project: parse_catalog_table(
                &self.project_path,
                project.get("mcp"),
                &self.config_store.snapshot().mcp_config().known_agents,
            )?,
            ..ConfigLayers::default()
        };
        layers.apply_global(self.config_store.snapshot().mcp_config());
        Ok(layers)
    }

    fn sync_global(&self) -> Result<(), RuntimeError> {
        let snapshot = self.config_store.snapshot();
        self.layers
            .write()
            .map_err(|_| RuntimeError::RuntimeStopped)?
            .apply_global(snapshot.mcp_config());
        Ok(())
    }

    fn sync_project(&self) -> Result<(), RuntimeError> {
        if !self.trusted {
            return Ok(());
        }
        let (fingerprint, read_error) = match document_fingerprint(&self.project_path) {
            Ok(fingerprint) => (fingerprint, None),
            Err(error) => (format!("unreadable:{error}"), Some(error)),
        };
        if self
            .project_observed
            .lock()
            .map_err(|_| RuntimeError::RuntimeStopped)?
            .as_deref()
            == Some(&fingerprint)
        {
            return Ok(());
        }
        let candidate = read_error.map_or_else(
            || {
                read_document(&self.project_path).and_then(|document| {
                    parse_catalog_table(
                        &self.project_path,
                        document.get("mcp"),
                        &self.config_store.snapshot().mcp_config().known_agents,
                    )
                })
            },
            Err,
        );
        *self
            .project_observed
            .lock()
            .map_err(|_| RuntimeError::RuntimeStopped)? = Some(fingerprint);
        match candidate {
            Ok(project) => {
                self.layers
                    .write()
                    .map_err(|_| RuntimeError::RuntimeStopped)?
                    .project = project;
            }
            Err(error) => {
                tracing::warn!(path = %self.project_path.display(), %error, "rejected MCP project configuration reload");
            }
        }
        Ok(())
    }

    fn reference_preview(&self, old: &str) -> (Vec<String>, Vec<String>) {
        let source = std::fs::read_to_string(&self.global_path).unwrap_or_default();
        let document = source
            .parse::<toml::Value>()
            .unwrap_or_else(|_| toml::Value::Table(toml::map::Map::new()));
        let mut policies = Vec::new();
        if let Some(agents) = document.get("agents").and_then(toml::Value::as_table) {
            for (agent, value) in agents {
                for list in ["allow", "deny"] {
                    if value
                        .get("mcp")
                        .and_then(|mcp| mcp.get(list))
                        .and_then(toml::Value::as_array)
                        .is_some_and(|values| {
                            values.iter().any(|value| value.as_str() == Some(old))
                        })
                    {
                        policies.push(format!("agents.{agent}.mcp.{list}"));
                    }
                }
            }
        }
        let permission_source = std::fs::read_to_string(&self.permissions_path).unwrap_or_default();
        let permissions = permission_source
            .parse::<toml::Value>()
            .ok()
            .map(|value| count_exact_server_references(&value, old))
            .unwrap_or_default();
        (
            policies,
            (0..permissions)
                .map(|index| format!("permissions.server[{index}]"))
                .collect(),
        )
    }
}

/// Parses common MCP client JSON shapes into canonical strict definitions.
///
/// # Errors
///
/// Returns an error for malformed JSON, unsupported fields/transports, or invalid definitions.
pub fn parse_mcp_json(
    source: &str,
    separate_name: Option<&str>,
) -> Result<BTreeMap<String, McpServerDefinition>, RuntimeError> {
    let mut source = source.as_bytes().to_vec();
    let value: serde_json::Value = simd_json::serde::from_slice(&mut source)
        .map_err(|error| RuntimeError::InvalidOption(format!("invalid MCP JSON: {error}")))?;
    let object = value
        .as_object()
        .ok_or_else(|| RuntimeError::InvalidOption("MCP JSON must be an object".into()))?;
    let definitions = if let Some(value) =
        object.get("mcpServers").or_else(|| object.get("servers"))
    {
        value.as_object().ok_or_else(|| {
            RuntimeError::InvalidOption("mcpServers/servers must be an object".into())
        })?
    } else if looks_like_definition(object) {
        let name = separate_name
            .filter(|name| !name.trim().is_empty())
            .ok_or_else(|| {
                RuntimeError::InvalidOption("a bare MCP definition requires a separate name".into())
            })?;
        return Ok(BTreeMap::from([(
            validate_name(name)?.to_owned(),
            parse_json_definition(&value)?,
        )]));
    } else {
        object
    };
    if definitions.is_empty() {
        return Err(RuntimeError::InvalidOption(
            "MCP JSON contains no server definitions".into(),
        ));
    }
    definitions
        .iter()
        .map(|(name, value)| {
            Ok((
                validate_name(name)?.to_owned(),
                parse_json_definition(value)?,
            ))
        })
        .collect()
}

#[allow(clippy::too_many_lines)]
fn parse_json_definition(value: &serde_json::Value) -> Result<McpServerDefinition, RuntimeError> {
    let object = value.as_object().ok_or_else(|| {
        RuntimeError::InvalidOption("each MCP server definition must be an object".into())
    })?;
    let known = BTreeSet::from([
        "type",
        "transport",
        "command",
        "args",
        "cwd",
        "env",
        "environment",
        "env_remove",
        "envRemove",
        "inherit_env",
        "inheritEnv",
        "url",
        "headers",
        "allow_insecure",
        "allowInsecure",
        "enabled",
        "disabled",
        "agents",
        "startup_timeout_seconds",
        "startupTimeoutSeconds",
        "request_timeout_seconds",
        "requestTimeoutSeconds",
        "timeout",
        "read_only_tools",
        "readOnlyTools",
    ]);
    if let Some(field) = object.keys().find(|field| !known.contains(field.as_str())) {
        return Err(RuntimeError::InvalidOption(format!(
            "unsupported MCP definition field: {field}"
        )));
    }
    let transport_name = object
        .get("transport")
        .or_else(|| object.get("type"))
        .and_then(serde_json::Value::as_str);
    if matches!(transport_name, Some("sse" | "oauth")) {
        return Err(RuntimeError::InvalidOption(format!(
            "unsupported MCP transport: {}",
            transport_name.unwrap_or_default()
        )));
    }
    let transport = if object.contains_key("url")
        || matches!(
            transport_name,
            Some("http" | "streamable-http" | "streamable_http")
        ) {
        let url = string_field(object, "url")?.ok_or_else(|| {
            RuntimeError::InvalidOption("HTTP MCP definition requires url".into())
        })?;
        McpTransportConfig::StreamableHttp {
            url,
            headers: string_map(object.get("headers"), "headers")?
                .into_iter()
                .map(|(name, value)| (name, value.into()))
                .collect(),
            allow_insecure: bool_alias(object, "allow_insecure", "allowInsecure")?.unwrap_or(false),
        }
    } else {
        let command_value = object.get("command").ok_or_else(|| {
            RuntimeError::InvalidOption("stdio MCP definition requires command".into())
        })?;
        let (command, command_args) = match command_value {
            serde_json::Value::String(command) => (command.clone(), Vec::new()),
            serde_json::Value::Array(values) if !values.is_empty() => {
                let values = values
                    .iter()
                    .map(|value| {
                        value.as_str().map(str::to_owned).ok_or_else(|| {
                            RuntimeError::InvalidOption(
                                "command array items must be strings".into(),
                            )
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                (values[0].clone(), values[1..].to_vec())
            }
            _ => {
                return Err(RuntimeError::InvalidOption(
                    "command must be a string or non-empty string array".into(),
                ));
            }
        };
        let mut args = command_args;
        args.extend(string_array(object.get("args"), "args")?);
        McpTransportConfig::Stdio {
            command,
            args,
            cwd: string_field(object, "cwd")?,
            env: string_map(
                object.get("env").or_else(|| object.get("environment")),
                "env/environment",
            )?
            .into_iter()
            .map(|(name, value)| (name, value.into()))
            .collect(),
            env_remove: string_array(
                object.get("env_remove").or_else(|| object.get("envRemove")),
                "env_remove",
            )?,
            inherit_env: bool_alias(object, "inherit_env", "inheritEnv")?.unwrap_or(true),
        }
    };
    let enabled = bool_alias(object, "enabled", "enabled")?.unwrap_or(true)
        && !bool_alias(object, "disabled", "disabled")?.unwrap_or(false);
    let definition = McpServerDefinition {
        transport,
        enabled,
        agents: string_array(object.get("agents"), "agents")?,
        eager: false,
        startup_timeout_seconds: u64_alias(
            object,
            "startup_timeout_seconds",
            "startupTimeoutSeconds",
        )?
        .unwrap_or(10),
        request_timeout_seconds: u64_alias(
            object,
            "request_timeout_seconds",
            "requestTimeoutSeconds",
        )?
        .or(u64_alias(object, "timeout", "timeout")?)
        .unwrap_or(60),
        read_only_tools: string_array(
            object
                .get("read_only_tools")
                .or_else(|| object.get("readOnlyTools")),
            "read_only_tools",
        )?,
        oauth: None,
        runner: super::McpRunner::Host,
        image: None,
        oci: super::McpOciConfig::default(),
        package: None,
        package_digest: None,
        parameters: BTreeMap::new(),
    };
    definition.validate().map_err(RuntimeError::InvalidOption)?;
    Ok(definition)
}

fn validate_assignments(
    definition: &McpServerDefinition,
    known_agents: &BTreeSet<String>,
) -> Result<(), RuntimeError> {
    definition.validate().map_err(RuntimeError::InvalidOption)?;
    for agent in &definition.agents {
        if !known_agents.contains(agent) {
            return Err(RuntimeError::InvalidOption(format!(
                "MCP server assigns unknown agent: {agent}"
            )));
        }
    }
    Ok(())
}

fn looks_like_definition(object: &serde_json::Map<String, serde_json::Value>) -> bool {
    object.contains_key("command")
        || object.contains_key("url")
        || object.contains_key("transport")
        || object.contains_key("type")
}

fn string_field(
    object: &serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Result<Option<String>, RuntimeError> {
    object
        .get(name)
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| RuntimeError::InvalidOption(format!("{name} must be a string")))
        })
        .transpose()
}

fn string_array(
    value: Option<&serde_json::Value>,
    name: &str,
) -> Result<Vec<String>, RuntimeError> {
    value.map_or(Ok(Vec::new()), |value| {
        value
            .as_array()
            .ok_or_else(|| RuntimeError::InvalidOption(format!("{name} must be an array")))?
            .iter()
            .map(|value| {
                value.as_str().map(str::to_owned).ok_or_else(|| {
                    RuntimeError::InvalidOption(format!("{name} items must be strings"))
                })
            })
            .collect()
    })
}

fn string_map(
    value: Option<&serde_json::Value>,
    name: &str,
) -> Result<BTreeMap<String, String>, RuntimeError> {
    value.map_or(Ok(BTreeMap::new()), |value| {
        value
            .as_object()
            .ok_or_else(|| RuntimeError::InvalidOption(format!("{name} must be an object")))?
            .iter()
            .map(|(key, value)| {
                value
                    .as_str()
                    .map(|value| (key.clone(), value.to_owned()))
                    .ok_or_else(|| {
                        RuntimeError::InvalidOption(format!("{name} values must be strings"))
                    })
            })
            .collect()
    })
}

fn bool_alias(
    object: &serde_json::Map<String, serde_json::Value>,
    snake: &str,
    camel: &str,
) -> Result<Option<bool>, RuntimeError> {
    object
        .get(snake)
        .or_else(|| object.get(camel))
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| RuntimeError::InvalidOption(format!("{snake} must be a boolean")))
        })
        .transpose()
}

fn u64_alias(
    object: &serde_json::Map<String, serde_json::Value>,
    snake: &str,
    camel: &str,
) -> Result<Option<u64>, RuntimeError> {
    object
        .get(snake)
        .or_else(|| object.get(camel))
        .map(|value| {
            value.as_u64().ok_or_else(|| {
                RuntimeError::InvalidOption(format!("{snake} must be a positive integer"))
            })
        })
        .transpose()
}

fn validate_name(name: &str) -> Result<&str, RuntimeError> {
    let valid = !name.trim().is_empty()
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'));
    if valid {
        Ok(name)
    } else {
        Err(RuntimeError::InvalidOption(format!(
            "invalid MCP server name {name:?}; use letters, digits, '-' or '_'"
        )))
    }
}

fn validate_mutation(mutation: &McpMutation) -> Result<(), RuntimeError> {
    match mutation {
        McpMutation::Put {
            location,
            name,
            definition,
        } => {
            location.validate().map_err(RuntimeError::InvalidOption)?;
            validate_name(name)?;
            definition.validate().map_err(RuntimeError::InvalidOption)
        }
        McpMutation::Remove { location, name } => {
            location.validate().map_err(RuntimeError::InvalidOption)?;
            validate_name(name).map(|_| ())
        }
        McpMutation::Move {
            from,
            name,
            to,
            new_name,
        } => {
            from.validate().map_err(RuntimeError::InvalidOption)?;
            to.validate().map_err(RuntimeError::InvalidOption)?;
            validate_name(name)?;
            validate_name(new_name).map(|_| ())
        }
    }
}

fn mutation_locations(mutation: &McpMutation) -> Vec<&McpLocation> {
    match mutation {
        McpMutation::Put { location, .. } | McpMutation::Remove { location, .. } => vec![location],
        McpMutation::Move { from, to, .. } => vec![from, to],
    }
}

fn definitions_at_mut<'a>(
    layers: &'a mut ConfigLayers,
    location: &McpLocation,
) -> &'a mut BTreeMap<String, McpServerDefinition> {
    match location.scope {
        McpScope::Global => &mut layers.global,
        McpScope::Project => &mut layers.project,
        McpScope::Agent => &mut layers.global,
    }
}

fn resolve_effective(layers: &ConfigLayers, agent: &str) -> Vec<McpEffectiveServer> {
    let mut resolved =
        BTreeMap::<String, (McpLocation, McpServerDefinition, Vec<McpLocation>)>::new();
    for (name, definition) in &layers.global {
        resolved.insert(
            name.clone(),
            (McpLocation::global(), definition.clone(), Vec::new()),
        );
    }
    for (name, definition) in &layers.project {
        if let Some((location, _, overridden)) = resolved.get_mut(name) {
            overridden.push(location.clone());
        }
        let overridden = resolved
            .remove(name)
            .map(|(_, _, overridden)| overridden)
            .unwrap_or_default();
        resolved.insert(
            name.clone(),
            (McpLocation::project(), definition.clone(), overridden),
        );
    }
    for (name, definition) in &layers.session {
        if let Some((location, _, overridden)) = resolved.get_mut(name) {
            overridden.push(location.clone());
        }
        let overridden = resolved
            .remove(name)
            .map(|(_, _, overridden)| overridden)
            .unwrap_or_default();
        resolved.insert(
            name.clone(),
            (McpLocation::project(), definition.clone(), overridden),
        );
    }
    resolved
        .into_iter()
        .map(|(name, (location, definition, overridden))| {
            let agents = definition.agents.clone();
            McpEffectiveServer {
                allowed_for_agent: agents.is_empty()
                    || agents.iter().any(|assigned| assigned == agent),
                status: if definition.enabled {
                    McpRuntimeStatus::NotStarted
                } else {
                    McpRuntimeStatus::Disabled
                },
                name,
                location,
                definition,
                agents,
                overridden,
                tools: Vec::new(),
                diagnostics: Vec::new(),
                generation: 0,
            }
        })
        .collect()
}

pub(crate) fn global_config_from_document(
    global_path: &Path,
    global: &toml::Value,
) -> Result<McpGlobalConfig, RuntimeError> {
    reject_removed_agent_mcp_layout(global_path, global)?;
    let mut known_agents = BTreeSet::from([crate::DEFAULT_AGENT_NAME.to_owned()]);
    known_agents.extend(
        crate::AgentCatalog::from_document(global)?
            .iter()
            .map(|(name, _)| name.clone()),
    );
    Ok(McpGlobalConfig {
        global: parse_catalog_table(global_path, global.get("mcp"), &known_agents)?,
        known_agents,
    })
}

fn parse_catalog_table(
    path: &Path,
    mcp: Option<&toml::Value>,
    known_agents: &BTreeSet<String>,
) -> Result<BTreeMap<String, McpServerDefinition>, RuntimeError> {
    let Some(mcp) = mcp else {
        return Ok(BTreeMap::new());
    };
    let mcp_table = mcp.as_table().ok_or_else(|| RuntimeError::Config {
        path: path.to_path_buf(),
        message: "mcp must be a table".into(),
    })?;
    let Some(table) = mcp_table.get("servers").and_then(toml::Value::as_table) else {
        if mcp_table.is_empty() {
            return Ok(BTreeMap::new());
        }
        return Err(RuntimeError::Config {
            path: path.to_path_buf(),
            message: "MCP servers must be declared under [mcp.servers.<name>]".into(),
        });
    };
    table
        .iter()
        .map(|(name, value)| {
            validate_name(name)?;
            let definition: McpServerDefinition =
                if let Some(package) = value.get("package").and_then(toml::Value::as_str) {
                    let package_digest = value
                        .get("package_digest")
                        .and_then(toml::Value::as_str)
                        .map(str::to_owned);
                    let parameters = value
                        .get("parameters")
                        .cloned()
                        .unwrap_or_else(|| toml::Value::Table(toml::map::Map::new()))
                        .try_into()
                        .map_err(|error| RuntimeError::Config {
                            path: path.to_path_buf(),
                            message: format!("invalid MCP package parameters for {name}: {error}"),
                        })?;
                    let mut definition = super::package::resolve_stored_package(
                        path,
                        package,
                        name,
                        package_digest,
                        parameters,
                    )
                    .map_err(|error| RuntimeError::Config {
                        path: path.to_path_buf(),
                        message: format!("invalid MCP package {name}: {error}"),
                    })?;
                    definition.enabled = value
                        .get("enabled")
                        .and_then(toml::Value::as_bool)
                        .unwrap_or(true);
                    definition.agents = value
                        .get("agents")
                        .cloned()
                        .unwrap_or_else(|| toml::Value::Array(Vec::new()))
                        .try_into()
                        .map_err(|error| RuntimeError::Config {
                            path: path.to_path_buf(),
                            message: format!("invalid MCP package agents for {name}: {error}"),
                        })?;
                    definition
                } else {
                    value
                        .clone()
                        .try_into()
                        .map_err(|error| RuntimeError::Config {
                            path: path.to_path_buf(),
                            message: format!("invalid MCP server {name}: {error}"),
                        })?
                };
            definition
                .validate()
                .map_err(|message| RuntimeError::Config {
                    path: path.to_path_buf(),
                    message: format!("invalid MCP server {name}: {message}"),
                })?;
            let serialized = toml::to_string(&definition).map_err(|error| RuntimeError::Config {
                path: path.to_path_buf(),
                message: format!("invalid MCP server {name}: {error}"),
            })?;
            let secret_prefix = format!("mcp/{name}/");
            for reference in super::secrets::referenced_secrets(&serialized).map_err(|error| {
                RuntimeError::Config {
                    path: path.to_path_buf(),
                    message: format!("invalid MCP server {name}: {error}"),
                }
            })? {
                if !reference.starts_with(&secret_prefix) {
                    return Err(RuntimeError::Config {
                        path: path.to_path_buf(),
                        message: format!(
                            "MCP server {name} cannot reference secret {reference}; secrets must be scoped under {secret_prefix}"
                        ),
                    });
                }
            }
            for agent in &definition.agents {
                if !known_agents.contains(agent) {
                    return Err(RuntimeError::Config {
                        path: path.to_path_buf(),
                        message: format!("MCP server {name} assigns unknown agent: {agent}"),
                    });
                }
            }
            Ok((name.clone(), definition))
        })
        .collect()
}

fn reject_removed_agent_mcp_layout(
    path: &Path,
    document: &toml::Value,
) -> Result<(), RuntimeError> {
    let Some(agents) = document.get("agents").and_then(toml::Value::as_table) else {
        return Ok(());
    };
    for (name, value) in agents {
        if value.get("mcp").is_some() {
            return Err(RuntimeError::Config {
                path: path.to_path_buf(),
                message: format!(
                    "agents.{name}.mcp was removed; use [mcp.servers.<name>] with agents = [\"{name}\"]"
                ),
            });
        }
    }
    Ok(())
}

fn read_document(path: &Path) -> Result<toml::Value, RuntimeError> {
    match std::fs::read_to_string(path) {
        Ok(source) => source
            .parse::<toml::Value>()
            .map_err(|error| RuntimeError::Config {
                path: path.to_path_buf(),
                message: error.to_string(),
            }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(toml::Value::Table(toml::map::Map::new()))
        }
        Err(error) => Err(RuntimeError::Config {
            path: path.to_path_buf(),
            message: error.to_string(),
        }),
    }
}

fn document_fingerprint(path: &Path) -> Result<String, RuntimeError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(format!("{:x}", Sha256::digest(bytes))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok("missing".into()),
        Err(error) => Err(RuntimeError::Config {
            path: path.to_path_buf(),
            message: error.to_string(),
        }),
    }
}

fn count_exact_server_references(value: &toml::Value, old: &str) -> usize {
    match value {
        toml::Value::Table(table) => table
            .iter()
            .map(|(key, value)| {
                usize::from(key == "server" && value.as_str() == Some(old))
                    + count_exact_server_references(value, old)
            })
            .sum(),
        toml::Value::Array(values) => values
            .iter()
            .map(|value| count_exact_server_references(value, old))
            .sum(),
        _ => 0,
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct TransactionRecord {
    files: Vec<TransactionFile>,
}

#[derive(Debug, Deserialize, Serialize)]
struct TransactionFile {
    target: PathBuf,
    staged: PathBuf,
}

impl McpConfigService {
    fn persist_batch(&self, mutations: &[McpMutation]) -> Result<(), RuntimeError> {
        let mut paths = BTreeSet::new();
        for mutation in mutations {
            for location in mutation_locations(mutation) {
                paths.insert(self.path_for(location)?);
            }
            if matches!(mutation, McpMutation::Move { name, new_name, .. } if name != new_name) {
                paths.insert(self.permissions_path.clone());
            }
        }
        let paths = paths.into_iter().collect::<Vec<_>>();
        let mut locks = Vec::new();
        for path in &paths {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let lock_path = path.with_extension("toml.lock");
            let file = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&lock_path)?;
            file.lock_exclusive()?;
            locks.push(file);
        }

        let mut documents = BTreeMap::new();
        for path in &paths {
            documents.insert(path.clone(), read_editable_document(path)?);
        }
        for mutation in mutations {
            apply_document_mutation(self, &mut documents, mutation)?;
        }
        // Validate every complete candidate before any staging or replacement.
        for (path, document) in &documents {
            document
                .to_string()
                .parse::<toml::Value>()
                .map_err(|error| RuntimeError::Config {
                    path: path.clone(),
                    message: error.to_string(),
                })?;
        }

        let mut record = TransactionRecord { files: Vec::new() };
        for (path, document) in &documents {
            let parent = path.parent().unwrap_or_else(|| Path::new("."));
            std::fs::create_dir_all(parent)?;
            let staged = parent.join(format!(
                ".{}.{}.mcp-stage",
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("config"),
                uuid::Uuid::now_v7()
            ));
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&staged)?;
            file.write_all(document.to_string().as_bytes())?;
            file.sync_all()?;
            record.files.push(TransactionFile {
                target: path.clone(),
                staged,
            });
        }
        let journal = self.transaction_path();
        let journal_source = serde_json::to_vec_pretty(&record)?;
        let mut journal_file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&journal)?;
        journal_file.write_all(&journal_source)?;
        journal_file.sync_all()?;
        sync_parent(&journal)?;
        for file in &record.files {
            std::fs::rename(&file.staged, &file.target)?;
            sync_parent(&file.target)?;
        }
        std::fs::remove_file(&journal)?;
        sync_parent(&journal)?;
        drop(locks);
        Ok(())
    }

    fn transaction_path(&self) -> PathBuf {
        self.global_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(TRANSACTION_FILE)
    }

    fn recover_transaction(&self) -> Result<bool, RuntimeError> {
        let path = self.transaction_path();
        let mut source = match std::fs::read(&path) {
            Ok(source) => source,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        let record: TransactionRecord = simd_json::serde::from_slice(&mut source)
            .map_err(|error| RuntimeError::InvalidOption(error.to_string()))?;
        for file in record.files {
            if file.staged.exists() {
                if let Some(parent) = file.target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::rename(&file.staged, &file.target)?;
                sync_parent(&file.target)?;
            }
        }
        std::fs::remove_file(path)?;
        Ok(true)
    }
}

fn read_editable_document(path: &Path) -> Result<toml_edit::DocumentMut, RuntimeError> {
    match std::fs::read_to_string(path) {
        Ok(source) => {
            source
                .parse::<toml_edit::DocumentMut>()
                .map_err(|error| RuntimeError::Config {
                    path: path.to_path_buf(),
                    message: error.to_string(),
                })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut document = toml_edit::DocumentMut::new();
            document["version"] = toml_edit::value(i64::from(crate::CONFIG_VERSION));
            Ok(document)
        }
        Err(error) => Err(RuntimeError::Config {
            path: path.to_path_buf(),
            message: error.to_string(),
        }),
    }
}

fn apply_document_mutation(
    service: &McpConfigService,
    documents: &mut BTreeMap<PathBuf, toml_edit::DocumentMut>,
    mutation: &McpMutation,
) -> Result<(), RuntimeError> {
    match mutation {
        McpMutation::Put {
            location,
            name,
            definition,
        } => {
            let path = service.path_for(location)?;
            let document = documents.get_mut(&path).ok_or_else(|| {
                RuntimeError::InvalidOption(format!(
                    "missing transaction candidate: {}",
                    path.display()
                ))
            })?;
            put_definition(document, location, name, definition)?;
        }
        McpMutation::Remove { location, name } => {
            let path = service.path_for(location)?;
            let document = documents.get_mut(&path).ok_or_else(|| {
                RuntimeError::InvalidOption(format!(
                    "missing transaction candidate: {}",
                    path.display()
                ))
            })?;
            remove_definition(document, location, name)?;
        }
        McpMutation::Move {
            from,
            name,
            to,
            new_name,
        } => {
            let source_path = service.path_for(from)?;
            let target_path = service.path_for(to)?;
            let definition = {
                let source = documents.get(&source_path).ok_or_else(|| {
                    RuntimeError::InvalidOption("missing source candidate".into())
                })?;
                definition_from_editable(source, from, name)?
            };
            remove_definition(
                documents.get_mut(&source_path).ok_or_else(|| {
                    RuntimeError::InvalidOption("missing source candidate".into())
                })?,
                from,
                name,
            )?;
            put_definition(
                documents.get_mut(&target_path).ok_or_else(|| {
                    RuntimeError::InvalidOption("missing destination candidate".into())
                })?,
                to,
                new_name,
                &definition,
            )?;
            if name != new_name
                && let Some(permissions) = documents.get_mut(&service.permissions_path)
            {
                rewrite_exact_server_fields(permissions.as_item_mut(), name, new_name);
            }
        }
    }
    Ok(())
}

fn definition_path<'a>(
    document: &'a toml_edit::DocumentMut,
    location: &McpLocation,
    name: &str,
) -> Option<&'a toml_edit::Item> {
    match location.scope {
        McpScope::Global | McpScope::Project => document.get("mcp")?.get("servers")?.get(name),
        McpScope::Agent => document
            .get("agents")?
            .get(location.agent.as_deref()?)?
            .get("mcp")?
            .get("servers")?
            .get(name),
    }
}

fn definition_from_editable(
    document: &toml_edit::DocumentMut,
    location: &McpLocation,
    name: &str,
) -> Result<McpServerDefinition, RuntimeError> {
    let item = definition_path(document, location, name)
        .ok_or_else(|| RuntimeError::InvalidOption(format!("unknown MCP server {name}")))?;
    toml::from_str(&item.to_string())
        .map_err(|error| RuntimeError::InvalidOption(error.to_string()))
}

fn definition_item(definition: &McpServerDefinition) -> Result<toml_edit::Item, RuntimeError> {
    #[derive(Serialize)]
    struct PackageReference<'a> {
        package: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        package_digest: &'a Option<String>,
        enabled: bool,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        agents: &'a Vec<String>,
        #[serde(skip_serializing_if = "BTreeMap::is_empty")]
        parameters: &'a BTreeMap<String, super::McpParameterValue>,
    }
    let source = if let Some(package) = definition.package.as_deref() {
        toml::to_string(&PackageReference {
            package,
            package_digest: &definition.package_digest,
            enabled: definition.enabled,
            agents: &definition.agents,
            parameters: &definition.parameters,
        })
    } else {
        toml::to_string(definition)
    }
    .map_err(|error| {
        RuntimeError::InvalidOption(format!("cannot serialize MCP definition: {error}"))
    })?;
    let document = source
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| RuntimeError::InvalidOption(error.to_string()))?;
    Ok(document.into_table().into())
}

fn put_definition(
    document: &mut toml_edit::DocumentMut,
    location: &McpLocation,
    name: &str,
    definition: &McpServerDefinition,
) -> Result<(), RuntimeError> {
    let item = definition_item(definition)?;
    match location.scope {
        McpScope::Global | McpScope::Project => {
            let mcp = ensure_child_table(document.as_table_mut(), "mcp")?;
            ensure_child_table(mcp, "servers")?.insert(name, item);
        }
        McpScope::Agent => {
            let agent = location.agent.as_deref().unwrap_or_default();
            let agents = ensure_child_table(document.as_table_mut(), "agents")?;
            let agent = ensure_child_table(agents, agent)?;
            let mcp = ensure_child_table(agent, "mcp")?;
            ensure_child_table(mcp, "servers")?.insert(name, item);
        }
    }
    Ok(())
}

fn ensure_child_table<'a>(
    table: &'a mut toml_edit::Table,
    key: &str,
) -> Result<&'a mut toml_edit::Table, RuntimeError> {
    if !table.contains_key(key) {
        table.insert(key, toml_edit::Item::Table(implicit_table()));
    }
    table
        .get_mut(key)
        .and_then(toml_edit::Item::as_table_mut)
        .ok_or_else(|| {
            RuntimeError::InvalidOption(format!("configuration field {key} must be a table"))
        })
}

fn remove_definition(
    document: &mut toml_edit::DocumentMut,
    location: &McpLocation,
    name: &str,
) -> Result<(), RuntimeError> {
    let removed = match location.scope {
        McpScope::Global | McpScope::Project => document
            .get_mut("mcp")
            .and_then(|mcp| mcp.get_mut("servers"))
            .and_then(toml_edit::Item::as_table_like_mut)
            .and_then(|table| table.remove(name)),
        McpScope::Agent => document
            .get_mut("agents")
            .and_then(|agents| agents.get_mut(location.agent.as_deref().unwrap_or_default()))
            .and_then(|agent| agent.get_mut("mcp"))
            .and_then(|mcp| mcp.get_mut("servers"))
            .and_then(toml_edit::Item::as_table_like_mut)
            .and_then(|table| table.remove(name)),
    };
    if removed.is_none() {
        return Err(RuntimeError::InvalidOption(format!(
            "unknown MCP server {name} at requested scope"
        )));
    }
    Ok(())
}

fn rewrite_exact_server_fields(item: &mut toml_edit::Item, old: &str, new: &str) {
    if let Some(table) = item.as_table_like_mut() {
        for (key, value) in table.iter_mut() {
            if key == "server" && value.as_str() == Some(old) {
                *value = toml_edit::value(new);
            } else {
                rewrite_exact_server_fields(value, old, new);
            }
        }
    } else if let Some(array) = item.as_array_of_tables_mut() {
        for table in array.iter_mut() {
            for (key, value) in table.iter_mut() {
                if key == "server" && value.as_str() == Some(old) {
                    *value = toml_edit::value(new);
                } else {
                    rewrite_exact_server_fields(value, old, new);
                }
            }
        }
    }
}

fn sync_parent(path: &Path) -> Result<(), RuntimeError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stdio(command: &str) -> McpServerDefinition {
        McpServerDefinition {
            transport: McpTransportConfig::Stdio {
                command: command.into(),
                args: Vec::new(),
                cwd: None,
                env: BTreeMap::new(),
                env_remove: Vec::new(),
                inherit_env: true,
            },
            enabled: true,
            agents: Vec::new(),
            eager: false,
            startup_timeout_seconds: 10,
            request_timeout_seconds: 60,
            read_only_tools: Vec::new(),
            ..McpServerDefinition::default()
        }
    }

    #[test]
    fn json_import_accepts_wrappers_arrays_aliases_and_rejects_sse() {
        let parsed = parse_mcp_json(
            r#"{"mcpServers":{"one":{"command":["node","server.js"],"environment":{"TOKEN":"${env:TOKEN}"}},"two":{"type":"streamable-http","url":"https://example.test/mcp","disabled":true}}}"#,
            None,
        )
        .unwrap();
        assert_eq!(parsed.len(), 2);
        assert!(!parsed["two"].enabled);
        assert!(matches!(
            &parsed["one"].transport,
            McpTransportConfig::Stdio { args, .. } if args == &["server.js"]
        ));
        assert!(parse_mcp_json(r#"{"x":{"type":"sse","url":"https://x"}}"#, None).is_err());
    }

    #[test]
    fn shared_catalog_validates_assignments_and_rejects_removed_agent_layout() {
        let valid: toml::Value = r#"
            version = 1
            [agents.home]
            description = "Home"
            [mcp.servers.home]
            transport = "stdio"
            command = "home-mcp"
            agents = ["home"]
        "#
        .parse()
        .unwrap();
        let parsed = global_config_from_document(Path::new("config.toml"), &valid).unwrap();
        assert_eq!(parsed.global["home"].agents, ["home"]);

        let unknown: toml::Value = r#"
            version = 1
            [mcp.servers.home]
            transport = "stdio"
            command = "home-mcp"
            agents = ["missing"]
        "#
        .parse()
        .unwrap();
        assert!(global_config_from_document(Path::new("config.toml"), &unknown).is_err());

        let removed: toml::Value = r#"
            version = 1
            [agents.home.mcp]
            allow = ["home"]
        "#
        .parse()
        .unwrap();
        assert!(global_config_from_document(Path::new("config.toml"), &removed).is_err());
    }

    #[test]
    fn server_cannot_reference_another_servers_managed_secret() {
        let invalid: toml::Value = r#"
            version = 1
            [mcp.servers.github]
            transport = "http"
            url = "https://example.test/mcp"
            [mcp.servers.github.headers]
            Authorization = "Bearer ${secret:mcp/gitlab/pat}"
        "#
        .parse()
        .unwrap();
        let error = global_config_from_document(Path::new("config.toml"), &invalid)
            .unwrap_err()
            .to_string();
        assert!(error.contains("cannot reference secret mcp/gitlab/pat"));

        let valid: toml::Value = r#"
            version = 1
            [mcp.servers.github]
            transport = "http"
            url = "https://example.test/mcp"
            [mcp.servers.github.headers]
            Authorization = "Bearer ${secret:mcp/github/pat}"
        "#
        .parse()
        .unwrap();
        assert!(global_config_from_document(Path::new("config.toml"), &valid).is_ok());
    }

    #[test]
    fn session_servers_shadow_persistent_catalog_without_mutating_it() {
        let layers = ConfigLayers {
            global: BTreeMap::from([("docs".into(), stdio("global"))]),
            project: BTreeMap::from([("docs".into(), stdio("project"))]),
            session: BTreeMap::from([("docs".into(), stdio("session"))]),
            known_agents: BTreeSet::new(),
        };
        let effective = resolve_effective(&layers, "general");
        assert!(matches!(
            &effective[0].definition.transport,
            McpTransportConfig::Stdio { command, .. } if command == "session"
        ));
    }

    #[test]
    fn resolution_honors_project_shadowing_and_agent_assignments() {
        let layers = ConfigLayers {
            global: BTreeMap::from([("docs".into(), stdio("global"))]),
            project: BTreeMap::from([(
                "docs".into(),
                McpServerDefinition {
                    enabled: false,
                    agents: vec!["general".into()],
                    ..stdio("project")
                },
            )]),
            session: BTreeMap::new(),
            known_agents: BTreeSet::from(["general".into(), "explore".into()]),
        };
        let resolved = resolve_effective(&layers, "general");
        let docs = resolved
            .iter()
            .find(|server| server.name == "docs")
            .unwrap();
        assert!(!docs.definition.enabled);
        assert!(docs.allowed_for_agent);
        assert_eq!(docs.location, McpLocation::project());
        assert_eq!(docs.agents, ["general"]);
    }

    #[tokio::test]
    async fn existing_service_uses_reloaded_global_config_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let global = temp.path().join("config.toml");
        std::fs::write(
            &global,
            "version = 1\n[mcp.servers.docs]\ntransport = 'stdio'\ncommand = 'old-docs'\n",
        )
        .unwrap();
        let store = crate::ConfigStore::open(&global).unwrap();
        let mut snapshots = store.subscribe();
        let service =
            McpConfigService::from_store(store, global.clone(), &workspace, true).unwrap();
        assert!(matches!(
            &service.get_effective("general", "docs").unwrap().unwrap().definition.transport,
            McpTransportConfig::Stdio { command, .. } if command == "old-docs"
        ));

        std::fs::write(
            &global,
            "version = 1\n[mcp.servers.docs]\ntransport = 'stdio'\ncommand = 'new-docs'\n",
        )
        .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), snapshots.changed())
            .await
            .unwrap()
            .unwrap();

        assert!(matches!(
            &service.get_effective("general", "docs").unwrap().unwrap().definition.transport,
            McpTransportConfig::Stdio { command, .. } if command == "new-docs"
        ));
    }

    #[test]
    fn existing_service_reloads_project_config_and_retains_last_valid_candidate() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let global = temp.path().join("config.toml");
        std::fs::write(&global, "version = 1\n").unwrap();
        let service = McpConfigService::load(global, &workspace, true).unwrap();
        let project = workspace.join(".cagent/config.toml");
        std::fs::create_dir_all(project.parent().unwrap()).unwrap();
        std::fs::write(
            &project,
            "[mcp.servers.docs]\ntransport = 'stdio'\ncommand = 'project-docs'\n",
        )
        .unwrap();

        assert_eq!(
            service
                .get_effective("general", "docs")
                .unwrap()
                .unwrap()
                .location,
            McpLocation::project()
        );

        std::fs::write(&project, "[mcp.servers.docs]\ntransport = 'stdio'\n").unwrap();
        assert!(service.get_effective("general", "docs").unwrap().is_some());
    }

    #[test]
    fn management_round_trips_shared_definitions_without_losing_comments() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let global = temp.path().join("config.toml");
        std::fs::write(&global, "# keep this comment\nversion = 1\n").unwrap();
        let service = McpConfigService::load(global.clone(), &workspace, true).unwrap();

        let global_preview = service
            .preview_mutations(
                "general",
                vec![McpMutation::Put {
                    location: McpLocation::global(),
                    name: "global-server".into(),
                    definition: stdio("global-command"),
                }],
            )
            .unwrap();
        service
            .apply_mutations("general", global_preview, false)
            .unwrap();

        let import = service
            .preview_import_json(
                McpLocation::global(),
                r#"{"shared-server":{"command":"agent-command","agents":["general"]}}"#,
                None,
            )
            .unwrap();
        service.apply_import("general", import, false).unwrap();

        let source = std::fs::read_to_string(&global).unwrap();
        assert!(source.starts_with("# keep this comment"));
        let reloaded = McpConfigService::load(global, &workspace, true).unwrap();
        let all = reloaded.list_all().unwrap();
        assert!(all.iter().any(|(location, name, definition)| {
            location == &McpLocation::global()
                && name == "global-server"
                && matches!(
                    &definition.transport,
                    McpTransportConfig::Stdio { command, .. } if command == "global-command"
                )
        }));
        assert!(all.iter().any(|(location, name, definition)| {
            location == &McpLocation::global()
                && name == "shared-server"
                && definition.agents == ["general"]
        }));
    }

    #[test]
    fn rename_rewrites_only_exact_permission_references() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let global = temp.path().join("config.toml");
        std::fs::write(
            &global,
            r#"version = 1

[mcp.servers.old]
transport = "stdio"
command = "old-command"
"#,
        )
        .unwrap();
        let permissions = temp.path().join("permissions.toml");
        std::fs::write(
            &permissions,
            r#"version = 1

[[global.rule]]
id = "exact"
effect = "allow"
tool = "mcp"
server = "old"

[[global.rule]]
id = "pattern"
effect = "allow"
tool = "mcp"
server = "old*"
"#,
        )
        .unwrap();
        let service = McpConfigService::load(global.clone(), &workspace, true).unwrap();
        let preview = service
            .preview_mutations(
                "review",
                vec![McpMutation::Move {
                    from: McpLocation::global(),
                    name: "old".into(),
                    to: McpLocation::global(),
                    new_name: "new".into(),
                }],
            )
            .unwrap();
        assert!(preview.touched_agent_policies.is_empty());
        assert_eq!(preview.touched_permission_rules.len(), 1);
        service.apply_mutations("review", preview, false).unwrap();

        let global = std::fs::read_to_string(global).unwrap();
        assert!(global.contains("[mcp.servers.new]"));
        assert!(!global.contains("[mcp.servers.old]"));
        let permissions = std::fs::read_to_string(permissions).unwrap();
        assert!(permissions.contains("server = \"new\""));
        assert!(permissions.contains("server = \"old*\""));
    }

    #[test]
    fn interrupted_transaction_is_recovered_before_configuration_load() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let global = temp.path().join("config.toml");
        std::fs::write(&global, "version = 1\n").unwrap();
        let staged = temp.path().join("staged.toml");
        std::fs::write(
            &staged,
            "version = 1\n[mcp.servers.recovered]\ntransport = \"stdio\"\ncommand = \"fixture\"\n",
        )
        .unwrap();
        let journal = TransactionRecord {
            files: vec![TransactionFile {
                target: global.clone(),
                staged: staged.clone(),
            }],
        };
        std::fs::write(
            temp.path().join(TRANSACTION_FILE),
            serde_json::to_vec(&journal).unwrap(),
        )
        .unwrap();

        let service = McpConfigService::load(global, &workspace, true).unwrap();
        assert!(
            service
                .get_effective("general", "recovered")
                .unwrap()
                .is_some()
        );
        assert!(!staged.exists());
        assert!(!temp.path().join(TRANSACTION_FILE).exists());
    }
}
