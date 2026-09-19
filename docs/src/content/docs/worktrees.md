---
title: Worktrees
description: Isolate work in Git worktrees or Jujutsu workspaces.
---

Ask Cagent to use a worktree when you want to isolate the current task:

```text
Use a worktree for this task.
```

Cagent can create or enter the worktree and continue there.

Use this before editing when you want the current checkout and its uncommitted changes left alone.
Creating an isolated checkout does not copy those changes unless the worktree copy policy includes
their files.

For a new interactive or headless session, `-w` creates a generated worktree and
`--worktree NAME` creates a named one.

You can also run `/worktree` to choose an existing worktree or create one under
`.cagent/worktrees`. Use `/worktree NAME [BASE]` to choose a name and optional revision directly.

```toml
[worktree]
base = "fresh"             # "fresh", "head", or another revision
branch_prefix = "worktree" # Git only
copy = false               # false, "include", or "all"
```

`fresh` starts from the repository's default branch; `head` starts from the revision currently
checked out in the source workspace. Another value is passed to Git or Jujutsu as a revision.

`copy = "include"` copies ignored files selected by `.worktreeinclude`; `"all"` also copies files
ignored by `.gitignore`. Review `.worktreeinclude` carefully: copied ignored files may contain API
keys, local databases, or build artifacts. Cagent preserves symlinks, never overwrites checked-out
files, and never deletes worktrees automatically. See the
[worktree configuration reference](/configuration/#context-retention-skills-and-worktrees) for all
fields.

Resumed conversations restore their recorded working directory. Use `/dir PATH` when you only want
to change the current directory without creating an isolated checkout.

The model-facing `enter_worktree` and `change_working_directory` tools follow the same rules. See
[Tools](/tools/#working-directories-and-worktrees) for their path and base semantics.

## Related

- [Files and diffs](/files-and-diffs/#review-changes) explains working-copy review.
- [Conversations](/conversations/) explains restored directories when you resume.
- [CLI arguments](/cli-arguments/#worktrees-and-directories) lists launch-time options.
- [Permissions](/permissions/#workspace-boundaries) explains path checks after changing directory.
