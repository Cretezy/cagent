---
title: Tools
description: Native tools, background execution, patches, worktrees, delegation, and tool IDs.
---

Cagent exposes a provider-neutral set of native tools to the model. The exact set depends on the
active agent, mode, provider capabilities, web-search setup, and agent tool policy. Explicit
permissions, workspace boundaries, and agent allow/deny rules still apply when a tool is available.

Built-in tools use explicit readable labels in the transcript. Custom tool and sub-agent profile
names are not automatically capitalized or split into words: for example, `explore` appears as
`Running explore sub-agent`, and `code-review` retains its hyphen.

These stable names are also the values accepted by headless `--allow-tool` and `--deny-tool`:

| Tool ID | Purpose |
| --- | --- |
| `change_working_directory` | Change the conversation working directory; see [Working directories and worktrees](#working-directories-and-worktrees). |
| `enter_worktree` | Create or enter a Git worktree or Jujutsu workspace; see [Worktrees](/worktrees/). |
| `apply_patch` | Add, update, move, or delete files; see [Files and diffs](/files-and-diffs/#review-changes). |
| `bash` | Start a supervised shell command; see [Shell and background work](/shell-and-background-work/). |
| `terminal_output` | Read new background-terminal output; see [Supervised terminals](#supervised-terminals). |
| `terminal_write` | Send input to an interactive terminal; see [Background work](/shell-and-background-work/#manage-background-work). |
| `terminal_kill` | Gracefully or forcibly stop a terminal; see [Background work](/shell-and-background-work/#manage-background-work). |
| `delegate_agent` | Give a bounded task to a sub-agent; see [Agents](/agents/#delegate-work). |
| `wait_join` | Wait concurrently for terminal or sub-agent IDs; see [Delegation and questions](#delegation-and-questions). |
| `request_user_input` | Ask multiple-choice questions; see [Composer questions](/composer/#answer-agent-questions). |
| `update_plan` | Replace the active turn's transient progress checklist; see [Task plans](#task-plans). |
| `web_search` | Search a configured provider; see [Search and fetch](/search-and-fetch/#search-the-web). |
| `web_fetch` | Fetch normalized HTTP(S) content; see [Fetch a page](/search-and-fetch/#fetch-a-page). |

`request_user_input` is not usable in headless execution because there is no interactive client to
answer it. `web_search` appears only when a usable search provider is selected; `web_fetch` does not
require a search provider. External MCP tools are dynamic rather than part of this fixed list. Their
provider-safe IDs begin with `mcp__`, followed by sanitized server and tool names; inspect `/mcp` or
the JSON event stream for the exact ID.

## Expanded views

Expanded tool output, sub-agent logs, files, directories, images, diffs, and compacted context use
the same header layout: a bold view title, the subject, then muted details separated by `·`.
When available, the status and elapsed time appear together at the end:

```text
Bash output  cargo test · running 12s
Sub-agent log  general · chatgpt/gpt-6-astra high · running 12s
MCP  github/release · completed 2s
Web Search  rust docs · brave · 3 results
File  src/main.rs · 42 lines · 1.2 KiB
```

Commands keep their syntax colors and diffs keep their addition/deletion colors. Long headers
truncate to the available width. Extra usage or warning information appears directly underneath,
above the shared scroll indicator.

## Task plans

`update_plan` gives the model a Codex-compatible progress checklist during a normal execution turn.
Each call supplies the complete ordered list and replaces the previous snapshot. A step has a
human-readable `step` and a `status` of `pending`, `in_progress`, or `completed`; the optional
`explanation` describes why the plan changed. At most one step should be `in_progress`.

The latest checklist appears at the bottom of the transcript below the working indicator. It is
transient, belongs only to the active primary turn, and disappears when that turn ends. Tool calls
and results remain recorded for model continuation and diagnostics, but they do not create ordinary
tool activity cards. `update_plan` is unavailable to delegated agents and is rejected in planning
modes, where Cagent instead uses the separate proposed-plan workflow.

## Patches and file mutation

`apply_patch` uses one patch envelope containing one or more file operations:

```text
*** Begin Patch
*** Add File: hello.txt
+Hello
*** Update File: src/main.rs
@@ fn main() {
-    println!("old");
+    println!("new");
*** Move to: src/app.rs
*** Delete File: obsolete.txt
*** End Patch
```

Every operation starts with `*** Add File`, `*** Update File`, or `*** Delete File`; `*** Move to`
may follow an update header. New-file content uses `+` lines. Cagent checks the patch and shows the
resulting diff before applying it when permission is required. It also verifies that an existing
file has not changed since the proposed edit was prepared.

Project-file changes should use `apply_patch` rather than shell redirection. This keeps mutation
structured, reviewable, and covered by file permission rules. See [Files and diffs](/files-and-diffs/)
for the user-facing review flow.

## Supervised terminals

`bash` accepts a command, optional working directory, additional environment values, names of
existing environment variables to forward, an optional timeout from 1 to 600 seconds, and a `wait`
choice.

- With `wait = true`, the call returns when the command completes.
- With `wait = false`, it immediately returns a terminal ID.
- `terminal_output` starts at cursor `0` (or no cursor) and returns the next cursor so later reads
  retrieve only new output.
- `terminal_write` supports programs that need interactive input.
- `terminal_kill` first supports graceful termination and can force termination when necessary.
- `wait_join` can wait for multiple background terminals and delegated agents concurrently.

All commands are supervised and visible from `/background`. Safe-command recognition may avoid a
prompt, but it is a permission policy—not an operating-system sandbox. See
[Shell and background work](/shell-and-background-work/) and [Permissions](/permissions/).

## Working directories and worktrees

`change_working_directory` changes the current conversation directory. Relative paths resolve from
the current directory; `.` returns to the directory where the conversation was launched. The launch
directory remains the project used for conversation grouping and external-path decisions, so moving
outside it requires approval.

`enter_worktree` handles both Git worktrees and Jujutsu workspaces directly. The model may provide a
configured name, a custom path, or neither for a generated name, plus a base revision. A missing
base uses the configured default. `fresh` starts from the repository default branch and `head`
starts from the current revision. See [Worktrees](/worktrees/) for interactive and CLI use.

## Delegation and questions

`delegate_agent` runs a bounded supporting task with a configured sub-agent profile. It can wait for
the result immediately or return an ID for `wait_join`; unrelated delegations can therefore proceed
in parallel. Delegated agents keep their own configured tools, MCP access, model, and permissions.
Optional `model` and `provider` arguments override model selection; `effort` overrides reasoning
effort for the run. Omitted or null `effort` keeps the default effort selection.
Permission and question requests return to the main interactive session.

`request_user_input` presents one to three questions with two or three supplied choices each.
Cagent adds **None of the above** so you can provide an alternative note. See
[Answer agent questions](/composer/#answer-agent-questions) for the controls.

## Restrict tools in headless runs

Use repeatable or comma-separated filters:

```sh
cagent exec \
  --allow-tool bash,web_fetch \
  --deny-tool web_fetch \
  "Run the tests and summarize the result"
```

When at least one allow entry is present, every unlisted tool is unavailable. Deny entries remove
matching tools from that allowed set and therefore win when a name appears in both. Tool filtering
does not grant permission: a listed operation must still satisfy mode, permission, trust, and
workspace rules. Headless runs cannot open permission or question prompts, so unresolved operations
are denied.

## Related

- [Permissions](/permissions/) controls whether available native and MCP tools may run.
- [MCP](/mcp/) adds dynamic external tools alongside this fixed native set.
- [Headless execution](/headless/) explains non-interactive behavior and output.
- [Configuration](/configuration/#agents) explains per-agent tool allow and deny lists.
