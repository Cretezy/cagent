use super::super::*;
use super::parsing::*;

pub(super) fn parse_git(args: &[String]) -> Option<SafeCommandParse> {
    let args = if args.first().is_some_and(|arg| arg == "--no-pager") {
        &args[1..]
    } else {
        args
    };
    let subcommand = args.first()?;
    let rest = &args[1..];
    let parsed = match subcommand.as_str() {
        "status" => parse_git_status(rest)?,
        "log" => parse_git_log(rest)?,
        "diff" => parse_git_diff(rest)?,
        "show" => parse_git_show(rest)?,
        "branch" => parse_git_branch(rest)?,
        "ls-files" => parse_git_ls_files(rest)?,
        "grep" => parse_git_grep(rest)?,
        "tag" => parse_git_tag(rest)?,
        "stash" => parse_git_stash(rest)?,
        "worktree" => parse_git_worktree(rest)?,
        _ => return None,
    };
    Some(parsed)
}

fn parse_git_ls_files(args: &[String]) -> Option<SafeCommandParse> {
    let (options, paths) = split_git_pathspecs(args);
    git_options(
        options,
        &grammar(
            "comdizt",
            "",
            &[
                "--cached",
                "--deleted",
                "--modified",
                "--others",
                "--ignored",
                "--stage",
                "--deleted",
                "--killed",
                "--directory",
                "--eol",
                "--full-name",
                "--resolve-undo",
                "--sparse",
                "--deduplicate",
                "--abbrev",
                "--debug",
                "--error-unmatch",
            ],
            EMPTY,
        ),
    )?;
    let mut result = with_git_paths(
        SafeCommandParse::new(Some(SafeShellPresentation::List)),
        paths,
    );
    result.detail =
        Some(SafePresentationDetail::new("List git files").arguments(paths.iter().cloned(), true));
    Some(result)
}

fn parse_git_grep(args: &[String]) -> Option<SafeCommandParse> {
    let (options, paths) = split_git_pathspecs(args);
    let parsed = parse_options(
        options,
        &grammar(
            "EinivwclLhHqIzo",
            "e",
            &[
                "--extended-regexp",
                "--ignore-case",
                "--invert-match",
                "--word-regexp",
                "--line-number",
                "--files-with-matches",
                "--files-without-match",
                "--count",
                "--heading",
                "--break",
                "--only-matching",
                "--name-only",
                "--null",
            ],
            EMPTY,
        ),
    )?;
    let query = if parsed.options.contains("-e") {
        parsed.option_value(&["-e"])
    } else {
        parsed.positionals.first().cloned()
    }?;
    let remaining = if parsed.options.contains("-e") {
        parsed.positionals
    } else {
        parsed.positionals.into_iter().skip(1).collect()
    };
    remaining.is_empty().then_some(())?;
    let mut result = with_git_paths(
        SafeCommandParse::new(Some(SafeShellPresentation::Search)),
        paths,
    );
    result.query = Some(query);
    result.detail = Some(
        SafePresentationDetail::new("Search git")
            .arguments([result.query.clone().unwrap()], false)
            .relation("in", paths.iter().cloned(), true),
    );
    Some(result)
}

fn parse_git_tag(args: &[String]) -> Option<SafeCommandParse> {
    let parsed = parse_options(
        args,
        &grammar(
            "ln",
            "",
            &["--list", "--ignore-case", "--column"],
            &[
                "--contains",
                "--no-contains",
                "--merged",
                "--no-merged",
                "--points-at",
                "--sort",
                "--format",
            ],
        ),
    )?;
    let listing = parsed.positionals.is_empty()
        || parsed.options.contains("-l")
        || parsed.options.contains("--list");
    (listing
        && !parsed
            .positionals
            .iter()
            .any(|pattern| pattern.starts_with('-')))
    .then(|| {
        let mut result = with_git_paths(
            SafeCommandParse::new(Some(SafeShellPresentation::List)),
            &[],
        );
        result.detail = Some(SafePresentationDetail::new("List git tags"));
        result
    })
}

fn parse_git_stash(args: &[String]) -> Option<SafeCommandParse> {
    let args = args.strip_prefix(&["list".into()])?;
    let parsed = parse_options(
        args,
        &grammar(
            "p",
            "n",
            &["--patch", "--stat", "--name-only", "--oneline"],
            &["--max-count"],
        ),
    )?;
    parsed.positionals.is_empty().then(|| {
        let mut result = with_git_paths(
            SafeCommandParse::new(Some(SafeShellPresentation::List)),
            &[],
        );
        result.detail = Some(SafePresentationDetail::new("List git stashes"));
        result
    })
}

