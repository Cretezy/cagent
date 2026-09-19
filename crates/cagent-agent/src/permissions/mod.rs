//! Frontend-neutral permission resources and policy evaluation.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::RuntimeError;
use crate::config::implicit_table;

pub const PERMISSIONS_VERSION: u16 = 1;

/// The outcome of evaluating a permission resource.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionEffect {
    Allow,
    Ask,
    Deny,
}

/// The filesystem access performed by a tool request.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionAccess {
    Read,
    Write,
    Execute,
}

impl PermissionAccess {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Execute => "execute",
        }
    }
}

/// A normalized request presented to the permission engine.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PermissionResource {
    pub tool: String,
    /// Original MCP server identity when `tool == "mcp"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    /// Original MCP tool name when `tool == "mcp"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    /// Canonical absolute path using `/` as its separator.
    pub path: Option<String>,
    pub access: Option<PermissionAccess>,
    pub mode: String,
    pub agent: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

/// One structured rule. Omitted fields are unconstrained.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionRule {
    pub id: String,
    pub effect: PermissionEffect,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<String>,
    /// Also authorizes crossing the active workspace boundary.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub external: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
}

impl PermissionRule {
    /// Returns the constraint a user can edit for an approval-created rule.
    ///
    /// Bash rules are stored as parsed words whenever possible, while opaque
    /// shell input uses `raw_command`. Keeping that distinction here lets every
    /// frontend present the actual rule rather than assuming it is a path rule.
    #[must_use]
    pub fn editable_pattern(&self) -> Option<(String, &'static str)> {
        if let Some(path) = &self.path {
            return Some((path.clone(), "path pattern"));
        }
        if let Some(command) = &self.command {
            return Some((command.join(" "), "command pattern"));
        }
        self.raw_command
            .as_ref()
            .map(|command| (command.clone(), "command pattern"))
    }

    /// Describes the wildcard syntax accepted by the editable constraint.
    #[must_use]
    pub fn editable_pattern_hint(&self) -> &'static str {
        if self.tool.as_deref() == Some("bash") && self.path.is_none() {
            "wildcard: * (for example, echo *)"
        } else {
            "glob patterns: *, **, ?"
        }
    }

    /// Replaces the user-editable constraint on an approval-created rule.
    ///
    /// Parsed Bash command rules use whitespace-separated command words. This
    /// preserves wildcard words such as `*` without interpreting shell syntax
    /// in the frontend.
    pub fn set_editable_pattern(&mut self, pattern: String) {
        if self.path.is_some() {
            self.path = Some(pattern);
        } else if self.command.is_some() {
            self.command = Some(pattern.split_whitespace().map(ToOwned::to_owned).collect());
        } else if self.raw_command.is_some() {
            self.raw_command = Some(pattern);
        }
    }
}

/// The source layer of a matched rule.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionLayerKind {
    Conversation,
    Project,
    Agent,
    Mode,
    Global,
    Default,
}

/// Rules resolved in the order required by the permission contract.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PermissionPolicy {
    /// In-memory rules granted for the lifetime of the active session.
    pub session: Vec<PermissionRule>,
    pub project: Vec<PermissionRule>,
    pub agent: Vec<PermissionRule>,
    pub mode: Vec<PermissionRule>,
    pub global: Vec<PermissionRule>,
}

/// An explainable permission result suitable for any frontend.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PermissionDecision {
    pub effect: PermissionEffect,
    pub layer: PermissionLayerKind,
    pub rule_id: Option<String>,
    pub reason: String,
}

/// The independent operation and external-boundary decisions for a filesystem
/// request. Outside access is allowed only when both decisions allow it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FilesystemPermissionDecision {
    pub effect: PermissionEffect,
    pub operation: PermissionDecision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external: Option<PermissionDecision>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionScope {
    Conversation,
    /// TUI approval state: conversation-scoped request with a global persistent choice.
    #[serde(skip)]
    ConversationGlobal,
    Project,
    Global,
}

/// Durable explanation recorded for each authorization decision.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PermissionAudit {
    pub resource: PermissionResource,
    pub decision: FilesystemPermissionDecision,
    pub outcome: PermissionEffect,
    /// Optional explanation submitted by the user when denying this request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<PermissionScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_pattern: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_pattern: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resulting_rule_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classifier: Option<crate::AutoClassifierRecord>,
}

#[derive(Debug, Default, Deserialize)]
struct PermissionDocument {
    version: u16,
    #[serde(default)]
    global: RuleSection,
    #[serde(default)]
    project: BTreeMap<String, RuleSection>,
}

#[derive(Debug, Default, Deserialize)]
struct RuleSection {
    #[serde(default)]
    trusted: bool,
    #[serde(default)]
    rule: Vec<PermissionRule>,
}

/// Loader and serialized atomic writer for the single permissions file.
#[derive(Clone, Debug)]
pub struct PermissionFile {
    path: PathBuf,
    workspace: PathBuf,
}

