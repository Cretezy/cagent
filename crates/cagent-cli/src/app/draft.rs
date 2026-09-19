//! Composer draft editing, chips, completion, and prompt-history navigation.

#![allow(clippy::wildcard_imports)]

use std::collections::HashSet;

use super::editor::ComposerEditor;
use super::{
    App, AttachmentChip, AttachmentCompletion, ChipRange, CompletionResult, DraftSnapshot,
    HistoryEntry, ListEntry, ListEntryKind, ListMode, ListState, PasteChip, SLASH_COMMANDS,
    SlashSuggestion, VISIBLE_MENU_ITEMS, adjust_chip_ranges, move_scroll_offset, split_command,
    trim_outer_blank_lines,
};

impl App {
    pub(super) fn begin_scroll_to_transcript_block(
        &mut self,
        target: cagent_agent::protocol::TranscriptBlockId,
    ) {
        self.pending_transcript_scroll_target = Some(target);
        self.finish_pending_transcript_scroll();
    }

    pub(super) fn finish_pending_transcript_scroll(&mut self) {
        let Some(target) = self.pending_transcript_scroll_target.clone() else {
            return;
        };
        if let Some(offset) = self.transcript_block_offset(&target) {
            self.history_scroll = offset.saturating_sub(1);
            self.follow_history_tail = false;
            self.transcript_home_drain = false;
            self.transcript_prefetch_armed = false;
            self.pending_transcript_scroll_target = None;
        } else if self.transcript_older.is_some() {
            self.scroll_history_to_top();
        } else {
            self.pending_transcript_scroll_target = None;
            self.transcript_home_drain = false;
        }
    }

    pub(super) fn apply_completion(&mut self, result: CompletionResult) {
        if self.is_read_only_view() {
            return;
        }
        let Some(completion) = &mut self.attachment_completion else {
            return;
        };
        if completion.start != result.request.start
            || completion.end != result.request.end
            || self.draft.get(completion.start..completion.end) != Some(result.request.raw.as_str())
        {
            return;
        }
        match result.rows {
            Ok(rows) => {
                completion.rows = rows;
                completion.list.reconcile(
                    ListMode::Selectable,
                    completion.rows.len(),
                    VISIBLE_MENU_ITEMS,
                );
            }
            Err(error) => self.set_notice(format!("path completion failed · {error}")),
        }
    }

    pub(super) fn accept_attachment_completion(&mut self) {
        if self.is_read_only_view() {
            return;
        }
        let Some(completion) = self.attachment_completion.take() else {
            return;
        };
        let Some(entry) = completion
            .list
            .selected
            .and_then(|selected| completion.rows.get(selected))
            .cloned()
        else {
            self.attachment_completion = Some(completion);
            return;
        };
        self.apply_path_completion(completion.start, &entry);
    }

    pub(super) fn apply_path_completion(&mut self, token_start: usize, entry: &ListEntry) {
        let path = entry.path.to_string_lossy();
        let token = if path.contains(char::is_whitespace) {
            format!("@{{{path}}}")
        } else {
            format!("@{path}")
        };
        self.replace_range(token_start, self.cursor, &format!("{token} "));
        let end = token_start + token.len();
        self.confirm_attachment(token_start, end, entry.kind);
        self.cursor = end + 1;
    }

    pub(super) fn accept_slash_command(&mut self) -> Option<SlashSuggestion> {
        if self.is_read_only_view() {
            let command = self.selected_slash_command()?;
            self.replace_observer_command(&command.name);
            self.observer_command_dismissed = true;
            return Some(command);
        }
        let command = self.selected_slash_command()?;
        let draft = if command.insert_argument_hint {
            command.argument_hint.map_or_else(
                || command.name.clone(),
                |hint| format!("{} {hint}", command.name),
            )
        } else {
            command.name.clone()
        };
        self.replace_draft(&draft);
        self.slash_dismissed = true;
        Some(command)
    }

    pub(super) fn selected_slash_command(&self) -> Option<SlashSuggestion> {
        let selected = if self.is_read_only_view() {
            self.observer_command_list.selected
        } else {
            self.slash_list.selected
        }?;
        self.slash_suggestions().get(selected).cloned()
    }

    pub(crate) fn valid_slash_command_end(&self) -> Option<usize> {
        let input = self.slash_input();
        let (command, _) = split_command(input);
        (SLASH_COMMANDS.iter().any(|candidate| {
            (!self.is_read_only_view() || candidate.observer_safe)
                && (candidate.name != "/cleanup" || self.manual_cleanup_available)
                && (candidate.name == command || candidate.alias == Some(command))
        }) || (!self.is_read_only_view() && command == "/bg"))
            .then_some(command.len())
            .or_else(|| {
                if self.is_read_only_view() {
                    return None;
                }
                self.mode_commands
                    .iter()
                    .any(|mode| command == format!("/{mode}"))
                    .then_some(command.len())
            })
    }

    /// Whether the draft is a complete command that Enter can run directly.
    ///
    /// Commands with required arguments remain completion-driven so Enter can
    /// insert the command followed by a space instead of showing a usage error.
    pub(super) fn has_exact_runnable_slash_command(&self) -> bool {
        if self.is_read_only_view() {
            return false;
        }
        let input = self.slash_input();
        let (command, argument) = split_command(input);
        if argument.is_some() || command != input {
            return false;
        }
        SLASH_COMMANDS.iter().any(|candidate| {
            (candidate.name == command || candidate.alias == Some(command))
                && (candidate.name != "/cleanup" || self.manual_cleanup_available)
                && !candidate
                    .argument_hint
                    .is_some_and(|hint| hint.starts_with('<'))
        }) || command == "/bg"
            || self
                .mode_commands
                .iter()
                .any(|mode| command.strip_prefix('/') == Some(mode))
    }

