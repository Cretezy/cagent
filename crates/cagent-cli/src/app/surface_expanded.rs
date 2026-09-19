use super::helpers::history_tree_row_line;
use super::list::{LinePolicy, ListHit, ListMode, ListState, ListWidget, truncate_line};
use super::scroll::{
    ScrollViewAction, ScrollViewHit, ScrollViewLayout, ScrollViewMetrics, ScrollViewState,
    ScrollViewWidget, scroll_window_metrics,
};
use super::{
    ACCENT_STYLE, AgentWizardStep, CHIP_STYLE, DIM_STYLE, ENABLED_STYLE, ERROR_STYLE, ExpandedView,
    HelpTab, MENU_DETAIL_STYLE, MENU_TITLE_STYLE, McpFormDraft, McpFormField, McpPackageField,
    MultilineInput, NOTICE_STYLE, PermissionMenuRow, SEARCH_CURSOR_STYLE, SELECTED_STYLE,
    SingleLineInput, StatusLineColor, StatusLineColorChoice, StatusLineEditorMode, Surface,
    UsageBreakdownKind, VISIBLE_HELP_ITEMS, VISIBLE_MENU_ITEMS, mcp_server_menu_item_count,
    settings_item_capacity, status_line_color_choices,
};
use crate::render::{
    DiffPreviewLayout, bash_command_spans, diff_preview_line_count, render_diff_preview_window,
    render_expanded_diff, status_line_color, status_line_content, terminal_safe,
    truncate_with_ellipsis, wrap_log_line, wrap_ranges,
};
use cagent_agent::WorkspaceEntryKind as ListEntryKind;
use cagent_agent::presentation::{
    conversation_picker_indexes, filter_model_picker_rows, filter_provider_picker_rows,
    filter_settings_rows, history_picker_indexes, model_picker_detail, permission_approval_choices,
    permission_tool_label,
};
use cagent_agent::protocol::InteractionRequestKind;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use std::cell::RefCell;
use std::ops::Range;
use std::path::Path;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

struct TabLayout<T> {
    line: Line<'static>,
    hits: Vec<(Range<usize>, T)>,
}

impl<T: Copy> TabLayout<T> {
    fn hit(&self, column: usize) -> Option<T> {
        self.hits
            .iter()
            .find_map(|(range, value)| range.contains(&column).then_some(*value))
    }
}

#[path = "surface_permissions.rs"]
mod surface_permissions;
#[path = "surface_settings.rs"]
pub(crate) mod surface_settings;
pub(crate) use old_surface_layout::{
    SurfaceHit, SurfaceLayout, SurfaceScrollRegion, SurfaceTab, change_help_tab,
    change_settings_section, surface_layout_with_viewport, surface_tab_at,
};
pub(crate) use surface_permissions::PermissionDiffRenderCache;
use surface_permissions::*;
use surface_settings::setting_input_description;

/// Shared indicator row for both selectable lists and content scrollers.
fn scroll_indicator(up: bool, visible: bool) -> Line<'static> {
    if visible {
        Line::from(if up { "  ↑" } else { "  ↓" }).style(DIM_STYLE)
    } else {
        Line::default()
    }
}

fn json_preview_lines(arguments: &serde_json::Value, width: u16) -> Vec<Line<'static>> {
    let source = serde_json::to_string_pretty(arguments).unwrap_or_else(|_| "null".into());
    let line_count = source.lines().count();
    let number_width = line_count.max(1).to_string().len();
    source
        .lines()
        .enumerate()
        .flat_map(|(index, line)| {
            let prefix = format!("  {:>number_width$} │ ", index + 1);
            let ranges = wrap_ranges(line, width.saturating_sub(prefix.width() as u16));
            let tokens = cagent_agent::presentation::highlight_code("json", line);
            ranges
                .into_iter()
                .enumerate()
                .map(move |(wrapped_index, (start, end))| {
                    let mut spans = vec![Span::styled(
                        if wrapped_index == 0 {
                            prefix.clone()
                        } else {
                            format!("  {:>number_width$} │ ", "")
                        },
                        DIM_STYLE,
                    )];
                    let mut token_start = 0;
                    for token in &tokens {
                        let token_end = token_start + token.text.len();
                        let slice_start = start.max(token_start);
                        let slice_end = end.min(token_end);
                        if slice_start < slice_end {
                            spans.push(Span::styled(
                                terminal_safe(
                                    &token.text[slice_start - token_start..slice_end - token_start],
                                ),
                                crate::markdown::code_style(token.kind),
                            ));
                        }
                        token_start = token_end;
                    }
                    Line::from(spans)
                })
        })
        .collect()
}

fn question_option_lines(
    selected: bool,
    label: &str,
    description: &str,
    label_width: usize,
    width: u16,
) -> Vec<Line<'static>> {
    let option_style = if selected {
        SELECTED_STYLE
    } else {
        Style::default()
    };
    let marker_style = if selected {
        ACCENT_STYLE
    } else {
        Style::default()
    };
    let description_style = if selected {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        DIM_STYLE
    };
    let label = truncate_with_ellipsis(&terminal_safe(label), label_width);
    let prefix_width = 2 + label_width + 2;
    let description_width = usize::from(width).saturating_sub(prefix_width).max(1);
    let description = terminal_safe(description);

    wrap_ranges(
        &description,
        u16::try_from(description_width).unwrap_or(u16::MAX),
    )
    .into_iter()
    .enumerate()
    .map(|(index, (start, end))| {
        let mut spans = if index == 0 {
            vec![
                Span::styled(if selected { "› " } else { "  " }, marker_style),
                Span::styled(label.clone(), option_style),
                Span::raw(" ".repeat(label_width.saturating_sub(label.width()) + 2)),
            ]
        } else {
            vec![Span::raw(" ".repeat(prefix_width))]
        };
        spans.push(Span::styled(
            description[start..end].to_owned(),
            description_style,
        ));
        Line::from(spans)
    })
    .collect()
}

fn indented_input(value: &str, cursor: usize, placeholder: &str, width: u16) -> Line<'static> {
    indented_syntax_input(value, cursor, placeholder, None, width)
}

fn indented_syntax_input(
    value: &str,
    cursor: usize,
    placeholder: &str,
    syntax_language: Option<&str>,
    width: u16,
) -> Line<'static> {
    let mut spans = vec![Span::raw("  ")];
    if value.is_empty() {
        let split = placeholder.chars().next().map_or(0, char::len_utf8);
        spans.push(Span::styled(
            placeholder[..split].to_owned(),
            SEARCH_CURSOR_STYLE,
        ));
        spans.push(Span::styled(placeholder[split..].to_owned(), DIM_STYLE));
        return truncate_line(Line::from(spans), usize::from(width));
    }
    let mut input = SingleLineInput::new(value.into(), cursor);
    if let Some(language) = syntax_language {
        input = input.with_syntax_language(language);
    }
    spans.extend(
        input
            .render_bounded(
                usize::from(width.saturating_sub(2)),
                Style::default(),
                SEARCH_CURSOR_STYLE,
            )
            .spans,
    );
    Line::from(spans)
}

fn setting_syntax_language(
    setting: &cagent_agent::config::SettingDefinition,
) -> Option<&'static str> {
    if setting.key == "ui.editor" {
        return Some("toml");
    }
    matches!(
        setting.kind,
        cagent_agent::config::SettingKind::KeyBindings
            | cagent_agent::config::SettingKind::TomlChoices(_)
    )
    .then_some("json")
}

const fn mcp_field_uses_json(field: McpFormField) -> bool {
    matches!(
        field,
        McpFormField::Agents
            | McpFormField::ReadOnlyTools
            | McpFormField::Arguments
            | McpFormField::Environment
            | McpFormField::RemovedEnvironment
            | McpFormField::Headers
    )
}

fn masked_input_parts(value: &str, cursor: usize) -> (String, usize, Vec<usize>) {
    let mut source_boundaries = value
        .grapheme_indices(true)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    source_boundaries.push(value.len());
    let grapheme_cursor = source_boundaries
        .iter()
        .rposition(|boundary| *boundary <= cursor.min(value.len()))
        .unwrap_or(0);
    (
        "•".repeat(source_boundaries.len().saturating_sub(1)),
        grapheme_cursor * '•'.len_utf8(),
        source_boundaries,
    )
}

fn masked_input_line(prefix: String, value: &str, cursor: usize, width: u16) -> Line<'static> {
    let available = usize::from(width).saturating_sub(prefix.width());
    let (display, display_cursor, _) = masked_input_parts(value, cursor);
    let mut spans = vec![Span::raw(prefix)];
    spans.extend(
        SingleLineInput::new(display, display_cursor)
            .render_bounded(available, ACCENT_STYLE, SEARCH_CURSOR_STYLE)
            .spans,
    );
    Line::from(spans)
}

pub(crate) fn external_url_in_text(text: &str) -> Option<(usize, &str)> {
    let start = [text.find("https://"), text.find("http://")]
        .into_iter()
        .flatten()
        .min()?;
    let tail = &text[start..];
    let untrimmed = tail.split_whitespace().next()?;
    let url = untrimmed.trim_end_matches(['.', ',', ';', ':', ')', ']', '}']);
    (!url.is_empty()).then_some((start, url))
}

fn package_description_line(description: Option<&str>) -> Line<'static> {
    let Some(description) = description else {
        return Line::default();
    };
    let safe = terminal_safe(description);
    let Some((start, url)) = external_url_in_text(&safe) else {
        return Line::from(format!("  {safe}")).style(DIM_STYLE);
    };
    let end = start + url.len();
    Line::from(vec![
        Span::styled(format!("  {}", &safe[..start]), DIM_STYLE),
        Span::styled(
            url.to_owned(),
            ACCENT_STYLE.add_modifier(Modifier::UNDERLINED),
        ),
        Span::styled(safe[end..].to_owned(), DIM_STYLE),
    ])
}

#[allow(clippy::too_many_lines)]
pub(crate) fn surface_lines(surface: &Surface, workspace: &Path, width: u16) -> Vec<Line<'static>> {
    surface_lines_with_viewport(surface, workspace, width, usize::MAX)
}

pub(crate) fn surface_lines_with_viewport(
    surface: &Surface,
    workspace: &Path,
    width: u16,
    viewport_rows: usize,
) -> Vec<Line<'static>> {
    surface_layout_with_viewport(surface, workspace, width, viewport_rows).lines
}

/// Number of message rows that fit in the full-screen history picker after
/// its title, spacer, scroll indicators, and trailing spacer.
pub(crate) fn history_picker_item_capacity(viewport_rows: usize) -> usize {
    viewport_rows.saturating_sub(5).max(1)
}

/// Number of conversation rows that fit in the full-screen resume picker.
/// Informational footer rows replace list slots so the surface remains exactly
/// full-height while filtering or showing archived conversations.
pub(crate) fn conversation_picker_item_capacity(
    viewport_rows: usize,
    include_archived: bool,
    no_matches: bool,
) -> usize {
    viewport_rows
        .saturating_sub(5 + usize::from(include_archived) + usize::from(no_matches))
        .max(1)
}

pub(crate) mod old_surface_layout {
    use super::*;
    use crate::app::{SkillWizardStep, TreePurpose};

    #[derive(Debug, Clone)]
    pub(crate) struct SurfaceLayout {
        pub(crate) lines: Vec<Line<'static>>,
        /// Compatibility map for list-backed callers. New input routing should use
        /// `surface_hit`, which retains the owning widget kind.
        pub(crate) hits: Vec<ListHit>,
        pub(crate) surface_hits: Vec<SurfaceHit>,
        input_hits: Vec<Option<SurfaceInputHit>>,
        pub(crate) scroll_region: Option<SurfaceScrollRegion>,
    }

    #[derive(Clone, Debug)]
    enum SurfaceInputHit {
        Single {
            input: SingleLineInput,
            content_column: usize,
            bounded_width: Option<usize>,
        },
        Masked {
            display: SingleLineInput,
            source_boundaries: Vec<usize>,
            content_column: usize,
            bounded_width: usize,
        },
        Multiline {
            input: MultilineInput,
            rendered_row: usize,
            content_column: usize,
            width: u16,
        },
    }

    impl SurfaceInputHit {
        fn cursor_at_column(&self, column: usize) -> usize {
            match self {
                Self::Single {
                    input,
                    content_column,
                    bounded_width,
                } => input.cursor_at_display_column(
                    column.saturating_sub(*content_column),
                    *bounded_width,
                ),
                Self::Multiline {
                    input,
                    rendered_row,
                    content_column,
                    width,
                } => input
                    .cursor_at_rendered_row(
                        *width,
                        *rendered_row,
                        column.saturating_sub(*content_column),
                    )
                    .unwrap_or_else(|| input.cursor()),
                Self::Masked {
                    display,
                    source_boundaries,
                    content_column,
                    bounded_width,
                } => {
                    let display_cursor = display.cursor_at_display_column(
                        column.saturating_sub(*content_column),
                        Some(*bounded_width),
                    );
                    let graphemes = display.text()[..display_cursor].graphemes(true).count();
                    source_boundaries
                        .get(graphemes)
                        .copied()
                        .unwrap_or_else(|| source_boundaries.last().copied().unwrap_or(0))
                }
            }
        }
    }

    /// The exact rows and geometry produced by a scroll widget in a composed
    /// surface. It is intentionally recorded with the visible layout rather than
    /// recomputed from the surface by input handling.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct SurfaceScrollRegion {
        pub(crate) rows: Range<usize>,
        pub(crate) metrics: ScrollViewMetrics,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum SurfaceHit {
        List(ListHit),
        Scroll(ScrollViewHit),
        DirectoryItem(usize),
        None,
    }

    impl SurfaceLayout {
        fn new(lines: Vec<Line<'static>>) -> Self {
            let line_count = lines.len();
            let hits = vec![ListHit::None; line_count];
            let surface_hits = vec![SurfaceHit::None; lines.len()];
            Self {
                lines,
                hits,
                surface_hits,
                input_hits: vec![None; line_count],
                scroll_region: None,
            }
        }

        fn overlay_list_hits(&mut self, prefix: usize, list_hits: Vec<ListHit>) {
            debug_assert!(prefix.saturating_add(list_hits.len()) <= self.lines.len());
            for (target, hit) in self.hits.iter_mut().skip(prefix).zip(list_hits) {
                *target = hit;
            }
            for (target, hit) in self
                .surface_hits
                .iter_mut()
                .skip(prefix)
                .zip(self.hits.iter().copied().skip(prefix))
            {
                if hit != ListHit::None {
                    *target = SurfaceHit::List(hit);
                }
            }
        }

        fn overlay_scroll_layout(&mut self, prefix: usize, scroll: ScrollViewLayout) {
            debug_assert!(prefix.saturating_add(scroll.hits.len()) <= self.lines.len());
            let end = prefix
                .saturating_add(scroll.hits.len())
                .min(self.lines.len());
            for (target, hit) in self.surface_hits.iter_mut().skip(prefix).zip(scroll.hits) {
                if hit != ScrollViewHit::None {
                    *target = SurfaceHit::Scroll(hit);
                }
            }
            self.scroll_region = Some(SurfaceScrollRegion {
                rows: prefix..end,
                metrics: scroll.metrics,
            });
        }

        pub(crate) fn surface_hit(&self, row: usize) -> SurfaceHit {
            self.surface_hits
                .get(row)
                .copied()
                .unwrap_or(SurfaceHit::None)
        }

        pub(crate) fn input_cursor_at(&self, row: usize, column: usize) -> Option<usize> {
            self.input_hits
                .get(row)
                .and_then(Option::as_ref)
                .map(|hit| hit.cursor_at_column(column))
        }

