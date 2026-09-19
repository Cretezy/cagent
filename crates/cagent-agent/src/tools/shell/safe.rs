#![allow(
    clippy::obfuscated_if_else,
    clippy::needless_lifetimes,
    clippy::only_used_in_recursion
)] // Recursive shell parsing preserves its shared path context and explicit grammar lifetimes.
//! Registry-backed parsing and availability checks for automatically read-safe Bash forms.

use super::{BashBackendInfo, BashBackendKind, ShellOperator, analyze_shell, shell_path};
use brush_parser::ast;
use brush_parser::word::{Parameter, ParameterExpr, WordPiece};
use brush_parser::{Parser, ParserOptions, SourceInfo};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[path = "classifiers/mod.rs"]
mod classifiers;
use classifiers::find_spec;
pub use classifiers::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SafeShellPresentation {
    Read,
    List,
    Search,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SafePresentationValue {
    pub value: String,
    pub path: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SafePresentationRelation {
    pub label: String,
    pub values: Vec<SafePresentationValue>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SafePresentationDetail {
    pub label: String,
    pub arguments: Vec<SafePresentationValue>,
    pub relations: Vec<SafePresentationRelation>,
}

impl SafePresentationDetail {
    #[must_use]
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            arguments: Vec::new(),
            relations: Vec::new(),
        }
    }

    #[must_use]
    pub fn arguments(mut self, values: impl IntoIterator<Item = String>, path: bool) -> Self {
        self.arguments.extend(
            values
                .into_iter()
                .map(|value| SafePresentationValue { value, path }),
        );
        self
    }

    #[must_use]
    pub fn relation(
        mut self,
        label: impl Into<String>,
        values: impl IntoIterator<Item = String>,
        path: bool,
    ) -> Self {
        let values = values
            .into_iter()
            .map(|value| SafePresentationValue { value, path })
            .collect();
        self.relations.push(SafePresentationRelation {
            label: label.into(),
            values,
        });
        self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SafeCommandCategory {
    Generic,
    Read,
    List,
    Search,
}

/// The authorization tier of a registered shell command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShellSafetyTier {
    /// Statically read-only commands, which may be shown as Explore activity.
    Level0,
    Level1,
    Level2,
    Level3,
}

impl ShellSafetyTier {
    pub const fn level(self) -> i8 {
        match self {
            Self::Level0 => 0,
            Self::Level1 => 1,
            Self::Level2 => 2,
            Self::Level3 => 3,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SafeCommandPlatform {
    All,
    Gnu,
}

impl SafeCommandPlatform {
    fn supported(self) -> bool {
        // The selected Bash backend, not the host process, is the availability
        // authority. GNU-only entries remain gated by backend resolution.
        true
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SafeCommandHardening {
    None,
    Git,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SafeCommandResolutionKind {
    BashBuiltin,
    ExternalExecutable,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SafeCommandAvailability {
    pub canonical_command: String,
    pub invocation_name: String,
    pub resolution_kind: SafeCommandResolutionKind,
    pub resolved_executable: Option<String>,
    pub backend: BashBackendKind,
    pub hardening_available: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ShellCommandInventory {
    pub backend: BashBackendInfo,
    pub commands: BTreeMap<String, SafeCommandAvailability>,
    pub probe_succeeded: bool,
}

impl ShellCommandInventory {
    #[must_use]
    pub fn empty(backend: BashBackendInfo) -> Self {
        Self {
            backend,
            commands: BTreeMap::new(),
            probe_succeeded: false,
        }
    }

    #[must_use]
    pub fn synthetic(names: &[&str]) -> Self {
        let backend = BashBackendInfo {
            kind: BashBackendKind::Native,
            executable: PathBuf::from("/bin/bash"),
            host_cwd: PathBuf::from("."),
            shell_cwd: ".".into(),
        };
        let mut inventory = Self {
            backend,
            commands: BTreeMap::new(),
            probe_succeeded: true,
        };
        for name in names {
            let Some(spec) = find_spec(name) else {
                continue;
            };
            let canonical = spec.canonical.to_owned();
            inventory.commands.insert(
                canonical.clone(),
                SafeCommandAvailability {
                    canonical_command: canonical,
                    invocation_name: (*name).to_owned(),
                    resolution_kind: if spec.builtin {
                        SafeCommandResolutionKind::BashBuiltin
                    } else {
                        SafeCommandResolutionKind::ExternalExecutable
                    },
                    resolved_executable: (!spec.builtin).then(|| format!("/usr/bin/{name}")),
                    backend: BashBackendKind::Native,
                    hardening_available: spec.hardening != SafeCommandHardening::Git
                        || *name == "git",
                },
            );
        }
        inventory.select_fd_alias();
        inventory
    }

    pub(crate) fn select_fd_alias(&mut self) {
        let Some(fd) = self.commands.get("fd").cloned() else {
            return;
        };
        if fd.invocation_name == "fd" {
            return;
        }
        if let Some(preferred) = self
            .commands
            .values()
            .find(|entry| entry.canonical_command == "fd" && entry.invocation_name == "fd")
            .cloned()
        {
            self.commands.insert("fd".into(), preferred);
        }
    }

    #[must_use]
    pub fn availability_for_invocation(
        &self,
        invocation: &str,
    ) -> Option<&SafeCommandAvailability> {
        let spec = find_spec(invocation)?;
        self.commands
            .get(spec.canonical)
            .filter(|availability| availability.invocation_name == invocation)
    }

    #[must_use]
    pub fn available(&self, canonical: &str) -> Option<&SafeCommandAvailability> {
        self.commands.get(canonical)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SafePathOperand {
    pub value: String,
    pub source: &'static str,
    pub base_directory: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SafeCommandParse {
    pub operands: Vec<SafePathOperand>,
    pub presentation: Option<SafeShellPresentation>,
    pub targets: Vec<String>,
    pub transparent: bool,
    pub query: Option<String>,
    pub detail: Option<SafePresentationDetail>,
    pub directory_transition: Option<String>,
    pub hardening: SafeCommandHardening,
    pub path_list_output: Option<SafePathListOutput>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SafePathListOutput {
    Lines,
}

impl SafeCommandParse {
    fn new(presentation: Option<SafeShellPresentation>) -> Self {
        Self {
            operands: Vec::new(),
            presentation,
            targets: Vec::new(),
            transparent: false,
            query: None,
            detail: None,
            directory_transition: None,
            hardening: SafeCommandHardening::None,
            path_list_output: None,
        }
    }

    fn path(mut self, value: impl Into<String>, source: &'static str) -> Self {
        let value = value.into();
        if value != "-" {
            self.operands.push(SafePathOperand {
                value,
                source,
                base_directory: PathBuf::from("."),
            });
        }
        self
    }
}

pub type SafeCommandParser = fn(&[String]) -> Option<SafeCommandParse>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SafeShellClassification {
    pub paths: Vec<String>,
    pub presentation: Option<SafeShellPresentation>,
    pub exploration: Option<Vec<SafeShellActivity>>,
    pub operands: Vec<SafePathOperand>,
    pub requires_git_hardening: bool,
    pub hardened_command: Option<String>,
    /// Nested commands whose output was included in the proof for an outer
    /// command substitution. They do not need a second permission decision.
    pub covered_nested_segments: Vec<String>,
    /// Present only when this standalone command emits a plain path list that
    /// may safely supply filesystem operands to another registered command.
    pub path_list_output: Option<SafePathListOutput>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SafeShellActivity {
    pub presentation: SafeShellPresentation,
    pub targets: Vec<String>,
    pub paths: Vec<String>,
    pub query: Option<String>,
    pub command: String,
    pub detail: Option<SafePresentationDetail>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SafeShellAssessment {
    pub read_safe: bool,
    pub paths: Vec<String>,
    pub presentation: Option<SafeShellPresentation>,
    pub exploration: Option<Vec<SafeShellActivity>>,
    pub fallback_reason: Option<String>,
}

/// Environment and filesystem context used by the runtime safe classifier.
///
/// The context-free [`classify_read_safe_shell`] API intentionally remains
/// conservative. Runtime callers provide the actual Bash working directory
/// and the child environment so that path expansion can be checked against
/// the same filesystem and environment that will execute the request.
#[derive(Clone, Debug)]
pub struct SafeShellContext {
    pub effective_directory: PathBuf,
    pub variables: BTreeMap<String, String>,
    pub backend: BashBackendInfo,
    pub max_glob_matches: usize,
}

impl SafeShellContext {
    #[must_use]
    pub fn new(
        effective_directory: PathBuf,
        backend: BashBackendInfo,
        variables: BTreeMap<String, String>,
    ) -> Self {
        Self {
            effective_directory,
            variables: variables
                .into_iter()
                .filter(|(name, _)| is_approved_path_variable(name))
                .collect(),
            backend,
            max_glob_matches: MAX_GLOB_MATCHES,
        }
    }

    /// Builds a context from the environment inherited by the Cagent
    /// process, overlaying only request-provided approved path variables.
    /// Unknown overrides and forwarded variables deliberately disable the
    /// automatic safe classification because they can change command intent.
    #[must_use]
    pub fn for_bash_request(
        request: &super::BashRequest,
        inventory: &ShellCommandInventory,
    ) -> Option<Self> {
        if request
            .env
            .keys()
            .chain(request.forward_env.iter())
            .any(|name| !is_approved_path_variable(name))
        {
            return None;
        }
        let effective_directory = request.cwd.as_deref().map_or_else(
            || inventory.backend.host_cwd.clone(),
            |cwd| {
                if cwd.is_absolute() {
                    cwd.to_path_buf()
                } else {
                    inventory.backend.host_cwd.join(cwd)
                }
            },
        );
        let mut variables = BTreeMap::new();
        for name in APPROVED_PATH_VARIABLES {
            if let Some(value) = std::env::var_os(name).and_then(|value| value.into_string().ok()) {
                variables.insert((*name).into(), value);
            }
        }
        for (name, value) in &request.env {
            variables.insert(name.clone(), value.clone());
        }
        if !request.env.contains_key("PWD") {
            variables.insert(
                "PWD".into(),
                shell_path(inventory.backend.kind, &effective_directory),
            );
        }
        Some(Self::new(
            effective_directory,
            inventory.backend.clone(),
            variables,
        ))
    }

    fn variable(&self, name: &str) -> Option<String> {
        self.variables.get(name).cloned()
    }

    fn actual_cwd(&self, logical_cwd: &Path) -> PathBuf {
        if logical_cwd.is_absolute() {
            logical_cwd.to_path_buf()
        } else {
            self.effective_directory.join(logical_cwd)
        }
    }
}

const APPROVED_PATH_VARIABLES: &[&str] = &[
    "HOME",
    "PWD",
    "OLDPWD",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "TMPDIR",
    "TMP",
    "TEMP",
    "USERPROFILE",
];
const MAX_GLOB_MATCHES: usize = 1024;
const MAX_PATH_LIST_OUTPUT_BYTES: usize = 1024 * 1024;
const PATH_LIST_PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(5);

#[must_use]
pub fn safe_path_variable_names() -> &'static [&'static str] {
    APPROVED_PATH_VARIABLES
}

fn is_approved_path_variable(name: &str) -> bool {
    APPROVED_PATH_VARIABLES.contains(&name)
}

#[must_use]
pub fn safe_shell_command_names() -> Vec<&'static str> {
    SAFE_COMMAND_REGISTRY
        .iter()
        .filter(|spec| spec.platform.supported() && spec.tier == ShellSafetyTier::Level0)
        .map(|spec| spec.canonical)
        .collect()
}

#[must_use]
pub fn safe_shell_examples() -> Vec<&'static str> {
    SAFE_COMMAND_REGISTRY
        .iter()
        .filter(|spec| spec.platform.supported() && spec.tier == ShellSafetyTier::Level0)
        .flat_map(|spec| spec.examples.iter().copied())
        .collect()
}

#[must_use]
pub fn safe_bash_guidance(inventory: &ShellCommandInventory, safe_level: i8) -> String {
    if safe_level < 0 {
        return String::new();
    }
    let names = inventory
        .commands
        .values()
        .filter(|availability| {
            availability.hardening_available
                && find_spec(&availability.canonical_command)
                    .is_some_and(|spec| spec.tier.level() <= safe_level)
        })
        .map(|availability| availability.invocation_name.as_str())
        .collect::<Vec<_>>();
    let restrictions = SAFE_COMMAND_REGISTRY
        .iter()
        .filter(|spec| spec.tier.level() <= safe_level)
        .filter_map(|spec| {
            inventory
                .available(spec.canonical)
                .and_then(|availability| {
                    availability
                        .hardening_available
                        .then(|| format!("{}: {}", availability.invocation_name, spec.restrictions))
                })
        })
        .collect::<Vec<_>>();
    let mut recommendations = vec![
        "Shell quoting: single-quote static patterns and other literal arguments by default. Double quotes still allow command substitution via backticks or `$()` and expand `$variables`. Example: use rg -n 'Press `r`' file, not rg -n \"Press `r`\" file.".into(),
    ];
    if let Some(rg) = inventory.available("rg") {
        recommendations.push(if inventory.available("grep").is_some() {
            format!(
                "Prefer {} over grep for searching file contents.",
                rg.invocation_name
            )
        } else {
            format!("Prefer {} for searching file contents.", rg.invocation_name)
        });
        recommendations.push(
            "For an rg pattern beginning with '-', put a standalone -- before the pattern; quoting alone does not stop option parsing. Example: rg -n -i -- '--prompt|prompt' SPEC.md.".into(),
        );
    } else if inventory.available("grep").is_some() {
        recommendations.push("Use grep for searching file contents.".into());
    }
    if let Some(fd) = inventory.available("fd") {
        recommendations.push(if inventory.available("find").is_some() {
            format!(
                "Prefer {} over find for locating files.",
                fd.invocation_name
            )
        } else {
            format!("Prefer {} for locating files.", fd.invocation_name)
        });
    } else if inventory.available("find").is_some() {
        recommendations.push("Use find for locating files.".into());
    }
    let mut examples = Vec::new();
    for spec in SAFE_COMMAND_REGISTRY {
        if spec.tier != ShellSafetyTier::Level0 {
            continue;
        }
        if (spec.canonical == "grep" && inventory.available("rg").is_some())
            || (spec.canonical == "find" && inventory.available("fd").is_some())
        {
            continue;
        }
        let Some(availability) = inventory.available(spec.canonical) else {
            continue;
        };
        if !availability.hardening_available {
            continue;
        }
        for example in spec.examples {
            let example = if spec.canonical == "fd" && availability.invocation_name != "fd" {
                example.replacen("fd", &availability.invocation_name, 1)
            } else {
                (*example).to_owned()
            };
            if authorize_available_safe_shell(&example, inventory).is_some() {
                examples.push(example);
            }
        }
    }
    let names = if names.is_empty() {
        "none positively identified".into()
    } else {
        names.join(", ")
    };
    let restrictions = if restrictions.is_empty() {
        String::new()
    } else {
        format!(" Restrictions: {}.", restrictions.join("; "))
    };
    let recommendations = if recommendations.is_empty() {
        String::new()
    } else {
        format!(" {}", recommendations.join(" "))
    };
    let examples = if examples.is_empty() {
        String::new()
    } else {
        format!(" Examples: {}.", examples.join("; "))
    };
    format!(
        "Available proven read-safe commands in this Bash environment: {names}.{restrictions}{recommendations} Each independently proven registered segment joined by |, &&, ||, ;, or a background operator can qualify; a single top-level for loop with at most 64 explicit values, including bounded filesystem globs expanded at runtime, may also qualify when its simple body uses only double-quoted loop-variable expansions. Unsupported substitutions, assignments, subshells, functions, aliases, dynamic arguments, and unsupported control flow fall back to normal Bash permissions. Runtime path globs and simple comma-list brace expansion are allowed in every documented filesystem-operand or loop-value position. An entire unquoted filesystem operand may also come from a bounded plain path-list substitution using fd, rg --files, or find with plain printing; quoted, embedded, custom-formatted, and arbitrary command substitutions remain unsupported. A bare-home `~` prefix, the fixed path-variable subset, and deterministic printf/echo substitutions are allowed only in their documented literal-output positions. Every parsed filesystem operand and cwd is checked against the project workspace; outside paths and symlinks resolving outside require separate external approval. Safe background commands still run as tracked supervised terminals.{examples} This static guarantee covers parsed intent and standard utility semantics, not malicious replacement binaries, OS compromise, or TOCTOU replacement."
    )
}

#[must_use]
pub fn assess_read_safe_shell(source: &str) -> SafeShellAssessment {
    match parse_known_safe_shell(source) {
        Ok(classification) => SafeShellAssessment {
            read_safe: true,
            paths: classification.paths,
            presentation: classification.presentation,
            exploration: classification.exploration,
            fallback_reason: None,
        },
        Err(reason) => SafeShellAssessment {
            read_safe: false,
            paths: Vec::new(),
            presentation: None,
            exploration: None,
            fallback_reason: Some(reason),
        },
    }
}

#[must_use]
pub fn classify_read_safe_shell(source: &str) -> Option<SafeShellClassification> {
    parse_known_safe_shell(source).ok()
}

/// Classifies the presentable read-only intent of a shell expression.
///
/// Unlike authorization, presentation does not expand pathname patterns
/// against the current filesystem. Pattern syntax is accepted only in words
/// that a registered command parser identifies as filesystem operands.
#[must_use]
pub(crate) fn classify_shell_exploration(source: &str) -> Option<SafeShellClassification> {
    parse_safe_shell_for_presentation(source, None).ok()
}

#[must_use]
pub fn authorize_available_safe_shell(
    source: &str,
    inventory: &ShellCommandInventory,
) -> Option<SafeShellClassification> {
    if !inventory.probe_succeeded {
        return None;
    }
    let context = SafeShellContext::new(
        inventory.backend.host_cwd.clone(),
        inventory.backend.clone(),
        approved_process_environment(),
    );
    parse_available_safe_shell(source, inventory, &context).ok()
}

/// Classifies a Bash request whose execution environment cannot alter command
/// resolution. Waiting for completion does not affect parsed command intent,
/// so foreground and background requests use the same safe subset.
#[must_use]
pub fn authorize_available_safe_bash_request(
    request: &super::BashRequest,
    inventory: &ShellCommandInventory,
) -> Option<SafeShellClassification> {
    if !inventory.probe_succeeded {
        return None;
    }
    let context = SafeShellContext::for_bash_request(request, inventory)?;
    parse_available_safe_shell(&request.command, inventory, &context).ok()
}

/// The safe portions of a Bash request. A compound request may contain both
/// safe and unsafe segments; the runtime can authorize the safe segments as
/// reads while applying the normal Bash policy to the remainder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SafeBashAuthorization {
    pub whole: Option<SafeShellClassification>,
    pub segments: BTreeMap<usize, SafeShellClassification>,
    pub leveled_segments: BTreeMap<usize, ShellSafetyTier>,
    pub safe_write_segments: BTreeSet<usize>,
}

/// Fully validates a classifier evidence command against the deliberately
/// smaller read-only subset. Unlike ordinary Bash authorization, partial
/// compounds are rejected rather than returned for per-segment prompting.
#[must_use]
pub fn authorize_auto_review_evidence(
    request: &super::BashRequest,
    inventory: &ShellCommandInventory,
    configured_safe_level: i8,
) -> Option<SafeBashAuthorization> {
    if configured_safe_level < 0
        || !request.wait
        || !request.env.is_empty()
        || !request.forward_env.is_empty()
    {
        return None;
    }
    let analysis = super::analyze_shell(&request.command).ok()?;
    if analysis.opaque
        || analysis.has_assignment
        || analysis.operators.iter().any(|operator| {
            matches!(
                operator,
                ShellOperator::Background | ShellOperator::Subshell | ShellOperator::Substitution
            )
        })
        || analysis
            .paths
            .iter()
            .any(|path| path.dynamic || path.access != super::ShellPathAccess::Read)
    {
        return None;
    }
    let effective_level = configured_safe_level.min(1);
    let authorization = authorize_available_safe_bash_segments(
        request,
        inventory,
        &analysis,
        effective_level,
        false,
    )?;
    if authorization.whole.is_none()
        && analysis
            .segments
            .iter()
            .enumerate()
            .any(|(index, segment)| {
                segment.opaque
                    || (!authorization.segments.contains_key(&index)
                        && authorization
                            .tier(index)
                            .is_none_or(|tier| tier.level() > effective_level))
            })
    {
        return None;
    }
    Some(authorization)
}

impl SafeBashAuthorization {
    /// Returns whether the segment receives the built-in safe-command default.
    /// Whole-command evidence covers every segment; otherwise each segment is
    /// considered independently so safe filters in a mixed pipeline do not
    /// inherit an unresolved sibling's permission outcome.
    #[must_use]
    pub fn allows_segment(&self, index: usize, safe_level: i8, safe_write: bool) -> bool {
        self.whole.is_some()
            || self.segment(index).is_some()
            || self
                .tier(index)
                .is_some_and(|tier| tier.level() <= safe_level)
            || (safe_write && self.safe_write_segment(index))
    }

    #[must_use]
    pub fn segment(&self, index: usize) -> Option<&SafeShellClassification> {
        self.segments.get(&index)
    }

    #[must_use]
    pub fn tier(&self, index: usize) -> Option<ShellSafetyTier> {
        self.leveled_segments.get(&index).copied().or_else(|| {
            self.segments
                .contains_key(&index)
                .then_some(ShellSafetyTier::Level0)
        })
    }
    pub fn safe_write_segment(&self, index: usize) -> bool {
        self.safe_write_segments.contains(&index)
    }
}

/// Classifies independently safe segments in a Bash request.
///
/// The existing whole-request classification is retained for Git hardening
/// and for the fast path where the complete request is safe. When that fails,
/// direct compound-command segments and commands in a static literal loop are
/// classified independently.
#[must_use]
pub fn authorize_available_safe_bash_segments(
    request: &super::BashRequest,
    inventory: &ShellCommandInventory,
    analysis: &super::ShellAnalysis,
    safe_level: i8,
    safe_write: bool,
) -> Option<SafeBashAuthorization> {
    if !inventory.probe_succeeded || safe_level < 0 {
        return None;
    }
    let context = SafeShellContext::for_bash_request(request, inventory)?;
    let whole = parse_available_safe_shell(&request.command, inventory, &context).ok();
    if whole.is_some() {
        return Some(SafeBashAuthorization {
            whole,
            segments: BTreeMap::new(),
            leveled_segments: BTreeMap::new(),
            safe_write_segments: BTreeSet::new(),
        });
    }
    if analysis.has_assignment
        || analysis.operators.iter().any(|operator| {
            matches!(
                operator,
                ShellOperator::Subshell | ShellOperator::Substitution
            )
        })
    {
        return Some(SafeBashAuthorization {
            whole: None,
            segments: BTreeMap::new(),
            leveled_segments: BTreeMap::new(),
            safe_write_segments: BTreeSet::new(),
        });
    }

    let mut segments = BTreeMap::new();
    for (index, segment) in analysis.segments.iter().enumerate() {
        if segment.depth != 0 || is_literal_shell_wrapper(&segment.words) {
            continue;
        }
        let Ok(classification) = parse_available_safe_shell(&segment.raw, inventory, &context)
        else {
            continue;
        };
        segments.insert(index, classification.clone());
        for nested_raw in &classification.covered_nested_segments {
            for (nested_index, nested) in analysis.segments.iter().enumerate() {
                if nested.depth > 0 && nested.raw.trim() == nested_raw {
                    segments.insert(nested_index, classification.clone());
                }
            }
        }
    }
    classify_static_loop_segments(
        &request.command,
        inventory,
        &context,
        analysis,
        &mut segments,
    );

    let leveled_segments = (safe_level >= 1)
        .then(|| {
            analysis
                .segments
                .iter()
                .enumerate()
                .filter_map(|(index, segment)| {
                    (segment.depth == 0 && !segments.contains_key(&index))
                        .then(|| classifiers::classify_generally_safe_segment(segment))
                        .flatten()
                        .filter(|tier| tier.level() <= safe_level)
                        .map(|tier| (index, tier))
                })
                .collect()
        })
        .unwrap_or_default();
    let safe_write_segments = safe_write
        .then(|| {
            analysis
                .segments
                .iter()
                .enumerate()
                .filter_map(|(index, segment)| {
                    classify_safe_write_segment(segment, analysis).then_some(index)
                })
                .collect()
        })
        .unwrap_or_default();
    Some(SafeBashAuthorization {
        whole,
        segments,
        leveled_segments,
        safe_write_segments,
    })
}

fn classify_safe_write_segment(
    segment: &super::ShellSegment,
    analysis: &super::ShellAnalysis,
) -> bool {
    if segment.depth != 0
        || segment.opaque
        || segment.words.is_empty()
        || analysis.opaque
        || analysis.paths.iter().any(|path| path.dynamic)
    {
        return false;
    }
    let args = &segment.words[1..];
    match segment.words[0].as_str() {
        "cagent" => matches!(args, [config, set, key, _value]
            if config == "config" && set == "set" && !key.is_empty()),
        "echo" | "printf" => {
            analysis.has_redirection
                && analysis
                    .paths
                    .iter()
                    .filter(|p| p.access != super::ShellPathAccess::Read)
                    .count()
                    == 1
        }
        "touch" => !args.is_empty() && args.iter().all(|a| !a.starts_with('-')),
        "mkdir" => !args.is_empty() && args.iter().all(|a| a == "-p" || !a.starts_with('-')),
        "cp" | "mv" => args.len() == 2 && args.iter().all(|a| !a.starts_with('-')),
        "rm" | "rmdir" => !args.is_empty() && args.iter().all(|a| !a.starts_with('-')),
        _ => false,
    }
}

fn classify_static_loop_segments(
    source: &str,
    inventory: &ShellCommandInventory,
    context: &SafeShellContext,
    analysis: &super::ShellAnalysis,
    segments: &mut BTreeMap<usize, SafeShellClassification>,
) {
    let Ok(clauses) = parse_static_for_clauses(source) else {
        return;
    };
    for clause in clauses {
        let Ok(commands) = collect_static_loop_commands(&clause.body) else {
            continue;
        };
        let mut values = Vec::new();
        let mut valid_values = true;
        for word in &clause.values {
            match expand_static_loop_value(word, Some(context)) {
                Ok(expanded) => values.extend(expanded),
                Err(_) => {
                    valid_values = false;
                    break;
                }
            }
        }
        if !valid_values || values.is_empty() {
            continue;
        }
        if values.len() > MAX_STATIC_LOOP_VALUES {
            continue;
        }

        for command in commands {
            let raw = command.to_string().trim().to_owned();
            let mut paths = Vec::new();
            let mut safe_for_all_values = true;
            let mut has_directory_change = false;
            for value in &values {
                let Ok(rendered) = render_static_loop_command(command, &clause.variable, value)
                else {
                    safe_for_all_values = false;
                    break;
                };
                let Ok(classification) = parse_safe_shell(&rendered, Some(inventory)) else {
                    safe_for_all_values = false;
                    break;
                };
                if classification.requires_git_hardening {
                    safe_for_all_values = false;
                    break;
                }
                has_directory_change |= analyze_shell(&rendered)
                    .ok()
                    .and_then(|parsed| parsed.segments.first().cloned())
                    .and_then(|segment| segment.words.first().cloned())
                    .is_some_and(|invocation| invocation == "cd");
                paths.extend(classification.paths);
            }
            if !safe_for_all_values || has_directory_change {
                continue;
            }
            paths.sort();
            paths.dedup();
            let classification = SafeShellClassification {
                paths,
                presentation: None,
                exploration: None,
                operands: Vec::new(),
                requires_git_hardening: false,
                hardened_command: None,
                covered_nested_segments: Vec::new(),
                path_list_output: None,
            };
            for (index, segment) in analysis.segments.iter().enumerate() {
                if segment.depth == 0 && segment.raw.trim() == raw {
                    segments.insert(index, classification.clone());
                }
            }
        }
    }
}

fn parse_available_safe_shell(
    source: &str,
    inventory: &ShellCommandInventory,
    context: &SafeShellContext,
) -> Result<SafeShellClassification, String> {
    let needs_runtime_context = analyze_shell(source)
        .map(|analysis| {
            analysis.opaque
                || analysis
                    .operators
                    .iter()
                    .any(|operator| matches!(operator, ShellOperator::Background))
        })
        .unwrap_or(true);
    if needs_runtime_context {
        parse_safe_shell_with_context(source, Some(inventory), Some(context))
    } else {
        parse_safe_shell(source, Some(inventory))
    }
}

/// Returns true only for literal, available development commands with a
/// deliberately narrow grammar. This is separate from read-safe parsing: it
/// is an approval convenience, not an inspection proof.
fn approved_process_environment() -> BTreeMap<String, String> {
    APPROVED_PATH_VARIABLES
        .iter()
        .filter_map(|name| {
            std::env::var_os(name)
                .and_then(|value| value.into_string().ok())
                .map(|value| ((*name).into(), value))
        })
        .collect()
}

pub fn parse_known_safe_shell(source: &str) -> Result<SafeShellClassification, String> {
    parse_safe_shell(source, None)
}

const MAX_STATIC_LOOP_VALUES: usize = 64;

#[derive(Clone, Debug)]
struct StaticForClause {
    variable: String,
    values: Vec<ast::Word>,
    body: ast::CompoundList,
}

fn parse_static_for_clauses(source: &str) -> Result<Vec<StaticForClause>, String> {
    let options = ParserOptions::default();
    let info = SourceInfo {
        source: "cagent-safe-static-for-list".into(),
    };
    let mut parser = Parser::new(std::io::Cursor::new(source.as_bytes()), &options, &info);
    let program = parser
        .parse_program()
        .map_err(|error| format!("Bash parse failed: {error}"))?;
    let mut clauses = Vec::new();
    for list in &program.complete_commands {
        collect_static_for_clauses_from_list(list, &mut clauses)?;
    }
    Ok(clauses)
}

fn collect_static_for_clauses_from_list(
    list: &ast::CompoundList,
    clauses: &mut Vec<StaticForClause>,
) -> Result<(), String> {
    for item in &list.0 {
        collect_static_for_clauses_from_pipeline(&item.0.first, clauses)?;
        for additional in &item.0.additional {
            let pipeline = match additional {
                ast::AndOr::And(pipeline) | ast::AndOr::Or(pipeline) => pipeline,
            };
            collect_static_for_clauses_from_pipeline(pipeline, clauses)?;
        }
    }
    Ok(())
}

fn collect_static_for_clauses_from_pipeline(
    pipeline: &ast::Pipeline,
    clauses: &mut Vec<StaticForClause>,
) -> Result<(), String> {
    for command in &pipeline.seq {
        let ast::Command::Compound(ast::CompoundCommand::ForClause(clause), redirects) = command
        else {
            continue;
        };
        if redirects.is_some() {
            return Err("static loops cannot have outer redirections".into());
        }
        let Some(values) = &clause.values else {
            return Err("the loop must use an explicit value list".into());
        };
        if values.is_empty() {
            return Err("the loop value list cannot be empty".into());
        }
        if values.len() > MAX_STATIC_LOOP_VALUES {
            return Err(format!(
                "the loop value list exceeds the {MAX_STATIC_LOOP_VALUES}-value static limit"
            ));
        }
        clauses.push(StaticForClause {
            variable: clause.variable_name.clone(),
            values: values.clone(),
            body: clause.body.0.clone(),
        });
    }
    Ok(())
}

fn collect_static_loop_commands<'a>(
    body: &'a ast::CompoundList,
) -> Result<Vec<&'a ast::Command>, String> {
    let mut commands = Vec::new();
    for item in &body.0 {
        if matches!(item.1, ast::SeparatorOperator::Async) {
            return Err("background commands are not supported in loops".into());
        }
        collect_static_loop_pipeline(&item.0.first, &mut commands)?;
        for additional in &item.0.additional {
            let pipeline = match additional {
                ast::AndOr::And(pipeline) | ast::AndOr::Or(pipeline) => pipeline,
            };
            collect_static_loop_pipeline(pipeline, &mut commands)?;
        }
    }
    Ok(commands)
}

fn collect_static_loop_pipeline<'a>(
    pipeline: &'a ast::Pipeline,
    commands: &mut Vec<&'a ast::Command>,
) -> Result<(), String> {
    if pipeline.bang || pipeline.timed.is_some() {
        return Err("negated and timed pipelines are not supported in loops".into());
    }
    for command in &pipeline.seq {
        if !matches!(command, ast::Command::Simple(_)) {
            return Err("the loop body may contain only simple commands".into());
        }
        commands.push(command);
    }
    Ok(())
}

fn parse_static_for_clause(source: &str) -> Result<Option<StaticForClause>, String> {
    let options = ParserOptions::default();
    let info = SourceInfo {
        source: "cagent-safe-static-for".into(),
    };
    let mut parser = Parser::new(std::io::Cursor::new(source.as_bytes()), &options, &info);
    let program = parser
        .parse_program()
        .map_err(|error| format!("Bash parse failed: {error}"))?;
    let Some(list) = (program.complete_commands.len() == 1).then(|| &program.complete_commands[0])
    else {
        return Ok(None);
    };
    if list.0.len() != 1 || !list.0[0].0.additional.is_empty() {
        return Ok(None);
    }
    let pipeline = &list.0[0].0.first;
    if pipeline.bang || pipeline.timed.is_some() || pipeline.seq.len() != 1 {
        return Ok(None);
    }
    let ast::Command::Compound(ast::CompoundCommand::ForClause(clause), redirects) =
        &pipeline.seq[0]
    else {
        return Ok(None);
    };
    if redirects.is_some() {
        return Err("static loops cannot have outer redirections".into());
    }
    let Some(values) = &clause.values else {
        return Err("the loop must use an explicit value list".into());
    };
    if values.is_empty() {
        return Err("the loop value list cannot be empty".into());
    }
    if values.len() > MAX_STATIC_LOOP_VALUES {
        return Err(format!(
            "the loop value list exceeds the {MAX_STATIC_LOOP_VALUES}-value static limit"
        ));
    }
    Ok(Some(StaticForClause {
        variable: clause.variable_name.clone(),
        values: values.clone(),
        body: clause.body.0.clone(),
    }))
}

fn expand_static_loop_word(
    word: &ast::Word,
    variable: Option<&str>,
    value: Option<&str>,
) -> Result<String, String> {
    let pieces = brush_parser::word::parse(&word.value, &ParserOptions::default())
        .map_err(|error| error.to_string())?;
    expand_static_loop_pieces(&pieces, variable, value, false, false, false)
        .map(|(_, _, output)| output)
}

fn expand_static_loop_value(
    word: &ast::Word,
    context: Option<&SafeShellContext>,
) -> Result<Vec<String>, String> {
    let pieces = brush_parser::word::parse(&word.value, &ParserOptions::default())
        .map_err(|error| error.to_string())?;
    let (has_glob, has_brace, value) =
        expand_static_loop_pieces(&pieces, None, None, false, true, true)?;
    let brace_values = if has_brace {
        expand_brace_patterns(&value, MAX_STATIC_LOOP_VALUES)?
    } else {
        vec![value]
    };
    if !has_glob {
        return Ok(brace_values);
    }
    let context = context.ok_or("loop pathname globs require runtime context")?;
    let cwd = context.actual_cwd(Path::new("."));
    let mut expanded = Vec::new();
    for pattern in brace_values {
        expanded.extend(expand_path_glob(&pattern, &cwd, context.max_glob_matches)?);
        if expanded.len() > MAX_STATIC_LOOP_VALUES {
            return Err(format!(
                "the loop value list exceeds the {MAX_STATIC_LOOP_VALUES}-value static limit"
            ));
        }
    }
    Ok(expanded)
}

fn expand_static_loop_pieces(
    pieces: &[brush_parser::word::WordPieceWithSource],
    variable: Option<&str>,
    value: Option<&str>,
    in_double_quotes: bool,
    allow_globs: bool,
    allow_braces: bool,
) -> Result<(bool, bool, String), String> {
    let mut output = String::new();
    let mut has_glob = false;
    let mut has_brace = false;
    for piece in pieces {
        match &piece.piece {
            WordPiece::Text(text) => {
                if !in_double_quotes && text.chars().any(|character| character == '{') {
                    if !allow_braces {
                        return Err("brace expansion is not supported here".into());
                    }
                    has_brace = true;
                }
                if !in_double_quotes
                    && text
                        .chars()
                        .any(|character| matches!(character, '*' | '?' | '['))
                {
                    if !allow_globs {
                        return Err("globbing is not supported".into());
                    }
                    has_glob = true;
                }
                output.push_str(text);
            }
            WordPiece::SingleQuotedText(text)
            | WordPiece::AnsiCQuotedText(text)
            | WordPiece::EscapeSequence(text) => output.push_str(text),
            WordPiece::DoubleQuotedSequence(nested)
            | WordPiece::GettextDoubleQuotedSequence(nested) => {
                let (nested_glob, _nested_brace, nested_output) = expand_static_loop_pieces(
                    nested,
                    variable,
                    value,
                    true,
                    allow_globs,
                    allow_braces,
                )?;
                has_glob |= nested_glob;
                output.push_str(&nested_output);
            }
            WordPiece::ParameterExpansion(ParameterExpr::Parameter {
                parameter: Parameter::Named(name),
                indirect: false,
            }) if in_double_quotes && variable == Some(name.as_str()) => {
                output.push_str(value.ok_or("missing loop value")?);
            }
            WordPiece::ParameterExpansion(_) => {
                return Err("only the loop variable may be expanded inside double quotes".into());
            }
            WordPiece::TildePrefix(_)
            | WordPiece::CommandSubstitution(_)
            | WordPiece::BackquotedCommandSubstitution(_)
            | WordPiece::ArithmeticExpression(_) => {
                return Err("shell expansion is not supported".into());
            }
        }
    }
    Ok((has_glob, has_brace, output))
}

fn expand_brace_patterns(pattern: &str, max_values: usize) -> Result<Vec<String>, String> {
    let Some((open, close, alternatives)) = find_brace_group(pattern) else {
        return Ok(vec![pattern.to_owned()]);
    };
    let prefix = &pattern[..open];
    let suffix = &pattern[close + 1..];
    let mut expanded = Vec::new();
    for alternative in alternatives {
        let value = format!("{prefix}{alternative}{suffix}");
        for value in expand_brace_patterns(&value, max_values)? {
            expanded.push(value);
            if expanded.len() > max_values {
                return Err(format!(
                    "brace expansion exceeded the {max_values}-value safety bound"
                ));
            }
        }
    }
    Ok(expanded)
}

fn find_brace_group(pattern: &str) -> Option<(usize, usize, Vec<&str>)> {
    let open = pattern.find('{')?;
    let mut depth = 0;
    let mut close = None;
    let mut separators = Vec::new();
    for (offset, character) in pattern[open..].char_indices() {
        let index = open + offset;
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(index);
                    break;
                }
            }
            ',' if depth == 1 => separators.push(index),
            _ => {}
        }
    }
    let close = close?;
    if separators.is_empty() {
        return None;
    }
    let mut start = open + 1;
    let mut alternatives = Vec::with_capacity(separators.len() + 1);
    for separator in separators {
        alternatives.push(&pattern[start..separator]);
        start = separator + 1;
    }
    alternatives.push(&pattern[start..close]);
    Some((open, close, alternatives))
}

fn render_static_loop_redirect(redirect: &ast::IoRedirect) -> Result<String, String> {
    if is_static_stderr_to_stdout_redirect(redirect) || is_static_null_redirect(redirect) {
        return Ok(redirect.to_string());
    }
    Err("the loop body contains an unsupported redirection".into())
}

fn is_static_stderr_to_stdout_redirect(redirect: &ast::IoRedirect) -> bool {
    match redirect {
        ast::IoRedirect::File(
            Some(2),
            ast::IoFileRedirectKind::DuplicateOutput,
            ast::IoFileRedirectTarget::Fd(1),
        ) => true,
        ast::IoRedirect::File(
            Some(2),
            ast::IoFileRedirectKind::DuplicateOutput,
            ast::IoFileRedirectTarget::Filename(word),
        )
        | ast::IoRedirect::File(
            Some(2),
            ast::IoFileRedirectKind::DuplicateOutput,
            ast::IoFileRedirectTarget::Duplicate(word),
        ) => expand_static_loop_word(word, None, None).is_ok_and(|value| value == "1"),
        _ => false,
    }
}

fn is_static_null_redirect(redirect: &ast::IoRedirect) -> bool {
    match redirect {
        ast::IoRedirect::File(_, _, target) => match target {
            ast::IoFileRedirectTarget::Filename(word)
            | ast::IoFileRedirectTarget::Duplicate(word) => {
                expand_static_loop_word(word, None, None).is_ok_and(|value| value == "/dev/null")
            }
            _ => false,
        },
        ast::IoRedirect::OutputAndError(word, _) => {
            expand_static_loop_word(word, None, None).is_ok_and(|value| value == "/dev/null")
        }
        ast::IoRedirect::HereDocument(_, _) | ast::IoRedirect::HereString(_, _) => false,
    }
}

fn render_static_loop_command(
    command: &ast::Command,
    variable: &str,
    value: &str,
) -> Result<String, String> {
    let ast::Command::Simple(command) = command else {
        return Err("the loop body may contain only simple commands".into());
    };
    let mut rendered = String::new();
    let mut word_count = 0;
    if let Some(prefix) = &command.prefix {
        for item in &prefix.0 {
            render_static_loop_item(item, variable, value, &mut rendered, &mut word_count)?;
        }
    }
    if let Some(word) = &command.word_or_name {
        if !rendered.is_empty() {
            rendered.push(' ');
        }
        rendered.push_str(&shell_quote_word(&expand_static_loop_word(
            word,
            Some(variable),
            Some(value),
        )?));
        word_count += 1;
    }
    if let Some(suffix) = &command.suffix {
        for item in &suffix.0 {
            render_static_loop_item(item, variable, value, &mut rendered, &mut word_count)?;
        }
    }
    (word_count > 0)
        .then_some(rendered)
        .ok_or_else(|| "the loop body contains an empty command".into())
}

fn render_static_loop_item(
    item: &ast::CommandPrefixOrSuffixItem,
    variable: &str,
    value: &str,
    rendered: &mut String,
    word_count: &mut usize,
) -> Result<(), String> {
    if !rendered.is_empty() {
        rendered.push(' ');
    }
    match item {
        ast::CommandPrefixOrSuffixItem::Word(word) => {
            rendered.push_str(&shell_quote_word(&expand_static_loop_word(
                word,
                Some(variable),
                Some(value),
            )?));
            *word_count += 1;
            Ok(())
        }
        ast::CommandPrefixOrSuffixItem::IoRedirect(redirect) => {
            rendered.push_str(&render_static_loop_redirect(redirect)?);
            Ok(())
        }
        ast::CommandPrefixOrSuffixItem::AssignmentWord(_, _)
        | ast::CommandPrefixOrSuffixItem::ProcessSubstitution(_, _) => {
            Err("assignments and process substitutions are not supported in loops".into())
        }
    }
}

fn render_static_loop_pipeline(
    pipeline: &ast::Pipeline,
    variable: &str,
    value: &str,
) -> Result<String, String> {
    if pipeline.bang || pipeline.timed.is_some() {
        return Err("negated and timed pipelines are not supported in loops".into());
    }
    pipeline
        .seq
        .iter()
        .map(|command| render_static_loop_command(command, variable, value))
        .collect::<Result<Vec<_>, _>>()
        .map(|commands| commands.join(" | "))
}

fn render_static_loop_and_or(
    and_or: &ast::AndOrList,
    variable: &str,
    value: &str,
) -> Result<String, String> {
    let mut rendered = render_static_loop_pipeline(&and_or.first, variable, value)?;
    for additional in &and_or.additional {
        let (operator, pipeline) = match additional {
            ast::AndOr::And(pipeline) => (" && ", pipeline),
            ast::AndOr::Or(pipeline) => (" || ", pipeline),
        };
        rendered.push_str(operator);
        rendered.push_str(&render_static_loop_pipeline(pipeline, variable, value)?);
    }
    Ok(rendered)
}

fn render_static_loop_body(
    body: &ast::CompoundList,
    variable: &str,
    value: &str,
) -> Result<String, String> {
    if body.0.is_empty() {
        return Err("the loop body cannot be empty".into());
    }
    let mut rendered = String::new();
    for (index, item) in body.0.iter().enumerate() {
        if matches!(item.1, ast::SeparatorOperator::Async) {
            return Err("background commands are not supported in loops".into());
        }
        if index > 0 {
            rendered.push_str("; ");
        }
        rendered.push_str(&render_static_loop_and_or(&item.0, variable, value)?);
    }
    Ok(rendered)
}

fn parse_static_for_safe(
    clause: StaticForClause,
    inventory: Option<&ShellCommandInventory>,
    context: Option<&SafeShellContext>,
) -> Result<SafeShellClassification, String> {
    let mut values = Vec::new();
    for word in &clause.values {
        let expanded = expand_static_loop_value(word, context)
            .map_err(|error| format!("loop value is not safe: {error}"))?;
        if values.len() + expanded.len() > MAX_STATIC_LOOP_VALUES {
            return Err(format!(
                "the loop value list exceeds the {MAX_STATIC_LOOP_VALUES}-value static limit"
            ));
        }
        values.extend(expanded);
    }
    let mut paths = Vec::new();
    let mut operands = Vec::new();
    for value in values {
        let body = render_static_loop_body(&clause.body, &clause.variable, &value)?;
        let body_analysis = analyze_shell(&body).map_err(|error| error.to_string())?;
        if body_analysis
            .segments
            .iter()
            .any(|segment| segment.words.first().is_some_and(|word| word == "cd"))
        {
            return Err("directory changes are not supported in loops".into());
        }
        let classification = parse_safe_shell(&body, inventory)?;
        if classification.requires_git_hardening {
            return Err("Git commands are not supported in static loops".into());
        }
        paths.extend(classification.paths);
        operands.extend(classification.operands);
    }
    paths.sort();
    paths.dedup();
    Ok(SafeShellClassification {
        paths,
        presentation: None,
        exploration: None,
        operands,
        requires_git_hardening: false,
        hardened_command: None,
        covered_nested_segments: Vec::new(),
        path_list_output: None,
    })
}

fn parse_safe_shell(
    source: &str,
    inventory: Option<&ShellCommandInventory>,
) -> Result<SafeShellClassification, String> {
    parse_safe_shell_with_context(source, inventory, None)
}

fn parse_safe_shell_for_presentation(
    source: &str,
    inventory: Option<&ShellCommandInventory>,
) -> Result<SafeShellClassification, String> {
    parse_safe_shell_impl(source, inventory, None, true)
}

fn parse_safe_shell_with_context(
    source: &str,
    inventory: Option<&ShellCommandInventory>,
    context: Option<&SafeShellContext>,
) -> Result<SafeShellClassification, String> {
    parse_safe_shell_impl(source, inventory, context, false)
}

fn parse_safe_shell_impl(
    source: &str,
    inventory: Option<&ShellCommandInventory>,
    context: Option<&SafeShellContext>,
    preserve_path_patterns: bool,
) -> Result<SafeShellClassification, String> {
    if let Some(clause) = parse_static_for_clause(source)? {
        return parse_static_for_safe(clause, inventory, context);
    }
    let analysis = if preserve_path_patterns {
        super::analysis::analyze_shell_for_presentation(source)
    } else {
        analyze_shell(source)
    }
    .map_err(|error| error.to_string())?;
    if context.is_none() && analysis.opaque {
        return Err("the invocation contains dynamic or unsupported shell syntax".into());
    }
    if analysis.operators.iter().any(|operator| {
        matches!(operator, ShellOperator::Subshell)
            || (context.is_none()
                && matches!(
                    operator,
                    ShellOperator::Background | ShellOperator::Substitution
                ))
    }) {
        return Err("the invocation uses unsupported shell syntax".into());
    }
    if analysis.segments.is_empty() {
        return Err("the invocation is empty or dynamic".into());
    }
    if analysis.has_redirection && !analysis.only_read_safe_redirections {
        return Err("the invocation contains a non-null redirection".into());
    }
    if analysis.has_assignment {
        return Err("the invocation contains an environment assignment".into());
    }

    let segments = analysis
        .segments
        .iter()
        .filter(|segment| {
            !is_literal_shell_wrapper(&segment.words) && context.is_none_or(|_| segment.depth == 0)
        })
        .collect::<Vec<_>>();
    if segments.is_empty() {
        return Err("the invocation contains no read-safe command".into());
    }
    let standalone = segments.len() == 1
        && analysis
            .operators
            .iter()
            .all(|operator| matches!(operator, ShellOperator::Substitution));
    let mut cwd = PathBuf::from(".");
    let mut paths = Vec::new();
    let mut operands = Vec::new();
    let mut exploration = Vec::new();
    let mut all_presentable = true;
    let mut requires_git_hardening = false;
    let mut git_words = None;
    let mut normalized_rg_words = None;
    let mut covered_nested_segments = Vec::new();
    let mut parsed_path_list_output = None;

    for (index, segment) in segments.iter().enumerate() {
        let invocation = segment.words.first().ok_or("empty command segment")?;
        if invocation.contains('/') || invocation.contains('=') {
            return Err(format!(
                "path-qualified or dynamic command target: {invocation}"
            ));
        }
        let spec =
            find_spec(invocation).ok_or_else(|| format!("unregistered command: {invocation}"))?;
        if spec.tier != ShellSafetyTier::Level0 {
            return Err(format!(
                "command is generally-safe, not read-safe: {invocation}"
            ));
        }
        if let Some(inventory) = inventory {
            let availability = inventory
                .availability_for_invocation(invocation)
                .ok_or_else(|| {
                    format!("command is unavailable in this Bash backend: {invocation}")
                })?;
            if spec.hardening == SafeCommandHardening::Git && !availability.hardening_available {
                return Err("Git hardening is unavailable in this Bash backend".into());
            }
        }
        let normalized_words = (invocation == "rg")
            .then(|| normalize_quoted_leading_dash_pattern(&segment.words, &segment.source_words))
            .flatten();
        if normalized_words.is_some() && !standalone {
            return Err(
                "a quoted leading-dash rg pattern is supported only in a standalone command".into(),
            );
        }
        let parser_words = normalized_words
            .as_ref()
            .map_or(&segment.words, |normalized| &normalized.words);
        let parser_source_words = normalized_words
            .as_ref()
            .map(|normalized| normalized.source_words.clone())
            .unwrap_or_else(|| segment.source_words.clone());
        let raw_parsed =
            parse_invocation_for_mode(spec, &parser_words[1..], preserve_path_patterns)
                .ok_or_else(|| {
                    format!(
                        "command is not in a proven read-safe form: {}",
                        segment.raw.trim()
                    )
                })?;
        if preserve_path_patterns {
            validate_path_pattern_positions(
                parser_words,
                &parser_source_words,
                &raw_parsed.operands,
            )?;
        }
        let (words, nested_paths) = context.map_or_else(
            || Ok((parser_words.clone(), Vec::new())),
            |context| {
                expand_segment_words(
                    parser_words,
                    &parser_source_words,
                    &raw_parsed.operands,
                    context,
                    &cwd,
                    inventory,
                )
            },
        )?;
        paths.extend(nested_paths);
        let mut parsed = parse_invocation_for_mode(spec, &words[1..], preserve_path_patterns)
            .ok_or_else(|| {
                format!(
                    "command is not in a proven read-safe form: {}",
                    segment.raw.trim()
                )
            })?;
        if context.is_some() {
            covered_nested_segments.extend(
                analysis
                    .segments
                    .iter()
                    .filter(|nested| nested.depth > 0)
                    .map(|nested| nested.raw.trim().to_owned()),
            );
        }
        parsed_path_list_output = parsed.path_list_output;
        parsed.hardening = spec.hardening;
        requires_git_hardening |= parsed.hardening == SafeCommandHardening::Git;
        if parsed.hardening == SafeCommandHardening::Git {
            git_words = Some(segment.words.clone());
        }
        if let Some(normalized) = normalized_words {
            normalized_rg_words = Some(normalized.words);
        }
        let mut segment_paths = Vec::new();
        for operand in &mut parsed.operands {
            operand.base_directory = cwd.clone();
            let resolved = resolve_shell_path(&cwd, &operand.value);
            segment_paths.push(resolved.clone());
            paths.push(resolved);
        }
        operands.extend(parsed.operands);
        if let Some(presentation) = parsed.presentation {
            let detail = parsed.detail.take().map(|mut detail| {
                for value in detail.arguments.iter_mut().chain(
                    detail
                        .relations
                        .iter_mut()
                        .flat_map(|relation| relation.values.iter_mut()),
                ) {
                    if value.path {
                        value.value = resolve_shell_path(&cwd, &value.value);
                    }
                }
                detail
            });
            exploration.push(SafeShellActivity {
                presentation,
                targets: parsed.targets.clone(),
                paths: segment_paths,
                query: parsed.query.clone(),
                command: segment.raw.trim().to_owned(),
                detail,
            });
        } else if !parsed.transparent && !matches!(invocation.as_str(), "true" | "false") {
            all_presentable = false;
        }
        if let Some(target) = parsed.directory_transition {
            match analysis.operators.get(index) {
                Some(ShellOperator::Pipeline) => {}
                Some(ShellOperator::And | ShellOperator::Sequence) => {
                    cwd = PathBuf::from(resolve_shell_path(&cwd, &target));
                }
                None if index + 1 == segments.len() => {}
                _ => return Err("cd transition has ambiguous conditional semantics".into()),
            }
        }
    }
    paths.sort();
    paths.dedup();
    if requires_git_hardening
        && (segments.len() != 1 || is_literal_shell_wrapper(&analysis.segments[0].words))
    {
        return Err(
            "safe Git hardening is supported only for a direct standalone Git command".into(),
        );
    }
    let hardened_command = git_words
        .as_deref()
        .map(harden_git_words)
        .or_else(|| normalized_rg_words.as_deref().map(harden_shell_words));
    covered_nested_segments.sort();
    covered_nested_segments.dedup();
    Ok(SafeShellClassification {
        paths,
        presentation: standalone
            .then(|| exploration.first().map(|row| row.presentation))
            .flatten(),
        exploration: (all_presentable && !exploration.is_empty()).then_some(exploration),
        operands,
        requires_git_hardening,
        hardened_command,
        covered_nested_segments,
        path_list_output: standalone.then_some(parsed_path_list_output).flatten(),
    })
}

fn harden_shell_words(words: &[String]) -> String {
    words
        .iter()
        .map(|word| shell_quote_word(word))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parses one registry-backed command form.
///
/// A sole `--help` is universally read-safe for a command already in the
/// registry. It is intentionally not accepted alongside another operation:
/// doing so would make each command's ordinary option grammar ambiguous.
fn parse_read_safe_invocation(spec: &SafeCommandSpec, args: &[String]) -> Option<SafeCommandParse> {
    if matches!(args, [help] if help == "--help") {
        return Some(SafeCommandParse::new(None));
    }
    (spec.parser)(args)
}

fn parse_invocation_for_mode(
    spec: &SafeCommandSpec,
    args: &[String],
    presentation: bool,
) -> Option<SafeCommandParse> {
    if presentation {
        classifiers::parse_presentation_invocation(spec, args)
    } else {
        parse_read_safe_invocation(spec, args)
    }
}

fn harden_git_words(words: &[String]) -> String {
    let mut hardened = vec![
        shell_quote_word(&words[0]),
        "-c".into(),
        "core.fsmonitor=false".into(),
        "-c".into(),
        "core.untrackedCache=false".into(),
        "-c".into(),
        "diff.external=".into(),
        "-c".into(),
        "diff.trustExitCode=false".into(),
    ];
    hardened.push(shell_quote_word(&words[1]));
    if matches!(words[1].as_str(), "log" | "diff" | "show") {
        hardened.extend(["--no-ext-diff".into(), "--no-textconv".into()]);
    }
    hardened.extend(words[2..].iter().map(|word| shell_quote_word(word)));
    let command = hardened.join(" ");
    format!(
        "unset GIT_EXTERNAL_DIFF; export GIT_TERMINAL_PROMPT=0 GIT_OPTIONAL_LOCKS=0 GIT_PAGER=cat GIT_EDITOR=true GIT_SEQUENCE_EDITOR=true; {command}"
    )
}

fn expand_segment_words(
    words: &[String],
    source_words: &[String],
    raw_operands: &[SafePathOperand],
    context: &SafeShellContext,
    logical_cwd: &Path,
    inventory: Option<&ShellCommandInventory>,
) -> Result<(Vec<String>, Vec<String>), String> {
    let (path_operand_words, remaining_paths) = path_operand_words(words, raw_operands);
    let actual_cwd = context.actual_cwd(logical_cwd);
    let mut expanded = Vec::new();
    let mut nested_paths = Vec::new();
    for (index, (word, path_operand)) in words.iter().zip(path_operand_words).enumerate() {
        let source_word = source_words.get(index).unwrap_or(word);
        if !path_operand && word_requires_context(source_word)? {
            return Err("dynamic expansion is not in a filesystem-operand position".into());
        }
        if path_operand && let Some(source) = whole_unquoted_command_substitution(source_word)? {
            let (values, producer_paths) =
                expand_path_list_substitution(&source, context, &actual_cwd, inventory)?;
            expanded.extend(values);
            nested_paths.extend(producer_paths);
            continue;
        }
        let values = expand_safe_word(source_word, context, &actual_cwd, inventory)
            .map_err(|error| format!("word {word:?} (path={path_operand}): {error}"))?;
        if !path_operand && values.len() != 1 {
            return Err("dynamic word does not resolve to one deterministic word".into());
        }
        expanded.extend(values);
    }
    if remaining_paths != 0 {
        return Err("the command parser did not account for every filesystem operand".into());
    }
    Ok((expanded, nested_paths))
}

fn whole_unquoted_command_substitution(word: &str) -> Result<Option<String>, String> {
    let pieces = brush_parser::word::parse(word, &ParserOptions::default())
        .map_err(|error| error.to_string())?;
    Ok(match pieces.as_slice() {
        [piece] => match &piece.piece {
            WordPiece::CommandSubstitution(source) => Some(source.clone()),
            _ => None,
        },
        _ => None,
    })
}

fn expand_path_list_substitution(
    source: &str,
    context: &SafeShellContext,
    cwd: &Path,
    inventory: Option<&ShellCommandInventory>,
) -> Result<(Vec<String>, Vec<String>), String> {
    let inventory = inventory.ok_or("path-list substitution requires command inventory")?;
    let classification = parse_safe_shell_with_context(source, Some(inventory), Some(context))?;
    if classification.path_list_output != Some(SafePathListOutput::Lines) {
        return Err("nested command is not a proven path-list producer".into());
    }
    let output = run_path_list_preflight(source, context, cwd)?;
    if output.len() > MAX_PATH_LIST_OUTPUT_BYTES {
        return Err("path-list output exceeded its byte safety bound".into());
    }
    let output = String::from_utf8(output).map_err(|_| "path-list output is not UTF-8")?;
    if output.contains('\0') {
        return Err("NUL-delimited path-list output is unsupported".into());
    }
    let mut values = Vec::new();
    for field in output.split_whitespace() {
        if field.starts_with('-') {
            return Err("path-list output contains an option-like value".into());
        }
        values.extend(expand_path_glob(field, cwd, context.max_glob_matches)?);
        if values.len() > context.max_glob_matches {
            return Err("path-list output exceeded its item safety bound".into());
        }
    }
    if values.is_empty() {
        return Err("path-list producer emitted no filesystem operands".into());
    }
    Ok((values, classification.paths))
}

fn run_path_list_preflight(
    source: &str,
    context: &SafeShellContext,
    cwd: &Path,
) -> Result<Vec<u8>, String> {
    let mut command = Command::new(&context.backend.executable);
    match context.backend.kind {
        BashBackendKind::Wsl => {
            let source = format!(
                "cd -- {} && {source}",
                shell_quote_word(&shell_path(BashBackendKind::Wsl, cwd))
            );
            command.args(["--exec", "bash", "-lc", &source]);
        }
        BashBackendKind::Native | BashBackendKind::GitBash => {
            command.args(["-lc", source]).current_dir(cwd);
        }
    }
    command
        .envs(&context.variables)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command
        .spawn()
        .map_err(|error| format!("path-list preflight failed: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or("path-list preflight has no stdout")?;
    let reader = std::thread::spawn(move || read_bounded_output(stdout));
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("path-list preflight failed: {error}"))?
        {
            break status;
        }
        if started.elapsed() >= PATH_LIST_PREFLIGHT_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return Err("path-list preflight timed out".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let output = reader
        .join()
        .map_err(|_| "path-list output reader panicked")??;
    if !status.success() {
        return Err(format!("path-list producer exited with status {}", status));
    }
    Ok(output)
}

fn read_bounded_output(mut stdout: impl Read) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        let read = stdout
            .read(&mut chunk)
            .map_err(|error| format!("cannot read path-list output: {error}"))?;
        if read == 0 {
            return Ok(output);
        }
        if output.len() + read > MAX_PATH_LIST_OUTPUT_BYTES {
            return Err("path-list output exceeded its byte safety bound".into());
        }
        output.extend_from_slice(&chunk[..read]);
    }
}

fn validate_path_pattern_positions(
    words: &[String],
    source_words: &[String],
    raw_operands: &[SafePathOperand],
) -> Result<(), String> {
    let (path_operand_words, _) = path_operand_words(words, raw_operands);
    for (index, (word, path_operand)) in words.iter().zip(path_operand_words).enumerate() {
        let source_word = source_words.get(index).unwrap_or(word);
        if !path_operand && word_requires_context(source_word)? {
            return Err("pathname pattern is not in a filesystem-operand position".into());
        }
    }
    Ok(())
}

fn path_operand_words(words: &[String], raw_operands: &[SafePathOperand]) -> (Vec<bool>, usize) {
    let mut remaining_paths = raw_operands
        .iter()
        .map(|operand| operand.value.as_str())
        .collect::<Vec<_>>();
    let matches = words
        .iter()
        .enumerate()
        .map(|(index, word)| {
            if index == 0 {
                return false;
            }
            let Some(position) = remaining_paths.iter().position(|value| *value == word) else {
                return false;
            };
            remaining_paths.remove(position);
            true
        })
        .collect();
    (matches, remaining_paths.len())
}

fn word_requires_context(word: &str) -> Result<bool, String> {
    let pieces = brush_parser::word::parse(word, &ParserOptions::default())
        .map_err(|error| error.to_string())?;
    Ok(word_pieces_require_context(&pieces, false))
}

fn word_pieces_require_context(
    pieces: &[brush_parser::word::WordPieceWithSource],
    in_double_quotes: bool,
) -> bool {
    pieces
        .iter()
        .any(|piece| word_piece_requires_context(piece, in_double_quotes))
}

fn word_piece_requires_context(
    piece: &brush_parser::word::WordPieceWithSource,
    in_double_quotes: bool,
) -> bool {
    match &piece.piece {
        WordPiece::Text(value) => value
            .chars()
            .any(|character| !in_double_quotes && matches!(character, '*' | '?' | '[' | '{')),
        WordPiece::ParameterExpansion(_)
        | WordPiece::CommandSubstitution(_)
        | WordPiece::BackquotedCommandSubstitution(_)
        | WordPiece::TildePrefix(_)
        | WordPiece::ArithmeticExpression(_) => true,
        WordPiece::DoubleQuotedSequence(nested)
        | WordPiece::GettextDoubleQuotedSequence(nested) => {
            word_pieces_require_context(nested, true)
        }
        WordPiece::SingleQuotedText(_)
        | WordPiece::AnsiCQuotedText(_)
        | WordPiece::EscapeSequence(_) => false,
    }
}

fn expand_safe_word(
    word: &str,
    context: &SafeShellContext,
    cwd: &Path,
    inventory: Option<&ShellCommandInventory>,
) -> Result<Vec<String>, String> {
    let pieces = brush_parser::word::parse(word, &ParserOptions::default())
        .map_err(|error| error.to_string())?;
    let (value, glob, brace, _dynamic) =
        expand_safe_pieces(&pieces, context, cwd, false, inventory)?;
    let values = if brace {
        let values = expand_brace_patterns(&value, MAX_STATIC_LOOP_VALUES)?;
        if values.len() == 1
            && values[0] == value
            && value.contains(',')
            && (value.contains('{') || value.contains('}'))
        {
            return Err("unsupported brace expansion pattern".into());
        }
        values
    } else {
        vec![value]
    };
    if !glob {
        return Ok(values);
    }
    let mut expanded = Vec::new();
    for value in values {
        expanded.extend(expand_path_glob(&value, cwd, context.max_glob_matches)?);
        if expanded.len() > context.max_glob_matches {
            return Err(format!(
                "path expansion exceeded the {}-match safety bound",
                context.max_glob_matches
            ));
        }
    }
    Ok(expanded)
}

fn expand_safe_pieces(
    pieces: &[brush_parser::word::WordPieceWithSource],
    context: &SafeShellContext,
    cwd: &Path,
    in_double_quotes: bool,
    inventory: Option<&ShellCommandInventory>,
) -> Result<(String, bool, bool, bool), String> {
    let mut output = String::new();
    let mut glob = false;
    let mut brace = false;
    let mut dynamic = false;
    for piece in pieces {
        match &piece.piece {
            WordPiece::Text(value) => {
                if !in_double_quotes
                    && value
                        .chars()
                        .any(|character| matches!(character, '*' | '?' | '['))
                {
                    glob = true;
                    dynamic = true;
                }
                if !in_double_quotes && value.contains('{') {
                    brace = true;
                    dynamic = true;
                }
                output.push_str(value);
            }
            WordPiece::SingleQuotedText(value)
            | WordPiece::AnsiCQuotedText(value)
            | WordPiece::EscapeSequence(value) => output.push_str(value),
            WordPiece::DoubleQuotedSequence(nested) => {
                let (value, nested_glob, nested_brace, nested_dynamic) =
                    expand_safe_pieces(nested, context, cwd, true, inventory)?;
                output.push_str(&value);
                brace |= nested_brace;
                if nested_glob {
                    // Quoting suppresses pathname expansion, so the glob
                    // characters remain ordinary text.
                    dynamic |= nested_dynamic;
                } else {
                    dynamic |= nested_dynamic;
                }
            }
            WordPiece::GettextDoubleQuotedSequence(_) => {
                return Err("gettext double-quoted expansion is unsupported".into());
            }
            WordPiece::TildePrefix(user) if user.is_empty() => {
                let home = context
                    .variable("HOME")
                    .ok_or("HOME is unset for tilde expansion")?;
                if home.is_empty()
                    || home.chars().any(char::is_whitespace)
                    || home
                        .chars()
                        .any(|character| matches!(character, '*' | '?' | '[' | '{'))
                {
                    return Err("HOME is not one deterministic path".into());
                }
                output.push_str(&home);
                dynamic = true;
            }
            WordPiece::TildePrefix(_) => {
                return Err("named-user tilde expansion is unsupported".into());
            }
            WordPiece::ArithmeticExpression(_) => {
                return Err("arithmetic expansion is unsupported".into());
            }
            WordPiece::ParameterExpansion(ParameterExpr::Parameter {
                parameter: Parameter::Named(name),
                indirect: false,
            }) => {
                let value = context
                    .variable(name)
                    .ok_or_else(|| format!("approved path variable is unset: {name}"))?;
                if !in_double_quotes
                    && (value.is_empty()
                        || value.chars().any(char::is_whitespace)
                        || value
                            .chars()
                            .any(|character| matches!(character, '*' | '?' | '[' | '{')))
                {
                    return Err("unquoted variable value is not one deterministic word".into());
                }
                output.push_str(&value);
                dynamic = true;
            }
            WordPiece::ParameterExpansion(_) => {
                return Err("only simple approved path variables are supported".into());
            }
            WordPiece::CommandSubstitution(source) => {
                let value = deterministic_command_substitution(source, context, inventory)?;
                if !in_double_quotes
                    && (value.is_empty()
                        || value.chars().any(char::is_whitespace)
                        || value
                            .chars()
                            .any(|character| matches!(character, '*' | '?' | '[' | '{')))
                {
                    return Err("unquoted command output is not one deterministic word".into());
                }
                output.push_str(&value);
                dynamic = true;
            }
            WordPiece::BackquotedCommandSubstitution(_) => {
                return Err("backquoted command substitution is unsupported".into());
            }
        }
    }
    Ok((output, glob, brace, dynamic))
}

fn deterministic_command_substitution(
    source: &str,
    _context: &SafeShellContext,
    inventory: Option<&ShellCommandInventory>,
) -> Result<String, String> {
    let analysis = analyze_shell(source).map_err(|error| error.to_string())?;
    if analysis.opaque
        || analysis
            .operators
            .iter()
            .any(|operator| !matches!(operator, ShellOperator::Substitution))
    {
        return Err("nested command is not a single deterministic command".into());
    }
    let segment = analysis
        .segments
        .iter()
        .find(|segment| segment.depth == 0)
        .ok_or("nested command is empty")?;
    let invocation = segment.words.first().ok_or("nested command is empty")?;
    let spec = find_spec(invocation).ok_or("nested command is not registered")?;
    if !matches!(spec.canonical, "printf" | "echo") {
        return Err("nested command has no deterministic-output capability".into());
    }
    if let Some(inventory) = inventory {
        inventory
            .availability_for_invocation(invocation)
            .ok_or("nested command is unavailable in this Bash backend")?;
    }
    let args = &segment.words[1..];
    if matches!(spec.canonical, "printf") {
        if args.len() != 1 {
            return Err("printf requires one literal format string".into());
        }
        let value = static_literal_word(&args[0])?;
        if value.contains('%')
            || value.contains('\\')
            || has_glob_chars(&value)
            || value.contains('{')
        {
            return Err("printf directives and escapes are unsupported".into());
        }
        return Ok(value);
    }
    if args.is_empty() || args.iter().any(|arg| arg.starts_with('-')) {
        return Err("echo option flags are unsupported in command substitution".into());
    }
    let output = args
        .iter()
        .map(|arg| static_literal_word(arg))
        .collect::<Result<Vec<_>, _>>()
        .map(|values| values.join(" "))?;
    if has_glob_chars(&output) || output.contains('{') {
        return Err("echo output containing shell pattern characters is unsupported".into());
    }
    Ok(output)
}

fn static_literal_word(word: &str) -> Result<String, String> {
    let pieces = brush_parser::word::parse(word, &ParserOptions::default())
        .map_err(|error| error.to_string())?;
    let mut output = String::new();
    append_static_literal_pieces(&pieces, &mut output)?;
    Ok(output)
}

fn append_static_literal_pieces(
    pieces: &[brush_parser::word::WordPieceWithSource],
    output: &mut String,
) -> Result<(), String> {
    for piece in pieces {
        match &piece.piece {
            WordPiece::Text(value)
            | WordPiece::SingleQuotedText(value)
            | WordPiece::AnsiCQuotedText(value)
            | WordPiece::EscapeSequence(value) => output.push_str(value),
            WordPiece::DoubleQuotedSequence(nested) => {
                append_static_literal_pieces(nested, output)?;
            }
            _ => return Err("nested output must be literal".into()),
        }
    }
    Ok(())
}

fn expand_path_glob(pattern: &str, cwd: &Path, max_matches: usize) -> Result<Vec<String>, String> {
    let pattern_path = Path::new(pattern);
    let base = if pattern_path.is_absolute() {
        PathBuf::from(std::path::MAIN_SEPARATOR.to_string())
    } else {
        cwd.to_path_buf()
    };
    let components = pattern_path.components().collect::<Vec<_>>();
    let mut output = Vec::new();
    expand_glob_components(&base, &components, 0, cwd, max_matches, &mut output)?;
    output.sort();
    output.dedup();
    if output.is_empty() {
        // Bash leaves an unmatched pattern unchanged unless nullglob is set.
        return Ok(vec![pattern.to_owned()]);
    }
    Ok(output)
}

fn expand_glob_components(
    current: &Path,
    components: &[Component<'_>],
    index: usize,
    cwd: &Path,
    max_matches: usize,
    output: &mut Vec<String>,
) -> Result<(), String> {
    if output.len() >= max_matches {
        return Err("pathname expansion exceeded its safety bound".into());
    }
    if index == components.len() {
        let rendered = if Path::new(current).is_absolute() {
            if let Ok(relative) = current.strip_prefix(cwd) {
                relative.to_string_lossy().into_owned()
            } else {
                current.to_string_lossy().into_owned()
            }
        } else {
            current.to_string_lossy().into_owned()
        };
        output.push(if rendered.is_empty() {
            ".".into()
        } else {
            rendered
        });
        return Ok(());
    }
    let component = components[index].as_os_str();
    if component == "." || component == ".." {
        return expand_glob_components(
            &current.join(component),
            components,
            index + 1,
            cwd,
            max_matches,
            output,
        );
    }
    let pattern = component.to_string_lossy();
    if !has_glob_chars(&pattern) {
        let next = current.join(component);
        if std::fs::symlink_metadata(&next).is_ok() {
            return expand_glob_components(&next, components, index + 1, cwd, max_matches, output);
        }
        return Ok(());
    }
    let entries = match std::fs::read_dir(current) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotADirectory => return Ok(()),
        Err(error) => return Err(format!("cannot safely expand pathname: {error}")),
    };
    let mut names = entries
        .map(|entry| entry.map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    names.sort_by_key(|entry| entry.file_name());
    for entry in names {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') && !pattern.starts_with('.') {
            continue;
        }
        if glob_component_matches(&pattern, &name) {
            expand_glob_components(
                &entry.path(),
                components,
                index + 1,
                cwd,
                max_matches,
                output,
            )?;
        }
    }
    Ok(())
}

fn has_glob_chars(value: &str) -> bool {
    value
        .chars()
        .any(|character| matches!(character, '*' | '?' | '['))
}

fn glob_component_matches(pattern: &str, value: &str) -> bool {
    let pattern = pattern.chars().collect::<Vec<_>>();
    let value = value.chars().collect::<Vec<_>>();
    glob_component_match_at(&pattern, &value, 0, 0)
}

fn glob_component_match_at(pattern: &[char], value: &[char], pi: usize, vi: usize) -> bool {
    if pi == pattern.len() {
        return vi == value.len();
    }
    match pattern[pi] {
        '*' => {
            let mut next = pi;
            while next + 1 < pattern.len() && pattern[next + 1] == '*' {
                next += 1;
            }
            (vi..=value.len())
                .any(|value_index| glob_component_match_at(pattern, value, next + 1, value_index))
        }
        '?' => vi < value.len() && glob_component_match_at(pattern, value, pi + 1, vi + 1),
        '[' => {
            let Some((end, matched)) = bracket_match(pattern, value.get(vi).copied(), pi) else {
                return value.get(vi) == Some(&'[')
                    && glob_component_match_at(pattern, value, pi + 1, vi + 1);
            };
            matched && vi < value.len() && glob_component_match_at(pattern, value, end + 1, vi + 1)
        }
        character => {
            vi < value.len()
                && value[vi] == character
                && glob_component_match_at(pattern, value, pi + 1, vi + 1)
        }
    }
}

fn bracket_match(pattern: &[char], value: Option<char>, start: usize) -> Option<(usize, bool)> {
    let mut end = start + 1;
    if end >= pattern.len() {
        return None;
    }
    let negated = matches!(pattern[end], '!' | '^');
    if negated {
        end += 1;
    }
    let mut matched = false;
    let mut has_item = false;
    while end < pattern.len() && pattern[end] != ']' {
        has_item = true;
        if end + 2 < pattern.len() && pattern[end + 1] == '-' && pattern[end + 2] != ']' {
            if value.is_some_and(|value| pattern[end] <= value && value <= pattern[end + 2]) {
                matched = true;
            }
            end += 3;
        } else {
            matched |= value == Some(pattern[end]);
            end += 1;
        }
    }
    (has_item && end < pattern.len()).then_some((end, if negated { !matched } else { matched }))
}

fn shell_quote_word(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn resolve_shell_path(cwd: &Path, value: &str) -> String {
    let path = Path::new(value);
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir if normalized.file_name().is_some_and(|name| name != "..") => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    if normalized.as_os_str().is_empty() {
        ".".into()
    } else {
        normalized.to_string_lossy().into_owned()
    }
}

fn is_literal_shell_wrapper(words: &[String]) -> bool {
    matches!(words, [shell, option, _] if matches!(shell.as_str(), "bash" | "sh" | "zsh") && matches!(option.as_str(), "-c" | "-lc"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_complete_and_examples_parse() {
        for spec in SAFE_COMMAND_REGISTRY {
            assert!(!spec.examples.is_empty(), "{}", spec.canonical);
            assert!(!spec.restrictions.is_empty(), "{}", spec.canonical);
            for example in spec.examples {
                if spec.tier == ShellSafetyTier::Level0 {
                    assert!(classify_read_safe_shell(example).is_some(), "{example}");
                }
            }
        }
    }

    #[test]
    fn inventory_gates_commands_and_aliases() {
        let inventory = ShellCommandInventory::synthetic(&["cat", "fdfind", "true"]);
        assert!(authorize_available_safe_shell("cat README.md", &inventory).is_some());
        assert!(authorize_available_safe_shell("rg needle .", &inventory).is_none());
        assert!(authorize_available_safe_shell("fdfind needle .", &inventory).is_some());
        assert!(authorize_available_safe_shell("fd needle .", &inventory).is_none());
        assert!(authorize_available_safe_shell("true", &inventory).is_some());
    }

    #[test]
    fn fd_allows_absolute_listing_without_execution_helpers() {
        let command = "fd -a 'SPEC.md|Cargo.toml|AGENTS.md' .";
        let classification = classify_read_safe_shell(command).expect("safe fd form");
        assert_eq!(classification.paths, ["."]);
        assert_eq!(
            classification.presentation,
            Some(SafeShellPresentation::List)
        );
        let activity = &classification.exploration.as_ref().unwrap()[0];
        assert_eq!(activity.presentation, SafeShellPresentation::List);
        assert_eq!(activity.targets, ["SPEC.md|Cargo.toml|AGENTS.md"]);

        let inventory = ShellCommandInventory::synthetic(&["fd"]);
        assert!(authorize_available_safe_shell(command, &inventory).is_some());
        assert!(
            classify_read_safe_shell(
                "fd -a 'SPEC.md|Cargo.toml|AGENTS.md' . && git status --short"
            )
            .is_none()
        );
        assert!(classify_read_safe_shell("fd -x cat {} .").is_none());
        assert!(classify_read_safe_shell("fd --exec cat {} .").is_none());
    }

    #[test]
    fn fd_does_not_interpret_options_after_double_dash() {
        let classification =
            classify_read_safe_shell("fd -- --extension rs").expect("safe fd form");
        let activity = &classification.exploration.unwrap()[0];

        assert_eq!(activity.targets, ["--extension"]);
        assert_eq!(activity.paths, ["rs"]);
    }

    #[test]
    fn fd_allows_regex_listing_with_excluded_directories() {
        let command = r#"fd -a '^(SPEC\.md|AGENTS\.md|Cargo\.toml|.*\.rs|.*\.toml|.*\.md)$' . --exclude target --exclude references"#;
        let classification = classify_read_safe_shell(command)
            .unwrap_or_else(|| panic!("safe fd form: {:?}", assess_read_safe_shell(command)));

        assert_eq!(classification.paths, ["."]);
        assert_eq!(
            classification.presentation,
            Some(SafeShellPresentation::List)
        );
        let activity = &classification.exploration.as_ref().unwrap()[0];
        assert_eq!(
            activity.targets,
            [r#"^(SPEC\.md|AGENTS\.md|Cargo\.toml|.*\.rs|.*\.toml|.*\.md)$"#]
        );

        let inventory = ShellCommandInventory::synthetic(&["fd"]);
        assert!(authorize_available_safe_shell(command, &inventory).is_some());
    }

    #[test]
    fn fd_list_targets_distinguish_filters_from_search_roots() {
        let extension = classify_read_safe_shell("fd -e rs . crates").unwrap();
        let extension_activity = &extension.exploration.unwrap()[0];
        assert_eq!(extension_activity.targets, ["*.rs"]);
        assert_eq!(extension_activity.paths, ["crates"]);

        let unfiltered = classify_read_safe_shell("fd --hidden -t f .").unwrap();
        let unfiltered_activity = &unfiltered.exploration.unwrap()[0];
        assert!(unfiltered_activity.targets.is_empty());
        assert_eq!(unfiltered_activity.paths, ["."]);

        let pattern = classify_read_safe_shell("fd -e rs crates").unwrap();
        let pattern_activity = &pattern.exploration.unwrap()[0];
        assert_eq!(pattern_activity.targets, ["crates"]);
        assert_eq!(pattern_activity.paths, ["."]);
    }

    #[test]
    fn background_requests_use_the_same_safe_classification() {
        let inventory = ShellCommandInventory::synthetic(&["cat"]);
        let request = super::super::BashRequest {
            command: "cat README.md &".into(),
            cwd: None,
            env: BTreeMap::new(),
            forward_env: Vec::new(),
            timeout: None,
            wait: false,
        };

        assert!(authorize_available_safe_bash_request(&request, &inventory).is_some());
    }

    #[test]
    fn safe_background_segments_are_independent_but_subshells_are_not_safe() {
        let inventory = ShellCommandInventory::synthetic(&["ls"]);

        let mixed = super::super::BashRequest {
            command: "ls & rm -f TEST.md".into(),
            cwd: None,
            env: BTreeMap::new(),
            forward_env: Vec::new(),
            timeout: None,
            wait: true,
        };
        let mixed_analysis = analyze_shell(&mixed.command).unwrap();
        let authorization =
            authorize_available_safe_bash_segments(&mixed, &inventory, &mixed_analysis, 3, false)
                .unwrap();
        assert!(authorization.segment(0).is_some());
        assert!(authorization.segment(1).is_none());

        let subshell = super::super::BashRequest {
            command: "(ls)".into(),
            cwd: None,
            env: BTreeMap::new(),
            forward_env: Vec::new(),
            timeout: None,
            wait: true,
        };
        assert!(authorize_available_safe_bash_request(&subshell, &inventory).is_none());
    }

    #[test]
    fn security_regressions_fall_back() {
        for command in [
            "rg --pre helper needle",
            "rg --search-zip needle archive.zip",
            "find . -exec echo {} ;",
            "find . -delete",
            "sort -o out input",
            "sort --compress-program helper input",
            "sha256sum -c sums",
            "tar -xf archive.tar",
            "unzip -d out archive.zip",
            "sed -i s/a/b/ file",
            "git -c x=y status",
            "git diff --output=out",
            "git diff --ext-diff",
            "git diff --textconv",
            "git branch new",
            "git branch -D old",
            "git tag release-candidate",
            "git tag -a release-candidate",
            "sqlite3 -readonly cagent.db",
            "sqlite3 -readonly cagent.db",
            "sqlite3 -readonly cagent.db 'DELETE FROM users'",
            "pwd --unknown",
            "./cat README.md",
        ] {
            assert!(classify_read_safe_shell(command).is_none(), "{command}");
        }
        assert!(classify_read_safe_shell("true").is_some());
        assert_eq!(
            classify_read_safe_shell("rg -f ../patterns src")
                .unwrap()
                .paths,
            ["../patterns", "src"]
        );
        assert_eq!(
            classify_read_safe_shell("grep -f ../patterns target")
                .unwrap()
                .paths,
            ["../patterns", "target"]
        );
        assert_eq!(
            classify_read_safe_shell("rg --ignore-file ../ignore needle .")
                .unwrap()
                .paths,
            [".", "../ignore"]
        );
    }

    #[test]
    fn requested_git_examples_extract_pathspecs_and_require_hardening() {
        for command in [
            "git diff --check -- plays/say_hi.toml",
            "git status --short -- plays/say_hi.toml",
            "git status --porcelain=v1 -- plays/say_hi.toml",
        ] {
            let classification = classify_read_safe_shell(command).expect("safe Git example");
            assert_eq!(classification.paths, ["plays/say_hi.toml"]);
            assert!(classification.requires_git_hardening);
            assert!(classification.hardened_command.is_some());
        }
        assert!(classify_read_safe_shell("git status --porcelain=v2").is_none());
    }

    #[test]
    fn jj_ignore_working_copy_inspections_are_level_zero() {
        for command in [
            "jj --ignore-working-copy root",
            "jj --ignore-working-copy --no-pager st",
            "jj --ignore-working-copy status --no-pager",
            "jj --ignore-working-copy file show -r 57df73a66657 crates/cagent-agent/src/presentation/history.rs",
            "jj --ignore-working-copy diff -r 57df73a66657 -- crates/cagent-agent/src/presentation/history.rs",
            "jj --ignore-working-copy file list -r @ crates",
            "jj --ignore-working-copy bookmark list",
        ] {
            assert!(classify_read_safe_shell(command).is_some(), "{command}");
        }
        assert!(
            authorize_available_safe_shell(
                "jj --ignore-working-copy --no-pager st",
                &ShellCommandInventory::synthetic(&["jj"]),
            )
            .is_some()
        );
        for command in [
            "jj",
            "jj root",
            "jj status --no-pager",
            "jj --config aliases.st='run rm' st",
            "jj diff --tool helper",
            "jj file show -T '{path}' SPEC.md",
            "jj file search --pattern needle --config x",
            "jj describe -m update",
            "jj file track new.md",
            "jj bookmark create feature",
            "jj operation restore abc",
        ] {
            assert!(classify_read_safe_shell(command).is_none(), "{command}");
        }
    }

    #[test]
    fn sqlite3_readonly_accepts_literal_database_paths_and_read_only_inputs() {
        for (command, path) in [
            (
                "sqlite3 -readonly cagent.db 'SELECT name FROM sqlite_master'",
                "cagent.db",
            ),
            ("sqlite3 -readonly ./cagent.db 'VALUES (1)'", "cagent.db"),
            (
                "sqlite3 -readonly subdir/cagent.db 'SELECT 1'",
                "subdir/cagent.db",
            ),
            ("sqlite3 -readonly ../cagent.db 'SELECT 1'", "../cagent.db"),
            (
                "sqlite3 -readonly /tmp/cagent.db 'SELECT 1'",
                "/tmp/cagent.db",
            ),
            (
                "sqlite3 -readonly -header -column cagent.db 'SELECT 1; SELECT 2;'",
                "cagent.db",
            ),
            (
                "sqlite3 -readonly cagent.db 'SELECT 1; VALUES (2); SELECT 3'",
                "cagent.db",
            ),
            (
                "sqlite3 -readonly cagent.db \"WITH RECURSIVE p(id,parent_id,kind,status,content_json,depth) AS (SELECT id,parent_id,kind,status,content_json,0 FROM nodes WHERE id=(SELECT active_node_id FROM conversations) UNION ALL SELECT n.id,n.parent_id,n.kind,n.status,n.content_json,p.depth+1 FROM nodes n JOIN p ON p.parent_id=n.id) SELECT depth,id,kind,status,substr(content_json,1,100) FROM p ORDER BY depth LIMIT 30; SELECT count(*),sum(length(content_json)) FROM nodes;\"",
                "cagent.db",
            ),
            (
                "sqlite3 -readonly cagent.db \"SELECT ';' AS separator; SELECT (SELECT 1);\"",
                "cagent.db",
            ),
            ("sqlite3 -readonly cagent.db '.tables'", "cagent.db"),
            ("sqlite3 -readonly cagent.db '.tables user%'", "cagent.db"),
            ("sqlite3 -readonly cagent.db '.indexes users'", "cagent.db"),
            (
                "sqlite3 -readonly cagent.db '.schema --indent --nosys user%'",
                "cagent.db",
            ),
            (
                "sqlite3 -readonly cagent.db '.fullschema --indent'",
                "cagent.db",
            ),
            ("sqlite3 -readonly cagent.db '.databases'", "cagent.db"),
            ("sqlite3 -readonly cagent.db '.dbinfo main'", "cagent.db"),
            (
                "sqlite3 -readonly cagent.db '.dump --data-only --newlines --nosys --preserve-rowids users audit%'",
                "cagent.db",
            ),
            (
                "sqlite3 -readonly /tmp/cagent.db '.tables'",
                "/tmp/cagent.db",
            ),
        ] {
            let classification = classify_read_safe_shell(command).expect("safe SQLite form");
            assert_eq!(classification.paths, [path]);
        }
        for command in [
            "sqlite3 cagent.db 'SELECT 1'",
            "sqlite3 -readonly cagent.db",
            "sqlite3 -readonly cagent.db 'DELETE FROM users'",
            "sqlite3 -readonly cagent.db 'SELECT 1; DELETE FROM users'",
            "sqlite3 -readonly cagent.db 'PRAGMA table_info(users)'",
            "sqlite3 -readonly cagent.db 'SELECT 1;; SELECT 2'",
            "sqlite3 -readonly cagent.db 'SELECT 1;;'",
            "sqlite3 -readonly cagent.db ';'",
            "sqlite3 -readonly cagent.db 'SELECT 1 -- comment'",
            "sqlite3 -readonly cagent.db 'SELECT /* comment */ 1'",
            "sqlite3 -readonly cagent.db 'SELECT 1; /* comment */ SELECT 2'",
            "sqlite3 -readonly cagent.db 'WITH rows AS (SELECT 1) INSERT INTO users SELECT * FROM rows'",
            "sqlite3 -readonly cagent.db 'WITH rows AS (SELECT 1) UPDATE users SET id = 1'",
            "sqlite3 -readonly cagent.db 'WITH rows AS (SELECT 1) DELETE FROM users'",
            "sqlite3 -readonly cagent.db 'SELECT 1; WITH rows AS (SELECT 1) DELETE FROM users'",
            "sqlite3 -readonly cagent.db 'SELECT FROM users'",
            "sqlite3 cagent.db '.tables'",
            "sqlite3 -readonly cagent.db '.table'",
            "sqlite3 -readonly cagent.db '.tables one two'",
            "sqlite3 -readonly cagent.db '.tables --unknown'",
            "sqlite3 -readonly cagent.db '.indexes one two'",
            "sqlite3 -readonly cagent.db '.schema one two'",
            "sqlite3 -readonly cagent.db '.schema --indent --indent'",
            "sqlite3 -readonly cagent.db '.schema users --indent'",
            "sqlite3 -readonly cagent.db '.fullschema --nosys'",
            "sqlite3 -readonly cagent.db '.databases extra'",
            "sqlite3 -readonly cagent.db '.dbinfo one two'",
            "sqlite3 -readonly cagent.db '.dump --unknown'",
            "sqlite3 -readonly cagent.db '.dump users --data-only'",
            "sqlite3 -readonly cagent.db '.tables; SELECT 1'",
            "sqlite3 -readonly cagent.db '.tables\nSELECT 1'",
            "sqlite3 -readonly cagent.db '.tables\n.dump'",
            "sqlite3 -readonly cagent.db '.open other.db'",
            "sqlite3 -readonly cagent.db '.read commands.sql'",
            "sqlite3 -readonly cagent.db '.import input.csv users'",
            "sqlite3 -readonly cagent.db '.load extension.so'",
            "sqlite3 -readonly cagent.db '.shell id'",
            "sqlite3 -readonly cagent.db '.system id'",
            "sqlite3 -readonly cagent.db '.output out.sql'",
            "sqlite3 -readonly cagent.db '.once out.sql'",
            "sqlite3 -readonly cagent.db '.backup backup.db'",
            "sqlite3 -readonly cagent.db '.save copy.db'",
            "sqlite3 -readonly cagent.db '.restore backup.db'",
            "sqlite3 -readonly cagent.db '.archive -c out.zip'",
        ] {
            assert!(classify_read_safe_shell(command).is_none(), "{command}");
        }
    }

    #[test]
    fn literal_null_stderr_redirection_is_safe_but_other_targets_are_not() {
        assert!(classify_read_safe_shell("cat SPEC.md 2>/dev/null").is_some());
        assert!(classify_read_safe_shell("cat SPEC.md 2> /dev/null").is_some());
        assert!(classify_read_safe_shell("cat SPEC.md 2>&1").is_some());
        assert!(classify_read_safe_shell("cat SPEC.md 2>&2").is_some());
        assert!(classify_read_safe_shell("cat SPEC.md 2>&3").is_none());
        assert!(classify_read_safe_shell("cat SPEC.md 2>errors.log").is_none());
        assert!(classify_read_safe_shell("cat SPEC.md 2>/tmp/errors.log").is_none());
    }

    #[test]
    fn test_file_predicate_is_narrowly_read_safe() {
        let classification = classify_read_safe_shell("test -f SPEC.md").expect("safe test form");
        assert_eq!(classification.paths, ["SPEC.md"]);
        assert_eq!(
            classification.presentation,
            Some(SafeShellPresentation::Read)
        );

        for command in [
            "test -d SPEC.md",
            "test ! -f SPEC.md",
            "test -f SPEC.md extra",
            "test -f",
            "test SPEC.md",
            "test -f \"$(echo SPEC.md)\"",
        ] {
            assert!(classify_read_safe_shell(command).is_none(), "{command}");
        }
    }

    #[test]
    fn static_literal_for_loop_with_test_file_predicate_is_safe() {
        let command = r#"for f in SPEC.md TEST.md PHASES.md; do echo "== $f =="; test -f "$f"; wc -l "$f"; done"#;
        let classification =
            classify_read_safe_shell(command).expect("static loop with test should be safe");
        assert_eq!(classification.paths, ["PHASES.md", "SPEC.md", "TEST.md"]);

        let inventory = ShellCommandInventory::synthetic(&["echo", "test", "wc"]);
        assert!(authorize_available_safe_shell(command, &inventory).is_some());
    }

    #[test]
    fn static_loop_can_compare_working_copy_and_jj_file_checksums() {
        let command = r#"for f in crates/cagent-agent/src/prompts.rs crates/cagent-agent/src/tools/shell/classifiers/sed.rs crates/cagent-agent/src/tools/shell/safe.rs; do echo "$f"; sha256sum "$f"; jj --ignore-working-copy file show -r @ "$f" | sha256sum; done"#;
        let classification = classify_read_safe_shell(command).expect("checksum loop is read-safe");

        assert_eq!(
            classification.paths,
            [
                "crates/cagent-agent/src/prompts.rs",
                "crates/cagent-agent/src/tools/shell/classifiers/sed.rs",
                "crates/cagent-agent/src/tools/shell/safe.rs",
            ]
        );
        let inventory = ShellCommandInventory::synthetic(&["echo", "jj", "sha256sum"]);
        assert!(authorize_available_safe_shell(command, &inventory).is_some());
    }

    #[test]
    fn basename_suffix_form_is_safe_in_a_glob_loop_pipeline() {
        assert!(classify_read_safe_shell("basename file.md .md").is_some());
        assert!(classify_read_safe_shell("basename --suffix .md file.md").is_some());
        assert!(classify_read_safe_shell("basename --multiple a.md b.md").is_some());
        assert!(classify_read_safe_shell("basename --suffix .md a.md b.md").is_none());
        assert!(classify_read_safe_shell("basename a b c").is_none());

        let temporary = tempfile::tempdir().unwrap();
        let docs_old = temporary.path().join("docs-old");
        std::fs::create_dir(&docs_old).unwrap();
        for name in ["beta.md", "alpha.md"] {
            std::fs::write(docs_old.join(name), name).unwrap();
        }

        let command = r#"for f in docs-old/*.md; do basename "$f" .md; done | sort"#;
        let analysis = analyze_shell(command).expect("loop pipeline should parse");
        let request = super::super::BashRequest {
            command: command.into(),
            cwd: Some(temporary.path().to_path_buf()),
            env: BTreeMap::new(),
            forward_env: Vec::new(),
            timeout: None,
            wait: true,
        };
        let inventory = ShellCommandInventory::synthetic(&["basename", "sort"]);
        let authorization =
            authorize_available_safe_bash_segments(&request, &inventory, &analysis, 0, false)
                .expect("approved request environment should have a safe context");

        assert!(authorization.whole.is_none());
        assert_eq!(authorization.segments.len(), analysis.segments.len());
        for index in 0..analysis.segments.len() {
            assert!(authorization.segment(index).is_some());
        }
    }

    #[test]
    fn static_literal_for_loops_expand_quoted_values_for_safe_commands() {
        let command =
            r#"for f in SPEC.md TEST.md PHASES.md; do echo "== $f =="; ls -la "$f" 2>&1; done"#;
        let classification = classify_read_safe_shell(command).expect("static loop is safe");
        assert_eq!(classification.paths, ["PHASES.md", "SPEC.md", "TEST.md"]);
        assert!(classification.presentation.is_none());
        assert!(classification.exploration.is_none());

        let inventory = ShellCommandInventory::synthetic(&["echo", "ls"]);
        assert!(authorize_available_safe_shell(command, &inventory).is_some());
    }

    #[test]
    fn compound_requests_classify_safe_segments_independently() {
        let command = r#"ls && ls . && for f in SPEC.md TEST.md PHASES.md; do echo "== $f =="; ls -la "$f" 2>&1; done"#;
        let analysis = analyze_shell(command).expect("compound command should parse");
        let request = super::super::BashRequest {
            command: command.into(),
            cwd: None,
            env: BTreeMap::new(),
            forward_env: Vec::new(),
            timeout: Some(30),
            wait: true,
        };
        let inventory = ShellCommandInventory::synthetic(&["echo", "ls"]);
        let authorization =
            authorize_available_safe_bash_segments(&request, &inventory, &analysis, 3, false)
                .expect("approved request environment should have a safe context");

        assert!(authorization.whole.is_none());
        assert_eq!(authorization.segments.len(), analysis.segments.len());
        for (index, segment) in analysis.segments.iter().enumerate() {
            assert!(
                authorization.segment(index).is_some(),
                "segment {} was not classified safe: {}",
                index,
                segment.raw
            );
        }

        let unsafe_command = "ls && rm -f TEST.md";
        let unsafe_analysis = analyze_shell(unsafe_command).expect("mixed command should parse");
        let unsafe_request = super::super::BashRequest {
            command: unsafe_command.into(),
            cwd: None,
            env: BTreeMap::new(),
            forward_env: Vec::new(),
            timeout: Some(30),
            wait: true,
        };
        let unsafe_authorization = authorize_available_safe_bash_segments(
            &unsafe_request,
            &inventory,
            &unsafe_analysis,
            3,
            false,
        )
        .expect("approved request environment should have a safe context");
        assert!(unsafe_authorization.segment(0).is_some());
        assert!(unsafe_authorization.segment(1).is_none());
    }

    #[test]
    fn mixed_build_and_migration_retains_only_build_authorization() {
        let inventory = ShellCommandInventory::synthetic(&["pnpm"]);
        for command in [
            "pnpm run build && pnpm run migrate",
            "pnpm --filter @logaway/api build && pnpm --filter @logaway/api db:migrate:production",
            "pnpm run build; pnpm run migrate",
        ] {
            let request = super::super::BashRequest {
                command: command.into(),
                cwd: None,
                env: BTreeMap::new(),
                forward_env: Vec::new(),
                timeout: None,
                wait: true,
            };
            let analysis = analyze_shell(command).unwrap();
            for level in 0..=3 {
                let authorization = authorize_available_safe_bash_segments(
                    &request, &inventory, &analysis, level, false,
                )
                .unwrap();
                assert_eq!(
                    authorization.allows_segment(0, level, false),
                    level >= 2,
                    "{command}"
                );
                assert!(!authorization.allows_segment(1, level, false), "{command}");
            }
        }
    }

    #[test]
    fn mixed_pipeline_retains_safe_filter_authorization() {
        let command = "custom-command | sort -nr | head -n 45";
        let analysis = analyze_shell(command).expect("pipeline should parse");
        let request = super::super::BashRequest {
            command: command.into(),
            cwd: None,
            env: BTreeMap::new(),
            forward_env: Vec::new(),
            timeout: Some(30),
            wait: true,
        };
        let inventory = ShellCommandInventory::synthetic(&["sort", "head"]);
        let authorization =
            authorize_available_safe_bash_segments(&request, &inventory, &analysis, 0, false)
                .expect("inventory should provide a safe-command context");

        assert!(authorization.whole.is_none());
        assert!(!authorization.allows_segment(0, 0, false));
        assert!(authorization.allows_segment(1, 0, false));
        assert!(authorization.allows_segment(2, 0, false));
    }

    #[test]
    fn static_for_loops_reject_dynamic_values_and_expansions() {
        let inventory = ShellCommandInventory::synthetic(&["ls"]);
        for command in [
            "for f in SPEC.md \"$OTHER\"; do ls \"$f\"; done",
            "for f in SPEC.md; do ls $f; done",
            "for f in SPEC.md; do while true; do ls \"$f\"; done; done",
            "for f in SPEC.md; do checked=1; ls \"$f\"; done",
            "for f in SPEC.md; do if true; then ls \"$f\"; fi; done",
        ] {
            assert!(classify_read_safe_shell(command).is_none(), "{command}");
            // Runtime context must not make a dynamically expanded loop
            // eligible for the read-safe bypass. It falls through to the
            // ordinary Bash approval path instead.
            assert!(
                authorize_available_safe_shell(command, &inventory).is_none(),
                "{command}"
            );
        }
        assert!(classify_read_safe_shell("for f in *.md; do ls \"$f\"; done").is_none());
    }

    #[test]
    fn static_for_loops_expand_comma_braces() {
        let command = "for file in {SPEC,AGENTS}.md; do ls \"$file\"; done";
        let classification =
            classify_read_safe_shell(command).expect("comma brace loop should be safe");
        assert_eq!(classification.paths, ["AGENTS.md", "SPEC.md"]);
        assert_eq!(
            classification
                .operands
                .iter()
                .map(|operand| operand.value.as_str())
                .collect::<Vec<_>>(),
            ["SPEC.md", "AGENTS.md"]
        );
        let inventory = ShellCommandInventory::synthetic(&["ls"]);
        assert!(authorize_available_safe_shell(command, &inventory).is_some());
        assert!(
            classify_read_safe_shell("for file in \"{SPEC,AGENTS}.md\"; do ls \"$file\"; done")
                .is_some()
        );
    }

    #[test]
    fn static_for_loops_have_a_bounded_literal_value_list() {
        let values = (0..65)
            .map(|index| format!("file-{index}.md"))
            .collect::<Vec<_>>()
            .join(" ");
        let command = format!("for f in {values}; do ls \"$f\"; done");
        assert!(classify_read_safe_shell(&command).is_none());
    }

    #[test]
    fn runtime_static_for_globs_expand_and_check_each_match() {
        let temporary = tempfile::tempdir().unwrap();
        for name in ["b.md", "a.md", ".hidden.md", "notes.txt"] {
            std::fs::write(temporary.path().join(name), name).unwrap();
        }
        let inventory = ShellCommandInventory::synthetic(&["ls"]);
        let request = super::super::BashRequest {
            command: "for file in *.md; do ls \"$file\"; done".into(),
            cwd: Some(temporary.path().to_path_buf()),
            env: BTreeMap::new(),
            forward_env: Vec::new(),
            timeout: None,
            wait: true,
        };

        let classification = authorize_available_safe_bash_request(&request, &inventory)
            .expect("runtime loop glob should be safe");
        assert_eq!(classification.paths, ["a.md", "b.md"]);
        assert_eq!(
            classification
                .operands
                .iter()
                .map(|operand| operand.value.as_str())
                .collect::<Vec<_>>(),
            ["a.md", "b.md"]
        );
    }

    #[test]
    fn runtime_static_for_globs_follow_bash_matching_and_unmatched_rules() {
        let temporary = tempfile::tempdir().unwrap();
        for name in ["a.md", "b.md", ".hidden.md"] {
            std::fs::write(temporary.path().join(name), name).unwrap();
        }
        let inventory = ShellCommandInventory::synthetic(&["ls"]);
        let context = SafeShellContext::new(
            temporary.path().to_path_buf(),
            inventory.backend.clone(),
            BTreeMap::new(),
        );

        let multiple = parse_safe_shell_with_context(
            "for file in b*.md [ab].md; do ls \"$file\"; done",
            Some(&inventory),
            Some(&context),
        )
        .expect("multiple loop globs should be safe");
        assert_eq!(multiple.paths, ["a.md", "b.md"]);
        assert_eq!(
            multiple
                .operands
                .iter()
                .map(|operand| operand.value.as_str())
                .collect::<Vec<_>>(),
            ["b.md", "a.md", "b.md"]
        );

        let dotfiles = parse_safe_shell_with_context(
            "for file in .*.md; do ls \"$file\"; done",
            Some(&inventory),
            Some(&context),
        )
        .expect("dotfile loop glob should be safe");
        assert_eq!(dotfiles.paths, [".hidden.md"]);

        let unmatched = parse_safe_shell_with_context(
            "for file in missing*.md; do ls \"$file\"; done",
            Some(&inventory),
            Some(&context),
        )
        .expect("unmatched loop glob should remain a literal");
        assert_eq!(unmatched.paths, ["missing*.md"]);
        assert_eq!(unmatched.operands[0].value, "missing*.md");

        let quoted = parse_safe_shell_with_context(
            "for file in \"*.md\"; do ls \"$file\"; done",
            Some(&inventory),
            Some(&context),
        )
        .expect("quoted loop glob should remain a literal");
        assert_eq!(quoted.paths, ["*.md"]);
    }

    #[test]
    fn runtime_static_for_globs_are_bounded_across_the_whole_value_list() {
        let temporary = tempfile::tempdir().unwrap();
        for index in 0..65 {
            std::fs::write(temporary.path().join(format!("file-{index:02}.md")), "file").unwrap();
        }
        let inventory = ShellCommandInventory::synthetic(&["ls"]);
        let context = SafeShellContext::new(
            temporary.path().to_path_buf(),
            inventory.backend.clone(),
            BTreeMap::new(),
        );

        assert!(
            parse_safe_shell_with_context(
                "for file in *.md; do ls \"$file\"; done",
                Some(&inventory),
                Some(&context),
            )
            .is_err()
        );
    }

    #[test]
    fn rg_file_globs_are_retained_in_argument_order() {
        let classification = classify_read_safe_shell(
            "rg --files -g 'SPEC.md' --glob AGENTS.md --glob=Cargo.toml -g '*.rs'",
        )
        .unwrap();
        let activity = &classification.exploration.unwrap()[0];

        assert_eq!(
            activity.targets,
            ["SPEC.md", "AGENTS.md", "Cargo.toml", "*.rs"]
        );
        assert_eq!(activity.paths, ["."]);
    }

    #[test]
    fn rg_requires_standalone_double_dash_for_leading_dash_patterns() {
        let classification =
            classify_read_safe_shell("rg -n -i -- '--prompt|prompt' SPEC.md").unwrap();
        let activity = &classification.exploration.unwrap()[0];

        assert_eq!(activity.query.as_deref(), Some("--prompt|prompt"));
        assert_eq!(activity.paths, ["SPEC.md"]);
        assert_eq!(activity.presentation, SafeShellPresentation::Search);

        let single_dash = classify_read_safe_shell("rg -n -- '-prompt|prompt' SPEC.md").unwrap();
        assert_eq!(
            single_dash.exploration.unwrap()[0].query.as_deref(),
            Some("-prompt|prompt")
        );

        let quoted = classify_read_safe_shell(
            "rg -n \"--surface-base|surface-base\" opencode/packages/ui/src/v2 opencode/packages/app/src/index.css opencode/packages/app/src --glob '*.{css,ts,tsx}'",
        )
            .expect("a quoted leading-dash pattern is normalized for ripgrep");
        assert_eq!(
            quoted.exploration.unwrap()[0].query.as_deref(),
            Some("--surface-base|surface-base")
        );
        let hardened = quoted.hardened_command.expect("normalized rg command");
        assert!(hardened.contains("'--glob' '*.{css,ts,tsx}' '--' '--surface-base|surface-base'"));
        assert!(hardened.ends_with("'opencode/packages/app/src'"));

        assert!(classify_read_safe_shell("rg -n --prompt SPEC.md").is_none());
        assert!(
            classify_read_safe_shell("rg -n '--prompt' SPEC.md && pwd").is_none(),
            "compound commands cannot be safely rewritten"
        );
    }

    #[test]
    fn cargo_fmt_check_and_plain_jj_are_not_level_zero() {
        for command in [
            "cargo fmt --check",
            "cargo fmt --all --check",
            "jj diff",
            "jj show --stat @-",
        ] {
            assert!(classify_read_safe_shell(command).is_none(), "{command}");
        }
        assert!(classify_read_safe_shell("cargo fmt").is_none());
        assert!(classify_read_safe_shell("jj log -T").is_none());
    }

    #[test]
    fn rg_context_value_is_not_presented_as_the_search_query() {
        let classification = classify_read_safe_shell(
            r#"rg -n -C 8 "mode_color|StatusLineValues" crates/cagent-agent/src/presentation/statusline.rs crates/cagent-cli/src/render/mod.rs"#,
        )
        .unwrap();
        let activity = &classification.exploration.unwrap()[0];

        assert_eq!(
            activity.query.as_deref(),
            Some("mode_color|StatusLineValues")
        );
        assert_eq!(
            activity.paths,
            [
                "crates/cagent-agent/src/presentation/statusline.rs",
                "crates/cagent-cli/src/render/mod.rs",
            ]
        );
    }

    #[test]
    fn read_only_command_arguments_cover_common_inspection_forms() {
        for command in [
            "cat --help",
            "cat -vet SPEC.md",
            "cat -v -e -t SPEC.md",
            "git --help",
            "rg --stats --heading --max-columns 160 --threads 4 needle src",
            "rg --type-list",
            "grep --line-buffered --exclude-dir target --label source TODO .",
            "find src -depth -xdev -printf '%p\\n'",
            "find . -prune",
            "fd --color always --threads 4 --max-results 20 --ignore-file .ignore rs src",
            "ls --format=long --sort=time --time-style=long-iso --width=120 src",
            "ls --hyperlink=auto src",
            "paste -z -d, left right",
            "tail -f diagnostic.log",
            "tar -tvf archive.tar",
            "unzip -t archive.zip",
            "sqlite3 -readonly -header -column cagent.db 'SELECT name FROM sqlite_master'",
            "tree -L 3 crates",
            "rustc --version",
            "node --version",
            "python3 --version",
            "readelf -h target/debug/cagent",
            "nm --demangle target/debug/cagent",
            "objdump -h target/debug/cagent",
        ] {
            assert!(
                classify_read_safe_shell(command).is_some(),
                "expected read-safe: {command}"
            );
        }
    }

    #[test]
    fn selected_codex_compatible_git_flags_remain_hardened() {
        for command in [
            "git --no-pager status --ahead-behind",
            "git status --no-ahead-behind",
            "git diff --no-ext-diff --no-textconv",
            "git show --no-ext-diff --no-textconv HEAD",
            "git ls-files --cached -- crates",
            "git grep -n TODO -- crates",
            "git tag --list --contains HEAD",
            "git stash list --oneline",
            "git worktree list --porcelain",
        ] {
            let classification = classify_read_safe_shell(command)
                .unwrap_or_else(|| panic!("expected read-safe Git form: {command}"));
            let hardened = classification
                .hardened_command
                .unwrap_or_else(|| panic!("expected hardened Git command: {command}"));
            assert!(hardened.contains("GIT_TERMINAL_PROMPT=0"));
        }
    }

    #[test]
    fn additional_inspection_forms_reject_execution_and_unbounded_output() {
        for command in [
            "tree crates",
            "tree -L 9 crates",
            "git grep --textconv TODO -- crates",
            "git ls-files --recurse-submodules",
            "cargo metadata --format-version 2",
            "readelf --debug-dump=info target/debug/cagent",
            "objdump --dwarf target/debug/cagent",
        ] {
            assert!(
                classify_read_safe_shell(command).is_none(),
                "expected rejection: {command}"
            );
        }
    }

    #[test]
    fn read_only_arguments_that_can_mutate_or_escape_remain_rejected() {
        for command in [
            "df --sync .",
            "file --preserve-date Cargo.toml",
            "file -p Cargo.toml",
            "rg --follow needle .",
            "fd --follow needle .",
            "find -L . -name '*.rs'",
            "ls -RL .",
            "npm --version",
            "go version",
            "java -version",
            "cargo locate-project",
            "cargo locate-project --workspace",
            "cargo metadata --no-deps --format-version 1",
            "find . -exec cat {} \\;",
            "tar -xf archive.tar",
            "sqlite3 cagent.db 'DELETE FROM nodes'",
        ] {
            assert!(
                classify_read_safe_shell(command).is_none(),
                "expected rejection: {command}"
            );
        }
    }

    #[test]
    fn rg_smart_case_short_flag_is_read_safe() {
        let classification = classify_read_safe_shell(
            r#"rg -n -S "fn (render|draw)|scroll|selected|selectable|KeyCode::(Up|Down|PageUp|PageDown)|↑|↓" crates/cagent-cli/src/app crates/cagent-cli/src/main.rs --glob '*.rs'"#,
        )
        .expect("-S is ripgrep's read-only smart-case flag");
        let activity = &classification.exploration.unwrap()[0];

        assert_eq!(
            activity.query.as_deref(),
            Some(
                "fn (render|draw)|scroll|selected|selectable|KeyCode::(Up|Down|PageUp|PageDown)|↑|↓"
            )
        );
        assert_eq!(
            activity.paths,
            ["crates/cagent-cli/src/app", "crates/cagent-cli/src/main.rs",]
        );
    }

    #[test]
    fn numeric_print_only_sed_is_read_safe_as_a_pipeline_filter() {
        let classification = classify_read_safe_shell("cat README.md | sed -n '1,240p'").unwrap();

        assert_eq!(classification.paths, ["README.md"]);
        assert_eq!(classification.exploration.as_ref().map(Vec::len), Some(1));
        assert_eq!(
            classification.exploration.unwrap()[0].presentation,
            SafeShellPresentation::Read
        );
    }

    #[test]
    fn multiple_numeric_print_sed_ranges_are_read_safe() {
        let command = "sed -n '1010,1040p;1195,1265p'";
        let classification = classify_read_safe_shell(command).expect("safe sed filter");

        assert!(classification.paths.is_empty());
        assert!(classification.presentation.is_none());
        assert!(classification.exploration.is_none());
        assert!(
            authorize_available_safe_shell(command, &ShellCommandInventory::synthetic(&["sed"]))
                .is_some()
        );
    }

    #[test]
    fn numeric_print_only_sed_with_multiple_files_is_read_safe() {
        let command = "sed -n '430,525p;1820,1880p;2460,2520p;2800,2870p' crates/cagent-cli/src/render/activity.rs crates/cagent-cli/src/render/mod.rs";
        let classification = classify_read_safe_shell(command).expect("safe multi-file sed read");

        assert_eq!(
            classification.paths,
            [
                "crates/cagent-cli/src/render/activity.rs",
                "crates/cagent-cli/src/render/mod.rs",
            ]
        );
        assert_eq!(
            classification.presentation,
            Some(SafeShellPresentation::Read)
        );
        assert_eq!(classification.exploration.as_ref().map(Vec::len), Some(1));
    }

    #[test]
    fn unfiltered_rg_file_listing_keeps_the_current_directory_as_its_target() {
        let activity = &classify_read_safe_shell("rg --files")
            .unwrap()
            .exploration
            .unwrap()[0];

        assert!(activity.targets.is_empty());
        assert_eq!(activity.paths, ["."]);
    }

    #[test]
    fn cd_directory_state_is_structured_and_or_transitions_fall_back() {
        assert_eq!(
            classify_read_safe_shell("cd subdirectory && cat file")
                .unwrap()
                .paths,
            ["subdirectory", "subdirectory/file"]
        );
        assert_eq!(
            classify_read_safe_shell("cd crates && cat cagent-agent/Cargo.toml")
                .unwrap()
                .paths,
            ["crates", "crates/cagent-agent/Cargo.toml"]
        );
        assert_eq!(
            classify_read_safe_shell("cd crates | cat Cargo.toml")
                .unwrap()
                .paths,
            ["Cargo.toml", "crates"]
        );
        assert!(
            classify_read_safe_shell("cd missing || cat Cargo.toml").is_none(),
            "a failed cd leaves the right side in an ambiguous directory"
        );
    }

    #[test]
    fn literal_cd_and_generic_echo_authorize_in_compounds() {
        let inventory = ShellCommandInventory::synthetic(&["cd", "cat", "echo"]);
        let command = "cd /workspace && cat TEST.md && echo \"===PHASES full===\"";
        let classification = authorize_available_safe_shell(command, &inventory)
            .expect("all available literal segments are authorization-safe");

        assert_eq!(classification.paths, ["/workspace", "/workspace/TEST.md"]);
        assert!(classification.exploration.is_none());
    }

    #[test]
    fn guidance_tracks_search_inventory_and_fd_alias() {
        let both = safe_bash_guidance(
            &ShellCommandInventory::synthetic(&["rg", "grep", "fd", "find"]),
            0,
        );
        assert!(both.contains("Prefer rg over grep"));
        assert!(both.contains("Prefer fd over find"));
        assert!(both.contains("quoting alone does not stop option parsing"));
        assert!(both.contains("rg -n -i -- '--prompt|prompt' SPEC.md"));
        assert!(both.contains("Shell quoting: single-quote static patterns"));
        assert!(both.contains("Double quotes still allow command substitution"));
        assert!(both.contains("rg -n 'Press `r`' file, not rg -n \"Press `r`\" file"));

        let fallbacks = safe_bash_guidance(&ShellCommandInventory::synthetic(&["grep", "find"]), 0);
        assert!(fallbacks.contains("Use grep"));
        assert!(fallbacks.contains("Use find"));
        assert!(!fallbacks.contains("rg:"));
        assert!(!fallbacks.contains("Prefer rg"));
        assert!(!fallbacks.contains("fd:"));
        assert!(!fallbacks.contains("Prefer fd"));

        let debian = safe_bash_guidance(&ShellCommandInventory::synthetic(&["fdfind", "find"]), 0);
        assert!(debian.contains("Prefer fdfind over find"));
        assert!(debian.contains("fdfind PATTERN src"));
        assert!(!debian.contains("Prefer fd over"));

        let neither = safe_bash_guidance(&ShellCommandInventory::synthetic(&[]), 0);
        assert!(!neither.contains("searching file contents"));
        assert!(!neither.contains("locating files"));

        let without_sqlite = safe_bash_guidance(&ShellCommandInventory::synthetic(&["cat"]), 0);
        assert!(!without_sqlite.contains("sqlite3"));
        assert!(without_sqlite.contains("bounded filesystem globs expanded at runtime"));
        let with_sqlite = safe_bash_guidance(&ShellCommandInventory::synthetic(&["sqlite3"]), 0);
        assert!(with_sqlite.contains("sqlite3 -readonly cagent.db"));
    }

    #[test]
    fn rg_literal_backticks_require_single_quotes() {
        let single_quoted = r#"rg -n 'muted|highlight|ui\.colors|`ui\.colors`' SPEC.md"#;
        let double_quoted = r#"rg -n "muted|highlight|ui\.colors|`ui\.colors`" SPEC.md"#;

        assert!(classify_read_safe_shell(single_quoted).is_some());
        assert!(classify_read_safe_shell(double_quoted).is_none());
    }

    #[test]
    fn git_safe_forms_supply_hardened_execution() {
        let diff = classify_read_safe_shell("git diff -- Cargo.toml").unwrap();
        assert!(diff.requires_git_hardening);
        let command = diff.hardened_command.unwrap();
        assert!(command.contains("--no-ext-diff"));
        assert!(command.contains("--no-textconv"));
        assert!(classify_read_safe_shell("git diff && pwd").is_none());

        let mut inventory = ShellCommandInventory::synthetic(&["git"]);
        inventory
            .commands
            .get_mut("git")
            .unwrap()
            .hardening_available = false;
        assert!(authorize_available_safe_shell("git status", &inventory).is_none());
    }

    #[test]
    fn runtime_context_allows_path_globs_but_not_option_or_query_globs() {
        let temporary = tempfile::tempdir().unwrap();
        for name in ["one.md", "two.md", "a.rs", "b.rs"] {
            std::fs::write(temporary.path().join(name), name).unwrap();
        }
        std::fs::create_dir(temporary.path().join("src")).unwrap();
        std::fs::write(temporary.path().join("src/a.rs"), "a").unwrap();
        std::fs::write(temporary.path().join("src/b.rs"), "b").unwrap();
        let inventory = ShellCommandInventory::synthetic(&["cat", "rg"]);
        let context = SafeShellContext::new(
            temporary.path().to_path_buf(),
            inventory.backend.clone(),
            BTreeMap::new(),
        );

        let cat = parse_safe_shell_with_context("cat *.md", Some(&inventory), Some(&context))
            .expect("path glob should be safe in a cat operand");
        assert_eq!(cat.paths, ["one.md", "two.md"]);
        let brace =
            parse_safe_shell_with_context("cat {one,two}.md", Some(&inventory), Some(&context))
                .expect("comma brace expansion should be safe in a cat operand");
        assert_eq!(brace.paths, ["one.md", "two.md"]);
        let brace_glob =
            parse_safe_shell_with_context("cat {one,two}.*", Some(&inventory), Some(&context))
                .expect("brace and pathname expansion should compose in an operand");
        assert_eq!(brace_glob.paths, ["one.md", "two.md"]);
        let rg = parse_safe_shell_with_context(
            "rg -n ShellExecutor *.rs",
            Some(&inventory),
            Some(&context),
        )
        .expect("path glob should be safe in an rg operand");
        assert_eq!(rg.paths, ["a.rs", "b.rs"]);
        let rg_brace = parse_safe_shell_with_context(
            "rg -n -C 2 ShellExecutor {a.rs,b.rs}",
            Some(&inventory),
            Some(&context),
        )
        .expect("comma brace expansion should be safe in an rg operand");
        assert_eq!(rg_brace.paths, ["a.rs", "b.rs"]);
        let request = super::super::BashRequest {
            command: "rg -n -C 2 ShellExecutor {a.rs,b.rs}".into(),
            cwd: Some(temporary.path().to_path_buf()),
            env: BTreeMap::new(),
            forward_env: Vec::new(),
            timeout: None,
            wait: true,
        };
        let authorized = authorize_available_safe_bash_request(&request, &inventory)
            .expect("available Bash authorization should expand filesystem braces");
        assert_eq!(authorized.paths, ["a.rs", "b.rs"]);
        let find_inventory = ShellCommandInventory::synthetic(&["find"]);
        let find_context = SafeShellContext::new(
            temporary.path().to_path_buf(),
            find_inventory.backend.clone(),
            BTreeMap::new(),
        );
        let find = parse_safe_shell_with_context(
            "find src/* -type f",
            Some(&find_inventory),
            Some(&find_context),
        )
        .expect("find roots are filesystem operands");
        assert_eq!(find.paths, ["src/a.rs", "src/b.rs"]);
        let bracket =
            parse_safe_shell_with_context("cat [ab].rs", Some(&inventory), Some(&context))
                .expect("bracket path glob should be safe");
        assert_eq!(bracket.paths, ["a.rs", "b.rs"]);
        let no_match =
            parse_safe_shell_with_context("cat missing*.md", Some(&inventory), Some(&context))
                .expect("Bash keeps an unmatched glob literal");
        assert_eq!(no_match.paths, ["missing*.md"]);
        let mut bounded = context.clone();
        bounded.max_glob_matches = 1;
        assert!(
            parse_safe_shell_with_context("cat *.md", Some(&inventory), Some(&bounded)).is_err()
        );
        assert!(
            parse_safe_shell_with_context("rg --glob *.rs .", Some(&inventory), Some(&context),)
                .is_err()
        );
        assert!(
            parse_safe_shell_with_context(
                "rg Shell{Executor,Agent} .",
                Some(&inventory),
                Some(&context)
            )
            .is_err()
        );
        parse_safe_shell_with_context("rg --glob \"*.rs\" .", Some(&inventory), Some(&context))
            .unwrap_or_else(|error| panic!("quoted glob should be safe: {error}"));
    }

    #[test]
    fn filesystem_globs_apply_to_wc_and_other_registered_path_operands() {
        let temporary = tempfile::tempdir().unwrap();
        for path in [
            "crates/cagent-cli/src/main.rs",
            "crates/cagent-cli/src/app/mod.rs",
            "crates/cagent-cli/src/render/mod.rs",
        ] {
            let path = temporary.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "fn main() {}\n").unwrap();
        }
        let inventory =
            ShellCommandInventory::synthetic(&["wc", "cat", "stat", "head", "find", "rg"]);
        let context = SafeShellContext::new(
            temporary.path().to_path_buf(),
            inventory.backend.clone(),
            BTreeMap::new(),
        );

        let command = "wc -l crates/*cli*/src/*.rs crates/*cli*/src/**/*.rs";
        let classification =
            parse_safe_shell_with_context(command, Some(&inventory), Some(&context))
                .expect("wc filesystem operands should support bounded globs");
        assert_eq!(
            classification.paths,
            [
                "crates/cagent-cli/src/app/mod.rs",
                "crates/cagent-cli/src/main.rs",
                "crates/cagent-cli/src/render/mod.rs",
            ]
        );

        for command in [
            "cat crates/*cli*/src/*.rs",
            "stat crates/*cli*/src/*.rs",
            "head crates/*cli*/src/*.rs",
            "find crates/*cli*/src -type f",
            "rg main crates/*cli*/src/*.rs",
        ] {
            parse_safe_shell_with_context(command, Some(&inventory), Some(&context))
                .unwrap_or_else(|error| panic!("{command}: {error}"));
        }
    }

    #[test]
    fn path_list_producers_can_supply_outer_filesystem_operands() {
        let temporary = tempfile::tempdir().unwrap();
        std::fs::create_dir(temporary.path().join("src")).unwrap();
        for name in ["a.rs", "with space.rs", "star*.rs"] {
            std::fs::write(temporary.path().join("src").join(name), "x\n").unwrap();
        }
        let inventory =
            ShellCommandInventory::synthetic(&["wc", "fd", "rg", "find", "sort", "head", "cat"]);
        let context = SafeShellContext::new(
            temporary.path().to_path_buf(),
            inventory.backend.clone(),
            BTreeMap::new(),
        );

        let command = "wc -l $(fd -e rs . src) | sort -nr | head -n 45";
        let classification =
            parse_safe_shell_with_context(command, Some(&inventory), Some(&context))
                .expect("fd path output should safely supply wc operands");
        assert!(classification.paths.contains(&"src/a.rs".into()));
        assert!(classification.paths.contains(&"src/with".into()));
        assert!(classification.paths.contains(&"space.rs".into()));
        assert!(
            classification
                .covered_nested_segments
                .contains(&"fd -e rs . src".into())
        );

        for command in [
            "wc -l $(rg --files src)",
            "wc -l $(find src -type f -print)",
        ] {
            parse_safe_shell_with_context(command, Some(&inventory), Some(&context))
                .unwrap_or_else(|error| panic!("{command}: {error}"));
        }

        for command in [
            "wc -l \"$(fd -e rs . src)\"",
            "wc -l prefix$(fd -e rs . src)",
            "wc -l $(cat src/a.rs)",
            "wc -l $(fd --format '{/}' . src)",
            "wc -l $(fd definitely-no-match src)",
            "wc -l $(fd --list-details . src)",
            "wc -l $(rg --files --null src)",
            "wc -l $(find src -printf '%f\\n')",
        ] {
            assert!(
                parse_safe_shell_with_context(command, Some(&inventory), Some(&context)).is_err(),
                "{command}"
            );
        }
    }

    #[test]
    fn bash_request_authorizes_rg_path_globs_in_filesystem_operands() {
        let temporary = tempfile::tempdir().unwrap();
        for path in [
            "crates/cagent-cli/src/app/tests.rs",
            "crates/cagent-cli/src/render/tests.rs",
            "crates/cagent-agent/src/presentation/permissions.rs",
            "crates/cagent-agent/tests/public_api.rs",
        ] {
            let path = temporary.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "permission_surface\n").unwrap();
        }
        let inventory = ShellCommandInventory::synthetic(&["rg"]);
        let command = "rg -n 'permission_(surface|approval|denied)|external|auto review|Auto review|Permission denied|Always allow|Allow reading' crates/cagent-cli/src/**/*tests.rs crates/cagent-agent/src/presentation/permissions.rs crates/cagent-agent/tests";
        let request = super::super::BashRequest {
            command: command.into(),
            cwd: Some(temporary.path().to_path_buf()),
            env: BTreeMap::new(),
            forward_env: Vec::new(),
            timeout: None,
            wait: true,
        };

        let analysis = super::super::analyze_shell(command).unwrap();
        let authorization =
            authorize_available_safe_bash_segments(&request, &inventory, &analysis, 0, false)
                .expect("rg path globs should be authorized at the Bash request boundary");
        let authorized = authorization
            .whole
            .expect("the complete rg request should be read-safe");

        assert_eq!(
            authorized.paths,
            [
                "crates/cagent-agent/src/presentation/permissions.rs",
                "crates/cagent-agent/tests",
                "crates/cagent-cli/src/app/tests.rs",
                "crates/cagent-cli/src/render/tests.rs",
            ]
        );
        let activity = &authorized.exploration.unwrap()[0];
        assert_eq!(
            activity.query.as_deref(),
            Some(
                "permission_(surface|approval|denied)|external|auto review|Auto review|Permission denied|Always allow|Allow reading"
            )
        );
        assert_eq!(
            activity.paths,
            [
                "crates/cagent-cli/src/app/tests.rs",
                "crates/cagent-cli/src/render/tests.rs",
                "crates/cagent-agent/src/presentation/permissions.rs",
                "crates/cagent-agent/tests",
            ]
        );
    }

    #[test]
    fn exploration_classification_preserves_path_globs_without_widening_static_safety() {
        let command = "rg -n TranscriptBlock crates/cagent-cli/src/app/*.rs";
        assert!(classify_read_safe_shell(command).is_none());

        let classification =
            classify_shell_exploration(command).expect("path glob has presentable read intent");
        let activity = &classification.exploration.unwrap()[0];
        assert_eq!(activity.presentation, SafeShellPresentation::Search);
        assert_eq!(activity.query.as_deref(), Some("TranscriptBlock"));
        assert_eq!(activity.paths, ["crates/cagent-cli/src/app/*.rs"]);

        for command in [
            "rg TranscriptBlock* .",
            "rg --glob *.rs TranscriptBlock .",
            "cat \"$(printf README.md)\"",
        ] {
            assert!(classify_shell_exploration(command).is_none(), "{command}");
        }
    }

    #[test]
    fn runtime_context_handles_approved_variables_and_caret_regexes() {
        let temporary = tempfile::tempdir().unwrap();
        std::fs::write(temporary.path().join("README.md"), "README").unwrap();
        let inventory = ShellCommandInventory::synthetic(&["cat", "rg"]);
        let variables = APPROVED_PATH_VARIABLES
            .iter()
            .map(|name| {
                (
                    (*name).into(),
                    temporary.path().to_string_lossy().into_owned(),
                )
            })
            .collect();
        let context = SafeShellContext::new(
            temporary.path().to_path_buf(),
            inventory.backend.clone(),
            variables,
        );
        for name in APPROVED_PATH_VARIABLES {
            let command = format!("cat \"${name}/README.md\"");
            assert!(
                parse_safe_shell_with_context(&command, Some(&inventory), Some(&context)).is_ok(),
                "{command}"
            );
        }
        let home_target = temporary.path().join("worktree-test/test-git");
        let tilde = parse_safe_shell_with_context(
            "ls ~/worktree-test/test-git",
            Some(&ShellCommandInventory::synthetic(&["ls"])),
            Some(&context),
        )
        .expect("bare-home tilde should expand from the runtime HOME");
        assert_eq!(tilde.paths, [home_target.to_string_lossy()]);
        assert!(
            parse_safe_shell_with_context(
                "ls ~other/worktree-test/test-git",
                Some(&ShellCommandInventory::synthetic(&["ls"])),
                Some(&context),
            )
            .is_err()
        );
        assert!(
            parse_safe_shell_with_context(
                "rg -n ^ShellExecutor .",
                Some(&inventory),
                Some(&context),
            )
            .is_ok()
        );
        assert!(
            parse_safe_shell_with_context(
                "rg -n \"^ShellExecutor|^TerminalSupervisor\" .",
                Some(&inventory),
                Some(&context),
            )
            .is_ok()
        );
        assert!(
            parse_safe_shell_with_context(
                "cat \"$PATH/README.md\"",
                Some(&inventory),
                Some(&context),
            )
            .is_err()
        );
    }

    #[test]
    fn deterministic_nested_output_is_proven_once() {
        let inventory = ShellCommandInventory::synthetic(&["cat", "printf", "echo"]);
        let context = SafeShellContext::new(
            PathBuf::from("."),
            inventory.backend.clone(),
            BTreeMap::new(),
        );
        for (source, nested) in [
            ("cat \"$(printf README.md)\"", "printf README.md"),
            ("cat \"$(echo README.md)\"", "echo README.md"),
        ] {
            let classification =
                parse_safe_shell_with_context(source, Some(&inventory), Some(&context))
                    .expect(source);
            assert_eq!(classification.paths, ["README.md"]);
            assert_eq!(classification.covered_nested_segments, [nested]);
        }
        for source in [
            "cat \"$(cat README.md)\"",
            "cat \"$(printf \"$UNKNOWN\")\"",
            "cat \"$(printf '*.md')\"",
            "cat `printf README.md`",
            "cat \"$(printf README.md | head)\"",
            "echo $\"literal\"",
        ] {
            assert!(
                parse_safe_shell_with_context(source, Some(&inventory), Some(&context)).is_err(),
                "{source}"
            );
        }
    }

    #[test]
    fn request_environment_overlays_only_approved_path_variables() {
        let inventory = ShellCommandInventory::synthetic(&["cat"]);
        let mut request = super::super::BashRequest {
            command: "cat \"$HOME/README.md\"".into(),
            cwd: None,
            env: BTreeMap::new(),
            forward_env: Vec::new(),
            timeout: None,
            wait: true,
        };
        request.env.insert("HOME".into(), "/tmp/project".into());
        assert!(authorize_available_safe_bash_request(&request, &inventory).is_some());
        request.command = "cat ~/README.md".into();
        let tilde = authorize_available_safe_bash_request(&request, &inventory)
            .expect("request HOME should authorize a bare-home tilde operand");
        assert_eq!(tilde.paths, ["/tmp/project/README.md"]);
        request.env.insert("PATH".into(), "/tmp/tools".into());
        assert!(authorize_available_safe_bash_request(&request, &inventory).is_none());
        request.env.remove("PATH");
        request.forward_env.push("SHELL".into());
        assert!(authorize_available_safe_bash_request(&request, &inventory).is_none());
    }

    #[test]
    fn generally_safe_ecosystem_commands_are_strict_and_not_read_safe() {
        let accepted = [
            "cargo build",
            "cargo build --workspace --release -j4",
            "cargo build -p cagent-agent --target-dir target/build",
            "cargo test -p cagent-agent",
            "cargo test -p cagent-agent -p cagent-cli --no-run",
            "cargo test -p cagent-agent config::instructions",
            "cargo test -p cagent-cli app::controller::tests::initial_prompt_uses_composer_command_dispatch app::controller::tests::blank_initial_prompt_is_ignored",
            "cargo test -- --nocapture",
            "cargo test --locked --offline --all-features -- --test-threads 2",
            "cargo test -- --ignored --exact",
            "cargo test --help",
            "cargo check --workspace",
            "cargo check --help",
            "cargo check -p cagent-cli 2>&1",
            "cargo check -p cagent-cli 2>&2",
            "cargo check -p cagent-cli 2>/dev/null",
            "cargo check -p cagent-cli >/dev/null",
            "cargo check -p cagent-cli &>/dev/null",
            "cargo check --workspace -- -D warnings",
            "cargo check --manifest-path crates/cagent-agent/Cargo.toml --target-dir target/check -j4",
            "cargo test --workspace --exclude slow-tests --features serde,tokio --release",
            "cargo fmt --check",
            "cargo fmt --all -- --check",
            "cargo fmt -p cagent-cli -- --check",
            "cargo fmt --package=cagent-agent --check",
            "cargo fmt --manifest-path crates/cagent-agent/Cargo.toml --message-format short --check",
            "cargo fmt --check -- crates/cagent-cli/src/app/events.rs crates/cagent-cli/src/app/input.rs crates/cagent-cli/src/app/tests.rs",
            "rustfmt --edition 2024 crates/cagent-agent/src/lib.rs",
            "rustfmt --check src/main.rs",
            "cargo clippy",
            "cargo clippy --workspace --all-targets -- -D warnings",
            "cargo nextest run",
            "cargo nextest run -- --nocapture",
            "cargo nextest run -p cagent-agent -E 'test(config)'",
            "cargo-nextest run --workspace",
            "cargo-nextest run --workspace -- --nocapture",
            "npm run test",
            "npm run test --silent --ignore-scripts --if-present",
            "npm run build --workspace web -- --mode production",
            "pnpm --filter web build -- --mode production",
            "pnpm -r run build",
            "pnpm lint:check",
            "yarn build -- --mode production",
            "yarn workspace web run build",
            "yarn format:check",
            "bun run build --filter web -- --minify",
            "bun build src/index.ts --outdir dist --target browser --minify",
            "bun run lint",
            "prettier --write .",
            "prettier src --check",
            "prettier --check --single-quote --tab-width 2 --parser rust src",
            "biome format --write src",
            "biome check src",
            "eslint --fix src",
            "eslint --max-warnings 0 src",
            "eslint --max-warnings=0 src",
            "tsc --noEmit --pretty=false",
            "tsc --project tsconfig.json",
            "tsc --build",
            "tsc --build packages/web --force --verbose",
            "tsc --diagnostics --traceResolution",
            "vitest run",
            "jest --runInBand",
            "deno fmt --check src",
            "deno compile --output bin/server src/server.ts -- --port 3000",
            "deno check mod.ts",
            "deno test tests",
            "deno test --help",
            "go test ./...",
            "go build ./...",
            "go build -trimpath -p 4 -o bin/server ./cmd/server",
            "go test -run TestName -count 1 -v ./...",
            "go test -run=TestName -count=1 -short -race -json ./...",
            "go test --help",
            "go fmt ./...",
            "go vet ./...",
            "gofmt main.go",
            "goimports -w cmd/server/main.go",
            "staticcheck ./...",
            "golangci-lint run",
            "pytest tests/test_x.py",
            "python -m pytest",
            "python -m build --wheel --outdir dist .",
            "python3 -m build --sdist --installer uv",
            "python -m pytest --help",
            "python3 -m ruff format src",
            "python -m isort --check src",
            "ruff format src",
            "ruff check src",
            "black src",
            "isort --profile black src tests",
            "flake8 src",
            "pylint src",
            "clang-format -i src/main.cc",
            "shfmt -w scripts/build.sh",
            "shfmt --diff scripts/build.sh",
            "stylua --check lua",
            "taplo fmt --check Cargo.toml",
            "terraform fmt -check -recursive",
            "terraform fmt -check -no-color",
            "tofu fmt -diff",
            "dotnet test --no-restore",
            "dotnet test --help",
            "dotnet build --configuration Release",
            "dotnet build src/App.csproj -c Release -o artifacts/app",
            "dotnet format --verify-no-changes",
            "gradle check --no-daemon",
            "gradle test --dry-run",
            "gradle check -m",
            "./gradlew :app:build --offline",
            "gradle test --max-workers 2 --parallel --warning-mode all",
            "./gradlew test --offline",
            "mvn -q test",
            "mvn -B -ntp compile",
            "mvn -T4 -pl app,core -am package -DskipTests",
            "dart format --set-exit-if-changed lib",
            "dart analyze",
            "dart test",
            "dart compile exe bin/app.dart -o build/app",
            "flutter analyze",
            "flutter build web --release --target lib/main.dart",
            "flutter build ios --release --no-codesign",
            "flutter test",
            "mix format --check-formatted",
            "mix test --warnings-as-errors",
            "mix credo --strict",
            "mix compile --warnings-as-errors",
            "zig fmt --check src/main.zig",
            "zig test src/main.zig",
            "zig build test",
            "zig build --release -j 4 -Doptimize=ReleaseFast",
            "rspec --format documentation",
            "rubocop --autocorrect lib",
            "standardrb --fix app",
            "phpunit --testsuite unit",
            "php-cs-fixer fix --dry-run src",
            "pint --test",
            "swift test --parallel",
            "swift build -c release --product App --jobs 4",
            "swift format --lint Sources",
        ];
        for command in accepted {
            let analysis = analyze_shell(command).unwrap();
            assert!(
                classifiers::classify_generally_safe_segment(&analysis.segments[0]).is_some(),
                "{command}"
            );
            assert!(classify_read_safe_shell(command).is_none(), "{command}");
        }
        for command in [
            "cargo build -- --emit asm",
            "cargo install thing",
            "cargo install --help",
            "cargo test -- --emit files",
            "cargo fmt --all -- --emit files",
            "cargo fmt -p -- --check",
            "cargo fmt --package= --check",
            "cargo test --unknown",
            "cargo test --help filter",
            "cargo check --package --release",
            "cargo check --jobs=0",
            "cargo test --manifest-path ../Cargo.toml",
            "cargo check --target-dir /tmp/cagent-target",
            "cargo test --config build.rustflags=[]",
            "cargo test -Zunstable-options",
            "cargo fmt --check -- --config rustfmt.toml crates/cagent-agent/src/lib.rs",
            "rustfmt --print-config default rustfmt.toml",
            "rustfmt --emit files",
            "rustfmt --edition future src/main.rs",
            "cargo clippy -- -Zunstable-options",
            "cargo test -- --",
            "cargo nextest run --config x",
            "npm run arbitrary",
            "npm run build:deploy",
            "npm run build --prefix ../other",
            "pnpm --dir ../other build",
            "yarn workspaces foreach run build",
            "bun run build:deploy",
            "bun build --watch src/index.ts",
            "bun build src/index.ts --outdir /tmp/build",
            "prettier --plugin ./formatter.js --write src",
            "eslint --max-warnings nope src",
            "eslint --max-warnings=-1 src",
            "eslint --config eslint.config.js src",
            "eslint --output-file report.txt src",
            "tsc --build --clean",
            "tsc --build ../shared",
            "gradle test --init-script init.gradle",
            "gradle test --build-file other.gradle",
            "prettier --write ../shared",
            "prettier --write ./../shared",
            "prettier --write src/../../shared",
            r"prettier --write ..\shared",
            r"prettier --write C:\shared",
            "go test -exec sh ./...",
            "go build -toolexec helper ./...",
            "go build -o ../server ./cmd/server",
            "go vet -vettool=tool ./...",
            "ruff check --fix src",
            "goimports -cpuprofile profile.out main.go",
            "isort --settings-path pyproject.toml src",
            "isort --line-length wide src",
            "cargo-nextest archive",
            "python3 -m http.server",
            "python -m build -Ckey=value",
            "python -m build --outdir ../dist",
            "biome lint src",
            "eslint --config formatter.js src",
            "vitest --watch",
            "deno test --allow-run tests",
            "deno compile --allow-all src/main.ts",
            "deno compile --output ../server src/main.ts",
            "clang-format --output-replacements-xml src/main.cc",
            "shfmt --to-json script.sh",
            "stylua --config-path ../stylua.toml src",
            "taplo fmt --config ../taplo.toml Cargo.toml",
            "terraform apply",
            "tofu plan",
            "dotnet build -t:Clean",
            "dotnet build --output ../artifacts",
            "dotnet format --report report.json",
            "gradle deploy",
            "gradle :app:deploy",
            "gradle check --init-script init.gradle",
            "mvn verify",
            "mvn install",
            "mvn package -DaltDeploymentRepository=x",
            "dart run tool.dart",
            "dart compile exe ../tool.dart -o build/tool",
            "flutter run",
            "flutter build ios --release",
            "mix deps.get",
            "mix compile --config ../config.exs",
            "zig build install",
            "zig build --prefix ../dist",
            "rspec --require evil.rb",
            "rubocop --config .rubocop.yml",
            "standardrb --config .standard.yml",
            "phpunit --configuration phpunit.xml",
            "php-cs-fixer fix --config .php-cs-fixer.php",
            "pint --config pint.json",
            "swift package resolve",
            "swift build --scratch-path ../scratch",
            "python -c 'print(1)'",
            "pytest tests > results.txt",
            "cargo check > results.txt",
            "cargo check 2> errors.txt",
            "cargo check 2>&3",
            "X=1 cargo test",
        ] {
            let analysis = analyze_shell(command).unwrap();
            assert!(
                classifiers::classify_generally_safe_segment(&analysis.segments[0]).is_none(),
                "{command}"
            );
        }
    }

    #[test]
    fn generally_safe_help_requires_a_supported_operation() {
        for command in [
            "cargo build --help",
            "cargo test --help",
            "cargo check --help",
            "deno test --help",
            "go test --help",
            "python -m pytest --help",
            "dotnet test --help",
            "npm run build --help",
            "bun build src/index.ts --help",
            "deno compile src/main.ts --help",
            "go build --help",
            "python -m build --help",
            "dotnet build --help",
            "mvn compile --help",
            "mix compile --help",
        ] {
            let analysis = analyze_shell(command).unwrap();
            assert!(
                classifiers::classify_generally_safe_segment(&analysis.segments[0]).is_some(),
                "{command}"
            );
        }
        for command in [
            "cargo install --help",
            "cargo test --help filter",
            "npm run build:deploy --help",
            "bun build --help",
            "mvn install --help",
        ] {
            let analysis = analyze_shell(command).unwrap();
            assert!(
                classifiers::classify_generally_safe_segment(&analysis.segments[0]).is_none(),
                "{command}"
            );
        }
    }

    #[test]
    fn generally_safe_commands_allow_only_read_safe_redirections() {
        for command in [
            "cargo check -p cagent-cli 2>&1",
            "cargo check -p cagent-cli 2>&2",
            "cargo check -p cagent-cli 2>/dev/null",
            "cargo check -p cagent-cli >/dev/null",
            "cargo check -p cagent-cli &>/dev/null",
        ] {
            let analysis = analyze_shell(command).unwrap();
            assert!(
                classifiers::classify_generally_safe_segment(&analysis.segments[0]).is_some(),
                "{command}"
            );
        }

        for command in [
            "cargo check > results.txt",
            "cargo check 2> errors.txt",
            "cargo check 2>&3",
        ] {
            let analysis = analyze_shell(command).unwrap();
            assert!(
                classifiers::classify_generally_safe_segment(&analysis.segments[0]).is_none(),
                "{command}"
            );
        }
    }

    #[test]
    fn generally_safe_respects_toggle_and_compound_segments() {
        let inventory = ShellCommandInventory::synthetic(&["cargo", "cat"]);
        let request = super::super::BashRequest {
            command: "cargo test && cat README.md".into(),
            cwd: None,
            env: BTreeMap::new(),
            forward_env: Vec::new(),
            timeout: None,
            wait: true,
        };
        let analysis = analyze_shell(&request.command).unwrap();
        let enabled =
            authorize_available_safe_bash_segments(&request, &inventory, &analysis, 3, false)
                .unwrap();
        assert!(enabled.tier(0).is_some());
        assert!(enabled.segment(1).is_some());
        let disabled =
            authorize_available_safe_bash_segments(&request, &inventory, &analysis, 0, false)
                .unwrap();
        assert!(disabled.tier(0).is_none());
        assert!(disabled.segment(1).is_some());
    }

    #[test]
    fn build_commands_are_level_two() {
        for command in [
            "cargo build",
            "npm run build",
            "pnpm build",
            "yarn build",
            "bun run build",
            "bun build src/index.ts",
            "tsc --build",
            "deno compile src/main.ts",
            "go build ./...",
            "python -m build",
            "dotnet build",
            "gradle build",
            "mvn compile",
            "dart compile exe bin/app.dart",
            "flutter build web",
            "mix compile",
            "zig build",
            "swift build",
        ] {
            let analysis = analyze_shell(command).unwrap();
            assert_eq!(
                classifiers::classify_generally_safe_segment(&analysis.segments[0]),
                Some(ShellSafetyTier::Level2),
                "{command}"
            );
        }
    }

    #[test]
    fn safety_levels_are_cumulative_and_minus_one_disables_all() {
        let inventory = ShellCommandInventory::synthetic(&[
            "cat", "cargo", "jj", "npm", "bun", "go", "python", "dotnet", "mvn",
        ]);
        for (command, expected) in [
            ("cat Cargo.toml", 0),
            ("jj status", 1),
            ("jj status --no-pager", 1),
            ("cargo test", 2),
            ("cargo build --release", 2),
            ("npm run build", 2),
            ("bun build src/index.ts", 2),
            ("go build ./...", 2),
            ("python -m build", 2),
            ("dotnet build", 2),
            ("mvn package", 2),
            ("cargo fmt", 3),
        ] {
            let request = super::super::BashRequest {
                command: command.into(),
                cwd: None,
                env: BTreeMap::new(),
                forward_env: Vec::new(),
                timeout: None,
                wait: true,
            };
            let analysis = analyze_shell(command).unwrap();
            for level in -1..=3 {
                let authorization = authorize_available_safe_bash_segments(
                    &request, &inventory, &analysis, level, false,
                );
                let classified = authorization
                    .as_ref()
                    .is_some_and(|value| value.whole.is_some() || value.tier(0).is_some());
                assert_eq!(classified, level >= expected, "{command} at {level}");
            }
        }
    }

    #[test]
    fn auto_review_evidence_is_fully_read_only_and_capped_at_level_one() {
        let inventory = ShellCommandInventory::synthetic(&["cat", "cargo", "jj"]);
        let request = |command: &str| super::super::BashRequest {
            command: command.into(),
            cwd: None,
            env: BTreeMap::new(),
            forward_env: Vec::new(),
            timeout: Some(5),
            wait: true,
        };
        assert!(
            authorize_auto_review_evidence(&request("cat Cargo.toml"), &inventory, -1).is_none()
        );
        assert!(
            authorize_auto_review_evidence(&request("cat Cargo.toml"), &inventory, 0).is_some()
        );
        assert!(authorize_auto_review_evidence(&request("jj status"), &inventory, 0).is_none());
        assert!(authorize_auto_review_evidence(&request("jj status"), &inventory, 1).is_some());
        assert!(authorize_auto_review_evidence(&request("jj status"), &inventory, 3).is_some());
        assert!(authorize_auto_review_evidence(&request("cargo test"), &inventory, 3).is_none());
        assert!(
            authorize_auto_review_evidence(&request("cat Cargo.toml && cargo test"), &inventory, 3)
                .is_none()
        );
        assert!(
            authorize_auto_review_evidence(&request("cat Cargo.toml > copy"), &inventory, 1)
                .is_none()
        );
        assert!(
            authorize_auto_review_evidence(&request("cat Cargo.toml &"), &inventory, 1).is_none()
        );
        let mut with_env = request("cat Cargo.toml");
        with_env.env.insert("X".into(), "1".into());
        assert!(authorize_auto_review_evidence(&with_env, &inventory, 1).is_none());
    }

    #[test]
    fn safe_write_includes_strict_config_set_without_secret_key_filtering() {
        let inventory = ShellCommandInventory::synthetic(&["cagent"]);
        for command in [
            "cagent config set api_key literal-secret",
            "cagent config set providers.openai.api_key literal-secret",
        ] {
            let request = super::super::BashRequest {
                command: command.into(),
                cwd: None,
                env: BTreeMap::new(),
                forward_env: Vec::new(),
                timeout: None,
                wait: true,
            };
            let analysis = analyze_shell(command).unwrap();
            let authorization =
                authorize_available_safe_bash_segments(&request, &inventory, &analysis, 0, true)
                    .unwrap();
            assert!(authorization.safe_write_segment(0));
        }
    }

    #[test]
    fn unavailable_inventory_disables_automatic_bash_authorization() {
        let inventory = ShellCommandInventory::empty(BashBackendInfo {
            kind: BashBackendKind::Native,
            executable: PathBuf::from("/bin/bash"),
            host_cwd: PathBuf::from("."),
            shell_cwd: ".".into(),
        });
        let request = super::super::BashRequest {
            command: "cargo fmt --all -- --check".into(),
            cwd: None,
            env: BTreeMap::new(),
            forward_env: Vec::new(),
            timeout: None,
            wait: true,
        };
        let analysis = analyze_shell(&request.command).unwrap();
        assert!(
            authorize_available_safe_bash_segments(&request, &inventory, &analysis, 3, false)
                .is_none()
        );
        assert!(authorize_available_safe_shell("cat README.md", &inventory).is_none());
    }

    #[test]
    fn generally_safe_commands_without_read_safe_forms_are_omitted_from_guidance() {
        let inventory = ShellCommandInventory::synthetic(&["cat", "cargo"]);
        let guidance = safe_bash_guidance(&inventory, 3);
        assert!(guidance.contains("cat"));
        assert!(guidance.contains("cargo"));
        assert!(!safe_shell_command_names().contains(&"cargo"));
        assert!(
            !safe_shell_examples()
                .iter()
                .any(|example| example.contains("cargo"))
        );
    }
}