    pub(crate) fn slash_suggestions(&self) -> Vec<SlashSuggestion> {
        if self.composer_mode == super::ComposerMode::Bash {
            return Vec::new();
        }
        let input = self.slash_input();
        let dismissed = if self.is_read_only_view() {
            self.observer_command_dismissed
        } else {
            self.slash_dismissed
        };
        if dismissed || !input.starts_with('/') {
            return Vec::new();
        }
        if let Some(argument) = input.strip_prefix("/diff ") {
            return [
                ("conversation", "show recorded successful patches"),
                ("git", "show the Git or Jujutsu working-copy diff"),
                ("clear", "reset conversation diff tracking only"),
            ]
            .into_iter()
            .filter(|(name, _)| {
                name.starts_with(argument) && (!self.is_read_only_view() || *name != "clear")
            })
            .map(|(name, description)| SlashSuggestion {
                name: format!("/diff {name}"),
                alias: None,
                argument_hint: None,
                insert_argument_hint: false,
                description: description.into(),
            })
            .collect();
        }
        if input.chars().any(char::is_whitespace) {
            return Vec::new();
        }
        let mut suggestions = SLASH_COMMANDS
            .into_iter()
            .filter(|command| !self.is_read_only_view() || command.observer_safe)
            .filter(|command| command.name != "/cleanup" || self.manual_cleanup_available)
            .filter(|command| {
                command.name.starts_with(input)
                    || command.alias.is_some_and(|alias| alias.starts_with(input))
            })
            .map(|command| SlashSuggestion {
                name: command.name.into(),
                alias: command.alias,
                argument_hint: command.argument_hint,
                insert_argument_hint: command.insert_argument_hint,
                description: command.description.into(),
            })
            .collect::<Vec<_>>();
        if self.is_read_only_view() {
            suggestions.sort_by(|left, right| left.name.cmp(&right.name));
            return suggestions;
        }
        suggestions.extend(self.mode_commands.iter().filter_map(|mode| {
            let name = format!("/{mode}");
            let command_safe = !mode.chars().any(char::is_whitespace);
            let collides = SLASH_COMMANDS
                .iter()
                .any(|command| command.name == name || command.alias == Some(name.as_str()));
            (command_safe && !collides && name.starts_with(&self.draft)).then(|| SlashSuggestion {
                name,
                alias: None,
                argument_hint: Some("[message]"),
                insert_argument_hint: false,
                description: format!("switch to {mode} mode and optionally send a message"),
            })
        }));
        suggestions.extend(
            self.skill_commands
                .iter()
                .filter_map(|(skill, description)| {
                    let name = format!("/{skill}");
                    name.starts_with(&self.draft).then(|| SlashSuggestion {
                        name,
                        alias: None,
                        argument_hint: Some("[arguments]"),
                        insert_argument_hint: false,
                        description: description.clone(),
                    })
                }),
        );
        suggestions.sort_by_key(|command| {
            self.recent_commands
                .iter()
                .position(|recent| *recent == command.name)
                .unwrap_or(usize::MAX)
        });
        let mut seen = HashSet::new();
        suggestions.retain(|command| seen.insert(command.name.clone()));
        suggestions
    }

    pub(super) fn record_executed_command(
        &mut self,
        command: impl Into<SlashSuggestion>,
        text: &str,
    ) {
        let command = command.into();
        self.recent_commands
            .retain(|recent| *recent != command.name);
        self.recent_commands.insert(0, command.name);
        self.record_history_entry(text.to_owned(), Vec::new());
    }

    pub(super) fn record_history_entry(
        &mut self,
        text: String,
        attachments: Vec<cagent_agent::protocol::AttachmentSpec>,
    ) {
        self.history_entries.retain(|entry| {
            entry.kind != cagent_agent::protocol::ComposerInputKind::Prompt
                || entry.text != text
                || entry.attachment_specs != attachments
                || !entry.images.is_empty()
                || !entry.image_chips.is_empty()
        });
        self.history_entries.push(HistoryEntry {
            kind: cagent_agent::protocol::ComposerInputKind::Prompt,
            text,
            attachment_specs: attachments,
            images: Vec::new(),
            image_chips: Vec::new(),
        });
    }

    pub(super) fn normalize_slash(&mut self) {
        // Editing or replacing the completion projection invalidates any
        // previously armed mouse target, even if the new list reuses its row.
        self.last_picker_click = None;
        if self.is_read_only_view() {
            let Some(input) = self.observer_command_text() else {
                return;
            };
            if !input.starts_with('/') {
                self.clear_observer_command();
                return;
            }
            let count = self.slash_suggestions().len();
            self.observer_command_list
                .reconcile(ListMode::Selectable, count, VISIBLE_MENU_ITEMS);
            return;
        }
        if !self.draft.starts_with('/') {
            self.slash_dismissed = false;
        }
        let count = self.slash_suggestions().len();
        self.slash_list
            .reconcile(ListMode::Selectable, count, VISIBLE_MENU_ITEMS);
    }

    pub(super) fn insert_paste(&mut self, value: &str) {
        if self.is_observer() {
            return;
        }
        if let Some(chip) = self
            .pastes
            .iter_mut()
            .find(|chip| !chip.range.expanded && chip.range.end == self.cursor)
        {
            chip.range.expanded = true;
            return;
        }
        let start = self.cursor;
        self.insert_text(value);
        let lines = value.lines().count().max(1);
        if lines > 20 || value.len() > 2 * 1024 {
            self.pastes.push(PasteChip {
                range: ChipRange::new(start, self.cursor),
                lines,
                bytes: value.len(),
            });
        }
    }

