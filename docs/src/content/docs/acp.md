---
title: ACP integrations
description: Use Cagent as an Agent Client Protocol server in Zed and other ACP clients.
---

[Agent Client Protocol (ACP)](https://agentclientprotocol.com/) lets an editor host Cagent in its
native agent interface. The editor owns the thread UI while Cagent continues to provide the model,
tools, permissions, instructions, skills, MCP configuration, and persisted conversation.

## Prepare Cagent

Install Cagent and configure a provider first:

```sh
cagent
```

Use `/providers` in the interactive interface to sign in or add an API key, then select a model with
`/model`. The ACP server uses the same configuration and data directory as the normal Cagent
interface. Zed's own provider settings do not configure Cagent.

Find the executable path for the Zed configuration:

```sh
command -v cagent
```

If you use environment variables for provider credentials, they must be available to the process
that launches the ACP server. This matters when Zed is opened from a desktop launcher rather than a
terminal.

## Configure Zed

Open **Agent Settings** with `agent: open settings`, go to **External Agents**, select **Add Agent**,
and choose **Add Custom Agent**. Add Cagent to `agent_servers` using the absolute path reported by
`command -v cagent`:

```json
{
  "agent_servers": {
    "Cagent": {
      "type": "custom",
      "command": "/home/you/.local/bin/cagent",
      "args": ["acp", "--trust"]
    }
  }
}
```

`--trust` temporarily trusts workspaces opened through that ACP server. It allows project
configuration and project MCP servers to load; it does **not** automatically approve file writes,
shell commands, network access, or MCP tool calls. Those operations still follow Cagent's normal
[permission rules](/permissions/).

To avoid trusting every workspace received by this ACP server, omit `--trust` and trust each project
by running `cagent` in it once. ACP cannot show the interactive workspace-trust prompt.

### Use a development build

Build Cagent and point Zed at the resulting binary:

```sh
cargo build --release -p cagent-cli
```

```json
{
  "agent_servers": {
    "Cagent Dev": {
      "type": "custom",
      "command": "/absolute/path/to/cagent/target/release/cagent",
      "args": ["acp", "--trust"]
    }
  }
}
```

Use an absolute path. Zed may not inherit the same `PATH` as your shell.

## Start a Cagent thread

Open Zed's Agent Panel with `agent: new thread` or the agent icon, then choose **Cagent** from the
agent selector. The open Zed project becomes the Cagent workspace.

Each thread is an ordinary persisted Cagent conversation. You can:

- attach files, embedded text, and images to prompts;
- select the Cagent agent, mode, model, and reasoning effort from session controls;
- approve an operation once, allow it for the session, or deny it;
- cancel active turns; and
- load, resume, close, delete, and import persisted conversations.

When Zed advertises terminal support, foreground `bash` calls run in a Zed-managed terminal embedded
in the tool card. Cagent still applies its normal command authorization first.

When the editor advertises ACP file-write support, approved text additions and updates are applied
through the editor. Cagent still plans the patch, checks for stale files and workspace boundaries,
and obtains any required permission before asking the editor to write it.

## Commands in ACP threads

Cagent advertises these prompt commands to ACP clients:

| Command | Purpose |
| --- | --- |
| `/read PROMPT` | Run the prompt in read mode. |
| `/edit PROMPT` | Run the prompt in edit mode. |
| `/auto PROMPT` | Run the prompt in auto mode. |
| `/plan PROMPT` | Run the prompt in plan mode. |
| `/compact [INSTRUCTIONS]` | Compact the conversation context. |
| `/recap` | Generate a conversation recap. |
| `/retry` | Retry the active conversation tip. |
| `/spawn PROMPT` | Delegate a task to a sub-agent. |
| `/web-search QUERY` | Start a dedicated web-search task. |

Enabled Cagent skills also appear as slash commands using each skill's name and description. Any
text after the command is passed to the skill as its optional arguments. Built-in commands take
precedence if a skill has the same name.

Not every terminal-interface command is available through ACP. In particular, configure provider
authentication in the normal Cagent interface.

## MCP servers

Cagent loads its normal global and project MCP configuration. An ACP client may also provide stdio
or streamable-HTTP MCP servers when it creates a session. Client-provided servers apply only to that
session and do not modify Cagent's configuration.

If a server does not appear, check both Zed's MCP settings and Cagent's native
[MCP configuration](/mcp/).

## Other ACP clients

For another ACP v1 client, configure a custom stdio agent with:

```text
command: /absolute/path/to/cagent
arguments: acp --trust
transport: stdio
```

The client must communicate using ACP JSON-RPC over standard input and output. Do not wrap the
command with a script that writes to stdout, because stdout is reserved for protocol messages.

## Troubleshooting

### Cagent does not appear in Zed

- Use an absolute executable path and confirm it with `cagent --version`.
- Check that `agent_servers` is in Zed's top-level settings object.
- Run `dev: open acp logs` from Zed's Command Palette and inspect the launch error.

### A thread fails to start

- Add `--trust`, or trust the project by running Cagent interactively in that workspace first.
- Configure a provider and model in Cagent rather than only in Zed.
- For a remote project, install Cagent and make its configuration and credentials available in the
  environment where Zed launches the external agent.

### Tool calls do not finish

Open `dev: open acp logs` and look for terminal request or permission errors. Confirm that Zed and
Cagent are current, then retry in a new thread. Redact sensitive values before sharing ACP logs.

### Changes in an open editor are missing

Cagent's file reads still come from disk, even when approved text writes are routed through an ACP
client. Save the file or attach/select it so the editor can include its current contents as prompt
context.

## Related

- [Getting started](/getting-started/) covers installation and provider setup.
- [Permissions](/permissions/) explains trust, approval scopes, and permission rules.
- [Conversations](/conversations/) explains persisted sessions and history.
- [CLI arguments](/cli-arguments/#acp) lists the `acp` command.
- [Zed External Agents](https://zed.dev/docs/ai/external-agents) documents Zed's ACP interface.
