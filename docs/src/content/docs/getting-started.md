---
title: Getting started
description: Install Cagent and send your first request.
---

## Install

Install the latest release on Linux or macOS:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://raw.githubusercontent.com/Cretezy/cagent/main/scripts/install.sh | sh
```

The installer detects Intel/AMD or Arm systems, downloads the matching binary from the latest
GitHub release, verifies its SHA-256 checksum, and installs it to `~/.local/bin`. It explains how to
add that directory to `PATH` when needed.

Set `CAGENT_BIN_DIR` to use another destination. To build and install from the latest `main` instead:

```sh
curl https://raw.githubusercontent.com/Cretezy/cagent/main/scripts/install-from-source.sh | bash
```

To install your current checkout, run `./scripts/install-from-local.sh`.

## Start Cagent

Open the project you want to work on and run:

```sh
cd my-project
cagent
```

The first time you open a workspace, Cagent asks whether you trust it. Trust enables project
configuration and MCP servers.

The onboarding screen will then guide you through choosing a provider, model, and reasoning effort.

Before starting, have credentials for one [supported provider](/providers-and-models/#supported-providers).
API keys can be entered in `/providers` or supplied through the provider's environment variable;
subscription providers guide you through browser or device-code login.

Workspace trust allows `.cagent/config.toml` and project MCP servers to load. It does **not** allow
file writes, shell commands, network access, or MCP calls. Those still follow [Permissions](/permissions/).

## Send your first request

Type a request and press `Enter`:

```text
Explain how this project is structured and cite the files you inspect.
```

Cagent can inspect the workspace, propose edits, run commands, and ask for permission when needed.
Use `Shift+Enter` or `Alt+Enter` for a newline.

[Modes](/modes/) control whether edits and commands are allowed, as well as allows planning. Use `Shift+Tab` to cycle to the `plan` mode to request a plan.

The default mode, `edit`, allows read and writes within your workspace, and asks for unsafe Bash commands. The `auto` mode uses a classifier agent to accept safe permission requests.

Start with these commands:

| Command | Purpose |
| --- | --- |
| `/help` | Show commands and keybindings. |
| `/providers` | Connect or enable a provider. |
| `/model` (`Alt+P`) | Choose a model and reasoning effort. |
| `/mode` (`Shift+Tab`) | Choose how Cagent should work.|
| `/settings` | Change common settings. |
| `/resume` | Reopen a conversation. |

## Run without the TUI

Use `exec` for scripts and CI after trusting the workspace:

```sh
cagent --trust exec --output json "Run the tests and summarize failures"
```

Headless runs cannot open questions or permission prompts. Unapproved operations are denied, so
configure explicit mode/rules or restrict the task to already allowed tools. See
[Headless execution](/headless/) for output formats, tool filtering, and exit codes.

## Build manually

Cagent requires Rust 1.97.1 or newer:

```sh
cargo build --release -p cagent-cli
./target/release/cagent
```

Use `--dir DIR` to start in another workspace, or explicit paths for a portable setup:

```sh
cagent --config ./cagent.toml --data-dir ./cagent-data --dir ./my-project
```

## Related

- [Composer](/composer/) covers prompts, attachments, queues, and questions.
- [Providers and models](/providers-and-models/) covers authentication and model selection.
- [ACP integrations](/acp/) covers using Cagent in Zed and other ACP clients.
- [Configuration](/configuration/) covers UI, CLI, and TOML settings.
- [Troubleshooting](/troubleshooting/) covers common setup failures.
