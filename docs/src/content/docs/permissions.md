---
title: Permissions
description: Control which files, commands, websites, and external tools Cagent may access.
---

Cagent checks permissions immediately before an operation. The result is **allow**, **ask**, or
**deny**. Reads, writes, and commands follow the active [mode](/modes/); built-in modes allow
read-only inspection inside the workspace by default.

Permissions reduce accidental or unauthorized operations; they are not an OS sandbox. Allowed
commands and trusted project tools run with the Cagent process's operating-system privileges and can
execute project-controlled code.

## Workspace boundaries

Access outside the workspace always has its own permission boundary. Cagent canonicalizes existing
paths so a symlink cannot silently cross it. A saved rule must set `external = true` to authorize
that crossing, and explicit denies still win.

## Answer a permission request

A prompt shows the operation decision, a separate workspace-boundary decision when applicable,
and the combined result. For file edits it also shows the exact diff. Long previews scroll inside
the prompt while the main view keeps some transcript context. Choose:

- On Bash and web fetch/search prompts, **Allow for this conversation** is the first row;
  press `Tab` to switch it to **Allow once**. Web conversation approval allows every subsequent
  call to that web tool, including matching calls already queued for approval. Fetch and search
  permissions remain separate.
- The second Bash/web row starts at **Allow globally**; press `Tab` to switch it to
  **Allow for this project**. The two rows' scopes are independent.
- Other prompts retain **Allow** for this request/conversation and path-specific or
  **Always allow** choices for project/global rules. Bash external-path prompts retain their
  path-focused **Allow reading** labels.
- **Deny** to reject it. Press `Tab` while Deny is selected to add a note for the agent.

For allow options, use `Tab` to change scope and `e` to edit a prepared rule before saving it. Saved rules take effect
immediately.

For parsed Bash commands, a prepared rule keeps up to the first two command words and adds `*` when
it omits later arguments. Its working-directory pattern covers the active workspace and its
subdirectories, so the same project rule continues to match when a command runs below the project
root. Commands run from outside the workspace keep that exact working directory instead.

## Manage saved rules

Run `/permissions` to edit or delete conversation, project, and global rules. Persistent rules live
in `permissions.toml` beside `config.toml` and reload automatically.

Use `/permissions simulate <bash command>` to inspect parsing, safe-subset classification, matching
rules, filesystem boundaries, and the resulting decisions without executing the command.

```toml
version = 1

[[global.rule]]
id = "deny-env"
effect = "deny"
tool = "apply_patch"
path = "**/.env*"

[[project."/home/me/src/widget".rule]]
id = "allow-tests"
effect = "allow"
tool = "bash"
command = ["cargo", "test", "*"]
```

Rules can match `tool`, `path`, Bash `command`, `cwd`, `mode`, `agent`, or MCP `server` and
`operation`. `*` does not cross path separators; `**` does. A matching deny always wins.

Set `external = true` only when a rule should also authorize crossing the workspace boundary.

## Safe commands

Cagent recognizes a limited set of read-only command forms for tools such as `rg`, `fd`, `cat`,
`sed -n`, and VCS inspection. These can run without prompting when every argument can be proven
read-only and remains inside an authorized path.

Compound commands are classified one executable segment at a time. If only part of a pipeline is
proven safe, Cagent prompts only for the unresolved segments. For example,
`custom-command | sort -nr | head -n 45` asks for `custom-command`; the registered read-only
`sort` and `head` filters do not create additional prompts. Explicit deny rules and filesystem
boundary checks still apply to every segment and path.

Unquoted `*`, `?`, bracket, brace, and `**` patterns are expanded with bounded Bash-compatible
semantics whenever they occur in a registered filesystem-operand position, including commands such
as `wc -l src/*.rs`. Cagent does not enable Bash's `globstar` option implicitly. An entire unquoted
filesystem operand may also come from a constrained plain path-list producer—`fd`, `rg --files`, or
`find` with plain printing—so pipelines such as
`wc -l $(fd -e rs . src) | sort -nr | head -n 45` can qualify. These producers are preflighted with
item, byte, and time bounds; quoted or embedded substitutions, custom-formatted output, and
arbitrary command substitutions still use normal permission handling.

Choose how broad the built-in safety policy should be in `/settings` or TOML:

```toml
[shell]
safe_level = 3 # -1 disables it; 0 reads; 1 checks; 2 builds/tests; 3 formatters/fixers
safe_write = false
```

| Level | Allows without prompting |
| --- | --- |
| `-1` | Nothing; disables the built-in safe-command policy. |
| `0` | Read-only commands that inspect data. |
| `1` | Checks and inspection that do not intentionally edit source or run project code. |
| `2` | Builds, tests, and analysis that may run project code or create artifacts. |
| `3` | Formatters and fixers; writing still requires `safe_write = true`. This is the default. |

Levels are cumulative. `safe_write` separately controls writing formatters and fixers and applies
only when the active mode uses `write = "allow"`. Neither setting overrides explicit denies or external-path
checks. See the [shell configuration reference](/configuration/#shell) for all shell safety
options.

Level 2 and higher allows local scripts such as building and testing through package managers.
Builds may execute project-controlled scripts or plugins and create
workspace-local artifacts, so use level 2 only for projects you trust. Install, publish, deploy,
run, clean, custom executable/plugin injection, unknown options, and external paths still use
normal permission handling.

## Auto mode

In [auto mode](/modes/#auto-mode), unresolved external tool reads, writes, commands, searches,
fetches, and MCP calls are reviewed by the small model. Explicit rules still win. Only a confident `allow` runs automatically; every
other result opens the normal permission prompt. Headless runs cannot open that prompt, so unresolved
operations are denied instead. Auto review is advisory policy, not process isolation.
When auto review falls back to the permission prompt, the prompt shows its explanation and wraps it
to the available terminal width. Ordinary permission prompts do not add internal policy-decision
details.

Compound Bash commands prompt only for parts that still need approval. For example, if
`pnpm run build` is already approved, `pnpm run build && pnpm run migrate` asks only about
`pnpm run migrate`. The auto-review explanation appears on that unresolved command's prompt.
The complete command waits for all required approvals before executing.

## Trust is separate

Workspace trust only controls whether Cagent loads `.cagent/config.toml` and project MCP servers. It
does not grant file, command, network, or tool permissions.

## Related

- [Tools](/tools/) shows how every native tool reaches this permission layer.
- [Modes](/modes/) defines fallback behavior when no explicit rule decides an operation.
- [Shell and background work](/shell-and-background-work/) covers supervised process lifecycle.
- [MCP](/mcp/#permissions) covers dynamic external tool rules.
