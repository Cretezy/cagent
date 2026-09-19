use cagent_agent::presentation::{KeyBindingAction, KeyChord, LogicalKey};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::App;

impl App {
    pub(super) fn key_for_action(&self, action: KeyBindingAction) -> Option<KeyEvent> {
        let (control, alt, shift, key) = self.key_bindings.first_chord(action)?.parts();
        let mut modifiers = KeyModifiers::NONE;
        if control {
            modifiers |= KeyModifiers::CONTROL;
        }
        if alt {
            modifiers |= KeyModifiers::ALT;
        }
        if shift {
            modifiers |= KeyModifiers::SHIFT;
        }
        let code = match key {
            LogicalKey::Enter => KeyCode::Enter,
            LogicalKey::Escape => KeyCode::Esc,
            LogicalKey::Tab => KeyCode::Tab,
            LogicalKey::Up => KeyCode::Up,
            LogicalKey::Down => KeyCode::Down,
            LogicalKey::Left => KeyCode::Left,
            LogicalKey::Right => KeyCode::Right,
            LogicalKey::Character(character) => KeyCode::Char(character),
        };
        Some(KeyEvent::new(code, modifiers))
    }
    pub(super) fn matches_action(&self, action: KeyBindingAction, event: KeyEvent) -> bool {
        event_chord(event).is_some_and(|chord| self.key_bindings.matches(action, chord))
    }

    pub(super) fn menu_navigation_code(&self, event: KeyEvent) -> KeyCode {
        if event.modifiers == KeyModifiers::CONTROL {
            match event.code {
                KeyCode::Char('k' | 'K') => return KeyCode::Up,
                KeyCode::Char('j' | 'J') => return KeyCode::Down,
                _ => {}
            }
        }
        let Some(chord) = event_chord(event) else {
            return event.code;
        };
        match self.key_bindings.action_for(chord) {
            Some(KeyBindingAction::NavigateUp) => KeyCode::Up,
            Some(KeyBindingAction::NavigateDown) => KeyCode::Down,
            Some(KeyBindingAction::NavigateLeft) => KeyCode::Left,
            Some(KeyBindingAction::NavigateRight) => KeyCode::Right,
            Some(KeyBindingAction::Submit) => KeyCode::Enter,
            Some(KeyBindingAction::Complete) => KeyCode::Tab,
            Some(KeyBindingAction::CloseSurface) => KeyCode::Esc,
            None if event.modifiers.is_empty()
                && matches!(
                    event.code,
                    KeyCode::Enter
                        | KeyCode::Esc
                        | KeyCode::Tab
                        | KeyCode::Up
                        | KeyCode::Down
                        | KeyCode::Left
                        | KeyCode::Right
                ) =>
            {
                KeyCode::Null
            }
            Some(_) | None => event.code,
        }
    }

    pub(crate) fn contextual_key_hints(&self, hints: &str) -> String {
        use KeyBindingAction as Action;
        let replacements = [
            ("↑/↓", vec![Action::NavigateUp, Action::NavigateDown]),
            ("←/→", vec![Action::NavigateLeft, Action::NavigateRight]),
            (
                "Shift+Left/Right/Tab",
                vec![Action::PreviousMode, Action::NextMode],
            ),
            ("Enter/Esc", vec![Action::Submit, Action::CloseSurface]),
            ("Enter", vec![Action::Submit]),
            ("Shift+Enter", vec![Action::InsertNewline]),
            ("Tab", vec![Action::Complete]),
            ("ScrollToMessage", vec![Action::ScrollToMessage]),
            ("Esc", vec![Action::CloseSurface]),
            ("Ctrl+D", vec![Action::Exit]),
        ];
        hints
            .split(" · ")
            .filter_map(|part| {
                // This permission-surface shortcut is literal and unrelated
                // to the configurable multiline-composer binding.
                if part.starts_with("Shift+Enter apply globally")
                    || part.starts_with("Shift+Enter apply for conversation")
                {
                    return Some(part.to_owned());
                }
                for (prefix, actions) in &replacements {
                    if let Some(rest) = part.strip_prefix(prefix) {
                        let labels = actions
                            .iter()
                            .filter(|action| self.key_bindings.is_bound(**action))
                            .map(|action| self.key_bindings.label(*action))
                            .collect::<Vec<_>>();
                        if labels.is_empty() {
                            return None;
                        }
                        return Some(format!("{}{rest}", labels.join("/")));
                    }
                }
                Some(part.to_owned())
            })
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

fn event_chord(event: KeyEvent) -> Option<KeyChord> {
    let modifiers = event.modifiers
        & (KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT)
        | if event.code == KeyCode::BackTab {
            KeyModifiers::SHIFT
        } else {
            KeyModifiers::NONE
        };
    let key = match event.code {
        KeyCode::Enter => LogicalKey::Enter,
        KeyCode::Esc | KeyCode::Char('\x1b') => LogicalKey::Escape,
        KeyCode::Tab | KeyCode::BackTab => LogicalKey::Tab,
        KeyCode::Up => LogicalKey::Up,
        KeyCode::Down => LogicalKey::Down,
        KeyCode::Left => LogicalKey::Left,
        KeyCode::Right => LogicalKey::Right,
        KeyCode::Char(character) => LogicalKey::Character(character.to_ascii_lowercase()),
        _ => return None,
    };
    Some(KeyChord::new(
        modifiers.contains(KeyModifiers::CONTROL),
        modifiers.contains(KeyModifiers::ALT),
        modifiers.contains(KeyModifiers::SHIFT),
        key,
    ))
}