        fn overlay_input_hit(&mut self, row: usize, hit: SurfaceInputHit) {
            if let Some(target) = self.input_hits.get_mut(row) {
                *target = Some(hit);
            }
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum SurfaceTab {
        Help(HelpTab),
        Settings(cagent_agent::config::SettingSection),
    }

    pub(crate) fn change_help_tab(
        tab: &mut HelpTab,
        command_rows: &[(String, String)],
        key_rows: &[(String, String)],
        list: &mut ScrollViewState,
        next: HelpTab,
    ) -> bool {
        if *tab == next {
            return false;
        }
        *tab = next;
        let count = match next {
            HelpTab::Commands => command_rows.len(),
            HelpTab::Keybindings => key_rows.len(),
        };
        list.reset(count);
        true
    }

    pub(crate) fn change_settings_section(
        rows: &[cagent_agent::config::SettingRow],
        section: &mut cagent_agent::config::SettingSection,
        list: &mut ListState,
        next: cagent_agent::config::SettingSection,
    ) -> bool {
        if *section == next {
            return false;
        }
        *section = next;
        let count = rows
            .iter()
            .filter(|row| row.definition.section == next)
            .count();
        list.reset(ListMode::Selectable, count);
        true
    }

    struct TabLayout<T> {
        line: Line<'static>,
        hits: Vec<(Range<usize>, T)>,
    }

    impl<T: Copy> TabLayout<T> {
        fn hit(&self, column: usize) -> Option<T> {
            self.hits
                .iter()
                .find_map(|(range, value)| range.contains(&column).then_some(*value))
        }
    }

    pub(crate) fn surface_tab_at(
        surface: &Surface,
        row: usize,
        column: usize,
    ) -> Option<SurfaceTab> {
        if row != 2 {
            return None;
        }
        match surface {
            Surface::Help { tab, .. } => help_tabs(*tab).hit(column).map(SurfaceTab::Help),
            Surface::Settings { section, query, .. } if query.is_empty() => settings_tabs(*section)
                .hit(column)
                .map(SurfaceTab::Settings),
            _ => None,
        }
    }

    pub(crate) fn surface_layout_with_viewport(
        surface: &Surface,
        workspace: &Path,
        width: u16,
        viewport_rows: usize,
    ) -> SurfaceLayout {
        let mut layout = SurfaceLayout::new(render_surface_lines(
            surface,
            workspace,
            width,
            viewport_rows,
        ));
        if let Some((prefix, list_hits)) =
            surface_list_hits(surface, workspace, width, viewport_rows)
        {
            layout.overlay_list_hits(prefix, list_hits);
        }
        if let Some((prefix, scroll_layout)) =
            surface_scroll_hits(surface, workspace, width, viewport_rows)
        {
            layout.overlay_scroll_layout(prefix, scroll_layout);
        }
        if let Surface::Expanded {
            view: ExpandedView::Directory { browser },
            ..
        } = surface
        {
            let rows = browser.tree.rows();
            for hit in &mut layout.surface_hits {
                let SurfaceHit::Scroll(ScrollViewHit::Content(index)) = *hit else {
                    continue;
                };
                if rows.get(index).is_some_and(|row| row.is_selectable()) {
                    *hit = SurfaceHit::DirectoryItem(index);
                }
            }
        }
        overlay_surface_input_hits(&mut layout, surface, width);
        layout
    }

    fn overlay_surface_input_hits(layout: &mut SurfaceLayout, surface: &Surface, width: u16) {
        if let Some(note) = surface.interaction_note().filter(|note| note.active) {
            let input = MultilineInput::new(note.text.to_owned(), note.cursor);
            let rows = layout
                .surface_hits
                .iter()
                .enumerate()
                .filter_map(|(row, hit)| match hit {
                    SurfaceHit::Scroll(ScrollViewHit::Content(source_row)) => {
                        Some((row, *source_row))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            for (row, rendered_row) in rows {
                layout.overlay_input_hit(
                    row,
                    SurfaceInputHit::Multiline {
                        input: input.clone(),
                        rendered_row,
                        content_column: 2,
                        width: note_input_width(width),
                    },
                );
            }
            return;
        }
        let mut single = |row, value: String, cursor, content_column, bounded_width| {
            layout.overlay_input_hit(
                row,
                SurfaceInputHit::Single {
                    input: SingleLineInput::new(value, cursor),
                    content_column,
                    bounded_width,
                },
            );
        };
        match surface {
            Surface::PermissionRuleEdit {
                pattern, cursor, ..
            }
            | Surface::PersistentPermissionEdit {
                pattern, cursor, ..
            }
            | Surface::Rename {
                title: pattern,
                cursor,
            }
            | Surface::McpFieldEdit {
                value: pattern,
                cursor,
                ..
            } => {
                single(
                    2,
                    pattern.clone(),
                    *cursor,
                    2,
                    Some(usize::from(width.saturating_sub(2))),
                );
            }
            Surface::WebSearchPicker {
                query,
                query_cursor,
                ..
            } => {
                let content_column =
                    menu_title("Web search", None).width() + 2 + "search: ".width();
                single(
                    0,
                    query.clone(),
                    *query_cursor,
                    content_column,
                    Some(usize::from(width).saturating_sub(content_column)),
                );
            }
            Surface::WorktreeNew {
                name,
                base,
                cursor,
                editing_base,
            } => {
                let value = if *editing_base { base } else { name };
                let label = if *editing_base { "Base" } else { "Name" };
                let content_column = 2 + label.width() + 2;
                single(
                    2,
                    value.clone(),
                    *cursor,
                    content_column,
                    Some(usize::from(width).saturating_sub(content_column)),
                );
            }
            Surface::Providers {
                query,
                query_cursor,
                ..
            } => {
                let content_column = menu_title("Providers", None).width() + 2 + "search: ".width();
                single(
                    0,
                    query.clone(),
                    *query_cursor,
                    content_column,
                    Some(usize::from(width).saturating_sub(content_column)),
                );
            }
            Surface::Models {
                query,
                query_cursor,
                ..
            } => {
                let content_column = menu_title("Models", None).width() + 2 + "search: ".width();
                single(
                    0,
                    query.clone(),
                    *query_cursor,
                    content_column,
                    Some(usize::from(width).saturating_sub(content_column)),
                );
            }
            Surface::HistoryTree {
                purpose,
                query,
                query_cursor,
                ..
            } => {
                let title = if *purpose == TreePurpose::Fork {
                    "Fork"
                } else {
                    "Conversation tree"
                };
                let content_column = menu_title(title, None).width() + 2 + "search: ".width();
                single(
                    0,
                    query.clone(),
                    *query_cursor,
                    content_column,
                    Some(usize::from(width).saturating_sub(content_column)),
                );
            }
            Surface::Conversations {
                query,
                query_cursor,
                ..
            } => {
                let content_column = menu_title("Resume", None).width() + 2 + "search: ".width();
                single(
                    0,
                    query.clone(),
                    *query_cursor,
                    content_column,
                    Some(usize::from(width).saturating_sub(content_column)),
                );
            }
            Surface::WebSearchSetup {
                provider,
                value,
                cursor,
            } => {
                let label = if *provider == cagent_agent::web_search::WebSearchProvider::Exa {
                    "API key"
                } else {
                    "Base URL"
                };
                let content_column = 2 + label.width() + 2;
                if *provider == cagent_agent::web_search::WebSearchProvider::Exa {
                    let bounded_width = usize::from(width).saturating_sub(content_column);
                    let (display, display_cursor, source_boundaries) =
                        masked_input_parts(value, *cursor);
                    layout.overlay_input_hit(
                        2,
                        SurfaceInputHit::Masked {
                            display: SingleLineInput::new(display, display_cursor),
                            source_boundaries,
                            content_column,
                            bounded_width,
                        },
                    );
                } else {
                    single(
                        2,
                        value.clone(),
                        *cursor,
                        content_column,
                        Some(usize::from(width).saturating_sub(content_column)),
                    );
                }
            }
            Surface::ProviderSetup {
                api_key_auth: true,
                api_key,
                api_key_cursor,
                ..
            } => {
                let content_column = "  API key: ".width();
                let bounded_width = usize::from(width).saturating_sub(content_column);
                let (display, display_cursor, source_boundaries) =
                    masked_input_parts(api_key, *api_key_cursor);
                layout.overlay_input_hit(
                    2,
                    SurfaceInputHit::Masked {
                        display: SingleLineInput::new(display, display_cursor),
                        source_boundaries,
                        content_column,
                        bounded_width,
                    },
                );
            }
            Surface::SkillWizard {
                name,
                description,
                step,
                ..
            } => match step {
                SkillWizardStep::Name => single(
                    3,
                    name.text().to_owned(),
                    name.cursor(),
                    2,
                    Some(usize::from(width.saturating_sub(2))),
                ),
                SkillWizardStep::Description => single(
                    3,
                    description.text().to_owned(),
                    description.cursor(),
                    2,
                    Some(usize::from(width.saturating_sub(2))),
                ),
                SkillWizardStep::Content => {
                    let Some(Surface::SkillWizard { content, .. }) = Some(surface) else {
                        unreachable!()
                    };
                    let rows = layout
                        .surface_hits
                        .iter()
                        .enumerate()
                        .filter_map(|(row, hit)| match hit {
                            SurfaceHit::Scroll(ScrollViewHit::Content(source_row)) => {
                                Some((row, *source_row))
                            }
                            _ => None,
                        })
                        .collect::<Vec<_>>();
                    for (row, rendered_row) in rows {
                        layout.overlay_input_hit(
                            row,
                            SurfaceInputHit::Multiline {
                                input: content.clone(),
                                rendered_row,
                                content_column: 2,
                                width,
                            },
                        );
                    }
                }
                SkillWizardStep::Scope => {}
            },
            Surface::AgentWizard {
                name,
                description,
                prompt,
                step,
                cursor,
                ..
            } => match step {
                AgentWizardStep::Name => single(
                    2,
                    name.clone(),
                    *cursor,
                    2,
                    Some(usize::from(width.saturating_sub(2))),
                ),
                AgentWizardStep::Description => single(
                    2,
                    description.clone(),
                    *cursor,
                    2,
                    Some(usize::from(width.saturating_sub(2))),
                ),
                AgentWizardStep::Prompt => {
                    let rows = layout
                        .surface_hits
                        .iter()
                        .enumerate()
                        .filter_map(|(row, hit)| match hit {
                            SurfaceHit::Scroll(ScrollViewHit::Content(source_row)) => {
                                Some((row, *source_row))
                            }
                            _ => None,
                        })
                        .collect::<Vec<_>>();
                    for (row, rendered_row) in rows {
                        layout.overlay_input_hit(
                            row,
                            SurfaceInputHit::Multiline {
                                input: prompt.clone(),
                                rendered_row,
                                content_column: 2,
                                width,
                            },
                        );
                    }
                }
                _ => {}
            },
            Surface::McpJsonEdit { editor, .. } => {
                let rows = layout
                    .surface_hits
                    .iter()
                    .enumerate()
                    .filter_map(|(row, hit)| match hit {
                        SurfaceHit::Scroll(ScrollViewHit::Content(source_row)) => {
                            Some((row, *source_row))
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                for (row, rendered_row) in rows {
                    layout.overlay_input_hit(
                        row,
                        SurfaceInputHit::Multiline {
                            input: editor.clone(),
                            rendered_row,
                            content_column: 2,
                            width,
                        },
                    );
                }
            }
            Surface::SettingInput { value, cursor, .. } => {
                single(
                    3,
                    value.clone(),
                    *cursor,
                    2,
                    Some(usize::from(width.saturating_sub(2))),
                );
            }
            Surface::Settings {
                query,
                query_cursor,
                ..
            } => {
                let title_width = menu_title("Settings", None).width();
                let prefix_width = "  search: ".width();
                single(
                    0,
                    query.clone(),
                    *query_cursor,
                    title_width + prefix_width,
                    Some(
                        usize::from(width)
                            .saturating_sub(title_width)
                            .saturating_sub(prefix_width),
                    ),
                );
            }
            Surface::StatusLine {
                mode: StatusLineEditorMode::Hex { input, cursor, .. },
                ..
            } => {
                single(
                    3,
                    input.clone(),
                    *cursor,
                    2,
                    Some(usize::from(width.saturating_sub(2))),
                );
            }
            _ => {}
        }
    }
}

pub(crate) fn surface_scroll_hits(
    surface: &Surface,
    workspace: &Path,
    width: u16,
    viewport_rows: usize,
) -> Option<(usize, ScrollViewLayout)> {
    if let Some(layout) = multiline_surface_scroll_hits(surface, workspace, width, viewport_rows) {
        return Some(layout);
    }
    match surface {
        Surface::Help {
            tab,
            command_rows,
            key_rows,
            list,
        } => {
            let rows = match tab {
                HelpTab::Commands => command_rows,
                HelpTab::Keybindings => key_rows,
            };
            let mut state = *list;
            state.reconcile(rows.len(), VISIBLE_HELP_ITEMS, false);
            Some((
                3,
                ScrollViewWidget::new(VISIBLE_HELP_ITEMS).render(&state, |_| Line::default()),
            ))
        }
        Surface::Permission {
            request,
            diff_scroll,
            selected,
            scope,
            denial_note,
            editing_note,
            note_cursor,
        } => {
            let (prefix, preview, mut suffix) =
                permission_surface_parts(request, *selected, *scope, workspace, width);
            append_permission_note(&mut suffix, denial_note, *editing_note, *note_cursor, width);
            let preview = preview?;
            if prefix.len().saturating_add(suffix.len()) > viewport_rows {
                return None;
            }
            let capacity = viewport_rows.saturating_sub(prefix.len().saturating_add(suffix.len()));
            if capacity == 0 {
                return None;
            }
            let content_rows = permission_preview_line_count(preview, width);
            let mut state = ScrollViewState {
                offset: *diff_scroll,
                content_rows,
            };
            state.reconcile(content_rows, capacity, false);
            Some((
                prefix.len().saturating_sub(1),
                ScrollViewWidget::new(capacity).render(&state, |_| Line::default()),
            ))
        }
        Surface::McpMutationPreview {
            preview, details, ..
        } => {
            let content_rows = mcp_mutation_detail_lines(preview).len();
            let list_rows: usize = 4;
            let prefix: usize = 1 + list_rows;
            let capacity = viewport_rows
                .saturating_sub(prefix.saturating_add(2))
                .min(VISIBLE_HELP_ITEMS);
            let mut state = *details;
            state.reconcile(content_rows, capacity, false);
            Some((
                prefix,
                ScrollViewWidget::new(capacity).render(&state, |_| Line::default()),
            ))
        }
        Surface::Expanded {
            view,
            scroll,
            viewport_rows: output_viewport_rows,
        } if !matches!(view, ExpandedView::Terminal { .. }) => {
            let ExpandedTextContent { header, body, .. } =
                expanded_text_content(view, workspace, width);
            let panel_rows = viewport_rows;
            let (capacity, maximum) = expanded_text_scroll_metrics(
                header.len(),
                body.len(),
                *output_viewport_rows,
                panel_rows,
            );
            let state = ScrollViewState {
                offset: (*scroll).min(maximum),
                content_rows: body.len(),
            };
            Some((
                header.len().saturating_sub(1),
                ScrollViewWidget::new(capacity)
                    .indicators(ScrollViewAction::Home, ScrollViewAction::End)
                    .render(&state, |_| Line::default()),
            ))
        }
        _ => None,
    }
}

fn multiline_surface_scroll_hits(
    surface: &Surface,
    workspace: &Path,
    width: u16,
    viewport_rows: usize,
) -> Option<(usize, ScrollViewLayout)> {
    let (prefix, layout) = match surface {
        Surface::AgentWizard {
            prompt,
            step: AgentWizardStep::Prompt,
            ..
        } => (
            1,
            prompt.render_scrolled(
                width,
                multiline_content_capacity(viewport_rows),
                Some(&agent_prompt_placeholder()),
            ),
        ),
        Surface::McpJsonEdit { editor, .. } => (
            1,
            editor.render_scrolled(width, multiline_content_capacity(viewport_rows), None),
        ),
        Surface::Question {
            answers,
            question_index,
            editing_note: true,
            note_cursor,
            ..
        } => {
            let note = answers
                .get(*question_index)?
                .note
                .as_deref()
                .unwrap_or_default();
            let layout = note_input_layout(note, *note_cursor, width);
            let prefix = render_surface_lines(surface, workspace, width, viewport_rows)
                .len()
                .saturating_sub(layout.lines.len());
            (prefix, layout)
        }
        Surface::PlanCompletion {
            note,
            editing_note: true,
            note_cursor,
            ..
        } => {
            let layout = note_input_layout(note, *note_cursor, width);
            let prefix = render_surface_lines(surface, workspace, width, viewport_rows)
                .len()
                .saturating_sub(layout.lines.len());
            (prefix, layout)
        }
        Surface::Permission {
            denial_note,
            editing_note: true,
            note_cursor,
            ..
        } => {
            let layout = note_input_layout(denial_note, *note_cursor, width);
            let rendered_len = render_surface_lines(surface, workspace, width, viewport_rows).len();
            if rendered_len < layout.lines.len() {
                return None;
            }
            let prefix = rendered_len.saturating_sub(layout.lines.len());
            (prefix, layout)
        }
        Surface::SkillWizard {
            content,
            step: super::SkillWizardStep::Content,
            ..
        } => (
            1,
            content.render_scrolled(width, multiline_content_capacity(viewport_rows), None),
        ),
        _ => return None,
    };
    Some((prefix, layout))
}

pub(crate) fn multiline_content_capacity(viewport_rows: usize) -> usize {
    viewport_rows.saturating_sub(3).max(1)
}

pub(crate) const NOTE_MAX_VISUAL_ROWS: usize =
    super::multiline_input::INTERACTION_NOTE_MAX_VISUAL_ROWS;

pub(crate) fn note_input_width(width: u16) -> u16 {
    width.max(1)
}

pub(crate) fn note_input_capacity(input: &MultilineInput, width: u16) -> usize {
    input
        .visual_row_count(note_input_width(width))
        .clamp(1, NOTE_MAX_VISUAL_ROWS)
}

pub(crate) fn note_input_layout(value: &str, cursor: usize, width: u16) -> ScrollViewLayout {
    let input = MultilineInput::new(value.to_owned(), cursor);
    let input_width = note_input_width(width);
    let capacity = note_input_capacity(&input, width);
    let placeholder = Line::from(vec![
        Span::styled("A", SEARCH_CURSOR_STYLE),
        Span::styled("dd note", DIM_STYLE),
    ]);
    let mut layout = input.render_scrolled(input_width, capacity, Some(&placeholder));
    let mut first_visible_row = true;
    for (line, hit) in layout.lines.iter_mut().zip(&layout.hits) {
        if matches!(hit, ScrollViewHit::Indicator(_))
            && line
                .spans
                .first()
                .is_some_and(|span| span.content == "  ↑" || span.content == "  ↓")
        {
            let marker = if line.spans[0].content.ends_with('↑') {
                "↑"
            } else {
                "↓"
            };
            line.spans[0].content = marker.into();
            continue;
        }
        let ScrollViewHit::Content(_) = hit else {
            continue;
        };
        if line.spans.first().is_some_and(|span| span.content == "  ") {
            line.spans.remove(0);
        }
        line.spans.insert(
            0,
            Span::styled(
                if first_visible_row { "› " } else { "  " },
                if first_visible_row {
                    SELECTED_STYLE
                } else {
                    Style::default()
                },
            ),
        );
        first_visible_row = false;
    }
    layout
}

fn saved_note_lines(value: &str, width: u16) -> Vec<Line<'static>> {
    value
        .split('\n')
        .enumerate()
        .flat_map(|(index, row)| {
            wrap_log_line(
                &Line::from(vec![
                    Span::raw(if index == 0 { "› " } else { "  " }),
                    Span::raw(terminal_safe(row)),
                ]),
                width,
            )
        })
        .collect()
}

fn agent_prompt_placeholder() -> Line<'static> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled("i", SEARCH_CURSOR_STYLE),
        Span::styled("nstructions for this agent", DIM_STYLE),
    ])
}

pub(crate) fn surface_list_hits(
    surface: &Surface,
    workspace: &Path,
    width: u16,
    viewport_rows: usize,
) -> Option<(usize, Vec<ListHit>)> {
    if let Surface::AgentWizard {
        step: AgentWizardStep::Parent,
        parents,
        parent_list,
        editing,
        ..
    } = surface
    {
        let mut state = *parent_list;
        state.reconcile(ListMode::Selectable, parents.len(), VISIBLE_MENU_ITEMS);
        let policy = if *editing {
            LinePolicy::Truncate
        } else {
            LinePolicy::Wrap
        };
        let layout = ListWidget::new(width, VISIBLE_MENU_ITEMS)
            .policy(policy)
            .render(&state, |index, _, selected| {
                let entry = &parents[index];
                vec![menu_choice_line(
                    selected,
                    entry,
                    if entry == "None" { "no parent" } else { "" },
                )]
            });
        return Some((1, layout.hits));
    }
    if let Surface::Help {
        tab,
        command_rows,
        key_rows,
        list,
    } = surface
    {
        let rows = match tab {
            HelpTab::Commands => command_rows,
            HelpTab::Keybindings => key_rows,
        };
        let mut state = *list;
        state.reconcile(rows.len(), VISIBLE_HELP_ITEMS, false);
        let hits = ScrollViewWidget::new(VISIBLE_HELP_ITEMS)
            .render(&state, |_| Line::default())
            .hits
            .into_iter()
            .map(|hit| match hit {
                super::scroll::ScrollViewHit::Indicator(ScrollViewAction::PagePrevious) => {
                    ListHit::Up
                }
                super::scroll::ScrollViewHit::Indicator(ScrollViewAction::PageNext) => {
                    ListHit::Down
                }
                _ => ListHit::None,
            })
            .collect();
        return Some((3, hits));
    }
    if let Surface::Question {
        request,
        question_index,
        option_index,
        editing_note,
        ..
    } = surface
    {
        let InteractionRequestKind::Question { questions } = &request.kind else {
            return None;
        };
        let question = questions.get(*question_index)?;
        let prompt = Line::from(Span::styled(
            format!("  {}", terminal_safe(&question.question)),
            Style::default().add_modifier(Modifier::BOLD),
        ));
        let prefix =
            1 + usize::from(request.origin.is_some()) + 1 + wrap_log_line(&prompt, width).len();
        let label_width = question
            .options
            .iter()
            .map(|choice| terminal_safe(&choice.label).width())
            .chain(std::iter::once(
                cagent_agent::QUESTION_NONE_OF_THE_ABOVE.width(),
            ))
            .max()
            .unwrap_or_default()
            .min(usize::from(width).saturating_sub(5).max(1));
        let count = question.options.len() + 1;
        let state = ListState::selectable_at(count, *option_index, count.max(1));
        let hits = ListWidget::new(width, count.max(1))
            .indicators(true, !*editing_note)
            .hits(&state, |index| {
                question.options.get(index).map_or_else(
                    || {
                        question_option_lines(
                            false,
                            cagent_agent::QUESTION_NONE_OF_THE_ABOVE,
                            "Optionally add a note with Tab",
                            label_width,
                            width,
                        )
                        .len()
                    },
                    |choice| {
                        question_option_lines(
                            false,
                            &choice.label,
                            &choice.description,
                            label_width,
                            width,
                        )
                        .len()
                    },
                )
            });
        return Some((prefix, hits));
    }
    if let Surface::PlanCompletion {
        selected,
        editing_note,
        ..
    } = surface
    {
        let state = ListState::selectable_at(4, *selected, 4);
        return Some((
            3,
            ListWidget::new(width, 4)
                .indicators(true, !*editing_note)
                .hits(&state, |_| 1),
        ));
    }
    if let Surface::Permission {
        request,
        selected,
        scope,
        denial_note,
        editing_note,
        note_cursor,
        ..
    } = surface
    {
        let (prefix, _, mut suffix) =
            permission_surface_parts(request, *selected, *scope, workspace, width);
        append_permission_note(&mut suffix, denial_note, *editing_note, *note_cursor, width);
        let choice_count = permission_approval_choices(request, *scope).len();
        let state = ListState::selectable_at(choice_count, *selected, choice_count.max(1));
        let mut hits = ListWidget::new(width, choice_count.max(1))
            .indicators(true, false)
            .hits(&state, |_| 1);
        hits.extend(std::iter::repeat_n(
            ListHit::None,
            suffix.len().saturating_sub(hits.len()),
        ));
        let rendered_rows = permission_surface_lines(
            request,
            *selected,
            *scope,
            match surface {
                Surface::Permission { diff_scroll, .. } => *diff_scroll,
                _ => unreachable!(),
            },
            denial_note,
            *editing_note,
            *note_cursor,
            workspace,
            width,
            viewport_rows,
        )
        .len();
        let start = if prefix.len().saturating_add(suffix.len()) > viewport_rows {
            viewport_rows.saturating_sub(suffix.len())
        } else {
            rendered_rows.saturating_sub(suffix.len())
        };
        hits.truncate(rendered_rows.saturating_sub(start));
        return Some((start, hits));
    }
    let (prefix, state, capacity, visual_rows, empty) = match surface {
        Surface::Providers {
            rows, query, list, ..
        } => {
            let mut state = *list;
            state.reconcile(
                ListMode::Selectable,
                filter_provider_picker_rows(rows, query).len() + 1,
                VISIBLE_MENU_ITEMS,
            );
            (1, state, VISIBLE_MENU_ITEMS, 1, false)
        }
        Surface::Models {
            rows,
            query,
            list,
            selection_target,
            ..
        } => {
            let mut state = *list;
            state.reconcile(
                ListMode::Selectable,
                filter_model_picker_rows(rows, query).len(),
                VISIBLE_MENU_ITEMS,
            );
            let prefix = if matches!(selection_target, super::ModelSelectionTarget::Setting(_)) {
                2
            } else {
                1
            };
            (prefix, state, VISIBLE_MENU_ITEMS, 1, false)
        }
        Surface::Effort { rows, list, .. } => {
            let mut state = *list;
            state.reconcile(ListMode::Selectable, rows.len(), VISIBLE_MENU_ITEMS);
            (1, state, VISIBLE_MENU_ITEMS, 1, false)
        }
        Surface::Profiles { rows, list, .. } => {
            let mut state = *list;
            state.reconcile(ListMode::Selectable, rows.len(), VISIBLE_MENU_ITEMS);
            (1, state, VISIBLE_MENU_ITEMS, 1, false)
        }
        Surface::Skills { rows, list } => {
            let mut state = *list;
            state.reconcile(ListMode::Selectable, rows.len() + 2, VISIBLE_MENU_ITEMS);
            (1, state, VISIBLE_MENU_ITEMS, 1, false)
        }
        Surface::SkillWizard {
            step: super::SkillWizardStep::Scope,
            scope_list,
            ..
        } => {
            let mut state = *scope_list;
            state.reconcile(ListMode::Selectable, 2, VISIBLE_MENU_ITEMS);
            (1, state, VISIBLE_MENU_ITEMS, 1, false)
        }
        Surface::SkillWizard { .. } => return None,
        Surface::Worktrees { rows, list } => {
            let mut state = *list;
            state.reconcile(ListMode::Selectable, rows.len() + 1, VISIBLE_MENU_ITEMS);
            (1, state, VISIBLE_MENU_ITEMS, 1, false)
        }
        Surface::AgentEdit { list, .. } => {
            let mut state = *list;
            state.reconcile(ListMode::Selectable, 5, VISIBLE_MENU_ITEMS);
            (1, state, VISIBLE_MENU_ITEMS, 1, false)
        }
        Surface::AgentWizard {
            step: AgentWizardStep::Availability,
            availability,
            ..
        } => (
            1,
            ListState::selectable_at(3, *availability, VISIBLE_MENU_ITEMS),
            VISIBLE_MENU_ITEMS,
            1,
            false,
        ),
        Surface::WebSearchPicker {
            rows, list, query, ..
        } => {
            let filter = query.to_lowercase();
            let count = rows
                .iter()
                .filter(|row| row.provider.label().contains(&filter))
                .count()
                + 1;
            let mut state = *list;
            state.reconcile(ListMode::Selectable, count, VISIBLE_MENU_ITEMS);
            (1, state, VISIBLE_MENU_ITEMS, 1, false)
        }
        Surface::McpServers { rows, list } => {
            let mut state = *list;
            state.reconcile(ListMode::Selectable, rows.len() + 3, VISIBLE_MENU_ITEMS);
            (1, state, VISIBLE_MENU_ITEMS, 1, false)
        }
        Surface::McpCatalog { rows, list } => {
            let mut state = *list;
            state.reconcile(ListMode::Selectable, rows.len() + 1, VISIBLE_MENU_ITEMS);
            (1, state, VISIBLE_MENU_ITEMS, 1, false)
        }
        Surface::McpServer {
            server, selected, ..
        } => (
            1,
            ListState::selectable_at(
                mcp_server_menu_item_count(server),
                *selected,
                VISIBLE_MENU_ITEMS,
            ),
            VISIBLE_MENU_ITEMS,
            1,
            false,
        ),
        Surface::McpPackageSetup { draft, selected } => (
            1,
            ListState::selectable_at(
                1 + draft.package.parameters.len() + draft.package.secrets.len(),
                *selected,
                VISIBLE_MENU_ITEMS,
            ),
            VISIBLE_MENU_ITEMS,
            1,
            false,
        ),
        Surface::McpRemoveConfirm { selected, .. }
        | Surface::SkillDeleteConfirm { selected, .. }
        | Surface::KillSupervisedWork { selected, .. } => (
            1,
            ListState::selectable_at(2, *selected, VISIBLE_MENU_ITEMS),
            VISIBLE_MENU_ITEMS,
            1,
            false,
        ),
        Surface::McpMutationPreview { list, .. } => {
            let mut state = *list;
            state.reconcile(ListMode::Selectable, 2, VISIBLE_MENU_ITEMS);
            (1, state, VISIBLE_MENU_ITEMS, 1, false)
        }
        Surface::McpAddScope { selected } => (
            1,
            ListState::selectable_at(3, *selected, VISIBLE_MENU_ITEMS),
            VISIBLE_MENU_ITEMS,
            1,
            false,
        ),
        Surface::McpTransport { selected, .. } => (
            1,
            ListState::selectable_at(4, *selected, VISIBLE_MENU_ITEMS),
            VISIBLE_MENU_ITEMS,
            1,
            false,
        ),
        Surface::McpForm { draft, list } => {
            let mut state = *list;
            state.reconcile(
                ListMode::Selectable,
                mcp_form_item_count(draft),
                VISIBLE_MENU_ITEMS,
            );
            (1, state, VISIBLE_MENU_ITEMS, 1, false)
        }
        Surface::Settings {
            rows,
            section,
            list,
            query,
            ..
        } => {
            let capacity = settings_item_capacity(query);
            let mut state = *list;
            state.reconcile(
                ListMode::Selectable,
                filter_settings_rows(rows, *section, query).len(),
                capacity,
            );
            (
                if query.is_empty() { 3 } else { 1 },
                state,
                capacity,
                2,
                false,
            )
        }
        Surface::SettingChoices { choices, list, .. } => {
            let mut state = *list;
            state.reconcile(ListMode::Selectable, choices.len(), VISIBLE_MENU_ITEMS);
            (2, state, VISIBLE_MENU_ITEMS, 1, false)
        }
        Surface::SupervisedWork {
            rows,
            list,
            show_past,
        } => {
            let mut state = *list;
            let count = rows
                .iter()
                .filter(|item| item.active() != *show_past)
                .count();
            state.reconcile(ListMode::Selectable, count, VISIBLE_MENU_ITEMS);
            (1, state, VISIBLE_MENU_ITEMS, 1, count == 0)
        }
        Surface::McpTools { tools, list, .. } => {
            let mut state = *list;
            state.reconcile(ListMode::Selectable, tools.len(), VISIBLE_MENU_ITEMS);
            (1, state, VISIBLE_MENU_ITEMS, 1, tools.is_empty())
        }
        Surface::Paths { rows, list, .. } => {
            let mut state = *list;
            state.reconcile(ListMode::Selectable, rows.len(), VISIBLE_MENU_ITEMS);
            (2, state, VISIBLE_MENU_ITEMS, 1, false)
        }
        Surface::Conversations {
            rows,
            list,
            query,
            include_archived,
            ..
        } => {
            let visible = conversation_picker_indexes(rows, query);
            let capacity = conversation_picker_item_capacity(
                viewport_rows,
                *include_archived,
                !query.is_empty() && visible.is_empty(),
            );
            let mut state = *list;
            state.reconcile(ListMode::Selectable, visible.len(), capacity);
            (2, state, capacity, 1, false)
        }
        Surface::HistoryTree {
            rows, list, query, ..
        } => {
            let visible = history_picker_indexes(rows, query);
            let capacity = history_picker_item_capacity(viewport_rows);
            let mut state = *list;
            state.reconcile(ListMode::Selectable, visible.len(), capacity);
            (2, state, capacity, 1, false)
        }
        Surface::StatusLine {
            mode: StatusLineEditorMode::Colors { list, .. },
            ..
        } => {
            let choices = status_line_color_choices();
            let mut state = *list;
            state.reconcile(ListMode::Selectable, choices.len(), VISIBLE_MENU_ITEMS);
            (3, state, VISIBLE_MENU_ITEMS, 1, false)
        }
        Surface::StatusLine {
            rows,
            list,
            mode: StatusLineEditorMode::Modules,
            ..
        } => {
            let mut state = *list;
            state.reconcile(ListMode::Selectable, rows.len(), VISIBLE_MENU_ITEMS);
            (3, state, VISIBLE_MENU_ITEMS, 1, false)
        }
        _ => return None,
    };
    let mut widget = if empty {
        ListWidget::new(width, capacity).empty_lines(vec![Line::default()])
    } else {
        ListWidget::new(width, capacity)
    };
    if matches!(surface, Surface::Settings { query, .. } if !query.is_empty()) {
        let occupied_rows = if state.item_count == 0 {
            2
        } else {
            state.visible_range(capacity).len() * visual_rows
        };
        widget = widget.spacing(0, capacity * visual_rows - occupied_rows);
    }
    Some((prefix, widget.hits(&state, |_| visual_rows)))
}

pub(crate) fn render_surface_lines(
    surface: &Surface,
    workspace: &Path,
    width: u16,
    viewport_rows: usize,
) -> Vec<Line<'static>> {
    match surface {
        Surface::Onboarding => vec![
            menu_title("Welcome to Cagent!", None),
            Line::default(),
            Line::from("  To get started, configure a provider and model."),
            Line::from(vec![
                Span::raw("  Press "),
                Span::styled("Enter", ACCENT_STYLE),
                Span::raw(" to begin setup, or "),
                Span::styled("Esc", DIM_STYLE),
                Span::raw(" to continue without it."),
            ]),
            Line::default(),
            Line::from(vec![
                Span::raw("  Run "),
                Span::styled("/help", ACCENT_STYLE),
                Span::raw(" for more help."),
            ]),
            Line::default(),
        ],
        Surface::Question {
            request,
            question_index,
            option_index,
            answers,
            answered: _,
            editing_note,
            note_cursor,
        } => {
            let InteractionRequestKind::Question { questions } = &request.kind else {
                return Vec::new();
            };
            let origin = request.origin.as_ref().map(|origin| match origin {
                cagent_agent::protocol::InteractionOrigin::SubAgent { id, profile } => {
                    format!("requested by {profile} ({id})")
                }
            });
            let mut title = menu_title(
                "Questions",
                Some(&format!("{}/{}", question_index + 1, questions.len())),
            );
            title.spans.push(Span::raw("  "));
            for (index, question) in questions.iter().enumerate() {
                if index > 0 {
                    title.spans.push(Span::raw("  "));
                }
                title.spans.push(Span::styled(
                    terminal_safe(&question.header),
                    if index == *question_index {
                        SELECTED_STYLE
                    } else {
                        DIM_STYLE
                    },
                ));
            }
            let mut lines = vec![title];
            if let Some(origin) = origin {
                lines.push(Line::from(Span::styled(format!("  {origin}"), DIM_STYLE)));
            }
            lines.push(Line::default());
            if let Some(question) = questions.get(*question_index) {
                let prompt = Line::from(Span::styled(
                    format!("  {}", terminal_safe(&question.question)),
                    Style::default().add_modifier(Modifier::BOLD),
                ));
                lines.extend(wrap_log_line(&prompt, width));
                let label_width = question
                    .options
                    .iter()
                    .map(|choice| terminal_safe(&choice.label).width())
                    .chain(std::iter::once(
                        cagent_agent::QUESTION_NONE_OF_THE_ABOVE.width(),
                    ))
                    .max()
                    .unwrap_or_default()
                    .min(usize::from(width).saturating_sub(5).max(1));
                let choice_count = question.options.len() + 1;
                let state =
                    ListState::selectable_at(choice_count, *option_index, choice_count.max(1));
                let choices = ListWidget::new(width, choice_count.max(1))
                    .indicators(true, !*editing_note)
                    .render(&state, |option, _, selected| {
                        question.options.get(option).map_or_else(
                            || {
                                question_option_lines(
                                    selected,
                                    cagent_agent::QUESTION_NONE_OF_THE_ABOVE,
                                    "Optionally add a note with Tab",
                                    label_width,
                                    width,
                                )
                            },
                            |choice| {
                                question_option_lines(
                                    selected,
                                    &choice.label,
                                    &choice.description,
                                    label_width,
                                    width,
                                )
                            },
                        )
                    });
                lines.extend(choices.lines);
                let note = answers[*question_index].note.clone().unwrap_or_default();
                if *editing_note {
                    lines.extend(note_input_layout(&note, *note_cursor, width).lines);
                } else if !note.is_empty() {
                    lines.push(Line::default());
                    lines.extend(saved_note_lines(&note, width));
                    lines.push(Line::default());
                }
            }
            lines
        }
        Surface::Permission {
            request,
            selected,
            scope,
            diff_scroll,
            denial_note,
            editing_note,
            note_cursor,
        } => permission_surface_lines(
            request,
            *selected,
            *scope,
            *diff_scroll,
            denial_note,
            *editing_note,
            *note_cursor,
            workspace,
            width,
            viewport_rows,
        ),
        Surface::PlanCompletion {
            request,
            selected,
            note,
            editing_note,
            note_cursor,
            implementation_mode_index,
            implementation_mode_colors,
            implementation_model,
            context_percent,
        } => {
            let InteractionRequestKind::PlanCompletion {
                implementation_modes,
                ..
            } = &request.kind
            else {
                return Vec::new();
            };
            let mode = implementation_modes
                .get(*implementation_mode_index)
                .map(String::as_str)
                .unwrap_or("unknown");
            let mut lines = vec![
                menu_title("Plan", None),
                Line::default(),
                Line::from(Span::styled(
                    "  Implement this plan?",
                    Style::default().add_modifier(Modifier::BOLD),
                )),
            ];
            let choices = plan_completion_choices(
                *selected,
                mode,
                implementation_model,
                implementation_mode_colors
                    .get(*implementation_mode_index)
                    .copied()
                    .unwrap_or(StatusLineColor::Cyan),
                *context_percent,
            );
            let state = ListState::selectable_at(choices.len(), *selected, choices.len());
            lines.extend(
                ListWidget::new(width, choices.len())
                    .indicators(true, !*editing_note)
                    .render(&state, |index, _, _| vec![choices[index].clone()])
                    .lines,
            );
            if *editing_note {
                lines.extend(note_input_layout(note, *note_cursor, width).lines);
            }
            lines
        }
        Surface::PermissionRuleEdit {
            rule,
            pattern,
            cursor,
            scope,
            ..
        } => {
            let label = rule
                .editable_pattern()
                .map_or("pattern", |(_, label)| label);
            vec![
                menu_title(
                    "Permission rule",
                    Some(&format!(
                        "{} · edit {label} · {}",
                        match scope {
                            cagent_agent::permissions::PermissionScope::Conversation =>
                                "conversation",
                            cagent_agent::permissions::PermissionScope::ConversationGlobal => {
                                "global"
                            }
                            cagent_agent::permissions::PermissionScope::Project => "project",
                            cagent_agent::permissions::PermissionScope::Global => "global",
                        },
                        rule.editable_pattern_hint(),
                    )),
                ),
                Line::default(),
                Line::from({
                    let mut spans = vec![Span::raw("  ")];
                    let mut input = SingleLineInput::new(pattern.clone(), *cursor);
                    if rule.tool.as_deref() == Some("bash") {
                        input = input.with_syntax_language("bash");
                    }
                    let text_style = if rule.path.is_some() {
                        CHIP_STYLE
                    } else {
                        Style::default()
                    };
                    spans.extend(
                        input
                            .render_bounded(
                                usize::from(width.saturating_sub(2)),
                                text_style,
                                SEARCH_CURSOR_STYLE,
                            )
                            .spans,
                    );
                    spans
                }),
                Line::default(),
            ]
        }
        Surface::Permissions { rows, list } => permission_menu_lines(rows, list, width),
        Surface::Usage { overview, list } => usage_menu_lines(overview, list, width),
        Surface::UsageBreakdown { kind, rows, list } => {
            usage_breakdown_lines(*kind, rows, list, width)
        }
        Surface::UsageResetConfirm { selected } => {
            let choices = ["Cancel", "Reset all global usage"];
            let state = ListState::selectable_at(2, *selected, VISIBLE_MENU_ITEMS);
            let mut lines = vec![menu_title(
                "Reset usage",
                Some("canonical conversation usage is retained"),
            )];
            lines.extend(
                ListWidget::new(width, VISIBLE_MENU_ITEMS)
                    .render(&state, |index, _, selected| {
                        vec![menu_choice_line(selected, choices[index], "")]
                    })
                    .lines,
            );
            lines
        }
        Surface::PersistentPermissionEdit {
            rule,
            pattern,
            cursor,
            scope,
            ..
        } => {
            let label = rule
                .editable_pattern()
                .map_or("pattern", |(_, label)| label);
            vec![
                menu_title(
                    "Permission rule",
                    Some(&format!(
                        "{} · edit {label} · {}",
                        match scope {
                            cagent_agent::permissions::PermissionScope::Conversation =>
                                "conversation",
                            cagent_agent::permissions::PermissionScope::ConversationGlobal => {
                                "global"
                            }
                            cagent_agent::permissions::PermissionScope::Project => "project",
                            cagent_agent::permissions::PermissionScope::Global => "global",
                        },
                        rule.editable_pattern_hint(),
                    )),
                ),
                Line::default(),
                Line::from({
                    let mut spans = vec![Span::raw("  ")];
                    let mut input = SingleLineInput::new(pattern.clone(), *cursor);
                    if rule.tool.as_deref() == Some("bash") {
                        input = input.with_syntax_language("bash");
                    }
                    let text_style = if rule.path.is_some() {
                        CHIP_STYLE
                    } else {
                        Style::default()
                    };
                    spans.extend(
                        input
                            .render_bounded(
                                usize::from(width.saturating_sub(2)),
                                text_style,
                                SEARCH_CURSOR_STYLE,
                            )
                            .spans,
                    );
                    spans
                }),
                Line::default(),
            ]
        }
        Surface::Rename { title, cursor } => vec![
            menu_title("Rename conversation", None),
            Line::default(),
            Line::from({
                let mut spans = vec![Span::raw("  ")];
                spans.extend(
                    SingleLineInput::new(title.clone(), *cursor)
                        .render_bounded(
                            usize::from(width.saturating_sub(2)),
                            Style::default(),
                            SEARCH_CURSOR_STYLE,
                        )
                        .spans,
                );
                spans
            }),
            Line::default(),
        ],
        Surface::Worktrees { rows, list } => {
            let mut state = *list;
            state.reconcile(ListMode::Selectable, rows.len() + 1, VISIBLE_MENU_ITEMS);
            let mut lines = vec![menu_title("Worktrees", None)];
            let layout = ListWidget::new(width, VISIBLE_MENU_ITEMS)
                .policy(LinePolicy::Truncate)
                .render(&state, |index, _, selected| {
                    let (label, detail) = if index == 0 {
                        ("New".to_owned(), "create and enter a worktree".to_owned())
                    } else {
                        let row = &rows[index - 1];
                        (
                            row.name.clone(),
                            if row.current {
                                "current".to_owned()
                            } else {
                                row.path.display().to_string()
                            },
                        )
                    };
                    vec![menu_choice_line(selected, label, detail)]
                });
            lines.extend(layout.lines);
            lines
        }
        Surface::WorktreeNew {
            name,
            base,
            cursor,
            editing_base,
        } => {
            let (label, value) = if *editing_base {
                ("Base", base)
            } else {
                ("Name", name)
            };
            vec![
                menu_title("New worktree", None),
                Line::default(),
                Line::from({
                    let mut spans = vec![Span::raw(format!("  {label}: "))];
                    spans.extend(
                        SingleLineInput::new(value.clone(), *cursor)
                            .render_bounded(
                                usize::from(width).saturating_sub(label.width() + 4),
                                Style::default(),
                                SEARCH_CURSOR_STYLE,
                            )
                            .spans,
                    );
                    spans
                }),
                Line::default(),
            ]
        }
        Surface::WebSearchPicker {
            rows,
            list,
            query,
            query_cursor,
        } => {
            let filter = query.to_lowercase();
            let visible = rows
                .iter()
                .filter(|row| row.provider.label().contains(&filter))
                .collect::<Vec<_>>();
            let mut state = *list;
            state.reconcile(ListMode::Selectable, visible.len() + 1, VISIBLE_MENU_ITEMS);
            let mut lines = vec![search_menu_title(
                "Web search",
                "search: ",
                &SingleLineInput::new(query.clone(), *query_cursor),
                width,
            )];
            let layout = ListWidget::new(width, VISIBLE_MENU_ITEMS)
                .policy(LinePolicy::Truncate)
                .render(&state, |index, _, selected| {
                    let row = (index != 0).then(|| visible[index - 1]);
                    let label = row.map_or_else(
                        || "Close".to_owned(),
                        |row| row.provider.display_name().to_owned(),
                    );
                    let style = if row.is_some_and(|row| !row.ready) {
                        DIM_STYLE
                    } else if selected {
                        SELECTED_STYLE
                    } else {
                        Style::default()
                    };
                    let mut spans = vec![
                        Span::styled(if selected { "› " } else { "  " }, style),
                        Span::styled(label, style),
                    ];
                    if let Some(row) = row {
                        if row.active {
                            let active_style = if row.ready
                                || row.provider
                                    != cagent_agent::web_search::WebSearchProvider::Chatgpt
                            {
                                ENABLED_STYLE
                            } else {
                                DIM_STYLE
                            };
                            spans.push(Span::styled("  active", active_style));
                        }
                        if !row.ready {
                            let detail = if row.provider
                                == cagent_agent::web_search::WebSearchProvider::Chatgpt
                            {
                                " · subscription required"
                            } else {
                                " · not setup"
                            };
                            spans.push(Span::styled(
                                detail,
                                if row.provider
                                    == cagent_agent::web_search::WebSearchProvider::Chatgpt
                                {
                                    DIM_STYLE
                                } else {
                                    ERROR_STYLE
                                },
                            ));
                        } else if !row.active {
                            spans.push(Span::styled("  ready", DIM_STYLE));
                        }
                    }
                    vec![Line::from(spans)]
                });
            lines.extend(layout.lines);
            lines
        }
        Surface::WebSearchSetup {
            provider,
            value,
            cursor,
        } => {
            let label = if *provider == cagent_agent::web_search::WebSearchProvider::Exa {
                "API key"
            } else {
                "Base URL"
            };
            vec![
                menu_title("Set up web search", Some(provider.label())),
                Line::default(),
                if *provider == cagent_agent::web_search::WebSearchProvider::Exa {
                    masked_input_line(format!("  {label}: "), value, *cursor, width)
                } else {
                    Line::from({
                        let mut spans = vec![Span::raw(format!("  {label}: "))];
                        spans.extend(
                            SingleLineInput::new(value.clone(), *cursor)
                                .render_bounded(
                                    usize::from(width).saturating_sub(2 + label.width() + 2),
                                    Style::default(),
                                    SEARCH_CURSOR_STYLE,
                                )
                                .spans,
                        );
                        spans
                    })
                },
                Line::default(),
                Line::from(vec![
                    Span::styled("  Fallback: ", DIM_STYLE),
                    Span::styled(
                        if *provider == cagent_agent::web_search::WebSearchProvider::Exa {
                            "EXA_API_KEY"
                        } else {
                            "SEARXNG_URL"
                        },
                        ACCENT_STYLE,
                    ),
                    Span::styled(" from the environment.", DIM_STYLE),
                ]),
                Line::default(),
            ]
        }
        Surface::ProvidersRequired => required_surface(
            "Providers",
            "No providers configured. Press Enter to configure.",
        ),
        Surface::ModelRequired => required_surface("Models", "no model configured"),
        Surface::ModelsUnavailable => {
            required_surface("Models", "No models available from enabled providers.")
        }
        Surface::ProviderSetup {
            provider,
            instructions,
            credential_environment_variable,
            auth_challenge,
            managed_auth,
            api_key_auth,
            api_key,
            api_key_cursor,
            auth_flows,
            authenticated,
            ..
        } => {
            let mut lines = vec![
                menu_title("Configure provider", Some(provider)),
                Line::default(),
            ];
            lines.push(if *authenticated {
                Line::from(Span::styled(
                    format!("  {}", terminal_safe(instructions)),
                    ENABLED_STYLE,
                ))
            } else if *api_key_auth {
                masked_input_line("  API key: ".into(), api_key, *api_key_cursor, width)
            } else if let Some(cagent_agent::provider::AuthChallenge::Device {
                verification_url,
                user_code,
                ..
            }) = auth_challenge
            {
                Line::from(vec![
                    Span::raw("  Open "),
                    Span::styled(
                        terminal_safe(verification_url),
                        ACCENT_STYLE.add_modifier(ratatui::style::Modifier::UNDERLINED),
                    ),
                    Span::raw(" and enter "),
                    Span::styled(terminal_safe(user_code), ACCENT_STYLE),
                    Span::raw("."),
                ])
            } else if *managed_auth {
                if auth_flows == &[cagent_agent::provider::AuthFlow::DeviceCode] {
                    Line::from(vec![
                        Span::raw(format!(
                            "  Connect your {provider} with device authentication ("
                        )),
                        Span::styled("Enter", ACCENT_STYLE),
                        Span::raw(")."),
                    ])
                } else if auth_flows == &[cagent_agent::provider::AuthFlow::BrowserPkce] {
                    Line::from(vec![
                        Span::raw(format!(
                            "  Connect your {provider} with browser authentication ("
                        )),
                        Span::styled("Enter", ACCENT_STYLE),
                        Span::raw(")."),
                    ])
                } else {
                    Line::from(vec![
                        Span::raw(format!("  Connect your {provider} with browser (")),
                        Span::styled("Enter", ACCENT_STYLE),
                        Span::raw(") or device authentication ("),
                        Span::styled("d", ACCENT_STYLE),
                        Span::raw(")."),
                    ])
                }
            } else if let Some(variable) = credential_environment_variable
                && let Some((before, after)) = instructions.split_once(variable)
            {
                Line::from(vec![
                    Span::raw(format!("  {}", terminal_safe(before))),
                    Span::styled(terminal_safe(variable), ACCENT_STYLE),
                    Span::raw(terminal_safe(after)),
                ])
            } else {
                Line::from(format!("  {}", terminal_safe(instructions)))
            });
            if *api_key_auth && let Some(variable) = credential_environment_variable {
                lines.push(Line::default());
                lines.push(Line::from(vec![
                    Span::styled("  Fallback: ", DIM_STYLE),
                    Span::styled(terminal_safe(variable), ACCENT_STYLE),
                    Span::styled(" from the environment.", DIM_STYLE),
                ]));
            }
            if !*authenticated
                && auth_flows == &[cagent_agent::provider::AuthFlow::DeviceCode]
                && matches!(
                    auth_challenge,
                    Some(cagent_agent::provider::AuthChallenge::Device { .. })
                )
            {
                lines.push(Line::from(Span::styled(
                    "  This will update automatically a few moments after you sign in.",
                    DIM_STYLE,
                )));
            }
            lines.push(Line::default());
            lines
        }
        Surface::Providers {
            rows,
            list,
            query,
            query_cursor,
            model_configuration_follows,
            ..
        } => {
            let mut lines = vec![search_menu_title(
                "Providers",
                "search: ",
                &SingleLineInput::new(query.clone(), *query_cursor),
                width,
            )];
            let continue_disabled =
                *model_configuration_follows && !rows.iter().any(|row| row.enabled);
            let visible = filter_provider_picker_rows(rows, query);
            let no_matches = !query.is_empty() && visible.is_empty();
            let mut state = *list;
            state.reconcile(ListMode::Selectable, visible.len() + 1, VISIBLE_MENU_ITEMS);
            let layout =
                ListWidget::new(width, VISIBLE_MENU_ITEMS).render(&state, |index, _, selected| {
                    let choice = if index == 0 {
                        (
                            selected,
                            continue_disabled,
                            if *model_configuration_follows {
                                "Continue".into()
                            } else {
                                "Close".into()
                            },
                            String::new(),
                            None,
                        )
                    } else {
                        let row = visible[index - 1];
                        (
                            selected,
                            false,
                            row.label.clone(),
                            format!("({})", row.id),
                            Some(row.status.clone()),
                        )
                    };
                    vec![menu_choice_line_with_status(
                        choice.0,
                        choice.1,
                        choice.2,
                        choice.3,
                        choice.4.as_deref(),
                    )]
                });
            lines.extend(layout.lines);
            if no_matches {
                lines.push(no_filter_matches("providers"));
                lines.push(Line::default());
            }
            lines
        }
        Surface::Models {
            rows,
            list,
            query,
            query_cursor,
            selection_target,
        } => {
            let visible = filter_model_picker_rows(rows, query);
            let no_matches = !query.is_empty() && visible.is_empty();
            let mut state = *list;
            state.reconcile(ListMode::Selectable, visible.len(), VISIBLE_MENU_ITEMS);
            let mut lines = vec![search_menu_title(
                match selection_target {
                    super::ModelSelectionTarget::Setting(setting) => &setting.label,
                    super::ModelSelectionTarget::Conversation
                    | super::ModelSelectionTarget::Mode(_) => "Models",
                },
                "search: ",
                &SingleLineInput::new(query.clone(), *query_cursor),
                width,
            )];
            if let super::ModelSelectionTarget::Setting(setting) = selection_target {
                lines.push(setting_description_line(&setting.description, width));
            }
            let layout = ListWidget::new(width, VISIBLE_MENU_ITEMS)
                .render(&state, |index, _, selected| {
                    vec![model_choice(selected, visible[index])]
                });
            lines.extend(layout.lines);
            if no_matches {
                lines.push(no_filter_matches("models"));
                lines.push(Line::default());
            }
            lines
        }
        Surface::Effort {
            model,
            rows,
            reasoning_control,
            list,
            ..
        } => {
            let title =
                if *reasoning_control == Some(cagent_agent::provider::ReasoningControl::Toggle) {
                    "Reasoning"
                } else {
                    "Effort"
                };
            let mut state = *list;
            state.reconcile(ListMode::Selectable, rows.len(), VISIBLE_MENU_ITEMS);
            let layout =
                ListWidget::new(width, VISIBLE_MENU_ITEMS).render(&state, |index, _, selected| {
                    let effort = &rows[index];
                    let label = match effort.as_str() {
                        "off" => "Off",
                        "on" => "On",
                        _ => effort,
                    };
                    vec![menu_choice_line(
                        selected,
                        format!("{}. {label}", index + 1),
                        "",
                    )]
                });
            let mut lines = vec![menu_title(title, Some(model))];
            lines.extend(layout.lines);
            lines
        }
        Surface::Profiles { kind, rows, list } => {
            let title = match kind {
                super::ProfileKind::Agent => "Agents",
                super::ProfileKind::Mode => "Modes",
            };
            let mut state = *list;
            state.reconcile(ListMode::Selectable, rows.len(), VISIBLE_MENU_ITEMS);
            let mut lines = vec![menu_title(title, None)];
            let layout = ListWidget::new(width, VISIBLE_MENU_ITEMS)
                .policy(LinePolicy::Truncate)
                .render(&state, |index, _, selected| {
                    let (name, description, selectable) = &rows[index];
                    let marker_style = if selected {
                        ACCENT_STYLE
                    } else {
                        Style::default()
                    };
                    let label_style = if !selectable {
                        DIM_STYLE
                    } else if selected {
                        SELECTED_STYLE
                    } else {
                        Style::default()
                    };
                    let mut spans = vec![
                        Span::styled(if selected { "› " } else { "  " }, marker_style),
                        Span::styled(terminal_safe(name), label_style),
                    ];
                    if !description.is_empty() {
                        spans.push(Span::styled(
                            format!("  {}", terminal_safe(description)),
                            DIM_STYLE,
                        ));
                    }
                    vec![Line::from(spans)]
                });
            lines.extend(layout.lines);
            lines
        }
        Surface::Skills { rows, list } => {
            let mut state = *list;
            state.reconcile(ListMode::Selectable, rows.len() + 2, VISIBLE_MENU_ITEMS);
            let mut lines = vec![menu_title("Skills", None)];
            lines.extend(
                ListWidget::new(width, VISIBLE_MENU_ITEMS)
                    .policy(LinePolicy::Truncate)
                    .render(&state, |index, _, selected| {
                        if index < 2 {
                            return vec![menu_choice_line(
                                selected,
                                if index == 0 { "Close" } else { "Add" },
                                "",
                            )];
                        }
                        let skill = &rows[index - 2];
                        let style = if !skill.enabled {
                            DIM_STYLE
                        } else if selected {
                            SELECTED_STYLE
                        } else {
                            Style::default()
                        };
                        let scope = skill.source_label();
                        vec![Line::from(vec![
                            Span::styled(
                                if selected { "› " } else { "  " },
                                if selected {
                                    ACCENT_STYLE
                                } else {
                                    Style::default()
                                },
                            ),
                            Span::styled(terminal_safe(&skill.name), style),
                            Span::styled(
                                format!("  {scope} · {}", terminal_safe(&skill.description)),
                                DIM_STYLE,
                            ),
                        ])]
                    })
                    .lines,
            );
            lines
        }
        Surface::SkillDeleteConfirm { skill, selected } => {
            let choices = ["Cancel", "Delete"];
            let state = ListState::selectable_at(choices.len(), *selected, VISIBLE_MENU_ITEMS);
            let layout = ListWidget::new(width, VISIBLE_MENU_ITEMS)
                .render(&state, |index, _, selected| {
                    vec![menu_choice_line(selected, choices[index], "")]
                });
            let mut lines = vec![menu_title("Delete skill", Some(&skill.name))];
            lines.extend(layout.lines);
            lines
        }
        Surface::SkillWizard {
            name,
            description,
            content,
            step,
            scope_list,
            ..
        } => {
            let detail = match step {
                super::SkillWizardStep::Scope => "scope",
                super::SkillWizardStep::Name => "name",
                super::SkillWizardStep::Description => "description",
                super::SkillWizardStep::Content => "content",
            };
            if *step == super::SkillWizardStep::Scope {
                let mut state = *scope_list;
                state.reconcile(ListMode::Selectable, 2, VISIBLE_MENU_ITEMS);
                let mut lines = vec![menu_title("Add skill", Some(detail))];
                lines.extend(
                    ListWidget::new(width, VISIBLE_MENU_ITEMS)
                        .render(&state, |index, _, selected| {
                            vec![menu_choice_line(
                                selected,
                                if index == 0 { "Project" } else { "Global" },
                                "",
                            )]
                        })
                        .lines,
                );
                lines
            } else if *step == super::SkillWizardStep::Content {
                let mut lines = vec![menu_title("Add skill", Some(detail))];
                lines.extend(
                    content
                        .render_scrolled(width, VISIBLE_MENU_ITEMS, None)
                        .lines,
                );
                lines
            } else {
                let (input, placeholder) = if *step == super::SkillWizardStep::Name {
                    (name, "skill-name")
                } else {
                    (description, "When should this skill be used?")
                };
                let mut spans = vec![Span::raw("  ")];
                spans.extend(
                    input
                        .render_bounded_with_placeholder(
                            usize::from(width.saturating_sub(2)),
                            placeholder,
                            Style::default(),
                            DIM_STYLE,
                            SEARCH_CURSOR_STYLE,
                        )
                        .spans,
                );
                vec![
                    menu_title("Add skill", Some(detail)),
                    Line::default(),
                    Line::from(spans),
                    Line::default(),
                ]
            }
        }
        Surface::AgentWizard {
            name,
            description,
            parent: _,
            parents,
            parent_list,
            prompt,
            availability,
            step,
            cursor,
            editing,
            original_name: _,
        } => {
            let title = format!(
                "{} agent {}",
                if *editing { "Edit" } else { "Add" },
                step.title()
            );
            let mut lines = vec![truncate_line(menu_title(&title, None), usize::from(width))];
            match step {
                AgentWizardStep::Name => {
                    lines.push(Line::default());
                    lines.push(indented_input(name, *cursor, "e.g. reviewer", width));
                    lines.push(Line::default());
                }
                AgentWizardStep::Description => {
                    lines.push(Line::default());
                    lines.push(indented_input(
                        description,
                        *cursor,
                        "what this agent is for",
                        width,
                    ));
                    lines.push(Line::default());
                }
                AgentWizardStep::Parent => {
                    let mut state = *parent_list;
                    state.reconcile(ListMode::Selectable, parents.len(), VISIBLE_MENU_ITEMS);
                    let policy = if *editing {
                        LinePolicy::Truncate
                    } else {
                        LinePolicy::Wrap
                    };
                    let layout = ListWidget::new(width, VISIBLE_MENU_ITEMS)
                        .policy(policy)
                        .render(&state, |index, _, selected| {
                            let entry = &parents[index];
                            vec![menu_choice_line(
                                selected,
                                entry,
                                if entry == "None" { "no parent" } else { "" },
                            )]
                        });
                    lines.extend(layout.lines);
                }
                AgentWizardStep::Prompt => {
                    lines.extend(
                        prompt
                            .render_scrolled(
                                width,
                                multiline_content_capacity(viewport_rows),
                                Some(&agent_prompt_placeholder()),
                            )
                            .lines,
                    );
                    lines.truncate(viewport_rows);
                }
                _ => {
                    let choices = [
                        ("User", "selectable directly"),
                        ("Subagent", "delegation only"),
                        ("Both", "either context"),
                    ];
                    let state =
                        ListState::selectable_at(choices.len(), *availability, VISIBLE_MENU_ITEMS);
                    let layout = ListWidget::new(width, VISIBLE_MENU_ITEMS).render(
                        &state,
                        |index, _, selected| {
                            let (label, detail) = choices[index];
                            vec![menu_choice_line(selected, label, detail)]
                        },
                    );
                    lines.extend(layout.lines);
                }
            }
            lines
        }
        Surface::AgentEdit { name, list } => {
            let choices = ["Name", "Description", "Parent", "Prompt", "Availability"];
            let mut state = *list;
            state.reconcile(ListMode::Selectable, choices.len(), VISIBLE_MENU_ITEMS);
            let layout = ListWidget::new(width, VISIBLE_MENU_ITEMS)
                .render(&state, |index, _, selected| {
                    vec![menu_choice_line(selected, choices[index], "")]
                });
            let mut lines = vec![truncate_line(
                menu_title("Edit agent", Some(name)),
                usize::from(width),
            )];
            lines.extend(layout.lines);
            lines
        }
        Surface::McpServers { rows, list } => {
            let mut state = *list;
            state.reconcile(ListMode::Selectable, rows.len() + 3, VISIBLE_MENU_ITEMS);
            let mut lines = vec![menu_title("MCP", None)];
            let layout = ListWidget::new(width, VISIBLE_MENU_ITEMS)
                .policy(LinePolicy::Truncate)
                .render(&state, |index, _, selected| {
                    if index < 3 {
                        let (label, detail) = match index {
                            0 => ("Close", ""),
                            1 => ("Add", "configure a server"),
                            _ => ("Add from catalog", "built-in MCPs"),
                        };
                        return vec![menu_choice_line(selected, label, detail)];
                    }
                    let server = &rows[index - 3];
                    let assignments = if server.agents.is_empty() {
                        "All".into()
                    } else {
                        server.agents.join(", ")
                    };
                    let style = if selected {
                        SELECTED_STYLE
                    } else {
                        Style::default()
                    };
                    let mut spans = vec![
                        Span::styled(if selected { "› " } else { "  " }, style),
                        Span::styled(server.name.clone(), style),
                        Span::styled(
                            format!("  {:?} · {assignments} · ", server.location.scope),
                            DIM_STYLE,
                        ),
                        Span::styled(
                            server.status.to_string(),
                            mcp_runtime_status_style(&server.status),
                        ),
                    ];
                    if !server.overridden.is_empty() {
                        spans.push(Span::styled(
                            format!(" · overrides {}", server.overridden.len()),
                            DIM_STYLE,
                        ));
                    }
                    vec![Line::from(spans)]
                });
            lines.extend(layout.lines);
            lines
        }
        Surface::McpCatalog { rows, list } => {
            let mut state = *list;
            state.reconcile(ListMode::Selectable, rows.len() + 1, VISIBLE_MENU_ITEMS);
            let layout =
                ListWidget::new(width, VISIBLE_MENU_ITEMS).render(&state, |index, _, selected| {
                    if index == 0 {
                        vec![menu_choice_line(selected, "Close", "")]
                    } else {
                        let package = &rows[index - 1];
                        vec![menu_choice_line(
                            selected,
                            &package.name,
                            format!("{} · {}", package.version, package.description),
                        )]
                    }
                });
            let mut lines = vec![menu_title("Add MCP from catalog", None)];
            lines.extend(layout.lines);
            lines
        }
        Surface::McpServer {
            server,
            selected,
            oauth_connected,
        } => {
            let toggle = if server.definition.enabled {
                "Disable"
            } else {
                "Enable"
            };
            let mut choices = vec![
                ("Close".to_owned(), String::new()),
                (toggle.into(), server.status.to_string()),
                ("Edit".into(), "structured fields".into()),
                ("Edit JSON".into(), "portable definition".into()),
                ("View tools".into(), "discovered tool schemas".into()),
            ];
            if matches!(
                &server.definition.transport,
                cagent_agent::mcp::McpTransportConfig::StreamableHttp { .. }
            ) {
                choices.push(if *oauth_connected {
                    ("Log out OAuth".into(), "remove managed credentials".into())
                } else {
                    ("Login with OAuth".into(), "opens authentication".into())
                });
            }
            if server.definition.package.is_some() {
                choices.push(("Configure package".into(), "parameters and secrets".into()));
            }
            choices.push(("Remove".into(), "delete this scope".into()));
            let state = ListState::selectable_at(choices.len(), *selected, VISIBLE_MENU_ITEMS);
            let layout =
                ListWidget::new(width, VISIBLE_MENU_ITEMS).render(&state, |index, _, selected| {
                    let (label, detail) = &choices[index];
                    if index == 1 {
                        vec![menu_choice_line_with_mcp_status(
                            selected,
                            label,
                            &server.status,
                        )]
                    } else {
                        vec![menu_choice_line(selected, label, detail)]
                    }
                });
            let mut lines = vec![menu_title("MCP server", Some(&server.name))];
            lines.extend(layout.lines);
            lines
        }
        Surface::McpOAuth { label, attempt, .. } => {
            let mut lines = vec![menu_title("Configure MCP", Some(label)), Line::default()];
            if matches!(
                &*attempt.completion.borrow(),
                cagent_agent::mcp::McpOAuthCompletion::Connected
            ) {
                lines.push(
                    Line::from(format!("  {} connected.", terminal_safe(label)))
                        .style(ENABLED_STYLE),
                );
                lines.push(Line::default());
                return lines;
            }
            match &attempt.prompt {
                cagent_agent::mcp::McpOAuthPrompt::Device {
                    verification_url,
                    user_code,
                } => lines.push(Line::from(vec![
                    Span::raw("  Open "),
                    Span::styled(
                        terminal_safe(verification_url),
                        ACCENT_STYLE.add_modifier(Modifier::UNDERLINED),
                    ),
                    Span::raw(" and enter "),
                    Span::styled(terminal_safe(user_code), ACCENT_STYLE),
                    Span::raw("."),
                ])),
                cagent_agent::mcp::McpOAuthPrompt::Browser {
                    authorization_url, ..
                } => lines.push(Line::from(vec![
                    Span::raw("  Complete authentication at "),
                    Span::styled(
                        terminal_safe(authorization_url),
                        ACCENT_STYLE.add_modifier(Modifier::UNDERLINED),
                    ),
                    Span::raw("."),
                ])),
            }
            lines.push(match (&attempt.prompt, &*attempt.completion.borrow()) {
                (
                    cagent_agent::mcp::McpOAuthPrompt::Device { .. },
                    cagent_agent::mcp::McpOAuthCompletion::Waiting,
                ) => {
                    Line::from("  This will update automatically a few moments after you sign in.")
                        .style(DIM_STYLE)
                }
                (_, cagent_agent::mcp::McpOAuthCompletion::Waiting) => {
                    Line::from("  Waiting for authentication…").style(DIM_STYLE)
                }
                (_, cagent_agent::mcp::McpOAuthCompletion::Connected) => unreachable!(),
                (_, cagent_agent::mcp::McpOAuthCompletion::Failed(error)) => Line::from(format!(
                    "  Authentication failed · {}",
                    terminal_safe(error)
                ))
                .style(ERROR_STYLE),
            });
            lines.push(Line::default());
            lines
        }
        Surface::McpPackageSetup { draft, selected } => {
            let count = 1 + draft.package.parameters.len() + draft.package.secrets.len();
            let state = ListState::selectable_at(count, *selected, VISIBLE_MENU_ITEMS);
            let layout =
                ListWidget::new(width, VISIBLE_MENU_ITEMS).render(&state, |index, _, selected| {
                    if index == 0 {
                        return vec![menu_choice_line(
                            selected,
                            if draft.installed {
                                "Save changes"
                            } else {
                                "Install"
                            },
                            "apply parameters and managed secrets",
                        )];
                    }
                    let field_index = index - 1;
                    if let Some((id, secret)) = draft.package.secrets.iter().nth(field_index) {
                        let status = match draft.secret_updates.get(id) {
                            Some(Some(value)) if !value.is_empty() => "Configured",
                            Some(_) => "Not configured",
                            None if draft.secret_statuses.get(id)
                                == Some(&cagent_agent::mcp::McpSecretStatus::Configured) =>
                            {
                                "Configured"
                            }
                            None => "Not configured",
                        };
                        let detail = secret
                            .description
                            .as_deref()
                            .map_or(status.into(), |description| {
                                format!("{status} · {description}")
                            });
                        return vec![menu_choice_line(selected, &secret.label, &detail)];
                    }
                    let parameter_index = field_index - draft.package.secrets.len();
                    let (id, parameter) = draft
                        .package
                        .parameters
                        .iter()
                        .nth(parameter_index)
                        .unwrap();
                    let value = draft
                        .parameters
                        .get(id)
                        .map(ToString::to_string)
                        .unwrap_or_else(|| "Not set".into());
                    let detail = parameter
                        .description
                        .as_deref()
                        .map_or(value.clone(), |description| {
                            format!("{value} · {description}")
                        });
                    vec![menu_choice_line(selected, &parameter.label, &detail)]
                });
            let mut lines = vec![menu_title(
                if draft.installed {
                    "Configure MCP package"
                } else {
                    "Install MCP package"
                },
                Some(&draft.package.name),
            )];
            lines.extend(layout.lines);
            lines
        }
        Surface::McpPackageValueEdit {
            draft,
            field,
            value,
            cursor,
        } => {
            let (label, description, secret) = match field {
                McpPackageField::Parameter(id) => (
                    draft
                        .package
                        .parameters
                        .get(id)
                        .map_or(id.as_str(), |parameter| parameter.label.as_str()),
                    draft
                        .package
                        .parameters
                        .get(id)
                        .and_then(|parameter| parameter.description.as_deref()),
                    false,
                ),
                McpPackageField::Secret(id) => (
                    draft
                        .package
                        .secrets
                        .get(id)
                        .map_or(id.as_str(), |secret| secret.label.as_str()),
                    draft
                        .package
                        .secrets
                        .get(id)
                        .and_then(|secret| secret.description.as_deref()),
                    true,
                ),
            };
            vec![
                menu_title("MCP package value", Some(label)),
                Line::default(),
                if secret {
                    masked_input_line("  ".into(), value, *cursor, width)
                } else {
                    Line::from({
                        let mut spans = vec![Span::raw("  ")];
                        spans.extend(
                            SingleLineInput::new(value.clone(), *cursor)
                                .render_bounded(
                                    usize::from(width.saturating_sub(2)),
                                    Style::default(),
                                    SEARCH_CURSOR_STYLE,
                                )
                                .spans,
                        );
                        spans
                    })
                },
                package_description_line(description),
                Line::default(),
            ]
        }
        Surface::McpTools {
            server,
            tools,
            list,
            loading,
            failure,
        } => {
            let mut lines = vec![menu_title("MCP tools", Some(server))];
            let mut state = *list;
            state.reconcile(ListMode::Selectable, tools.len(), VISIBLE_MENU_ITEMS);
            let empty_line = if *loading {
                Line::from("  Loading tools…").style(DIM_STYLE)
            } else if let Some(failure) = failure {
                Line::from(format!("  {failure}")).style(DIM_STYLE)
            } else {
                Line::from("  No discovered tools.").style(DIM_STYLE)
            };
            let layout = ListWidget::new(width, VISIBLE_MENU_ITEMS)
                .empty_lines(vec![empty_line])
                .policy(LinePolicy::Truncate)
                .render(&state, |index, _, selected| {
                    let tool = &tools[index];
                    let mut spans = vec![Span::raw(format!(
                        "{} {}",
                        if selected { "›" } else { " " },
                        tool.name,
                    ))];
                    if !tool.description.is_empty() {
                        spans.push(Span::styled(format!("  {}", tool.description), DIM_STYLE));
                    }
                    if tool.configured_read_only {
                        spans.push(Span::styled(" · read-only", DIM_STYLE));
                    }
                    vec![Line::from(spans)]
                });
            lines.extend(layout.lines);
            lines
        }
        Surface::McpRemoveConfirm { server, selected } => {
            let revealed = server.overridden.first().map_or_else(
                || "no broader definition will be revealed".into(),
                |location| format!("reveals {:?} definition", location.scope),
            );
            let choices = [
                ("Cancel".to_owned(), String::new()),
                ("Remove".into(), revealed),
            ];
            let state = ListState::selectable_at(choices.len(), *selected, VISIBLE_MENU_ITEMS);
            let layout =
                ListWidget::new(width, VISIBLE_MENU_ITEMS).render(&state, |index, _, selected| {
                    let (label, detail) = &choices[index];
                    vec![menu_choice_line(selected, label, detail)]
                });
            let mut lines = vec![menu_title("Remove MCP server", Some(&server.name))];
            lines.extend(layout.lines);
            lines
        }
        Surface::McpAddScope { selected } => {
            let choices = ["Close", "Global", "Project"];
            let state = ListState::selectable_at(choices.len(), *selected, VISIBLE_MENU_ITEMS);
            let layout = ListWidget::new(width, VISIBLE_MENU_ITEMS)
                .render(&state, |index, _, selected| {
                    vec![menu_choice_line(selected, choices[index], "")]
                });
            let mut lines = vec![menu_title("Add MCP", Some("scope"))];
            lines.extend(layout.lines);
            lines
        }
        Surface::McpTransport { selected, .. } => {
            let choices = ["Close", "Stdio", "Streamable HTTP", "Import JSON"];
            let state = ListState::selectable_at(choices.len(), *selected, VISIBLE_MENU_ITEMS);
            let layout = ListWidget::new(width, VISIBLE_MENU_ITEMS)
                .render(&state, |index, _, selected| {
                    vec![menu_choice_line(selected, choices[index], "")]
                });
            let mut lines = vec![menu_title("Add MCP", Some("transport"))];
            lines.extend(layout.lines);
            lines
        }
        Surface::McpForm { draft, list } => {
            let selected = list.selected.unwrap_or(0);
            let choices = mcp_form_choices(draft, selected, width);
            let mut state = *list;
            state.reconcile(ListMode::Selectable, choices.len(), VISIBLE_MENU_ITEMS);
            let layout =
                ListWidget::new(width, VISIBLE_MENU_ITEMS).render(&state, |index, _, selected| {
                    let (_, label, detail) = &choices[index];
                    vec![menu_choice_line(selected, label, detail)]
                });
            let mut lines = vec![menu_title(
                if draft.original.is_some() {
                    "Edit MCP server"
                } else {
                    "Add MCP server"
                },
                Some(&draft.name),
            )];
            lines.extend(layout.lines);
            lines
        }
        Surface::McpFieldEdit {
            field,
            value,
            cursor,
            ..
        } => vec![
            menu_title("MCP field", Some(mcp_field_label(*field))),
            Line::default(),
            Line::from({
                let mut spans = vec![Span::raw("  ")];
                let mut input = SingleLineInput::new(value.clone(), *cursor);
                if mcp_field_uses_json(*field) {
                    input = input.with_syntax_language("json");
                }
                spans.extend(
                    input
                        .render_bounded(
                            usize::from(width.saturating_sub(2)),
                            Style::default(),
                            SEARCH_CURSOR_STYLE,
                        )
                        .spans,
                );
                spans
            }),
            Line::default(),
        ],
        Surface::McpJsonEdit { editor, .. } => {
            let mut lines = vec![menu_title("MCP JSON", None)];
            lines.extend(
                editor
                    .render_scrolled(width, multiline_content_capacity(viewport_rows), None)
                    .lines,
            );
            lines.truncate(viewport_rows);
            lines
        }
        Surface::McpMutationPreview {
            preview,
            list,
            details,
        } => {
            let choices = [
                ("Cancel".to_owned(), String::new()),
                (
                    "Apply".into(),
                    if preview.requires_confirmation {
                        "confirms replacements".into()
                    } else {
                        "commit atomically".into()
                    },
                ),
            ];
            let mut state = *list;
            state.reconcile(ListMode::Selectable, choices.len(), VISIBLE_MENU_ITEMS);
            let layout =
                ListWidget::new(width, VISIBLE_MENU_ITEMS).render(&state, |index, _, selected| {
                    let (label, detail) = &choices[index];
                    vec![menu_choice_line(selected, label, detail)]
                });
            let mut lines = vec![menu_title("MCP change preview", None)];
            lines.extend(layout.lines);
            let detail_lines = mcp_mutation_detail_lines(preview);
            let capacity = viewport_rows
                .saturating_sub(lines.len().saturating_add(2))
                .min(VISIBLE_HELP_ITEMS);
            let mut details = *details;
            details.reconcile(detail_lines.len(), capacity, false);
            lines.extend(
                ScrollViewWidget::new(capacity)
                    .render(&details, |row| detail_lines[row].clone())
                    .lines,
            );
            lines
        }
        Surface::StatusLine {
            config,
            rows,
            list,
            mode,
            preview,
        } => {
            let mut lines = vec![menu_title("Statusline", None), Line::default()];
            let preview_values = preview;
            let preview = status_line_content(
                config,
                preview_values,
                usize::from(width.saturating_sub(13)),
            );
            let mut preview_spans = vec![Span::raw("  "), Span::styled("Preview  ", DIM_STYLE)];
            preview_spans.extend(preview.spans);
            lines.push(Line::from(preview_spans));
            match mode {
                StatusLineEditorMode::Modules => {
                    let mut state = *list;
                    state.reconcile(ListMode::Selectable, rows.len(), VISIBLE_MENU_ITEMS);
                    let layout = ListWidget::new(width, VISIBLE_MENU_ITEMS)
                        .policy(LinePolicy::Truncate)
                        .render(&state, |index, _, selected| {
                            let module = &rows[index];
                            let enabled = config.modules.contains(module);
                            let color = config.color(*module);
                            let description = module.description();
                            let pointer = if selected { "›" } else { " " };
                            let module_style = Style::new().fg(status_line_color(color));
                            vec![Line::from(vec![
                                Span::styled(format!("{pointer} "), SELECTED_STYLE),
                                Span::styled(if enabled { "●" } else { "○" }, module_style),
                                Span::raw(" "),
                                Span::styled(module.to_string(), module_style),
                                Span::raw("  "),
                                Span::styled(description, DIM_STYLE),
                            ])]
                        });
                    lines.extend(layout.lines);
                }
                StatusLineEditorMode::Colors { module, list } => {
                    let choices = status_line_color_choices();
                    let mut state = *list;
                    state.reconcile(ListMode::Selectable, choices.len(), VISIBLE_MENU_ITEMS);
                    let layout = ListWidget::new(width, VISIBLE_MENU_ITEMS).render(
                        &state,
                        |index, _, selected| {
                            let choice = &choices[index];
                            let (label, color) = match choice {
                                StatusLineColorChoice::Default => (
                                    format!("Default ({})", module.default_color()),
                                    Some(module.default_color()),
                                ),
                                StatusLineColorChoice::Named(color) => {
                                    (color.to_string(), Some(*color))
                                }
                                StatusLineColorChoice::Custom => ("Custom hex…".into(), None),
                            };
                            let pointer = if selected { "›" } else { " " };
                            let style = color.map_or(Style::new(), |color| {
                                Style::new().fg(status_line_color(color))
                            });
                            vec![Line::from(vec![
                                Span::styled(format!("{pointer} "), SELECTED_STYLE),
                                Span::styled("● ", style),
                                Span::styled(label, style),
                            ])]
                        },
                    );
                    lines.extend(layout.lines);
                }
                StatusLineEditorMode::Hex {
                    module,
                    input,
                    cursor,
                } => {
                    lines.push(Line::from(vec![
                        Span::raw("  Custom color for "),
                        Span::styled(module.to_string(), ACCENT_STYLE),
                        Span::styled(" (#RRGGBB)", DIM_STYLE),
                    ]));
                    lines.push(Line::default());
                    let mut spans = vec![Span::raw("  ")];
                    spans.extend(
                        SingleLineInput::new(input.clone(), *cursor)
                            .render_bounded(
                                usize::from(width.saturating_sub(2)),
                                Style::default(),
                                SEARCH_CURSOR_STYLE,
                            )
                            .spans,
                    );
                    lines.push(Line::from(spans));
                    lines.push(Line::default());
                }
            }
            lines
        }
        Surface::Settings {
            rows,
            section,
            list,
            query,
            query_cursor,
        } => {
            let visible = filter_settings_rows(rows, *section, query);
            let capacity = settings_item_capacity(query);
            // Settings use two visual lines per item. Filtering reclaims the
            // two tab rows for one additional selectable item.
            let mut state = *list;
            state.reconcile(ListMode::Selectable, visible.len(), capacity);
            let mut lines = vec![search_menu_title(
                "Settings",
                "search: ",
                &SingleLineInput::new(query.clone(), *query_cursor),
                width,
            )];
            if query.is_empty() {
                lines.push(Line::default());
                lines.push(settings_tabs(*section).line);
            }
            let no_matches = !query.is_empty() && visible.is_empty();
            let mut widget = ListWidget::new(width, capacity).policy(LinePolicy::Truncate);
            if no_matches {
                widget = widget.empty_lines(vec![no_filter_matches("settings"), Line::default()]);
            }
            if !query.is_empty() {
                let occupied_rows = if visible.is_empty() {
                    2
                } else {
                    state.visible_range(capacity).len() * 2
                };
                widget = widget.spacing(0, capacity * 2 - occupied_rows);
            }
            let layout = widget.render(&state, |index, _, is_selected| {
                let row = visible[index];
                let marker = if is_selected { "●" } else { "○" };
                let title = Line::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        format!("{marker} {}", row.definition.label),
                        if is_selected {
                            SELECTED_STYLE
                        } else {
                            Style::default()
                        },
                    ),
                    Span::raw("  "),
                    Span::styled(row.display_value.clone(), ACCENT_STYLE),
                ]);
                let mut metadata = vec![Span::raw("    ")];
                if row.explicit_value.is_some() {
                    metadata.push(Span::styled("overridden", NOTICE_STYLE));
                    metadata.push(Span::styled(" · ", DIM_STYLE));
                }
                metadata.push(Span::styled(
                    terminal_safe(&row.definition.description),
                    DIM_STYLE,
                ));
                vec![title, Line::from(metadata)]
            });
            lines.extend(layout.lines);
            lines
        }
        Surface::SettingChoices {
            setting,
            choices,
            list,
            ..
        } => {
            let mut state = *list;
            state.reconcile(ListMode::Selectable, choices.len(), VISIBLE_MENU_ITEMS);
            let widget = ListWidget::new(width, VISIBLE_MENU_ITEMS).policy(LinePolicy::Truncate);
            let layout = widget.render(&state, |index, _, selected| {
                let (value, description) = &choices[index];
                vec![menu_choice_line(selected, value, description)]
            });
            let mut lines = vec![
                menu_title(&setting.label, Some(&setting.key)),
                setting_description_line(&setting.description, width),
            ];
            lines.extend(layout.lines);
            lines
        }
        Surface::SettingInput {
            setting,
            value,
            cursor,
        } => {
            let mut lines = vec![
                menu_title(&setting.label, Some(&setting.key)),
                truncate_line(
                    Line::from(vec![
                        Span::raw("  "),
                        Span::styled(
                            terminal_safe(&setting_input_description(setting)),
                            DIM_STYLE,
                        ),
                    ]),
                    usize::from(width),
                ),
                Line::default(),
            ];
            lines.push(indented_syntax_input(
                value,
                *cursor,
                match &setting.kind {
                    cagent_agent::config::SettingKind::KeyBindings => "e.g. [\"ctrl+v\"]",
                    cagent_agent::config::SettingKind::TomlArray => {
                        "e.g. [\"provider/model\", \"provider/model\"]"
                    }
                    cagent_agent::config::SettingKind::TomlChoices(_) => {
                        match setting.key.as_str() {
                            "ui.title.requested_input" => "e.g. [\"(!)\", \"(.)\"]",
                            "ui.title.progress" => "e.g. [\".\", \"..\", \"...\"]",
                            "ui.editor" => "e.g. { command = [\"nvim\"], mode = \"foreground\" }",
                            _ => "e.g. [\"one\", \"two\"]",
                        }
                    }
                    _ => "enter a value",
                },
                setting_syntax_language(setting),
                width,
            ));
            lines.push(Line::default());
            lines
        }
        Surface::SupervisedWork {
            rows,
            list,
            show_past,
        } => {
            let mut lines = vec![menu_title(
                "Background work",
                Some(if *show_past { "past" } else { "active" }),
            )];
            let visible = rows
                .iter()
                .filter(|item| item.active() != *show_past)
                .collect::<Vec<_>>();
            let mut state = *list;
            state.reconcile(ListMode::Selectable, visible.len(), VISIBLE_MENU_ITEMS);
            let empty = Line::from(Span::styled(
                if *show_past {
                    "  No past background work."
                } else {
                    "  No active background work."
                },
                DIM_STYLE,
            ));
            let layout = ListWidget::new(width, VISIBLE_MENU_ITEMS)
                .empty_lines(vec![empty])
                .policy(LinePolicy::Truncate)
                .render(&state, |index, width, selected| {
                    let choice = match visible[index] {
                        cagent_agent::presentation::SupervisedWork::Agent { run } => (
                            selected,
                            format!("{} · {:?}", run.profile, run.status).to_ascii_lowercase(),
                            run.task.clone(),
                        ),
                        cagent_agent::presentation::SupervisedWork::Terminal { terminal } => (
                            selected,
                            format!("terminal · {:?}", terminal.status).to_ascii_lowercase(),
                            terminal.command.clone(),
                        ),
                    };
                    vec![single_line_menu_choice(choice.0, choice.1, choice.2, width)]
                });
            lines.extend(layout.lines);
            lines
        }
        Surface::KillSupervisedWork { target, selected } => {
            let subject = match target {
                cagent_agent::runtime::SupervisedWorkTarget::Agent(_) => "sub-agent",
                cagent_agent::runtime::SupervisedWorkTarget::Terminal(_) => "terminal",
            };
            let choices = [("Kill", "gracefully terminate active work"), ("Close", "")];
            let state = ListState::selectable_at(choices.len(), *selected, VISIBLE_MENU_ITEMS);
            let layout =
                ListWidget::new(width, VISIBLE_MENU_ITEMS).render(&state, |index, _, selected| {
                    let (label, detail) = choices[index];
                    vec![menu_choice_line(selected, label, detail)]
                });
            let mut lines = vec![menu_title("Kill background work", Some(subject))];
            lines.extend(layout.lines);
            lines
        }
        Surface::Expanded {
            view,
            scroll,
            viewport_rows: output_viewport_rows,
        } => expanded_surface_lines(
            view,
            *scroll,
            *output_viewport_rows,
            workspace,
            width,
            viewport_rows,
        ),
        Surface::Paths { rows, list, .. } => {
            let mut lines = vec![menu_title("Path", None), Line::default()];
            let mut state = *list;
            state.reconcile(ListMode::Selectable, rows.len(), VISIBLE_MENU_ITEMS);
            let layout =
                ListWidget::new(width, VISIBLE_MENU_ITEMS).render(&state, |index, _, selected| {
                    let entry = &rows[index];
                    vec![menu_choice_line(
                        selected,
                        format!("{}. {}", index + 1, entry.path.to_string_lossy()),
                        list_entry_kind(entry.kind),
                    )]
                });
            lines.extend(layout.lines);
            lines
        }
        Surface::HistoryTree {
            rows,
            list,
            purpose,
            loading,
            query,
            query_cursor,
            opened_at_millis,
            ..
        } => history_tree_picker_lines(
            rows,
            list,
            *purpose,
            *loading,
            query,
            *query_cursor,
            *opened_at_millis,
            width,
            viewport_rows,
        ),
        Surface::Conversations {
            rows,
            list,
            query,
            query_cursor,
            opened_at_millis,
            include_archived,
        } => conversation_picker_lines(
            rows,
            list,
            query,
            *query_cursor,
            *opened_at_millis,
            *include_archived,
            width,
            viewport_rows,
        ),
        Surface::Help {
            tab,
            command_rows,
            key_rows,
            list,
        } => {
            let mut lines = vec![
                menu_title("Help", None),
                Line::default(),
                help_tabs(*tab).line,
            ];
            let rows = match tab {
                HelpTab::Commands => command_rows,
                HelpTab::Keybindings => key_rows,
            };
            let mut state = *list;
            state.reconcile(rows.len(), VISIBLE_HELP_ITEMS, false);
            lines.extend(scrollable_help_list(rows, width, &state));
            lines
        }
    }
}

