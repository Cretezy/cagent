---
title: CLI arguments
description: Launch Cagent, resume conversations, and run subcommands from the command line.
---

Run `cagent --help` or `cagent <command> --help` for the authoritative option list.

## Global options

Global options may appear before or after a subcommand.

| Option | Purpose |
| --- | --- |
| `--dir DIR` | Change directory before resolving the workspace. |
| `-w`, `--worktree[=NAME]` | Create a generated or named worktree; with `continue`, select a named worktree. |
| `--config FILE` | Use another `config.toml`. |
| `--data-dir DIR` | Use another data directory. |
| `-p`, `--prompt PROMPT` | Submit an initial prompt. |
| `--provider PROVIDER` | Override the provider for this session. |
| `--model MODEL` | Override the model; accepts `MODEL` or `PROVIDER/MODEL`. |
| `--agent AGENT` | Select an agent profile. |
| `--mode MODE` | Select a mode. |
| `--effort EFFORT` | Select a supported reasoning effort. |
| `--trust` | Trust the workspace for this launch only. |
| `--archive` | Immediately archive a new persisted conversation. |
| `-h`, `--help` | Show help. |
| `-V`, `--version` | Show the version. |

Without a subcommand, positional words form the new conversation name:

```sh
cagent Dependency audit --prompt "Inspect the dependency graph"
```

## Worktrees and directories

`--dir` is applied before workspace, trust, config, and conversation resolution. Relative paths in
later options therefore resolve from that directory. `-w` without a value creates a generated
worktree; use `--worktree=NAME` when supplying a name so the following positional word is not parsed
as the conversation name. See [Worktrees](/worktrees/) for base and copy policy.

## Conversation commands

| Command | Purpose |
| --- | --- |
| `cagent resume [ID_OR_TITLE]` | Resume an ID or matching title, or open the workspace conversation picker. |
| `cagent continue` | Resume the latest non-empty conversation in this workspace. |

Use `cagent continue --worktree=NAME` to continue the latest conversation recorded in that
worktree. Exact resume always restores the conversation's recorded directory.
Title lookup prefers an exact title, then the most recently updated title containing the query.

## `exec`

```sh
cagent exec [OPTIONS] [PROMPT]
```

If `PROMPT` is omitted, Cagent reads UTF-8 stdin.

| Option | Purpose |
| --- | --- |
| `--output text\|json\|structured` | Select output format. |
| `--delta` | Stream assistant deltas with JSON output. |
| `--output-schema JSON` | Require a final value matching an inline JSON Schema. |
| `--output-schema-file FILE` | Read the JSON Schema from a file. |
| `--allow-tool TOOL` | Restrict the run to named tools; repeat or use commas. |
| `--deny-tool TOOL` | Remove named tools; repeat or use commas. |
| `--no-session` | Do not retain the conversation; conflicts with `--archive`. |
| `--fast` | Request Fast service-tier routing for this run without changing configuration. |
| `-n`, `--name NAME` | Name the persisted exec conversation. |

See [Headless execution](/headless/) for output and permission behavior.
See [Tools](/tools/#restrict-tools-in-headless-runs) for valid native tool names and filter
precedence.

## `acp`

```sh
cagent acp
```

Runs Cagent as an [Agent Client Protocol](https://agentclientprotocol.com/) server over standard
input and output. This mode is intended to be launched by editors such as Zed, not run in an
interactive terminal. Each Zed thread is a persisted Cagent conversation in the workspace supplied
by Zed. Normal Cagent configuration, provider authentication, instructions, tools, and workspace
trust still apply. See [ACP integrations](/acp/) for setup, usage, and troubleshooting.

The ACP frontend supports conversation creation, loading with history replay, listing, deletion,
resume, close, cancellation, attachments, tool and plan updates, and agent/mode/model/reasoning
selectors. Editors also receive command discovery for `/compact [instructions]`, `/recap`, and
prompt-bearing `/read`, `/edit`, `/auto`, and `/plan` commands, as well as `/retry`, `/spawn`, and
`/web-search`; enabled skills are advertised as additional slash commands. Client-provided stdio and HTTP MCP
servers remain session-local. Additional workspace directories are honored by read and mutation
tools. When negotiated, Cagent uses structured form elicitation; older clients receive a
multiple-choice question fallback. ACP filesystem and terminal methods are client primitives, not
user-facing slash commands, and Cagent does not advertise command wrappers for them. When the client
advertises terminal support, ordinary model-generated foreground `bash` tool calls run through the
client terminal after Cagent's normal authorization checks. Bash cards show the actual command and
read-only exploration commands are categorized as reads. When the client advertises file-write
support, authorized text additions and updates are written through the client after Cagent performs
its normal planning, stale-file, boundary, and permission checks. Session listing supports continuation cursors,
and tool results include display content and file locations.

To add a development build to Zed, build Cagent and add this to Zed's `settings.json`, replacing
the command with the absolute path to the built binary:

```json
{
  "agent_servers": {
    "Cagent": {
      "type": "custom",
      "command": "/absolute/path/to/cagent",
      "args": ["acp", "--trust"]
    }
  }
}
```

Open Zed's Agent Panel and select **Cagent** for a new thread. Use `dev: open acp logs` from Zed's
Command Palette to inspect protocol traffic. The integration exposes agent, mode, model, and
reasoning-effort selectors; accepts file, embedded-text, and image attachments; streams assistant
messages, tool calls, and plans; loads persisted sessions with history replay; supports cancellation;
and presents Cagent permission requests in Zed with allow-once, allow-for-session, and deny choices.

## Configuration commands

```text
cagent config view [--safe]
cagent config get KEY
cagent config set KEY VALUE
cagent config edit
cagent config list
```

`view --safe` redacts sensitive values from the resolved view. `get` and `set` use documented dotted
configuration keys; `edit` opens the configured editor and keeps the previous valid configuration
active if validation fails. See [Configuration](/configuration/) for files, value shapes, and live
reload.

## MCP commands

```text
cagent mcp add [NAME] [--scope global|project] [--url URL | --json [FILE]] [--yes] [-- COMMAND...]
cagent mcp add [NAME] --package PACKAGE [--catalog MANIFEST_URL] [--scope global|project]
cagent mcp list [--all] [--json]
cagent mcp get NAME [--json]
cagent mcp update NAME [--yes]
cagent mcp detach NAME [--yes]
cagent mcp oauth connect NAME [--scope global|project]
cagent mcp oauth status NAME [--scope global|project]
cagent mcp oauth disconnect NAME [--scope global|project]
cagent mcp enable NAME [--scope global|project]
cagent mcp disable NAME [--scope global|project]
cagent mcp remove NAME [--scope global|project] [--yes]
```

Project-scoped changes target `.cagent/config.toml` and require workspace trust before the server is
loaded. `--json [FILE]` on `add` reads a JSON server definition from the optional file or stdin.
Review commands, environment inheritance, headers, and URL security before accepting imported
configuration. See [MCP](/mcp/) for server configuration and examples.

`--package` corresponds to **Add from catalog** in `/mcp`; omit `--catalog` for a bundled package.
URL manifests are cached and SHA-256 locked. `update` previews and explicitly accepts changed
package content. `detach` is **Convert to custom MCP**: it writes the currently resolved server and
removes the package relationship.

## Related

- [Getting started](/getting-started/) covers installation and the first interactive launch.
- [Headless execution](/headless/) covers JSON Lines, schemas, permissions, and exit codes.
- [Conversations](/conversations/) covers resume-title matching and ownership.
- [Tools](/tools/) lists stable tool IDs used by exec filters.
