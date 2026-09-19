use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::RuntimeError;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct InstructionSource {
    pub path: PathBuf,
    pub content: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SkillMetadata {
    pub path: PathBuf,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub source: SkillSource,
    /// Compatibility implementation (for example `codex`), when applicable.
    #[serde(default)]
    pub compatibility: Option<String>,
    /// Whether a compatibility skill came from the workspace rather than the user account.
    #[serde(default)]
    pub compatibility_project: bool,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub content_hash: String,
}

const fn default_enabled() -> bool {
    true
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillSource {
    Project,
    #[default]
    Global,
    System,
    Compatibility,
}

impl SkillMetadata {
    #[must_use]
    pub fn directory(&self) -> &Path {
        self.path.parent().unwrap_or_else(|| Path::new("."))
    }

    #[must_use]
    pub fn source_label(&self) -> String {
        match self.source {
            SkillSource::Project => "project".into(),
            SkillSource::Global => "global".into(),
            SkillSource::System => "system".into(),
            SkillSource::Compatibility => format!(
                "{} ({})",
                if self.compatibility_project {
                    "project"
                } else {
                    "global"
                },
                self.compatibility.as_deref().unwrap_or("compatibility")
            ),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct LocalContextWarning {
    pub path: PathBuf,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LocalContextSnapshot {
    pub revision: String,
    pub instructions: Vec<InstructionSource>,
    pub skills: Vec<SkillMetadata>,
    /// Canonical label for the operating system running Cagent.
    #[serde(default = "unknown_operating_system")]
    pub operating_system: String,
    /// Canonical configured skill containers that proven read-only Bash may inspect.
    #[serde(default)]
    pub skill_read_roots: Vec<PathBuf>,
    /// Paths excluded from otherwise broader trusted skill containers.
    #[serde(default)]
    pub skill_read_exclusions: Vec<PathBuf>,
    #[serde(default)]
    pub warnings: Vec<LocalContextWarning>,
    #[serde(default)]
    pub workspace: Option<PathBuf>,
    #[serde(default)]
    pub home_dir: Option<PathBuf>,
}

impl Default for LocalContextSnapshot {
    fn default() -> Self {
        Self {
            revision: String::new(),
            instructions: Vec::new(),
            skills: Vec::new(),
            operating_system: unknown_operating_system(),
            skill_read_roots: Vec::new(),
            skill_read_exclusions: Vec::new(),
            warnings: Vec::new(),
            workspace: None,
            home_dir: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalContextPaths {
    pub config_dir: PathBuf,
    pub workspace: PathBuf,
    pub home_dir: PathBuf,
    pub opencode_config_dir: PathBuf,
}

impl LocalContextPaths {
    pub fn resolve(config_dir: PathBuf, workspace: PathBuf) -> Result<Self, RuntimeError> {
        let base =
            directories::BaseDirs::new().ok_or(RuntimeError::PlatformDirectoriesUnavailable)?;
        Ok(Self {
            config_dir,
            workspace,
            home_dir: base.home_dir().to_path_buf(),
            opencode_config_dir: base.config_dir().join("opencode"),
        })
    }

    #[must_use]
    pub fn instruction_paths(&self, external_agents: bool) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        if external_agents {
            paths.extend([
                self.home_dir.join(".claude/CLAUDE.md"),
                self.home_dir.join(".claude/CLAUDE.override.md"),
            ]);
        }
        paths.extend([
            self.config_dir.join("AGENTS.md"),
            self.config_dir.join("AGENTS.override.md"),
        ]);
        if external_agents {
            paths.extend([
                self.workspace.join("CLAUDE.md"),
                self.workspace.join("CLAUDE.override.md"),
            ]);
        }
        paths.extend([
            self.workspace.join("AGENTS.md"),
            self.workspace.join("AGENTS.override.md"),
        ]);
        paths
    }

    #[must_use]
    pub fn skill_roots(&self, external_agents: bool) -> Vec<PathBuf> {
        let mut roots = vec![
            self.config_dir.join("skills"),
            self.home_dir.join(".cagent/skills"),
            self.workspace.join("skills"),
            self.workspace.join(".cagent/skills"),
        ];
        if external_agents {
            roots.extend([
                self.home_dir.join(".agents/skills"),
                self.home_dir.join(".claude/skills"),
                self.home_dir.join(".codex/skills"),
                self.home_dir.join(".opencode/skills"),
                self.opencode_config_dir.join("skills"),
                self.workspace.join(".agents/skills"),
                self.workspace.join(".claude/skills"),
                self.workspace.join(".codex/skills"),
                self.workspace.join(".opencode/skills"),
            ]);
        }
        roots
    }

    fn skill_source(&self, root: &Path, skill: &Path) -> (SkillSource, Option<String>, bool) {
        if root == self.workspace.join("skills") || root == self.workspace.join(".cagent/skills") {
            (SkillSource::Project, None, false)
        } else if root == self.config_dir.join("skills")
            || root == self.home_dir.join(".cagent/skills")
        {
            if skill
                .strip_prefix(root)
                .is_ok_and(|relative| relative.starts_with(".system"))
            {
                (SkillSource::System, None, false)
            } else {
                (SkillSource::Global, None, false)
            }
        } else {
            let project = root.starts_with(&self.workspace);
            let implementation = if root.to_string_lossy().contains("claude") {
                "claude"
            } else if root.to_string_lossy().contains("codex") {
                "codex"
            } else if root.to_string_lossy().contains("opencode") {
                "opencode"
            } else {
                "agents"
            };
            (
                SkillSource::Compatibility,
                Some(implementation.into()),
                project,
            )
        }
    }

    #[must_use]
    pub fn watch_targets(&self) -> Vec<(PathBuf, bool)> {
        let mut targets = BTreeSet::new();
        for path in self.instruction_paths(true) {
            if let Some(parent) = path.parent() {
                targets.insert((parent.to_path_buf(), false));
            }
        }
        for root in self.skill_roots(true) {
            targets.insert((root, true));
        }
        targets.into_iter().collect()
    }
}

impl LocalContextSnapshot {
    pub fn load(
        paths: &LocalContextPaths,
        external_agents: bool,
        bundled_skills: bool,
        previous: Option<&Self>,
    ) -> Result<Self, RuntimeError> {
        let previous_instructions = previous
            .into_iter()
            .flat_map(|snapshot| &snapshot.instructions)
            .map(|source| (source.path.clone(), source.clone()))
            .collect::<BTreeMap<_, _>>();
        let previous_skills = previous
            .into_iter()
            .flat_map(|snapshot| &snapshot.skills)
            .map(|skill| (skill.path.clone(), skill.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut warnings = Vec::new();
        let mut instructions = Vec::new();
        for path in paths.instruction_paths(external_agents) {
            match std::fs::read_to_string(&path) {
                Ok(content) => instructions.push(InstructionSource { path, content }),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    if let Some(previous) = previous_instructions.get(&path) {
                        instructions.push(previous.clone());
                    }
                    warnings.push(LocalContextWarning {
                        path,
                        message: error.to_string(),
                    });
                }
            }
        }

        let system_root = if bundled_skills {
            match super::system_skills::install(&paths.config_dir) {
                Ok(root) => Some(root),
                Err(error) => {
                    warnings.push(LocalContextWarning {
                        path: super::system_skills::root(&paths.config_dir),
                        message: format!("failed to install bundled skills: {error}"),
                    });
                    None
                }
            }
        } else {
            None
        };
        let canonical_system_root = system_root
            .as_ref()
            .and_then(|root| root.canonicalize().ok());
        let configured_skill_roots = paths.skill_roots(external_agents);
        let mut skill_read_roots = configured_skill_roots
            .iter()
            .filter_map(|root| root.canonicalize().ok())
            .filter(|root| root.is_dir())
            .collect::<Vec<_>>();
        skill_read_roots.sort();
        skill_read_roots.dedup();
        let mut skills_by_target = BTreeMap::<PathBuf, SkillMetadata>::new();
        let mut seen_targets = BTreeSet::new();
        for root in configured_skill_roots {
            if !root.exists() {
                continue;
            }
            let mut builder = WalkBuilder::new(&root);
            let system_root = root.join(".system");
            builder
                .hidden(false)
                .follow_links(true)
                .standard_filters(false)
                .filter_entry(move |entry| entry.depth() == 0 || entry.path() != system_root);
            for entry in builder.build() {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) => {
                        warnings.push(LocalContextWarning {
                            path: root.clone(),
                            message: error.to_string(),
                        });
                        continue;
                    }
                };
                if entry.file_type().is_none_or(|kind| !kind.is_file())
                    || entry.file_name() != "SKILL.md"
                {
                    continue;
                }
                let advertised = entry.path().to_path_buf();
                let canonical = match advertised.canonicalize() {
                    Ok(path) => path,
                    Err(error) => {
                        warnings.push(LocalContextWarning {
                            path: advertised,
                            message: error.to_string(),
                        });
                        continue;
                    }
                };
                if canonical_system_root
                    .as_ref()
                    .is_some_and(|root| canonical.starts_with(root))
                {
                    continue;
                }
                if !seen_targets.insert(canonical.clone()) {
                    continue;
                }
                match parse_skill(&canonical) {
                    Ok(mut skill) => {
                        let (source, compatibility, compatibility_project) =
                            paths.skill_source(&root, &canonical);
                        skill.source = source;
                        skill.compatibility = compatibility;
                        skill.compatibility_project = compatibility_project;
                        skills_by_target.insert(canonical, skill);
                    }
                    Err(message) => {
                        if let Some(previous) = previous_skills.get(&canonical) {
                            skills_by_target.insert(canonical.clone(), previous.clone());
                        }
                        warnings.push(LocalContextWarning {
                            path: canonical,
                            message,
                        });
                    }
                }
            }
        }
        if let Some(root) = system_root {
            if let Some(canonical) = canonical_system_root {
                skill_read_roots.push(canonical);
                skill_read_roots.sort();
                skill_read_roots.dedup();
            }
            let mut builder = WalkBuilder::new(&root);
            builder
                .hidden(false)
                .follow_links(false)
                .standard_filters(false);
            for entry in builder.build() {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) => {
                        warnings.push(LocalContextWarning {
                            path: root.clone(),
                            message: error.to_string(),
                        });
                        continue;
                    }
                };
                if entry.file_type().is_none_or(|kind| !kind.is_file())
                    || entry.file_name() != "SKILL.md"
                {
                    continue;
                }
                let advertised = entry.path().to_path_buf();
                let canonical = match advertised.canonicalize() {
                    Ok(path) => path,
                    Err(error) => {
                        warnings.push(LocalContextWarning {
                            path: advertised,
                            message: error.to_string(),
                        });
                        continue;
                    }
                };
                if !seen_targets.insert(canonical.clone()) {
                    continue;
                }
                match parse_skill(&canonical) {
                    Ok(skill) => {
                        skills_by_target.insert(canonical, skill);
                    }
                    Err(message) => warnings.push(LocalContextWarning {
                        path: canonical,
                        message,
                    }),
                }
            }
        }
        let mut skills = skills_by_target.into_values().collect::<Vec<_>>();
        skills.sort_by(|left, right| left.path.cmp(&right.path));
        warnings.sort();
        warnings.dedup();
        let mut snapshot = Self {
            revision: String::new(),
            instructions,
            skills,
            operating_system: detect_operating_system(),
            skill_read_roots,
            skill_read_exclusions: if bundled_skills {
                Vec::new()
            } else {
                let root = super::system_skills::root(&paths.config_dir);
                vec![root.canonicalize().unwrap_or(root)]
            },
            warnings,
            workspace: Some(paths.workspace.clone()),
            home_dir: Some(paths.home_dir.clone()),
        };
        snapshot.revision = snapshot.compute_revision()?;
        Ok(snapshot)
    }

    /// Applies name-based skill disablement while retaining rows for management UIs.
    #[must_use]
    pub fn with_disabled_skills(mut self, disabled: &BTreeSet<String>) -> Self {
        for skill in &mut self.skills {
            skill.enabled = !disabled.contains(&skill.name);
        }
        self.revision = self.compute_revision().unwrap_or_default();
        self
    }

    fn compute_revision(&self) -> Result<String, RuntimeError> {
        let mut warnings = self.warnings.clone();
        warnings.sort();
        let encoded = serde_json::to_vec(&(
            &self.instructions,
            &self.skills,
            &self.operating_system,
            &self.skill_read_roots,
            &self.skill_read_exclusions,
            warnings,
        ))?;
        Ok(format!("{:x}", Sha256::digest(encoded)))
    }

    #[must_use]
    pub fn trusted_skill_roots(&self) -> Vec<PathBuf> {
        let mut roots = self.skill_read_roots.clone();
        roots.extend(
            self.skills
                .iter()
                .map(|skill| skill.directory().to_path_buf()),
        );
        roots.sort();
        roots.dedup();
        roots
    }

    #[must_use]
    pub fn is_trusted_skill_read(&self, path: &Path) -> bool {
        let Ok(path) = path.canonicalize() else {
            return false;
        };
        !self
            .skill_read_exclusions
            .iter()
            .filter_map(|excluded| excluded.canonicalize().ok())
            .any(|excluded| path.starts_with(excluded))
            && self
                .trusted_skill_roots()
                .iter()
                .filter_map(|root| root.canonicalize().ok())
                .any(|root| path.starts_with(root))
    }

    #[must_use]
    pub fn full_notice(&self) -> String {
        let mut notice = String::from(
            "<cagent:local-context revision=\"complete\">\nThe following runtime-authored local context is authoritative. Later local-context revisions supersede it.\n",
        );
        let _ = writeln!(notice, "\nOperating system: {}", self.operating_system);
        append_instruction_catalog(&mut notice, &self.instructions);
        append_skill_catalog(&mut notice, self, "Available skills");
        notice.push_str(SKILL_ACTIVATION_GUIDANCE);
        notice.push_str("\n</cagent:local-context>");
        notice
    }

    #[must_use]
    pub fn delta_notice(&self, previous: &Self) -> String {
        let mut notice = format!(
            "<cagent:local-context revision=\"update\" previous=\"{}\" current=\"{}\">\nThis cumulative update supersedes the changed portions of earlier local context.\n",
            previous.revision, self.revision
        );
        if previous.operating_system != self.operating_system {
            let _ = writeln!(
                notice,
                "\nOperating system changed: {} -> {}",
                previous.operating_system, self.operating_system
            );
        }
        append_instruction_diffs(&mut notice, previous, self);
        append_skill_changes(&mut notice, previous, self);
        notice.push_str("</cagent:local-context>");
        notice
    }
}

fn unknown_operating_system() -> String {
    "Unknown".into()
}

fn detect_operating_system() -> String {
    let (os_release, kernel_release, kernel_version) = if std::env::consts::OS == "linux" {
        (
            std::fs::read_to_string("/etc/os-release")
                .or_else(|_| std::fs::read_to_string("/usr/lib/os-release"))
                .ok(),
            std::fs::read_to_string("/proc/sys/kernel/osrelease").ok(),
            std::fs::read_to_string("/proc/version").ok(),
        )
    } else {
        (None, None, None)
    };
    operating_system_label(
        std::env::consts::OS,
        os_release.as_deref(),
        kernel_release.as_deref(),
        kernel_version.as_deref(),
    )
}

fn operating_system_label(
    target_os: &str,
    os_release: Option<&str>,
    kernel_release: Option<&str>,
    kernel_version: Option<&str>,
) -> String {
    let label = match target_os {
        "windows" => "Windows",
        "macos" => "Mac",
        "linux" => linux_distribution_label(os_release),
        "freebsd" => "FreeBSD",
        "openbsd" => "OpenBSD",
        "netbsd" => "NetBSD",
        "dragonfly" => "DragonFly BSD",
        "android" => "Android",
        "solaris" => "Solaris",
        "illumos" => "illumos",
        "haiku" => "Haiku",
        "bsd" | "bitrig" => "BSD",
        _ => "Unknown",
    };
    if target_os == "linux" && is_wsl(kernel_release, kernel_version) {
        format!("{label} in WSL")
    } else {
        label.into()
    }
}

fn linux_distribution_label(os_release: Option<&str>) -> &'static str {
    let id = os_release
        .and_then(os_release_id)
        .unwrap_or_default()
        .to_ascii_lowercase();
    match id.as_str() {
        "ubuntu" => "Ubuntu Linux",
        "debian" => "Debian Linux",
        "fedora" => "Fedora Linux",
        "arch" | "archlinux" => "Arch Linux",
        "alpine" => "Alpine Linux",
        "linuxmint" => "Linux Mint",
        "nixos" => "NixOS",
        "opensuse" | "opensuse-leap" | "opensuse-tumbleweed" => "openSUSE Linux",
        "rhel" | "redhat" | "redhatenterpriseserver" => "Red Hat Enterprise Linux",
        "rocky" => "Rocky Linux",
        "almalinux" => "AlmaLinux",
        "amzn" | "amazon" => "Amazon Linux",
        "manjaro" => "Manjaro Linux",
        "kali" => "Kali Linux",
        "pop" | "pop-os" => "Pop!_OS",
        _ => "Linux",
    }
}

fn os_release_id(source: &str) -> Option<&str> {
    source.lines().find_map(|line| {
        let value = line.strip_prefix("ID=")?.trim();
        value
            .strip_prefix('"')
            .and_then(|quoted| quoted.strip_suffix('"'))
            .or_else(|| {
                value
                    .strip_prefix('\'')
                    .and_then(|quoted| quoted.strip_suffix('\''))
            })
            .or((!value.is_empty()).then_some(value))
    })
}

fn is_wsl(kernel_release: Option<&str>, kernel_version: Option<&str>) -> bool {
    kernel_release
        .into_iter()
        .chain(kernel_version)
        .any(|value| {
            let value = value.to_ascii_lowercase();
            value.contains("microsoft") || value.contains("wsl")
        })
}

const SKILL_ACTIVATION_GUIDANCE: &str = "\nAll available skills are listed above with exact paths. When a skill matches the task, read that skill directory's SKILL.md with a documented read-only Bash command before following it. Read only the listed skill directory and its descendants; resolve relative paths from SKILL.md's directory and don't search parent/sibling/home for skills. Skill bodies and supporting files are not injected automatically; read supporting files only when SKILL.md directs you to them or the task requires them.\n";

fn append_instruction_catalog(output: &mut String, instructions: &[InstructionSource]) {
    if instructions.is_empty() {
        output.push_str("\nNo local instruction files are currently active.\n");
        return;
    }
    output.push_str("\nEffective instructions, in increasing precedence order:\n");
    for source in instructions {
        let _ = writeln!(
            output,
            "--- BEGIN LOCAL INSTRUCTIONS: {} ---",
            source.path.display()
        );
        output.push_str(&source.content);
        if !source.content.ends_with('\n') {
            output.push('\n');
        }
        let _ = writeln!(
            output,
            "--- END LOCAL INSTRUCTIONS: {} ---",
            source.path.display()
        );
    }
}

fn append_skill_catalog(output: &mut String, snapshot: &LocalContextSnapshot, title: &str) {
    let _ = writeln!(output, "\n{title}:");
    if snapshot.skills.is_empty() {
        output.push_str("- none\n");
    } else {
        for skill in snapshot.skills.iter().filter(|skill| skill.enabled) {
            let _ = writeln!(
                output,
                "- {}: {}",
                display_skill_path(skill, snapshot),
                skill.description
            );
        }
    }
}

fn display_skill_path(skill: &SkillMetadata, snapshot: &LocalContextSnapshot) -> String {
    let directory = skill.directory();
    if let Some(workspace) = snapshot.workspace.as_deref()
        && let Ok(relative) = directory.strip_prefix(workspace)
    {
        return format!("./{}", relative.display());
    }
    if let Some(home_dir) = snapshot.home_dir.as_deref()
        && let Ok(relative) = directory.strip_prefix(home_dir)
    {
        return format!("~/{}", relative.display());
    }
    directory.display().to_string()
}

fn append_instruction_diffs(
    output: &mut String,
    previous: &LocalContextSnapshot,
    current: &LocalContextSnapshot,
) {
    let old = previous
        .instructions
        .iter()
        .map(|source| (source.path.clone(), source.content.as_str()))
        .collect::<BTreeMap<_, _>>();
    let new = current
        .instructions
        .iter()
        .map(|source| (source.path.clone(), source.content.as_str()))
        .collect::<BTreeMap<_, _>>();
    for path in old
        .keys()
        .chain(new.keys())
        .cloned()
        .collect::<BTreeSet<_>>()
    {
        let before = old.get(&path).copied().unwrap_or_default();
        let after = new.get(&path).copied().unwrap_or_default();
        if old.get(&path) == new.get(&path) {
            continue;
        }
        let diff = similar::TextDiff::from_lines(before, after)
            .unified_diff()
            .context_radius(usize::MAX)
            .header(
                &format!("a/{}", path.display()),
                &format!("b/{}", path.display()),
            )
            .to_string();
        output.push_str("\nInstruction change:\n");
        output.push_str(&diff);
        if !diff.ends_with('\n') {
            output.push('\n');
        }
    }
}

fn append_skill_changes(
    output: &mut String,
    previous: &LocalContextSnapshot,
    current: &LocalContextSnapshot,
) {
    let old = previous
        .skills
        .iter()
        .map(|skill| (skill.path.clone(), skill))
        .collect::<BTreeMap<_, _>>();
    let new = current
        .skills
        .iter()
        .map(|skill| (skill.path.clone(), skill))
        .collect::<BTreeMap<_, _>>();
    let mut changes = Vec::new();
    for path in old
        .keys()
        .chain(new.keys())
        .cloned()
        .collect::<BTreeSet<_>>()
    {
        match (old.get(&path), new.get(&path)) {
            (None, Some(skill)) => changes.push(("added", *skill)),
            (Some(skill), None) => changes.push(("removed", *skill)),
            (Some(before), Some(after)) if before != after => changes.push(("updated", *after)),
            _ => {}
        }
    }
    if !changes.is_empty() {
        output.push_str("\nSkill changes:\n");
        for (change, skill) in changes {
            let _ = writeln!(
                output,
                "- {change}: {}: {}",
                display_skill_path(skill, current),
                skill.description
            );
        }
    }
}

#[derive(Deserialize)]
struct SkillFrontmatter {
    name: Option<String>,
    description: Option<String>,
}

fn parse_skill(path: &Path) -> Result<SkillMetadata, String> {
    let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
    let source = std::str::from_utf8(&bytes).map_err(|error| error.to_string())?;
    let parsed: SkillFrontmatter =
        serde_yaml::from_str(yaml_frontmatter(source)?).map_err(|error| error.to_string())?;
    let description = parsed
        .description
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "YAML frontmatter field `description` is required".to_owned())?;
    let fallback_name = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|value| value.to_str())
        .ok_or_else(|| "skill directory has no valid UTF-8 name".to_owned())?;
    let name = parsed
        .name
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| fallback_name.to_owned());
    Ok(SkillMetadata {
        path: path.to_path_buf(),
        name,
        description,
        source: SkillSource::Global,
        compatibility: None,
        compatibility_project: false,
        enabled: true,
        content_hash: format!("{:x}", Sha256::digest(bytes)),
    })
}

fn yaml_frontmatter(source: &str) -> Result<&str, String> {
    let source = source
        .strip_prefix("---\n")
        .or_else(|| source.strip_prefix("---\r\n"))
        .ok_or_else(|| "SKILL.md must begin with YAML frontmatter delimited by `---`".to_owned())?;
    let mut offset = 0;
    for line in source.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            return Ok(&source[..offset]);
        }
        offset += line.len();
    }
    Err("SKILL.md YAML frontmatter has no closing `---` delimiter".into())
}

