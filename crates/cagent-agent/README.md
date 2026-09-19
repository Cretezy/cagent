# cagent-agent

`cagent-agent` is the UI-independent runtime for Cagent. It owns conversation
persistence, session orchestration, provider access, workspace tools, and the
semantic session views that frontends render.

## Usage

Add the crate and its async dependencies:

```toml
[dependencies]
cagent-agent = { path = "../cagent-agent" }
futures-util = "0.3"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

This example explicitly enables the built-in mock provider, which is disabled by default, so it can
run without API credentials:

```rust
use cagent_agent::config::{ConfigSnapshot, ConfigStore};
use cagent_agent::protocol::SessionCommand;
use cagent_agent::runtime::{AgentRuntime, NewSession, RuntimeOptions};
use futures_util::StreamExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let workspace = std::env::current_dir()?;
    let config = ConfigSnapshot::parse(
        std::path::Path::new("example-config.toml"),
        "version = 1\n[providers.mock]\ntype = 'mock'\nenabled = true\n",
    )?;
    let config = ConfigStore::in_memory(config);
    let runtime = AgentRuntime::open(
        RuntimeOptions::new(workspace.join(".cagent-data")).with_config(config),
    )
    .await?;
    let session = runtime
        .create_session(NewSession {
            workspace: workspace.clone(),
        })
        .await?;

    let mut attachment = session.attach().await?;
    let mut snapshot = attachment.snapshot;
    session
        .submit(SessionCommand::submit_input("Summarize this project"))
        .await?;

    while let Some(update) = attachment.updates.next().await {
        let update = update?;
        if !snapshot.apply(update) {
            attachment = session.attach().await?;
            snapshot = attachment.snapshot;
            continue;
        }
        if matches!(snapshot.turn, cagent_agent::protocol::TurnState::Idle) {
            break;
        }
    }

    println!();
    Ok(())
}
```

Use `session.attach()` to hydrate a frontend and receive its ordered updates in
one operation. If `SessionSnapshot::apply` returns `false`, the subscriber fell
behind and must attach again. Persist `session.id()` to reopen the same
conversation later with `runtime.resume_session(id)`.

New conversation IDs are human-readable, such as
`cagent-0.1.0-260815.0139.960-d2.5add51`; older eight-character formatted IDs and UUID conversation
IDs remain accepted when reopening existing sessions. The two-digit fingerprint is a hash of a
platform machine identifier; set `machine_fingerprint = false` to use `00` instead.

Applications that use a real configuration file should pass
`ConfigStore::open(path)?` instead. The store validates and publishes live file
changes; `runtime.config_snapshot()` returns the current immutable snapshot and
`runtime.subscribe_config()` observes later valid snapshots.

## Frontend projections

The crate also owns color- and layout-neutral projections for semantic Markdown,
tool activity, provider selection, and model selection. Markdown is exposed as an
owned document tree with nested blocks and inline content; code spans are already
classified for syntax highlighting and links are marked safe or unsafe before a
frontend sees them. In particular,
`ToolActivityTracker` applies the live grouping lifecycle and
`project_live_tool_activities` converts open read/list/grep blocks into reusable
exploration groups, retaining search queries and scopes as separate semantic fields.
`project_tool_activities` converts closed blocks for
committed history. Frontends supply width-aware layout, terminal escaping, glyphs,
and themes without reparsing Markdown source or reconstructing its structure.

## Internal layout

The public API is grouped by domain: `config`, `permissions`, `presentation`, `protocol`,
`provider`, `runtime`, and `tools`. Runtime API methods, authentication, request preparation,
history projection, path completion, and pricing live in focused files under `runtime/`. The
private store keeps one serialized SQLite writer per conversation. Canonical conversation data and
disposable global projections use independent migration directories; global connections are
short-lived so unrelated conversation writers do not share a SQLite write lock.

Keep provider-neutral parsing and display preparation in this crate. Frontends should receive
semantic Markdown, diffs, picker rows, and tool activity rather than recreate those projections.