impl PermissionFile {
    /// Creates a permission-file view for one canonical workspace.
    ///
    /// # Errors
    ///
    /// Returns a permission error when the workspace cannot be canonicalized.
    pub fn new(path: PathBuf, workspace: &Path) -> Result<Self, RuntimeError> {
        let workspace = workspace
            .canonicalize()
            .map_err(|error| RuntimeError::Permissions {
                path: path.clone(),
                message: format!(
                    "cannot canonicalize workspace {}: {error}",
                    workspace.display()
                ),
            })?;
        Ok(Self { path, workspace })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Loads the global and matching canonical-project rules.
    ///
    /// # Errors
    ///
    /// Returns a permission error for unreadable, malformed, or unsupported files.
    pub fn load(&self) -> Result<PermissionPolicy, RuntimeError> {
        let document = self.read_document()?;
        Ok(PermissionPolicy {
            project: document
                .project
                .get(&permission_path(&self.workspace))
                .map(|section| section.rule.clone())
                .unwrap_or_default(),
            global: document.global.rule,
            ..PermissionPolicy::default()
        })
    }

    /// Returns whether this canonical workspace has persisted project trust.
    ///
    /// # Errors
    ///
    /// Returns a permission error for unreadable, malformed, or unsupported files.
    pub fn is_trusted(&self) -> Result<bool, RuntimeError> {
        let document = self.read_document()?;
        Ok(document
            .project
            .get(&permission_path(&self.workspace))
            .is_some_and(|section| section.trusted))
    }

    /// Persists or clears trust for this canonical workspace.
    ///
    /// # Errors
    ///
    /// Returns a permission error when validation, locking, or persistence fails.
    pub fn set_trusted(&self, trusted: bool) -> Result<(), RuntimeError> {
        let workspace = permission_path(&self.workspace);
        self.edit_document(|document, _| {
            let project = ensure_child_table(document.as_table_mut(), "project", &self.path)?;
            let project = ensure_child_table(project, &workspace, &self.path)?;
            project.insert("trusted", toml_edit::value(trusted));
            Ok(())
        })
    }

    /// Adds one stable rule through the comment-preserving TOML editor.
    ///
    /// # Errors
    ///
    /// Returns a permission error when validation, locking, or persistence fails.
    pub fn persist_rule(
        &self,
        scope: PermissionScope,
        rule: PermissionRule,
    ) -> Result<PermissionRule, RuntimeError> {
        self.persist_rules(scope, vec![rule])?
            .pop()
            .ok_or_else(|| RuntimeError::Permissions {
                path: self.path.clone(),
                message: "no permission rule was supplied".into(),
            })
    }

    /// Adds several rules in one locked atomic replacement.
    ///
    /// # Errors
    ///
    /// Returns a permission error when validation, locking, or persistence fails.
    pub fn persist_rules(
        &self,
        scope: PermissionScope,
        mut rules: Vec<PermissionRule>,
    ) -> Result<Vec<PermissionRule>, RuntimeError> {
        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .to_string();
        for rule in &mut rules {
            if rule.id.trim().is_empty() {
                rule.id = uuid::Uuid::now_v7().to_string();
            }
            if rule.source.is_none() {
                rule.source = Some("approval".into());
            }
            if rule.created_at.is_none() {
                rule.created_at = Some(created_at.clone());
            }
        }
        validate_rules(&self.path, &rules)?;
        self.edit_document(|document, current| {
            if let Some(existing) = current
                .global
                .rule
                .iter()
                .chain(current.project.values().flat_map(|section| &section.rule))
                .find(|existing| rules.iter().any(|rule| rule.id == existing.id))
            {
                return Err(RuntimeError::Permissions {
                    path: self.path.clone(),
                    message: format!("permission rule ID already exists: {}", existing.id),
                });
            }
            for rule in &rules {
                append_rule(document, scope, &self.workspace, rule, &self.path)?;
            }
            Ok(())
        })?;
        Ok(rules)
    }

    /// Replaces one rule in its existing scope while retaining its stable metadata.
    pub fn update_rule(
        &self,
        scope: PermissionScope,
        id: &str,
        mut rule: PermissionRule,
    ) -> Result<PermissionRule, RuntimeError> {
        validate_rules(&self.path, std::slice::from_ref(&rule))?;
        self.edit_document(|document, current| {
            let existing = scoped_rules(current, scope, &self.workspace)
                .iter()
                .find(|candidate| candidate.id == id)
                .ok_or_else(|| missing_rule_error(&self.path, id))?;
            rule.id = existing.id.clone();
            rule.source = existing.source.clone();
            rule.created_at = existing.created_at.clone();
            validate_rules(&self.path, std::slice::from_ref(&rule))?;
            let rules = scoped_rule_tables_mut(document, scope, &self.workspace, &self.path)?;
            let index = rules
                .iter()
                .position(|table| table.get("id").and_then(toml_edit::Item::as_str) == Some(id))
                .ok_or_else(|| missing_rule_error(&self.path, id))?;
            let table = toml_edit::ser::to_document(&rule)
                .map_err(|error| permission_error(&self.path, error))?
                .into_table();
            *rules.get_mut(index).expect("permission rule index exists") = table;
            Ok(())
        })?;
        Ok(rule)
    }

    /// Deletes one rule from the requested scope.
    pub fn delete_rule(&self, scope: PermissionScope, id: &str) -> Result<(), RuntimeError> {
        self.edit_document(|document, current| {
            if !scoped_rules(current, scope, &self.workspace)
                .iter()
                .any(|rule| rule.id == id)
            {
                return Err(missing_rule_error(&self.path, id));
            }
            let rules = scoped_rule_tables_mut(document, scope, &self.workspace, &self.path)?;
            let index = rules
                .iter()
                .position(|table| table.get("id").and_then(toml_edit::Item::as_str) == Some(id))
                .ok_or_else(|| missing_rule_error(&self.path, id))?;
            rules.remove(index);
            Ok(())
        })
    }

    fn edit_document(
        &self,
        edit: impl FnOnce(&mut toml_edit::DocumentMut, &PermissionDocument) -> Result<(), RuntimeError>,
    ) -> Result<(), RuntimeError> {
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent).map_err(|error| permission_error(&self.path, error))?;
        let _lock = PermissionLock::acquire(&self.path)?;
        let source = match std::fs::read_to_string(&self.path) {
            Ok(source) => source,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                format!("version = {PERMISSIONS_VERSION}\n")
            }
            Err(error) => return Err(permission_error(&self.path, error)),
        };
        let current: PermissionDocument =
            toml::from_str(&source).map_err(|error| permission_error(&self.path, error))?;
        self.validate_document(&current)?;
        let mut document = source
            .parse::<toml_edit::DocumentMut>()
            .map_err(|error| permission_error(&self.path, error))?;
        edit(&mut document, &current)?;

