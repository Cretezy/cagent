use std::io;
use std::path::{Path, PathBuf};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use crate::app::{ACCENT_STYLE, DIM_STYLE, SELECTED_STYLE};

const CHOICE_COUNT: usize = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WorkspaceTrustChoice {
    Trust,
    ContinueUntrusted,
    Exit,
}

#[derive(Debug, Default)]
struct WorkspaceTrustPrompt {
    selected: usize,
}

impl WorkspaceTrustPrompt {
    fn handle_key(&mut self, key: KeyEvent) -> Option<WorkspaceTrustChoice> {
        if key.kind == KeyEventKind::Release {
            return None;
        }
        match key.code {
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => self.selected = (self.selected + 1).min(CHOICE_COUNT - 1),
            KeyCode::Enter => return Some(choice_at(self.selected)),
            KeyCode::Char(character)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                return match character.to_ascii_lowercase() {
                    't' => Some(WorkspaceTrustChoice::Trust),
                    'c' => Some(WorkspaceTrustChoice::ContinueUntrusted),
                    'e' => Some(WorkspaceTrustChoice::Exit),
                    _ => None,
                };
            }
            KeyCode::Esc => return Some(WorkspaceTrustChoice::Exit),
            KeyCode::Char('c' | 'C') if key.modifiers == KeyModifiers::CONTROL => {
                return Some(WorkspaceTrustChoice::Exit);
            }
            _ => {}
        }
        None
    }
}

pub(crate) fn run_workspace_trust_prompt(
    terminal: &mut ratatui::DefaultTerminal,
    workspace: &Path,
    project_config_exists: bool,
) -> io::Result<WorkspaceTrustChoice> {
    let display_path = workspace_display_path(workspace, home_dir().as_deref());
    let mut prompt = WorkspaceTrustPrompt::default();

    loop {
        terminal.draw(|frame| {
            render_workspace_trust_prompt(
                frame,
                &display_path,
                project_config_exists,
                prompt.selected,
            );
        })?;
        if let Event::Key(key) = event::read()?
            && let Some(choice) = prompt.handle_key(key)
        {
            return Ok(choice);
        }
    }
}

fn render_workspace_trust_prompt(
    frame: &mut Frame<'_>,
    workspace: &str,
    project_config_exists: bool,
    selected: usize,
) {
    let area = inset(frame.area(), 2, 1);
    let mut lines = vec![
        Line::from(vec![
            Span::styled(">_ ", ACCENT_STYLE),
            Span::styled("Cagent", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(": You are in "),
            Span::styled(workspace.to_owned(), Style::default().fg(Color::Cyan)),
        ]),
        Line::default(),
        Line::from(
            "Do you trust the contents of this directory? Trusting the directory allows \
             project-local config and MCP servers to load.",
        ),
    ];
    if project_config_exists {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            "Project config found at .cagent/config.toml. It has not been loaded.",
            DIM_STYLE,
        )));
    }
    lines.push(Line::default());
    lines.extend(
        [
            "Yes, trust and continue (t)",
            "Continue without project config (c)",
            "No, exit (e)",
        ]
        .into_iter()
        .enumerate()
        .map(|(index, label)| menu_choice(label, index == selected)),
    );
    lines.push(Line::default());
    lines.push(Line::from(Span::styled("press enter to select", DIM_STYLE)));

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn menu_choice(label: &'static str, selected: bool) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            if selected { "› " } else { "  " },
            if selected {
                ACCENT_STYLE
            } else {
                Style::default()
            },
        ),
        Span::styled(
            label,
            if selected {
                SELECTED_STYLE
            } else {
                Style::default()
            },
        ),
    ])
}

fn choice_at(index: usize) -> WorkspaceTrustChoice {
    match index {
        0 => WorkspaceTrustChoice::Trust,
        1 => WorkspaceTrustChoice::ContinueUntrusted,
        _ => WorkspaceTrustChoice::Exit,
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn workspace_display_path(workspace: &Path, home: Option<&Path>) -> String {
    let Some(home) = home else {
        return workspace.display().to_string();
    };
    let Ok(relative) = workspace.strip_prefix(home) else {
        return workspace.display().to_string();
    };
    if relative.as_os_str().is_empty() {
        "~".to_owned()
    } else {
        format!("~/{}", relative.display())
    }
}

fn inset(area: Rect, horizontal: u16, vertical: u16) -> Rect {
    Rect::new(
        area.x.saturating_add(horizontal),
        area.y.saturating_add(vertical),
        area.width.saturating_sub(horizontal.saturating_mul(2)),
        area.height.saturating_sub(vertical.saturating_mul(2)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn trust_prompt_uses_menu_navigation_and_shortcuts() {
        let mut prompt = WorkspaceTrustPrompt::default();
        assert_eq!(
            prompt.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
            None
        );
        assert_eq!(prompt.selected, 1);
        assert_eq!(
            prompt.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Some(WorkspaceTrustChoice::ContinueUntrusted)
        );
        assert_eq!(
            prompt.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE)),
            Some(WorkspaceTrustChoice::Trust)
        );
        assert_eq!(
            prompt.handle_key(KeyEvent::new(KeyCode::Char('E'), KeyModifiers::SHIFT)),
            Some(WorkspaceTrustChoice::Exit)
        );
    }

    #[test]
    fn trust_prompt_renders_full_screen_content_and_menu_highlight() {
        let backend = TestBackend::new(80, 16);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_workspace_trust_prompt(frame, "~/code", true, 0))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let text = buffer
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();

        assert!(text.contains(">_ Cagent: You are in ~/code"));
        assert!(text.contains("Project config found at .cagent/config.toml"));
        assert!(text.contains("› Yes, trust and continue (t)"));

        let prompt = buffer.cell((2, 1)).unwrap();
        assert_eq!(prompt.fg, Color::Cyan);
        assert!(prompt.modifier.contains(Modifier::BOLD));
        let cagent = buffer.cell((5, 1)).unwrap();
        assert!(cagent.modifier.contains(Modifier::BOLD));
        let path = buffer.cell((24, 1)).unwrap();
        assert_eq!(path.fg, Color::Cyan);
        let selected = buffer.cell((4, 8)).unwrap();
        assert_eq!(selected.fg, Color::Cyan);
        assert!(selected.modifier.contains(Modifier::BOLD));
        assert!(text.contains("No, exit (e)"));
        assert!(text.contains("press enter to select"));
        let footer = buffer.cell((2, 12)).unwrap();
        assert!(footer.modifier.contains(Modifier::DIM));
    }

    #[test]
    fn workspace_path_is_abbreviated_against_home() {
        assert_eq!(
            workspace_display_path(Path::new("/home/alex/code"), Some(Path::new("/home/alex"))),
            "~/code"
        );
        assert_eq!(
            workspace_display_path(Path::new("/work/code"), Some(Path::new("/home/alex"))),
            "/work/code"
        );
    }
}
