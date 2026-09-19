use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{ModelSelection, ModelSelectionConfig};
use crate::RuntimeError;

const PROMPT_SEPARATOR: &str = "\n\n## Additional agent instructions\n\n";

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptMerge {
    #[default]
    Append,
    Replace,
}

/// Controls where an agent profile may be selected.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentAvailability {
    /// May be selected by the user with `/agent`, but not delegated to.
    #[default]
    User,
    /// May only be selected for a delegated sub-agent run.
    Subagent,
    /// May be selected in either context.
    Both,
}

impl AgentAvailability {
    #[must_use]
    pub const fn user_selectable(self) -> bool {
        matches!(self, Self::User | Self::Both)
    }

    #[must_use]
    pub const fn subagent_selectable(self) -> bool {
        matches!(self, Self::Subagent | Self::Both)
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ListPolicy {
    pub allow: Option<BTreeSet<String>>,
    pub deny: Option<BTreeSet<String>>,
}

impl ListPolicy {
    fn is_empty(&self) -> bool {
        self.allow.is_none() && self.deny.is_none()
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct McpPolicy {
    pub allow: Option<BTreeSet<String>>,
    pub deny: Option<BTreeSet<String>>,
    /// Definitions are resolved by the MCP configuration service, not the agent picker catalog.
    #[serde(default, rename = "servers")]
    pub servers: BTreeMap<String, crate::McpServerDefinition>,
}

impl McpPolicy {
    fn is_empty(&self) -> bool {
        self.allow.is_none() && self.deny.is_none() && self.servers.is_empty()
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentModeOverrideDraft {
    pub model: Option<ModelSelectionConfig>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentProfileDraft {
    pub extends: Option<String>,
    pub description: Option<String>,
    pub prompt: Option<String>,
    /// `None` leaves the built-in default in effect. This distinction is important
    /// when an editor round-trips an inherited profile.
    pub prompt_merge: Option<PromptMerge>,
    pub model: Option<ModelSelectionConfig>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub modes: BTreeMap<String, AgentModeOverrideDraft>,
    pub mode: Option<String>,
    pub availability: Option<AgentAvailability>,
    /// Disabled profiles remain configured but cannot be selected or delegated to.
    pub enabled: Option<bool>,
    /// Inline permission rules are resolved by the permission service. Keeping
    /// their raw TOML values here lets profile editors round-trip them without
    /// interpreting rule syntax.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<toml::Value>,
    pub permissions_replace: Option<bool>,
    #[serde(default, skip_serializing_if = "ListPolicy::is_empty")]
    pub tools: ListPolicy,
    #[serde(default, skip_serializing_if = "McpPolicy::is_empty")]
    pub mcp: McpPolicy,
}

impl AgentProfileDraft {
    fn with_builtin_defaults(mut self, defaults: Self) -> Self {
        self.extends = self.extends.or(defaults.extends);
        self.description = self.description.or(defaults.description);
        self.prompt = match (self.prompt, defaults.prompt) {
            (Some(prompt), Some(base))
                if self.prompt_merge.unwrap_or_default() == PromptMerge::Append =>
            {
                self.prompt_merge = Some(PromptMerge::Replace);
                Some(format!("{base}{PROMPT_SEPARATOR}{prompt}"))
            }
            (Some(prompt), _) => Some(prompt),
            (None, prompt) => prompt,
        };
        if self.model.is_none() {
            self.model = defaults.model;
        }
        for (mode, definition) in defaults.modes {
            self.modes.entry(mode).or_insert(definition);
        }
        self.mode = self.mode.or(defaults.mode);
        self.availability = self.availability.or(defaults.availability);
        self.enabled = self.enabled.or(defaults.enabled);
        self.tools.allow = self.tools.allow.or(defaults.tools.allow);
        self.tools.deny = self.tools.deny.or(defaults.tools.deny);
        self.mcp.allow = self.mcp.allow.or(defaults.mcp.allow);
        self.mcp.deny = self.mcp.deny.or(defaults.mcp.deny);
        self
    }
}

/// Model and effort overrides for one named mode of an agent profile.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentModeOverride {
    pub model: Option<ModelSelection>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentProfile {
    pub name: String,
    pub description: String,
    pub prompt: String,
    pub model: Option<ModelSelection>,
    pub mode_overrides: BTreeMap<String, AgentModeOverride>,
    pub mode: Option<String>,
    pub availability: AgentAvailability,
    pub enabled: bool,
    pub tools_allow: Option<BTreeSet<String>>,
    pub tools_deny: Option<BTreeSet<String>>,
    pub mcp_allow: Option<BTreeSet<String>>,
    pub mcp_deny: Option<BTreeSet<String>>,
}

impl AgentProfile {
    #[must_use]
    pub fn allows_tool(&self, tool: &str) -> bool {
        self.tools_allow
            .as_ref()
            .is_none_or(|allow| allow.contains(tool))
            && !self
                .tools_deny
                .as_ref()
                .is_some_and(|deny| deny.contains(tool))
    }

    #[must_use]
    pub fn allows_mcp(&self, server: &str) -> bool {
        self.mcp_allow
            .as_ref()
            .is_none_or(|allow| allow.contains(server))
            && !self
                .mcp_deny
                .as_ref()
                .is_some_and(|deny| deny.contains(server))
    }

    fn empty(name: &str) -> Self {
        Self {
            name: name.into(),
            description: String::new(),
            prompt: String::new(),
            model: None,
            mode_overrides: BTreeMap::new(),
            mode: None,
            availability: AgentAvailability::User,
            enabled: true,
            tools_allow: None,
            tools_deny: None,
            mcp_allow: None,
            mcp_deny: None,
        }
    }

    fn merge(mut self, name: &str, child: &AgentProfileDraft) -> Result<Self, String> {
        self.name = name.into();
        if let Some(description) = &child.description {
            self.description.clone_from(description);
        }
        if let Some(prompt) = &child.prompt {
            self.prompt = if child.prompt_merge.unwrap_or_default() == PromptMerge::Replace
                || self.prompt.is_empty()
            {
                prompt.clone()
            } else {
                format!("{}{PROMPT_SEPARATOR}{prompt}", self.prompt)
            };
        }
        if let Some(model) = &child.model {
            let selection = ModelSelection::try_from_config(model.clone(), true)?;
            self.model = Some(selection.merged_over(self.model.as_ref()));
        }
        for (mode, definition) in &child.modes {
            let entry = self.mode_overrides.entry(mode.clone()).or_default();
            if let Some(model) = &definition.model {
                let selection = ModelSelection::try_from_config(model.clone(), true)?;
                entry.model = Some(selection.merged_over(entry.model.as_ref()));
            }
        }
        if child.mode.is_some() {
            self.mode.clone_from(&child.mode);
        }
        if let Some(availability) = child.availability {
            self.availability = availability;
        }
        if let Some(enabled) = child.enabled {
            self.enabled = enabled;
        }
        merge_tools(&mut self, &child.tools);
        merge_mcp(&mut self, &child.mcp);
        Ok(self)
    }
}

fn merge_tools(profile: &mut AgentProfile, policy: &ListPolicy) {
    if let Some(allow) = &policy.allow {
        profile.tools_allow = Some(allow.clone());
    }
    if let Some(deny) = &policy.deny {
        profile.tools_deny = Some(deny.clone());
    }
}

fn merge_mcp(profile: &mut AgentProfile, policy: &McpPolicy) {
    if let Some(allow) = &policy.allow {
        profile.mcp_allow = Some(allow.clone());
    }
    if let Some(deny) = &policy.deny {
        profile.mcp_deny = Some(deny.clone());
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentCatalog {
    profiles: BTreeMap<String, AgentProfile>,
}

impl AgentCatalog {
    /// Resolves built-in and configured profiles with single-parent inheritance.
    ///
    /// # Errors
    /// Returns a configuration error for cycles, unknown parents, or invalid model references.
    pub fn from_document(document: &toml::Value) -> Result<Self, RuntimeError> {
        let mut definitions = builtin_definitions();
        if let Some(configured) = document.get("agents") {
            let configured: BTreeMap<String, AgentProfileDraft> =
                configured.clone().try_into().map_err(|error| {
                    RuntimeError::InvalidOption(format!("invalid agents configuration: {error}"))
                })?;
            for (name, definition) in configured {
                let definition = match definitions.remove(&name) {
                    Some(builtin) => definition.with_builtin_defaults(builtin),
                    None => definition,
                };
                definitions.insert(name, definition);
            }
        }
        let mut profiles = BTreeMap::new();
        let mut visiting = Vec::new();
        for name in definitions.keys() {
            resolve_profile(name, &definitions, &mut profiles, &mut visiting)
                .map_err(RuntimeError::InvalidOption)?;
        }
        Ok(Self { profiles })
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&AgentProfile> {
        self.profiles.get(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &AgentProfile)> {
        self.profiles.iter()
    }

    pub fn user_profiles(&self) -> impl Iterator<Item = (&String, &AgentProfile)> {
        self.profiles
            .iter()
            .filter(|(_, profile)| profile.enabled && profile.availability.user_selectable())
    }

    pub fn subagent_profiles(&self) -> impl Iterator<Item = (&String, &AgentProfile)> {
        self.profiles
            .iter()
            .filter(|(_, profile)| profile.enabled && profile.availability.subagent_selectable())
    }
}

fn resolve_profile(
    name: &str,
    definitions: &BTreeMap<String, AgentProfileDraft>,
    resolved: &mut BTreeMap<String, AgentProfile>,
    visiting: &mut Vec<String>,
) -> Result<AgentProfile, String> {
    if let Some(profile) = resolved.get(name) {
        return Ok(profile.clone());
    }
    if let Some(start) = visiting.iter().position(|entry| entry == name) {
        let mut cycle = visiting[start..].to_vec();
        cycle.push(name.into());
        return Err(format!("agent inheritance cycle: {}", cycle.join(" -> ")));
    }
    let definition = definitions
        .get(name)
        .ok_or_else(|| format!("unknown agent profile: {name}"))?;
    visiting.push(name.into());
    let base = match definition.extends.as_deref() {
        Some(parent) => resolve_profile(parent, definitions, resolved, visiting)?,
        None => AgentProfile::empty(name),
    };
    visiting.pop();
    let profile = base.merge(name, definition)?;
    resolved.insert(name.into(), profile.clone());
    Ok(profile)
}

fn builtin_definitions() -> BTreeMap<String, AgentProfileDraft> {
    BTreeMap::from([
        ("general".into(), AgentProfileDraft {
            description: Some("General coding agent".into()),
            prompt: Some(crate::prompts::GENERAL_AGENT_PROMPT.into()),
            availability: Some(AgentAvailability::Both),
            ..AgentProfileDraft::default()
        }),
        ("explore".into(), AgentProfileDraft {
            description: Some("Fast, lower-cost read-only explorer for narrow fact-finding".into()),
            prompt: Some("Explore only the delegated question. Cite paths and return a concise, evidence-backed result. This role is not intended to produce final audits, reviews, security assessments, or architecture analysis; gather evidence for the delegated question, but do not own the final audit, review, recommendation, security assessment, or architecture analysis. Use only the documented read-safe Bash forms and the installed search/listing tools recommended by Bash. Do not launch background work or attempt commands that need approval.".into()),
            model: Some(ModelSelectionConfig {
                tier: Some("small".into()),
                ..ModelSelectionConfig::default()
            }),
            availability: Some(AgentAvailability::Subagent),
            tools: ListPolicy {
                allow: Some(BTreeSet::from(["bash".into(), "web_fetch".into()])),
                deny: None,
            },
            ..AgentProfileDraft::default()
        }),
    ])
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ModeProfile {
    pub name: String,
    pub description: String,
    pub prompt: String,
    pub color: crate::StatusLineColor,
    pub model: Option<ModelSelection>,
    pub enabled: bool,
    pub cycleable: bool,
    pub read: ReadPolicy,
    pub write: WritePolicy,
    pub run: RunPolicy,
    pub auto_level: AutoLevel,
    pub plan: bool,
    pub order: usize,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoLevel {
    Medium,
    #[default]
    High,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadPolicy {
    Allow,
    Ask,
    Auto,
    Deny,
}

impl ReadPolicy {
    #[must_use]
    pub const fn fallback(self) -> crate::PermissionEffect {
        match self {
            Self::Allow | Self::Auto => crate::PermissionEffect::Allow,
            Self::Ask => crate::PermissionEffect::Ask,
            Self::Deny => crate::PermissionEffect::Deny,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunPolicy {
    Allow,
    Ask,
    Auto,
    Deny,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WritePolicy {
    Allow,
    Ask,
    Auto,
    Deny,
}

impl WritePolicy {
    #[must_use]
    pub const fn fallback(self) -> crate::PermissionEffect {
        match self {
            Self::Allow | Self::Auto => crate::PermissionEffect::Allow,
            Self::Ask => crate::PermissionEffect::Ask,
            Self::Deny => crate::PermissionEffect::Deny,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModeDefinition {
    description: Option<String>,
    prompt: Option<String>,
    color: Option<String>,
    model: Option<ModelSelectionConfig>,
    enabled: Option<bool>,
    cycleable: Option<bool>,
    read: Option<ReadPolicy>,
    write: Option<WritePolicy>,
    run: Option<RunPolicy>,
    auto_level: Option<AutoLevel>,
    plan: Option<bool>,
    #[allow(dead_code)]
    permissions: Option<Vec<toml::Value>>,
}

/// Resolves built-in modes and validated user overrides.
///
/// # Errors
/// Returns an error when a configured mode has an invalid model reference or shape.
pub fn resolve_modes(
    document: &toml::Value,
) -> Result<BTreeMap<String, ModeProfile>, RuntimeError> {
    let declared_order = document
        .get("modes")
        .and_then(toml::Value::as_table)
        .map(|table| table.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    let explicit_order = mode_order(document)?;
    resolve_modes_ordered(document, &declared_order, explicit_order.as_deref())
}

#[allow(clippy::too_many_lines)]
pub(crate) fn resolve_modes_ordered(
    document: &toml::Value,
    declared_order: &[String],
    explicit_order: Option<&[String]>,
) -> Result<BTreeMap<String, ModeProfile>, RuntimeError> {
    if let Some(configured) = document.get("modes").and_then(toml::Value::as_table)
        && let Some((name, _)) = configured
            .iter()
            .find(|(_, definition)| definition.get("edit").is_some())
    {
        return Err(RuntimeError::InvalidOption(format!(
            "modes.{name}.edit was removed; use write = \"allow\", \"ask\", \"auto\", or \"deny\" instead"
        )));
    }
    if let Some(configured) = document.get("modes").and_then(toml::Value::as_table)
        && let Some((name, _)) = configured
            .iter()
            .find(|(_, definition)| definition.get("plan_exit_modes").is_some())
    {
        return Err(RuntimeError::InvalidOption(format!(
            "modes.{name}.plan_exit_modes was removed; use plan = true instead"
        )));
    }
    let mut definitions: BTreeMap<String, ModeDefinition> = BTreeMap::new();
    if let Some(configured) = document.get("modes") {
        definitions = configured.clone().try_into().map_err(|error| {
            RuntimeError::InvalidOption(format!("invalid modes configuration: {error}"))
        })?;
    }
    let builtins = [
        (
            "read",
            "Ask before unresolved mutations",
            "",
            crate::StatusLineColor::LightMagenta,
            ReadPolicy::Allow,
            WritePolicy::Ask,
            RunPolicy::Ask,
            false,
            false,
        ),
        (
            "edit",
            "Allow workspace edits and ask before Bash",
            "",
            crate::StatusLineColor::LightGreen,
            ReadPolicy::Allow,
            WritePolicy::Allow,
            RunPolicy::Ask,
            false,
            true,
        ),
        (
            "auto",
            "Classify unresolved writes and commands",
            "",
            crate::StatusLineColor::LightYellow,
            ReadPolicy::Auto,
            WritePolicy::Auto,
            RunPolicy::Auto,
            false,
            true,
        ),
        (
            "plan",
            "Plan without modifying the workspace",
            crate::prompts::PLAN_MODE_PROMPT,
            crate::StatusLineColor::LightBlue,
            ReadPolicy::Allow,
            WritePolicy::Deny,
            RunPolicy::Ask,
            true,
            true,
        ),
    ];
    let mut modes = BTreeMap::new();
    for (order, (name, description, prompt, default_color, read, write, run, plan, cycleable)) in
        builtins.into_iter().enumerate()
    {
        let custom: ModeDefinition = definitions.remove(name).unwrap_or_default();
        modes.insert(
            name.into(),
            ModeProfile {
                name: name.into(),
                description: custom.description.unwrap_or_else(|| description.into()),
                prompt: custom.prompt.unwrap_or_else(|| prompt.into()),
                color: parse_mode_color(name, custom.color, default_color)?,
                model: custom
                    .model
                    .map(|model| ModelSelection::try_from_config(model, true))
                    .transpose()
                    .map_err(|error| RuntimeError::InvalidOption(error.to_string()))?,
                enabled: custom.enabled.unwrap_or(true),
                cycleable: custom.cycleable.unwrap_or(cycleable),
                read: custom.read.unwrap_or(read),
                write: custom.write.unwrap_or(write),
                run: custom.run.unwrap_or(run),
                auto_level: custom.auto_level.unwrap_or_default(),
                plan: custom.plan.unwrap_or(plan),
                order,
            },
        );
    }
    let mut custom_names = declared_order
        .iter()
        .filter(|name| definitions.contains_key(*name))
        .cloned()
        .collect::<Vec<_>>();
    let remaining = definitions
        .keys()
        .filter(|name| !custom_names.contains(name))
        .cloned()
        .collect::<Vec<_>>();
    custom_names.extend(remaining);
    for name in custom_names {
        let custom = definitions.remove(&name).expect("custom mode exists");
        let order = modes.len();
        let color = parse_mode_color(name.as_str(), custom.color, custom_mode_color(order))?;
        modes.insert(
            name.clone(),
            ModeProfile {
                name,
                description: custom.description.unwrap_or_default(),
                prompt: custom.prompt.unwrap_or_default(),
                color,
                model: custom
                    .model
                    .map(|model| ModelSelection::try_from_config(model, true))
                    .transpose()
                    .map_err(|error| RuntimeError::InvalidOption(error.to_string()))?,
                enabled: custom.enabled.unwrap_or(true),
                cycleable: custom.cycleable.unwrap_or(true),
                read: custom.read.unwrap_or(ReadPolicy::Allow),
                write: custom.write.unwrap_or(WritePolicy::Ask),
                run: custom.run.unwrap_or(RunPolicy::Ask),
                auto_level: custom.auto_level.unwrap_or_default(),
                plan: custom.plan.unwrap_or(false),
                order,
            },
        );
    }
    apply_mode_order(&mut modes, explicit_order)?;
    validate_modes(&modes)?;
    Ok(modes)
}

pub(crate) fn mode_order(document: &toml::Value) -> Result<Option<Vec<String>>, RuntimeError> {
    let Some(order) = document
        .get("mode")
        .and_then(toml::Value::as_table)
        .and_then(|mode| mode.get("order"))
    else {
        return Ok(None);
    };
    let order = order.as_array().ok_or_else(|| {
        RuntimeError::InvalidOption("mode.order must be an array of mode names".into())
    })?;
    order
        .iter()
        .map(|value| {
            value.as_str().map(str::to_owned).ok_or_else(|| {
                RuntimeError::InvalidOption("mode.order must contain only strings".into())
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

fn apply_mode_order(
    modes: &mut BTreeMap<String, ModeProfile>,
    explicit_order: Option<&[String]>,
) -> Result<(), RuntimeError> {
    let Some(explicit_order) = explicit_order else {
        return Ok(());
    };
    let mut seen = BTreeSet::new();
    for name in explicit_order {
        if !seen.insert(name) {
            return Err(RuntimeError::InvalidOption(format!(
                "mode.order contains duplicate mode: {name}"
            )));
        }
        if !modes.contains_key(name) {
            return Err(RuntimeError::InvalidOption(format!(
                "mode.order names an unknown mode: {name}"
            )));
        }
    }
    let mut names = explicit_order.to_vec();
    let mut remaining = modes.values().collect::<Vec<_>>();
    remaining.sort_by_key(|mode| mode.order);
    names.extend(
        remaining
            .into_iter()
            .map(|mode| mode.name.clone())
            .filter(|name| !seen.contains(name)),
    );
    for (order, name) in names.into_iter().enumerate() {
        modes
            .get_mut(&name)
            .expect("validated mode order names existing mode")
            .order = order;
    }
    Ok(())
}

fn parse_mode_color(
    name: &str,
    color: Option<String>,
    default: crate::StatusLineColor,
) -> Result<crate::StatusLineColor, RuntimeError> {
    color
        .map(|color| {
            color.parse().map_err(|error: String| {
                RuntimeError::InvalidOption(format!("modes.{name}.color: {error}"))
            })
        })
        .transpose()
        .map(|color| color.unwrap_or(default))
}

fn custom_mode_color(order: usize) -> crate::StatusLineColor {
    const COLORS: [crate::StatusLineColor; 8] = [
        crate::StatusLineColor::Cyan,
        crate::StatusLineColor::LightCyan,
        crate::StatusLineColor::Yellow,
        crate::StatusLineColor::LightRed,
        crate::StatusLineColor::Blue,
        crate::StatusLineColor::LightGreen,
        crate::StatusLineColor::Magenta,
        crate::StatusLineColor::LightYellow,
    ];
    COLORS[order % COLORS.len()]
}

fn validate_modes(modes: &BTreeMap<String, ModeProfile>) -> Result<(), RuntimeError> {
    if !modes.values().any(|mode| mode.enabled) {
        return Err(RuntimeError::InvalidOption(
            "at least one mode must be enabled".into(),
        ));
    }
    if modes.values().any(|mode| mode.enabled && mode.plan)
        && !modes
            .values()
            .any(|mode| mode.enabled && mode.cycleable && !mode.plan)
    {
        return Err(RuntimeError::InvalidOption(
            "an enabled planning mode requires an enabled, cycleable non-planning mode".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_colors_use_defaults_and_accept_named_or_hex_overrides() {
        let document = toml::from_str(
            r##"
            [modes.read]
            color = "#123456"
            [modes.review]
            color = "light-cyan"
            "##,
        )
        .unwrap();
        let modes = resolve_modes(&document).unwrap();
        assert_eq!(modes["read"].color, crate::StatusLineColor::Rgb(18, 52, 86));
        assert_eq!(modes["edit"].color, crate::StatusLineColor::LightGreen);
        assert_eq!(modes["plan"].color, crate::StatusLineColor::LightBlue);
        assert_eq!(modes["review"].color, crate::StatusLineColor::LightCyan);
    }

    #[test]
    fn invalid_mode_color_is_rejected() {
        let document = toml::from_str(
            r#"
            [modes.review]
            color = "not-a-color"
            "#,
        )
        .unwrap();
        assert!(
            resolve_modes(&document)
                .unwrap_err()
                .to_string()
                .contains("modes.review.color")
        );
    }

    #[test]
    fn explicit_mode_order_precedes_the_default_order() {
        let document = toml::from_str(
            r#"
            [mode]
            order = ["plan", "review"]

            [modes.review]
            description = "Review changes"
            "#,
        )
        .unwrap();

        let modes = resolve_modes(&document).unwrap();
        assert_eq!(modes["plan"].order, 0);
        assert_eq!(modes["review"].order, 1);
        assert!(modes["read"].order > modes["review"].order);
    }

    #[test]
    fn explicit_mode_order_rejects_duplicates_and_unknown_modes() {
        for (order, expected_error) in [
            ("[\"read\", \"read\"]", "duplicate mode: read"),
            ("[\"unknown\"]", "unknown mode: unknown"),
        ] {
            let document = format!("[mode]\norder = {order}")
                .parse::<toml::Value>()
                .unwrap();
            assert!(
                resolve_modes(&document)
                    .unwrap_err()
                    .to_string()
                    .contains(expected_error)
            );
        }
    }

    #[test]
    fn planning_mode_uses_its_configured_permission_defaults() {
        let document = toml::from_str(
            r#"
            [modes.design]
            plan = true
            write = "auto"
            run = "deny"
            "#,
        )
        .unwrap();

        let modes = resolve_modes(&document).unwrap();
        let design = &modes["design"];
        assert!(design.plan);
        assert_eq!(design.read, ReadPolicy::Allow);
        assert_eq!(design.write, WritePolicy::Auto);
        assert_eq!(design.run, RunPolicy::Deny);
        assert_eq!(design.auto_level, AutoLevel::High);
    }

    #[test]
    fn auto_level_defaults_to_high_and_accepts_both_values() {
        let document = r#"
            [modes.default_level]
            run = "auto"
            [modes.medium_level]
            run = "auto"
            auto_level = "medium"
            [modes.high_level]
            run = "auto"
            auto_level = "high"
        "#
        .parse::<toml::Value>()
        .unwrap();
        let modes = resolve_modes(&document).unwrap();

        assert_eq!(modes["default_level"].auto_level, AutoLevel::High);
        assert_eq!(modes["medium_level"].auto_level, AutoLevel::Medium);
        assert_eq!(modes["high_level"].auto_level, AutoLevel::High);
        assert_eq!(
            serde_json::to_value(&modes["medium_level"]).unwrap()["auto_level"],
            "medium"
        );
    }

    #[test]
    fn invalid_auto_level_is_rejected() {
        let document = r#"
            [modes.auto]
            auto_level = "critical"
        "#
        .parse::<toml::Value>()
        .unwrap();

        assert!(
            resolve_modes(&document)
                .unwrap_err()
                .to_string()
                .contains("invalid modes configuration")
        );
    }

    #[test]
    fn write_policy_parses_and_serializes_all_values() {
        let document = r#"
            [modes.allow]
            write = "allow"
            [modes.ask]
            write = "ask"
            [modes.auto_write]
            write = "auto"
            [modes.deny]
            write = "deny"
        "#
        .parse::<toml::Value>()
        .unwrap();
        let modes = resolve_modes(&document).unwrap();
        for (name, expected, serialized) in [
            ("allow", WritePolicy::Allow, "allow"),
            ("ask", WritePolicy::Ask, "ask"),
            ("auto_write", WritePolicy::Auto, "auto"),
            ("deny", WritePolicy::Deny, "deny"),
        ] {
            assert_eq!(modes[name].write, expected);
            assert_eq!(
                serde_json::to_value(&modes[name]).unwrap()["write"],
                serialized
            );
        }
    }

    #[test]
    fn read_policy_parses_and_serializes_all_values() {
        let document = r#"
            [modes.allow]
            read = "allow"
            [modes.ask]
            read = "ask"
            [modes.auto_read]
            read = "auto"
            [modes.deny]
            read = "deny"
        "#
        .parse::<toml::Value>()
        .unwrap();
        let modes = resolve_modes(&document).unwrap();
        for (name, expected, serialized) in [
            ("allow", ReadPolicy::Allow, "allow"),
            ("ask", ReadPolicy::Ask, "ask"),
            ("auto_read", ReadPolicy::Auto, "auto"),
            ("deny", ReadPolicy::Deny, "deny"),
        ] {
            assert_eq!(modes[name].read, expected);
            assert_eq!(
                serde_json::to_value(&modes[name]).unwrap()["read"],
                serialized
            );
        }
    }

    #[test]
    fn built_in_plan_mode_uses_the_shared_prompt() {
        let modes = resolve_modes(&toml::Value::Table(toml::Table::new())).unwrap();
        assert_eq!(modes["plan"].prompt, crate::prompts::PLAN_MODE_PROMPT);
        assert_eq!(modes["read"].read, ReadPolicy::Allow);
        assert_eq!(modes["edit"].read, ReadPolicy::Allow);
        assert_eq!(modes["auto"].read, ReadPolicy::Auto);
        assert_eq!(modes["plan"].read, ReadPolicy::Allow);
        assert_eq!(modes["edit"].write, WritePolicy::Allow);
        assert_eq!(modes["auto"].write, WritePolicy::Auto);
        assert_eq!(modes["plan"].write, WritePolicy::Deny);
    }

    #[test]
    fn builtins_have_expected_read_only_explore_contract() {
        let catalog = AgentCatalog::from_document(&toml::Value::Table(toml::Table::new())).unwrap();
        let explore = catalog.get("explore").unwrap();
        assert_eq!(
            explore.description,
            "Fast, lower-cost read-only explorer for narrow fact-finding"
        );
        assert!(explore
            .prompt
            .contains("not intended to produce final audits, reviews, security assessments, or architecture analysis"));
        assert_eq!(
            explore
                .model
                .as_ref()
                .and_then(|selection| match selection.target.as_ref() {
                    Some(crate::ModelTarget::Tier(tier)) => Some(tier.as_str()),
                    _ => None,
                }),
            Some("small")
        );
        assert_eq!(
            explore.tools_allow.as_ref(),
            Some(&BTreeSet::from(["bash".into(), "web_fetch".into()]))
        );
        assert_eq!(explore.tools_deny, None);
        assert_eq!(explore.mcp_allow, None);
        assert_eq!(explore.mcp_deny, None);
        assert!(explore.allows_tool("bash"));
        assert!(!explore.allows_tool("read"));
        assert!(explore.allows_mcp("docs"));
        let general = catalog.get("general").unwrap();
        assert_eq!(general.prompt, crate::prompts::GENERAL_AGENT_PROMPT);
        assert!(general.allows_tool("bash"));
        assert!(general.allows_mcp("docs"));
        assert_eq!(explore.availability, AgentAvailability::Subagent);
        assert!(explore.enabled);
        assert_eq!(
            catalog
                .user_profiles()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            vec!["general"]
        );
    }

    #[test]
    fn disabled_profiles_are_inherited_and_excluded_from_selection() {
        let document: toml::Value = toml::from_str(
            r#"
            [agents.explore]
            enabled = false

            [agents.child]
            extends = "explore"
            availability = "both"
            "#,
        )
        .unwrap();

        let catalog = AgentCatalog::from_document(&document).unwrap();
        assert!(!catalog.get("explore").unwrap().enabled);
        assert!(!catalog.get("child").unwrap().enabled);
        assert_eq!(
            catalog
                .subagent_profiles()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            vec!["general"]
        );
    }

    #[test]
    fn inheritance_appends_prompts_and_replaces_configured_policies() {
        let document: toml::Value = toml::from_str(
            r#"
            [agents.base]
            prompt = "base"
            [agents.base.tools]
            allow = ["read", "apply_patch"]

            [agents.child]
            extends = "base"
            prompt = "child"
            model = { tier = "small" }
            availability = "both"
            [agents.child.tools]
            deny = ["apply_patch"]
        "#,
        )
        .unwrap();
        let catalog = AgentCatalog::from_document(&document).unwrap();
        let child = catalog.get("child").unwrap();
        assert_eq!(child.prompt, format!("base{PROMPT_SEPARATOR}child"));
        assert_eq!(
            child.tools_allow,
            Some(BTreeSet::from(["apply_patch".into(), "read".into()]))
        );
        assert_eq!(
            child.tools_deny,
            Some(BTreeSet::from(["apply_patch".into()]))
        );
        assert_eq!(child.availability, AgentAvailability::Both);
    }

    #[test]
    fn replacement_and_cycles_are_validated() {
        let replacement: toml::Value = toml::from_str(
            r#"
            [agents.base]
            prompt = "base"
            [agents.child]
            extends = "base"
            prompt = "child"
            prompt_merge = "replace"
        "#,
        )
        .unwrap();
        assert_eq!(
            AgentCatalog::from_document(&replacement)
                .unwrap()
                .get("child")
                .unwrap()
                .prompt,
            "child"
        );
        let cycle: toml::Value =
            toml::from_str("[agents.a]\nextends='b'\n[agents.b]\nextends='a'").unwrap();
        assert!(
            AgentCatalog::from_document(&cycle)
                .unwrap_err()
                .to_string()
                .contains("cycle")
        );

        let unsupported: toml::Value =
            toml::from_str("[agents.quick]\nmodel_tier='turbo'").unwrap();
        assert!(
            AgentCatalog::from_document(&unsupported)
                .unwrap_err()
                .to_string()
                .contains("model_tier")
        );
    }

    #[test]
    fn exact_agent_model_override_replaces_the_builtin_fast_tier() {
        let document = r#"
            [agents.explore]
            model = { model = "openai/gpt-specialized" }
        "#
        .parse::<toml::Value>()
        .unwrap();

        let catalog = AgentCatalog::from_document(&document).unwrap();
        let explore = catalog.get("explore").unwrap();

        assert_eq!(
            explore.model.as_ref().map(ToString::to_string).as_deref(),
            Some("openai/gpt-specialized")
        );
        assert_eq!(explore.availability, AgentAvailability::Subagent);
        assert_eq!(
            explore.tools_allow,
            Some(BTreeSet::from(["bash".into(), "web_fetch".into()]))
        );
    }

    #[test]
    fn mode_overrides_inherit_per_field() {
        let document: toml::Value = toml::from_str(
            r#"
            [agents.base.modes.review]
            model = { model = "openai/reviewer", effort = "high" }

            [agents.child]
            extends = "base"
            [agents.child.modes.plan]
            model = { model = "openai/planner", effort = "xhigh" }
            [agents.child.modes.review]
            model = { effort = "low" }
        "#,
        )
        .unwrap();
        let child = AgentCatalog::from_document(&document)
            .unwrap()
            .get("child")
            .unwrap()
            .clone();
        assert_eq!(
            child.mode_overrides["review"]
                .model
                .as_ref()
                .map(ToString::to_string),
            Some("openai/reviewer".into())
        );
        assert_eq!(
            child.mode_overrides["review"]
                .model
                .as_ref()
                .and_then(|selection| selection.effort.as_deref()),
            Some("low")
        );
        assert_eq!(
            child.mode_overrides["plan"]
                .model
                .as_ref()
                .map(ToString::to_string),
            Some("openai/planner".into())
        );
        assert_eq!(
            child.mode_overrides["plan"]
                .model
                .as_ref()
                .and_then(|selection| selection.effort.as_deref()),
            Some("xhigh")
        );
    }

    #[test]
    fn removed_profile_fields_are_rejected() {
        let document: toml::Value = toml::from_str(
            r#"
            [agents.bad]
            plan_model = "openai/legacy"
        "#,
        )
        .unwrap();
        assert!(AgentCatalog::from_document(&document).is_err());
    }

    #[test]
    fn inheritance_applies_scalar_list_and_mcp_merge_rules() {
        let document: toml::Value = toml::from_str(
            r#"
            [agents.base]
            description = "base description"
            prompt = "base prompt"
            model = { model = "mock/base", effort = "medium" }
            mode = "auto"
            availability = "both"
            [agents.base.modes.plan]
            model = { model = "mock/base-plan", effort = "high" }
            [agents.base.tools]
            allow = ["read", "grep"]
            deny = ["bash"]
            [agents.base.mcp]
            allow = ["docs", "issues"]

            [agents.child]
            extends = "base"
            prompt = "replacement"
            prompt_merge = "replace"
            model = { tier = "small", effort = "low" }
            mode = "plan"
            availability = "subagent"
            [agents.child.tools]
            allow = ["list", "bash"]
            deny = ["bash"]
            [agents.child.mcp]
            deny = ["issues"]
            "#,
        )
        .unwrap();
        let child = AgentCatalog::from_document(&document)
            .unwrap()
            .get("child")
            .unwrap()
            .clone();
        assert_eq!(child.description, "base description");
        assert_eq!(child.prompt, "replacement");
        assert_eq!(
            child
                .model
                .as_ref()
                .and_then(|selection| selection.effort.as_deref()),
            Some("low")
        );
        assert_eq!(
            child.mode_overrides["plan"]
                .model
                .as_ref()
                .unwrap()
                .to_string(),
            "mock/base-plan"
        );
        assert_eq!(
            child.mode_overrides["plan"]
                .model
                .as_ref()
                .and_then(|selection| selection.effort.as_deref()),
            Some("high")
        );
        assert_eq!(child.mode.as_deref(), Some("plan"));
        assert_eq!(child.availability, AgentAvailability::Subagent);
        assert_eq!(
            child.tools_allow,
            Some(BTreeSet::from(["bash".into(), "list".into()]))
        );
        assert_eq!(child.tools_deny, Some(BTreeSet::from(["bash".into()])));
        assert_eq!(
            child.mcp_allow,
            Some(BTreeSet::from(["docs".into(), "issues".into()]))
        );
        assert_eq!(child.mcp_deny, Some(BTreeSet::from(["issues".into()])));
        assert!(child.allows_mcp("docs"));
        assert!(!child.allows_mcp("issues"));
        assert!(!child.allows_mcp("other"));
    }

    #[test]
    fn modes_resolve_enabled_policies_and_custom_declaration_order() {
        let document = r#"
            [modes.edit]
            enabled = false

            [modes.review]
            write = "deny"
            run = "allow"

            [modes.focus]
            write = "allow"
            run = "deny"
        "#
        .parse::<toml::Value>()
        .unwrap();
        let order = vec!["edit".into(), "review".into(), "focus".into()];
        let modes = resolve_modes_ordered(&document, &order, None).unwrap();

        assert!(!modes["edit"].enabled);
        assert!(!modes["read"].cycleable);
        assert!(modes["review"].cycleable);
        assert_eq!(modes["review"].write, WritePolicy::Deny);
        assert_eq!(modes["review"].run, RunPolicy::Allow);
        assert!(modes["review"].order < modes["focus"].order);
    }

    #[test]
    fn mode_cycleability_can_be_overridden() {
        let document = r#"
            [modes.read]
            cycleable = true

            [modes.review]
            cycleable = false
        "#
        .parse::<toml::Value>()
        .unwrap();

        let modes = resolve_modes(&document).unwrap();
        assert!(modes["read"].cycleable);
        assert!(!modes["review"].cycleable);
    }

    #[test]
    fn planning_modes_require_an_enabled_non_planning_mode() {
        let document = r"
            [modes.read]
            enabled = false
            [modes.edit]
            enabled = false
            [modes.auto]
            enabled = false
        "
        .parse::<toml::Value>()
        .unwrap();

        assert!(
            resolve_modes_ordered(&document, &["edit".into(), "auto".into()], None)
                .unwrap_err()
                .to_string()
                .contains("requires an enabled, cycleable non-planning mode")
        );
    }

    #[test]
    fn legacy_plan_exit_modes_are_rejected() {
        let document = r#"
            [modes.plan]
            plan_exit_modes = ["edit"]
        "#
        .parse::<toml::Value>()
        .unwrap();

        assert!(
            resolve_modes(&document)
                .unwrap_err()
                .to_string()
                .contains("use plan = true instead")
        );
    }

    #[test]
    fn removed_edit_policy_is_rejected_with_write_guidance() {
        let document = r#"
            [modes.review]
            edit = "deny"
        "#
        .parse::<toml::Value>()
        .unwrap();

        let error = resolve_modes(&document).unwrap_err().to_string();
        assert!(error.contains("modes.review.edit was removed"));
        assert!(error.contains("write ="));
    }
}