        let candidate = document.to_string();
        let candidate_document: PermissionDocument =
            toml::from_str(&candidate).map_err(|error| permission_error(&self.path, error))?;
        self.validate_document(&candidate_document)?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)
            .map_err(|error| permission_error(&self.path, error))?;
        temporary
            .write_all(candidate.as_bytes())
            .and_then(|()| temporary.as_file().sync_all())
            .map_err(|error| permission_error(&self.path, error))?;
        temporary
            .persist(&self.path)
            .map_err(|error| permission_error(&self.path, error.error))?;
        Ok(())
    }

    fn read_document(&self) -> Result<PermissionDocument, RuntimeError> {
        let source = match std::fs::read_to_string(&self.path) {
            Ok(source) => source,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(PermissionDocument {
                    version: PERMISSIONS_VERSION,
                    ..PermissionDocument::default()
                });
            }
            Err(error) => return Err(permission_error(&self.path, error)),
        };
        let document: PermissionDocument =
            toml::from_str(&source).map_err(|error| permission_error(&self.path, error))?;
        self.validate_document(&document)?;
        Ok(document)
    }

    fn validate_document(&self, document: &PermissionDocument) -> Result<(), RuntimeError> {
        if document.version != PERMISSIONS_VERSION {
            return Err(RuntimeError::Permissions {
                path: self.path.clone(),
                message: format!(
                    "unsupported permissions version {}; expected {PERMISSIONS_VERSION}",
                    document.version
                ),
            });
        }
        validate_rules(&self.path, &document.global.rule)?;
        for rules in document.project.values() {
            validate_rules(&self.path, &rules.rule)?;
        }
        Ok(())
    }
}

fn scoped_rules<'a>(
    document: &'a PermissionDocument,
    scope: PermissionScope,
    workspace: &Path,
) -> &'a [PermissionRule] {
    match scope {
        PermissionScope::Conversation | PermissionScope::ConversationGlobal => &[],
        PermissionScope::Global => &document.global.rule,
        PermissionScope::Project => document
            .project
            .get(&permission_path(workspace))
            .map_or(&[], |section| section.rule.as_slice()),
    }
}

fn missing_rule_error(path: &Path, id: &str) -> RuntimeError {
    RuntimeError::Permissions {
        path: path.to_path_buf(),
        message: format!("permission rule not found: {id}"),
    }
}

fn scoped_rule_tables_mut<'a>(
    document: &'a mut toml_edit::DocumentMut,
    scope: PermissionScope,
    workspace: &Path,
    path: &Path,
) -> Result<&'a mut toml_edit::ArrayOfTables, RuntimeError> {
    let section = match scope {
        PermissionScope::Conversation | PermissionScope::ConversationGlobal => {
            return Err(RuntimeError::Permissions {
                path: path.to_path_buf(),
                message: "conversation permission rules are stored in the conversation database"
                    .into(),
            });
        }
        PermissionScope::Global => ensure_child_table(document.as_table_mut(), "global", path)?,
        PermissionScope::Project => {
            let project = ensure_child_table(document.as_table_mut(), "project", path)?;
            ensure_child_table(project, &permission_path(workspace), path)?
        }
    };
    section
        .get_mut("rule")
        .and_then(toml_edit::Item::as_array_of_tables_mut)
        .ok_or_else(|| RuntimeError::Permissions {
            path: path.to_path_buf(),
            message: "rule must be an array of tables".into(),
        })
}

fn append_rule(
    document: &mut toml_edit::DocumentMut,
    scope: PermissionScope,
    workspace: &Path,
    rule: &PermissionRule,
    path: &Path,
) -> Result<(), RuntimeError> {
    let section = match scope {
        PermissionScope::Conversation | PermissionScope::ConversationGlobal => {
            return Err(RuntimeError::Permissions {
                path: path.to_path_buf(),
                message: "conversation permission rules are stored in the conversation database"
                    .into(),
            });
        }
        PermissionScope::Global => ensure_child_table(document.as_table_mut(), "global", path)?,
        PermissionScope::Project => {
            let project = ensure_child_table(document.as_table_mut(), "project", path)?;
            ensure_child_table(project, &permission_path(workspace), path)?
        }
    };
    if !section.contains_key("rule") {
        section.insert(
            "rule",
            toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new()),
        );
    }
    let rules = section
        .get_mut("rule")
        .and_then(toml_edit::Item::as_array_of_tables_mut)
        .ok_or_else(|| RuntimeError::Permissions {
            path: path.to_path_buf(),
            message: "rule must be an array of tables".into(),
        })?;
    let table = toml_edit::ser::to_document(rule)
        .map_err(|error| permission_error(path, error))?
        .into_table();
    rules.push(table);
    Ok(())
}

fn ensure_child_table<'a>(
    parent: &'a mut dyn toml_edit::TableLike,
    key: &str,
    path: &Path,
) -> Result<&'a mut dyn toml_edit::TableLike, RuntimeError> {
    if !parent.contains_key(key) {
        parent.insert(key, toml_edit::Item::Table(implicit_table()));
    }
    parent
        .get_mut(key)
        .and_then(toml_edit::Item::as_table_like_mut)
        .ok_or_else(|| RuntimeError::Permissions {
            path: path.to_path_buf(),
            message: format!("{key} must be a TOML table"),
        })
}

fn validate_rules(path: &Path, rules: &[PermissionRule]) -> Result<(), RuntimeError> {
    let mut ids = std::collections::HashSet::new();
    for rule in rules {
        if rule.id.trim().is_empty() {
            return Err(RuntimeError::Permissions {
                path: path.to_path_buf(),
                message: "permission rule IDs must not be empty".into(),
            });
        }
        if !ids.insert(&rule.id) {
            return Err(RuntimeError::Permissions {
                path: path.to_path_buf(),
                message: format!("duplicate permission rule ID: {}", rule.id),
            });
        }
        if rule
            .access
            .as_deref()
            .is_some_and(|access| !matches!(access, "read" | "write" | "execute" | "*"))
        {
            return Err(RuntimeError::Permissions {
                path: path.to_path_buf(),
                message: format!("invalid access on permission rule {}", rule.id),
            });
        }
    }
    Ok(())
}