fn permission_menu_lines(
    rows: &[PermissionMenuRow],
    state: &ListState,
    width: u16,
) -> Vec<Line<'static>> {
    let mut lines = vec![menu_title("Permissions", None)];
    let mut state = *state;
    state.reconcile(ListMode::Selectable, rows.len() + 1, VISIBLE_MENU_ITEMS);
    let layout = ListWidget::new(width, VISIBLE_MENU_ITEMS).render(&state, |index, _, selected| {
        let style = if selected {
            SELECTED_STYLE
        } else {
            Style::default()
        };
        if index == 0 {
            return vec![Line::from(vec![
                Span::styled(if selected { "› " } else { "  " }, style),
                Span::styled("Close", style),
            ])];
        }
        let row = &rows[index - 1];
        let scope = match row.scope {
            cagent_agent::permissions::PermissionScope::Conversation => "conversation",
            cagent_agent::permissions::PermissionScope::ConversationGlobal => "global",
            cagent_agent::permissions::PermissionScope::Project => "project",
            cagent_agent::permissions::PermissionScope::Global => "global",
        };
        let effect = match row.rule.effect {
            cagent_agent::permissions::PermissionEffect::Allow => "allow",
            cagent_agent::permissions::PermissionEffect::Ask => "ask",
            cagent_agent::permissions::PermissionEffect::Deny => "deny",
        };
        let resource = if row.rule.external {
            match row.rule.access.as_deref() {
                Some("read") => "external read",
                Some("write") => "external write",
                Some("execute") => "external execute",
                _ => "external access",
            }
        } else {
            row.rule
                .tool
                .as_deref()
                .or(row.rule.server.as_deref())
                .unwrap_or("all")
        };
        let matcher = row
            .rule
            .editable_pattern()
            .map(|(pattern, _)| pattern)
            .or_else(|| row.rule.operation.clone())
            .unwrap_or_else(|| "all resources".into());
        vec![Line::from(vec![
            Span::styled(if selected { "› " } else { "  " }, style),
            Span::styled(format!("{scope} · {effect} · {resource}"), style),
            Span::styled(format!("  {matcher}"), DIM_STYLE),
        ])]
    });
    lines.extend(layout.lines);
    if rows.is_empty() {
        lines.push(Line::from("  No persistent permissions set.").style(DIM_STYLE));
        lines.push(Line::default());
    }
    lines
}

