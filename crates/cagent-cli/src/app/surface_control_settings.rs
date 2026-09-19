use super::*;

pub(super) fn adjacent_setting_section(
    section: cagent_agent::config::SettingSection,
    forward: bool,
) -> cagent_agent::config::SettingSection {
    let sections = cagent_agent::config::SettingSection::ALL;
    let index = sections
        .iter()
        .position(|candidate| *candidate == section)
        .unwrap_or(0);
    sections[if forward {
        (index + 1) % sections.len()
    } else {
        index.checked_sub(1).unwrap_or(sections.len() - 1)
    }]
}

pub(super) fn custom_setting_value(explicit: Option<String>) -> String {
    match explicit {
        Some(value)
            if value.trim_start().starts_with('[') || value.trim_start().starts_with('{') =>
        {
            value
        }
        _ => "[]".into(),
    }
}

pub(super) fn refresh_settings_surface(
    surfaces: &mut [Surface],
    session: &SessionHandle,
) -> Result<(), cagent_agent::protocol::RuntimeError> {
    if let Some(Surface::Settings {
        rows,
        list,
        section,
        query,
        ..
    }) = surfaces.last_mut()
    {
        *rows = session.settings()?;
        let count = filter_settings_rows(rows, *section, query).len();
        list.reconcile(ListMode::Selectable, count, settings_item_capacity(query));
    }
    Ok(())
}

pub(crate) fn concise_setting_error(error: &cagent_agent::protocol::RuntimeError) -> String {
    let message = match error {
        cagent_agent::protocol::RuntimeError::InvalidOption(message)
        | cagent_agent::protocol::RuntimeError::Config { message, .. } => message,
        _ => return error.to_string(),
    };
    message.lines().next().unwrap_or(message).trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::surfaces::setting_input_description;

    #[test]
    fn diff_mode_choice_editor_saves_and_resets() {
        std::thread::Builder::new()
            .name("diff-mode-settings-test".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(diff_mode_choice_editor_saves_and_resets_inner());
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[allow(clippy::too_many_lines)]
    async fn diff_mode_choice_editor_saves_and_resets_inner() {
        use cagent_agent::config::{ConfigSnapshot, ConfigStore, UiDiffMode};

        let temporary = tempfile::tempdir().unwrap();
        let config_path = temporary.path().join("config.toml");
        std::fs::write(
            &config_path,
            "version = 1\ndefault_model = { model = 'mock/echo' }\n[providers.mock]\ntype = 'mock'\nenabled = true\n",
        )
        .unwrap();
        let config = ConfigStore::open(&config_path).unwrap();
        let runtime = AgentRuntime::open(
            cagent_agent::runtime::RuntimeOptions::new(temporary.path().join("cagent.db"))
                .with_config(config.clone()),
        )
        .await
        .unwrap();
        let session = runtime
            .create_session(cagent_agent::runtime::NewSession {
                workspace: temporary.path().to_path_buf(),
            })
            .await
            .unwrap();
        let mut app = App::new(
            temporary.path(),
            ("mock".into(), "echo".into(), None),
            ["mock".into()].into_iter().collect(),
            "ask",
            None,
        );
        app.replace_draft("/settings");
        app.submit_draft(&session, QueueTarget::NextBoundary)
            .await
            .unwrap();
        for character in "diff_mode".chars() {
            app.handle_key(
                &session,
                KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            )
            .await
            .unwrap();
        }
        app.handle_key(&session, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();
        let Some(Surface::SettingChoices {
            setting,
            choices,
            list,
            custom_value,
        }) = app.surfaces.last()
        else {
            panic!("diff mode should open a choice editor");
        };
        assert_eq!(setting.key, "ui.diff_mode");
        assert_eq!(
            choices
                .iter()
                .map(|(value, _)| value.as_str())
                .collect::<Vec<_>>(),
            ["conversation", "git"]
        );
        assert_eq!(list.selected, Some(0));
        assert!(custom_value.is_none());

        for key in [KeyCode::Down, KeyCode::Enter] {
            app.handle_key(&session, KeyEvent::new(key, KeyModifiers::NONE))
                .await
                .unwrap();
        }
        assert_eq!(config.snapshot().diff_mode(), UiDiffMode::Git);
        assert_eq!(
            ConfigSnapshot::load(&config_path).unwrap().diff_mode(),
            UiDiffMode::Git
        );
        assert!(
            matches!(app.surfaces.last(), Some(Surface::Settings { rows, .. })
            if rows.iter().any(|row| row.definition.key == "ui.diff_mode" && row.effective_value == "git"))
        );

        app.handle_key(&session, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();
        assert!(
            matches!(app.surfaces.last(), Some(Surface::SettingChoices { list, .. }) if list.selected == Some(1))
        );
        app.handle_key(&session, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .await
            .unwrap();
        app.handle_key(
            &session,
            KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
        )
        .await
        .unwrap();
        assert_eq!(config.snapshot().diff_mode(), UiDiffMode::Conversation);
        assert_eq!(config.get_value("ui.diff_mode").unwrap(), None);
        assert!(
            matches!(app.surfaces.last(), Some(Surface::Settings { rows, .. })
            if rows.iter().any(|row| row.definition.key == "ui.diff_mode" && row.effective_value == "conversation" && row.explicit_value.is_none()))
        );
    }

    #[test]
    fn custom_editors_start_empty_for_presets_and_restore_commands() {
        assert_eq!(custom_setting_value(None), "[]");
        assert_eq!(custom_setting_value(Some("triangles".into())), "[]");
        assert_eq!(custom_setting_value(Some("[\"!\"]".into())), "[\"!\"]");
    }

    #[test]
    fn setting_input_descriptions_explain_type_and_default() {
        let setting = cagent_agent::config::SettingDefinition {
            key: "subagents.max_concurrent".into(),
            label: String::new(),
            description: "maximum active sub-agents".into(),
            kind: cagent_agent::config::SettingKind::NonNegativeInteger,
            section: cagent_agent::config::SettingSection::Ui,
            default_value: "10".into(),
        };
        assert_eq!(
            setting_input_description(&setting),
            "maximum active sub-agents (int, default: 10)"
        );
    }
}
