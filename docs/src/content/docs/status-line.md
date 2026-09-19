---
title: Status line
description: Choose the live session details shown below the composer.
---

The status line summarizes the active session below the composer. It updates as the mode, agent,
model, context usage, and costs change.

## Customize interactively

Run `/statusline` to open the editor. It includes a live preview.

- Press `Enter` to show or hide the selected module.
- Press `Left` or `Right` to reorder it.
- Press `c` to choose a named color or enter `#RRGGBB`.
- Press `r` to reset that module's color.
- Press `Esc` to save and close.

If you enable a module listed after an enabled `hint`, the editor moves `hint` immediately after
that module. You can still move `hint` anywhere afterward with `Left` or `Right`.

## Modules

| Module | Shows |
| --- | --- |
| `mode` | Active permission and behavior mode. |
| `agent` | Active non-default agent profile. |
| `model` | Model and reasoning effort. |
| `provider` | Model provider. |
| `fast` | `fast` while the selected model's Fast service tier is active; otherwise hidden. |
| `provider_usage` | Provider-reported remaining limits. |
| `context` | Context-window percentage. |
| `cost` | Complete session cost. |
| `tokens` | Session input and output tokens. |
| `token_rate` | Average input and output tokens per second over the trailing 60 seconds of active model work. |
| `cache` | Session cache-read rate. |
| `cache_tokens` | Cache read and write tokens. |
| `context_tokens` | Context-window tokens used. |
| `hint` | Contextual composer shortcuts and combined background-terminal/sub-agent count. |

Some values appear only when relevant or available from the provider. On narrow terminals, Cagent
removes or shortens lower-priority segments to keep the line usable; enabling a module does not
guarantee it is visible at every width.

`token_rate` is disabled by default. When enabled, it displays provider-reported usage such as
`in:123/s out:71/s` and updates when the provider reports new token usage. Its trailing 60-second
clock advances only while the conversation is actively working, resets when you change models, and is hidden
when both rates are zero. It also resets when Fast mode becomes effective or stops being effective, so
the displayed average does not mix Standard and Fast requests. It uses reported request usage rather
than estimating tokens from streamed text.

## Configure with TOML

Set the visible modules in display order:

```toml
[ui.statusline]
modules = [
  "mode",
  "agent",
  "model",
  "provider",
  "fast",
  "context",
  "provider_usage",
  "cost",
  "token_rate",
  "hint",
]

[ui.statusline.colors]
agent = "green"
model = "light-cyan"
provider = "magenta"
fast = "red"
context = "yellow"
provider_usage = "green"
cost = "light-yellow"
token_rate = "gray"
hint = "dark-gray"
```

Colors accept `#RRGGBB` or a named terminal color. The `mode` module uses the active mode's color;
configure that under `[modes.<name>]`. See
[Configuration](/configuration/#colors-title-bell-and-statusline) for the complete color list.

## Related

- [Usage and costs](/usage-and-costs/) defines local totals and provider-reported limits.
- [Context and compaction](/context-and-compaction/) explains the context estimate.
- [Customize the interface](/customize-interface/) covers themes, titles, bells, and keys.
- [Configuration](/configuration/#colors-title-bell-and-statusline) lists modules and colors.
