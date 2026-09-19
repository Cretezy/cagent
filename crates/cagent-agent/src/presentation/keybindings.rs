//! Frontend-neutral keybinding definitions and configuration resolution.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::config::ConfigSnapshot;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum KeyBindingAction {
    Submit,
    InsertNewline,
    ModelPicker,
    ToggleFast,
    Tree,
    Fork,
    Rename,
    RetryTurn,
    ExternalEditor,
    LineStart,
    LineEnd,
    DeleteWord,
    Undo,
    Cancel,
    Exit,
    CloseSurface,
    PreviousMode,
    NextMode,
    NavigateLeft,
    NavigateRight,
    NavigateUp,
    NavigateDown,
    Background,
    Complete,
    ToggleChip,
    DeleteQueued,
    PromoteQueued,
    ScrollToMessage,
}

impl KeyBindingAction {
    pub const ALL: [Self; 28] = [
        Self::Submit,
        Self::InsertNewline,
        Self::ModelPicker,
        Self::ToggleFast,
        Self::Tree,
        Self::Fork,
        Self::Rename,
        Self::RetryTurn,
        Self::ExternalEditor,
        Self::LineStart,
        Self::LineEnd,
        Self::DeleteWord,
        Self::Undo,
        Self::Cancel,
        Self::Exit,
        Self::CloseSurface,
        Self::PreviousMode,
        Self::NextMode,
        Self::NavigateLeft,
        Self::NavigateRight,
        Self::NavigateUp,
        Self::NavigateDown,
        Self::Background,
        Self::Complete,
        Self::ToggleChip,
        Self::DeleteQueued,
        Self::PromoteQueued,
        Self::ScrollToMessage,
    ];

    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Submit => "submit",
            Self::InsertNewline => "insert_newline",
            Self::ModelPicker => "model_picker",
            Self::ToggleFast => "toggle_fast",
            Self::Tree => "tree",
            Self::Fork => "fork",
            Self::Rename => "rename",
            Self::RetryTurn => "retry_turn",
            Self::ExternalEditor => "external_editor",
            Self::LineStart => "line_start",
            Self::LineEnd => "line_end",
            Self::DeleteWord => "delete_word",
            Self::Undo => "undo",
            Self::Cancel => "cancel",
            Self::Exit => "exit",
            Self::CloseSurface => "close_surface",
            Self::PreviousMode => "previous_mode",
            Self::NextMode => "next_mode",
            Self::NavigateLeft => "navigate_left",
            Self::NavigateRight => "navigate_right",
            Self::NavigateUp => "navigate_up",
            Self::NavigateDown => "navigate_down",
            Self::Background => "background",
            Self::Complete => "complete",
            Self::ToggleChip => "toggle_chip",
            Self::DeleteQueued => "delete_queued",
            Self::PromoteQueued => "promote_queued",
            Self::ScrollToMessage => "scroll_to_message",
        }
    }

    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Submit => "submit or select",
            Self::InsertNewline => "insert a newline",
            Self::ModelPicker => "open model picker",
            Self::ToggleFast => "toggle Fast mode",
            Self::Tree => "open conversation tree",
            Self::Fork => "open fork picker",
            Self::Rename => "open conversation rename editor",
            Self::RetryTurn => "wake the model from safe history",
            Self::ExternalEditor => "edit draft in $EDITOR",
            Self::LineStart => "move to the start of the line",
            Self::LineEnd => "move to the end of the line",
            Self::DeleteWord => "delete the previous word",
            Self::Undo => "undo the last composer edit",
            Self::Cancel => "clear the draft or arm exit",
            Self::Exit => "exit when the composer is empty",
            Self::CloseSurface => "close the current context or interrupt active work",
            Self::PreviousMode => "switch to the previous mode",
            Self::NextMode => "switch to the next mode",
            Self::NavigateLeft => "switch to the previous tab",
            Self::NavigateRight => "switch to the next tab",
            Self::NavigateUp => "navigate up or history",
            Self::NavigateDown => "navigate down or history",
            Self::Background => "view background work",
            Self::Complete => "complete or queue at end of turn",
            Self::ToggleChip => "expand or collapse the selected chip",
            Self::DeleteQueued => "delete selected queued message",
            Self::PromoteQueued => "send selected queued message next",
            Self::ScrollToMessage => "scroll to the selected history message",
        }
    }

    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|action| action.id() == id)
    }

    fn default_chords(self) -> Vec<KeyChord> {
        let values: &[&str] = match self {
            Self::Submit => &["enter"],
            Self::InsertNewline => &["shift+enter", "alt+enter"],
            Self::ModelPicker => &["alt+p"],
            Self::ToggleFast => &["alt+s"],
            Self::Tree => &["alt+t"],
            Self::Fork => &["alt+f"],
            Self::Rename => &["alt+n"],
            Self::RetryTurn => &["alt+r"],
            Self::ExternalEditor => &["ctrl+g"],
            Self::LineStart => &["ctrl+a"],
            Self::LineEnd => &["ctrl+e"],
            Self::DeleteWord => &["ctrl+w"],
            Self::Undo => &["ctrl+u"],
            Self::Cancel => &["ctrl+c"],
            Self::Exit => &["ctrl+d"],
            Self::CloseSurface => &["esc"],
            Self::PreviousMode => &["shift+left"],
            Self::NextMode => &["shift+right", "shift+tab"],
            Self::NavigateLeft => &["left"],
            Self::NavigateRight => &["right"],
            Self::NavigateUp => &["up"],
            Self::NavigateDown => &["down"],
            Self::Background => &["alt+down"],
            Self::Complete => &["tab"],
            Self::ToggleChip => &["alt+e"],
            Self::DeleteQueued => &["d"],
            Self::PromoteQueued => &["s"],
            Self::ScrollToMessage => &["s"],
        };
        values
            .iter()
            .map(|value| KeyChord::parse(value).expect("valid built-in chord"))
            .collect()
    }

    /// TOML/JSON-compatible source for this action's built-in chords.
    #[must_use]
    pub fn default_source(self) -> String {
        let values = self
            .default_chords()
            .into_iter()
            .map(|chord| toml::Value::String(chord.source()))
            .collect();
        toml::Value::Array(values).to_string()
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LogicalKey {
    Enter,
    Escape,
    Tab,
    Up,
    Down,
    Left,
    Right,
    Character(char),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct KeyChord {
    control: bool,
    alt: bool,
    shift: bool,
    key: LogicalKey,
}

impl KeyChord {
    fn source(self) -> String {
        let mut parts = Vec::new();
        if self.control {
            parts.push("ctrl".to_owned());
        }
        if self.alt {
            parts.push("alt".to_owned());
        }
        if self.shift {
            parts.push("shift".to_owned());
        }
        parts.push(match self.key {
            LogicalKey::Enter => "enter".into(),
            LogicalKey::Escape => "esc".into(),
            LogicalKey::Tab => "tab".into(),
            LogicalKey::Up => "up".into(),
            LogicalKey::Down => "down".into(),
            LogicalKey::Left => "left".into(),
            LogicalKey::Right => "right".into(),
            LogicalKey::Character(character) => character.to_string(),
        });
        parts.join("+")
    }
    /// Returns the crossterm-independent parts of this chord for frontends
    /// that need to invoke the configured action programmatically.
    #[must_use]
    pub const fn parts(self) -> (bool, bool, bool, LogicalKey) {
        (self.control, self.alt, self.shift, self.key)
    }
    #[must_use]
    pub const fn new(control: bool, alt: bool, shift: bool, key: LogicalKey) -> Self {
        Self {
            control,
            alt,
            shift,
            key,
        }
    }

    /// Parses a case-insensitive logical chord such as `ctrl+enter`.
    ///
    /// # Errors
    /// Returns an error for empty chords, unknown keys or modifiers, and duplicate modifiers.
    pub fn parse(value: &str) -> Result<Self, String> {
        let parts = value
            .split('+')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        let Some((key, modifiers)) = parts.split_last() else {
            return Err("binding is empty".into());
        };
        let key = match key.to_ascii_lowercase().as_str() {
            "enter" => LogicalKey::Enter,
            "esc" | "escape" => LogicalKey::Escape,
            "tab" => LogicalKey::Tab,
            "up" => LogicalKey::Up,
            "down" => LogicalKey::Down,
            "left" => LogicalKey::Left,
            "right" => LogicalKey::Right,
            value if value.chars().count() == 1 => LogicalKey::Character(
                value
                    .chars()
                    .next()
                    .unwrap_or_default()
                    .to_ascii_lowercase(),
            ),
            _ => return Err(format!("unknown key: {key}")),
        };
        let mut chord = Self::new(false, false, false, key);
        for modifier in modifiers {
            let field = match modifier.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => &mut chord.control,
                "alt" => &mut chord.alt,
                "shift" => &mut chord.shift,
                _ => return Err(format!("unknown modifier: {modifier}")),
            };
            if *field {
                return Err(format!("duplicate modifier: {modifier}"));
            }
            *field = true;
        }
        Ok(chord)
    }

    #[must_use]
    pub fn label(self) -> String {
        let mut parts = Vec::new();
        if self.control {
            parts.push("Ctrl".to_owned());
        }
        if self.alt {
            parts.push("Alt".to_owned());
        }
        if self.shift {
            parts.push("Shift".to_owned());
        }
        parts.push(match self.key {
            LogicalKey::Enter => "Enter".into(),
            LogicalKey::Escape => "Esc".into(),
            LogicalKey::Tab => "Tab".into(),
            LogicalKey::Up => "↑".into(),
            LogicalKey::Down => "↓".into(),
            LogicalKey::Left => "←".into(),
            LogicalKey::Right => "→".into(),
            LogicalKey::Character(character) => character.to_ascii_uppercase().to_string(),
        });
        parts.join("+")
    }
}

#[derive(Clone, Debug)]
pub struct ResolvedKeyBindings {
    bindings: BTreeMap<KeyBindingAction, Vec<KeyChord>>,
}

impl Default for ResolvedKeyBindings {
    fn default() -> Self {
        Self {
            bindings: KeyBindingAction::ALL
                .into_iter()
                .map(|action| (action, action.default_chords()))
                .collect(),
        }
    }
}

impl ResolvedKeyBindings {
    #[must_use]
    pub fn first_chord(&self, action: KeyBindingAction) -> Option<KeyChord> {
        self.bindings[&action].first().copied()
    }
    #[must_use]
    pub fn from_config(config: &ConfigSnapshot) -> (Self, Vec<String>) {
        let defaults = Self::default();
        let mut resolved = defaults.clone();
        let mut warnings = Vec::new();
        for (id, configured) in config.key_bindings() {
            let Some(action) = KeyBindingAction::from_id(&id) else {
                warnings.push(format!("unknown key action: {id}"));
                continue;
            };
            let Some(values) = configured else {
                warnings.push(format!(
                    "invalid binding for {id}: expected an array of strings"
                ));
                continue;
            };
            let mut chords = Vec::new();
            for (index, value) in values.into_iter().enumerate() {
                let Some(value) = value else {
                    warnings.push(format!(
                        "invalid binding for {id}[{index}]: expected a string"
                    ));
                    continue;
                };
                match KeyChord::parse(&value) {
                    Ok(chord) if !chords.contains(&chord) => chords.push(chord),
                    Ok(_) => {}
                    Err(error) => {
                        warnings.push(format!("invalid binding for {id}[{index}]: {error}"));
                    }
                }
            }
            resolved.bindings.insert(action, chords);
        }
        loop {
            let mut owners: HashMap<KeyChord, Vec<KeyBindingAction>> = HashMap::new();
            for (action, chords) in &resolved.bindings {
                for chord in chords {
                    owners.entry(*chord).or_default().push(*action);
                }
            }
            let conflicts = owners
                .into_iter()
                .filter(|(_, actions)| {
                    actions.len() > 1
                        && !actions.iter().enumerate().all(|(index, action)| {
                            actions[index + 1..]
                                .iter()
                                .all(|other| actions_may_share(*action, *other))
                        })
                })
                .collect::<Vec<_>>();
            if conflicts.is_empty() {
                break;
            }
            let mut reset = HashSet::new();
            for (chord, actions) in conflicts {
                warnings.push(format!(
                    "key conflict for {}: {}; keeping defaults",
                    chord.label(),
                    actions
                        .iter()
                        .map(|action| action.id())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
                reset.extend(actions);
            }
            let unchanged = reset
                .iter()
                .all(|action| resolved.bindings.get(action) == defaults.bindings.get(action));
            for action in reset {
                resolved
                    .bindings
                    .insert(action, defaults.bindings[&action].clone());
            }
            if unchanged {
                break;
            }
        }
        warnings.sort();
        warnings.dedup();
        (resolved, warnings)
    }

    /// Validates one prospective override, including conflicts with every other effective action.
    ///
    /// # Errors
    /// Returns an error for an invalid chord or a chord owned by another action.
    pub fn validate_override(
        config: &ConfigSnapshot,
        action: KeyBindingAction,
        values: &[String],
    ) -> Result<(), String> {
        let (resolved, _) = Self::from_config(config);
        let mut chords = Vec::new();
        for value in values {
            let chord = KeyChord::parse(value)?;
            if !chords.contains(&chord) {
                chords.push(chord);
            }
        }
        for chord in chords {
            if let Some(owner) = KeyBindingAction::ALL.into_iter().find(|candidate| {
                *candidate != action
                    && !actions_may_share(action, *candidate)
                    && resolved.matches(*candidate, chord)
            }) {
                return Err(format!(
                    "key conflict for {}: {} is already bound to {}",
                    chord.label(),
                    values.join(", "),
                    owner.id()
                ));
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn matches(&self, action: KeyBindingAction, chord: KeyChord) -> bool {
        self.bindings[&action].contains(&chord)
    }
    #[must_use]
    pub fn action_for(&self, chord: KeyChord) -> Option<KeyBindingAction> {
        KeyBindingAction::ALL
            .into_iter()
            .find(|action| self.matches(*action, chord))
    }
    #[must_use]
    pub fn is_bound(&self, action: KeyBindingAction) -> bool {
        !self.bindings[&action].is_empty()
    }
    #[must_use]
    pub fn label(&self, action: KeyBindingAction) -> String {
        let labels = self.bindings[&action]
            .iter()
            .map(|chord| chord.label())
            .collect::<Vec<_>>();
        if labels.is_empty() {
            "Unbound".into()
        } else {
            labels.join(" / ")
        }
    }
    #[must_use]
    pub fn help_rows(&self) -> Vec<(String, String)> {
        KeyBindingAction::ALL
            .into_iter()
            .map(|action| (self.label(action), action.description().into()))
            .collect()
    }
}

fn actions_may_share(left: KeyBindingAction, right: KeyBindingAction) -> bool {
    matches!(
        (left, right),
        (
            KeyBindingAction::PromoteQueued,
            KeyBindingAction::ScrollToMessage
        ) | (
            KeyBindingAction::ScrollToMessage,
            KeyBindingAction::PromoteQueued
        )
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn resolve(keys: &str) -> (ResolvedKeyBindings, Vec<String>) {
        let config = ConfigSnapshot::parse(
            Path::new("config.toml"),
            &format!("version = 1\n[keys]\n{keys}"),
        )
        .unwrap();
        ResolvedKeyBindings::from_config(&config)
    }

    #[test]
    fn arrays_preserve_order_normalize_and_deduplicate() {
        let (keys, warnings) = resolve("exit = [\"CONTROL+x\", \"ctrl+y\", \"ctrl+x\"]\n");
        assert_eq!(keys.label(KeyBindingAction::Exit), "Ctrl+X / Ctrl+Y");
        assert!(warnings.is_empty());
    }

    #[test]
    fn empty_and_all_invalid_arrays_unbind_actions() {
        let (empty, _) = resolve("exit = []\n");
        assert_eq!(empty.label(KeyBindingAction::Exit), "Unbound");
        let (invalid, warnings) = resolve("exit = [3, \"ctrl+nope\"]\n");
        assert_eq!(invalid.label(KeyBindingAction::Exit), "Unbound");
        assert_eq!(warnings.len(), 2);
    }

    #[test]
    fn scalar_falls_back_unknown_warns_and_partial_arrays_survive() {
        let (keys, warnings) =
            resolve("exit = \"ctrl+x\"\nunknown = [\"x\"]\nsubmit = [false, \"ctrl+s\"]\n");
        assert_eq!(keys.label(KeyBindingAction::Exit), "Ctrl+D");
        assert_eq!(keys.label(KeyBindingAction::Submit), "Ctrl+S");
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("expected an array"))
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("unknown key action"))
        );
    }

    #[test]
    fn conflicts_restore_every_involved_action_to_complete_defaults() {
        let (keys, warnings) = resolve("submit = [\"ctrl+x\"]\ninsert_newline = [\"ctrl+x\"]\n");
        assert_eq!(keys.label(KeyBindingAction::Submit), "Enter");
        assert_eq!(
            keys.label(KeyBindingAction::InsertNewline),
            "Shift+Enter / Alt+Enter"
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("key conflict"))
        );
    }

    #[test]
    fn help_describes_background_work_with_its_own_shortcut() {
        let rows = ResolvedKeyBindings::default().help_rows();
        assert!(
            rows.iter()
                .any(|(_, description)| { description == "navigate down or history" })
        );
        assert!(rows.iter().any(|(shortcut, description)| {
            shortcut == "Alt+↓" && description == "view background work"
        }));
    }

    #[test]
    fn scroll_to_message_uses_s_alongside_contextual_queue_promotion() {
        let bindings = ResolvedKeyBindings::default();
        let chord = KeyChord::parse("s").unwrap();
        assert!(bindings.matches(KeyBindingAction::ScrollToMessage, chord));
        assert!(bindings.matches(KeyBindingAction::PromoteQueued, chord));
        assert_eq!(bindings.label(KeyBindingAction::ScrollToMessage), "S");
    }

    #[test]
    fn rename_uses_alt_n_and_retry_uses_alt_r_by_default() {
        let keys = ResolvedKeyBindings::default();
        assert_eq!(keys.label(KeyBindingAction::Rename), "Alt+N");
        assert_eq!(keys.label(KeyBindingAction::RetryTurn), "Alt+R");
    }

    #[test]
    fn mode_cycling_uses_shifted_arrows_and_shift_tab() {
        let keys = ResolvedKeyBindings::default();
        assert_eq!(keys.label(KeyBindingAction::PreviousMode), "Shift+←");
        assert_eq!(
            keys.label(KeyBindingAction::NextMode),
            "Shift+→ / Shift+Tab"
        );
    }
}