fn parse_git_worktree(args: &[String]) -> Option<SafeCommandParse> {
    matches!(args, [command] if command == "list")
        .then(|| {
            let mut result = with_git_paths(
                SafeCommandParse::new(Some(SafeShellPresentation::List)),
                &[],
            );
            result.detail = Some(SafePresentationDetail::new("List git worktrees"));
            result
        })
        .or_else(|| {
            matches!(args, [command, flag] if command == "list" && flag == "--porcelain").then(
                || {
                    let mut result = with_git_paths(
                        SafeCommandParse::new(Some(SafeShellPresentation::List)),
                        &[],
                    );
                    result.detail = Some(SafePresentationDetail::new("List git worktrees"));
                    result
                },
            )
        })
}

fn split_git_pathspecs(args: &[String]) -> (&[String], &[String]) {
    args.iter()
        .position(|arg| arg == "--")
        .map_or((args, &[]), |index| (&args[..index], &args[index + 1..]))
}
fn git_options(args: &[String], grammar: &OptionGrammar<'_>) -> Option<ParsedArgs> {
    let parsed = parse_options(args, grammar)?;
    parsed.positionals.is_empty().then_some(parsed)
}
fn with_git_paths(mut parsed: SafeCommandParse, paths: &[String]) -> SafeCommandParse {
    for path in paths {
        parsed = parsed.path(path.clone(), "Git pathspec");
    }
    parsed.hardening = SafeCommandHardening::Git;
    parsed
}

pub(super) fn parse_git_status(args: &[String]) -> Option<SafeCommandParse> {
    let (options, paths) = split_git_pathspecs(args);
    let normalized = options
        .iter()
        .map(|arg| match arg.as_str() {
            "--porcelain=v1" => "--porcelain".into(),
            _ => arg.clone(),
        })
        .collect::<Vec<_>>();
    git_options(
        &normalized,
        &grammar(
            "sbuvz",
            "",
            &[
                "--short",
                "--branch",
                "--show-stash",
                "--porcelain",
                "--long",
                "--verbose",
                "--untracked-files",
                "--ignore-submodules",
                "--ignored",
                "--column",
                "--no-renames",
                "--renames",
                "--find-renames",
                "--ahead-behind",
                "--no-ahead-behind",
            ],
            EMPTY,
        ),
    )?;
    let mut result = with_git_paths(
        SafeCommandParse::new(Some(SafeShellPresentation::Read)),
        paths,
    );
    result.detail =
        Some(SafePresentationDetail::new("Read git status").arguments(paths.iter().cloned(), true));
    Some(result)
}

pub(super) fn parse_git_log(args: &[String]) -> Option<SafeCommandParse> {
    let (before, paths) = split_git_pathspecs(args);
    let normalized = before
        .iter()
        .map(|arg| {
            if arg.len() > 1 && arg.starts_with('-') && arg[1..].chars().all(|c| c.is_ascii_digit())
            {
                format!("--max-count={}", &arg[1..])
            } else {
                arg.clone()
            }
        })
        .collect::<Vec<_>>();
    let parsed = parse_options(
        &normalized,
        &grammar(
            "pgu",
            "n",
            &[
                "--oneline",
                "--decorate",
                "--no-decorate",
                "--graph",
                "--stat",
                "--shortstat",
                "--numstat",
                "--name-only",
                "--name-status",
                "--summary",
                "--patch",
                "--no-patch",
                "--full-diff",
                "--first-parent",
                "--merges",
                "--no-merges",
                "--all",
                "--branches",
                "--tags",
                "--remotes",
                "--reverse",
                "--topo-order",
                "--date-order",
                "--author-date-order",
                "--walk-reflogs",
                "--boundary",
                "--left-right",
                "--cherry-mark",
                "--cherry-pick",
                "--simplify-merges",
                "--show-pulls",
            ],
            &[
                "--max-count",
                "--skip",
                "--since",
                "--after",
                "--until",
                "--before",
                "--author",
                "--committer",
                "--grep",
                "--format",
                "--pretty",
                "--date",
                "--decorate-refs",
                "--decorate-refs-exclude",
                "--diff-filter",
                "--find-renames",
                "--find-copies",
            ],
        ),
    )?;
    let mut result = with_git_paths(
        SafeCommandParse::new(Some(SafeShellPresentation::Read)),
        paths,
    );
    result.detail = Some(
        SafePresentationDetail::new("Read git log")
            .arguments(parsed.positionals, false)
            .relation("in", paths.iter().cloned(), true),
    );
    Some(result)
}