    pub(super) fn activate_observer_command(&mut self) {
        if !self.is_read_only_view() {
            return;
        }
        self.observer_command_editor = Some(super::editor::ComposerEditor::new("/"));
        self.observer_command_list
            .reset(ListMode::Selectable, self.slash_suggestions().len());
        self.observer_command_dismissed = false;
        self.exit_armed = None;
    }

    pub(super) fn clear_observer_command(&mut self) {
        self.observer_command_editor = None;
        self.observer_command_list.reset(ListMode::Selectable, 0);
        self.observer_command_dismissed = false;
        self.exit_armed = None;
    }

    pub(super) fn replace_observer_command(&mut self, value: &str) {
        if !self.is_read_only_view() {
            return;
        }
        let Some(editor) = self.observer_command_editor.as_mut() else {
            return;
        };
        editor.set_text(value);
        self.observer_command_dismissed = false;
        self.normalize_slash();
    }

    #[cfg(test)]
    pub(super) fn insert_observer_command_text(&mut self, value: &str) {
        let Some(editor) = self.observer_command_editor.as_mut() else {
            return;
        };
        editor.insert(value);
        self.observer_command_dismissed = false;
        self.normalize_slash();
    }

    #[cfg(test)]
    pub(super) fn delete_observer_command_range(&mut self, start: usize, end: usize) {
        let Some(editor) = self.observer_command_editor.as_mut() else {
            return;
        };
        editor.delete_range(start, end);
        if editor.text().is_empty() || !editor.text().starts_with('/') {
            self.clear_observer_command();
        } else {
            self.normalize_slash();
        }
    }

    // These small test helpers keep regression coverage on the same editor
    // implementation as keyboard input.
    #[cfg(test)]
    pub(super) fn delete_observer_command_previous_word(&mut self) {
        let Some(editor) = self.observer_command_editor.as_mut() else {
            return;
        };
        editor.delete_previous_word();
        self.normalize_observer_command_editor();
    }

    #[cfg(test)]
    pub(super) fn undo_observer_command(&mut self) {
        let Some(editor) = self.observer_command_editor.as_mut() else {
            return;
        };
        editor.undo();
        self.normalize_observer_command_editor();
    }

    #[cfg(test)]
    pub(super) fn move_observer_command_word_left(&mut self) {
        if let Some(editor) = self.observer_command_editor.as_mut() {
            editor.move_word_left();
        }
    }

    #[cfg(test)]
    pub(super) fn move_observer_command_word_right(&mut self) {
        if let Some(editor) = self.observer_command_editor.as_mut() {
            editor.move_word_right();
        }
    }

    #[cfg(test)]
    fn normalize_observer_command_editor(&mut self) {
        if self
            .observer_command_text()
            .is_some_and(|text| text.is_empty() || !text.starts_with('/'))
        {
            self.clear_observer_command();
        } else {
            self.observer_command_dismissed = false;
            self.normalize_slash();
        }
    }

    fn slash_input(&self) -> &str {
        if self.is_read_only_view() {
            self.observer_command_text().unwrap_or_default()
        } else {
            &self.draft
        }
    }

    fn owner_editor(&self) -> ComposerEditor {
        ComposerEditor::from_parts(self.draft.clone(), self.cursor, self.preferred_column)
    }

    fn update_owner_from_editor(&mut self, editor: ComposerEditor) {
        editor.text().clone_into(&mut self.draft);
        self.cursor = editor.cursor();
        self.preferred_column = editor.preferred_column();
    }

    pub(super) fn insert_text(&mut self, value: &str) {
        if self.is_observer() {
            return;
        }
        if value.is_empty() {
            return;
        }
        self.snap_cursor_out_of_atomic_chip();
        self.checkpoint_undo();
        let added = value.len();
        for chip in &mut self.attachments {
            if self.cursor <= chip.range.start {
                chip.range.start += added;
                chip.range.end += added;
            } else if self.cursor < chip.range.end {
                chip.range.end += added;
                chip.range.expanded = true;
            }
        }
        for chip in &mut self.pastes {
            if self.cursor <= chip.range.start {
                chip.range.start += added;
                chip.range.end += added;
            } else if self.cursor < chip.range.end {
                chip.range.end += added;
                chip.range.expanded = true;
            }
        }
        for chip in &mut self.images {
            if self.cursor <= chip.range.start {
                chip.range.start += added;
                chip.range.end += added;
            }
        }
        let start = self.cursor;
        let mut editor = self.owner_editor();
        editor.insert(value);
        self.update_owner_from_editor(editor);
        self.exit_armed = None;
        if value.contains(char::is_whitespace) {
            self.attachment_completion = None;
        }
        if self
            .attachment_completion
            .as_ref()
            .is_some_and(|completion| self.draft[completion.start..self.cursor].starts_with("@@"))
        {
            self.attachment_completion = None;
        }
        if value.contains('@')
            && (start == 0
                || self.draft[..start]
                    .chars()
                    .next_back()
                    .is_some_and(char::is_whitespace))
            && value == "@"
        {
            self.attachment_completion = Some(AttachmentCompletion {
                start,
                end: self.cursor,
                rows: Vec::new(),
                list: ListState::selectable(0),
            });
        } else if let Some(completion) = &mut self.attachment_completion {
            completion.end = self.cursor;
        }
        self.normalize_slash();
        self.last_composer_input_at = Some(std::time::Instant::now());
    }

