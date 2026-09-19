---
name: session-forensics
description: Diagnose persisted Cagent conversations from an ID, their canonical SQLite database, diagnostics, and runtime/store code. Use when given a `cagent-` conversation ID or when a conversation hangs, cannot resume, loses an active turn, shows an empty response, or has stuck delegated work or terminals.
---

# Session Forensics

Trace a Cagent conversation from durable state to the code path that produced it. Keep diagnosis read-only; do not edit a database or restart/kill a process unless explicitly asked.

## Workflow

1. Conversation IDs normally start with `cagent-`; their canonical database is `~/.local/share/cagent/conversations/<conversation-id>.db` on Linux. If Cagent uses a custom data directory or another operating system, locate the data directory from launch/configuration instead. The data directory also contains `diagnostic.log`; its records include conversation and turn IDs, so filter it by the supplied conversation ID first. `global.db` is disposable cross-conversation cache; `conversation-writer-locks/` controls writer ownership; `conversation-open-locks/` protects open sessions from cleanup. Ignore the old monolithic `cagent.db`.

2. Resolve the ID to its canonical database and build a timeline: conversation and active branch, agent runs/events, queued messages, background terminals, response continuations, and matching diagnostic records. Convert times to the user's timezone when useful.

3. Establish the first unresolved transition after the last completed durable action. Check the relevant store/runtime code for recovery, ownership/takeover, completion, cancellation, and resume. Cite function names and state predicates; label timing-based conclusions as inference.

4. Report the timeline, cause and confidence, whether work was lost, resumability, and the smallest safe next step. Do not implement it unless asked.

## Required checks

- Inspect `kind`, `status`, `parent_id`, `turn_id`, and `completed_at` on the active ancestry. A non-completed empty assistant node is not a completed empty answer.
- Ensure `active_node_id` matches its ancestry and status; inspect `agent_runs`, `queued_messages`, and `background_terminals` separately before declaring the conversation idle or resumable.
- Search diagnostics using the conversation, node, and turn IDs, including `NodeNotFoundOrInvalidState`.
- Inspect writer-lock ownership/takeover timing and open-lock protection separately. Only a writer owner migrates or recovers; an observer opens the database read-only with `query_only` and retains the shared open lock.
- Treat `global.db` only as corroboration. It may be missing, stale, rebuilding, or locked, and cannot show loss of canonical data or perform conversation recovery.

## SQLite discipline

- Use explicit IDs and read-only, `query_only` queries against the canonical database. Never migrate, write, `VACUUM`, repair, or recover while diagnosing.
- Do not create or migrate `global.db` to inspect it. Copy only necessary output and avoid credentials and full prompts.
- Do not infer a race from an error alone; require an ordering that links a state change to a later operation requiring the prior state.

## Common diagnosis pattern

If recovery changes a streaming node to `interrupted` and a live runtime later tries to complete it as `streaming`, investigate a per-conversation ownership/recovery race. Confirm ownership and ordering first. Do not blame delegated work merely because it precedes the failure; verify its terminal status and persistence.
