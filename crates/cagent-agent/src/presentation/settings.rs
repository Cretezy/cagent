//! Frontend-neutral setting descriptions used by interactive configuration UIs.

use crate::config::{SettingRow, SettingSection};

/// Projects settings for either the active section or a case-insensitive
/// search across every section.
#[must_use]
pub fn filter_settings_rows<'a>(
    rows: &'a [SettingRow],
    section: SettingSection,
    query: &str,
) -> Vec<&'a SettingRow> {
    let query = query.to_lowercase();
    rows.iter()
        .filter(|row| {
            if query.is_empty() {
                return row.definition.section == section;
            }
            row.definition.label.to_lowercase().contains(&query)
                || row.definition.key.to_lowercase().contains(&query)
                || row.definition.description.to_lowercase().contains(&query)
                || row
                    .definition
                    .section
                    .label()
                    .to_lowercase()
                    .contains(&query)
                || row.display_value.to_lowercase().contains(&query)
        })
        .collect()
}

/// Adds the input-shape and default-value details that are useful when editing
/// a setting.  Frontends can style and wrap the returned text themselves.
pub fn setting_input_description(setting: &crate::config::SettingDefinition) -> String {
    let description = match setting.key.as_str() {
        "ui.title.requested_input" => {
            "JSON-compatible array of strings to show before the title, e.g. [\"!\", \" \"]"
        }
        "ui.title.progress" => {
            "JSON-compatible array of strings to cycle in the title, e.g. [\".\", \"..\", \"...\"]"
        }
        "ui.editor" => {
            "TOML command array (foreground) or { command = [...], mode = \"foreground\" | \"background\" }"
        }
        _ => &setting.description,
    };
    let default = &setting.default_value;
    if matches!(
        &setting.kind,
        crate::config::SettingKind::PositiveInteger
            | crate::config::SettingKind::NonNegativeInteger
    ) {
        format!("{description} (int, default: {default})")
    } else {
        format!("{description} (default: {default})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{SettingDefinition, SettingKind};

    fn row(
        section: SettingSection,
        key: &str,
        label: &str,
        description: &str,
        value: &str,
    ) -> SettingRow {
        SettingRow {
            definition: SettingDefinition {
                key: key.into(),
                label: label.into(),
                description: description.into(),
                kind: SettingKind::String,
                section,
                default_value: String::new(),
            },
            effective_value: value.into(),
            display_value: value.into(),
            explicit_value: None,
        }
    }

    #[test]
    fn settings_filter_uses_section_until_searching_then_matches_all_metadata() {
        let rows = vec![
            row(
                SettingSection::General,
                "shell.mode",
                "Terminal",
                "shell behavior",
                "smart",
            ),
            row(
                SettingSection::Ui,
                "ui.theme",
                "Theme",
                "visual palette",
                "dark",
            ),
            row(
                SettingSection::Files,
                "files.hidden",
                "Hidden",
                "show dotfiles",
                "false",
            ),
        ];

        assert_eq!(
            filter_settings_rows(&rows, SettingSection::General, "").len(),
            1
        );
        for query in ["THEME", "ui.theme", "palette", "files", "FALSE"] {
            assert_eq!(
                filter_settings_rows(&rows, SettingSection::General, query).len(),
                1
            );
        }
    }
}