    pub(super) fn replace_draft(&mut self, draft: &str) {
        if self.is_observer() {
            return;
        }
        self.undo_stack.clear();
        draft.clone_into(&mut self.draft);
        self.cursor = self.draft.len();
        self.attachments.clear();
        self.pastes.clear();
        self.images.clear();
        self.attachment_completion = None;
        self.history_index = None;
        self.history_scratch = None;
        self.recalling_cleared_draft = false;
        self.slash_dismissed = false;
        self.normalize_slash();
        self.last_composer_input_at = Some(std::time::Instant::now());
    }

    pub(super) fn replace_draft_preserving_images(&mut self, draft: &str) {
        let images = self
            .images
            .iter()
            .map(|chip| chip.image.clone())
            .collect::<Vec<_>>();
        self.replace_draft(draft);
        for image in images {
            let label = format!("[Image #{}]", image.number);
            let mut offset = 0;
            while let Some(relative) = self.draft[offset..].find(&label) {
                let start = offset + relative;
                let end = start + label.len();
                self.images.push(super::ImageChip {
                    image: image.clone(),
                    range: ChipRange::new(start, end),
                });
                offset = end;
            }
        }
        self.images.sort_by_key(|chip| chip.range.start);
    }

    pub(super) fn replace_draft_with_attachment_specs(
        &mut self,
        draft: &str,
        specs: &[cagent_agent::protocol::AttachmentSpec],
    ) {
        if self.is_observer() {
            return;
        }
        self.replace_draft(draft);
        self.restore_attachment_specs(specs);
    }

    pub(super) fn clear_draft(&mut self) {
        if !self.is_observer() {
            self.replace_draft("");
            self.preferred_column = None;
            self.exit_armed = None;
            return;
        }
        self.undo_stack.clear();
        self.draft.clear();
        self.cursor = 0;
        self.attachments.clear();
        self.pastes.clear();
        self.images.clear();
        self.attachment_completion = None;
        self.history_index = None;
        self.history_scratch = None;
        self.slash_dismissed = false;
        self.normalize_slash();
        self.last_composer_input_at = None;
        self.preferred_column = None;
        self.exit_armed = None;
    }

    /// Clears the composer while retaining one local-only snapshot for the
    /// next history recall. Ordinary draft clearing intentionally does not use
    /// this path.
    pub(super) fn clear_draft_for_recall(&mut self) {
        if !self.is_observer() && !self.draft.is_empty() {
            self.cleared_draft = Some(self.snapshot());
        }
        self.clear_draft();
    }

    /// Drops the local-only cleared-draft recall slot after a message has been
    /// accepted for submission.
    pub(super) fn discard_cleared_draft(&mut self) {
        self.cleared_draft = None;
        self.recalling_cleared_draft = false;
    }

    pub(super) fn checkpoint_undo(&mut self) {
        if self.undo_stack.len() >= 100 {
            self.undo_stack.remove(0);
        }
        self.undo_stack.push(DraftSnapshot {
            mode: self.composer_mode,
            draft: self.draft.clone(),
            cursor: self.cursor,
            attachments: self.attachments.clone(),
            pastes: self.pastes.clone(),
            images: self.images.clone(),
        });
    }

    pub(super) fn undo_last_edit(&mut self) {
        if self.is_observer() {
            return;
        }
        if let Some(snapshot) = self.undo_stack.pop() {
            self.draft = snapshot.draft;
            self.cursor = snapshot.cursor;
            self.attachments = snapshot.attachments;
            self.pastes = snapshot.pastes;
            self.images = snapshot.images;
            self.attachment_completion = None;
            self.last_composer_input_at = Some(std::time::Instant::now());
        }
    }

    pub(super) fn backspace(&mut self) {
        if self.is_observer() {
            return;
        }
        if self.cursor == 0 {
            return;
        }
        if let Some((start, end)) = self.collapsed_chip_at(self.cursor) {
            self.delete_range(start, end);
            return;
        }
        if let Some((start, end)) = self.owner_editor().backspace_range() {
            self.delete_range(start, end);
        }
    }

    pub(super) fn delete_forward(&mut self) {
        if self.is_observer() {
            return;
        }
        if let Some((start, end)) = self.collapsed_chip_at(self.cursor) {
            self.delete_range(start, end);
            return;
        }
        if let Some((start, end)) = self.owner_editor().delete_forward_range() {
            self.delete_range(start, end);
        }
    }

    pub(super) fn delete_range(&mut self, start: usize, end: usize) {
        if self.is_observer() {
            return;
        }
        let (start, end) = self.expand_range_over_atomic_chips(start, end);
        if start >= end {
            return;
        }
        let active_completion = self.attachment_completion.clone();
        self.checkpoint_undo();
        let mut editor = self.owner_editor();
        editor.delete_range(start, end);
        self.update_owner_from_editor(editor);
        let removed = end - start;
        adjust_chip_ranges(&mut self.attachments, start, end, removed, |chip| {
            &mut chip.range
        });
        adjust_chip_ranges(&mut self.pastes, start, end, removed, |chip| {
            &mut chip.range
        });
        adjust_chip_ranges(&mut self.images, start, end, removed, |chip| {
            &mut chip.range
        });
        self.cursor = start;
        self.preferred_column = None;
        self.attachment_completion = active_completion.and_then(|mut completion| {
            if end <= completion.start {
                completion.start -= removed;
                completion.end -= removed;
            } else if start < completion.end {
                completion.end = completion.end.saturating_sub(removed).max(completion.start);
                completion.list.reconcile(
                    ListMode::Selectable,
                    completion.rows.len(),
                    VISIBLE_MENU_ITEMS,
                );
            }
            self.draft
                .get(completion.start..completion.end)
                .filter(|raw| raw.starts_with('@') && !raw.contains(char::is_whitespace))
                .map(|_| completion)
        });
        self.normalize_slash();
        self.last_composer_input_at = Some(std::time::Instant::now());
    }

