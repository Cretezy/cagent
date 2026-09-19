---
title: Conversations
description: Resume, branch, archive, and manage conversations.
---

Cagent stores conversations locally and restores their workspace, model, mode, agent, and active
branch when resumed.

## Resume and branch

- `/resume` opens the workspace conversation picker.
- `/continue` opens the latest non-empty conversation.
- `/tree` browses the complete conversation tree.
- `/fork` continues from an earlier message without deleting the existing branch.
- In either history picker, `Shift+F` creates and opens a separate conversation containing only the
  history through the selected point. The original conversation and its branches remain unchanged.
  When a user message is selected, it is restored as an editable draft in the new conversation.
- `/retry` asks the model to continue from the active tip.
- `/rename` changes the current title. `/archive` hides the conversation from normal lists and
  exits; `/delete` permanently removes it and exits.

You can also run `cagent resume <id-or-title>` or `cagent continue` from the shell. A title query
prefers an exact title, then the most recently updated title containing the query. Cagent prints the
exact resume command when you exit a non-empty conversation.

Automatic title refinement runs in the background without interrupting a response. Manual renames
also leave the active response and branch unchanged. Rename notices are restored with their branch
history; forking does not change the conversation's current title.

Archived conversations are hidden from the picker by default but remain resumable. Deletion is
permanent and also removes its conversation scratchpad. In the picker, press `f` to favourite a
conversation; favourites are marked with `★` and kept above other conversations. If another process
owns a conversation, it opens read-only until you take it over.

Each conversation has a private scratchpad for temporary files that should not be added to the
project. Cagent provides its path to both the primary agent and delegated agents. The scratchpad
survives exit and resume, and is removed when the conversation is permanently deleted.

## Read-only observers

A read-only observer follows durable updates from the process that owns the conversation. You can
scroll and inspect it, but cannot submit messages, alter its branch, answer its interactions, or
change conversation-local state. Press `/` to use the reduced observer command set: `/providers`,
`/search`, `/statusline`, `/mcp`, `/help`, `/background` (or `/bg`), `/copy`, `/diff` (including
`conversation` and `git`, but not `clear`), `/new`,
`/files`, `/resume`, and `/quit`.

After the owner closes, choose the takeover action to continue from the observer. See the
[Interactive reference](/interactive-reference/#read-only-observers) for command details.

## Cleanup

Configure retention in TOML. Use `false` to disable a limit:

```toml
[conversation_cleanup]
automatic = true
max_size = "10gb"
max_age = "1y"
max_conversations = false
action = "trash" # or "delete"
```

When automatic cleanup is disabled, run `/cleanup` manually. See the
[retention configuration reference](/configuration/#context-retention-skills-and-worktrees) for
all limits and defaults.

Cleanup first removes empty conversations, then conversations older than `max_age`, then the oldest
remaining conversations until size and count limits are met. Open conversations are skipped. The
default `trash` action uses the operating system trash when available; `delete` removes database
files immediately. Review the settings before running cleanup because removal also drops the global
index entry and associated scratchpad.

## Related

- [Context and compaction](/context-and-compaction/) reduces model context without deleting history.
- [Worktrees](/worktrees/) explains how a recorded working directory is restored.
- [Usage and costs](/usage-and-costs/) reports persisted usage across conversations.
- [Interactive reference](/interactive-reference/) lists conversation commands and picker keys.
