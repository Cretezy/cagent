---
title: Instructions
description: Give Cagent persistent guidance for all projects or one workspace.
---

Instruction files tell Cagent how you want it to work: coding conventions, commands to run, project
architecture, or team policies. Use global instructions for personal preferences and workspace
instructions for project-specific guidance.
You can create or update these files yourself, or ask the agent to manage instructions for you.

## Create instructions

Add `AGENTS.md` to your workspace:

```markdown
# Project guidelines

- Run `cargo fmt` after editing Rust.
- Keep user-facing parsing outside the frontend crate.
- Update the docs when behavior changes.
```

Create global instructions at the platform path:

| OS | Global instructions |
| --- | --- |
| Linux | `${XDG_CONFIG_HOME:-~/.config}/cagent/AGENTS.md` |
| macOS | `~/.config/cagent/AGENTS.md` |
| Windows | `%APPDATA%\cagent\AGENTS.md` |

With `--config FILE`, place global instructions beside that file instead.

Use `AGENTS.override.md` at either scope for higher-priority personal or temporary instructions.
Workspace overrides should be ignored by your version-control system. Cagent applies native files
in this order, with later files winning:

1. Global `AGENTS.md`
2. Global `AGENTS.override.md`
3. Workspace `AGENTS.md`
4. Workspace `AGENTS.override.md`

Trust separately controls project configuration and MCP servers. Instructions guide model behavior;
they do not grant file, shell, network, or MCP permissions.

## Compatibility

Cagent also reads compatible Claude, Codex, and OpenCode instruction files by default. Native
`AGENTS.md` files take precedence within the same scope. Disable compatible instructions and skill
roots with:

```toml
[compatibility]
external_agents = false
```

See [compatibility settings](/configuration/#context-retention-skills-and-worktrees) for the
configuration reference.

## Reload instructions

Cagent watches instruction files and applies valid changes at the next request or tool boundary.
Run `/reload` to reload immediately. If an edit is invalid or unreadable, Cagent keeps the last
valid version and reports the affected path.

Instructions are included in model context but hidden from the normal transcript.

## Related

- [Skills](/skills/) packages reusable task-specific workflows and supporting files.
- [Agents](/agents/) adds profile-specific instructions, tools, and defaults.
- [Configuration](/configuration/#context-retention-skills-and-worktrees) controls compatibility.
- [Permissions](/permissions/#trust-is-separate) explains why guidance is not authorization.