    pub(super) fn replace_range(&mut self, start: usize, end: usize, replacement: &str) {
        let (start, end) = self.expand_range_over_atomic_chips(start, end);
        self.checkpoint_undo();
        let mut editor = self.owner_editor();
        editor.delete_range(start, end);
        editor.insert(replacement);
        self.update_owner_from_editor(editor);
        adjust_chip_ranges(&mut self.attachments, start, end, end - start, |chip| {
            &mut chip.range
        });
        adjust_chip_ranges(&mut self.pastes, start, end, end - start, |chip| {
            &mut chip.range
        });
        adjust_chip_ranges(&mut self.images, start, end, end - start, |chip| {
            &mut chip.range
        });
        self.cursor = start + replacement.len();
        self.attachment_completion = None;
        self.normalize_slash();
        self.last_composer_input_at = Some(std::time::Instant::now());
    }

    pub(super) fn confirm_attachment(&mut self, start: usize, end: usize, kind: ListEntryKind) {
        if self.is_observer() {
            return;
        }
        let Ok(references) = cagent_agent::parse_attachment_references(&self.draft) else {
            return;
        };
        if let Some(reference) = references
            .into_iter()
            .find(|reference| reference.start == start && reference.end == end)
        {
            self.attachments
                .retain(|chip| chip.range.start != start || chip.range.end != end);
            self.attachments.push(AttachmentChip {
                spec: reference.spec,
                kind,
                range: ChipRange::new(start, end),
            });
            self.attachments.sort_by_key(|chip| chip.range.start);
        }
    }

    pub(super) fn confirmed_attachments(&self) -> Vec<cagent_agent::protocol::AttachmentSpec> {
        self.attachments
            .iter()
            .map(|chip| chip.spec.clone())
            .collect()
    }

    pub(super) fn insert_image(&mut self, mut image: cagent_agent::protocol::ImageAttachment) {
        self.snap_cursor_out_of_atomic_chip();
        if let Some(existing) = self
            .images
            .iter()
            .find(|chip| chip.image.sha256 == image.sha256)
        {
            image = existing.image.clone();
        }
        let label = format!("[Image #{}]", image.number);
        let adjacent = self.images.iter().any(|chip| {
            chip.image.id == image.id
                && (chip.range.end == self.cursor || chip.range.start == self.cursor)
        });
        if adjacent {
            return;
        }
        self.checkpoint_undo();
        let start = self.cursor;
        let mut editor = self.owner_editor();
        editor.insert(&label);
        self.update_owner_from_editor(editor);
        let added = label.len();
        for chip in &mut self.attachments {
            if start <= chip.range.start {
                chip.range.start += added;
                chip.range.end += added;
            }
        }
        for chip in &mut self.pastes {
            if start <= chip.range.start {
                chip.range.start += added;
                chip.range.end += added;
            }
        }
        for chip in &mut self.images {
            if start <= chip.range.start {
                chip.range.start += added;
                chip.range.end += added;
            }
        }
        self.images.push(super::ImageChip {
            image,
            range: ChipRange::new(start, start + added),
        });
        self.images.sort_by_key(|chip| chip.range.start);
        self.last_composer_input_at = Some(std::time::Instant::now());
    }

    pub(super) fn next_image_number(&self) -> u64 {
        let used = self
            .images
            .iter()
            .map(|chip| chip.image.number)
            .collect::<std::collections::HashSet<_>>();
        (1..)
            .find(|number| !used.contains(number))
            .unwrap_or(u64::MAX)
    }

    pub(super) fn user_draft(&self) -> cagent_agent::protocol::UserDraft {
        let mut unique = Vec::new();
        for chip in &self.images {
            if !unique
                .iter()
                .any(|image: &cagent_agent::protocol::ImageAttachment| image.id == chip.image.id)
            {
                unique.push(chip.image.clone());
            }
        }
        cagent_agent::protocol::UserDraft {
            text: self.draft.clone(),
            attachment_specs: self.confirmed_attachments(),
            images: unique,
            image_chips: self
                .images
                .iter()
                .map(|chip| cagent_agent::protocol::ImageChipRange {
                    image_id: chip.image.id,
                    start: chip.range.start,
                    end: chip.range.end,
                })
                .collect(),
        }
    }

    pub(super) fn restore_failed_submission(&mut self, draft: cagent_agent::protocol::UserDraft) {
        if !self.draft.is_empty() {
            return;
        }
        self.replace_draft_with_attachment_specs(&draft.text, &draft.attachment_specs);
        let images = draft
            .images
            .into_iter()
            .map(|image| (image.id, image))
            .collect::<std::collections::HashMap<_, _>>();
        self.images = draft
            .image_chips
            .into_iter()
            .filter_map(|range| {
                images
                    .get(&range.image_id)
                    .cloned()
                    .map(|image| super::ImageChip {
                        image,
                        range: ChipRange::new(range.start, range.end),
                    })
            })
            .collect();
    }

    pub(crate) fn attachment_is_active(&self, chip: &AttachmentChip) -> bool {
        self.attachment_completion
            .as_ref()
            .is_some_and(|completion| completion.start == chip.range.start)
    }

    pub(super) fn collapsed_chip_at(&self, cursor: usize) -> Option<(usize, usize)> {
        if let Some(chip) = self.attachments.iter().find(|chip| {
            !chip.range.expanded
                && !self.attachment_is_active(chip)
                && cursor > chip.range.start
                && cursor <= chip.range.end
        }) {
            return Some((chip.range.start, chip.range.end));
        }
        self.pastes
            .iter()
            .find(|chip| {
                !chip.range.expanded && cursor > chip.range.start && cursor <= chip.range.end
            })
            .map(|chip| (chip.range.start, chip.range.end))
            .or_else(|| {
                self.images
                    .iter()
                    .find(|chip| cursor > chip.range.start && cursor <= chip.range.end)
                    .map(|chip| (chip.range.start, chip.range.end))
            })
    }