#[derive(Clone, Debug)]
pub struct InstructionSnapshot(Arc<RwLock<LocalContextSnapshot>>);

impl Default for InstructionSnapshot {
    fn default() -> Self {
        Self(Arc::new(RwLock::new(LocalContextSnapshot::default())))
    }
}

impl InstructionSnapshot {
    #[must_use]
    pub fn from_snapshot(snapshot: LocalContextSnapshot) -> Self {
        Self(Arc::new(RwLock::new(snapshot)))
    }

    #[must_use]
    pub fn current(&self) -> LocalContextSnapshot {
        self.0.read().map_or_else(
            |_| LocalContextSnapshot::default(),
            |snapshot| snapshot.clone(),
        )
    }

    pub(crate) fn replace(&self, snapshot: LocalContextSnapshot) {
        if let Ok(mut current) = self.0.write() {
            *current = snapshot;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn paths(temporary: &TempDir) -> LocalContextPaths {
        LocalContextPaths {
            config_dir: temporary.path().join("config"),
            workspace: temporary.path().join("workspace"),
            home_dir: temporary.path().join("home"),
            opencode_config_dir: temporary.path().join("opencode-config"),
        }
    }

    #[test]
    fn classifies_supported_operating_systems() {
        let linux = |id| operating_system_label("linux", Some(&format!("ID={id}\n")), None, None);
        assert_eq!(
            operating_system_label("windows", None, None, None),
            "Windows"
        );
        assert_eq!(operating_system_label("macos", None, None, None), "Mac");
        assert_eq!(linux("ubuntu"), "Ubuntu Linux");
        assert_eq!(linux("debian"), "Debian Linux");
        assert_eq!(linux("fedora"), "Fedora Linux");
        assert_eq!(linux("arch"), "Arch Linux");
        assert_eq!(linux("alpine"), "Alpine Linux");
        assert_eq!(linux("linuxmint"), "Linux Mint");
        assert_eq!(linux("nixos"), "NixOS");
        assert_eq!(linux("opensuse-tumbleweed"), "openSUSE Linux");
        assert_eq!(linux("rhel"), "Red Hat Enterprise Linux");
        assert_eq!(linux("rocky"), "Rocky Linux");
        assert_eq!(linux("almalinux"), "AlmaLinux");
        assert_eq!(linux("amzn"), "Amazon Linux");
        assert_eq!(linux("manjaro"), "Manjaro Linux");
        assert_eq!(linux("kali"), "Kali Linux");
        assert_eq!(linux("pop"), "Pop!_OS");
        assert_eq!(linux("unlisted"), "Linux");
        assert_eq!(
            operating_system_label("freebsd", None, None, None),
            "FreeBSD"
        );
        assert_eq!(
            operating_system_label("openbsd", None, None, None),
            "OpenBSD"
        );
        assert_eq!(operating_system_label("netbsd", None, None, None), "NetBSD");
        assert_eq!(
            operating_system_label("dragonfly", None, None, None),
            "DragonFly BSD"
        );
        assert_eq!(operating_system_label("bsd", None, None, None), "BSD");
        assert_eq!(
            operating_system_label("android", None, None, None),
            "Android"
        );
        assert_eq!(
            operating_system_label("solaris", None, None, None),
            "Solaris"
        );
        assert_eq!(
            operating_system_label("illumos", None, None, None),
            "illumos"
        );
        assert_eq!(operating_system_label("haiku", None, None, None), "Haiku");
        assert_eq!(
            operating_system_label("unsupported", None, None, None),
            "Unknown"
        );
    }

    #[test]
    fn parses_quoted_os_release_ids_and_detects_wsl() {
        assert_eq!(
            operating_system_label(
                "linux",
                Some("NAME=Ubuntu\nID=\"ubuntu\"\n"),
                Some("6.6.87.2-microsoft-standard-WSL2"),
                None,
            ),
            "Ubuntu Linux in WSL"
        );
        assert_eq!(
            operating_system_label("linux", Some("NAME=No ID\n"), None, Some("Linux version")),
            "Linux"
        );
    }

    #[test]
    fn operating_system_is_rendered_and_old_snapshots_default_to_unknown() {
        let old: LocalContextSnapshot = serde_json::from_value(serde_json::json!({
            "revision": "old",
            "instructions": [],
            "skills": []
        }))
        .unwrap();
        assert_eq!(old.operating_system, "Unknown");

        let mut current = old.clone();
        current.revision = "current".into();
        current.operating_system = "Ubuntu Linux in WSL".into();
        assert_ne!(
            old.compute_revision().unwrap(),
            current.compute_revision().unwrap()
        );
        assert!(
            current
                .full_notice()
                .contains("Operating system: Ubuntu Linux in WSL")
        );
        assert!(
            current
                .delta_notice(&old)
                .contains("Operating system changed: Unknown -> Ubuntu Linux in WSL")
        );
    }

    #[test]
    fn loads_precedence_and_compatibility_sources() {
        let temporary = TempDir::new().unwrap();
        let paths = paths(&temporary);
        fs::create_dir_all(&paths.config_dir).unwrap();
        fs::create_dir_all(&paths.workspace).unwrap();
        fs::create_dir_all(paths.home_dir.join(".claude")).unwrap();
        fs::write(paths.home_dir.join(".claude/CLAUDE.md"), "global claude").unwrap();
        fs::write(paths.config_dir.join("AGENTS.md"), "global agents").unwrap();
        fs::write(paths.workspace.join("CLAUDE.md"), "workspace claude").unwrap();
        fs::write(paths.workspace.join("AGENTS.md"), "workspace agents").unwrap();
        let snapshot = LocalContextSnapshot::load(&paths, true, false, None).unwrap();
        assert_eq!(
            snapshot
                .instructions
                .iter()
                .map(|source| source.content.as_str())
                .collect::<Vec<_>>(),
            [
                "global claude",
                "global agents",
                "workspace claude",
                "workspace agents"
            ]
        );
        assert_eq!(
            LocalContextSnapshot::load(&paths, false, false, None)
                .unwrap()
                .instructions
                .len(),
            2
        );
    }

    #[test]
    fn recursively_discovers_and_retains_invalid_skill_edits() {
        let temporary = TempDir::new().unwrap();
        let paths = paths(&temporary);
        let skill = paths.config_dir.join("skills/nested/demo/SKILL.md");
        fs::create_dir_all(skill.parent().unwrap()).unwrap();
        fs::write(
            &skill,
            "---\ndescription: Demo skill\nunknown: true\n---\nbody\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            let aliases = paths.home_dir.join(".cagent/skills");
            fs::create_dir_all(&aliases).unwrap();
            std::os::unix::fs::symlink(skill.parent().unwrap(), aliases.join("same-demo")).unwrap();
        }
        let first = LocalContextSnapshot::load(&paths, false, false, None).unwrap();
        assert_eq!(first.skills.len(), 1);
        assert_eq!(first.skills[0].name, "demo");
        fs::write(&skill, "---\nname: broken\n---\n").unwrap();
        let retained = LocalContextSnapshot::load(&paths, false, false, Some(&first)).unwrap();
        assert_eq!(retained.skills, first.skills);
        assert_eq!(retained.warnings.len(), 1);
        fs::remove_file(&skill).unwrap();
        assert!(
            LocalContextSnapshot::load(&paths, false, false, Some(&retained))
                .unwrap()
                .skills
                .is_empty()
        );
    }

    #[test]
    fn body_edits_emit_only_skill_metadata() {
        let temporary = TempDir::new().unwrap();
        let paths = paths(&temporary);
        let skill = paths.workspace.join("skills/demo/SKILL.md");
        fs::create_dir_all(skill.parent().unwrap()).unwrap();
        fs::write(&skill, "---\ndescription: Demo\n---\nfirst\n").unwrap();
        let first = LocalContextSnapshot::load(&paths, false, false, None).unwrap();
        assert!(
            first
                .trusted_skill_roots()
                .contains(&paths.workspace.join("skills").canonicalize().unwrap())
        );
        fs::write(&skill, "---\ndescription: Demo\n---\nsecret body\n").unwrap();
        let second = LocalContextSnapshot::load(&paths, false, false, Some(&first)).unwrap();
        let delta = second.delta_notice(&first);
        assert!(delta.contains("updated:"));
        assert!(!delta.contains("secret body"));
    }

    #[test]
    fn compatibility_skill_root_is_trusted_as_a_read_container() {
        let temporary = TempDir::new().unwrap();
        let paths = paths(&temporary);
        let root = paths.home_dir.join(".agents/skills");
        let skill = root.join("demo/SKILL.md");
        fs::create_dir_all(skill.parent().unwrap()).unwrap();
        fs::write(&skill, "---\ndescription: Demo\n---\n").unwrap();

        let snapshot = LocalContextSnapshot::load(&paths, true, false, None).unwrap();

        assert!(
            snapshot
                .trusted_skill_roots()
                .contains(&root.canonicalize().unwrap())
        );
    }

    #[test]
    fn bundled_skills_are_separate_disableable_and_do_not_follow_links() {
        let temporary = TempDir::new().unwrap();
        let paths = paths(&temporary);
        let enabled = LocalContextSnapshot::load(&paths, false, true, None).unwrap();
        let names = enabled
            .skills
            .iter()
            .map(|skill| skill.name.as_str())
            .collect::<BTreeSet<_>>();
        assert!(names.contains("customize-cagent"));
        assert!(names.contains("skill-creator"));
        assert!(names.contains("skill-installer"));
        let marker = paths
            .config_dir
            .join("skills/.system/.cagent-system-skills.marker");
        assert!(marker.exists());

        #[cfg(unix)]
        {
            let outside = temporary.path().join("outside/demo");
            fs::create_dir_all(&outside).unwrap();
            fs::write(outside.join("SKILL.md"), "---\ndescription: escaped\n---\n").unwrap();
            std::os::unix::fs::symlink(&outside, paths.config_dir.join("skills/.system/escaped"))
                .unwrap();
            let rescanned =
                LocalContextSnapshot::load(&paths, false, true, Some(&enabled)).unwrap();
            assert!(!rescanned.skills.iter().any(|skill| skill.name == "demo"));
        }

        let marker_contents = fs::read_to_string(&marker).unwrap();
        let disabled = LocalContextSnapshot::load(&paths, false, false, Some(&enabled)).unwrap();
        assert!(disabled.skills.is_empty());
        assert!(
            !disabled.is_trusted_skill_read(
                &paths
                    .config_dir
                    .join("skills/.system/customize-cagent/SKILL.md")
                    .canonicalize()
                    .unwrap()
            )
        );
        assert_eq!(fs::read_to_string(marker).unwrap(), marker_contents);
    }

    #[test]
    fn bundle_install_failure_keeps_unrelated_local_context() {
        let temporary = TempDir::new().unwrap();
        let mut paths = paths(&temporary);
        fs::create_dir_all(paths.workspace.join("skills/demo")).unwrap();
        fs::write(
            paths.workspace.join("skills/demo/SKILL.md"),
            "---\ndescription: Demo\n---\n",
        )
        .unwrap();
        paths.config_dir = temporary.path().join("not-a-directory");
        fs::write(&paths.config_dir, "file").unwrap();
        let snapshot = LocalContextSnapshot::load(&paths, false, true, None).unwrap();
        assert!(snapshot.skills.iter().any(|skill| skill.name == "demo"));
        assert!(
            snapshot
                .warnings
                .iter()
                .any(|warning| warning.message.contains("failed to install bundled skills"))
        );
    }

    #[test]
    fn skill_catalog_uses_compact_directory_paths_without_group_spacing() {
        let workspace = PathBuf::from("/workspace/project");
        let home = PathBuf::from("/home/alice");
        let snapshot = LocalContextSnapshot {
            revision: String::new(),
            instructions: Vec::new(),
            skills: vec![
                SkillMetadata {
                    path: home.join(".agents/skills/find-skills/SKILL.md"),
                    name: "find-skills".into(),
                    description: "Helps users discover skills.".into(),
                    source: SkillSource::Compatibility,
                    compatibility: Some("agents".into()),
                    compatibility_project: false,
                    enabled: true,
                    content_hash: String::new(),
                },
                SkillMetadata {
                    path: workspace.join("skills/abc/SKILL.md"),
                    name: "abc".into(),
                    description: "An example skill.".into(),
                    source: SkillSource::Project,
                    compatibility: None,
                    compatibility_project: false,
                    enabled: true,
                    content_hash: String::new(),
                },
            ],
            operating_system: "Linux".into(),
            skill_read_roots: Vec::new(),
            skill_read_exclusions: Vec::new(),
            warnings: Vec::new(),
            workspace: Some(workspace),
            home_dir: Some(home),
        };

        assert_eq!(snapshot.full_notice().match_indices("- ").count(), 2);
        assert!(
            snapshot
                .full_notice()
                .contains("- ~/.agents/skills/find-skills: Helps users discover skills.")
        );
        assert!(
            snapshot
                .full_notice()
                .contains("- ./skills/abc: An example skill.")
        );
        let notice = snapshot.full_notice();
        let catalog = notice
            .split("\nAvailable skills:\n")
            .nth(1)
            .and_then(|catalog| {
                catalog
                    .split("\nAll available skills are listed above")
                    .next()
            })
            .unwrap();
        assert!(!catalog.contains("SKILL.md"));
        assert!(!catalog.contains("find-skills —"));
    }

    #[test]
    fn warning_order_does_not_change_the_revision() {
        let first = LocalContextSnapshot {
            revision: String::new(),
            instructions: Vec::new(),
            skills: Vec::new(),
            operating_system: "Linux".into(),
            skill_read_roots: Vec::new(),
            skill_read_exclusions: Vec::new(),
            warnings: vec![
                LocalContextWarning {
                    path: "b/SKILL.md".into(),
                    message: "second".into(),
                },
                LocalContextWarning {
                    path: "a/SKILL.md".into(),
                    message: "first".into(),
                },
            ],
            workspace: None,
            home_dir: None,
        };
        let mut second = first.clone();
        second.warnings.reverse();
        assert_eq!(
            first.compute_revision().unwrap(),
            second.compute_revision().unwrap()
        );
    }
}