struct PermissionLock {
    path: PathBuf,
}

impl PermissionLock {
    fn acquire(permission_path: &Path) -> Result<Self, RuntimeError> {
        let lock_path = permission_path.with_extension("toml.lock");
        for _ in 0..100 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
            {
                Ok(mut file) => {
                    let _ = writeln!(file, "{}", std::process::id());
                    return Ok(Self { path: lock_path });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => return Err(permission_error(permission_path, error)),
            }
        }
        Err(RuntimeError::Permissions {
            path: permission_path.to_path_buf(),
            message: "timed out waiting for the permissions file lock".into(),
        })
    }
}

impl Drop for PermissionLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn permission_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn permission_error(path: &Path, error: impl std::fmt::Display) -> RuntimeError {
    RuntimeError::Permissions {
        path: path.to_path_buf(),
        message: error.to_string(),
    }
}

impl PermissionPolicy {
    /// Evaluates a resource using terminal deny semantics and layer precedence.
    ///
    /// Any matching deny is terminal. Otherwise the first layer containing a
    /// match wins. Within a layer, more constrained rules win, then rules with
    /// more literal pattern characters, then later file order.
    #[must_use]
    #[tracing::instrument(level = "trace", name = "agent.permission.evaluate", skip_all)]
    pub fn evaluate(
        &self,
        resource: &PermissionResource,
        default: PermissionEffect,
    ) -> PermissionDecision {
        self.evaluate_matching(resource, default, |rule| !rule.external)
    }

    fn evaluate_matching(
        &self,
        resource: &PermissionResource,
        default: PermissionEffect,
        include: impl Fn(&PermissionRule) -> bool,
    ) -> PermissionDecision {
        let layers = [
            (PermissionLayerKind::Conversation, self.session.as_slice()),
            (PermissionLayerKind::Project, self.project.as_slice()),
            (PermissionLayerKind::Agent, self.agent.as_slice()),
            (PermissionLayerKind::Mode, self.mode.as_slice()),
            (PermissionLayerKind::Global, self.global.as_slice()),
        ];

        for (kind, rules) in layers {
            if let Some(rule) =
                strongest_matching_rule(rules, resource, Some(PermissionEffect::Deny), &include)
            {
                return matched_decision(kind, rule);
            }
        }
        for (kind, rules) in layers {
            if let Some(rule) = strongest_matching_rule(rules, resource, None, &include) {
                return matched_decision(kind, rule);
            }
        }

        PermissionDecision {
            effect: default,
            layer: PermissionLayerKind::Default,
            rule_id: None,
            reason: format!("no permission rule matched; using the {default:?} tool default")
                .to_lowercase(),
        }
    }

    /// Evaluates a filesystem operation and, for outside paths, the additional
    /// external-boundary permission required by the specification.
    #[must_use]
    #[tracing::instrument(
        level = "trace",
        name = "agent.permission.evaluate_filesystem",
        skip_all
    )]
    pub fn evaluate_filesystem(
        &self,
        resource: &PermissionResource,
        outside_workspace: bool,
        operation_default: PermissionEffect,
    ) -> FilesystemPermissionDecision {
        let operation = self.evaluate_matching(resource, operation_default, |rule| {
            !rule.external || outside_workspace
        });
        let external = outside_workspace
            .then(|| self.evaluate_matching(resource, PermissionEffect::Ask, |rule| rule.external));
        let effect = external.as_ref().map_or(operation.effect, |external| {
            aggregate_effect(operation.effect, external.effect)
        });
        FilesystemPermissionDecision {
            effect,
            operation,
            external,
        }
    }
}

const fn aggregate_effect(left: PermissionEffect, right: PermissionEffect) -> PermissionEffect {
    match (left, right) {
        (PermissionEffect::Deny, _) | (_, PermissionEffect::Deny) => PermissionEffect::Deny,
        (PermissionEffect::Ask, _) | (_, PermissionEffect::Ask) => PermissionEffect::Ask,
        (PermissionEffect::Allow, PermissionEffect::Allow) => PermissionEffect::Allow,
    }
}

fn matched_decision(kind: PermissionLayerKind, rule: &PermissionRule) -> PermissionDecision {
    PermissionDecision {
        effect: rule.effect,
        layer: kind,
        rule_id: Some(rule.id.clone()),
        reason: format!(
            "matched {} permission rule {}",
            format!("{kind:?}").to_lowercase(),
            rule.id
        ),
    }
}

fn strongest_matching_rule<'a>(
    rules: &'a [PermissionRule],
    resource: &PermissionResource,
    effect: Option<PermissionEffect>,
    include: &impl Fn(&PermissionRule) -> bool,
) -> Option<&'a PermissionRule> {
    rules
        .iter()
        .enumerate()
        .filter(|(_, rule)| include(rule) && effect.is_none_or(|effect| rule.effect == effect))
        .filter(|(_, rule)| rule_matches(rule, resource))
        .max_by_key(|(index, rule)| (specificity(rule), *index))
        .map(|(_, rule)| rule)
}

fn specificity(rule: &PermissionRule) -> (usize, usize) {
    let patterns = [
        rule.tool.as_deref(),
        rule.server.as_deref(),
        rule.operation.as_deref(),
        rule.path.as_deref(),
        rule.raw_command.as_deref(),
        rule.cwd.as_deref(),
        rule.access.as_deref(),
        rule.mode.as_deref(),
        rule.agent.as_deref(),
    ];
    let constrained = patterns.iter().flatten().count();
    let command_constrained = usize::from(rule.command.is_some());
    let literal_characters = patterns
        .iter()
        .flatten()
        .flat_map(|pattern| pattern.chars())
        .filter(|character| !matches!(character, '*' | '?'))
        .count();
    let command_literals = rule
        .command
        .iter()
        .flatten()
        .flat_map(|pattern| pattern.chars())
        .filter(|character| !matches!(character, '*' | '?'))
        .count();
    (
        constrained + command_constrained,
        literal_characters + command_literals,
    )
}