    pub(super) fn activate_chip_at_cursor(&mut self) -> bool {
        if let Some(chip) = self.attachments.iter().find(|chip| {
            !chip.range.expanded && self.cursor >= chip.range.start && self.cursor <= chip.range.end
        }) {
            self.attachment_completion = Some(AttachmentCompletion {
                start: chip.range.start,
                end: chip.range.end,
                rows: Vec::new(),
                list: ListState::selectable(0),
            });
            self.cursor = chip.range.end;
            self.preferred_column = None;
            return true;
        }
        if let Some(chip) = self.pastes.iter_mut().find(|chip| {
            !chip.range.expanded && self.cursor >= chip.range.start && self.cursor <= chip.range.end
        }) {
            chip.range.expanded = true;
            self.cursor = chip.range.end;
            self.preferred_column = None;
            return true;
        }
        false
    }

    pub(super) fn move_left(&mut self) {
        if let Some(start) = self.collapsed_chip_start_at(self.cursor) {
            self.cursor = start;
        } else {
            let mut editor = self.owner_editor();
            editor.move_left();
            self.update_owner_from_editor(editor);
        }
        self.preferred_column = None;
    }
    pub(super) fn move_right(&mut self) {
        if let Some(end) = self.collapsed_chip_end_at(self.cursor) {
            self.cursor = end;
        } else {
            let mut editor = self.owner_editor();
            editor.move_right();
            self.update_owner_from_editor(editor);
        }
        self.preferred_column = None;
    }
    pub(super) fn move_word_left(&mut self) {
        if let Some(start) = self.collapsed_chip_start_at(self.cursor) {
            self.cursor = start;
        } else {
            let mut editor = self.owner_editor();
            editor.move_word_left();
            self.update_owner_from_editor(editor);
            self.snap_cursor_to_atomic_chip_boundary(false);
        }
        self.preferred_column = None;
    }
    pub(super) fn move_word_right(&mut self) {
        if let Some(end) = self.collapsed_chip_end_at(self.cursor) {
            self.cursor = end;
        } else {
            let mut editor = self.owner_editor();
            editor.move_word_right();
            self.update_owner_from_editor(editor);
            self.snap_cursor_to_atomic_chip_boundary(true);
        }
        self.preferred_column = None;
    }
    pub(super) fn move_to_line_start(&mut self) {
        let mut editor = self.owner_editor();
        editor.move_to_line_start();
        self.update_owner_from_editor(editor);
    }
    pub(super) fn move_to_line_end(&mut self) {
        let mut editor = self.owner_editor();
        editor.move_to_line_end();
        self.update_owner_from_editor(editor);
    }
    pub(super) fn delete_previous_word(&mut self) {
        let mut editor = self.owner_editor();
        let end = self.cursor;
        editor.move_word_left();
        self.delete_range(editor.cursor(), end);
    }

    fn atomic_chip_ranges(&self) -> Vec<(usize, usize)> {
        let mut ranges = self
            .attachments
            .iter()
            .filter(|chip| !chip.range.expanded && !self.attachment_is_active(chip))
            .map(|chip| (chip.range.start, chip.range.end))
            .chain(
                self.pastes
                    .iter()
                    .filter(|chip| !chip.range.expanded)
                    .map(|chip| (chip.range.start, chip.range.end)),
            )
            .chain(
                self.images
                    .iter()
                    .map(|chip| (chip.range.start, chip.range.end)),
            )
            .collect::<Vec<_>>();
        ranges.sort_unstable();
        ranges
    }

    fn expand_range_over_atomic_chips(&self, mut start: usize, mut end: usize) -> (usize, usize) {
        let ranges = self.atomic_chip_ranges();
        if start == end
            && let Some((_, chip_end)) = ranges
                .iter()
                .find(|(chip_start, chip_end)| start > *chip_start && start < *chip_end)
        {
            return (*chip_end, *chip_end);
        }
        loop {
            let mut changed = false;
            for &(chip_start, chip_end) in &ranges {
                if start < chip_end && end > chip_start {
                    let expanded_start = start.min(chip_start);
                    let expanded_end = end.max(chip_end);
                    changed |= expanded_start != start || expanded_end != end;
                    start = expanded_start;
                    end = expanded_end;
                }
            }
            if !changed {
                return (start, end);
            }
        }
    }

    fn snap_cursor_out_of_atomic_chip(&mut self) {
        self.snap_cursor_to_atomic_chip_boundary(true);
    }

    fn snap_cursor_to_atomic_chip_boundary(&mut self, toward_end: bool) {
        if let Some((start, end)) = self
            .atomic_chip_ranges()
            .into_iter()
            .find(|(start, end)| self.cursor > *start && self.cursor < *end)
        {
            self.cursor = if toward_end { end } else { start };
        }
    }

    pub(super) fn collapsed_chip_start_at(&self, cursor: usize) -> Option<usize> {
        self.attachments
            .iter()
            .find(|chip| {
                !chip.range.expanded
                    && !self.attachment_is_active(chip)
                    && cursor > chip.range.start
                    && cursor <= chip.range.end
            })
            .map(|chip| chip.range.start)
            .or_else(|| {
                self.pastes
                    .iter()
                    .find(|chip| {
                        !chip.range.expanded
                            && cursor > chip.range.start
                            && cursor <= chip.range.end
                    })
                    .map(|chip| chip.range.start)
            })
            .or_else(|| {
                self.images
                    .iter()
                    .find(|chip| cursor > chip.range.start && cursor <= chip.range.end)
                    .map(|chip| chip.range.start)
            })
    }