fn usage_summary(usage: &cagent_agent::protocol::SessionUsage) -> String {
    let compact = |value: Option<u64>| {
        value
            .map(cagent_agent::presentation::format_compact_tokens)
            .unwrap_or_else(|| "—".into())
    };
    let mut value = format!(
        "{} calls · in {} · cache r:{} w:{} · out {}",
        usage.calls(),
        compact(usage.non_cached_input_tokens.or(usage.input_tokens)),
        compact(usage.cache_read_input_tokens),
        compact(usage.cache_write_input_tokens),
        compact(usage.output_tokens),
    );
    if let Some(cost) = usage
        .total_cost()
        .and_then(|cost| cost.total_cost.as_deref())
    {
        let currency = usage.total_cost().map_or("", |cost| cost.currency.as_str());
        let amount = cagent_agent::presentation::format_currency(
            cost,
            cagent_agent::presentation::CurrencyFormat::Short,
        );
        value.push_str(&if currency == "USD" {
            format!(" · ${amount}")
        } else {
            format!(" · {amount} {currency}")
        });
    }
    value
}

fn usage_menu_lines(
    overview: &cagent_agent::UsageOverview,
    list: &ListState,
    width: u16,
) -> Vec<Line<'static>> {
    let mut lines = vec![menu_title("Usage", None), Line::default()];
    for (label, usage) in [
        ("Current conversation", &overview.current_conversation),
        ("Last 24 hours", &overview.rolling_24_hours),
        ("Last 7 days", &overview.rolling_7_days),
        ("Last 30 days", &overview.rolling_30_days),
        ("Total", &overview.total),
    ] {
        lines.push(Line::from(vec![
            Span::styled(format!("  {label:<20}"), MENU_DETAIL_STYLE),
            Span::raw(format!("  {}", usage_summary(usage))),
        ]));
    }
    let choices = ["Close", "Reset", "View per project", "View per model"];
    lines.extend(
        ListWidget::new(width, VISIBLE_MENU_ITEMS)
            .render(list, |index, _, selected| {
                vec![menu_choice_line(selected, choices[index], "")]
            })
            .lines,
    );
    lines
}

fn usage_breakdown_lines(
    kind: UsageBreakdownKind,
    rows: &[cagent_agent::UsageBreakdown],
    list: &ListState,
    width: u16,
) -> Vec<Line<'static>> {
    let title = match kind {
        UsageBreakdownKind::Project => "Usage per project",
        UsageBreakdownKind::Model => "Usage per model",
    };
    let mut lines = vec![menu_title(title, Some("all tracked time"))];
    if rows.is_empty() {
        lines.push(Line::from("  No tracked usage.").style(DIM_STYLE));
        return lines;
    }
    lines.extend(
        ListWidget::new(width, VISIBLE_MENU_ITEMS)
            .render(list, |index, _, selected| {
                let row = &rows[index];
                let style = if selected {
                    SELECTED_STYLE
                } else {
                    Style::default()
                };
                vec![
                    Line::from(vec![
                        Span::styled(if selected { "› " } else { "  " }, style),
                        Span::styled(row.label.clone(), style),
                    ]),
                    Line::from(format!("    {}", usage_summary(&row.usage))).style(DIM_STYLE),
                ]
            })
            .lines,
    );
    lines
}

/// Shared title row for expanded views: title, subject, then muted metadata.
/// Explicit span styles (for example, Bash syntax or diff counts) are retained.
fn expanded_title(
    title: &str,
    subject: Option<Line<'static>>,
    details: impl IntoIterator<Item = Line<'static>>,
    width: u16,
) -> Line<'static> {
    let mut spans = vec![
        Span::raw("  "),
        Span::styled(title.to_owned(), MENU_TITLE_STYLE),
    ];
    let mut has_detail = false;
    for (line, style) in subject
        .into_iter()
        .map(|line| (line, Style::default()))
        .chain(details.into_iter().map(|line| (line, DIM_STYLE)))
    {
        if line.width() == 0 {
            continue;
        }
        spans.push(Span::styled(
            if has_detail { " · " } else { "  " },
            DIM_STYLE,
        ));
        spans.extend(line.spans.into_iter().map(|span| {
            let text = terminal_safe(&span.content).replace(['\n', '\r', '\t'], " ");
            let explicit_style = line.style.patch(span.style);
            Span::styled(
                text,
                if explicit_style == Style::default() {
                    style
                } else {
                    explicit_style
                },
            )
        }));
        has_detail = true;
    }
    truncate_line(Line::from(spans), usize::from(width))
}

fn expanded_status_detail(status: &str, elapsed: Option<&str>) -> Line<'static> {
    Line::from(match elapsed {
        Some(elapsed) => format!("{status} {elapsed}"),
        None => status.to_owned(),
    })
}

/// Calculates the content rows and last valid offset for a scrolling panel.
/// When the content overflows, one row is retained for the down-arrow.
fn expanded_surface_lines(
    view: &ExpandedView,
    scroll: usize,
    viewport_rows: usize,
    workspace: &Path,
    width: u16,
    panel_rows: usize,
) -> Vec<Line<'static>> {
    if let ExpandedView::Terminal {
        command,
        completion,
        started_at,
        completed_at,
        ..
    } = view
    {
        let elapsed = started_at.as_deref().and_then(|started_at| {
            cagent_agent::presentation::timestamp_elapsed(started_at, completed_at.as_deref())
        });
        return vec![
            expanded_title(
                "Bash output",
                Some(Line::from(bash_command_spans(command))),
                [expanded_status_detail(
                    expanded_terminal_status(completion),
                    elapsed.as_deref(),
                )],
                width,
            ),
            // Terminal scroll offsets are clamped whenever they are updated,
            // so the top indicator needs no VT100 replay to determine whether
            // there is content above the viewport.
            scroll_indicator(true, scroll > 0),
        ];
    }

    let content = expanded_text_content(view, workspace, width);
    expanded_text_surface_lines(view, scroll, viewport_rows, panel_rows, &content)
}

fn expanded_text_surface_lines(
    view: &ExpandedView,
    scroll: usize,
    viewport_rows: usize,
    panel_rows: usize,
    content: &ExpandedTextContent,
) -> Vec<Line<'static>> {
    let mut header = content.header.clone();
    let body = &content.body;
    let inset = content.inset;
    let (visible_rows, maximum) =
        expanded_text_scroll_metrics(header.len(), body.len(), viewport_rows, panel_rows);
    if let ExpandedView::AgentLog { max_scroll, .. } = view {
        max_scroll.set(Some(maximum));
    }
    // The trailing blank header row becomes the permanent upper indicator;
    // the widget owns the matching lower slot. Expanded arrows jump directly
    // to a boundary rather than moving one page.
    header.pop();
    let state = ScrollViewState {
        offset: scroll.min(maximum),
        content_rows: body.len(),
    };
    let visible = body.visible_rows(view, state.offset, visible_rows);
    header.extend(
        ScrollViewWidget::new(visible_rows)
            .indicators(ScrollViewAction::Home, ScrollViewAction::End)
            .render(&state, |row| {
                let line = visible
                    .get(row.saturating_sub(state.offset))
                    .map_or_else(Line::default, |row| row.line.clone());
                if inset {
                    inset_expanded_output_line(line)
                } else {
                    line
                }
            })
            .lines,
    );
    header
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum ExpandedTextCacheKey {
    Invalid,
    Mcp {
        call: usize,
        width: u16,
    },
    Compaction {
        summary: usize,
        summary_len: usize,
        width: u16,
    },
    RepositoryDiff {
        diff: usize,
        width: u16,
    },
    WebFetch {
        output: usize,
        output_len: usize,
        format: cagent_agent::WebFetchFormat,
        width: u16,
    },
    WebSearch {
        results: usize,
        results_len: usize,
        width: u16,
    },
    AgentLog {
        run: usize,
        streaming: usize,
        streaming_len: usize,
        terminals: usize,
        terminals_len: usize,
        width: u16,
    },
    File {
        view: usize,
        width: u16,
    },
    Diff {
        view: usize,
        width: u16,
    },
    Directory {
        tree: usize,
        revision: u64,
        selected: usize,
        width: u16,
    },
}

