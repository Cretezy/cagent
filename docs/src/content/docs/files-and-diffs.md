---
title: Files and diffs
description: Attach files, browse the workspace, and review edits.
---

## Reference files

Type `@` in the composer to attach a file or directory. Line references are supported:

```text
@src/main.rs
@src/main.rs:42
@src/main.rs:42-60
```

Files become chips and are captured as snapshots when the message starts; later disk edits do not
change that captured attachment. Cagent rejects a capture if the file changes while it is being
read. Directory chips provide a bounded, sorted tree rather than recursively including file text.

## Attachment limits and snapshots

An un-ranged text file can be up to 256 KiB by default. Larger files are kept as path references
instead of having their contents attached; Cagent privately tells the agent to inspect relevant
sections with targeted reads. You can attach contents directly by adding an explicit line range,
which must stay within the 1 MiB hard cap. A requested range is inclusive and limited to 20,000
lines. Binary and invalid UTF-8 files cannot be attached as text. Configure the byte limits under
[`[limits]`](/configuration/#interface-and-files).

Paths outside the workspace require explicit approval, even when reached through a symlink. Ignore
files affect path suggestions and trees, but typing a specific permitted path can still reference
it. `~/` and absolute `/` suggestions list one explicitly typed directory at a time; showing those
names does not bypass attachment approval. Treat attached secrets as model input: permission to read
a file is not a promise that its contents stay on your machine.

## Browse files

Run `/files` to toggle a sidebar tree of workspace files. Select directories to expand or collapse
them and select files to open them.

File and directory paths shown in the conversation are clickable. With the default built-in
viewer, a file opens in an expanded text or image view and a directory opens as an expandable tree.
Line references such as `src/main.rs:42` open at that line.

## Configure the path editor

`ui.editor` controls what happens when you open a path:

```toml
[ui]
editor = "vscode"

[ui.files]
width = 32
```

Use `"builtin"` for Cagent's expanded view, `false` to disable opening, or a preset such as
`"vscode"`, `"cursor"`, `"zed"`, `"neovim"`, `"vim"`, or `"helix"`. GUI presets launch in the
background; terminal editor presets temporarily leave the TUI and wait for the editor to close.

For a custom editor, use a command array for a foreground process:

```toml
[ui]
editor = ["micro"]
```

Or specify commands and launch mode. `{path}` and `{line}` in `line_command` preserve clickable line
references:

```toml
[ui]
editor = {
  command = ["my-editor", "--wait"],
  line_command = ["my-editor", "--wait", "--line", "{line}", "{path}"],
  mode = "foreground"
}
```

`mode` is `foreground` or `background`. See [Configuration](/configuration/#interface-and-files)
for every preset and option.

## Review changes

Cagent previews file edits before permission is granted and checks that files have not changed
before applying them. Edit cards show additions, deletions, moves, and surrounding context.

| Command | Changes shown |
| --- | --- |
| `/diff` | Your default diff mode; initially **conversation**. |
| `/diff conversation` | Successful patches recorded in this conversation, including delegated agents. |
| `/diff git` | The current Git or Jujutsu working-copy diff. |
| `/diff clear` | Reset this conversation's recorded diff only. |

Set **Default diff mode** in `/settings`, or set `ui.diff_mode` to `"conversation"` or `"git"`.
The change takes effect on the next `/diff`; explicit subcommands do not change the default.
`git` is the repository mode name, not a restriction to Git: Cagent automatically detects the
repository and prefers Jujutsu when both are present.

Conversation diffs group successful `apply_patch` changes by file while retaining successive
patches. Changes later undone remain visible; this is not a combined net diff. The viewer uses
recorded paths and hunks, not current files, and works without a repository. Manual edits, shell
edits, permission previews, denied patches, and unsuccessful results are not tracked.

Tracking and clearing survive restarts and resume. `/diff clear` leaves files, repository state,
conversation history, and the default mode untouched. Results saved after a clear begin the new
tracking period, including tools already running when you cleared it. Older conversations recover
diffs from retained successful output where possible; a notice identifies incomplete historical
coverage when output had been truncated or was unavailable.

Viewing either mode adds no conversation messages. Observers may view both modes but cannot clear
tracking. Empty and oversized diffs show a local notice. The view is bounded; use the supervised
shell for targeted repository diff output if it is too large.

## Related

- [Composer](/composer/#attach-files-and-directories) explains attachment chips and path completion.
- [Tools](/tools/#patches-and-file-mutation) documents the structured edit format.
- [Permissions](/permissions/) explains path authorization and external-path checks.
- [Worktrees](/worktrees/) isolates edits in another checkout.