    pub(super) fn collapsed_chip_end_at(&self, cursor: usize) -> Option<usize> {
        self.attachments
            .iter()
            .find(|chip| {
                !chip.range.expanded
                    && !self.attachment_is_active(chip)
                    && cursor >= chip.range.start
                    && cursor < chip.range.end
            })
            .map(|chip| chip.range.end)
            .or_else(|| {
                self.pastes
                    .iter()
                    .find(|chip| {
                        !chip.range.expanded
                            && cursor >= chip.range.start
                            && cursor < chip.range.end
                    })
                    .map(|chip| chip.range.end)
            })
            .or_else(|| {
                self.images
                    .iter()
                    .find(|chip| cursor >= chip.range.start && cursor < chip.range.end)
                    .map(|chip| chip.range.end)
            })
    }

    pub(super) fn normalized_submission_text(&self) -> String {
        let mut text = String::new();
        let mut offset = 0;
        for chip in &self.attachments {
            if chip.range.start < offset || chip.range.end > self.draft.len() {
                continue;
            }
            text.push_str(&self.draft[offset..chip.range.start]);
            if !text.is_empty() && !text.chars().next_back().is_some_and(char::is_whitespace) {
                text.push(' ');
            }
            text.push_str(&self.draft[chip.range.start..chip.range.end]);
            if self.draft[chip.range.end..]
                .chars()
                .next()
                .is_some_and(|character| !character.is_whitespace())
            {
                text.push(' ');
            }
            offset = chip.range.end;
        }
        text.push_str(&self.draft[offset..]);
        trim_outer_blank_lines(&text)
    }

    pub(super) fn move_vertical(&mut self, down: bool) -> bool {
        let width = self.render_width;
        let (_, _, composer_rows, _) = self.control_heights_within(width, self.render_height);
        let capacity = usize::from(composer_rows.saturating_sub(1)).max(1);
        let viewport = self.composer_viewport_layout(width, capacity);
        let Some((line_index, column)) = viewport.caret else {
            return false;
        };
        let target = if down {
            Some(line_index + 1)
        } else {
            line_index.checked_sub(1)
        };
        let Some(target) = target.filter(|target| *target < viewport.ranges.len()) else {
            return false;
        };
        let desired = self.preferred_column.unwrap_or(column);
        let Some(target_offset) = viewport.rendered_offset_at(target, desired) else {
            return false;
        };
        self.cursor = viewport.source_offset_at(target_offset);
        self.preferred_column = Some(desired);
        self.composer_scroll = self.composer_viewport_layout(width, capacity).state;
        true
    }

    pub(super) fn begin_history_navigation(&mut self) {
        let Some(index) = self.history_entries.len().checked_sub(1) else {
            return;
        };
        if self.history_scratch.is_none() {
            self.history_scratch = Some(self.snapshot());
        }
        self.recall_history(index);
    }

    pub(super) fn recall_previous_history(&mut self) {
        if let Some(index) = self.history_index {
            self.recall_history(index.saturating_sub(1));
        } else if self.recalling_cleared_draft {
            let Some(index) = self.history_entries.len().checked_sub(1) else {
                return;
            };
            self.recalling_cleared_draft = false;
            self.recall_history(index);
        } else if let Some(snapshot) = self.cleared_draft.clone() {
            if self.history_scratch.is_none() {
                self.history_scratch = Some(self.snapshot());
            }
            self.restore_snapshot(snapshot);
            self.recalling_cleared_draft = true;
            self.slash_dismissed = true;
        } else {
            self.begin_history_navigation();
        }
    }

    pub(super) fn recall_history(&mut self, index: usize) {
        if let Some(entry) = self.history_entries.get(index).cloned() {
            let scratch = self.history_scratch.take();
            self.composer_mode = if entry.kind == cagent_agent::protocol::ComposerInputKind::Bash {
                super::ComposerMode::Bash
            } else {
                super::ComposerMode::Prompt
            };
            self.replace_draft_with_attachment_specs(&entry.text, &entry.attachment_specs);
            let metadata = entry
                .images
                .iter()
                .map(|image| (image.id, image))
                .collect::<std::collections::HashMap<_, _>>();
            self.images = entry
                .image_chips
                .iter()
                .filter_map(|range| {
                    metadata.get(&range.image_id).map(|image| super::ImageChip {
                        image: (*image).clone(),
                        range: ChipRange::new(range.start, range.end),
                    })
                })
                .collect();
            self.history_scratch = scratch;
            self.history_index = Some(index);
            self.slash_dismissed = true;
        }
    }

    pub(super) fn restore_attachment_specs(
        &mut self,
        attachment_specs: &[cagent_agent::protocol::AttachmentSpec],
    ) {
        let Ok(references) = cagent_agent::parse_attachment_references(&self.draft) else {
            return;
        };
        let mut used = vec![false; references.len()];
        self.attachments = attachment_specs
            .iter()
            .filter_map(|attachment| {
                let (index, reference) = references
                    .iter()
                    .enumerate()
                    .find(|(index, reference)| !used[*index] && reference.spec == *attachment)?;
                used[index] = true;
                let path = if attachment.path.is_absolute() {
                    attachment.path.clone()
                } else {
                    self.workspace.join(&attachment.path)
                };
                let kind =
                    std::fs::symlink_metadata(path).map_or(ListEntryKind::File, |metadata| {
                        if metadata.file_type().is_symlink() {
                            ListEntryKind::Symlink
                        } else if metadata.is_dir() {
                            ListEntryKind::Directory
                        } else {
                            ListEntryKind::File
                        }
                    });
                Some(AttachmentChip {
                    spec: reference.spec.clone(),
                    kind,
                    range: ChipRange::new(reference.start, reference.end),
                })
            })
            .collect();
        self.attachments.sort_by_key(|chip| chip.range.start);
    }

