---
title: Skills
description: Add reusable workflows that Cagent can activate for matching tasks.
---

A skill is a directory containing `SKILL.md`. Its description tells Cagent when to use it; the body
contains the workflow. Skills can also include scripts, references, and other supporting files.
You can manage skills yourself with `/skills`, or ask the agent to create, update, enable, disable,
or otherwise manage skills for you.

## Manage skills

Run `/skills` to view, enable, disable, create, edit, or delete skills. Project skills travel with a
workspace; global skills are available everywhere. Bundled system skills cannot be edited or
deleted.

Enabled skills also become slash commands. For example, `/release-check v1.2.0` activates a skill
named `release-check` and passes `v1.2.0` as its arguments.

## Create a skill

Run `/skills`, choose **Add**, and select **Project** or **Global**. Enter a name, description, and
workflow content. Cagent creates the skill directory and its `SKILL.md` file in the selected scope,
then reloads skills so the new one is immediately available.

You can also create the files manually. For example, create
`.cagent/skills/release-check/SKILL.md` in a project:

```markdown
---
name: release-check
description: Verify release artifacts and changelogs.
---

Check the release against references/checklist.md.
Use $ARGUMENTS as the requested version.
```

`description` is required. `name` defaults to the containing directory. Relative references resolve
from the skill directory, and supporting files are read only when needed.

Keep the main workflow concise. Put detailed background in a `references/` directory and repeatable
automation in `scripts/`; refer to those files from `SKILL.md` with relative paths. The agent reads
only the supporting material needed for the current task, subject to normal filesystem permissions
and canonical-path checks.

Proven read-only Bash commands can inspect discovered skill directories without external-path
approval, including when combined with other commands. Each command is checked independently:
writes, unclassified commands, and symlinks escaping trusted skill roots still require normal
permissions.

Global native skills use these platform directories:

| OS | Global skills |
| --- | --- |
| Linux | `${XDG_CONFIG_HOME:-~/.config}/cagent/skills` |
| macOS | `~/.config/cagent/skills` |
| Windows | `%APPDATA%\cagent\skills` |

With `--config FILE`, use `skills` beside that file. Cagent also scans these native locations:

- `~/.cagent/skills`
- `<workspace>/skills`
- `<workspace>/.cagent/skills`

Compatible Agent, Claude, Codex, and OpenCode skill directories are also scanned when
`compatibility.external_agents` is enabled.
That switch controls both compatible instruction roots and compatible skill roots.

## Configure skills

Skills are enabled by default. Disable names or bundled skills in TOML:

```toml
[skills]
disabled = ["release-check"]

[skills.bundled]
enabled = false
```

Cagent watches skill directories. Run `/reload` if a new or edited skill does not appear. See the
[skills configuration reference](/configuration/#context-retention-skills-and-worktrees) for all
skill settings.

## Related

- [Instructions](/instructions/) supplies persistent guidance without a task-triggered workflow.
- [Agents](/agents/) defines reusable profiles that can invoke skills.
- [Composer](/composer/#slash-commands) explains skill slash commands.
- [Configuration](/configuration/#context-retention-skills-and-worktrees) lists discovery settings.
