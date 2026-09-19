---
title: Modes & Planning
description: Choose how Cagent works and what happens when you accept a plan.
---

Modes change how Cagent works in the current conversation. They can add instructions, change
permission defaults, and select a different model or reasoning effort.

## Built-in modes

| Mode | Use it for | Reads | Edits | Commands |
| --- | --- | --- | --- | --- |
| `read` | Careful inspection | Allow; ask externally | Ask | Ask |
| `edit` | Everyday coding | Allow; ask externally | Allow | Ask |
| `auto` | Trusted work with fewer prompts | Allow; review externally | Allow project edits; review external edits | Automatically reviewed |
| `plan` | Designing an implementation first | Allow; ask externally | Deny | Ask |

Explicit [permission rules](/permissions/) always take precedence.
Modes supply fallback behavior; they do not bypass external-path checks, explicit denies, agent
policies, or tool availability. Auto review is a decision step, not an OS sandbox.

## Switch modes

Press `Shift+Tab` to cycle through enabled, cycleable modes. You can also run `/mode`, switch
directly, or use `Shift+Left` and `Shift+Right`:

```text
/mode plan
/plan Review this design before editing anything.
/auto
```

Every enabled mode has a direct `/<mode>` command, including modes excluded from cycling. A mode
command with a message is queued if Cagent is already working; the mode changes when that message
is sent.

## Plan mode

Plan mode is conversational: Cagent inspects the relevant code before asking about discoverable
facts, and it can ask for your input when consequential questions about intent, scope, preferences,
or tradeoffs remain. It does not finalize a plan while material questions are unanswered, and you
can explicitly ask it to discuss or list ideas without proposing a plan. Once the result is
decision-ready, Cagent produces an implementation plan without editing files. After the plan,
choose a mode to continue implementation or keep planning.

### Plan acceptance choices

| Choice | What happens |
| --- | --- |
| **Accept** | Start implementing with the conversation as it is. |
| **Accept and clear context** | Start from the accepted plan with a fresh context window. Your earlier conversation remains available in `/tree`. |
| **Accept and compact context** | Summarize older discussion, keep recent context, and start implementing from the accepted plan. |
| **Keep planning** | Continue discussing or revising the plan. |

All three acceptance choices can target a different implementation mode or model before you
continue. By default, the menu starts on `default_mode` when that mode can be used for
implementation. Set `default_plan_exit_mode = "auto"` (or another enabled, cycleable,
non-planning mode) to choose a different initial mode.

## Auto mode

Auto mode allows reads and writes inside the project. It uses the configured
[small model](/providers-and-models/#small-model) to review unresolved external reads,
external writes, commands, web requests, and MCP calls. Clearly safe actions can proceed automatically.
Reviews with material uncertainty, insufficient authorization, or failures still open the normal permission prompt with the edit preview.
Its per-mode `auto_level` defaults to `high`, which tells the reviewer to strongly prefer automatic
approval for low- and medium-risk actions. High-risk actions can also proceed when your request
explicitly authorizes the operation and target, the action matches that scope, and no hard constraint,
malicious injection, or material uncertainty remains. They remain classified as high risk; general
implementation or build requests do not authorize production migrations. Set it to `medium` to ask
at medium risk or above. This setting guides the reviewer rather than overriding its final decision,
so unresolved facts that could materially change risk or authorization, and hard safety constraints,
can still prompt. The authorization
shown in a review summary is separately inferred from the trusted conversation; it is not the
configured auto level.
Requests to implement, fix, investigate, or verify cover proportionate, task-related local development
steps without specifying exact commands. Running project code or creating build artifacts does not
by itself make local builds, tests, or established project scripts high risk. This does not authorize
unrelated changes, publishing, production operations, credential access, or destructive actions;
permission rules and external-path protections still apply.
When `run = "auto"` but `write` is not `auto`, Bash auto review cannot approve file-writing
commands; deterministic safe builds, tests, formatters, and lint fixers remain available.

## Add a custom mode

Add a named table to `config.toml`, then run `/reload`:

```toml
[modes.review]
description = "Review changes without editing"
prompt = "Report prioritized findings with file references."
color = "light-cyan"
enabled = true
cycleable = true
read = "allow"
write = "deny"
run = "ask"
auto_level = "high"
plan = false
# model = "provider/model"
# effort = "high"
```

`read`, `write`, and `run` accept `allow`, `ask`, `auto`, or `deny`. `read = "auto"` allows project
reads and sends unresolved external tool reads through auto review. `write = "auto"` does the same
for writes. The policies are
independent. User-supplied attachments and workspace changes retain their explicit external-boundary
prompts. `auto_level` accepts `medium` or `high` and defaults to `high`. Set `cycleable = false` to
exclude the mode from keyboard cycling while retaining `/review`. Use `[mode] order = [...]` to
set cycle order. See [Configuration](/configuration/#modes) for every field.

## Models by mode

A `/model` selection belongs to the active mode. Configure mode defaults in TOML:

```toml
[modes.plan]
model = { model = "openai/gpt-5.6-sol", effort = "high" }
```

See the [mode configuration reference](/configuration/#modes) for model and effort fields.

To override a built-in mode, define only the fields you want under its existing name. Set
`default_mode = "review"` to use a custom mode for new conversations. See the
[mode configuration reference](/configuration/#modes) for every field and inline permission shape.

## Related

- [Permissions](/permissions/) explains rule precedence and saved approvals.
- [Providers and models](/providers-and-models/#choose-a-model) explains mode-specific model choices.
- [Composer](/composer/#queue-messages) explains messages queued during active work.
- [Agents](/agents/) explains profiles that can select a default mode.