impl ExpandedTextCacheKey {
    fn new(view: &ExpandedView, width: u16) -> Option<Self> {
        match view {
            ExpandedView::Mcp { call } => Some(Self::Mcp {
                call: std::sync::Arc::as_ptr(call) as usize,
                width,
            }),
            ExpandedView::Compaction { summary } => Some(Self::Compaction {
                summary: summary.as_ptr() as usize,
                summary_len: summary.len(),
                width,
            }),
            ExpandedView::RepositoryDiff { diff, .. } => Some(Self::RepositoryDiff {
                diff: std::ptr::from_ref(diff) as usize,
                width,
            }),
            ExpandedView::WebFetch { format, output, .. } => Some(Self::WebFetch {
                output: output.as_ptr() as usize,
                output_len: output.len(),
                format: *format,
                width,
            }),
            ExpandedView::WebSearch { results, .. } => Some(Self::WebSearch {
                results: results.as_ptr() as usize,
                results_len: results.len(),
                width,
            }),
            ExpandedView::AgentLog {
                run,
                streaming,
                terminals,
                ..
            } => Some(Self::AgentLog {
                run: std::ptr::from_ref(run.as_ref()) as usize,
                streaming: streaming.as_ptr() as usize,
                streaming_len: streaming.len(),
                terminals: terminals.as_ptr() as usize,
                terminals_len: terminals.len(),
                width,
            }),
            ExpandedView::File { view } => Some(Self::File {
                view: std::ptr::from_ref(view) as usize,
                width,
            }),
            ExpandedView::Diff { view } => Some(Self::Diff {
                view: std::ptr::from_ref(view) as usize,
                width,
            }),
            ExpandedView::Image { png, .. } => Some(Self::File {
                view: png.as_ptr() as usize,
                width,
            }),
            ExpandedView::Directory { browser } => Some(Self::Directory {
                tree: std::ptr::from_ref(&browser.tree) as usize,
                revision: browser.tree.revision(),
                selected: browser.selected_index(),
                width,
            }),
            ExpandedView::Terminal { .. } => None,
        }
    }

    fn is_layout_for_file(&self, view: &ExpandedView) -> bool {
        matches!(
            (self, view),
            (Self::File { view: cached, .. }, ExpandedView::File { view })
                if *cached == std::ptr::from_ref(view) as usize
        )
    }
}

pub(crate) struct ExpandedTextRenderCache {
    key: ExpandedTextCacheKey,
    content: ExpandedTextContent,
    agent: AgentLogLayoutCache,
}

/// Keep immutable log layout across streaming/status refreshes. Structural
/// changes rebuild the prefix; expansion changes reuse projected entries and
/// rendered cards, and text updates only wrap the current assistant message.
#[derive(Default)]
struct AgentLogLayoutCache {
    identity: Option<(cagent_agent::protocol::AgentRunId, u16, usize, usize, usize)>,
    log: Option<cagent_agent::presentation::AgentRunLog>,
    rows: AgentLogRowsCache,
    expanded_explorations: std::collections::HashSet<usize>,
    expanded_activity_runs: std::collections::HashSet<usize>,
    collapse_tool_activity: bool,
    prefix: std::sync::Arc<Vec<crate::markdown::DisplayRow>>,
    prefix_role: Option<crate::render::transcript::TranscriptFlowRole>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum AgentLogRowKey {
    Assistant(usize),
    Tool(usize, bool),
}

type AgentLogRowsCache =
    std::collections::HashMap<AgentLogRowKey, std::sync::Arc<Vec<crate::markdown::DisplayRow>>>;

pub(crate) fn invalidate_expanded_text(cache: &RefCell<Option<ExpandedTextRenderCache>>) {
    if let Some(cache) = cache.borrow_mut().as_mut() {
        cache.key = ExpandedTextCacheKey::Invalid;
    }
}

fn rebuild_expanded_text_cache(
    cache: &mut Option<ExpandedTextRenderCache>,
    key: ExpandedTextCacheKey,
    view: &ExpandedView,
    workspace: &Path,
    width: u16,
) {
    let mut agent = cache.take().map(|cache| cache.agent).unwrap_or_default();
    let content = if let ExpandedView::AgentLog {
        run,
        streaming,
        terminals,
        expanded_explorations,
        expanded_activity_runs,
        collapse_tool_activity,
        ..
    } = view
    {
        // Timeline entries are immutable and append-only within one open log.
        let identity = (
            run.id,
            width,
            run.timeline.len(),
            run.activity.len(),
            terminals.as_ptr() as usize,
        );
        let source_changed = agent.identity != Some(identity);
        if source_changed {
            agent.log = Some(
                cagent_agent::presentation::project_agent_run_log_with_terminals(
                    run, workspace, terminals,
                ),
            );
            agent.rows.clear();
            agent.identity = Some(identity);
        }
        if source_changed
            || agent.expanded_explorations != *expanded_explorations
            || agent.expanded_activity_runs != *expanded_activity_runs
            || agent.collapse_tool_activity != *collapse_tool_activity
        {
            let (content, role) = agent_log_content_inner(
                run,
                "",
                agent.log.as_ref().expect("agent log was projected"),
                &mut agent.rows,
                expanded_explorations,
                expanded_activity_runs,
                *collapse_tool_activity,
                workspace,
                width,
                false,
            );
            let ExpandedTextBody::Materialized(prefix) = content.body else {
                unreachable!()
            };
            agent.prefix_role = role;
            agent.prefix = std::sync::Arc::new(prefix);
            agent
                .expanded_explorations
                .clone_from(expanded_explorations);
            agent
                .expanded_activity_runs
                .clone_from(expanded_activity_runs);
            agent.collapse_tool_activity = *collapse_tool_activity;
        }
        let mut prefix_len = agent.prefix.len();
        let mut tail = Vec::new();
        if run.error.is_none() && !streaming.is_empty() {
            if agent.prefix_role.is_some() {
                while prefix_len > 0
                    && agent.prefix[prefix_len - 1]
                        .line
                        .to_string()
                        .trim()
                        .is_empty()
                {
                    prefix_len -= 1;
                }
                crate::render::transcript::append_transcript_boundary(
                    &mut tail,
                    agent.prefix_role,
                    crate::render::transcript::TranscriptFlowRole::Assistant,
                    width,
                );
            }
            let document = cagent_agent::presentation::parse_markdown(streaming);
            tail.extend(crate::render::transcript::transcript_entry_rows_with_state(
                crate::render::transcript::TranscriptEntry::Assistant(&document),
                workspace,
                width,
                true,
                |_, _| None,
            ));
        }
        append_agent_log_tail(
            &mut tail,
            run.error.as_deref(),
            agent.prefix_role.is_none(),
            streaming.is_empty(),
            width,
        );
        ExpandedTextContent {
            header: agent_log_header(run, width),
            body: ExpandedTextBody::AgentLog {
                prefix: agent.prefix.clone(),
                prefix_len,
                tail,
            },
            inset: false,
        }
    } else {
        expanded_text_content(view, workspace, width)
    };
    *cache = Some(ExpandedTextRenderCache {
        key,
        content,
        agent,
    });
}

pub(crate) fn cached_expanded_surface_lines(
    cache: &RefCell<Option<ExpandedTextRenderCache>>,
    view: &ExpandedView,
    scroll: usize,
    viewport_rows: usize,
    workspace: &Path,
    width: u16,
    panel_rows: usize,
    preserve_file_layout: bool,
) -> Vec<Line<'static>> {
    let Some(key) = ExpandedTextCacheKey::new(view, width) else {
        return expanded_surface_lines(view, scroll, viewport_rows, workspace, width, panel_rows);
    };
    let mut cache = cache.borrow_mut();
    let reuse_file_layout = preserve_file_layout
        && cache
            .as_ref()
            .is_some_and(|cached| cached.key.is_layout_for_file(view));
    if !reuse_file_layout && cache.as_ref().is_none_or(|cached| cached.key != key) {
        rebuild_expanded_text_cache(&mut cache, key, view, workspace, width);
    }
    let content = &mut cache
        .as_mut()
        .expect("expanded text cache was initialized")
        .content;
    if let ExpandedView::AgentLog { run, .. } = view {
        // Refresh the live clock without reprojecting and wrapping the log body.
        content.header[0] = agent_log_title(run, width);
    }
    expanded_text_surface_lines(view, scroll, viewport_rows, panel_rows, content)
}

pub(crate) fn cached_expanded_scroll_metrics(
    cache: &RefCell<Option<ExpandedTextRenderCache>>,
    view: &ExpandedView,
    viewport_rows: usize,
    workspace: &Path,
    width: u16,
    panel_rows: usize,
) -> (usize, usize) {
    let Some(key) = ExpandedTextCacheKey::new(view, width) else {
        return expanded_scroll_metrics(view, viewport_rows, workspace, width, panel_rows);
    };
    let mut cache = cache.borrow_mut();
    if cache.as_ref().is_none_or(|cached| cached.key != key) {
        rebuild_expanded_text_cache(&mut cache, key, view, workspace, width);
    }
    let cached = cache.as_ref().expect("expanded text cache was initialized");
    expanded_text_scroll_metrics(
        cached.content.header.len(),
        cached.content.body.len(),
        viewport_rows,
        panel_rows,
    )
}

pub(crate) fn cached_expanded_visible_path_rows(
    cache: &RefCell<Option<ExpandedTextRenderCache>>,
    view: &ExpandedView,
    scroll: usize,
    viewport_rows: usize,
    workspace: &Path,
    width: u16,
    panel_rows: usize,
) -> Vec<(usize, crate::markdown::DisplayRow)> {
    let Some(key) = ExpandedTextCacheKey::new(view, width) else {
        return Vec::new();
    };
    let mut cache = cache.borrow_mut();
    if cache.as_ref().is_none_or(|cached| cached.key != key) {
        rebuild_expanded_text_cache(&mut cache, key, view, workspace, width);
    }
    let content = &cache
        .as_ref()
        .expect("expanded text cache was initialized")
        .content;
    let (capacity, maximum) = expanded_text_scroll_metrics(
        content.header.len(),
        content.body.len(),
        viewport_rows,
        panel_rows,
    );
    let offset = scroll.min(maximum);
    content
        .body
        .visible_rows(view, offset, capacity)
        .into_iter()
        .enumerate()
        .map(|(index, row)| (content.header.len().saturating_add(index), row))
        .collect()
}

/// Returns the action for a pointer over an expanded text view's scroll
/// indicator. This uses the same cached content as rendering, so input does
/// not reparse, highlight, and wrap the complete output merely to hit-test an
/// arrow.
pub(crate) fn cached_expanded_indicator_action(
    cache: &RefCell<Option<ExpandedTextRenderCache>>,
    view: &ExpandedView,
    scroll: usize,
    viewport_rows: usize,
    workspace: &Path,
    width: u16,
    panel_rows: usize,
    row: usize,
) -> Option<(ScrollViewAction, usize, usize)> {
    let key = ExpandedTextCacheKey::new(view, width)?;
    let mut cache = cache.borrow_mut();
    if cache.as_ref().is_none_or(|cached| cached.key != key) {
        rebuild_expanded_text_cache(&mut cache, key, view, workspace, width);
    }
    let content = &cache
        .as_ref()
        .expect("expanded text cache was initialized")
        .content;
    let (capacity, maximum) = expanded_text_scroll_metrics(
        content.header.len(),
        content.body.len(),
        viewport_rows,
        panel_rows,
    );
    // Rendering replaces the trailing blank header row with the widget's top
    // indicator, so the widget begins exactly one row before the header ends.
    let widget_row = row.checked_sub(content.header.len().saturating_sub(1))?;
    let state = ScrollViewState {
        offset: scroll.min(maximum),
        content_rows: content.body.len(),
    };
    let ScrollViewHit::Indicator(action) = ScrollViewWidget::new(capacity)
        .indicators(ScrollViewAction::Home, ScrollViewAction::End)
        .render(&state, |_| Line::default())
        .hits
        .get(widget_row)
        .copied()
        .unwrap_or(ScrollViewHit::None)
    else {
        return None;
    };
    Some((action, capacity, maximum))
}

fn expanded_terminal_status(completion: &Option<String>) -> &'static str {
    let Some(completion) = completion else {
        return "running";
    };
    let failed = completion
        .strip_prefix("[exited with status ")
        .and_then(|status| status.strip_suffix(']'))
        .and_then(|status| status.parse::<i32>().ok())
        .is_some_and(|status| status != 0);
    if failed { "failed" } else { "completed" }
}

/// Returns the visible body rows and last valid offset for any expanded view.
#[must_use]
pub(crate) fn expanded_scroll_metrics(
    view: &ExpandedView,
    viewport_rows: usize,
    workspace: &Path,
    width: u16,
    panel_rows: usize,
) -> (usize, usize) {
    if let ExpandedView::Terminal {
        ansi_output,
        completion,
        ..
    } = view
    {
        let maximum = crate::render::terminal_scrollback_max_with_completion(
            ansi_output,
            completion.as_deref(),
            width.saturating_sub(4),
            u16::try_from(viewport_rows).unwrap_or(u16::MAX),
        );
        return (viewport_rows, maximum);
    }
    let ExpandedTextContent { header, body, .. } = expanded_text_content(view, workspace, width);
    let metrics = expanded_text_scroll_metrics(header.len(), body.len(), viewport_rows, panel_rows);
    if let ExpandedView::AgentLog { max_scroll, .. } = view {
        max_scroll.set(Some(metrics.1));
    }
    metrics
}

fn expanded_text_scroll_metrics(
    header_rows: usize,
    body_rows: usize,
    viewport_rows: usize,
    panel_rows: usize,
) -> (usize, usize) {
    let fixed_header_rows = header_rows.saturating_sub(1);
    let capacity = viewport_rows.min(
        panel_rows
            .saturating_sub(fixed_header_rows)
            .saturating_sub(2),
    );
    scroll_window_metrics(body_rows, capacity)
}

struct ExpandedTextContent {
    header: Vec<Line<'static>>,
    body: ExpandedTextBody,
    inset: bool,
}

enum ExpandedTextBody {
    Materialized(Vec<crate::markdown::DisplayRow>),
    AgentLog {
        prefix: std::sync::Arc<Vec<crate::markdown::DisplayRow>>,
        prefix_len: usize,
        tail: Vec<crate::markdown::DisplayRow>,
    },
    HighlightedFile(HighlightedFileRows),
    FullFileDiff(FullFileDiffRows),
}

impl ExpandedTextBody {
    fn len(&self) -> usize {
        match self {
            Self::Materialized(rows) => rows.len(),
            Self::AgentLog {
                prefix_len, tail, ..
            } => prefix_len + tail.len(),
            Self::HighlightedFile(rows) => rows.len(),
            Self::FullFileDiff(rows) => rows.len(),
        }
    }

    fn visible_rows(
        &self,
        view: &ExpandedView,
        offset: usize,
        capacity: usize,
    ) -> Vec<crate::markdown::DisplayRow> {
        match self {
            Self::Materialized(rows) => rows.iter().skip(offset).take(capacity).cloned().collect(),
            Self::AgentLog {
                prefix,
                prefix_len,
                tail,
            } => prefix[..*prefix_len]
                .iter()
                .chain(tail)
                .skip(offset)
                .take(capacity)
                .cloned()
                .collect(),
            Self::HighlightedFile(layout) => {
                let ExpandedView::File {
                    view:
                        cagent_agent::presentation::FileView {
                            content: cagent_agent::presentation::FileViewContent::Text { lines, .. },
                            ..
                        },
                } = view
                else {
                    return Vec::new();
                };
                layout.visible_rows(lines, offset, capacity)
            }
            Self::FullFileDiff(layout) => {
                let ExpandedView::Diff { view } = view else {
                    return Vec::new();
                };
                layout.visible_rows(view, offset, capacity)
            }
        }
    }
}

impl From<Vec<crate::markdown::DisplayRow>> for ExpandedTextBody {
    fn from(rows: Vec<crate::markdown::DisplayRow>) -> Self {
        Self::Materialized(rows)
    }
}

fn expanded_text_content(view: &ExpandedView, workspace: &Path, width: u16) -> ExpandedTextContent {
    match view {
        ExpandedView::Mcp { call } => {
            let status = match call.status {
                cagent_agent::presentation::ToolActivityStatus::Pending => "running",
                cagent_agent::presentation::ToolActivityStatus::Succeeded => "completed",
                cagent_agent::presentation::ToolActivityStatus::Failed => "failed",
            };
            let elapsed = call.duration_millis.map(|ms| {
                if ms < 1_000 {
                    format!("{ms}ms")
                } else {
                    cagent_agent::presentation::format_elapsed(ms / 1_000)
                }
            });
            let mut lines = vec![Line::styled("Parameters:", MENU_TITLE_STYLE)];
            lines.extend(json_preview_lines(
                &call.parameters,
                width.saturating_sub(4).max(1),
            ));
            lines.push(Line::default());
            lines.push(Line::styled("Output:", MENU_TITLE_STYLE));
            if let Some(output) = call.display_output() {
                lines.extend(json_preview_lines(&output, width.saturating_sub(4).max(1)));
            } else {
                lines.push(Line::styled(
                    if call.status == cagent_agent::presentation::ToolActivityStatus::Pending {
                        "Waiting for output…"
                    } else {
                        "No output available."
                    },
                    DIM_STYLE,
                ));
            }
            ExpandedTextContent {
                header: vec![
                    expanded_title(
                        "MCP",
                        Some(Line::from(format!("{}/{}", call.server, call.tool))),
                        [expanded_status_detail(status, elapsed.as_deref())],
                        width,
                    ),
                    Line::default(),
                ],
                body: lines
                    .into_iter()
                    .map(crate::markdown::DisplayRow::plain)
                    .collect::<Vec<_>>()
                    .into(),
                inset: true,
            }
        }
        ExpandedView::Compaction { summary } => ExpandedTextContent {
            header: vec![
                expanded_title("Compacted context", None, [], width),
                Line::default(),
            ],
            body: crate::markdown::layout_document_with_paths(
                &cagent_agent::presentation::parse_markdown(summary),
                width.saturating_sub(4).max(1),
                workspace,
                false,
            )
            .into(),
            inset: true,
        },
        ExpandedView::RepositoryDiff { diff, title } => ExpandedTextContent {
            header: vec![
                expanded_title(
                    title,
                    None,
                    [Line::from(format!("{} files", diff.files.len()))],
                    width,
                ),
                Line::default(),
            ],
            body: render_expanded_diff(diff, width)
                .into_iter()
                .map(crate::markdown::DisplayRow::plain)
                .collect::<Vec<_>>()
                .into(),
            inset: false,
        },
        ExpandedView::WebFetch {
            url,
            redirected_url,
            format,
            output,
        } => {
            let mut url_spans = vec![Span::raw(url.clone())];
            if let Some(destination) = redirected_url {
                url_spans.push(Span::styled(" → ", DIM_STYLE));
                url_spans.push(Span::raw(destination.clone()));
            }
            ExpandedTextContent {
                header: vec![
                    expanded_title("Web Fetch", Some(Line::from(url_spans)), [], width),
                    Line::default(),
                ],
                body: web_fetch_content_lines(format, output, width)
                    .into_iter()
                    .collect::<Vec<_>>()
                    .into(),
                inset: true,
            }
        }
        ExpandedView::WebSearch {
            provider,
            query,
            results,
        } => ExpandedTextContent {
            header: vec![
                expanded_title(
                    "Web Search",
                    Some(Line::from(query.clone())),
                    [
                        Line::from(provider.clone()),
                        Line::from(format!("{} results", results.len())),
                    ],
                    width,
                ),
                Line::default(),
            ],
            body: web_search_result_lines(results, width)
                .into_iter()
                .map(crate::markdown::DisplayRow::plain)
                .collect::<Vec<_>>()
                .into(),
            inset: true,
        },
        ExpandedView::AgentLog {
            run,
            streaming,
            terminals,
            expanded_explorations,
            expanded_activity_runs,
            collapse_tool_activity,
            ..
        } => agent_log_content(
            run,
            streaming,
            terminals,
            expanded_explorations,
            expanded_activity_runs,
            *collapse_tool_activity,
            workspace,
            width,
        ),
        ExpandedView::File { view } => file_content(view, width),
        ExpandedView::Diff { view } => full_file_diff_content(view, width),
        ExpandedView::Image { metadata, .. } => ExpandedTextContent {
            header: vec![
                expanded_title(
                    "Image",
                    Some(Line::from(format!("#{}", metadata.number))),
                    [
                        Line::from("PNG"),
                        Line::from(format!("{}×{}", metadata.width, metadata.height)),
                        Line::from(crate::render::format_bytes(metadata.size_bytes as usize)),
                    ],
                    width,
                ),
                Line::default(),
            ],
            body: Vec::new().into(),
            inset: true,
        },
        ExpandedView::Directory { browser } => {
            directory_content(&browser.tree, browser.selected_index(), width)
        }
        ExpandedView::Terminal { .. } => unreachable!("terminal content uses its renderer"),
    }
}

fn agent_log_title(run: &cagent_agent::protocol::AgentRun, width: u16) -> Line<'static> {
    let status = format!("{:?}", run.status).to_ascii_lowercase();
    let elapsed = cagent_agent::presentation::agent_run_elapsed(run);
    expanded_title(
        "Sub-agent log",
        Some(Line::from(run.profile.clone())),
        [
            Line::from(format!(
                "{}{}",
                run.model,
                run.effort
                    .as_deref()
                    .map_or_else(String::new, |effort| format!(" {effort}")),
            )),
            expanded_status_detail(&status, elapsed.as_deref()),
        ],
        width,
    )
}

fn agent_log_content(
    run: &cagent_agent::protocol::AgentRun,
    streaming: &str,
    terminals: &[cagent_agent::tools::TerminalSnapshot],
    expanded_explorations: &std::collections::HashSet<usize>,
    expanded_activity_runs: &std::collections::HashSet<usize>,
    collapse_tool_activity: bool,
    workspace: &Path,
    width: u16,
) -> ExpandedTextContent {
    let log =
        cagent_agent::presentation::project_agent_run_log_with_terminals(run, workspace, terminals);
    agent_log_content_inner(
        run,
        streaming,
        &log,
        &mut AgentLogRowsCache::default(),
        expanded_explorations,
        expanded_activity_runs,
        collapse_tool_activity,
        workspace,
        width,
        true,
    )
    .0
}

fn agent_log_header(run: &cagent_agent::protocol::AgentRun, width: u16) -> Vec<Line<'static>> {
    let mut header = vec![agent_log_title(run, width)];
    let mut usage = cagent_agent::protocol::SessionUsage::default();
    if let Some(model_usage) = &run.usage {
        usage.add(model_usage);
    }
    if let Some(usage) = cagent_agent::presentation::agent_run_usage_line(&usage) {
        header.push(truncate_line(
            Line::from(Span::styled(format!("  {usage}"), DIM_STYLE)),
            usize::from(width),
        ));
    }
    header.push(Line::default());
    header
}

#[allow(clippy::too_many_arguments)]
fn agent_log_content_inner(
    run: &cagent_agent::protocol::AgentRun,
    streaming: &str,
    log: &cagent_agent::presentation::AgentRunLog,
    row_cache: &mut AgentLogRowsCache,
    expanded_explorations: &std::collections::HashSet<usize>,
    expanded_activity_runs: &std::collections::HashSet<usize>,
    collapse_tool_activity: bool,
    workspace: &Path,
    width: u16,
    include_tail: bool,
) -> (
    ExpandedTextContent,
    Option<crate::render::transcript::TranscriptFlowRole>,
) {
    let safe_task = terminal_safe(&run.task);
    let task_prefix = "  Task  ";
    let header = agent_log_header(run, width);
    let mut body = Vec::new();
    for (index, (start, end)) in wrap_ranges(
        &safe_task,
        width.saturating_sub(u16::try_from(task_prefix.width()).unwrap_or(u16::MAX)),
    )
    .into_iter()
    .enumerate()
    {
        body.push(crate::markdown::DisplayRow::plain(Line::from(vec![
            Span::styled(if index == 0 { task_prefix } else { "        " }, DIM_STYLE),
            Span::raw(safe_task[start..end].to_owned()),
        ])));
    }
    body.push(crate::markdown::DisplayRow::plain(Line::default()));

    let mut exploration_index = 0;
    let mut group_index = 0;
    let mut flow_items = Vec::new();
    let mut collapsed: Option<(
        usize,
        cagent_agent::presentation::CollapsedActivitySummary,
        Vec<crate::markdown::DisplayRow>,
        bool,
    )> = None;
    let flush_collapsed = |collapsed: &mut Option<(
        usize,
        cagent_agent::presentation::CollapsedActivitySummary,
        Vec<crate::markdown::DisplayRow>,
        bool,
    )>,
                           flow_items: &mut Vec<_>,
                           is_tail: bool| {
        if let Some((index, summary, details, active)) = collapsed.take() {
            if summary.is_empty() {
                flow_items.push((
                    crate::render::transcript::TranscriptFlowRole::NonMessage,
                    details,
                ));
                return;
            }
            let target = crate::markdown::ActivityCollapseTarget::AgentLog {
                run_id: run.id,
                group_index: index,
            };
            let expanded = expanded_activity_runs.contains(&index);
            let mut rows = crate::render::transcript::collapsed_summary_rows(
                &summary,
                &target,
                expanded,
                active && is_tail,
                width,
            );
            if expanded {
                rows.extend(crate::render::transcript::expanded_activity_rows(
                    &details, &target, width,
                ));
            }
            flow_items.push((
                crate::render::transcript::TranscriptFlowRole::NonMessage,
                rows,
            ));
        }
    };
    for (entry_index, entry) in log.entries.iter().enumerate() {
        let item = match entry {
            cagent_agent::presentation::AgentRunLogEntry::Assistant(document) => {
                flush_collapsed(&mut collapsed, &mut flow_items, false);
                (
                    crate::render::transcript::TranscriptFlowRole::Assistant,
                    row_cache
                        .entry(AgentLogRowKey::Assistant(entry_index))
                        .or_insert_with(|| {
                            std::sync::Arc::new(
                                crate::render::transcript::transcript_entry_rows_with_state(
                                    crate::render::transcript::TranscriptEntry::Assistant(document),
                                    workspace,
                                    width,
                                    true,
                                    |_, _| None,
                                ),
                            )
                        })
                        .as_ref()
                        .clone(),
                )
            }
            cagent_agent::presentation::AgentRunLogEntry::ToolGroups(groups) => {
                for group in groups {
                    let ordinal = exploration_index;
                    let exploration_expanded = matches!(
                        group,
                        cagent_agent::presentation::ToolActivityGroup::Exploration { .. }
                    ) && expanded_explorations.contains(&ordinal);
                    let expandable = matches!(group,
                        cagent_agent::presentation::ToolActivityGroup::Exploration { activities, .. }
                            if activities.len() > 4
                    );
                    let rows = row_cache
                        .entry(AgentLogRowKey::Tool(group_index, exploration_expanded))
                        .or_insert_with(|| {
                            std::sync::Arc::new(
                                crate::render::transcript::transcript_entry_rows_with_state(
                                    crate::render::transcript::TranscriptEntry::ToolGroups(
                                        std::slice::from_ref(group),
                                    ),
                                    workspace,
                                    width,
                                    true,
                                    |_, _| {
                                        expandable.then(|| {
                                            let target = crate::markdown::ExplorationToggleTarget::AgentLog {
                                                run_id: run.id,
                                                group_index: ordinal,
                                            };
                                            (target, exploration_expanded)
                                        })
                                    },
                                ),
                            )
                        });
                    let mut summary =
                        cagent_agent::presentation::CollapsedActivitySummary::default();
                    if collapse_tool_activity
                        && cagent_agent::presentation::summarize_collapsible_group(
                            &mut summary,
                            group,
                        )
                    {
                        let pending = collapsed.get_or_insert_with(|| {
                            (group_index, Default::default(), Vec::new(), false)
                        });
                        pending.1.merge(&summary);
                        // Hidden cards stay shared in the cache; only visible details
                        // need to be copied into the composed log.
                        if expanded_activity_runs.contains(&pending.0) || pending.1.is_empty() {
                            if !pending.2.is_empty() && !rows.is_empty() {
                                crate::render::transcript::append_transcript_boundary(
                                    &mut pending.2,
                                    Some(crate::render::transcript::TranscriptFlowRole::NonMessage),
                                    crate::render::transcript::TranscriptFlowRole::NonMessage,
                                    width,
                                );
                            }
                            pending.2.extend(rows.iter().cloned());
                        }
                        pending.3 |= crate::render::transcript::activity_group_is_active(group);
                    } else {
                        flush_collapsed(&mut collapsed, &mut flow_items, false);
                        flow_items.push((
                            crate::render::transcript::TranscriptFlowRole::NonMessage,
                            rows.as_ref().clone(),
                        ));
                    }
                    if matches!(
                        group,
                        cagent_agent::presentation::ToolActivityGroup::Exploration { .. }
                    ) {
                        exploration_index += 1;
                    }
                    group_index += 1;
                }
                continue;
            }
        };
        flow_items.push(item);
    }
    flush_collapsed(&mut collapsed, &mut flow_items, streaming.is_empty());
    if log.error.is_none() && !streaming.is_empty() {
        let document = cagent_agent::presentation::parse_markdown(streaming);
        flow_items.push((
            crate::render::transcript::TranscriptFlowRole::Assistant,
            crate::render::transcript::transcript_entry_rows_with_state(
                crate::render::transcript::TranscriptEntry::Assistant(&document),
                workspace,
                width,
                true,
                |_, _| None,
            ),
        ));
    }
    let last_role = flow_items.last().map(|(role, _)| *role);
    body.extend(crate::render::transcript::transcript_flow_rows(
        flow_items, width,
    ));
    if include_tail {
        append_agent_log_tail(
            &mut body,
            log.error.as_deref(),
            log.entries.is_empty(),
            streaming.is_empty(),
            width,
        );
    }
    (
        ExpandedTextContent {
            header,
            body: body.into(),
            inset: false,
        },
        last_role,
    )
}