pub(super) fn parse_git_diff(args: &[String]) -> Option<SafeCommandParse> {
    let (before, paths) = split_git_pathspecs(args);
    let parsed = parse_options(
        before,
        &grammar(
            "puw",
            "U",
            &[
                "--patch",
                "--no-patch",
                "--stat",
                "--shortstat",
                "--numstat",
                "--name-only",
                "--name-status",
                "--summary",
                "--check",
                "--raw",
                "--compact-summary",
                "--binary",
                "--full-index",
                "--color",
                "--no-color",
                "--word-diff",
                "--color-words",
                "--minimal",
                "--patience",
                "--histogram",
                "--anchored",
                "--diff-algorithm",
                "--ignore-all-space",
                "--ignore-space-change",
                "--ignore-space-at-eol",
                "--ignore-cr-at-eol",
                "--ignore-blank-lines",
                "--relative",
                "--no-renames",
                "--submodule",
                "--cached",
                "--staged",
                "--merge-base",
                "--no-ext-diff",
                "--no-textconv",
                "--quiet",
                "--exit-code",
            ],
            &[
                "--unified",
                "--output-indicator-new",
                "--output-indicator-old",
                "--output-indicator-context",
                "--stat-width",
                "--stat-name-width",
                "--stat-count",
                "--diff-filter",
                "--find-renames",
                "--find-copies",
                "--word-diff-regex",
                "--color-moved",
                "--color-moved-ws",
                "--ignore-matching-lines",
                "--src-prefix",
                "--dst-prefix",
                "--line-prefix",
                "--inter-hunk-context",
            ],
        ),
    )?;
    let mut result = with_git_paths(
        SafeCommandParse::new(Some(SafeShellPresentation::Read)),
        paths,
    );
    result.detail = Some(
        SafePresentationDetail::new("Read git diff")
            .arguments(paths.iter().cloned(), true)
            .relation("in", parsed.positionals, false),
    );
    Some(result)
}

pub(super) fn parse_git_show(args: &[String]) -> Option<SafeCommandParse> {
    let (before, paths) = split_git_pathspecs(args);
    let parsed = parse_options(
        before,
        &grammar(
            "puw",
            "U",
            &[
                "--patch",
                "--no-patch",
                "--stat",
                "--shortstat",
                "--numstat",
                "--name-only",
                "--name-status",
                "--summary",
                "--raw",
                "--compact-summary",
                "--full-index",
                "--color",
                "--no-color",
                "--word-diff",
                "--minimal",
                "--patience",
                "--histogram",
                "--ignore-all-space",
                "--ignore-space-change",
                "--ignore-space-at-eol",
                "--ignore-blank-lines",
                "--relative",
                "--no-renames",
                "--no-ext-diff",
                "--no-textconv",
            ],
            &[
                "--unified",
                "--format",
                "--pretty",
                "--date",
                "--diff-filter",
                "--find-renames",
                "--find-copies",
            ],
        ),
    )?;
    let mut result = with_git_paths(
        SafeCommandParse::new(Some(SafeShellPresentation::Read)),
        paths,
    );
    result.detail = Some(
        SafePresentationDetail::new("Read git")
            .arguments(parsed.positionals, false)
            .relation("in", paths.iter().cloned(), true),
    );
    Some(result)
}

pub(super) fn parse_git_branch(args: &[String]) -> Option<SafeCommandParse> {
    let parsed = parse_options(
        args,
        &grammar(
            "avvr",
            "",
            &[
                "--list",
                "--all",
                "--remotes",
                "--verbose",
                "--no-abbrev",
                "--column",
                "--no-column",
                "--color",
                "--no-color",
                "--ignore-case",
                "--show-current",
                "--contains",
                "--no-contains",
                "--merged",
                "--no-merged",
                "--points-at",
            ],
            &["--sort", "--format", "--abbrev"],
        ),
    )?;
    if !parsed.positionals.is_empty() && !parsed.options.contains("--list") {
        return None;
    }
    let mut result = SafeCommandParse::new(Some(SafeShellPresentation::List));
    result.detail =
        Some(SafePresentationDetail::new("List git branches").arguments(parsed.positionals, false));
    Some(result)
}
