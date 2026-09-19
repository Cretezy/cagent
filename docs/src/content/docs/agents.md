---
title: Agents and delegation
description: Create specialized agent profiles and delegate independent work.
---

Agents are reusable profiles with a role, instructions, tools, permissions, and optional model
defaults. `general` is the everyday coding agent. `explore` is a fast read-only agent for focused
research.

## Choose an agent

Run `/agent` to choose a profile. Set the default for new conversations in `/settings` or TOML:

```toml
default_agent = "general"
```

## Add a custom agent

Run `/agent` and choose **Add**, or add a named table to `config.toml` and run `/reload`:

```toml
[agents.reviewer]
extends = "general"
description = "Review code without editing it"
prompt = "Report prioritized findings and cite the relevant files."
prompt_merge = "append"
availability = "both"
enabled = true
mode = "read"
# model = { model = "provider/model", effort = "high" }
# model = { tier = "small" }

[agents.reviewer.tools]
allow = ["bash"]

[agents.reviewer.mcp]
allow = ["docs"]
```

`availability` may be `user`, `subagent`, or `both`. Child agents inherit their parent and can
override prompts, models, modes, tools, MCP servers, and permissions. See
[Configuration](/configuration/#agents) for every field.

Allow lists narrow inherited access; deny lists remove entries and win when a name appears in both.
`permissions_replace = true` discards inherited inline permission rules before adding the child's
rules. Selecting an agent never grants permission by itself: active mode rules, saved permission
rules, and workspace boundaries are still evaluated immediately before each operation.

## Delegate work

Cagent can delegate independent research, verification, or parallel tasks. `/spawn <message>`
requires at least one delegated task for that turn. Use `/background` or press `Alt+Down` from an
empty composer to inspect or stop delegated work.

Configure automatic delegation in `/settings` or TOML:

```toml
[subagents]
strategy = "complex" # "off", "on_demand", "complex", "aggressive", or "always"
max_concurrent = 10
```

| Strategy | Behavior |
| --- | --- |
| `off` | Do not delegate automatically. `/spawn` can still require delegation for one turn. |
| `on_demand` | Delegate only when you clearly request it or the task requires it. |
| `complex` | Keep implementation in the primary agent and delegate useful independent work. This is the default. |
| `aggressive` | Proactively look for useful independent work to delegate. |
| `always` | Delegate every substantive request. The primary agent can only delegate, wait, ask clarifying questions, and synthesize results. |

Set `max_concurrent = 0` to disable delegation. Delegated work uses the selected profile's tools and
permissions; permission requests appear in the main session. See
[title generation and delegation settings](/configuration/#title-generation-and-delegation) for
the complete option reference.

A delegated agent uses its configured mode, or inherits the parent mode when none is set. Its mode
and permissions determine what it can edit or run. When the profile does not configure a model or
tier, the delegated run inherits the conversation's current effective model and effort. The
built-in `general` agent follows this behavior, while `explore` explicitly uses the `small` model
tier. Working-directory and worktree changes affect only that agent. The built-in `explore` agent
is read-only and uses only Bash and `web_fetch`.
Opening a delegated task shows its expanded log, including tokens, cost, and elapsed run time.
Logs load progressively only when opened; the background task list keeps lightweight summaries.
An open log fetches new entries without reloading earlier tool output. Streaming updates are batched
so long logs do not require a redraw for every model token.
The log header shows `profile · provider/model effort · status elapsed`, omitting effort when unset.
Elapsed time follows the status just like Bash output (`running 1m 01s`), updates live even before
usage arrives, and stays fixed after completion. It is omitted when no start time is available.
In the main transcript, `Running general sub-agent` has a pulsing dot while queued or running and
changes to `Finished general sub-agent` with a static dot when the run ends.

`delegate_agent` accepts an optional `effort` string (for example, `"high"`) to override reasoning
effort for that run, independently of its optional `model` and `provider` overrides. Omit it or pass
`null` to keep the resolved default. An explicit model override uses the provider's default effort
unless `effort` is also supplied.

Delegated permission requests and questions are routed back to the main session, where their source
profile is shown.

## Related

- [Tools](/tools/#delegation-and-questions) explains delegation and joining.
- [MCP](/mcp/#assign-servers-to-agents) explains server assignment.
- [Modes](/modes/) and [Permissions](/permissions/) define effective behavior.
- [Shell and background work](/shell-and-background-work/#manage-background-work) covers monitoring.