fn append_agent_log_tail(
    body: &mut Vec<crate::markdown::DisplayRow>,
    error: Option<&str>,
    entries_empty: bool,
    streaming_empty: bool,
    width: u16,
) {
    if let Some(error) = error {
        if !entries_empty {
            body.push(crate::markdown::DisplayRow::plain(Line::default()));
        }
        body.extend(
            wrap_log_line(
                &Line::from(Span::styled(
                    format!("  Error  {}", terminal_safe(error)),
                    ERROR_STYLE,
                )),
                width,
            )
            .into_iter()
            .map(crate::markdown::DisplayRow::plain),
        );
    } else if entries_empty && streaming_empty {
        body.push(crate::markdown::DisplayRow::plain(Line::from(
            Span::styled("  No activity yet.", DIM_STYLE),
        )));
    }
    body.push(crate::markdown::DisplayRow::plain(Line::default()));
}

fn file_content(view: &cagent_agent::presentation::FileView, width: u16) -> ExpandedTextContent {
    use cagent_agent::presentation::FileViewContent;

    let detail = match &view.content {
        FileViewContent::Text { lines, .. } => format!("{} lines", lines.len()),
        FileViewContent::Image { format } => format!("{format} image"),
        FileViewContent::Binary => "binary".into(),
        FileViewContent::TooLarge { .. } => "too large".into(),
        FileViewContent::Unavailable { .. } => "unavailable".into(),
    };
    let header = vec![
        expanded_title(
            "File",
            Some(Line::from(view.path.to_string_lossy().into_owned())),
            [
                Line::from(detail),
                Line::from(crate::render::format_bytes(
                    usize::try_from(view.bytes).unwrap_or(usize::MAX),
                )),
            ],
            width,
        ),
        Line::default(),
    ];
    let body = match &view.content {
        FileViewContent::Text {
            language,
            lines,
            highlighting,
        } => ExpandedTextBody::HighlightedFile(HighlightedFileRows::new(
            lines,
            width,
            language,
            *highlighting,
        )),
        FileViewContent::Image { .. } => Vec::new().into(),
        FileViewContent::Binary => vec![crate::markdown::DisplayRow::plain(Line::from(
            Span::styled("  Binary files cannot be displayed.", DIM_STYLE),
        ))]
        .into(),
        FileViewContent::TooLarge { maximum_bytes } => vec![crate::markdown::DisplayRow::plain(
            Line::from(Span::styled(
                format!(
                    "  File is too large to display (limit {}).",
                    crate::render::format_bytes(
                        usize::try_from(*maximum_bytes).unwrap_or(usize::MAX)
                    )
                ),
                DIM_STYLE,
            )),
        )]
        .into(),
        FileViewContent::Unavailable { message } => vec![crate::markdown::DisplayRow::plain(
            Line::from(Span::styled(
                format!("  File is unavailable: {}", terminal_safe(message)),
                ERROR_STYLE,
            )),
        )]
        .into(),
    };
    ExpandedTextContent {
        header,
        body,
        inset: false,
    }
}

fn full_file_diff_content(
    view: &cagent_agent::presentation::FullFileDiffView,
    width: u16,
) -> ExpandedTextContent {
    ExpandedTextContent {
        header: vec![
            expanded_title(
                "Diff",
                Some(Line::from(view.path.to_string_lossy().into_owned())),
                [Line::from(vec![
                    Span::styled(
                        format!("+{}", view.added_lines),
                        crate::render::DIFF_ADDITION_STYLE,
                    ),
                    Span::raw(" "),
                    Span::styled(
                        format!("-{}", view.removed_lines),
                        crate::render::DIFF_DELETION_STYLE,
                    ),
                ])],
                width,
            ),
            Line::default(),
        ],
        body: ExpandedTextBody::FullFileDiff(FullFileDiffRows::new(view, width)),
        inset: false,
    }
}

struct FullFileDiffRows {
    row_starts: Vec<usize>,
    number_width: usize,
    content_width: u16,
    width: u16,
}

impl FullFileDiffRows {
    fn new(view: &cagent_agent::presentation::FullFileDiffView, width: u16) -> Self {
        let largest = view
            .lines
            .iter()
            .flat_map(|line| [line.old_line, line.new_line])
            .flatten()
            .max()
            .unwrap_or(1);
        let number_width = largest.to_string().len();
        let prefix_width = format!("  {:>number_width$} + ", largest).width();
        let content_width = width.saturating_sub(u16::try_from(prefix_width).unwrap_or(u16::MAX));
        let mut row_starts = Vec::with_capacity(view.lines.len().saturating_add(1));
        row_starts.push(0usize);
        for line in &view.lines {
            let highlighted = cagent_agent::presentation::HighlightedLine {
                tokens: line.tokens.clone(),
            };
            let count = file_line_wrap_count(&highlighted, content_width);
            row_starts.push(
                row_starts
                    .last()
                    .copied()
                    .unwrap_or_default()
                    .saturating_add(count),
            );
        }
        Self {
            row_starts,
            number_width,
            content_width,
            width,
        }
    }

    fn len(&self) -> usize {
        self.row_starts.last().copied().unwrap_or_default()
    }

    fn visible_rows(
        &self,
        view: &cagent_agent::presentation::FullFileDiffView,
        offset: usize,
        capacity: usize,
    ) -> Vec<crate::markdown::DisplayRow> {
        if capacity == 0 || offset >= self.len() {
            return Vec::new();
        }
        let mut line_index = self
            .row_starts
            .partition_point(|start| *start <= offset)
            .saturating_sub(1)
            .min(view.lines.len().saturating_sub(1));
        let mut rows = Vec::with_capacity(capacity);
        while rows.len() < capacity {
            let Some(line) = view.lines.get(line_index) else {
                break;
            };
            let highlighted =
                if view.highlighting == cagent_agent::presentation::FileHighlighting::Viewport {
                    let source = line
                        .tokens
                        .iter()
                        .map(|token| token.text.as_str())
                        .collect::<String>();
                    cagent_agent::presentation::highlight_file_line(&view.language, &source)
                } else {
                    cagent_agent::presentation::HighlightedLine {
                        tokens: line.tokens.clone(),
                    }
                };
            let (tokens, source) = prepared_file_line(&highlighted);
            let ranges = file_wrap_ranges(&source, self.content_width);
            let first = offset
                .saturating_sub(self.row_starts[line_index])
                .min(ranges.len());
            for (wrapped, (start, end)) in ranges.into_iter().enumerate().skip(first) {
                rows.push(full_file_diff_row(
                    line,
                    &tokens,
                    start,
                    end,
                    wrapped,
                    self.number_width,
                    self.width,
                ));
                if rows.len() == capacity {
                    break;
                }
            }
            line_index = line_index.saturating_add(1);
        }
        rows
    }
}

pub(crate) fn full_file_diff_line_offset(
    view: &cagent_agent::presentation::FullFileDiffView,
    width: u16,
    old_line: Option<u64>,
    new_line: Option<u64>,
) -> usize {
    let layout = FullFileDiffRows::new(view, width);
    let index = view
        .lines
        .iter()
        .position(|line| {
            new_line.is_some_and(|number| line.new_line == Some(number))
                || (new_line.is_none()
                    && old_line.is_some_and(|number| line.old_line == Some(number)))
        })
        .unwrap_or_default();
    layout.row_starts.get(index).copied().unwrap_or_default()
}

pub(crate) fn full_file_diff_scroll_metrics(
    view: &cagent_agent::presentation::FullFileDiffView,
    width: u16,
    viewport_rows: usize,
) -> (usize, usize) {
    let layout = FullFileDiffRows::new(view, width);
    expanded_text_scroll_metrics(2, layout.len(), viewport_rows, viewport_rows)
}

fn full_file_diff_row(
    line: &cagent_agent::presentation::FullFileDiffLine,
    tokens: &[(cagent_agent::presentation::CodeTokenKind, String)],
    start: usize,
    end: usize,
    wrapped: usize,
    number_width: usize,
    width: u16,
) -> crate::markdown::DisplayRow {
    let (number, marker, style) = match line.kind {
        cagent_agent::tools::DiffLineKind::Addition => {
            (line.new_line, '+', crate::render::DIFF_ADDITION_STYLE)
        }
        cagent_agent::tools::DiffLineKind::Deletion => {
            (line.old_line, '-', crate::render::DIFF_DELETION_STYLE)
        }
        cagent_agent::tools::DiffLineKind::Context => {
            (line.new_line.or(line.old_line), ' ', Style::default())
        }
    };
    let mut spans = vec![Span::styled(
        if wrapped == 0 {
            format!("  {:>number_width$} {marker} ", number.unwrap_or_default())
        } else {
            format!("  {:>number_width$}   ", "")
        },
        if wrapped == 0 { style } else { DIM_STYLE },
    )];
    let mut token_start = 0;
    for (kind, text) in tokens {
        let token_end = token_start + text.len();
        let slice_start = start.max(token_start);
        let slice_end = end.min(token_end);
        if slice_start < slice_end {
            spans.push(Span::styled(
                text[slice_start - token_start..slice_end - token_start].to_owned(),
                crate::markdown::code_style(*kind),
            ));
        }
        token_start = token_end;
    }
    let mut line = Line::from(spans).style(style);
    let padding = usize::from(width).saturating_sub(line.width());
    if padding > 0 {
        line.spans.push(Span::styled(" ".repeat(padding), style));
    }
    crate::markdown::DisplayRow::plain(line)
}

struct HighlightedFileRows {
    row_starts: Vec<usize>,
    number_width: usize,
    content_width: u16,
    viewport_language: Option<String>,
    visible_cache: RefCell<Option<HighlightedFileWindow>>,
}

struct HighlightedFileWindow {
    start: usize,
    rows: Vec<crate::markdown::DisplayRow>,
}

impl HighlightedFileRows {
    fn new(
        lines: &[cagent_agent::presentation::HighlightedLine],
        width: u16,
        language: &str,
        highlighting: cagent_agent::presentation::FileHighlighting,
    ) -> Self {
        let number_width = lines.len().max(1).to_string().len();
        let prefix_width = format!("  {:>number_width$} │ ", 1).width();
        let content_width = width.saturating_sub(u16::try_from(prefix_width).unwrap_or(u16::MAX));
        let mut row_starts = Vec::with_capacity(lines.len().saturating_add(1));
        row_starts.push(0usize);
        for line in lines {
            let count = file_line_wrap_count(line, content_width);
            let next = row_starts
                .last()
                .copied()
                .unwrap_or_default()
                .saturating_add(count);
            row_starts.push(next);
        }
        Self {
            row_starts,
            number_width,
            content_width,
            viewport_language: (highlighting
                == cagent_agent::presentation::FileHighlighting::Viewport)
                .then(|| language.to_owned()),
            visible_cache: RefCell::new(None),
        }
    }

    fn len(&self) -> usize {
        self.row_starts.last().copied().unwrap_or_default()
    }

    fn visible_rows(
        &self,
        lines: &[cagent_agent::presentation::HighlightedLine],
        offset: usize,
        capacity: usize,
    ) -> Vec<crate::markdown::DisplayRow> {
        if capacity == 0 || offset >= self.len() {
            return Vec::new();
        }
        let end = offset.saturating_add(capacity).min(self.len());
        if let Some(cached) = self.visible_cache.borrow().as_ref()
            && offset >= cached.start
            && end <= cached.start.saturating_add(cached.rows.len())
        {
            let start = offset - cached.start;
            return cached.rows[start..start + end - offset].to_vec();
        }
        let margin = capacity.max(32);
        let start = offset.saturating_sub(margin);
        let cached_end = end.saturating_add(margin).min(self.len());
        let rows = self.render_rows(lines, start, cached_end.saturating_sub(start));
        let visible_start = offset - start;
        let visible = rows[visible_start..visible_start + end - offset].to_vec();
        *self.visible_cache.borrow_mut() = Some(HighlightedFileWindow { start, rows });
        visible
    }

    fn render_rows(
        &self,
        lines: &[cagent_agent::presentation::HighlightedLine],
        offset: usize,
        capacity: usize,
    ) -> Vec<crate::markdown::DisplayRow> {
        let mut line_index = self
            .row_starts
            .partition_point(|start| *start <= offset)
            .saturating_sub(1)
            .min(lines.len().saturating_sub(1));
        let mut rows = Vec::with_capacity(capacity.min(self.len().saturating_sub(offset)));
        while rows.len() < capacity {
            let Some(line) = lines.get(line_index) else {
                break;
            };
            let highlighted = self.viewport_language.as_ref().map(|language| {
                cagent_agent::presentation::highlight_file_line(
                    language,
                    &raw_file_line_source(line),
                )
            });
            let line = highlighted.as_ref().unwrap_or(line);
            let (tokens, source) = prepared_file_line(line);
            let ranges = file_wrap_ranges(&source, self.content_width);
            let first = offset
                .saturating_sub(self.row_starts[line_index])
                .min(ranges.len());
            for (wrapped, (start, end)) in ranges.into_iter().enumerate().skip(first) {
                rows.push(highlighted_file_row(
                    &tokens,
                    start,
                    end,
                    line_index,
                    wrapped,
                    self.number_width,
                ));
                if rows.len() == capacity {
                    break;
                }
            }
            line_index = line_index.saturating_add(1);
        }
        rows
    }
}

pub(crate) fn file_logical_line_offset(
    view: &cagent_agent::presentation::FileView,
    width: u16,
    line: usize,
) -> usize {
    let cagent_agent::presentation::FileViewContent::Text {
        language,
        lines,
        highlighting,
    } = &view.content
    else {
        return 0;
    };
    let layout = HighlightedFileRows::new(lines, width, language, *highlighting);
    let logical = line.saturating_sub(1).min(lines.len().saturating_sub(1));
    layout.row_starts.get(logical).copied().unwrap_or_default()
}

fn file_line_wrap_count(line: &cagent_agent::presentation::HighlightedLine, width: u16) -> usize {
    let width = usize::from(width.max(1));
    let mut display_width = 0usize;
    for token in &line.tokens {
        if file_token_needs_sanitizing(&token.text) {
            return file_wrap_ranges(
                &file_line_source(line),
                u16::try_from(width).unwrap_or(u16::MAX),
            )
            .len();
        }
        display_width = display_width.saturating_add(token.text.width());
    }
    if display_width <= width {
        1
    } else {
        file_wrap_ranges(
            &file_line_source(line),
            u16::try_from(width).unwrap_or(u16::MAX),
        )
        .len()
    }
}

fn file_wrap_ranges(source: &str, width: u16) -> Vec<(usize, usize)> {
    if source.width() <= usize::from(width.max(1)) {
        vec![(0, source.len())]
    } else {
        wrap_ranges(source, width)
    }
}

fn file_line_source(line: &cagent_agent::presentation::HighlightedLine) -> String {
    let mut source = String::with_capacity(line.tokens.iter().map(|token| token.text.len()).sum());
    for token in &line.tokens {
        if file_token_needs_sanitizing(&token.text) {
            source.push_str(&terminal_safe(&token.text).replace('\t', "    "));
        } else {
            source.push_str(&token.text);
        }
    }
    source
}

fn raw_file_line_source(line: &cagent_agent::presentation::HighlightedLine) -> String {
    line.tokens
        .iter()
        .map(|token| token.text.as_str())
        .collect()
}

fn file_token_needs_sanitizing(text: &str) -> bool {
    text.chars().any(char::is_control)
}

fn prepared_file_line(
    line: &cagent_agent::presentation::HighlightedLine,
) -> (
    Vec<(cagent_agent::presentation::CodeTokenKind, String)>,
    String,
) {
    let tokens = line
        .tokens
        .iter()
        .map(|token| {
            let text = if file_token_needs_sanitizing(&token.text) {
                terminal_safe(&token.text).replace('\t', "    ")
            } else {
                token.text.clone()
            };
            (token.kind, text)
        })
        .collect::<Vec<_>>();
    let source = tokens
        .iter()
        .map(|(_, text)| text.as_str())
        .collect::<String>();
    (tokens, source)
}

fn highlighted_file_row(
    tokens: &[(cagent_agent::presentation::CodeTokenKind, String)],
    start: usize,
    end: usize,
    line_index: usize,
    wrapped: usize,
    number_width: usize,
) -> crate::markdown::DisplayRow {
    let mut spans = vec![Span::styled(
        if wrapped == 0 {
            format!("  {:>number_width$} │ ", line_index + 1)
        } else {
            format!("  {:>number_width$} │ ", "")
        },
        DIM_STYLE,
    )];
    let mut token_start = 0;
    for (kind, text) in tokens {
        let token_end = token_start + text.len();
        let slice_start = start.max(token_start);
        let slice_end = end.min(token_end);
        if slice_start < slice_end {
            spans.push(Span::styled(
                text[slice_start - token_start..slice_end - token_start].to_owned(),
                crate::markdown::code_style(*kind),
            ));
        }
        token_start = token_end;
    }
    crate::markdown::DisplayRow::plain(Line::from(spans))
}

fn directory_content(
    tree: &cagent_agent::presentation::DirectoryTree,
    selected: usize,
    width: u16,
) -> ExpandedTextContent {
    let entries = tree.rows();
    let entry_count = entries.iter().filter(|entry| entry.is_selectable()).count();
    let mut header = vec![
        expanded_title(
            "Directory",
            Some(Line::from(tree.root().to_string_lossy().into_owned())),
            [Line::from(format!("{entry_count} entries"))],
            width,
        ),
        Line::default(),
    ];
    if tree.directory_was_truncated(tree.root()) {
        header.insert(
            1,
            truncate_line(
                Line::from(Span::styled(
                    format!(
                        "  Showing the first {} entries.",
                        cagent_agent::presentation::DIRECTORY_VIEW_MAX_ENTRIES
                    ),
                    DIM_STYLE,
                )),
                usize::from(width),
            ),
        );
    }
    let body = tree.unavailable().map_or_else(
        || super::file_tree::styled_rows(tree, selected, width),
        |message| {
            vec![crate::markdown::DisplayRow::plain(Line::from(
                Span::styled(
                    format!("  Directory is unavailable: {}", terminal_safe(message)),
                    ERROR_STYLE,
                ),
            ))]
        },
    );
    ExpandedTextContent {
        header,
        body: body.into(),
        inset: false,
    }
}

pub(crate) fn web_fetch_content_lines(
    format: &cagent_agent::WebFetchFormat,
    output: &str,
    width: u16,
) -> Vec<crate::markdown::DisplayRow> {
    match format {
        cagent_agent::WebFetchFormat::Markdown => crate::markdown::layout_document(
            &cagent_agent::presentation::parse_markdown(output),
            width.saturating_sub(4),
        ),
        cagent_agent::WebFetchFormat::Text => output
            .lines()
            .flat_map(|line| {
                wrap_log_line(&Line::from(terminal_safe(line)), width.saturating_sub(4))
            })
            .map(crate::markdown::DisplayRow::plain)
            .collect(),
        cagent_agent::WebFetchFormat::Html => output
            .lines()
            .flat_map(|line| {
                let highlighted = Line::from(
                    cagent_agent::presentation::highlight_code("html", line)
                        .into_iter()
                        .map(|token| {
                            Span::styled(token.text, crate::markdown::code_style(token.kind))
                        })
                        .collect::<Vec<_>>(),
                );
                wrap_log_line(&highlighted, width.saturating_sub(4))
            })
            .map(crate::markdown::DisplayRow::plain)
            .collect(),
    }
}

/// Formats normalized web-search results for the shared expanded result view.
#[must_use]
pub(crate) fn web_search_result_lines(
    results: &[cagent_agent::web_search::WebSearchResult],
    width: u16,
) -> Vec<Line<'static>> {
    let text_width = width.saturating_sub(4);
    let mut lines = Vec::new();
    for (index, result) in results.iter().enumerate() {
        if index > 0 {
            lines.push(Line::default());
        }
        let title = terminal_safe(&result.title);
        for (line_index, (start, end)) in wrap_ranges(&title, text_width).into_iter().enumerate() {
            lines.push(Line::from(vec![
                Span::styled(if line_index == 0 { "• " } else { "  " }, ACCENT_STYLE),
                Span::styled(
                    title[start..end].to_owned(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ]));
        }
        lines.push(Line::from(Span::styled(
            format!("  {}", terminal_safe(&result.url)),
            ACCENT_STYLE,
        )));
        let snippet = terminal_safe(&result.snippet);
        if result.published_at.is_some() || !snippet.is_empty() {
            lines.push(Line::default());
        }
        if let Some(published_at) = &result.published_at {
            lines.push(Line::from(Span::styled(
                format!("  {}", terminal_safe(published_at)),
                DIM_STYLE,
            )));
        }
        if !snippet.is_empty() {
            for (start, end) in wrap_ranges(&snippet, text_width.saturating_sub(2)) {
                lines.push(Line::from(Span::styled(
                    format!("  {}", &snippet[start..end]),
                    DIM_STYLE,
                )));
            }
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled("No results.", DIM_STYLE)));
    }
    lines
}

/// Matches the two-column terminal inset used by Bash output surfaces.
fn inset_expanded_output_line(line: Line<'static>) -> Line<'static> {
    let mut spans = vec![Span::raw("  ")];
    spans.extend(line.spans);
    Line::from(spans).style(line.style)
}

pub(crate) fn mcp_form_choices(
    draft: &McpFormDraft,
    selected: usize,
    width: u16,
) -> Vec<(bool, String, String)> {
    let scope = match draft.location.scope {
        cagent_agent::mcp::McpScope::Global => "global".into(),
        cagent_agent::mcp::McpScope::Project => "project".into(),
        cagent_agent::mcp::McpScope::Agent => "removed agent scope".into(),
    };
    let mut rows: Vec<(String, String)> = vec![
        ("Close".into(), String::new()),
        ("Preview and save".into(), "validate before writing".into()),
        ("Name".into(), draft.name.clone()),
        ("Scope".into(), scope),
        ("Enabled".into(), draft.definition.enabled.to_string()),
        (
            "Assigned agents".into(),
            json_detail(&draft.definition.agents),
        ),
        (
            "Startup timeout".into(),
            format!("{} seconds", draft.definition.startup_timeout_seconds),
        ),
        (
            "Request timeout".into(),
            format!("{} seconds", draft.definition.request_timeout_seconds),
        ),
        (
            "Read-only tools".into(),
            json_detail(&draft.definition.read_only_tools),
        ),
    ];
    match &draft.definition.transport {
        cagent_agent::mcp::McpTransportConfig::Stdio {
            command,
            args,
            cwd,
            env,
            env_remove,
            inherit_env,
        } => rows.extend([
            ("Command".into(), command.clone()),
            ("Arguments".into(), json_detail(args)),
            ("Working directory".into(), cwd.clone().unwrap_or_default()),
            ("Environment".into(), json_detail(env)),
            ("Removed variables".into(), json_detail(env_remove)),
            ("Inherit environment".into(), inherit_env.to_string()),
        ]),
        cagent_agent::mcp::McpTransportConfig::StreamableHttp {
            url,
            headers,
            allow_insecure,
        } => rows.extend([
            ("URL".into(), url.clone()),
            ("Headers".into(), json_detail(headers)),
            ("Allow insecure".into(), allow_insecure.to_string()),
        ]),
    }
    rows.into_iter()
        .enumerate()
        .map(|(index, (label, detail))| {
            let available = usize::from(width)
                .saturating_sub(2)
                .saturating_sub(label.width())
                .saturating_sub(2);
            (
                selected == index,
                label,
                truncate_with_ellipsis(&detail, available),
            )
        })
        .collect()
}

pub(crate) const fn mcp_form_item_count(draft: &McpFormDraft) -> usize {
    match &draft.definition.transport {
        cagent_agent::mcp::McpTransportConfig::Stdio { .. } => 15,
        cagent_agent::mcp::McpTransportConfig::StreamableHttp { .. } => 12,
    }
}

fn json_detail(value: &impl serde::Serialize) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "invalid".into())
}

pub(super) const fn mcp_field_label(field: McpFormField) -> &'static str {
    match field {
        McpFormField::Name => "name",
        McpFormField::Agents => "assigned agents JSON array",
        McpFormField::StartupTimeout => "startup timeout seconds",
        McpFormField::RequestTimeout => "request timeout seconds",
        McpFormField::ReadOnlyTools => "read-only tools JSON array",
        McpFormField::Command => "command",
        McpFormField::Arguments => "arguments JSON array",
        McpFormField::Cwd => "working directory",
        McpFormField::Environment => "environment JSON object",
        McpFormField::RemovedEnvironment => "removed variables JSON array",
        McpFormField::Url => "URL",
        McpFormField::Headers => "headers JSON object",
    }
}

pub(super) fn required_surface(title: &str, message: &str) -> Vec<Line<'static>> {
    vec![
        menu_title(title, None),
        Line::default(),
        Line::from(format!("  {message}")).style(DIM_STYLE),
        Line::default(),
    ]
}

fn search_menu_title(
    title: &str,
    prefix: &str,
    input: &SingleLineInput,
    width: u16,
) -> Line<'static> {
    let mut spans = menu_title(title, None).spans;
    spans.push(Span::raw("  "));
    spans.push(Span::styled(prefix.to_owned(), MENU_DETAIL_STYLE));
    let available = usize::from(width).saturating_sub(Line::from(spans.clone()).width());
    spans.extend(
        input
            .render_bounded(available, MENU_DETAIL_STYLE, SEARCH_CURSOR_STYLE)
            .spans,
    );
    Line::from(spans)
}

fn no_filter_matches(kind: &str) -> Line<'static> {
    Line::from(format!("  No {kind} match your search.")).style(DIM_STYLE)
}

#[allow(clippy::too_many_lines)]
fn permission_surface_lines(
    request: &cagent_agent::protocol::InteractionRequest,
    selected: usize,
    scope: cagent_agent::permissions::PermissionScope,
    diff_scroll: usize,
    denial_note: &str,
    editing_note: bool,
    note_cursor: usize,
    workspace: &Path,
    width: u16,
    viewport_rows: usize,
) -> Vec<Line<'static>> {
    permission_surface_lines_with_cache(
        request,
        selected,
        scope,
        diff_scroll,
        denial_note,
        editing_note,
        note_cursor,
        workspace,
        width,
        viewport_rows,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn permission_surface_lines_with_cache(
    request: &cagent_agent::protocol::InteractionRequest,
    selected: usize,
    scope: cagent_agent::permissions::PermissionScope,
    diff_scroll: usize,
    denial_note: &str,
    editing_note: bool,
    note_cursor: usize,
    workspace: &Path,
    width: u16,
    viewport_rows: usize,
    cache: Option<&RefCell<Option<PermissionDiffRenderCache>>>,
) -> Vec<Line<'static>> {
    let (prefix, diff, mut suffix) =
        permission_surface_parts(request, selected, scope, workspace, width);
    append_permission_note(&mut suffix, denial_note, editing_note, note_cursor, width);
    let diff_rows = diff.map_or(0, |preview| {
        cache.map_or_else(
            || permission_preview_line_count(preview, width),
            |cache| cached_permission_preview_line_count(cache, request.id, preview, width),
        )
    });
    if prefix.len().saturating_add(suffix.len()) > viewport_rows {
        return clipped_permission_chrome(prefix, suffix, viewport_rows);
    }
    let available_diff_rows =
        viewport_rows.saturating_sub(prefix.len().saturating_add(suffix.len()));
    let mut lines = prefix;
    if let Some(preview) = diff
        && available_diff_rows > 0
    {
        // The blank row after the prompt and the list's upper blank indicator
        // are the permanent scroll-view indicator slots. Reuse them so the
        // choices never move while the preview scrolls.
        lines.pop();
        suffix.remove(0);
        let mut state = ScrollViewState {
            offset: diff_scroll,
            content_rows: diff_rows,
        };
        state.reconcile(diff_rows, available_diff_rows, false);
        lines.extend(
            render_permission_scroll_window(&state, available_diff_rows, |start, rows| {
                cache.map_or_else(
                    || render_permission_preview_window(preview, width, start, rows),
                    |cache| {
                        render_cached_permission_preview_window(
                            cache, request.id, preview, width, start, rows,
                        )
                    },
                )
            })
            .lines,
        );
    }
    lines.extend(suffix);
    lines
}

