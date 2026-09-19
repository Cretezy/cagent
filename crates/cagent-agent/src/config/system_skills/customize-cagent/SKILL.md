---
name: customize-cagent
description: Use when the user asks Cagent to customize or configure itself, including config.toml, providers, agents, modes, permissions, MCP servers, UI, shell, local instructions, or skills. Do not use for ordinary application code.
---

# Customizing Cagent

Cagent uses versioned TOML. The examples below cover common settings, but they are a summary rather
than a complete schema. Preserve unrelated settings and comments, and do not guess an unfamiliar
field's shape.

## Inspecting and applying changes

When inspecting configuration through the CLI, call only the secret-safe form:

```sh
cagent config view --safe
```

Do not call the unredacted `config view`, and do not expose credentials or secret values in tool
output. When the user asks Cagent to change its own configuration, edit `config.toml` directly with
`apply_patch`; do not use `cagent config set` or `cagent config edit`. Preserve
comments and unrelated keys. You may suggest those interactive commands for the user to run
themselves when useful.

Most valid edits hot-reload at the next safe boundary. You can tell the config was automatically reloaded after doing a change. A change to process environment or launch
arguments requires a restart.

## Where files live

| Scope | Path |
| --- | --- |
| Linux global config | `${XDG_CONFIG_HOME:-~/.config}/cagent/config.toml` |
| macOS global config | `~/.config/cagent/config.toml` |
| Windows global config | `%APPDATA%\cagent\config.toml` |
| Global instructions | `<config-dir>/AGENTS.md` and `AGENTS.override.md` |
| Workspace instructions | `<workspace>/AGENTS.md` and `AGENTS.override.md` |
| Global skills | `<config-dir>/skills/<name>/SKILL.md` |
| Workspace skills | `<workspace>/skills/<name>/SKILL.md` or `.cagent/skills/<name>/SKILL.md` |
| Global permissions | `<config-dir>/permissions.toml` |

## Common configuration

Every file needs `version = 1`. All sections are optional. This compact example demonstrates the
most common shapes:

```toml
version = 1
default_agent = "general"
default_mode = "edit"
default_plan_exit_mode = "auto"
default_model = { model = "anthropic/claude-sonnet-4-6", effort = "high" }

[tiers]
small = [{ model = "anthropic/claude-haiku-4-5", effort = "low" }]

[subagents]
strategy = "complex" # off, on_demand, complex, aggressive, or always
max_concurrent = 10

[compaction]
enabled = true
threshold_percent = 90

[ui]
show_tips = true
diff_context_lines = 3
editor = "vscode" # false, a known preset, or a command definition

[ui.bell]
requested_input = true
completed_turn = true
method = "osc777" # or "bell"

[shell]
terminal_mode = "normal" # or "dumb"
safe_level = 3
safe_write = false

[providers.anthropic]
enabled = true
api_key_env = "ANTHROPIC_API_KEY" # environment variable name, never the key itself

[agents.reviewer]
extends = "general"
description = "Review changes without editing"
availability = "both"
model = { tier = "small" }

[agents.reviewer.tools]
allow = ["bash"]

[modes.review]
enabled = true
cycleable = true
description = "Read-only review"
write = "deny"
run = "ask"

[skills.bundled]
enabled = true

[compatibility]
external_agents = true
```

Provider IDs include `chatgpt`, `github-copilot`, `openai`, `anthropic`, `openrouter`, `google`,
`google-vertex`, and `opencode`; custom providers live under `[providers.custom.<id>]` and require
their provider type and endpoint settings. API keys belong in environment variables or Cagent's
credential flow, not directly in TOML.

Agents are tables keyed by name. They may inherit with `extends`, select a structured `model`
target and optional effort, select a `mode`, append a prompt, and restrict tools or permissions. Modes are also keyed by
name and control unresolved reads, writes, and commands through `read = "allow" | "ask" | "auto" | "deny"`,
`write = "allow" | "ask" | "auto" | "deny"`, and
`run = "allow" | "ask" | "auto" | "deny"`. Permission rules can be declared inline as arrays of
tables or globally in `permissions.toml`.

MCP servers are configured in Cagent's MCP section or with the `cagent mcp` commands. Before
adding one, inspect CLI help or an existing entry to confirm the current local/remote shape.

## Skills and instructions

A skill is a directory containing `SKILL.md` with YAML frontmatter. `description` is required and
should say what the skill does and when it activates. Supporting scripts, references, and assets
remain in that skill directory.

Cagent's bundled skills live in `<config-dir>/skills/.system`. Do not edit that cache because a
fingerprint refresh replaces it. Disable the entire bundle with:

```toml
[skills.bundled]
enabled = false
```

External-agent instruction and skill roots are independently controlled with:

```toml
[compatibility]
external_agents = false
```

When an exact option is not covered here, inspect the safe effective config, CLI help, and existing
configuration types before proposing an edit.
