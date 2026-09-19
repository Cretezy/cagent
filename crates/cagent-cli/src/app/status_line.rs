use super::list::ListState;
use cagent_agent::presentation::{StatusLineColor, StatusLineConfig, StatusLineModule};

pub(crate) enum StatusLineEditorMode {
    Modules,
    Colors {
        module: StatusLineModule,
        list: ListState,
    },
    Hex {
        module: StatusLineModule,
        input: String,
        cursor: usize,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StatusLineColorChoice {
    Default,
    Named(StatusLineColor),
    Custom,
}

pub(crate) fn status_line_color_choices() -> Vec<StatusLineColorChoice> {
    std::iter::once(StatusLineColorChoice::Default)
        .chain(
            StatusLineColor::NAMED
                .into_iter()
                .map(StatusLineColorChoice::Named),
        )
        .chain(std::iter::once(StatusLineColorChoice::Custom))
        .collect()
}

pub(crate) fn status_line_color_choice_index(
    module: StatusLineModule,
    color: StatusLineColor,
) -> usize {
    if color == module.default_color() {
        return 0;
    }
    StatusLineColor::NAMED
        .iter()
        .position(|candidate| *candidate == color)
        .map_or(StatusLineColor::NAMED.len() + 1, |index| index + 1)
}

pub(crate) fn status_line_rows(config: &StatusLineConfig) -> Vec<StatusLineModule> {
    let mut rows = config.modules.clone();
    rows.extend(
        StatusLineModule::ALL
            .into_iter()
            .filter(|module| !config.modules.contains(module)),
    );
    rows
}

#[must_use]
pub(crate) const fn status_line_module_color_editable(module: StatusLineModule) -> bool {
    !matches!(module, StatusLineModule::Mode)
}

pub(crate) fn set_status_line_module_enabled(
    config: &mut StatusLineConfig,
    rows: &mut Vec<StatusLineModule>,
    module: StatusLineModule,
    enabled: bool,
) {
    if enabled {
        if !config.modules.contains(&module) {
            config.modules.push(module);
            if module != StatusLineModule::Hint && config.modules.contains(&StatusLineModule::Hint)
            {
                let module_row = rows.iter().position(|candidate| *candidate == module);
                let hint_row = rows
                    .iter()
                    .position(|candidate| *candidate == StatusLineModule::Hint);
                if let (Some(module_row), Some(hint_row)) = (module_row, hint_row)
                    && module_row > hint_row
                {
                    let hint = rows.remove(hint_row);
                    let module_row = rows
                        .iter()
                        .position(|candidate| *candidate == module)
                        .unwrap_or(rows.len());
                    rows.insert(module_row + 1, hint);
                }
            }
        }
    } else {
        config.modules.retain(|candidate| *candidate != module);
    }
    config.modules.sort_by_key(|module| {
        rows.iter()
            .position(|candidate| candidate == module)
            .unwrap_or(usize::MAX)
    });
}

pub(crate) fn move_status_line_module(
    config: &mut StatusLineConfig,
    rows: &mut [StatusLineModule],
    module: StatusLineModule,
    earlier: bool,
) {
    let Some(index) = config
        .modules
        .iter()
        .position(|candidate| *candidate == module)
    else {
        return;
    };
    let target = if earlier {
        let Some(target) = index.checked_sub(1) else {
            return;
        };
        target
    } else {
        let target = index + 1;
        if target >= config.modules.len() {
            return;
        }
        target
    };
    let other = config.modules[target];
    config.modules.swap(index, target);
    let Some(row) = rows.iter().position(|candidate| *candidate == module) else {
        return;
    };
    let Some(other_row) = rows.iter().position(|candidate| *candidate == other) else {
        return;
    };
    rows.swap(row, other_row);
}

pub(crate) fn reset_status_line_module(
    config: &mut StatusLineConfig,
    rows: &mut Vec<StatusLineModule>,
    module: StatusLineModule,
) {
    config.colors.remove(&module);
    rows.retain(|candidate| *candidate != module);
    let default_index = StatusLineModule::ALL
        .iter()
        .position(|candidate| *candidate == module)
        .unwrap_or(usize::MAX);
    let insert_at = rows
        .iter()
        .position(|candidate| {
            StatusLineModule::ALL
                .iter()
                .position(|default| default == candidate)
                .is_some_and(|index| index > default_index)
        })
        .unwrap_or(rows.len());
    rows.insert(insert_at, module);
    set_status_line_module_enabled(config, rows, module, true);
}