fn rule_matches(rule: &PermissionRule, resource: &PermissionResource) -> bool {
    optional_match(rule.tool.as_deref(), Some(&resource.tool), false)
        && optional_match(rule.server.as_deref(), resource.server.as_deref(), false)
        && optional_match(
            rule.operation.as_deref(),
            resource.operation.as_deref(),
            false,
        )
        && optional_match(rule.path.as_deref(), resource.path.as_deref(), true)
        && optional_match(
            rule.access.as_deref(),
            resource.access.map(PermissionAccess::as_str),
            false,
        )
        && optional_match(rule.mode.as_deref(), Some(&resource.mode), false)
        && optional_match(rule.agent.as_deref(), Some(&resource.agent), false)
        && rule
            .command
            .as_ref()
            .is_none_or(|patterns| command_matches(patterns, &resource.command))
        && optional_match(
            rule.raw_command.as_deref(),
            resource.raw_command.as_deref(),
            false,
        )
        && optional_match(rule.cwd.as_deref(), resource.cwd.as_deref(), true)
}

fn command_matches(patterns: &[String], words: &[String]) -> bool {
    if patterns.is_empty() {
        return false;
    }
    command_matches_from(patterns, words, 0, 0)
}

fn command_matches_from(
    patterns: &[String],
    words: &[String],
    pattern_index: usize,
    word_index: usize,
) -> bool {
    let Some(pattern) = patterns.get(pattern_index) else {
        return word_index == words.len();
    };
    if pattern == "*" {
        return command_matches_from(patterns, words, pattern_index + 1, word_index)
            || (word_index < words.len()
                && command_matches_from(patterns, words, pattern_index, word_index + 1));
    }
    words
        .get(word_index)
        .is_some_and(|word| glob_matches(pattern, word, false))
        && command_matches_from(patterns, words, pattern_index + 1, word_index + 1)
}

fn optional_match(pattern: Option<&str>, value: Option<&str>, path: bool) -> bool {
    match (pattern, value) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(pattern), Some(value)) => glob_matches(pattern, value, path),
    }
}

/// Matches the permission glob contract: `*` does not cross a path separator,
/// `**` does, and `?` matches one scalar value.
fn glob_matches(raw_pattern: &str, value: &str, path: bool) -> bool {
    const LITERAL_STAR: char = '\u{e000}';
    const LITERAL_QUESTION: char = '\u{e001}';
    const LITERAL_BACKSLASH: char = '\u{e002}';
    if path
        && let Some(directory_pattern) = raw_pattern.strip_suffix("/**")
        && glob_matches(directory_pattern, value, true)
    {
        return true;
    }
    let mut pattern = Vec::new();
    let mut characters = raw_pattern.chars();
    while let Some(character) = characters.next() {
        if character == '\\' {
            match characters.next() {
                Some('*') => pattern.push(LITERAL_STAR),
                Some('?') => pattern.push(LITERAL_QUESTION),
                Some('\\') => pattern.push(LITERAL_BACKSLASH),
                Some(other) => {
                    pattern.push('\\');
                    pattern.push(other);
                }
                None => pattern.push('\\'),
            }
        } else {
            pattern.push(character);
        }
    }
    let value = value.chars().collect::<Vec<_>>();
    let mut memo = vec![vec![None; value.len() + 1]; pattern.len() + 1];
    glob_matches_from(&pattern, &value, path, 0, 0, &mut memo)
}

