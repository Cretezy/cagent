---
title: Composer
description: Write prompts, run shell commands, attach files, and navigate message history.
---

The composer is the input area at the bottom of Cagent. Press `Enter` to submit and `Shift+Enter` or
`Alt+Enter` for a newline. Press `Ctrl+G` to edit the draft in `$VISUAL` or `$EDITOR`.

## Run a shell command

Type `!` into an empty composer to enter shell mode, then type a command and press `Enter`. Cagent
runs it as supervised background work and returns to the normal composer.

```text
! cargo test
```

Press `Enter`, `Backspace`, or `Ctrl+C` on an empty shell input to leave shell mode. Press `Up` to
recall a previous shell command.

## Attach files and directories

Type `@` followed by a path. Workspace-relative completion searches recursively from the current
workspace. Paths beginning with `~/` or `/` complete one directory at a time from the current
user's home or filesystem root. You can reference a whole file, directory, line, range, or line and
column:

```text
@src/main.rs
@src/main.rs:42
@src/main.rs:42-60
@src/main.rs:42:8
@~/notes.md
@/var/log/app.log
```

Press `Tab` or `Enter` to confirm the highlighted path. Files become chips and their text is added
to model context as a snapshot for that message. Directories provide a bounded tree. Click a chip
to open it in the configured editor or built-in viewer. Attachment size limits and ignored-path
behavior are documented under [Files and diffs](/files-and-diffs/#attachment-limits-and-snapshots).
Listing an external directory for completion does not grant read access; attaching a path outside
the workspace still uses the normal permission flow.

Paste an image with `Ctrl+V`. Images become chips when the selected model supports image input.
If the model does not support images, Cagent leaves a notice instead of silently dropping the image.

## Navigate prompt history

From an empty composer, press `Up` and `Down` to move through your messages in the current
conversation. History includes slash commands and shell commands; shell entries restore shell mode.
Editing a recalled entry creates a new draft and does not change the old message.

Use `/tree` to browse the full conversation and `/fork` to continue from an earlier message. Press
`Enter` to fork within the current conversation. Press `Shift+F` in either picker to hard-fork into a
new conversation containing only the selected branch. Selecting a user message restores its text and
attachments to the composer so you can edit and resend it.

## Queue messages

When Cagent is working, press `Enter` to queue a message for the next safe boundary, or `Tab` to
queue it for the end of the turn. Use `Alt+Up` and `Alt+Down` to select queued messages. Press `d`
to delete one or `s` to promote it to the next boundary. If Cagent is idle, pressing `Tab` on a
non-empty draft sends it normally when there is no completion to accept. Press `Enter` on a selected
message to edit it. While it is being edited, Cagent waits to send it and every message queued after
it; submitting or cancelling the edit releases the queue.

## Answer agent questions

When Cagent needs a consequential preference it cannot determine by inspecting the workspace, it
may open one to three multiple-choice questions. Use `Up` and `Down` to choose an answer and
`Enter` to confirm it. Use `Left` and `Right` to revisit questions. Each question also includes
**None of the above**; select it and press `Tab` to enter your own answer as a note. You can press
`Tab` on any choice to add optional context before confirming.

Questions, permission approvals, and plan-completion choices share one interaction queue. Cagent
shows one at a time in request order. If you are actively typing a draft, the next interaction waits
briefly instead of replacing the composer; your draft is preserved while ordinary question and plan
surfaces are open.

## Slash commands

Type `/` to open command completion. Use the arrow keys to select, `Tab` to complete, and `Enter` to
run. To send text beginning with `/` as an ordinary prompt, start it with one space.

Common commands include:

| Command | Purpose |
| --- | --- |
| `/providers`, `/model` | Choose a provider and model. |
| `/mode`, `/plan`, `/auto` | Change working mode. |
| `/agent` | Change agent profile. |
| `/search` | Configure or perform web search. |
| `/skills`, `/mcp`, `/permissions` | Manage tools and access. |
| `/files` | Toggle the workspace file tree. |
| `/diff [conversation\|git\|clear]` | View conversation patches (default), view the Git/Jujutsu working-copy diff, or clear conversation diff tracking. |
| `/tree`, `/fork` | Browse or branch conversation history. |
| `/retry` | Continue the model from the current point. |
| `/compact` | Summarize older context. |
| `/context save` | Export the latest model context request as JSON. |
| `/resume`, `/continue`, `/new` | Move between conversations. |
| `/background` | View running and past terminals or delegated work. |
| `/copy [slack\|discord]` | Copy the last completed response as Markdown or for a chat service. |
| `/settings`, `/help` | Configure Cagent or view help. |

See the [Interactive reference](/interactive-reference/) for every command, argument form,
read-only availability, configurable action, and default chord.

## Useful keys

| Key | Action |
| --- | --- |
| `Ctrl+C` | Clear a non-empty draft; when empty, press once to arm exit and again to exit. |
| `Ctrl+D` | Exit when the composer is empty. |
| `Esc` | Close the current menu, or interrupt active work. |
| `Ctrl+U` | Undo the last composer edit. |
| `Ctrl+W` | Delete the previous word or chip. |
| `Alt+P` | Open the model picker. |
| `Alt+T` | Open the conversation tree. |
| `Alt+F` | Open the fork picker. |
| `Alt+R` | Retry or continue the model. |

Run `/help` for the complete resolved key map, or use the
[keybinding reference](/interactive-reference/#configurable-keybindings). Customize bindings in
`/settings` or TOML:

```toml
[keys]
insert_newline = ["shift+enter", "ctrl+enter"]
model_picker = ["alt+p"]
```

Use an empty array to unbind an action.

See the [keybinding configuration reference](/configuration/#keybindings) for every action and its
default chords.

## Configure the composer

```toml
[ui]
composer_max_rows = 12
editor = "builtin" # false, a preset such as "vscode", or a command array

[ui.files]
width = 32
```

Omit `composer_max_rows` for an unlimited height. Editor presets include VS Code, Cursor, Zed,
JetBrains IDEs, Neovim, Vim, Helix, Emacs, and Sublime Text. See
[interface and file settings](/configuration/#interface-and-files) for all editor shapes and UI
limits.

## Related

- [Interactive reference](/interactive-reference/) lists every command and configurable key.
- [Files and diffs](/files-and-diffs/) covers snapshots, the file tree, viewers, and edit review.
- [Shell and background work](/shell-and-background-work/) covers supervised commands.
- [Tools](/tools/#delegation-and-questions) explains model-facing questions and delegation.
