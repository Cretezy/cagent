---
title: Customize the interface
description: Configure the theme, status line, progress, editor, and keybindings.
---

Run `/settings` for common options. Type to search across every settings tab, and press `Ctrl+R` on
an overridden setting to restore its default.

See [Status line](/status-line/) to choose, reorder, and recolor status modules.

## Theme and progress

```toml
[ui]
theme = "dark" # or "light"
show_tips = true
collapse_tool_activity = true
open_links = true
progress_osc = true

[ui.colors]
highlight = "cyan"
muted = "#9CA3AF"
```

By default, `collapse_tool_activity` replaces consecutive reads, directory listings, local and web
searches, web fetches, edits, and Bash commands with one dim, live-updating summary. Pending edits
are collapsed from their first frame, so their mutation card does not briefly appear before the
completed diff. Click the summary to show the original cards in a surface-colored box; click it
again to hide them. This also applies inside expanded sub-agent logs, with a blank line between
cards. Sub-agent expansion reuses the rendered log instead of reparsing it on every toggle. Set it
to `false` to always show the original cards.

Terminal-title progress and requested-input indicators can also be selected from `/settings`.
See [interface and color settings](/configuration/#interface-and-files) for all UI fields and color
values.

Requested-input and completed-turn notifications are enabled by default. Choose OSC 777 desktop
notifications or terminal BEL under `ui.bell`; actual delivery depends on terminal and operating
system support. Terminal-title progress can use a spinner, while `progress_osc` controls OSC progress
reporting. Disable these when logs, multiplexers, or accessibility tools do not handle escape
sequences well.

## Web links

With `ui.open_links = true` (the default), a single left click on an HTTP(S) Markdown link opens
your default browser. This includes bare HTTP(S) URLs in Markdown prose. Toggle **Open web links**
in `/settings`, or set `open_links = false` under `[ui]`, to disable app-handled web-link clicks.
Terminal OSC 8 links remain available through your terminal's own link-opening gesture when
supported. Local file clicks are separate and still follow `ui.editor`.

## Keybindings

Run `/help` to see the resolved bindings. Override actions with arrays of chords:

```toml
[keys]
insert_newline = ["shift+enter", "ctrl+enter"]
model_picker = ["alt+p"]
```

Use `[]` to unbind an action. See [Files and diffs](/files-and-diffs/#configure-the-path-editor) for
editor and sidebar configuration, or the
[keybinding configuration reference](/configuration/#keybindings) for every action.

Run `/help` after editing bindings to check the final resolved keys. If overrides assign one chord
to multiple actions, Cagent reports the conflict and keeps the complete defaults for the affected
actions rather than choosing one unpredictably.

## Related

- [Status line](/status-line/) covers modules, responsive hiding, and colors.
- [Composer](/composer/#useful-keys) explains the most common editing actions.
- [Files and diffs](/files-and-diffs/#configure-the-path-editor) configures viewers and editors.
- [Configuration](/configuration/#interface-and-files) lists all UI defaults and limits.