pub(crate) fn cached_permission_surface_lines_with_viewport(
    cache: &RefCell<Option<PermissionDiffRenderCache>>,
    surface: &Surface,
    workspace: &Path,
    width: u16,
    viewport_rows: usize,
) -> Vec<Line<'static>> {
    let Surface::Permission {
        request,
        selected,
        scope,
        diff_scroll,
        denial_note,
        editing_note,
        note_cursor,
    } = surface
    else {
        return surface_lines_with_viewport(surface, workspace, width, viewport_rows);
    };
    permission_surface_lines_with_cache(
        request,
        *selected,
        *scope,
        *diff_scroll,
        denial_note,
        *editing_note,
        *note_cursor,
        workspace,
        width,
        viewport_rows,
        Some(cache),
    )
}

fn append_permission_note(
    suffix: &mut Vec<Line<'static>>,
    denial_note: &str,
    editing_note: bool,
    note_cursor: usize,
    width: u16,
) {
    if !editing_note {
        return;
    }
    suffix.extend(note_input_layout(denial_note, note_cursor, width).lines);
}

fn clipped_permission_chrome(
    mut prefix: Vec<Line<'static>>,
    suffix: Vec<Line<'static>>,
    viewport_rows: usize,
) -> Vec<Line<'static>> {
    let prefix_rows = viewport_rows.saturating_sub(suffix.len());
    let prefix_was_clipped = prefix.len() > prefix_rows;
    prefix.truncate(prefix_rows);
    if prefix_was_clipped {
        if let Some(last) = prefix.last_mut() {
            *last = Line::from("  …").style(DIM_STYLE);
        }
    }
    prefix.extend(
        suffix
            .into_iter()
            .take(viewport_rows.saturating_sub(prefix.len())),
    );
    prefix
}

/// Returns the uncropped height of a permission prompt without rendering its
/// diff rows. Large diff prompts use this to claim the available composer area
/// without paying to syntax-highlight the full preview.
#[cfg(test)]
pub(crate) fn permission_surface_line_count(
    surface: &Surface,
    workspace: &Path,
    width: u16,
) -> usize {
    permission_surface_line_count_with_cache(surface, workspace, width, None)
}

pub(crate) fn cached_permission_surface_line_count(
    cache: &RefCell<Option<PermissionDiffRenderCache>>,
    surface: &Surface,
    workspace: &Path,
    width: u16,
) -> usize {
    permission_surface_line_count_with_cache(surface, workspace, width, Some(cache))
}

fn permission_surface_line_count_with_cache(
    surface: &Surface,
    workspace: &Path,
    width: u16,
    cache: Option<&RefCell<Option<PermissionDiffRenderCache>>>,
) -> usize {
    let Surface::Permission {
        request,
        selected,
        scope,
        denial_note,
        editing_note,
        note_cursor,
        ..
    } = surface
    else {
        return 0;
    };
    let (prefix, diff, mut suffix) =
        permission_surface_parts(request, *selected, *scope, workspace, width);
    append_permission_note(&mut suffix, denial_note, *editing_note, *note_cursor, width);
    prefix
        .len()
        .saturating_add(diff.map_or(0, |preview| {
            cache.map_or_else(
                || permission_preview_line_count(preview, width),
                |cache| cached_permission_preview_line_count(cache, request.id, preview, width),
            )
        }))
        .saturating_add(suffix.len())
}

pub(crate) fn cached_permission_diff_scroll_metrics(
    cache: &RefCell<Option<PermissionDiffRenderCache>>,
    surface: &Surface,
    workspace: &Path,
    width: u16,
    viewport_rows: usize,
) -> (usize, usize) {
    permission_diff_scroll_metrics_with_cache(surface, workspace, width, viewport_rows, Some(cache))
}

pub(crate) fn cached_permission_diff_max_scroll(
    cache: &RefCell<Option<PermissionDiffRenderCache>>,
    surface: &Surface,
    workspace: &Path,
    width: u16,
    viewport_rows: usize,
) -> usize {
    cached_permission_diff_scroll_metrics(cache, surface, workspace, width, viewport_rows).1
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn cached_permission_diff_max_scroll_for_request(
    cache: &RefCell<Option<PermissionDiffRenderCache>>,
    request: &cagent_agent::protocol::InteractionRequest,
    scope: cagent_agent::permissions::PermissionScope,
    denial_note: &str,
    editing_note: bool,
    note_cursor: usize,
    workspace: &Path,
    width: u16,
    viewport_rows: usize,
) -> usize {
    permission_diff_scroll_metrics_for_request(
        request,
        scope,
        denial_note,
        editing_note,
        note_cursor,
        workspace,
        width,
        viewport_rows,
        Some(cache),
    )
    .1
}

fn permission_diff_scroll_metrics_with_cache(
    surface: &Surface,
    workspace: &Path,
    width: u16,
    viewport_rows: usize,
    cache: Option<&RefCell<Option<PermissionDiffRenderCache>>>,
) -> (usize, usize) {
    let Surface::Permission {
        request,
        scope,
        denial_note,
        editing_note,
        note_cursor,
        ..
    } = surface
    else {
        return (0, 0);
    };
    permission_diff_scroll_metrics_for_request(
        request,
        *scope,
        denial_note,
        *editing_note,
        *note_cursor,
        workspace,
        width,
        viewport_rows,
        cache,
    )
}

#[allow(clippy::too_many_arguments)]
fn permission_diff_scroll_metrics_for_request(
    request: &cagent_agent::protocol::InteractionRequest,
    scope: cagent_agent::permissions::PermissionScope,
    denial_note: &str,
    editing_note: bool,
    note_cursor: usize,
    workspace: &Path,
    width: u16,
    viewport_rows: usize,
    cache: Option<&RefCell<Option<PermissionDiffRenderCache>>>,
) -> (usize, usize) {
    let (prefix, diff, mut suffix) = permission_surface_parts(request, 0, scope, workspace, width);
    append_permission_note(&mut suffix, denial_note, editing_note, note_cursor, width);
    if prefix.len().saturating_add(suffix.len()) > viewport_rows {
        return (0, 0);
    }
    let base_diff_rows = viewport_rows.saturating_sub(prefix.len().saturating_add(suffix.len()));
    let available_diff_rows = base_diff_rows;
    let maximum = diff
        .map_or(0_usize, |preview| {
            cache.map_or_else(
                || permission_preview_line_count(preview, width),
                |cache| cached_permission_preview_line_count(cache, request.id, preview, width),
            )
        })
        .saturating_sub(available_diff_rows);
    (available_diff_rows, maximum)
}

fn permission_surface_parts<'a>(
    request: &'a cagent_agent::protocol::InteractionRequest,
    selected: usize,
    scope: cagent_agent::permissions::PermissionScope,
    _workspace: &Path,
    width: u16,
) -> (
    Vec<Line<'static>>,
    Option<PermissionPreview<'a>>,
    Vec<Line<'static>>,
) {
    let InteractionRequestKind::PermissionApproval {
        resource,
        message,
        preview,
        arguments,
        auto_review,
        ..
    } = &request.kind
    else {
        return (Vec::new(), None, Vec::new());
    };
    let detail = match &request.origin {
        Some(cagent_agent::protocol::InteractionOrigin::SubAgent { profile, .. }) => {
            format!(
                "{} sub-agent · {}",
                profile,
                permission_tool_label(&resource.tool)
            )
        }
        None => permission_tool_label(&resource.tool),
    };
    let mut prefix = vec![menu_title("Permission", Some(&detail)), Line::default()];
    if let Some(cagent_agent::protocol::InteractionOrigin::SubAgent { id, profile }) =
        &request.origin
    {
        prefix.push(Line::from(vec![
            Span::styled("  Requested by  ", DIM_STYLE),
            Span::raw(format!("{profile} sub-agent · {id}")),
        ]));
        prefix.push(Line::default());
    }
    if resource.tool == "web_fetch" {
        let url = resource.command.first().map_or("", String::as_str);
        let format = arguments
            .as_ref()
            .and_then(|arguments| arguments.get("format"))
            .and_then(serde_json::Value::as_str)
            .filter(|format| *format != "markdown");
        let redirected_from = arguments
            .as_ref()
            .and_then(|arguments| arguments.get("redirected_from"))
            .and_then(serde_json::Value::as_array)
            .map(|urls| {
                urls.iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(terminal_safe)
                    .collect::<Vec<_>>()
            })
            .filter(|urls| !urls.is_empty());
        let mut spans = vec![
            Span::raw("  Allow web fetch of "),
            Span::styled(terminal_safe(url), CHIP_STYLE),
            Span::raw("?"),
        ];
        if let Some(redirected_from) = redirected_from {
            spans.push(Span::styled(
                format!(" (redirected from {})", redirected_from.join(" → ")),
                DIM_STYLE,
            ));
        }
        if let Some(format) = format {
            spans.push(Span::styled(format!(" ({format})"), DIM_STYLE));
        }
        prefix.push(Line::from(spans));
    } else if resource.tool == "web_search" {
        let query = resource.command.first().map_or("", String::as_str);
        prefix.push(Line::from(vec![
            Span::raw("  Allow web search of "),
            Span::styled(terminal_safe(query), CHIP_STYLE),
            Span::raw("?"),
        ]));
    } else if let Some(path) = cagent_agent::presentation::bash_read_permission_path(request) {
        prefix.push(Line::from(vec![
            Span::raw("  Allow reading from "),
            Span::styled(terminal_safe(&path), CHIP_STYLE),
            Span::raw("?"),
        ]));
    } else if resource.tool == "enter_worktree" {
        let target = resource.command.first().map_or("", String::as_str);
        prefix.push(Line::from(vec![
            Span::raw("  Allow changing worktree to "),
            Span::styled(terminal_safe(target), CHIP_STYLE),
            Span::raw("?"),
        ]));
    } else if resource.tool == "bash" && resource.raw_command.is_some() {
        prefix.push(Line::from("  Allow running the following?"));
    } else {
        prefix.push(Line::from(format!("  {}", terminal_safe(message))));
    }
    if resource.tool == "bash"
        && cagent_agent::presentation::bash_read_permission_path(request).is_none()
        && let Some(command) = resource.raw_command.as_deref()
    {
        prefix.push(Line::default());
        prefix.extend(command.lines().flat_map(|line| {
            let mut spans = vec![Span::styled("  │ ", MENU_DETAIL_STYLE)];
            spans.extend(bash_command_spans(line));
            wrap_log_line(&Line::from(spans), width)
        }));
    }
    if let Some(review) = auto_review {
        prefix.extend(wrap_log_line(
            &Line::from(format!("  Auto review  {}", terminal_safe(&review.reason)))
                .style(DIM_STYLE),
            width,
        ));
    }

    let preview = (resource.tool != "web_fetch")
        .then(|| arguments.as_ref().map(PermissionPreview::Json))
        .flatten()
        .or_else(|| preview.as_ref().map(PermissionPreview::Diff));
    if preview.is_some() {
        prefix.push(Line::default());
    }

    let choices = permission_approval_choices(request, scope);
    let state = ListState::selectable_at(choices.len(), selected, choices.len().max(1));
    let suffix = ListWidget::new(width, choices.len().max(1))
        .indicators(true, false)
        .render(&state, |index, _, selected| {
            let choice = &choices[index];
            vec![menu_choice_line(selected, &choice.label, &choice.detail)]
        })
        .lines;
    (prefix, preview, suffix)
}

pub(crate) fn mcp_mutation_detail_lines(
    preview: &cagent_agent::mcp::McpMutationPreview,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for name in &preview.affected {
        lines.push(Line::from(format!("  affects {name}")).style(DIM_STYLE));
    }
    for server in &preview.revealed {
        lines.push(
            Line::from(format!(
                "  reveals {} at {:?}",
                server.name, server.location.scope
            ))
            .style(DIM_STYLE),
        );
    }
    for reference in preview
        .touched_agent_policies
        .iter()
        .chain(&preview.touched_permission_rules)
    {
        lines.push(Line::from(format!("  rewrites {reference}")).style(DIM_STYLE));
    }
    lines
}

pub(crate) fn surface_status(surface: &Surface) -> String {
    match surface {
        Surface::Onboarding => "Enter setup · Esc dismiss".to_owned(),
        Surface::Permission {
            request,
            selected,
            scope,
            editing_note,
            ..
        } => {
            if *editing_note {
                return "Enter deny · Shift/Alt+Enter newline · Tab/Ctrl+C close · Esc discard note"
                    .to_owned();
            }
            let choices = permission_approval_choices(request, *scope);
            let choice = choices.get(*selected);
            if choice.is_some_and(|choice| choice.decision == "deny") {
                "↑/↓ navigate · PgUp/PgDn scroll preview · Enter/Esc deny · Tab add denial note"
                    .to_owned()
            } else if choice.is_some_and(cagent_agent::presentation::PermissionApprovalChoice::is_persistent) {
                "↑/↓ navigate · PgUp/PgDn scroll preview · Enter select · Tab scope · e edit · Shift+Enter apply globally · Esc deny"
                    .to_owned()
            } else if choice.is_some_and(cagent_agent::presentation::PermissionApprovalChoice::supports_session) {
                "↑/↓ navigate · PgUp/PgDn scroll preview · Enter select · Tab scope · e edit · Shift+Enter apply for conversation · Esc deny"
                    .to_owned()
            } else {
                "↑/↓ navigate · PgUp/PgDn scroll preview · Enter select · Esc deny".to_owned()
            }
        }
        Surface::PermissionRuleEdit { scope, .. } => format!(
            "Enter submit {} · Esc cancel",
            match scope {
                cagent_agent::permissions::PermissionScope::Conversation => "for conversation",
                cagent_agent::permissions::PermissionScope::ConversationGlobal => "globally",
                cagent_agent::permissions::PermissionScope::Project => "for project",
                cagent_agent::permissions::PermissionScope::Global => "globally",
            }
        ),
        Surface::Permissions { .. } => {
            "↑/↓ navigate · Enter close · e edit · d delete · Esc close".to_owned()
        }
        Surface::Usage { .. } => "↑/↓ navigate · Enter select · Esc close".to_owned(),
        Surface::UsageBreakdown { .. } => "↑/↓ scroll · Esc back".to_owned(),
        Surface::UsageResetConfirm { .. } => "↑/↓ choose · Enter select · Esc cancel".to_owned(),
        Surface::PersistentPermissionEdit { scope, .. } => format!(
            "Enter save {} · Esc cancel",
            match scope {
                cagent_agent::permissions::PermissionScope::Conversation => "for conversation",
                cagent_agent::permissions::PermissionScope::ConversationGlobal => "globally",
                cagent_agent::permissions::PermissionScope::Project => "for project",
                cagent_agent::permissions::PermissionScope::Global => "globally",
            }
        ),
        Surface::PlanCompletion { editing_note: true, .. } =>
            "Enter accept · Shift/Alt+Enter newline · Tab/Ctrl+C close · Esc discard note".to_owned(),
        Surface::PlanCompletion { selected, .. } => {
            if *selected < 3 {
                "↑/↓ choices · Shift+Left/Right/Tab mode · Alt-p/m change model · Tab add note · Enter select · Esc keep planning"
                    .to_owned()
            } else {
                "↑/↓ choices · Shift+Left/Right/Tab mode · Alt-p/m change model · Enter select · Esc keep planning"
                    .to_owned()
            }
        }
        Surface::Question {
            editing_note: true,
            ..
        } => "Enter select · Shift/Alt+Enter newline · Tab return · Esc discard note"
            .to_owned(),
        Surface::Question { .. } => {
            "↑/↓ choose · ←/→ tabs · Tab add note · Enter select · Esc cancel".to_owned()
        }
        Surface::Rename { .. } | Surface::WebSearchSetup { .. } => {
            "Enter save · Esc cancel".to_owned()
        }
        Surface::Worktrees { .. } => "↑/↓ navigate · Enter select · Esc close".to_owned(),
        Surface::WorktreeNew { editing_base, .. } => {
            if *editing_base {
                "Enter create · Tab edit name · Esc cancel".to_owned()
            } else {
                "Enter/Tab edit base · Esc cancel".to_owned()
            }
        }
        Surface::WebSearchPicker {
            rows, list, query, ..
        } => {
            let selected = list.selected.unwrap_or(0);
            let ready = selected
                .checked_sub(1)
                .and_then(|index| {
                    rows.iter()
                        .filter(|row| row.provider.label().contains(&query.to_lowercase()))
                        .nth(index)
                })
                .is_some_and(|row| row.ready);
            if selected == 0 {
                "↑/↓ navigate · Enter close · Esc close".to_owned()
            } else if ready {
                "↑/↓ navigate · Enter select · r reconfigure · d disconnect · Esc close".to_owned()
            } else {
                "↑/↓ navigate · Enter configure · Esc close".to_owned()
            }
        }
        Surface::McpServers { .. }
        | Surface::McpCatalog { .. }
        | Surface::McpServer { .. }
        | Surface::McpRemoveConfirm { .. }
        | Surface::McpAddScope { .. }
        | Surface::McpTransport { .. }
        | Surface::McpForm { .. } => "↑/↓ navigate · Enter select · Esc back".to_owned(),
        Surface::McpMutationPreview { .. } => {
            "↑/↓ navigate · PgUp/PgDn scroll details · Enter select · Esc back".to_owned()
        }
        Surface::McpTools { .. } => "↑/↓ navigate · Enter/Esc back".to_owned(),
        Surface::McpOAuth { attempt, .. }
            if matches!(
                &*attempt.completion.borrow(),
                cagent_agent::mcp::McpOAuthCompletion::Connected
            ) =>
        {
            "Enter/Esc continue".to_owned()
        }
        Surface::McpOAuth { attempt, .. } => match &attempt.prompt {
            cagent_agent::mcp::McpOAuthPrompt::Device { .. } => {
                "Enter reopen device login · c copy URL · C copy code · Esc back".to_owned()
            }
            cagent_agent::mcp::McpOAuthPrompt::Browser { .. } => {
                "Enter reopen browser login · c copy URL · Esc back".to_owned()
            }
        },
        Surface::McpPackageSetup { .. } => "↑/↓ navigate · Enter edit/save · Esc back".to_owned(),
        Surface::McpPackageValueEdit { field, .. } => match field {
            McpPackageField::Secret(_) => {
                "Enter save secret · empty removes · Esc discard".to_owned()
            }
            McpPackageField::Parameter(_) => "Enter save value · Esc discard".to_owned(),
        },
        Surface::McpFieldEdit { .. } => "Enter save field · Esc restore draft".to_owned(),
        Surface::McpJsonEdit { .. } => "Ctrl+Enter submit · Esc back".to_owned(),
        Surface::Providers {
            rows,
            list,
            query,
            model_configuration_follows,
            ..
        } => {
            let selected = list.selected.unwrap_or(0);
            let continue_disabled = selected == 0
                && *model_configuration_follows
                && !rows.iter().any(|row| row.enabled);
            if continue_disabled {
                return "↑/↓ navigate · Esc back".to_owned();
            }
            let action = if selected == 0 {
                if *model_configuration_follows {
                    "continue"
                } else {
                    "close"
                }
            } else {
                filter_provider_picker_rows(rows, query)
                    .get(selected - 1)
                    .and_then(|row| row.setup_instructions.as_ref())
                    .map_or("toggle", |_| "configure")
            };
            let binding = "Enter";
            if selected == 0 {
                format!("↑/↓ navigate · {binding} {action} · Esc back")
            } else if filter_provider_picker_rows(rows, query)
                .get(selected - 1)
                .is_some_and(|row| row.managed_auth && row.setup_instructions.is_none())
            {
                format!("↑/↓ navigate · {binding} {action} · r reconnect · d disconnect · Esc back")
            } else if filter_provider_picker_rows(rows, query)
                .get(selected - 1)
                .is_some_and(|row| !row.managed_auth && row.setup_instructions.is_none())
            {
                if filter_provider_picker_rows(rows, query)
                    .get(selected - 1)
                    .is_some_and(|row| row.api_key_auth && row.has_managed_api_key)
                {
                    format!(
                        "↑/↓ navigate · {binding} {action} · r replace key · d remove key · Esc back"
                    )
                } else {
                    format!("↑/↓ navigate · {binding} {action} · r reconfigure · Esc back")
                }
            } else {
                format!("↑/↓ navigate · {binding} {action} · Esc back")
            }
        }
        Surface::ProviderSetup {
            managed_auth,
            api_key_auth,
            authenticated,
            auth_challenge,
            auth_flows,
            ..
        } => {
            if *api_key_auth {
                "Enter save API key · Esc discard".into()
            } else if *authenticated {
                "Enter/Esc continue".into()
            } else if *managed_auth
                && matches!(
                    auth_challenge,
                    Some(cagent_agent::provider::AuthChallenge::Device { .. })
                )
            {
                if auth_flows == &[cagent_agent::provider::AuthFlow::DeviceCode] {
                    "Enter reopen device login · c copy URL · C copy code · Esc back".into()
                } else {
                    "Enter browser login · c copy URL · C copy code · d device login · Esc back"
                        .into()
                }
            } else if *managed_auth {
                if auth_flows == &[cagent_agent::provider::AuthFlow::DeviceCode] {
                    "Enter device login · Esc back".into()
                } else if auth_flows == &[cagent_agent::provider::AuthFlow::BrowserPkce] {
                    "Enter browser login · c copy URL · Esc back".into()
                } else {
                    "Enter browser login · c copy URL · d device login · Esc back".into()
                }
            } else {
                "Enter/Esc back".into()
            }
        }
        Surface::ProvidersRequired | Surface::ModelRequired | Surface::ModelsUnavailable => {
            "Enter continue · Esc close".to_owned()
        }
        Surface::Models {
            selection_target: super::ModelSelectionTarget::Setting(_),
            ..
        } => "↑/↓ navigate · Enter save · Esc back".to_owned(),
        Surface::Models { .. } => {
            "↑/↓ navigate · Enter select · f favourite · Ctrl+D agent default · Esc back".to_owned()
        }
        Surface::Effort { .. } | Surface::Paths { .. } => {
            "↑/↓ navigate · Enter select · Esc back".to_owned()
        }
        Surface::Profiles {
            kind: super::ProfileKind::Agent,
            ..
        } => "↑/↓ navigate · Enter select · e edit · Esc back".to_owned(),
        Surface::Profiles { .. } => "↑/↓ navigate · Enter select · Esc back".to_owned(),
        Surface::Skills { .. } => {
            "↑/↓ navigate · Enter toggle · e edit · d delete · Esc close".to_owned()
        }
        Surface::SkillDeleteConfirm { .. } => {
            "↑/↓ navigate · Enter select · Esc back".to_owned()
        }
        Surface::SkillWizard { step: super::SkillWizardStep::Content, .. } => "Enter save · Ctrl+Enter newline · Esc back".to_owned(),
        Surface::SkillWizard { step: super::SkillWizardStep::Scope, .. } => "↑/↓ scope · Enter next · Esc back".to_owned(),
        Surface::SkillWizard { .. } => "Enter next · Esc back".to_owned(),
        Surface::AgentEdit { .. } => "↑/↓ navigate · Enter edit · Esc back".to_owned(),
        Surface::AgentWizard {
            step: AgentWizardStep::Prompt,
            ..
        } => "Enter save · Ctrl+Enter newline · Esc back".to_owned(),
        Surface::AgentWizard { .. } => "Enter save · Esc back".to_owned(),
        Surface::StatusLine { mode, .. } => match mode {
            StatusLineEditorMode::Modules => {
                "↑/↓ navigate · Enter toggle · ←/→ reorder · c color · r reset · Esc save"
                    .to_owned()
            }
            StatusLineEditorMode::Colors { .. } => {
                "↑/↓ colors · Enter select · Esc back".to_owned()
            }
            StatusLineEditorMode::Hex { .. } => "Enter apply #RRGGBB · Esc back".to_owned(),
        },
        Surface::Settings { query, .. } if query.is_empty() => {
            "type to search · ↑/↓ navigate · ←/→ tabs · Enter edit · Ctrl+R reset · Esc close"
                .to_owned()
        }
        Surface::Settings { .. } => {
            "type to search · ↑/↓ navigate · Enter edit · Ctrl+R reset · Esc close".to_owned()
        }
        Surface::SettingChoices { .. } => "↑/↓ navigate · Enter save · Esc back".to_owned(),
        Surface::SettingInput { .. } => "Enter save · Esc back".to_owned(),
        Surface::SupervisedWork { show_past, .. } => {
            let kill_hint = if *show_past { "" } else { " · k kill" };
            format!(
                "↑/↓ navigate · Enter expand{} · p show {} · Esc close",
                kill_hint,
                if *show_past { "active" } else { "past" }
            )
        }
        Surface::KillSupervisedWork { .. } => "↑/↓ navigate · Enter select · Esc close".to_owned(),
        Surface::Expanded {
            view: ExpandedView::Directory { .. },
            ..
        } => "↑/↓ navigate · ←/→ collapse/expand · Enter open · PgUp/PgDn page · Esc close"
            .to_owned(),
        Surface::Expanded { .. } => {
            "↑/↓ scroll · PgUp/PgDn page · Home/End jump · Enter/Esc close".to_owned()
        }
        Surface::HistoryTree { purpose, .. } => {
            if *purpose == super::TreePurpose::Fork {
                "type to search · ↑/↓ messages · p preview · ScrollToMessage scroll to · Enter fork · Shift+F hard fork · Esc cancel".to_owned()
            } else {
                "type to search · ↑/↓ messages · p preview · ScrollToMessage scroll to · Enter switch to · Shift+F hard fork · Esc close".to_owned()
            }
        }
        Surface::Conversations { .. } => {
            "type to search · ↑/↓ conversations · Enter resume · f favourite · a archived · Shift+A archive · Shift+D delete · Esc close".to_owned()
        }
        Surface::Help { .. } => "↑/↓ scroll · ←/→ tabs · Esc close".to_owned(),
    }
}

fn conversation_picker_lines(
    rows: &[cagent_agent::protocol::ConversationSummary],
    state: &ListState,
    query: &str,
    query_cursor: usize,
    opened_at_millis: u128,
    include_archived: bool,
    width: u16,
    viewport_rows: usize,
) -> Vec<Line<'static>> {
    let visible = conversation_picker_indexes(rows, query);
    let mut lines = vec![
        search_menu_title(
            "Resume",
            "search: ",
            &SingleLineInput::new(query.into(), query_cursor),
            width,
        ),
        Line::default(),
    ];
    let no_matches = !query.is_empty() && visible.is_empty();
    let capacity = conversation_picker_item_capacity(viewport_rows, include_archived, no_matches);
    let mut state = *state;
    state.reconcile(ListMode::Selectable, visible.len(), capacity);
    let layout = ListWidget::new(width, capacity).fill_capacity().render(
        &state,
        |position, _, is_selected| {
            let row = &rows[visible[position]];
            let title = if row.archived {
                format!("{} (archived)", row.title)
            } else {
                row.title.clone()
            };
            vec![picker_row(
                position,
                is_selected,
                &cagent_agent::presentation::relative_time(&row.updated_at, opened_at_millis),
                &title,
                row.message_count,
                row.model.as_deref().unwrap_or("no model"),
                row.favourite,
                width,
            )]
        },
    );
    lines.extend(layout.lines);
    if include_archived {
        lines.push(Line::from(Span::styled(
            "showing archived conversations",
            DIM_STYLE,
        )));
    }
    if no_matches {
        lines.push(no_filter_matches("conversations"));
    }
    lines.push(Line::default());
    lines
}

fn history_tree_picker_lines(
    rows: &[cagent_agent::presentation::HistoryRow],
    state: &ListState,
    purpose: super::TreePurpose,
    loading: bool,
    query: &str,
    query_cursor: usize,
    opened_at_millis: u128,
    width: u16,
    viewport_rows: usize,
) -> Vec<Line<'static>> {
    let visible_nodes = history_picker_indexes(rows, query);
    let title = if purpose == super::TreePurpose::Fork {
        "Fork"
    } else {
        "Conversation tree"
    };
    let mut lines = vec![
        search_menu_title(
            title,
            "search: ",
            &SingleLineInput::new(query.into(), query_cursor),
            width,
        ),
        Line::default(),
    ];
    if loading {
        let capacity = history_picker_item_capacity(viewport_rows);
        lines.push(Line::from("  Loading history…").style(DIM_STYLE));
        lines.extend(std::iter::repeat_n(
            Line::default(),
            capacity.saturating_sub(1),
        ));
        // Reserve the same two rows the populated list uses for scroll
        // indicators so the full-screen surface does not resize on arrival.
        lines.extend([Line::default(), Line::default()]);
        lines.push(Line::default());
        return lines;
    }
    let no_matches = !query.is_empty() && visible_nodes.is_empty();
    let capacity = history_picker_item_capacity(viewport_rows);
    let mut state = *state;
    state.reconcile(ListMode::Selectable, visible_nodes.len(), capacity);
    let layout = ListWidget::new(width, capacity).render(&state, |position, _, is_selected| {
        let index = visible_nodes[position];
        let row = &rows[index];
        let content = history_tree_row_line(row, is_selected, purpose, width.saturating_sub(14));
        vec![picker_tree_row(
            position,
            is_selected,
            &cagent_agent::presentation::relative_time(&row.created_at, opened_at_millis),
            content,
            width,
        )]
    });
    lines.extend(layout.lines);
    if no_matches {
        lines.push(no_filter_matches("messages"));
    }
    lines.push(Line::default());
    lines
}

