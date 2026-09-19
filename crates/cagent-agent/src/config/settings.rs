use super::{
    ConfigSnapshot, ConfigStore, DEFAULT_AGENT_NAME, DEFAULT_ATTACHMENT_BYTES,
    DEFAULT_ATTACHMENT_HARD_CAP_BYTES, DEFAULT_COMPACTION_THRESHOLD_PERCENT,
    DEFAULT_RECAP_IDLE_SECONDS, DEFAULT_SUBAGENT_MAX_CONCURRENT, UiColors, UiDiffMode,
};
use crate::RuntimeError;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SettingKind {
    Boolean,
    List(Vec<(String, String)>),
    TomlChoices(Vec<(String, String)>),
    PositiveInteger,
    NonNegativeInteger,
    String,
    TomlArray,
    ModelPicker,
    DefaultAgentPicker(Vec<(String, String)>),
    KeyBindings,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingSection {
    General,
    Ui,
    Files,
    Limits,
    Keybindings,
}

impl SettingSection {
    pub const ALL: [Self; 5] = [
        Self::General,
        Self::Ui,
        Self::Files,
        Self::Limits,
        Self::Keybindings,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Ui => "UI",
            Self::Files => "Files",
            Self::Limits => "Limits",
            Self::Keybindings => "Keybindings",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SettingDefinition {
    pub key: String,
    pub label: String,
    pub description: String,
    pub kind: SettingKind,
    pub section: SettingSection,
    pub default_value: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SettingRow {
    pub definition: SettingDefinition,
    pub effective_value: String,
    pub display_value: String,
    pub explicit_value: Option<String>,
}

impl ConfigStore {
    /// Returns frontend-ready definitions and effective values for globally editable settings.
    ///
    /// # Errors
    /// Returns an error when the file-backed source cannot be read or mode definitions are invalid.
    pub fn settings(&self) -> Result<Vec<SettingRow>, RuntimeError> {
        let snapshot = self.snapshot();
        let definitions = setting_definitions(&snapshot)?;
        definitions
            .into_iter()
            .map(|definition| {
                let explicit_value = self.get_value(&definition.key)?;
                let effective_value = effective_value(&snapshot, &definition.key)
                    .unwrap_or_else(|| definition.default_value.clone());
                let display_value = match definition.key.as_str() {
                    "limits.attachment_bytes" | "limits.attachment_hard_cap_bytes" => {
                        effective_value
                            .parse()
                            .map_or_else(|_| effective_value.clone(), format_byte_count)
                    }
                    _ => effective_value.clone(),
                };
                Ok(SettingRow {
                    definition,
                    effective_value,
                    display_value,
                    explicit_value,
                })
            })
            .collect()
    }

    /// Validates a known setting with its typed editor rules before atomically saving it.
    ///
    /// # Errors
    /// Returns an error for unknown settings, invalid values, conflicts, or persistence failures.
    pub fn save_setting(&self, key: &str, raw_value: &str) -> Result<(), RuntimeError> {
        let row = self
            .settings()?
            .into_iter()
            .find(|row| row.definition.key == key)
            .ok_or_else(|| RuntimeError::InvalidOption(format!("unknown setting: {key}")))?;
        if matches!(
            &row.definition.kind,
            SettingKind::KeyBindings | SettingKind::TomlArray
        ) {
            let invalid_array = if row.definition.kind == SettingKind::KeyBindings {
                "invalid keybinding array"
            } else {
                "invalid TOML string array"
            };
            let non_string_array = if row.definition.kind == SettingKind::KeyBindings {
                "keybindings must be an array of strings"
            } else {
                "value must be an array of strings"
            };
            let parsed: toml::Value = toml::from_str(&format!("value = {raw_value}"))
                .map_err(|_| RuntimeError::InvalidOption(invalid_array.into()))?;
            let values = parsed
                .get("value")
                .and_then(toml::Value::as_array)
                .ok_or_else(|| RuntimeError::InvalidOption(non_string_array.into()))?;
            if key == "tiers.small" {
                for value in values {
                    let table = value.as_table().ok_or_else(|| {
                        RuntimeError::InvalidOption(
                            "tier candidates must be inline tables with a model".into(),
                        )
                    })?;
                    if table.get("model").and_then(toml::Value::as_str).is_none()
                        || table.contains_key("tier")
                        || table
                            .keys()
                            .any(|field| field != "model" && field != "effort")
                    {
                        return Err(RuntimeError::InvalidOption(
                            "tier candidates require model and optional effort only".into(),
                        ));
                    }
                }
            } else {
                let values = values
                    .iter()
                    .map(|value| {
                        value
                            .as_str()
                            .map(str::to_owned)
                            .ok_or_else(|| RuntimeError::InvalidOption(non_string_array.into()))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                if row.definition.kind == SettingKind::KeyBindings {
                    let action = crate::presentation::KeyBindingAction::from_id(&key[5..])
                        .ok_or_else(|| {
                            RuntimeError::InvalidOption(format!(
                                "unknown keybinding action: {}",
                                &key[5..]
                            ))
                        })?;
                    crate::presentation::ResolvedKeyBindings::validate_override(
                        &self.snapshot(),
                        action,
                        &values,
                    )
                    .map_err(RuntimeError::InvalidOption)?;
                }
            }
        }
        if row.definition.kind == SettingKind::NonNegativeInteger
            && raw_value.parse::<usize>().is_err()
        {
            return Err(RuntimeError::InvalidOption(format!(
                "{key} must be a non-negative integer"
            )));
        }
        let normalized = match (key, &row.definition.kind) {
            ("shell.safe_level", SettingKind::TomlChoices(_)) => shell_safe_level_value(raw_value)
                .ok_or_else(|| RuntimeError::InvalidOption("invalid shell safety level".into()))?
                .to_string(),
            (_, SettingKind::TomlChoices(_))
                if raw_value.starts_with('[') || raw_value.starts_with('{') =>
            {
                raw_value.to_owned()
            }
            (_, SettingKind::TomlChoices(_)) if raw_value != "false" => {
                toml::Value::String(raw_value.to_owned()).to_string()
            }
            (_, SettingKind::ModelPicker | SettingKind::DefaultAgentPicker(_)) => {
                toml::Value::String(raw_value.to_owned()).to_string()
            }
            _ => raw_value.to_owned(),
        };
        self.set_value(key, &normalized)?;
        Ok(())
    }
}

fn setting_definitions(snapshot: &ConfigSnapshot) -> Result<Vec<SettingDefinition>, RuntimeError> {
    let theme_colors = UiColors::for_theme(snapshot.ui_theme());
    let enabled_modes = snapshot.enabled_modes()?;
    let inherited_plan_exit_mode = enabled_modes
        .iter()
        .find(|mode| mode.name == snapshot.default_mode() && mode.cycleable && !mode.plan)
        .or_else(|| {
            enabled_modes
                .iter()
                .find(|mode| mode.cycleable && !mode.plan)
        })
        .map(|mode| mode.name.clone())
        .unwrap_or_else(|| "edit".into());
    let modes = enabled_modes
        .iter()
        .map(|mode| (mode.name.clone(), mode.description.clone()))
        .collect::<Vec<_>>();
    let plan_exit_modes = enabled_modes
        .into_iter()
        .filter(|mode| mode.cycleable && !mode.plan)
        .map(|mode| (mode.name, mode.description))
        .collect::<Vec<_>>();
    let agents = snapshot
        .agent_catalog()?
        .user_profiles()
        .map(|(name, profile)| (name.clone(), profile.description.clone()))
        .collect::<Vec<_>>();
    let mut rows = vec![
        definition(
            "default_mode",
            "Default mode",
            "mode used by future sessions",
            SettingKind::List(modes),
            "edit",
        ),
        definition(
            "default_plan_exit_mode",
            "Default plan exit mode",
            "implementation mode initially selected after completing a plan",
            SettingKind::List(plan_exit_modes),
            &inherited_plan_exit_mode,
        ),
        definition(
            "tiers.small",
            "Small model tier",
            "explicit candidates tried before Cagent's built-in small models",
            SettingKind::TomlArray,
            "[]",
        ),
        definition(
            "default_agent",
            "Default agent",
            "agent profile used by future sessions",
            SettingKind::DefaultAgentPicker(agents),
            DEFAULT_AGENT_NAME,
        ),
        definition(
            "subagents.strategy",
            "Sub-agent strategy",
            "how readily primary agents delegate work",
            SettingKind::List(vec![
                choice("off", "reject ordinary delegation requests"),
                choice(
                    "on_demand",
                    "delegate only when the task clearly requires it",
                ),
                choice(
                    "complex",
                    "keep implementation primary; delegate independent work",
                ),
                choice("aggressive", "proactively seek useful work to delegate"),
                choice("always", "delegate all substantive work"),
            ]),
            "complex",
        ),
        definition(
            "subagents.max_concurrent",
            "Max concurrent",
            "maximum active sub-agents; zero disables delegation",
            SettingKind::NonNegativeInteger,
            &DEFAULT_SUBAGENT_MAX_CONCURRENT.to_string(),
        ),
        definition(
            "compaction.enabled",
            "Compaction enabled",
            "automatically compact conversation context",
            SettingKind::Boolean,
            "true",
        ),
        definition(
            "compaction.threshold_percent",
            "Compaction threshold",
            "context percentage that triggers automatic compaction",
            SettingKind::PositiveInteger,
            &DEFAULT_COMPACTION_THRESHOLD_PERCENT.to_string(),
        ),
        definition(
            "title_generation.enabled",
            "Title generation enabled",
            "refine conversation titles after the first message",
            SettingKind::Boolean,
            "true",
        ),
        definition(
            "automatic_recaps",
            "Automatic recaps",
            "summarize completed conversations after they become idle",
            SettingKind::Boolean,
            "true",
        ),
        definition(
            "recap_idle_seconds",
            "Recap idle time",
            "seconds of conversation inactivity before generating a recap",
            SettingKind::PositiveInteger,
            &DEFAULT_RECAP_IDLE_SECONDS.to_string(),
        ),
        definition(
            "shell.safe_level",
            "Shell safety level",
            "highest built-in shell safety class allowed without approval",
            SettingKind::TomlChoices(vec![
                choice("Read-only", "static commands that only inspect data"),
                choice(
                    "Checks and inspection",
                    "checks that do not intentionally edit source or run project code",
                ),
                choice(
                    "Builds, tests, and analysis",
                    "commands that may run project code or create artifacts",
                ),
                choice(
                    "Formatters and fixers",
                    "commands intended to modify source files",
                ),
            ]),
            "Formatters and fixers",
        ),
        definition(
            "shell.terminal_mode",
            "Terminal mode",
            "terminal capabilities for supervised shell commands",
            SettingKind::TomlChoices(vec![
                choice("normal", "enable interactive terminal capabilities"),
                choice("dumb", "use plain non-interactive terminal behavior"),
            ]),
            "normal",
        ),
        definition(
            "web_fetch.redirects.generally_safe",
            "Generally safe web redirects",
            "reuse this fetch's approval for HTTP-to-HTTPS and www canonical redirects",
            SettingKind::Boolean,
            "true",
        ),
        definition(
            "web_fetch.redirects.same_site",
            "Same-site web redirects",
            "reuse this fetch's approval when a same-site redirect changes its path or query",
            SettingKind::Boolean,
            "true",
        ),
        definition(
            "ui.show_tips",
            "Show tips",
            "show a tip in fresh conversations",
            SettingKind::Boolean,
            "true",
        ),
        definition(
            "ui.collapse_tool_activity",
            "Collapse tool activity",
            "collapse consecutive reads, lists, searches, edits, and commands into a summary",
            SettingKind::Boolean,
            "true",
        ),
        definition(
            "ui.open_links",
            "Open web links",
            "open HTTP(S) Markdown links in the default browser with a single left click",
            SettingKind::Boolean,
            "true",
        ),
        definition(
            "ui.theme",
            "Theme",
            "terminal color scheme",
            SettingKind::List(vec![
                choice("dark", "colors for dark terminal backgrounds"),
                choice("light", "colors for light terminal backgrounds"),
            ]),
            "dark",
        ),
        definition(
            "ui.colors.background",
            "Background color",
            "application background; reset uses the terminal default",
            SettingKind::String,
            "terminal default",
        ),
        definition(
            "ui.colors.foreground",
            "Foreground color",
            "main text color; reset uses the theme default",
            SettingKind::String,
            &theme_colors.foreground.to_string(),
        ),
        definition(
            "ui.colors.surface",
            "Surface color",
            "composer, message, and selected-row background",
            SettingKind::String,
            &theme_colors.surface.to_string(),
        ),
        definition(
            "ui.colors.highlight",
            "Highlight color",
            "links, selections, paths, and primary accents",
            SettingKind::String,
            &theme_colors.highlight.to_string(),
        ),
        definition(
            "ui.colors.bash",
            "Bash color",
            "Bash composer marker",
            SettingKind::String,
            &theme_colors.bash.to_string(),
        ),
        definition(
            "ui.colors.muted",
            "Muted color",
            "secondary labels, hints, and borders",
            SettingKind::String,
            &theme_colors.muted.to_string(),
        ),
        definition(
            "ui.colors.diff_added_background",
            "Added-line background",
            "background for added diff lines",
            SettingKind::String,
            &theme_colors.diff_added_background.to_string(),
        ),
        definition(
            "ui.colors.diff_removed_background",
            "Removed-line background",
            "background for removed diff lines",
            SettingKind::String,
            &theme_colors.diff_removed_background.to_string(),
        ),
        definition(
            "ui.scrollback_reflow_rows",
            "Scrollback reflow rows",
            "maximum transcript rows reflowed after resize",
            SettingKind::PositiveInteger,
            "2000",
        ),
        definition(
            "ui.diff_context_lines",
            "Diff context lines",
            "unchanged lines shown around file edits",
            SettingKind::PositiveInteger,
            "3",
        ),
        definition(
            "ui.diff_mode",
            "Default diff mode",
            "default source of changes in the diff viewer",
            SettingKind::List(vec![
                choice(
                    UiDiffMode::Conversation.as_str(),
                    "changes made during this conversation",
                ),
                choice(UiDiffMode::Git.as_str(), "repository working-copy changes"),
            ]),
            UiDiffMode::default().as_str(),
        ),
        definition(
            "ui.composer_max_rows",
            "Composer max rows",
            "maximum composer height; reset means unlimited",
            SettingKind::PositiveInteger,
            "unlimited",
        ),
        definition(
            "ui.files.width",
            "Files sidebar width",
            "default /files sidebar width in terminal columns",
            SettingKind::PositiveInteger,
            "32",
        ),
        definition(
            "ui.title.requested_input",
            "Requested-input title",
            "terminal title marker while user input is requested",
            SettingKind::TomlChoices(vec![
                choice("warning", "show  before the title"),
                choice("warnings", "alternate  and  before the title"),
                choice("triangle", "show ▲ before the title"),
                choice("triangles", "alternate ▲ and △ before the title"),
                choice("exclamation", "alternate ! and . before the title"),
                choice("custom", "enter a custom TOML string array"),
            ]),
            "warning",
        ),
        definition(
            "ui.title.progress",
            "Title progress spinner",
            "spinner style shown in the terminal title while work is active",
            SettingKind::TomlChoices(vec![
                choice("false", "hide progress from the terminal title"),
                choice("braille", "braille-dot spinner"),
                choice("quadrant", "rotating quadrant spinner"),
                choice("arc", "rotating arc spinner"),
                choice("circle", "pulsing circle spinner"),
                choice("line", "rotating line spinner"),
                choice("block", "moving block spinner"),
                choice("dots", "animated dots spinner"),
                choice("nerd_circle", "Nerd Font circle spinner"),
                choice("nerd_arrow", "Nerd Font arrow spinner"),
                choice("custom", "enter a custom TOML string array"),
            ]),
            "false",
        ),
        definition(
            "ui.progress_osc",
            "OSC progress",
            "report active work with terminal OSC 9;4 progress sequences",
            SettingKind::Boolean,
            "true",
        ),
        definition(
            "ui.bell.requested_input",
            "Bell on requested input",
            "notify when user input is required",
            SettingKind::Boolean,
            "true",
        ),
        definition(
            "ui.bell.completed_turn",
            "Bell on completed turn",
            "notify when a full turn completes normally",
            SettingKind::Boolean,
            "true",
        ),
        definition(
            "ui.bell.method",
            "Bell method",
            "terminal notification method",
            SettingKind::TomlChoices(vec![
                choice("osc777", "send a desktop notification through OSC 777"),
                choice("bell", "emit a terminal bell (BEL)"),
            ]),
            "osc777",
        ),
        definition(
            "ui.editor",
            "Path editor",
            "open paths in the built-in viewer, a background GUI, or a foreground terminal editor",
            SettingKind::TomlChoices(vec![
                choice("builtin", "open paths in Cagent's built-in viewer"),
                choice("false", "disable path opening"),
                choice("vscode", "open paths in Visual Studio Code"),
                choice(
                    "vscode-insiders",
                    "open paths in Visual Studio Code Insiders",
                ),
                choice("cursor", "open paths in Cursor"),
                choice("zed", "open paths in Zed"),
                choice("zed-preview", "open paths in Zed Preview"),
                choice("intellij", "open paths in IntelliJ IDEA"),
                choice("webstorm", "open paths in WebStorm"),
                choice("pycharm", "open paths in PyCharm"),
                choice("rustrover", "open paths in RustRover"),
                choice("goland", "open paths in GoLand"),
                choice("clion", "open paths in CLion"),
                choice("rider", "open paths in Rider"),
                choice("fleet", "open paths in Fleet"),
                choice("sublime", "open paths in Sublime Text"),
                choice("lapce", "open paths in Lapce"),
                choice("emacs", "open paths in Emacs"),
                choice("neovim", "open paths in Neovim"),
                choice("vim", "open paths in Vim"),
                choice("helix", "open paths in Helix"),
                choice("custom", "enter a custom command and launch mode"),
            ]),
            "builtin",
        ),
        definition(
            "ui.file_picker.respect_gitignore",
            "Respect .gitignore",
            "hide ignored paths from file completion",
            SettingKind::Boolean,
            "true",
        ),
        definition(
            "ui.file_picker.hide_hidden_files",
            "Hide hidden files",
            "hide dotfiles and dot-directories from file completion",
            SettingKind::Boolean,
            "true",
        ),
        definition(
            "limits.attachment_bytes",
            "Attachment bytes",
            "implicit attachment size limit",
            SettingKind::PositiveInteger,
            &DEFAULT_ATTACHMENT_BYTES.to_string(),
        ),
        definition(
            "limits.attachment_hard_cap_bytes",
            "Attachment hard cap bytes",
            "maximum explicit attachment size",
            SettingKind::PositiveInteger,
            &DEFAULT_ATTACHMENT_HARD_CAP_BYTES.to_string(),
        ),
        definition(
            "limits.resize_images",
            "Resize images",
            "resize model transport copies to a 2048px maximum dimension",
            SettingKind::Boolean,
            "true",
        ),
    ];
    rows.extend(
        crate::presentation::KeyBindingAction::ALL
            .into_iter()
            .map(|action| {
                definition(
                    &format!("keys.{}", action.id()),
                    &format!("Key: {}", action.id().replace('_', " ")),
                    action.description(),
                    SettingKind::KeyBindings,
                    &action.default_source(),
                )
            }),
    );
    Ok(rows)
}

fn definition(
    key: &str,
    label: &str,
    description: &str,
    kind: SettingKind,
    default_value: &str,
) -> SettingDefinition {
    SettingDefinition {
        key: key.into(),
        label: label.into(),
        description: description.into(),
        kind,
        section: setting_section(key),
        default_value: default_value.into(),
    }
}

fn choice(value: &str, description: &str) -> (String, String) {
    (value.into(), description.into())
}

fn shell_safe_level_value(label: &str) -> Option<i8> {
    match label {
        "Read-only" => Some(0),
        "Checks and inspection" => Some(1),
        "Builds, tests, and analysis" => Some(2),
        "Formatters and fixers" => Some(3),
        _ => None,
    }
}

fn shell_safe_level_label(level: i8) -> &'static str {
    match level {
        -1 => "Disabled",
        0 => "Read-only",
        1 => "Checks and inspection",
        2 => "Builds, tests, and analysis",
        3 => "Formatters and fixers",
        _ => "Unknown",
    }
}

fn setting_section(key: &str) -> SettingSection {
    if key.starts_with("keys.") {
        SettingSection::Keybindings
    } else if key.starts_with("limits.") {
        SettingSection::Limits
    } else if key.starts_with("ui.file_picker.") || key.starts_with("ui.files.") {
        SettingSection::Files
    } else if key.starts_with("ui.") {
        SettingSection::Ui
    } else {
        SettingSection::General
    }
}

fn effective_value(snapshot: &ConfigSnapshot, key: &str) -> Option<String> {
    Some(match key {
        "default_mode" => snapshot.default_mode().to_owned(),
        "default_plan_exit_mode" => snapshot.default_plan_exit_mode().to_owned(),
        "tiers.small" => tier_candidates_value(snapshot.tier("small").unwrap_or_default()),
        "default_agent" => snapshot.default_agent().to_owned(),
        "subagents.strategy" => snapshot.delegation_policy().as_str().into(),
        "subagents.max_concurrent" => snapshot.subagent_max_concurrent().to_string(),
        "compaction.enabled" => snapshot.compaction().enabled.to_string(),
        "compaction.threshold_percent" => snapshot.compaction().threshold_percent.to_string(),
        "title_generation.enabled" => snapshot.title_generation_enabled().to_string(),
        "automatic_recaps" => snapshot.automatic_recaps().to_string(),
        "recap_idle_seconds" => snapshot.recap_idle_seconds().to_string(),
        "shell.safe_level" => shell_safe_level_label(snapshot.shell().safe_level).into(),
        "shell.terminal_mode" => match snapshot.shell().terminal_mode {
            crate::config::TerminalMode::Normal => "normal".into(),
            crate::config::TerminalMode::Dumb => "dumb".into(),
        },
        "web_fetch.redirects.generally_safe" => {
            snapshot.web_fetch_redirects().generally_safe().to_string()
        }
        "web_fetch.redirects.same_site" => snapshot.web_fetch_redirects().same_site().to_string(),
        "ui.show_tips" => snapshot.show_tips().to_string(),
        "ui.collapse_tool_activity" => snapshot.collapse_tool_activity().to_string(),
        "ui.open_links" => snapshot.open_links().to_string(),
        "ui.theme" => snapshot.ui_theme().as_str().into(),
        "ui.colors.background" => snapshot
            .ui_colors()
            .background
            .map_or_else(|| "terminal default".into(), |color| color.to_string()),
        "ui.colors.foreground" => snapshot.ui_colors().foreground.to_string(),
        "ui.colors.surface" => snapshot.ui_colors().surface.to_string(),
        "ui.colors.highlight" => snapshot.ui_colors().highlight.to_string(),
        "ui.colors.bash" => snapshot.ui_colors().bash.to_string(),
        "ui.colors.muted" => snapshot.ui_colors().muted.to_string(),
        "ui.colors.diff_added_background" => snapshot.ui_colors().diff_added_background.to_string(),
        "ui.colors.diff_removed_background" => {
            snapshot.ui_colors().diff_removed_background.to_string()
        }
        "ui.editor" => editor_setting_value(snapshot),
        "ui.title.requested_input" => requested_input_value(snapshot),
        "ui.title.progress" => progress_setting_value(snapshot),
        "ui.progress_osc" => snapshot.progress_osc().to_string(),
        "ui.bell.requested_input" => snapshot.bell().requested_input().to_string(),
        "ui.bell.completed_turn" => snapshot.bell().completed_turn().to_string(),
        "ui.bell.method" => snapshot.bell().method().as_str().into(),
        "ui.scrollback_reflow_rows" => snapshot.scrollback_reflow_rows().to_string(),
        "ui.diff_context_lines" => snapshot.diff_context_lines().to_string(),
        "ui.diff_mode" => snapshot.diff_mode().as_str().into(),
        "ui.composer_max_rows" => snapshot
            .composer_max_rows()
            .map_or_else(|| "unlimited".into(), |value| value.to_string()),
        "ui.files.width" => snapshot.files_width().to_string(),
        "ui.file_picker.respect_gitignore" => snapshot.file_picker_respect_gitignore().to_string(),
        "ui.file_picker.hide_hidden_files" => snapshot.file_picker_hide_hidden_files().to_string(),
        "limits.attachment_bytes" => snapshot.attachment_bytes().to_string(),
        "limits.attachment_hard_cap_bytes" => snapshot.attachment_hard_cap_bytes().to_string(),
        "limits.resize_images" => snapshot.resize_images().to_string(),
        key if key.starts_with("keys.") => snapshot
            .key_bindings()
            .get(&key[5..])
            .cloned()
            .flatten()
            .map_or_else(
                || {
                    crate::presentation::KeyBindingAction::from_id(&key[5..]).map_or_else(
                        || "[]".into(),
                        crate::presentation::KeyBindingAction::default_source,
                    )
                },
                |values| {
                    let values = values.into_iter().flatten().collect::<Vec<_>>();
                    toml::Value::Array(values.into_iter().map(toml::Value::String).collect())
                        .to_string()
                },
            ),
        _ => return None,
    })
}

fn tier_candidates_value(candidates: &[super::TierCandidate]) -> String {
    let entries = candidates
        .iter()
        .map(|candidate| {
            let model = toml_edit::Value::from(candidate.model.to_string());
            candidate.effort.as_ref().map_or_else(
                || format!("{{ model = {model} }}"),
                |effort| {
                    let effort = toml_edit::Value::from(effort.clone());
                    format!("{{ model = {model}, effort = {effort} }}")
                },
            )
        })
        .collect::<Vec<_>>();
    format!("[{}]", entries.join(", "))
}

fn editor_setting_value(snapshot: &ConfigSnapshot) -> String {
    use super::{UiEditor, UiEditorMode};

    match snapshot.editor() {
        UiEditor::Disabled => "false".into(),
        UiEditor::BuiltIn => "builtin".into(),
        UiEditor::Command {
            command,
            line_command,
            mode,
            ..
        } => {
            let preset = match (command.as_slice(), mode) {
                ([executable], UiEditorMode::Background) => match executable.as_str() {
                    "code" => Some("vscode"),
                    "code-insiders" => Some("vscode-insiders"),
                    "idea" => Some("intellij"),
                    "subl" => Some("sublime"),
                    known @ ("cursor" | "zed" | "zed-preview" | "webstorm" | "pycharm"
                    | "rustrover" | "goland" | "clion" | "rider" | "fleet" | "lapce"
                    | "emacs") => Some(known),
                    _ => None,
                },
                ([executable], UiEditorMode::Foreground) => match executable.as_str() {
                    "nvim" => Some("neovim"),
                    "vim" => Some("vim"),
                    "hx" => Some("helix"),
                    _ => None,
                },
                _ => None,
            };
            preset
                .filter(|_| {
                    line_command.as_deref().is_some_and(|line_command| {
                        preset_line_command_matches(
                            command.first().map(String::as_str),
                            line_command,
                        )
                    })
                })
                .map_or_else(
                    || editor_command_setting_value(command, line_command.as_deref(), *mode),
                    str::to_owned,
                )
        }
    }
}

fn preset_line_command_matches(executable: Option<&str>, actual: &[String]) -> bool {
    let Some(executable) = executable else {
        return false;
    };
    let expected: Vec<&str> = match executable {
        "code" | "code-insiders" | "cursor" => vec![executable, "--goto", "{path}:{line}"],
        "zed" | "zed-preview" | "fleet" | "subl" | "lapce" => {
            vec![executable, "{path}:{line}"]
        }
        "idea" | "webstorm" | "pycharm" | "rustrover" | "goland" | "clion" | "rider" => {
            vec![executable, "--line", "{line}", "{path}"]
        }
        "emacs" | "nvim" | "vim" | "hx" => vec![executable, "+{line}", "{path}"],
        _ => return false,
    };
    actual
        .iter()
        .map(String::as_str)
        .eq(expected.iter().copied())
}

fn editor_command_setting_value(
    command: &[String],
    line_command: Option<&[String]>,
    mode: super::UiEditorMode,
) -> String {
    let command =
        toml::Value::Array(command.iter().cloned().map(toml::Value::String).collect()).to_string();
    if mode == super::UiEditorMode::Foreground && line_command.is_none() {
        command
    } else {
        let mode = toml::Value::String(mode.as_str().into()).to_string();
        let line_command = line_command
            .map(|values| {
                let values =
                    toml::Value::Array(values.iter().cloned().map(toml::Value::String).collect());
                format!(", line_command = {values}")
            })
            .unwrap_or_default();
        format!("{{ command = {command}{line_command}, mode = {mode} }}")
    }
}

fn requested_input_value(snapshot: &ConfigSnapshot) -> String {
    let frames = snapshot.title().requested_input().frames();
    match frames {
        [frame] if frame == "" => "warning".into(),
        [first, second] if first == "" && second == "" => "warnings".into(),
        [frame] if frame == "▲" => "triangle".into(),
        [first, second] if first == "▲" && second == "△" => "triangles".into(),
        [first, second] if first == "!" && second == "." => "exclamation".into(),
        _ => "custom".into(),
    }
}

fn progress_setting_value(snapshot: &ConfigSnapshot) -> String {
    snapshot.title().progress().as_str().into()
}

fn format_byte_count(bytes: u64) -> String {
    const KILOBYTE: u64 = 1_000;
    const MEGABYTE: u64 = 1_000_000;
    const GIGABYTE: u64 = 1_000_000_000;

    if bytes < 10 * KILOBYTE {
        return format_integer(bytes) + "b";
    }
    let (value, suffix) = if bytes >= GIGABYTE {
        (bytes as f64 / GIGABYTE as f64, "gb")
    } else if bytes >= MEGABYTE {
        (bytes as f64 / MEGABYTE as f64, "mb")
    } else {
        (bytes as f64 / KILOBYTE as f64, "kb")
    };
    if value < 10.0 {
        format!("{value:.1}{suffix}")
    } else {
        format!("{value:.0}{suffix}")
    }
}

fn format_integer(value: u64) -> String {
    let source = value.to_string();
    let mut formatted = String::with_capacity(source.len() + source.len() / 3);
    for (index, character) in source.chars().enumerate() {
        if index > 0 && (source.len() - index).is_multiple_of(3) {
            formatted.push(',');
        }
        formatted.push(character);
    }
    formatted
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn diff_mode_setting_exposes_choices_persists_and_resets() {
        let temporary = TempDir::new().unwrap();
        let path = temporary.path().join("config.toml");
        std::fs::write(&path, "version = 1\n").unwrap();
        let store = ConfigStore::open(&path).unwrap();
        let row = || {
            store
                .settings()
                .unwrap()
                .into_iter()
                .find(|row| row.definition.key == "ui.diff_mode")
                .unwrap()
        };
        let default = row();
        assert_eq!(default.definition.label, "Default diff mode");
        assert_eq!(default.definition.section, SettingSection::Ui);
        assert_eq!(default.definition.default_value, "conversation");
        assert_eq!(default.effective_value, "conversation");
        assert_eq!(default.display_value, "conversation");
        assert_eq!(default.explicit_value, None);
        assert!(matches!(default.definition.kind, SettingKind::List(choices)
            if choices.iter().map(|(value, _)| value.as_str()).collect::<Vec<_>>()
                == ["conversation", "git"]));

        for mode in [UiDiffMode::Git, UiDiffMode::Conversation] {
            let raw = serde_json::to_string(mode.as_str()).unwrap();
            store.save_setting("ui.diff_mode", &raw).unwrap();
            assert_eq!(store.snapshot().diff_mode(), mode);
            assert_eq!(ConfigSnapshot::load(&path).unwrap().diff_mode(), mode);
            assert_eq!(row().effective_value, mode.as_str());
            assert_eq!(row().explicit_value.as_deref(), Some(raw.as_str()));
        }
        let source = std::fs::read_to_string(&path).unwrap();
        for invalid in ["\"auto\"", "false", "1", "[]"] {
            assert!(store.save_setting("ui.diff_mode", invalid).is_err());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
            assert_eq!(store.snapshot().diff_mode(), UiDiffMode::Conversation);
        }
        store.save_setting("ui.diff_mode", "\"git\"").unwrap();
        store.reset_value("ui.diff_mode").unwrap();
        assert_eq!(row().explicit_value, None);
        assert_eq!(row().effective_value, "conversation");
        assert_eq!(store.snapshot().diff_mode(), UiDiffMode::Conversation);
        assert_eq!(
            ConfigSnapshot::load(&path).unwrap().diff_mode(),
            UiDiffMode::Conversation
        );
    }

    #[test]
    fn diff_mode_reload_publishes_valid_changes_and_keeps_last_good_snapshot() {
        let temporary = TempDir::new().unwrap();
        let path = temporary.path().join("config.toml");
        std::fs::write(&path, "version = 1\n").unwrap();
        let store = ConfigStore::open(&path).unwrap();
        let mut snapshots = store.subscribe();

        std::fs::write(&path, "version = 1\n[ui]\ndiff_mode = 'git'\n").unwrap();
        assert_eq!(store.reload().unwrap().diff_mode(), UiDiffMode::Git);
        assert!(snapshots.has_changed().unwrap());
        assert_eq!(snapshots.borrow_and_update().diff_mode(), UiDiffMode::Git);

        std::fs::write(&path, "version = 1\n[ui]\ndiff_mode = false\n").unwrap();
        assert!(store.reload().is_err());
        assert!(!snapshots.has_changed().unwrap());
        assert_eq!(store.snapshot().diff_mode(), UiDiffMode::Git);

        std::fs::write(&path, "version = 1\n").unwrap();
        assert_eq!(
            store.reload().unwrap().diff_mode(),
            UiDiffMode::Conversation
        );
        assert!(snapshots.has_changed().unwrap());
        assert_eq!(
            snapshots.borrow_and_update().diff_mode(),
            UiDiffMode::Conversation
        );
    }

    #[test]
    fn settings_include_all_key_actions_and_track_explicit_overrides() {
        let temporary = TempDir::new().unwrap();
        let path = temporary.path().join("config.toml");
        std::fs::write(&path, "version = 1\n[ui]\nshow_tips = false\n").unwrap();
        let store = ConfigStore::open(path).unwrap();

        let rows = store.settings().unwrap();
        assert_eq!(
            rows.iter()
                .filter(|row| row.definition.key.starts_with("keys."))
                .count(),
            crate::presentation::KeyBindingAction::ALL.len()
        );
        let tips = rows
            .iter()
            .find(|row| row.definition.key == "ui.show_tips")
            .unwrap();
        assert_eq!(tips.effective_value, "false");
        assert_eq!(tips.explicit_value.as_deref(), Some("false"));
        let files_width = rows
            .iter()
            .find(|row| row.definition.key == "ui.files.width")
            .unwrap();
        assert_eq!(files_width.effective_value, "32");
        assert_eq!(files_width.definition.section, SettingSection::Files);
    }

    #[test]
    fn default_plan_exit_setting_lists_eligible_modes_and_resets_to_inheritance() {
        let temporary = TempDir::new().unwrap();
        let path = temporary.path().join("config.toml");
        std::fs::write(&path, "version = 1\ndefault_mode = 'auto'\n").unwrap();
        let store = ConfigStore::open(path).unwrap();

        let row = store
            .settings()
            .unwrap()
            .into_iter()
            .find(|row| row.definition.key == "default_plan_exit_mode")
            .unwrap();
        assert_eq!(row.effective_value, "auto");
        assert_eq!(row.definition.default_value, "auto");
        assert!(row.explicit_value.is_none());
        assert!(matches!(
            row.definition.kind,
            SettingKind::List(choices)
                if choices.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>()
                    == ["edit", "auto"]
        ));

        store
            .save_setting("default_plan_exit_mode", "edit")
            .unwrap();
        assert_eq!(store.snapshot().default_plan_exit_mode(), "edit");
        store.reset_value("default_plan_exit_mode").unwrap();
        let reset = store
            .settings()
            .unwrap()
            .into_iter()
            .find(|row| row.definition.key == "default_plan_exit_mode")
            .unwrap();
        assert_eq!(reset.effective_value, "auto");
        assert!(reset.explicit_value.is_none());
    }

    #[test]
    fn terminal_mode_is_available_as_a_shell_setting() {
        let temporary = TempDir::new().unwrap();
        let path = temporary.path().join("config.toml");
        std::fs::write(&path, "version = 1\n").unwrap();
        let store = ConfigStore::open(path).unwrap();

        let terminal_mode = store
            .settings()
            .unwrap()
            .into_iter()
            .find(|row| row.definition.key == "shell.terminal_mode")
            .unwrap();
        assert_eq!(terminal_mode.effective_value, "normal");
        assert_eq!(terminal_mode.definition.section, SettingSection::General);
        assert!(matches!(
            &terminal_mode.definition.kind,
            SettingKind::TomlChoices(choices)
                if choices == &[
                    choice("normal", "enable interactive terminal capabilities"),
                    choice("dumb", "use plain non-interactive terminal behavior"),
                ]
        ));

        store.save_setting("shell.terminal_mode", "dumb").unwrap();
        assert_eq!(
            ConfigSnapshot::load(store.path().unwrap())
                .unwrap()
                .shell()
                .terminal_mode,
            crate::config::TerminalMode::Dumb
        );
    }

    #[test]
    fn shell_safe_level_uses_named_choices_and_persists_numeric_level() {
        let temporary = TempDir::new().unwrap();
        let path = temporary.path().join("config.toml");
        std::fs::write(&path, "version = 1\n[shell]\nsafe_level = -1\n").unwrap();
        let store = ConfigStore::open(path).unwrap();

        let row = store
            .settings()
            .unwrap()
            .into_iter()
            .find(|row| row.definition.key == "shell.safe_level")
            .unwrap();
        assert_eq!(row.effective_value, "Disabled");
        assert_eq!(row.display_value, "Disabled");
        assert!(matches!(
            &row.definition.kind,
            SettingKind::TomlChoices(choices)
                if choices.iter().map(|(label, _)| label.as_str()).collect::<Vec<_>>() == [
                    "Read-only",
                    "Checks and inspection",
                    "Builds, tests, and analysis",
                    "Formatters and fixers",
                ]
        ));

        store
            .save_setting("shell.safe_level", "Builds, tests, and analysis")
            .unwrap();
        assert_eq!(
            store.get_value("shell.safe_level").unwrap().as_deref(),
            Some("2")
        );
        assert_eq!(
            ConfigSnapshot::load(store.path().unwrap())
                .unwrap()
                .shell()
                .safe_level,
            2
        );
        assert!(store.save_setting("shell.safe_level", "Disabled").is_err());
        assert!(store.save_setting("shell.safe_level", "2").is_err());
    }

    #[test]
    fn general_settings_expose_runtime_defaults_and_specialized_editors() {
        let temporary = TempDir::new().unwrap();
        let path = temporary.path().join("config.toml");
        std::fs::write(
            &path,
            r#"version = 1
[title_generation]
enabled = true

[agents.review]
description = "Review changes"

[agents.hidden]
enabled = false

[agents.worker]
availability = "subagent"
"#,
        )
        .unwrap();
        let store = ConfigStore::open(path).unwrap();
        let rows = store.settings().unwrap();
        let row = |key: &str| rows.iter().find(|row| row.definition.key == key).unwrap();

        assert!(
            rows.iter()
                .all(|row| row.definition.key != "machine_fingerprint")
        );

        for key in [
            "tiers.small",
            "default_agent",
            "subagents.strategy",
            "subagents.max_concurrent",
            "compaction.enabled",
            "compaction.threshold_percent",
            "title_generation.enabled",
            "automatic_recaps",
            "recap_idle_seconds",
        ] {
            assert_eq!(row(key).definition.section, SettingSection::General);
        }
        assert_eq!(row("tiers.small").definition.label, "Small model tier");
        assert_eq!(row("automatic_recaps").effective_value, "true");
        assert_eq!(row("recap_idle_seconds").effective_value, "180");
        assert_eq!(row("tiers.small").effective_value, "[]");
        assert_eq!(row("tiers.small").definition.kind, SettingKind::TomlArray);
        assert!(matches!(
            &row("default_agent").definition.kind,
            SettingKind::DefaultAgentPicker(choices)
                if choices == &[
                    ("general".into(), "General coding agent".into()),
                    ("review".into(), "Review changes".into()),
                ]
        ));
        assert!(matches!(
            &row("subagents.strategy").definition.kind,
            SettingKind::List(choices)
                if choices == &[
                    choice("off", "reject ordinary delegation requests"),
                    choice("on_demand", "delegate only when the task clearly requires it"),
                    choice(
                        "complex",
                        "keep implementation primary; delegate independent work",
                    ),
                    choice("aggressive", "proactively seek useful work to delegate"),
                    choice("always", "delegate all substantive work"),
                ]
        ));
        assert_eq!(row("subagents.strategy").effective_value, "complex");
        assert_eq!(row("subagents.max_concurrent").effective_value, "10");
        assert_eq!(
            row("subagents.max_concurrent").definition.kind,
            SettingKind::NonNegativeInteger
        );
        assert_eq!(row("compaction.enabled").effective_value, "true");
        assert_eq!(row("compaction.threshold_percent").effective_value, "90");
        assert_eq!(row("title_generation.enabled").effective_value, "true");
        assert!(
            rows.iter()
                .all(|row| row.definition.key != "title_generation.timeout_seconds")
        );
    }

    #[test]
    fn collapse_tool_activity_defaults_on_and_persists() {
        let temporary = TempDir::new().unwrap();
        let path = temporary.path().join("config.toml");
        std::fs::write(&path, "version = 1\n").unwrap();
        let store = ConfigStore::open(path).unwrap();
        let row = store
            .settings()
            .unwrap()
            .into_iter()
            .find(|row| row.definition.key == "ui.collapse_tool_activity")
            .unwrap();
        assert_eq!(row.effective_value, "true");
        assert_eq!(row.definition.section, SettingSection::Ui);

        store
            .save_setting("ui.collapse_tool_activity", "false")
            .unwrap();
        assert!(
            !ConfigSnapshot::load(store.path().unwrap())
                .unwrap()
                .collapse_tool_activity()
        );
        store.reset_value("ui.collapse_tool_activity").unwrap();
        assert!(store.snapshot().collapse_tool_activity());
        assert!(
            store
                .save_setting("ui.collapse_tool_activity", "maybe")
                .is_err()
        );
    }

    #[test]
    fn open_links_defaults_on_and_persists() {
        let temporary = TempDir::new().unwrap();
        let path = temporary.path().join("config.toml");
        std::fs::write(&path, "version = 1\n").unwrap();
        let store = ConfigStore::open(path).unwrap();
        let row = store
            .settings()
            .unwrap()
            .into_iter()
            .find(|row| row.definition.key == "ui.open_links")
            .unwrap();
        assert_eq!(row.effective_value, "true");
        assert_eq!(row.definition.default_value, "true");
        assert_eq!(row.definition.section, SettingSection::Ui);
        assert_eq!(row.definition.kind, SettingKind::Boolean);

        store.save_setting("ui.open_links", "false").unwrap();
        assert!(!store.snapshot().open_links());
        assert!(
            !ConfigSnapshot::load(store.path().unwrap())
                .unwrap()
                .open_links()
        );
        let saved = std::fs::read_to_string(store.path().unwrap()).unwrap();
        assert!(store.save_setting("ui.open_links", "maybe").is_err());
        assert!(!store.snapshot().open_links());
        assert_eq!(
            std::fs::read_to_string(store.path().unwrap()).unwrap(),
            saved
        );

        store.reset_value("ui.open_links").unwrap();
        assert!(store.snapshot().open_links());
        assert!(
            ConfigSnapshot::load(store.path().unwrap())
                .unwrap()
                .open_links()
        );
    }

    #[test]
    fn general_runtime_settings_validate_persist_and_reset_atomically() {
        let temporary = TempDir::new().unwrap();
        let path = temporary.path().join("config.toml");
        std::fs::write(&path, "# retained\nversion = 1\n").unwrap();
        let store = ConfigStore::open(path).unwrap();

        store
            .save_setting(
                "tiers.small",
                r#"[{ model = "openai/gpt-small", effort = "low" }, { model = "anthropic/claude-small" }]"#,
            )
            .unwrap();
        store.save_setting("default_agent", "general").unwrap();
        store
            .save_setting("subagents.strategy", r#""always""#)
            .unwrap();
        store.save_setting("subagents.max_concurrent", "0").unwrap();
        store.save_setting("compaction.enabled", "false").unwrap();
        store
            .save_setting("compaction.threshold_percent", "75")
            .unwrap();
        store
            .save_setting("title_generation.enabled", "false")
            .unwrap();

        let snapshot = ConfigSnapshot::load(store.path().unwrap()).unwrap();
        assert_eq!(
            snapshot
                .tier("small")
                .unwrap()
                .iter()
                .map(|candidate| candidate.model.to_string())
                .collect::<Vec<_>>(),
            ["openai/gpt-small", "anthropic/claude-small"]
        );
        assert_eq!(
            snapshot.tier("small").unwrap()[0].effort.as_deref(),
            Some("low")
        );
        assert_eq!(snapshot.delegation_policy().as_str(), "always");
        assert_eq!(snapshot.subagent_max_concurrent(), 0);
        assert!(!snapshot.compaction().enabled);
        assert_eq!(snapshot.compaction().threshold_percent, 75);
        assert!(!snapshot.title_generation_enabled());
        assert!(store.source().unwrap().contains("# retained"));

        for (key, invalid) in [
            ("tiers.small", "not-an-array"),
            ("tiers.small", "[1]"),
            ("tiers.small", "[{ tier = 'other' }]"),
            ("subagents.max_concurrent", "-1"),
            ("subagents.max_concurrent", "many"),
            ("compaction.threshold_percent", "0"),
            ("compaction.threshold_percent", "101"),
            ("compaction.enabled", "maybe"),
            ("title_generation.enabled", "maybe"),
        ] {
            assert!(
                store.save_setting(key, invalid).is_err(),
                "{key} accepted {invalid}"
            );
        }

        store.reset_value("tiers.small").unwrap();
        assert!(store.snapshot().tier("small").unwrap().is_empty());
        assert_eq!(
            store
                .settings()
                .unwrap()
                .into_iter()
                .find(|row| row.definition.key == "tiers.small")
                .unwrap()
                .effective_value,
            "[]"
        );
    }

    #[test]
    fn editor_settings_preserve_preset_and_custom_launch_modes() {
        let temporary = TempDir::new().unwrap();
        let path = temporary.path().join("config.toml");
        std::fs::write(&path, "version = 1\n").unwrap();
        let store = ConfigStore::open(path).unwrap();

        store.save_setting("ui.editor", "vscode").unwrap();
        assert_eq!(
            ConfigSnapshot::load(store.path().unwrap())
                .unwrap()
                .editor()
                .mode(),
            Some(crate::config::UiEditorMode::Background)
        );

        store
            .save_setting(
                "ui.editor",
                "{ command = [\"emacs\", \"-nw\"], mode = \"foreground\" }",
            )
            .unwrap();
        let snapshot = ConfigSnapshot::load(store.path().unwrap()).unwrap();
        assert_eq!(
            snapshot.editor(),
            &crate::config::UiEditor::Command {
                command: vec!["emacs".into(), "-nw".into()],
                line_command: None,
                fallback_executables: Vec::new(),
                mode: crate::config::UiEditorMode::Foreground,
            }
        );
        let row = store
            .settings()
            .unwrap()
            .into_iter()
            .find(|row| row.definition.key == "ui.editor")
            .unwrap();
        assert!(row.explicit_value.unwrap().starts_with("{ command ="));
    }

    #[test]
    fn title_settings_expose_presets_arrays_and_spinner_choices() {
        let temporary = TempDir::new().unwrap();
        let path = temporary.path().join("config.toml");
        std::fs::write(&path, "version = 1\n").unwrap();
        let store = ConfigStore::open(path).unwrap();

        let rows = store.settings().unwrap();
        let requested_input = rows
            .iter()
            .find(|row| row.definition.key == "ui.title.requested_input")
            .unwrap();
        assert_eq!(requested_input.effective_value, "warning");
        assert_eq!(requested_input.definition.section, SettingSection::Ui);
        assert!(matches!(
            &requested_input.definition.kind,
            SettingKind::TomlChoices(choices)
                if choices.iter().map(|(value, _)| value.as_str()).collect::<Vec<_>>()
                    == [
                        "warning",
                        "warnings",
                        "triangle",
                        "triangles",
                        "exclamation",
                        "custom"
                    ]
        ));

        let progress = rows
            .iter()
            .find(|row| row.definition.key == "ui.title.progress")
            .unwrap();
        assert_eq!(progress.effective_value, "false");
        assert!(matches!(
            &progress.definition.kind,
            SettingKind::TomlChoices(choices)
                if choices.iter().any(|(choice, _)| choice == "braille")
        ));

        store
            .save_setting("ui.title.requested_input", "triangles")
            .unwrap();
        assert_eq!(
            ConfigSnapshot::load(store.path().unwrap())
                .unwrap()
                .title()
                .requested_input()
                .frames(),
            ["▲", "△"]
        );
        store
            .save_setting("ui.title.requested_input", "[\"!\", \"!!\"]")
            .unwrap();
        assert_eq!(
            ConfigSnapshot::load(store.path().unwrap())
                .unwrap()
                .title()
                .requested_input()
                .frames(),
            ["!", "!!"]
        );
        assert_eq!(
            store
                .settings()
                .unwrap()
                .into_iter()
                .find(|row| row.definition.key == "ui.title.requested_input")
                .unwrap()
                .effective_value,
            "custom"
        );
        store.save_setting("ui.title.progress", "braille").unwrap();
        assert_eq!(
            ConfigSnapshot::load(store.path().unwrap())
                .unwrap()
                .title()
                .progress(),
            &crate::config::ProgressMode::Braille
        );
        store
            .save_setting("ui.title.progress", "[\".\", \"..\", \"...\"]")
            .unwrap();
        assert_eq!(
            ConfigSnapshot::load(store.path().unwrap())
                .unwrap()
                .title()
                .progress()
                .frame(2),
            Some("...")
        );
        assert!(store.save_setting("ui.title.progress", "invalid").is_err());
        store.save_setting("ui.progress_osc", "false").unwrap();
        assert!(
            !ConfigSnapshot::load(store.path().unwrap())
                .unwrap()
                .progress_osc()
        );
    }

    #[test]
    fn bell_settings_are_exposed_and_persist_together() {
        let temporary = TempDir::new().unwrap();
        let path = temporary.path().join("config.toml");
        std::fs::write(&path, "version = 1\n").unwrap();
        let store = ConfigStore::open(path).unwrap();
        let rows = store.settings().unwrap();

        for key in [
            "ui.bell.requested_input",
            "ui.bell.completed_turn",
            "ui.bell.method",
        ] {
            let row = rows.iter().find(|row| row.definition.key == key).unwrap();
            assert_eq!(row.definition.section, SettingSection::Ui);
        }
        assert!(matches!(
            rows.iter()
                .find(|row| row.definition.key == "ui.bell.requested_input")
                .unwrap()
                .definition
                .kind,
            SettingKind::Boolean
        ));
        assert!(matches!(
            &rows
                .iter()
                .find(|row| row.definition.key == "ui.bell.method")
                .unwrap()
                .definition
                .kind,
            SettingKind::TomlChoices(choices)
                if choices.iter().map(|(value, _)| value.as_str()).collect::<Vec<_>>()
                    == ["osc777", "bell"]
        ));

        store
            .save_setting("ui.bell.requested_input", "false")
            .unwrap();
        store
            .save_setting("ui.bell.completed_turn", "false")
            .unwrap();
        store.save_setting("ui.bell.method", "bell").unwrap();
        let bell = ConfigSnapshot::load(store.path().unwrap()).unwrap().bell();
        assert_eq!(
            bell,
            crate::config::BellConfig::new(false, false, crate::config::BellMethod::Bell)
        );
        assert!(store.save_setting("ui.bell.method", "desktop").is_err());
        assert!(
            store
                .save_setting("ui.bell.requested_input", "not-a-boolean")
                .is_err()
        );

        store.reset_value("ui.bell.requested_input").unwrap();
        assert!(
            ConfigSnapshot::load(store.path().unwrap())
                .unwrap()
                .bell()
                .requested_input()
        );
    }

    #[test]
    fn web_fetch_redirect_settings_are_exposed_and_persisted() {
        let temporary = TempDir::new().unwrap();
        let path = temporary.path().join("config.toml");
        std::fs::write(&path, "version = 1\n").unwrap();
        let store = ConfigStore::open(path).unwrap();
        for key in [
            "web_fetch.redirects.generally_safe",
            "web_fetch.redirects.same_site",
        ] {
            let row = store
                .settings()
                .unwrap()
                .into_iter()
                .find(|row| row.definition.key == key)
                .unwrap();
            assert_eq!(row.definition.section, SettingSection::General);
            assert_eq!(row.effective_value, "true");
            assert!(matches!(row.definition.kind, SettingKind::Boolean));
        }

        store
            .save_setting("web_fetch.redirects.generally_safe", "false")
            .unwrap();
        store
            .save_setting("web_fetch.redirects.same_site", "false")
            .unwrap();
        let redirects = ConfigSnapshot::load(store.path().unwrap())
            .unwrap()
            .web_fetch_redirects();
        assert!(!redirects.generally_safe());
        assert!(!redirects.same_site());
    }

    #[test]
    fn keybinding_setting_accepts_empty_arrays_and_rejects_invalid_or_conflicting_values() {
        let temporary = TempDir::new().unwrap();
        let path = temporary.path().join("config.toml");
        std::fs::write(&path, "version = 1\n").unwrap();
        let store = ConfigStore::open(path).unwrap();

        store.save_setting("keys.submit", "[]").unwrap();
        assert_eq!(
            store.get_value("keys.submit").unwrap().as_deref(),
            Some("[]")
        );
        assert!(store.save_setting("keys.submit", "not an array").is_err());
        assert!(matches!(
            store.save_setting("keys.submit", r#"["enter"a]"#),
            Err(RuntimeError::InvalidOption(message)) if message == "invalid keybinding array"
        ));
        assert!(store.save_setting("keys.submit", "[1]").is_err());
        assert!(store.save_setting("keys.submit", "[\"ctrl+d\"]").is_err());

        store.reset_value("keys.submit").unwrap();
        let submit = store
            .settings()
            .unwrap()
            .into_iter()
            .find(|row| row.definition.key == "keys.submit")
            .unwrap();
        assert_eq!(submit.effective_value, "[\"enter\"]");
        assert!(submit.explicit_value.is_none());
    }

    #[test]
    fn attachment_limits_have_compact_display_values_without_changing_editor_values() {
        assert_eq!(format_byte_count(1_232), "1,232b");
        assert_eq!(format_byte_count(262_144), "262kb");
        assert_eq!(format_byte_count(1_300_000), "1.3mb");
        assert_eq!(format_byte_count(1_000_000), "1.0mb");

        let temporary = TempDir::new().unwrap();
        let path = temporary.path().join("config.toml");
        std::fs::write(&path, "version = 1\n").unwrap();
        let store = ConfigStore::open(path).unwrap();
        let row = store
            .settings()
            .unwrap()
            .into_iter()
            .find(|row| row.definition.key == "limits.attachment_bytes")
            .unwrap();
        assert_eq!(row.effective_value, "262144");
        assert_eq!(row.display_value, "262kb");
    }
}