    pub(super) fn recall_next_history(&mut self) {
        if self.history_index.is_none() && self.recalling_cleared_draft {
            self.recalling_cleared_draft = false;
            if let Some(snapshot) = self.history_scratch.take() {
                self.restore_snapshot(snapshot);
            }
            return;
        }
        let Some(index) = self.history_index else {
            return;
        };
        if index + 1 < self.history_entries.len() {
            self.recall_history(index + 1);
        } else if let Some(snapshot) = self.cleared_draft.clone() {
            self.restore_snapshot(snapshot);
            self.recalling_cleared_draft = true;
            self.slash_dismissed = true;
        } else if let Some(snapshot) = self.history_scratch.take() {
            self.restore_snapshot(snapshot);
        } else {
            self.history_index = None;
        }
    }

    pub(super) fn snapshot(&self) -> DraftSnapshot {
        DraftSnapshot {
            mode: self.composer_mode,
            draft: self.draft.clone(),
            cursor: self.cursor,
            attachments: self.attachments.clone(),
            pastes: self.pastes.clone(),
            images: self.images.clone(),
        }
    }
    pub(super) fn restore_snapshot(&mut self, snapshot: DraftSnapshot) {
        self.composer_mode = snapshot.mode;
        self.draft = snapshot.draft;
        self.cursor = snapshot.cursor;
        self.attachments = snapshot.attachments;
        self.pastes = snapshot.pastes;
        self.images = snapshot.images;
        self.history_index = None;
        self.slash_dismissed = false;
        self.last_composer_input_at = Some(std::time::Instant::now());
    }

    pub(super) fn scroll_history_up(&mut self, amount: usize) {
        let was_following_tail = self.follow_history_tail;
        let transcript_is_empty = self.history.is_empty()
            && self.streaming_source.is_empty()
            && self.streaming_plan_source.is_empty();
        self.arm_transcript_prefetch_if_idle();
        self.prepare_history_scroll_up(amount);
        let prepared_scroll = self.history_scroll;
        move_scroll_offset(
            &mut self.history_scroll,
            usize::MAX,
            -isize::try_from(amount).unwrap_or(isize::MAX),
        );
        // An upward gesture over existing content is an explicit request to
        // stop following, even when that content does not overflow yet. This
        // prevents later streaming rows from snapping the viewport to the
        // tail. Preserve the no-op behavior only for an empty conversation.
        if was_following_tail && transcript_is_empty && self.history_scroll == prepared_scroll {
            self.follow_history_tail = true;
        }
    }
    pub(super) fn scroll_history_down(&mut self, amount: usize) {
        move_scroll_offset(
            &mut self.history_scroll,
            usize::MAX,
            isize::try_from(amount).unwrap_or(isize::MAX),
        );
        if self
            .transcript_scrollbar
            .is_none_or(|layout| self.history_scroll >= layout.maximum)
        {
            self.follow_history_tail = true;
        }
    }

    pub(super) fn scroll_history_to_top(&mut self) {
        // Home is the explicit full-history action. Finish the frontend-local
        // loaded layout before draining older durable pages so offset zero is
        // the true beginning of the current window.
        self.ensure_history_layout(self.render_width);
        self.follow_history_tail = false;
        self.history_scroll = 0;
        self.transcript_prefetch_armed = false;
        self.transcript_home_drain = self.transcript_older.is_some();
    }

    pub(super) fn scroll_history_to_end(&mut self) {
        self.transcript_home_drain = false;
        self.transcript_prefetch_armed = false;
        self.follow_history_tail = false;
        // `usize::MAX` records explicit End navigation. The render pass
        // clamps it to the current maximum and restores tail-following.
        self.history_scroll = usize::MAX;
    }

    /// Moves the transcript viewport between submitted user messages.
    ///
    /// User cards are identified from the width-aware layout rather than the
    /// source items, so a target remains correct when a message wraps.
    pub(super) fn navigate_user_message(&mut self, down: bool) -> bool {
        if down && self.follow_history_tail {
            return false;
        }

        self.ensure_history_layout(self.render_width);
        let rows = &self
            .history_layout
            .rendered
            .as_ref()
            .expect("history layout is initialized")
            .rows;
        let user_offsets = rows
            .iter()
            .enumerate()
            .filter(|(index, row)| {
                row.line.style == super::USER_STYLE
                    && (*index == 0 || rows[*index - 1].line.style != super::USER_STYLE)
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if user_offsets.is_empty() {
            return false;
        }
        let current = if self.follow_history_tail {
            usize::MAX
        } else {
            self.history_scroll
        };

        let target = if down {
            // A selected card begins one row below the scroll origin.
            user_offsets
                .into_iter()
                .find(|offset| *offset > current.saturating_add(1))
        } else {
            user_offsets
                .into_iter()
                .rev()
                .find(|offset| *offset < current)
        };
        if let Some(target) = target {
            // Leave one visual row above the user card when the transcript
            // has room, so navigation does not pin the card to the viewport.
            self.history_scroll = target.saturating_sub(1);
            self.follow_history_tail = false;
        } else if down {
            self.scroll_history_to_end();
            self.follow_history_tail = true;
        } else {
            self.scroll_history_to_top();
        }
        true
    }
}