fn picker_row(
    stripe: usize,
    selected: bool,
    timestamp: &str,
    label: &str,
    message_count: u64,
    detail: &str,
    favourite: bool,
    width: u16,
) -> Line<'static> {
    let background = if stripe.is_multiple_of(2) {
        Style::new().bg(ratatui::style::Color::Indexed(235))
    } else {
        Style::new().bg(ratatui::style::Color::Indexed(236))
    };
    let marker = if selected { "›" } else { " " };
    let label_style = if selected {
        SELECTED_STYLE
    } else {
        Style::default()
    };
    let message_plural = if message_count == 1 { "" } else { "s" };
    let message_detail = format!("{message_count} message{message_plural}");
    let label_limit = usize::from(width)
        .saturating_sub(14)
        .saturating_sub(message_detail.width() + 2)
        .saturating_sub(detail.width())
        .saturating_sub(if favourite { 2 } else { 0 });
    let label = truncate_with_ellipsis(label, label_limit);
    let mut spans = vec![
        Span::styled(
            format!(" {marker} {timestamp:>7}  "),
            background.add_modifier(Modifier::DIM),
        ),
        Span::styled(format!("{label}  "), background.patch(label_style)),
        Span::styled(
            format!("{message_detail}  "),
            background.patch(MENU_DETAIL_STYLE),
        ),
        Span::styled(detail.to_owned(), background.patch(MENU_DETAIL_STYLE)),
    ];
    if favourite {
        spans.push(Span::styled(" ★", background.patch(NOTICE_STYLE)));
    }
    let used = spans.iter().map(|span| span.content.width()).sum::<usize>();
    spans.push(Span::styled(
        " ".repeat(usize::from(width).saturating_sub(used)),
        background,
    ));
    Line::from(spans).style(background)
}

fn picker_tree_row(
    stripe: usize,
    selected: bool,
    timestamp: &str,
    content: Line<'static>,
    width: u16,
) -> Line<'static> {
    let background = if stripe.is_multiple_of(2) {
        Style::new().bg(ratatui::style::Color::Indexed(235))
    } else {
        Style::new().bg(ratatui::style::Color::Indexed(236))
    };
    let marker = if selected { "›" } else { " " };
    let mut spans = vec![Span::styled(
        format!(" {marker} {timestamp:>7}  "),
        background.add_modifier(Modifier::DIM),
    )];
    spans.extend(content.spans);
    let used = spans.iter().map(|span| span.content.width()).sum::<usize>();
    spans.push(Span::styled(
        " ".repeat(usize::from(width).saturating_sub(used)),
        background,
    ));
    Line::from(spans).style(background)
}

#[cfg(test)]
mod agent_log_cache_tests {
    use super::*;
    use cagent_agent::protocol::{AgentRun, AgentRunActivity, AgentRunTimelineEntry};
    use std::cell::Cell;
    use std::sync::Arc;

    #[test]
    fn subagent_expansion_reuses_cards_and_separates_them() {
        let mut timeline = vec![AgentRunTimelineEntry::Assistant {
            sequence: 0,
            text: "Inspecting the code.\n\n```rust\nfn main() {}\n```".into(),
            created_at: "0".into(),
        }];
        for sequence in 1..=6 {
            let (tool, arguments) = if sequence == 6 {
                (
                    "bash",
                    serde_json::json!({"command": "cargo check", "wait": true}),
                )
            } else {
                (
                    "grep",
                    serde_json::json!({"pattern": format!("needle{sequence}"), "path": "src"}),
                )
            };
            timeline.push(AgentRunTimelineEntry::Tool {
                activity: AgentRunActivity {
                    sequence,
                    tool: tool.into(),
                    arguments,
                    output: serde_json::json!({"output": "Finished\n", "exit_code": 0}),
                    is_error: false,
                    permission_audit: None,
                    created_at: sequence.to_string(),
                },
            });
        }
        let mut view = ExpandedView::AgentLog {
            run: Box::new(AgentRun {
                id: cagent_agent::protocol::AgentRunId::new(),
                conversation_id: cagent_agent::protocol::ConversationId::new(),
                parent_turn_id: cagent_agent::protocol::TurnId::new(),
                sequence: 0,
                profile: "explore".into(),
                model: cagent_agent::provider::ModelRef::parse("mock/echo").unwrap(),
                effort: None,
                task: "Inspect the project".into(),
                status: cagent_agent::protocol::AgentRunStatus::Completed,
                result: None,
                error: None,
                usage: None,
                created_at: "0".into(),
                started_at: Some("0".into()),
                completed_at: Some("6".into()),
                timeline,
                activity: Vec::new(),
            }),
            streaming: String::new(),
            terminals: Vec::new(),
            expanded_explorations: Default::default(),
            expanded_activity_runs: Default::default(),
            collapse_tool_activity: true,
            max_scroll: Cell::new(None),
        };
        let cache = RefCell::new(None);
        let render = |view: &ExpandedView, width| {
            cached_expanded_surface_lines(&cache, view, 0, 100, Path::new("."), width, 100, false)
        };
        let initial = render(&view, 80);
        assert!(
            !initial
                .iter()
                .any(|line| line.to_string().contains("Ran cargo check"))
        );
        let (projected, cards) = {
            let cache = cache.borrow();
            let agent = &cache.as_ref().unwrap().agent;
            (
                agent.log.as_ref().unwrap().entries.as_ptr(),
                agent.rows.clone(),
            )
        };
        assert_eq!(cards.len(), 3, "assistant, exploration and Bash are cached");

        for expanded in [true, false, true] {
            let ExpandedView::AgentLog {
                expanded_activity_runs,
                ..
            } = &mut view
            else {
                unreachable!()
            };
            if expanded {
                expanded_activity_runs.insert(0);
            } else {
                expanded_activity_runs.clear();
            }
            invalidate_expanded_text(&cache);
            // Metrics must populate the same cache used by rendering and hit testing.
            cached_expanded_scroll_metrics(&cache, &view, 100, Path::new("."), 80, 100);
            let lines = render(&view, 80)
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>();
            let ran = lines
                .iter()
                .position(|line| line.contains("Ran cargo check"));
            assert_eq!(ran.is_some(), expanded);
            if let Some(ran) = ran {
                assert!(lines[ran - 1].trim().is_empty());
                assert!(!lines[ran - 2].trim().is_empty(), "exactly one spacer");
            }
            let cache = cache.borrow();
            let agent = &cache.as_ref().unwrap().agent;
            assert_eq!(agent.log.as_ref().unwrap().entries.as_ptr(), projected);
            for (key, rows) in &cards {
                assert!(Arc::ptr_eq(rows, &agent.rows[key]));
            }
        }

        let ExpandedView::AgentLog {
            expanded_explorations,
            ..
        } = &mut view
        else {
            unreachable!()
        };
        expanded_explorations.insert(0);
        invalidate_expanded_text(&cache);
        render(&view, 80);
        {
            let cache = cache.borrow();
            let agent = &cache.as_ref().unwrap().agent;
            assert!(agent.rows.contains_key(&AgentLogRowKey::Tool(0, true)));
            for (key, rows) in &cards {
                assert!(Arc::ptr_eq(rows, &agent.rows[key]));
            }
        }

        let prefix = cache.borrow().as_ref().unwrap().agent.prefix.clone();
        let ExpandedView::AgentLog { streaming, .. } = &mut view else {
            unreachable!()
        };
        *streaming = "A streaming update".into();
        invalidate_expanded_text(&cache);
        render(&view, 80);
        assert!(Arc::ptr_eq(
            &prefix,
            &cache.borrow().as_ref().unwrap().agent.prefix
        ));

        render(&view, 40);
        let cache = cache.borrow();
        for (key, rows) in &cards {
            // The collapsed exploration variant is no longer needed at this width.
            if let Some(new_rows) = cache.as_ref().unwrap().agent.rows.get(key) {
                assert!(!Arc::ptr_eq(rows, new_rows));
            }
        }
    }
}

#[cfg(test)]
mod picker_tests {
    use super::{
        ExpandedTextBody, FullFileDiffRows, HighlightedFileRows, SurfaceHit,
        cached_expanded_surface_lines, picker_row, render_permission_scroll_window,
        single_line_menu_choice, surface_layout_with_viewport, surface_lines,
    };
    use crate::app::scroll::{ScrollViewAction, ScrollViewHit, ScrollViewState};
    use crate::app::{
        AgentWizardStep, ExpandedView, HelpTab, ListState, MultilineInput, SEARCH_CURSOR_STYLE,
        Surface,
    };
    use cagent_agent::presentation::{
        CodeToken, CodeTokenKind, FileHighlighting, FileView, FileViewContent, FullFileDiffLine,
        FullFileDiffView, HighlightedLine,
    };
    use ratatui::style::{Color, Modifier};
    use std::cell::{Cell, RefCell};
    use std::path::{Path, PathBuf};

    #[test]
    fn expanded_title_keeps_styles_and_sanitizes_single_line_fields() {
        use ratatui::text::{Line, Span};
        let title = super::expanded_title(
            "Bash output",
            Some(Line::from(vec![Span::styled(
                "echo\n\t界\x1b",
                super::ACCENT_STYLE,
            )])),
            [
                Line::default(),
                super::expanded_status_detail("running", Some("2s")),
            ],
            80,
        );
        assert_eq!(
            title.to_string(),
            "  Bash output  echo  界\\u{1b} · running 2s"
        );
        assert_eq!(title.spans[1].style, super::MENU_TITLE_STYLE);
        assert_eq!(title.spans[3].style, super::ACCENT_STYLE);
        assert_eq!(title.spans.last().unwrap().style, super::DIM_STYLE);
        assert_eq!(
            super::expanded_title("Diff", None, [Line::from("0 files")], 80).to_string(),
            "  Diff  0 files"
        );
    }

    #[test]
    fn permission_scroll_renders_the_visible_window_once() {
        let state = ScrollViewState {
            offset: 100,
            content_rows: 1_000,
        };
        let calls = Cell::new(0);

        let layout = render_permission_scroll_window(&state, 5, |start, rows| {
            calls.set(calls.get() + 1);
            assert_eq!((start, rows), (100, 5));
            (start..start + rows)
                .map(|row| ratatui::text::Line::from(format!("row {row}")))
                .collect()
        });

        assert_eq!(calls.get(), 1);
        assert_eq!(layout.lines.len(), 7);
        assert_eq!(layout.lines[1].to_string(), "row 100");
        assert_eq!(layout.lines[5].to_string(), "row 104");
    }

    #[test]
    fn full_file_diff_rows_fill_the_expanded_width_with_their_background() {
        let view = FullFileDiffView {
            path: PathBuf::from("file.rs"),
            language: "rs".into(),
            lines: vec![FullFileDiffLine {
                kind: cagent_agent::tools::DiffLineKind::Addition,
                old_line: None,
                new_line: Some(1),
                tokens: vec![CodeToken {
                    kind: CodeTokenKind::Plain,
                    text: "x".into(),
                }],
            }],
            highlighting: FileHighlighting::Eager,
            added_lines: 1,
            removed_lines: 0,
        };
        let rows = FullFileDiffRows::new(&view, 40).visible_rows(&view, 0, 1);
        assert_eq!(rows[0].line.width(), 40);
        assert_eq!(
            rows[0].line.spans.last().unwrap().style,
            crate::render::DIFF_ADDITION_STYLE
        );
        assert_eq!(
            rows[0].line.spans.last().unwrap().style.bg,
            Some(Color::Rgb(33, 58, 43))
        );
    }

    #[test]
    fn large_highlighted_files_materialize_only_the_visible_rows() {
        use cagent_agent::presentation::{CodeToken, CodeTokenKind, HighlightedLine};

        let lines = (1..=10_000)
            .map(|line| HighlightedLine {
                tokens: vec![CodeToken {
                    kind: CodeTokenKind::Plain,
                    text: format!("let value_{line} = {line};"),
                }],
            })
            .collect::<Vec<_>>();
        let body = ExpandedTextBody::HighlightedFile(HighlightedFileRows::new(
            &lines,
            80,
            "rs",
            cagent_agent::presentation::FileHighlighting::Viewport,
        ));
        assert_eq!(body.len(), 10_000);
        let ExpandedTextBody::HighlightedFile(layout) = body else {
            unreachable!();
        };
        let visible = layout.visible_rows(&lines, 8_999, 20);
        assert_eq!(visible.len(), 20);
        assert!(visible[0].line.to_string().contains("value_9000"));
        assert!(visible[19].line.to_string().contains("value_9019"));
        assert!(
            visible[0]
                .line
                .spans
                .iter()
                .any(|span| span.style == crate::markdown::code_style(CodeTokenKind::Keyword))
        );
        assert_eq!(layout.visible_rows(&lines, 9_000, 20).len(), 20);
    }

    #[test]
    fn file_layout_is_reused_while_the_files_divider_is_dragged() {
        let view = ExpandedView::File {
            view: FileView {
                path: PathBuf::from("large.rs"),
                bytes: 80,
                content: FileViewContent::Text {
                    language: "rs".into(),
                    lines: vec![HighlightedLine {
                        tokens: vec![CodeToken {
                            kind: CodeTokenKind::Plain,
                            text: "a very long source line that wraps at narrow widths".into(),
                        }],
                    }],
                    highlighting: FileHighlighting::Eager,
                },
            },
        };
        let cache = RefCell::new(None);
        let initial =
            cached_expanded_surface_lines(&cache, &view, 0, 8, Path::new("."), 20, 8, false);
        let dragging =
            cached_expanded_surface_lines(&cache, &view, 0, 8, Path::new("."), 80, 8, true);
        let released =
            cached_expanded_surface_lines(&cache, &view, 0, 8, Path::new("."), 80, 8, false);

        assert_eq!(initial, dragging);
        assert_ne!(dragging, released);
    }

    #[test]
    fn picker_rows_fill_the_available_width() {
        assert_eq!(
            picker_row(0, false, "10s ago", "Title", 3, "model", false, 40).width(),
            40
        );
    }

    #[test]
    fn picker_rows_show_message_count_before_model() {
        let row = picker_row(0, false, "10s ago", "Title", 3, "model", false, 40);
        assert_eq!(row.spans[2].content, "3 messages  ");
        assert_eq!(row.spans[3].content, "model");
        assert!(row.spans[2].style.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn favourite_conversation_star_matches_model_favourite_style() {
        let row = picker_row(0, false, "10s ago", "Title", 3, "model", true, 40);
        let star = row.spans.iter().find(|span| span.content == " ★").unwrap();
        assert_eq!(star.style.fg, super::NOTICE_STYLE.fg);
    }

    #[test]
    fn background_rows_are_truncated_to_one_terminal_line() {
        let row = single_line_menu_choice(
            true,
            "general · running",
            "run a long command with enough details to overflow a narrow terminal",
            32,
        );
        assert!(row.width() <= 32);
        assert!(row.to_string().contains('…'));
    }

    #[test]
    fn empty_mcp_tool_states_keep_rendered_lines_and_hits_aligned() {
        for (loading, failure) in [
            (true, None),
            (false, Some("inspection failed".to_owned())),
            (false, None),
        ] {
            let layout = surface_layout_with_viewport(
                &Surface::McpTools {
                    server: "docs".into(),
                    tools: Vec::new(),
                    list: ListState::selectable(0),
                    loading,
                    failure,
                },
                Path::new("/workspace"),
                80,
                usize::MAX,
            );
            assert_eq!(layout.lines.len(), layout.hits.len());
            assert_eq!(layout.lines.len(), 4);
            assert!(layout.lines[1].to_string().is_empty());
            assert!(!layout.lines[2].to_string().is_empty());
            assert!(layout.lines[3].to_string().is_empty());
        }
    }

    #[test]
    fn composed_help_layout_retains_the_scroll_geometry_that_rendered_it() {
        let rows = (0..20)
            .map(|index| (format!("command-{index}"), "details".to_owned()))
            .collect();
        let layout = surface_layout_with_viewport(
            &Surface::Help {
                tab: HelpTab::Commands,
                command_rows: rows,
                key_rows: Vec::new(),
                list: ScrollViewState::new(20),
            },
            Path::new("/workspace"),
            80,
            40,
        );
        let region = layout.scroll_region.expect("help owns a scroll region");
        assert_eq!(region.rows, 3..17);
        assert_eq!(region.metrics.content_rows, 20);
        assert_eq!(region.metrics.capacity, 12);
        assert_eq!(region.metrics.maximum_offset, 8);
    }

    #[test]
    fn input_hits_map_single_line_and_masked_credentials() {
        let rename = Surface::Rename {
            title: "a界b".into(),
            cursor: 0,
        };
        let layout = surface_layout_with_viewport(&rename, Path::new("/workspace"), 80, 12);
        assert_eq!(layout.input_cursor_at(2, 3), Some(1));

        let masked = Surface::WebSearchSetup {
            provider: cagent_agent::web_search::WebSearchProvider::Exa,
            value: "e\u{301}x".into(),
            cursor: 0,
        };
        let layout = surface_layout_with_viewport(&masked, Path::new("/workspace"), 80, 12);
        // The second mask glyph is the source boundary after the first grapheme.
        assert_eq!(layout.input_cursor_at(2, 12), Some("e\u{301}".len()));
        assert!(
            layout.lines[2]
                .spans
                .iter()
                .any(|span| span.style == SEARCH_CURSOR_STYLE)
        );
    }

    #[test]
    fn unbounded_layout_does_not_expand_multiline_input_hits() {
        let surface = Surface::AgentWizard {
            name: String::new(),
            description: String::new(),
            parent: "None".into(),
            parents: vec!["None".into()],
            parent_list: ListState::selectable(1),
            prompt: MultilineInput::new("one\ntwo".into(), 0),
            availability: 0,
            step: AgentWizardStep::Prompt,
            cursor: 0,
            editing: false,
            original_name: None,
        };
        let lines = surface_lines(&surface, Path::new("/workspace"), 80);
        assert_eq!(lines.len(), 5);
    }

    #[test]
    fn multiline_surfaces_expose_the_scroll_widgets_rows_and_hits() {
        let source = (0..10)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let surface = Surface::McpJsonEdit {
            location: cagent_agent::mcp::McpLocation::global(),
            separate_name: None,
            editor: MultilineInput::new(source, 0),
        };
        let layout = surface_layout_with_viewport(&surface, Path::new("/workspace"), 80, 8);

        assert_eq!(layout.lines.len(), 8);
        assert_eq!(layout.surface_hit(1), SurfaceHit::None);
        assert_eq!(
            layout.surface_hit(2),
            SurfaceHit::Scroll(ScrollViewHit::Content(0))
        );
        assert_eq!(
            layout.surface_hit(7),
            SurfaceHit::Scroll(ScrollViewHit::Indicator(ScrollViewAction::PageNext))
        );
        assert_eq!(layout.input_cursor_at(2, 4), Some(2));
        let region = layout.scroll_region.unwrap();
        assert_eq!(region.rows, 1..8);
        assert_eq!(region.metrics.capacity, 5);
    }
}

pub(super) fn menu_title(title: &str, detail: Option<&str>) -> Line<'static> {
    let mut spans = vec![
        Span::raw("  "),
        Span::styled(title.to_owned(), MENU_TITLE_STYLE),
    ];
    if let Some(detail) = detail {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(terminal_safe(detail), MENU_DETAIL_STYLE));
    }
    Line::from(spans)
}

pub(crate) fn menu_choice_line(
    selected: bool,
    label: impl AsRef<str>,
    detail: impl AsRef<str>,
) -> Line<'static> {
    menu_choice_line_with_status(selected, false, label, detail, None)
}

fn plan_completion_choices(
    selected: usize,
    mode: &str,
    model: &str,
    mode_color: StatusLineColor,
    context_percent: Option<u64>,
) -> Vec<Line<'static>> {
    let mode_style = Style::new()
        .fg(status_line_color(mode_color))
        .add_modifier(Modifier::BOLD);
    let choices = [
        ("Yes, with ", "", true, selected == 0),
        ("Yes, with ", " and clear context", false, selected == 1),
        ("Yes, with ", " and compact context", false, selected == 2),
    ];
    let mut lines = choices
        .into_iter()
        .map(|(prefix, suffix, shows_context_usage, is_selected)| {
            let marker = if is_selected { "› " } else { "  " };
            let choice_style = if is_selected {
                ACCENT_STYLE
            } else {
                Style::default()
            };
            let mut spans = vec![
                Span::styled(marker, choice_style),
                Span::styled(prefix, choice_style),
                Span::styled(mode.to_owned(), mode_style),
                Span::styled(" and ", choice_style),
                Span::styled(model.to_owned(), choice_style),
                Span::styled(suffix, choice_style),
            ];
            if shows_context_usage && let Some(percent) = context_percent {
                spans.push(Span::styled(
                    format!(" context used: {percent}%"),
                    DIM_STYLE,
                ));
            }
            Line::from(spans)
        })
        .collect::<Vec<_>>();
    let decline_style = (selected == 3).then_some(ACCENT_STYLE).unwrap_or_default();
    lines.push(Line::from(vec![
        Span::styled(if selected == 3 { "› " } else { "  " }, decline_style),
        Span::styled("No, keep planning", decline_style),
    ]));
    lines
}

/// Compact background work rows must stay one line so picker navigation keeps
/// matching what is visible. Other menus may use the general, unconstrained
/// helper because their labels are intentionally allowed to reflow.
fn single_line_menu_choice(
    selected: bool,
    label: impl AsRef<str>,
    detail: impl AsRef<str>,
    width: u16,
) -> Line<'static> {
    let marker = if selected { "› " } else { "  " };
    let available = usize::from(width).saturating_sub(marker.width());
    let detail_reserve = 12.min(available / 2);
    let label = truncate_with_ellipsis(
        &terminal_safe(label.as_ref()),
        available.saturating_sub(detail_reserve).saturating_sub(2),
    );
    let detail_space = available.saturating_sub(label.width()).saturating_sub(2);
    let detail = truncate_with_ellipsis(&terminal_safe(detail.as_ref()), detail_space);
    let mut spans = vec![Span::styled(
        marker,
        if selected {
            ACCENT_STYLE
        } else {
            Style::default()
        },
    )];
    spans.push(Span::styled(
        label,
        if selected {
            SELECTED_STYLE
        } else {
            Style::default()
        },
    ));
    if !detail.is_empty() {
        spans.push(Span::styled(format!("  {detail}"), DIM_STYLE));
    }
    Line::from(spans)
}

fn menu_choice_line_with_status(
    selected: bool,
    disabled: bool,
    label: impl AsRef<str>,
    detail: impl AsRef<str>,
    status: Option<&str>,
) -> Line<'static> {
    let mut spans = vec![Span::styled(
        if selected { "› " } else { "  " },
        if selected {
            ACCENT_STYLE
        } else {
            Style::default()
        },
    )];
    let label_style = if disabled {
        DIM_STYLE
    } else if selected {
        SELECTED_STYLE
    } else {
        Style::default()
    };
    spans.push(Span::styled(terminal_safe(label.as_ref()), label_style));
    if !detail.as_ref().is_empty() {
        spans.push(Span::styled(
            format!("  {}", terminal_safe(detail.as_ref())),
            DIM_STYLE,
        ));
    }
    if let Some(status) = status {
        spans.push(Span::styled(
            format!(" · {}", terminal_safe(status)),
            provider_status_style(status),
        ));
    }
    Line::from(spans)
}

fn provider_status_style(status: &str) -> Style {
    match status {
        "enabled" | "active" => ENABLED_STYLE,
        "disabled" => NOTICE_STYLE,
        "not setup" | "not set up" => ERROR_STYLE,
        _ => DIM_STYLE,
    }
}

fn mcp_runtime_status_style(status: &cagent_agent::mcp::McpRuntimeStatus) -> Style {
    match status {
        cagent_agent::mcp::McpRuntimeStatus::Connected => ENABLED_STYLE,
        cagent_agent::mcp::McpRuntimeStatus::Disabled
        | cagent_agent::mcp::McpRuntimeStatus::Stopped => NOTICE_STYLE,
        cagent_agent::mcp::McpRuntimeStatus::AuthenticationRequired
        | cagent_agent::mcp::McpRuntimeStatus::Failed { .. } => ERROR_STYLE,
        cagent_agent::mcp::McpRuntimeStatus::NotStarted
        | cagent_agent::mcp::McpRuntimeStatus::Starting
        | cagent_agent::mcp::McpRuntimeStatus::Restarting => DIM_STYLE,
    }
}

fn menu_choice_line_with_mcp_status(
    selected: bool,
    label: impl AsRef<str>,
    status: &cagent_agent::mcp::McpRuntimeStatus,
) -> Line<'static> {
    let label_style = if selected {
        SELECTED_STYLE
    } else {
        Style::default()
    };
    Line::from(vec![
        Span::styled(if selected { "› " } else { "  " }, label_style),
        Span::styled(terminal_safe(label.as_ref()), label_style),
        Span::raw("  "),
        Span::styled(status.to_string(), mcp_runtime_status_style(status)),
    ])
}

fn model_choice(selected: bool, row: &cagent_agent::presentation::ModelPickerRow) -> Line<'static> {
    let mut spans = vec![Span::styled(
        if selected { "› " } else { "  " },
        if selected {
            ACCENT_STYLE
        } else {
            Style::default()
        },
    )];
    spans.push(Span::styled(
        terminal_safe(&row.label),
        if selected {
            SELECTED_STYLE
        } else {
            Style::default()
        },
    ));

    let detail = model_picker_detail(row);
    if row.current {
        let suffix = if row.favourite {
            " · current ★"
        } else {
            " · current"
        };
        let detail = detail
            .strip_suffix(suffix)
            .expect("current model detail should end with its marker");
        spans.push(Span::styled(
            format!("  {}", terminal_safe(detail)),
            DIM_STYLE,
        ));
        spans.push(Span::styled(" · ", DIM_STYLE));
        spans.push(Span::styled("current", Style::default()));
        if row.favourite {
            spans.push(Span::raw(" "));
            spans.push(Span::styled("★", NOTICE_STYLE));
        }
    } else if row.favourite {
        let detail = detail
            .strip_suffix(" · ★")
            .expect("favourite model detail should end with its marker");
        spans.push(Span::styled(
            format!("  {}", terminal_safe(detail)),
            DIM_STYLE,
        ));
        spans.push(Span::styled(" · ", DIM_STYLE));
        spans.push(Span::styled("★", NOTICE_STYLE));
    } else {
        spans.push(Span::styled(
            format!("  {}", terminal_safe(&detail)),
            DIM_STYLE,
        ));
    }
    Line::from(spans)
}

fn scrollable_help_list(
    rows: &[(String, String)],
    width: u16,
    state: &ScrollViewState,
) -> Vec<Line<'static>> {
    ScrollViewWidget::new(VISIBLE_HELP_ITEMS)
        .render(state, |index| {
            let (label, description) = &rows[index];
            let label = terminal_safe(label);
            let prefix_width = 2 + label.width() + 2;
            let description = truncate_with_ellipsis(
                &terminal_safe(description),
                usize::from(width).saturating_sub(prefix_width),
            );
            Line::from(vec![
                Span::raw("  "),
                Span::styled(label, ACCENT_STYLE),
                Span::raw("  "),
                Span::styled(description, DIM_STYLE),
            ])
        })
        .lines
}

fn tab_line<T: Copy>(tabs: impl IntoIterator<Item = (T, String, bool)>) -> TabLayout<T> {
    let mut spans = vec![Span::raw("  ")];
    let mut hits = Vec::new();
    let mut column = 2;
    for (index, (value, label, selected)) in tabs.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw("  "));
            column += 2;
        }
        let text = format!("[ {label} ]");
        let end = column + text.width();
        hits.push((column..end, value));
        spans.push(Span::styled(
            text,
            if selected { SELECTED_STYLE } else { DIM_STYLE },
        ));
        column = end;
    }
    TabLayout {
        line: Line::from(spans),
        hits,
    }
}

fn help_tabs(active: HelpTab) -> TabLayout<HelpTab> {
    tab_line([
        (
            HelpTab::Commands,
            "Commands".to_owned(),
            active == HelpTab::Commands,
        ),
        (
            HelpTab::Keybindings,
            "Keybindings".to_owned(),
            active == HelpTab::Keybindings,
        ),
    ])
}

fn settings_tabs(
    active: cagent_agent::config::SettingSection,
) -> TabLayout<cagent_agent::config::SettingSection> {
    tab_line(
        cagent_agent::config::SettingSection::ALL
            .into_iter()
            .map(|section| (section, section.label().to_owned(), section == active)),
    )
}

fn setting_description_line(description: &str, width: u16) -> Line<'static> {
    truncate_line(
        Line::from(vec![
            Span::raw("  "),
            Span::styled(terminal_safe(description), DIM_STYLE),
        ]),
        usize::from(width),
    )
}

pub(crate) fn list_entry_kind(kind: ListEntryKind) -> String {
    match kind {
        ListEntryKind::Directory => "directory".into(),
        ListEntryKind::File => "file".into(),
        ListEntryKind::Symlink => "symlink".into(),
    }
}
