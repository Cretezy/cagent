---
title: Context and compaction
description: Understand model context and summarize long conversations.
---

Cagent sends the active conversation branch, instructions, captured attachments, and relevant tool
results to the model. This is different from the visible transcript: hidden instructions, tool
protocol records, summaries, and provider-specific formatting also consume context. The status line
shows an estimate of the selected model's context window usage.

## Save the current context

Run `/context save` to export the exact latest provider-neutral request sent to the model. Cagent
writes a new pretty-printed JSON file to `<data-dir>/contexts/`, normally
`~/.local/share/cagent/contexts/`. An explicit `--data-dir` changes this location.

The export includes the model and effort, stable prompts, ordered messages and tool traffic, tool
definitions, structured-output configuration, and response-continuation metadata. It represents
the latest request actually sent, not a projection of the next request. Therefore, the latest
assistant response does not appear until it is included in a later model request.

Context exports can contain sensitive system instructions, conversation text, file contents,
tool output, and opaque encrypted reasoning returned by OpenAI or ChatGPT. Cagent does not decrypt
that reasoning. Treat these exports as private and review them before sharing.

## Compact a conversation

Run `/compact` to summarize older context while keeping recent messages intact:

```text
/compact
/compact Preserve all API decisions and unresolved test failures.
```

The summary becomes a checkpoint in `/tree` and `/fork`. Forking before it excludes the summary;
forking after it may reuse it. The normal transcript shows a `Compacted` divider; select the divider
to open a scrollable **Compacted context** view containing the generated summary rendered as
Markdown. The recent raw context retained alongside that summary remains in its normal transcript
position. Compaction is durable conversation history, not deletion: the original nodes remain
available for tree navigation, while later model requests use the checkpoint plus retained recent
groups. Tool calls and their results are kept together so the provider protocol remains valid.

The plan acceptance menu also offers **compact context**. It first records the normal compact
`Plan accepted` confirmation, then applies the normal compaction policy to the context before the
proposed plan. It excludes that proposed-plan message from the checkpoint and replays the accepted
plan fresh after the checkpoint before implementation starts.

Running `/compact` only creates the checkpoint; it does not start a follow-up model turn.

## Automatic compaction

Cagent can compact automatically near the model's context limit. Configure it in `/settings` or
TOML:

```toml
[compaction]
enabled = true
threshold_percent = 90
```

See the [compaction configuration reference](/configuration/#context-retention-skills-and-worktrees)
for valid values and defaults.

If a provider rejects an oversized request before producing output, Cagent may compact once and
retry. Completed tools are never run again during that retry. Compaction can still fail when one
indivisible message or tool group is larger than the model's usable window; shorten the prompt,
attach a narrower line range, remove a large tool result by forking earlier, or choose a model with a
larger window.

## Related

- [Conversations](/conversations/#resume-and-branch) explains trees, forks, and durable branches.
- [Files and diffs](/files-and-diffs/#attachment-limits-and-snapshots) limits attachment size.
- [Status line](/status-line/) explains context indicators.
- [Configuration](/configuration/#context-retention-skills-and-worktrees) lists compaction settings.
