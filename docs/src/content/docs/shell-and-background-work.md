---
title: Shell and background work
description: Run commands and manage supervised terminals.
---

## Run commands

Type `!` into an empty composer, enter a command, and press `Enter`:

```text
! cargo test
```

Composer `!` commands start detached, so you can keep chatting while they run. They are recorded in
prompt history and appear in `/background`, but their output is not automatically sent to the model.
Agent-run commands use the same supervised terminal service and may wait in the foreground or return
a background ID. Read-only inspection may run automatically; other commands follow your
[permissions](/permissions/).

## Manage background work

Run `/background` or press `Alt+Down` to see active and past terminals and delegated tasks. Press
`Enter` to inspect one and `k` to stop active work. Interactive terminals can receive input while
they are running.

`Esc` from the composer interrupts the active model turn and foreground work. Detached background
terminals and explicitly backgrounded delegated tasks continue unless you stop them from
`/background`. Exiting while terminals are active asks for confirmation; do not assume closing the
conversation has terminated detached processes.

The model uses the same lifecycle through `bash`, `terminal_output`, `terminal_write`,
`terminal_kill`, and `wait_join`. See [Tools](/tools/#supervised-terminals) for wait behavior,
output cursors, environment forwarding, and headless tool IDs.

Terminal output is bounded. By default Cagent retains an 8 MiB terminal buffer and exposes at most
2 MiB to the model; older or excess bytes are reported as discarded or truncated. Use
`terminal_output` with its returned cursor to read incremental output instead of repeatedly asking
for the whole log.

## Configure the shell

```toml
[shell]
executable = "bash"
login = true
timeout_seconds = 600
safe_level = 3
safe_write = false
forward_env = []
```

`safe_level` ranges from `-1` (disabled) to `3` (formatters and fixers). Explicit permission rules
and workspace boundaries always take precedence. See the
[shell configuration reference](/configuration/#shell) for terminal mode, output limits, and all
other fields.

Level 2 includes narrowly parsed local builds such as `cargo build`, package-manager `build`
scripts, `bun build`, `go build`, `dotnet build`, Gradle/Maven builds, and supported language
compile commands. Builds can execute project-controlled scripts or plugins and write artifacts
inside the workspace; this is a prompt exemption, not a sandbox. Install, publish, deploy, run,
clean, unknown/custom build operations, and external paths are not included.

## Related

- [Tools](/tools/#supervised-terminals) documents terminal IDs, cursors, input, and termination.
- [Permissions](/permissions/#safe-commands) explains safe-command classification.
- [Agents](/agents/#delegate-work) covers background delegated work.
- [Headless execution](/headless/) explains non-interactive command output and denial behavior.
