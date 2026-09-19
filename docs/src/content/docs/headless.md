---
title: Headless execution
description: Run Cagent from scripts, pipes, and CI.
---

Use `exec` to run one prompt without opening the interactive UI:

```sh
cagent exec "Summarize this repository"
printf '%s' 'Summarize this repository' | cagent exec
```

The workspace must already be trusted. Add global `--trust` for a one-run override:

```sh
cagent --trust exec "Inspect the project configuration"
```

Sessions are saved by default. Use `--no-session` for an ephemeral run or `--archive` to keep it
out of normal conversation lists. Headless runs cannot answer model questions, complete interactive
plans, or open permission prompts. A permission request is denied and reported to the agent, which
may recover with another approach; an interactive question or plan ends the run with failure.

Use `--fast` to request the model's Fast service tier for this run. This one-run override does not
change the persisted `fast` configuration preference. If the selected model does not advertise Fast
support, the run continues on its normal service tier.

## Output formats

Text is the default. Use JSON Lines for automation:

```sh
cagent exec --output json "Run the tests and summarize failures"
```

Add `--delta` to stream assistant deltas. Use structured output with a JSON Schema:

```sh
cagent exec --output structured \
  --output-schema '{"type":"object","properties":{"answer":{"type":"string"}},"required":["answer"],"additionalProperties":false}' \
  "Return an answer"
```

The validated JSON value is the last line written to stdout. With `--output structured`, it is the
only output. With `--output text` or `json`, it follows the normal activity output.

JSON output is versioned JSON Lines. Consumers should dispatch on `event`, tolerate fields added to
version `1`, and use the final `completed.exit_status` rather than parsing human-readable text.
`--delta` adds incremental `assistant_delta` records; without it, JSON emits the final assistant
message when no schema is active.

Restrict available tools with repeatable `--allow-tool` and `--deny-tool` options. See
[Tools](/tools/#restrict-tools-in-headless-runs) for valid native IDs and filter precedence, and
[CLI arguments](/cli-arguments/) for exec and global options.

Exit codes are `0` for success, `1` for runtime, provider, or unsupported-interaction failure, `2`
for invalid exec input/schema or unsupported structured output, `3` when the workspace is not
trusted. CLI parser errors exit `2`; when a shell terminates the process with SIGINT, it commonly
reports `130`. An individual permission denial does not necessarily fail the run if the agent
completes without that operation.

## Related

- [CLI arguments](/cli-arguments/#exec) lists every exec option.
- [Tools](/tools/#restrict-tools-in-headless-runs) explains allow and deny filters.
- [Permissions](/permissions/) explains how to pre-authorize required operations.
- [Worktrees](/worktrees/) isolates automation from the current checkout.
