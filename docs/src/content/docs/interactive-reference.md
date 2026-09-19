---
title: Interactive reference
description: Complete reference for slash commands and configurable keybindings.
---

Type `/` in the composer to search commands. Use the arrow keys to select a result, `Tab` to
complete it, and `Enter` to run it. `/help` shows the same command set together with the keybindings
resolved from your configuration.

To send a prompt that begins with `/`, put one space before the slash. An unrecognized slash command
is recorded in the conversation as an error instead of being sent to the model.

## Slash commands

The **Read-only** column identifies commands available while observing a conversation owned by
another Cagent process. See [Read-only observers](#read-only-observers) for the restrictions.

| Command | Purpose | Read-only |
| --- | --- | :---: |
| `/providers` | Choose, connect, enable, or disable providers. | ✓ |
| `/model [query]` | Choose a model and reasoning effort, optionally starting with a search query. | — |
| `/fast [on|off]` | Toggle or explicitly set the global Fast service-tier preference. It applies only to models that advertise support. | — |
| `/dir [path]` | Show or change the conversation's working directory. | — |
| `/agent [name]` | Choose an agent profile. | — |
| `/skills` | Manage project and global skills. | — |
| `/mode [name] [message]` | Choose a mode, or switch mode and submit a message. | — |
| `/<mode> [message]` | Switch directly to any enabled mode, optionally submitting a message. | — |
| `/permissions` | View and manage conversation, project, and global permission rules. | — |
| `/spawn <message>` | Submit a message that requires at least one delegated task. | — |
| `/search [query]` | Configure web search, or require a search for a submitted query. | ✓ |
| `/statusline` | Choose, order, and color status-line modules. | ✓ |
| `/settings` | Edit common UI, limit, mode, model, and keybinding settings. | — |
| `/usage` | View project, model, recent, and all-time usage, or reset global tracking. | — |
| `/mcp` | Manage external MCP servers and inspect their tools. | ✓ |
| `/help` | Show commands and resolved keybindings. | ✓ |
| `/background` | Show active and past terminals and delegated work. | ✓ |
| `/copy [slack\|discord]` | Copy the last completed assistant response as Markdown or a chat dialect. | ✓ |
| `/diff [conversation\|git\|clear]` | View recorded conversation patches (default) or the current Git/Jujutsu working-copy diff. `clear` resets only tracking. | View only |
| `/new [title]` | Start a new conversation, optionally with a title. | ✓ |
| `/tree` | Browse the durable conversation tree. | — |
| `/files` | Toggle the workspace file tree. | ✓ |
| `/worktree [worktree] [base]` | Enter or create a Git worktree or Jujutsu workspace. | — |
| `/fork` | Select an earlier message and continue on a new branch. | — |
| `/resume [conversation-id]` | Open the conversation picker or resume an exact ID. | ✓ |
| `/continue` | Resume the latest non-empty conversation in this workspace. | — |
| `/rename [name]` | Rename the current conversation. | — |
| `/archive` | Archive the current conversation and exit. | — |
| `/delete` | Permanently delete the current conversation and exit. | — |
| `/retry` | Continue the model from reconstructed safe history without rerunning completed tools. | — |
| `/compact [instructions]` | Summarize older branch context, optionally preserving specified details. | — |
| `/context save` | Save the exact latest model request as JSON under the configured data directory. | — |
| `/cleanup` | Apply the configured conversation-retention policy now. | — |
| `/reload` | Reload configuration and refresh model catalogs. | — |
| `/quit` | Exit Cagent. | ✓ |

In `/tree` and `/fork`, press `Shift+F` to hard-fork the selected point into a separate conversation.
This picker-local shortcut is fixed rather than a configurable global keybinding.

`/bg` is an alias for `/background`. Mode commands are generated from enabled built-in and custom
modes; for example, `/plan Review this design` switches to plan mode and submits only
`Review this design`. The complete command remains available in prompt history.

`/copy` uses ordinary Markdown when no dialect is supplied. `/copy slack` and `/copy discord`
convert the last completed assistant response for the selected chat service before placing it on
the system clipboard.

## Configurable keybindings

Every action below has a stable configuration ID under `[keys]`. Chords are contextual: arrow keys
navigate menus when one is open, but move through prompt history or the composer where appropriate.

| Action | Default chord | Behavior |
| --- | --- | --- |
| `submit` | `Enter` | Submit the composer or choose the selected item. |
| `insert_newline` | `Shift+Enter`, `Alt+Enter` | Insert a composer newline. |
| `model_picker` | `Alt+P` | Open the model picker. |
| `toggle_fast` | `Alt+S` | Immediately toggle the global Fast service-tier preference. |
| `tree` | `Alt+T` | Open the conversation tree. |
| `fork` | `Alt+F` | Open the fork picker. |
| `rename` | `Alt+N` | Open the conversation rename editor. |
| `retry_turn` | `Alt+R` | Continue the model from reconstructed safe history. |
| `external_editor` | `Ctrl+G` | Edit the current draft in `$VISUAL` or `$EDITOR`. |
| `line_start` | `Ctrl+A` | Move to the start of the current composer line. |
| `line_end` | `Ctrl+E` | Move to the end of the current composer line. |
| `delete_word` | `Ctrl+W` | Delete the previous word or attachment chip. |
| `undo` | `Ctrl+U` | Undo the last composer edit. |
| `cancel` | `Ctrl+C` | Clear a non-empty draft. When empty, press once to arm exit and again within two seconds to exit. |
| `exit` | `Ctrl+D` | Exit when the composer is empty. |
| `close_surface` | `Esc` | Close the current surface, leave a contextual selection, or interrupt active work. |
| `previous_mode` | `Shift+Left` | Switch to the previous enabled cycleable mode. |
| `next_mode` | `Shift+Right`, `Shift+Tab` | Switch to the next enabled cycleable mode. |
| `navigate_left` | `Left` | Move left, open the previous tab, or move to the previous question. |
| `navigate_right` | `Right` | Move right, open the next tab, or move to the next question. |
| `navigate_up` | `Up` | Navigate upward, recall history, or select queued messages. |
| `navigate_down` | `Down` | Navigate downward or move through history and queued messages. |
| `background` | `Alt+Down` | Open background work from an empty composer. |
| `complete` | `Tab` | Complete the current item; otherwise send an idle non-empty draft or queue it for the end of an active turn. |
| `toggle_chip` | `Alt+E` | Expand or collapse the selected attachment chip. |
| `delete_queued` | `d` | Delete the selected queued message. |
| `promote_queued` | `s` | Promote the selected queued message to the next safe boundary. |
| `scroll_to_message` | `s` | In conversation history, scroll the transcript to the selected message. |

Some keys are intentionally contextual. In particular, `d` and `s` type normally unless queued
message or history selection is active.

Once the welcome card has scrolled out of view, closing a panel, shrinking the composer, or
resizing the terminal does not bring it back at the live tail. Short transcripts stay against
the composer with extra space above them. Scroll upward or press `Home` to see the welcome card
again; `End` returns to the latest content.

At the live tail, Cagent loads enough older history to fill the screen even when tool activity
is collapsed. It does not wait for an upward scroll to fill an otherwise empty area. The welcome
card is only shown at the actual beginning of the conversation, not the start of a loaded page.

Right-click inside an expanded view (Bash output, sub-agent logs, files, and other expanded
content) to close it and return to the previous view or transcript. Right-clicking the sidebar
or status row does not close the view.

Override one or more chords with arrays. An empty array unbinds an action:

```toml
[keys]
insert_newline = ["shift+enter", "ctrl+enter"]
model_picker = ["alt+p"]
toggle_chip = []
```

Invalid entries produce a warning. Exact conflicts between actions restore the affected actions to
their complete built-in defaults. See [Configuration](/configuration/#keybindings) for the accepted
shape.

## Read-only observers

If another process owns a conversation, Cagent opens it as a live read-only observer. You can
scroll the transcript, inspect supported surfaces, and run commands marked **Read-only** above, but
you cannot submit messages, change branch state, modify conversation settings, or answer the
owner's interactions.

Press `/` to activate the observer command input. Close the owning process and choose the takeover
action when you need to continue the conversation from the observer.

## Related

- [Composer](/composer/) explains how commands, queues, questions, and attachments behave.
- [Configuration](/configuration/#keybindings) contains the keybinding configuration schema.
- [Conversations](/conversations/#read-only-observers) explains ownership and takeover.
- [CLI arguments](/cli-arguments/) lists non-interactive commands and launch options.