fn glob_matches_from(
    pattern: &[char],
    value: &[char],
    path: bool,
    pattern_index: usize,
    value_index: usize,
    memo: &mut [Vec<Option<bool>>],
) -> bool {
    if let Some(result) = memo[pattern_index][value_index] {
        return result;
    }
    let result = match pattern.get(pattern_index) {
        None => value_index == value.len(),
        Some('*') if pattern.get(pattern_index + 1) == Some(&'*') => {
            let next = pattern_index
                + pattern[pattern_index..]
                    .iter()
                    .take_while(|character| **character == '*')
                    .count();
            glob_matches_from(pattern, value, path, next, value_index, memo)
                || (path
                    && pattern.get(next) == Some(&'/')
                    && glob_matches_from(pattern, value, path, next + 1, value_index, memo))
                || (value_index < value.len()
                    && glob_matches_from(
                        pattern,
                        value,
                        path,
                        pattern_index,
                        value_index + 1,
                        memo,
                    ))
        }
        Some('*') => {
            glob_matches_from(pattern, value, path, pattern_index + 1, value_index, memo)
                || (value_index < value.len()
                    && (!path || value[value_index] != '/')
                    && glob_matches_from(
                        pattern,
                        value,
                        path,
                        pattern_index,
                        value_index + 1,
                        memo,
                    ))
        }
        Some('\u{e000}') => {
            value.get(value_index) == Some(&'*')
                && glob_matches_from(
                    pattern,
                    value,
                    path,
                    pattern_index + 1,
                    value_index + 1,
                    memo,
                )
        }
        Some('\u{e001}') => {
            value.get(value_index) == Some(&'?')
                && glob_matches_from(
                    pattern,
                    value,
                    path,
                    pattern_index + 1,
                    value_index + 1,
                    memo,
                )
        }
        Some('\u{e002}') => {
            value.get(value_index) == Some(&'\\')
                && glob_matches_from(
                    pattern,
                    value,
                    path,
                    pattern_index + 1,
                    value_index + 1,
                    memo,
                )
        }
        Some('?') => {
            value_index < value.len()
                && (!path || value[value_index] != '/')
                && glob_matches_from(
                    pattern,
                    value,
                    path,
                    pattern_index + 1,
                    value_index + 1,
                    memo,
                )
        }
        Some(character) => {
            value.get(value_index) == Some(character)
                && glob_matches_from(
                    pattern,
                    value,
                    path,
                    pattern_index + 1,
                    value_index + 1,
                    memo,
                )
        }
    };
    memo[pattern_index][value_index] = Some(result);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(id: &str, effect: PermissionEffect) -> PermissionRule {
        PermissionRule {
            id: id.into(),
            effect,
            tool: None,
            server: None,
            operation: None,
            path: None,
            command: None,
            raw_command: None,
            cwd: None,
            access: None,
            external: false,
            mode: None,
            agent: None,
            source: None,
            created_at: None,
        }
    }

    fn attachment(path: &str) -> PermissionResource {
        PermissionResource {
            tool: "read".into(),
            server: None,
            operation: None,
            path: Some(path.into()),
            access: Some(PermissionAccess::Read),
            mode: "ask".into(),
            agent: "general".into(),
            command: Vec::new(),
            raw_command: None,
            cwd: None,
        }
    }

    #[test]
    fn path_globs_observe_separator_and_scalar_semantics() {
        assert!(glob_matches("/work/**/main.?s", "/work/src/main.rs", true));
        assert!(glob_matches("/work/**/main.rs", "/work/main.rs", true));
        assert!(!glob_matches("/work/*/main.rs", "/work/a/b/main.rs", true));
        assert!(glob_matches("gpt-*", "gpt-5.6-sol", false));
        assert!(glob_matches("/work/project/**", "/work/project", true));
        assert!(glob_matches(
            "/work/project/**",
            "/work/project/src/main.rs",
            true
        ));
    }

    #[test]
    fn glob_escaping_preserves_literal_url_wildcards() {
        assert!(glob_matches(
            "https://example.test/a\\*b\\?c",
            "https://example.test/a*b?c",
            false
        ));
        assert!(!glob_matches(
            "https://example.test/a\\*b\\?c",
            "https://example.test/axbyc",
            false
        ));
        assert!(glob_matches("one\\\\two", "one\\two", false));
    }

    #[test]
    fn editable_bash_rule_uses_its_command_pattern() {
        let mut rule = rule("cargo-test", PermissionEffect::Allow);
        rule.command = Some(vec!["cargo".into(), "test".into()]);

        assert_eq!(
            rule.editable_pattern(),
            Some(("cargo test".into(), "command pattern"))
        );

        rule.set_editable_pattern("cargo test *".into());
        assert_eq!(
            rule.command,
            Some(vec!["cargo".into(), "test".into(), "*".into()])
        );
    }

    #[test]
    fn resources_and_decisions_round_trip_for_frontend_boundaries() {
        let resource = attachment("/work/src/main.rs");
        let encoded = serde_json::to_string(&resource).unwrap();
        assert_eq!(
            serde_json::from_str::<PermissionResource>(&encoded).unwrap(),
            resource
        );

        let decision = PermissionPolicy::default().evaluate(&resource, PermissionEffect::Allow);
        let encoded = serde_json::to_string(&decision).unwrap();
        assert_eq!(
            serde_json::from_str::<PermissionDecision>(&encoded).unwrap(),
            decision
        );
    }

    #[test]
    fn mcp_rules_match_original_server_and_operation_identity() {
        let mut exact = rule("exact-mcp", PermissionEffect::Allow);
        exact.tool = Some("mcp".into());
        exact.server = Some("github".into());
        exact.operation = Some("issues.list".into());
        let policy = PermissionPolicy {
            project: vec![exact],
            ..PermissionPolicy::default()
        };
        let mut resource = PermissionResource {
            tool: "mcp".into(),
            server: Some("github".into()),
            operation: Some("issues.list".into()),
            path: None,
            access: Some(PermissionAccess::Execute),
            mode: "ask".into(),
            agent: "general".into(),
            command: Vec::new(),
            raw_command: None,
            cwd: None,
        };
        assert_eq!(
            policy.evaluate(&resource, PermissionEffect::Ask).effect,
            PermissionEffect::Allow
        );
        resource.operation = Some("issues.delete".into());
        assert_eq!(
            policy.evaluate(&resource, PermissionEffect::Ask).effect,
            PermissionEffect::Ask
        );
        resource.server = None;
        resource.operation = Some("issues.list".into());
        assert_eq!(
            policy.evaluate(&resource, PermissionEffect::Ask).effect,
            PermissionEffect::Ask
        );
    }

    #[test]
    fn project_layer_precedes_allow_or_ask_from_less_specific_layers() {
        let mut project = rule("project", PermissionEffect::Ask);
        project.path = Some("/work/**".into());
        let mut global = rule("global", PermissionEffect::Allow);
        global.tool = Some("read".into());
        let policy = PermissionPolicy {
            project: vec![project],
            global: vec![global],
            ..PermissionPolicy::default()
        };

        let decision = policy.evaluate(&attachment("/work/src/main.rs"), PermissionEffect::Allow);
        assert_eq!(decision.effect, PermissionEffect::Ask);
        assert_eq!(decision.layer, PermissionLayerKind::Project);
        assert_eq!(decision.rule_id.as_deref(), Some("project"));
    }

    #[test]
    fn explicit_deny_is_terminal_across_layers() {
        let mut project = rule("project-allow", PermissionEffect::Allow);
        project.path = Some("/work/**".into());
        let mut global = rule("global-deny-env", PermissionEffect::Deny);
        global.path = Some("**/.env*".into());
        let policy = PermissionPolicy {
            project: vec![project],
            global: vec![global],
            ..PermissionPolicy::default()
        };

        let decision = policy.evaluate(&attachment("/work/.env.local"), PermissionEffect::Ask);
        assert_eq!(decision.effect, PermissionEffect::Deny);
        assert_eq!(decision.rule_id.as_deref(), Some("global-deny-env"));
    }

    #[test]
    fn specificity_then_later_file_order_selects_within_a_layer() {
        let mut broad = rule("broad", PermissionEffect::Ask);
        broad.tool = Some("read".into());
        let mut specific_first = rule("specific-first", PermissionEffect::Ask);
        specific_first.tool = Some("read".into());
        specific_first.path = Some("/work/src/**".into());
        let mut specific_later = specific_first.clone();
        specific_later.id = "specific-later".into();
        specific_later.effect = PermissionEffect::Allow;
        let policy = PermissionPolicy {
            global: vec![broad, specific_first, specific_later],
            ..PermissionPolicy::default()
        };

        let decision = policy.evaluate(&attachment("/work/src/lib.rs"), PermissionEffect::Ask);
        assert_eq!(decision.effect, PermissionEffect::Allow);
        assert_eq!(decision.rule_id.as_deref(), Some("specific-later"));
    }

    #[test]
    fn constrained_rule_does_not_match_a_resource_missing_that_dimension() {
        let mut rule = rule("path", PermissionEffect::Allow);
        rule.path = Some("/work/**".into());
        let policy = PermissionPolicy {
            project: vec![rule],
            ..PermissionPolicy::default()
        };
        let resource = PermissionResource {
            tool: "bash".into(),
            server: None,
            operation: None,
            path: None,
            access: None,
            mode: "ask".into(),
            agent: "general".into(),
            command: Vec::new(),
            raw_command: None,
            cwd: None,
        };

        let decision = policy.evaluate(&resource, PermissionEffect::Ask);
        assert_eq!(decision.layer, PermissionLayerKind::Default);
        assert_eq!(decision.effect, PermissionEffect::Ask);
    }

    #[test]
    fn bash_rules_match_parsed_words_and_cwd_independently() {
        let mut allow = rule("cargo-test", PermissionEffect::Allow);
        allow.tool = Some("bash".into());
        allow.command = Some(vec!["cargo".into(), "test".into(), "*".into()]);
        allow.cwd = Some("/work/**".into());
        let policy = PermissionPolicy {
            project: vec![allow],
            ..PermissionPolicy::default()
        };
        let resource = PermissionResource {
            tool: "bash".into(),
            server: None,
            operation: None,
            path: None,
            access: Some(PermissionAccess::Execute),
            mode: "ask".into(),
            agent: "general".into(),
            command: vec!["cargo".into(), "test".into(), "--all".into()],
            raw_command: Some("cargo test --all".into()),
            cwd: Some("/work/project".into()),
        };
        assert_eq!(
            policy.evaluate(&resource, PermissionEffect::Ask).effect,
            PermissionEffect::Allow
        );
        let mut push = resource;
        push.command = vec!["git".into(), "push".into()];
        assert_eq!(
            policy.evaluate(&push, PermissionEffect::Ask).effect,
            PermissionEffect::Ask
        );
    }

    #[test]
    fn bash_command_patterns_are_exact_unless_they_include_a_wildcard_word() {
        let mut exact = rule("cargo-test", PermissionEffect::Allow);
        exact.tool = Some("bash".into());
        exact.command = Some(vec!["cargo".into(), "test".into()]);
        let mut wildcard = exact.clone();
        wildcard.id = "cargo-test-with-arguments".into();
        wildcard.command = Some(vec!["cargo".into(), "test".into(), "*".into()]);

        let resource = PermissionResource {
            tool: "bash".into(),
            server: None,
            operation: None,
            path: None,
            access: Some(PermissionAccess::Execute),
            mode: "ask".into(),
            agent: "general".into(),
            command: vec!["cargo".into(), "test".into(), "--all".into()],
            raw_command: Some("cargo test --all".into()),
            cwd: Some("/work".into()),
        };

        assert_eq!(
            PermissionPolicy {
                project: vec![exact],
                ..PermissionPolicy::default()
            }
            .evaluate(&resource, PermissionEffect::Ask)
            .effect,
            PermissionEffect::Ask
        );
        assert_eq!(
            PermissionPolicy {
                project: vec![wildcard],
                ..PermissionPolicy::default()
            }
            .evaluate(&resource, PermissionEffect::Ask)
            .effect,
            PermissionEffect::Allow
        );
    }

    #[test]
    fn outside_attachment_requires_an_external_read_authorization() {
        let mut external_allow = rule("external-read", PermissionEffect::Allow);
        external_allow.tool = Some("read".into());
        external_allow.path = Some("/shared/**".into());
        external_allow.access = Some("read".into());
        external_allow.external = true;
        let policy = PermissionPolicy {
            project: vec![external_allow],
            ..PermissionPolicy::default()
        };

        let allowed = policy.evaluate_filesystem(
            &attachment("/shared/notes.md"),
            true,
            PermissionEffect::Allow,
        );
        assert_eq!(allowed.effect, PermissionEffect::Allow);
        assert_eq!(
            allowed
                .external
                .as_ref()
                .and_then(|value| value.rule_id.as_deref()),
            Some("external-read")
        );

        let unresolved = PermissionPolicy::default().evaluate_filesystem(
            &attachment("/shared/notes.md"),
            true,
            PermissionEffect::Allow,
        );
        assert_eq!(unresolved.operation.effect, PermissionEffect::Allow);
        assert_eq!(
            unresolved.external.as_ref().map(|value| value.effect),
            Some(PermissionEffect::Ask)
        );
        assert_eq!(unresolved.effect, PermissionEffect::Ask);
    }

    #[test]
    fn either_filesystem_decision_can_terminally_deny_the_request() {
        let mut read_deny = rule("read-deny", PermissionEffect::Deny);
        read_deny.tool = Some("read".into());
        read_deny.path = Some("**/.env*".into());
        let mut external_allow = rule("external-read", PermissionEffect::Allow);
        external_allow.tool = Some("read".into());
        external_allow.path = Some("/shared/**".into());
        external_allow.access = Some("read".into());
        external_allow.external = true;
        let policy = PermissionPolicy {
            global: vec![read_deny, external_allow],
            ..PermissionPolicy::default()
        };

        let decision =
            policy.evaluate_filesystem(&attachment("/shared/.env"), true, PermissionEffect::Allow);
        assert_eq!(decision.operation.effect, PermissionEffect::Deny);
        assert_eq!(decision.effect, PermissionEffect::Deny);
    }

    #[test]
    fn permission_file_loads_only_the_canonical_project_and_global_rules() {
        let temp = tempfile::TempDir::new().unwrap();
        let workspace = temp.path().join("work");
        std::fs::create_dir(&workspace).unwrap();
        let path = temp.path().join("permissions.toml");
        let workspace_key = permission_path(&workspace.canonicalize().unwrap());
        std::fs::write(
            &path,
            format!(
                "version = 1\n[[global.rule]]\nid = 'g'\neffect = 'deny'\npath = '**/.env*'\n\n[[project.\"{workspace_key}\".rule]]\nid = 'p'\neffect = 'allow'\ntool = 'apply_patch'\n"
            ),
        )
        .unwrap();
        let policy = PermissionFile::new(path, &workspace)
            .unwrap()
            .load()
            .unwrap();
        assert_eq!(policy.global[0].id, "g");
        assert_eq!(policy.project[0].id, "p");
    }

    #[test]
    fn project_trust_uses_the_canonical_project_section_and_preserves_rules() {
        let temp = tempfile::TempDir::new().unwrap();
        let workspace = temp.path().join("work");
        std::fs::create_dir(&workspace).unwrap();
        let path = temp.path().join("permissions.toml");
        let workspace_key = permission_path(&workspace.canonicalize().unwrap());
        std::fs::write(
            &path,
            format!(
                "# keep me\nversion = 1\n\n[[project.\"{workspace_key}\".rule]]\nid = 'existing'\neffect = 'allow'\ntool = 'read'\n\n[[project.\"/tmp/other\".rule]]\nid = 'other'\neffect = 'deny'\n"
            ),
        )
        .unwrap();
        let file = PermissionFile::new(path.clone(), &workspace).unwrap();

        assert!(!file.is_trusted().unwrap());
        file.set_trusted(true).unwrap();
        assert!(file.is_trusted().unwrap());
        assert_eq!(file.load().unwrap().project[0].id, "existing");

        let source = std::fs::read_to_string(path).unwrap();
        assert!(source.contains("# keep me"));
        assert!(source.contains("trusted = true"));
        assert!(source.contains("id = 'existing'"));
        assert!(source.contains("id = 'other'"));

        file.set_trusted(false).unwrap();
        assert!(!file.is_trusted().unwrap());
    }

    #[test]
    fn persistent_rules_are_atomic_stable_and_preserve_comments() {
        let temp = tempfile::TempDir::new().unwrap();
        let workspace = temp.path().join("work");
        std::fs::create_dir(&workspace).unwrap();
        let path = temp.path().join("permissions.toml");
        std::fs::write(&path, "# keep me\nversion = 1\n").unwrap();
        let file = PermissionFile::new(path.clone(), &workspace).unwrap();
        let mut value = rule("", PermissionEffect::Allow);
        value.tool = Some("apply_patch".into());
        value.path = Some(format!(
            "{}/**",
            permission_path(&workspace.canonicalize().unwrap())
        ));
        let stored = file.persist_rule(PermissionScope::Project, value).unwrap();
        let source = std::fs::read_to_string(path).unwrap();
        assert!(source.contains("# keep me"));
        assert!(source.contains(&stored.id));
        assert_eq!(file.load().unwrap().project[0].id, stored.id);

        let mut global = rule("", PermissionEffect::Deny);
        global.path = Some("**/.env*".into());
        let global = file.persist_rule(PermissionScope::Global, global).unwrap();
        let loaded = file.load().unwrap();
        assert_eq!(loaded.project[0].id, stored.id);
        assert_eq!(loaded.global[0].id, global.id);
    }

    #[test]
    fn persistent_rules_can_be_updated_and_deleted_with_scope_isolation() {
        let temp = tempfile::TempDir::new().unwrap();
        let workspace = temp.path().join("work");
        std::fs::create_dir(&workspace).unwrap();
        let path = temp.path().join("permissions.toml");
        std::fs::write(&path, "# keep me\nversion = 1\n").unwrap();
        let file = PermissionFile::new(path.clone(), &workspace).unwrap();
        let mut project = rule("project", PermissionEffect::Allow);
        project.path = Some("old/**".into());
        project.source = Some("user".into());
        project.created_at = Some("original".into());
        file.persist_rule(PermissionScope::Project, project)
            .unwrap();
        let mut global = rule("global", PermissionEffect::Deny);
        global.path = Some("**/.env".into());
        file.persist_rule(PermissionScope::Global, global).unwrap();

        let mut edited = file.load().unwrap().project[0].clone();
        edited.id = "replacement".into();
        edited.source = Some("changed".into());
        edited.created_at = Some("changed".into());
        edited.path = Some("new/**".into());
        let edited = file
            .update_rule(PermissionScope::Project, "project", edited)
            .unwrap();
        assert_eq!(edited.id, "project");
        assert_eq!(edited.source.as_deref(), Some("user"));
        assert_eq!(edited.created_at.as_deref(), Some("original"));
        assert_eq!(
            file.load().unwrap().project[0].path.as_deref(),
            Some("new/**")
        );
        assert_eq!(file.load().unwrap().global[0].id, "global");

        file.delete_rule(PermissionScope::Project, "project")
            .unwrap();
        let loaded = file.load().unwrap();
        assert!(loaded.project.is_empty());
        assert_eq!(loaded.global[0].id, "global");
        assert!(std::fs::read_to_string(path).unwrap().contains("# keep me"));
        assert!(
            file.delete_rule(PermissionScope::Global, "missing")
                .is_err()
        );
    }
}
