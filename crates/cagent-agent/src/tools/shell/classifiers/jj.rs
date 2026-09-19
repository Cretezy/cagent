use super::super::*;

/// Strict read-only Jujutsu inspection forms.
///
/// Jujutsu snapshots the working copy when opening a repository, so this is a
/// user-approved inspection policy rather than a guarantee of zero metadata
/// writes. Keep the grammar literal: configuration, templates, external diff
/// tools, and every mutation command remain ordinary Bash.
pub(super) fn parse_jj(args: &[String]) -> Option<SafeCommandParse> {
    let args = strip_global_flags(args, true)?;
    parse_jj_inspection(&args)
}

pub(super) fn parse_jj_level1(args: &[String]) -> Option<SafeCommandParse> {
    let args = strip_global_flags(args, false)?;
    parse_jj_inspection(&args)
}

fn parse_jj_inspection(args: &[String]) -> Option<SafeCommandParse> {
    match args {
        [command] if matches!(command.as_str(), "status" | "st") => rich(Read, "Read jj status"),
        [command] if command == "root" => rich(Read, "Read jj root"),
        [command, rest @ ..] if command == "diff" => parse_diff(rest),
        [command, rest @ ..] if command == "log" => parse_log(rest),
        [command, rest @ ..] if command == "show" => parse_show(rest),
        [file, command, rest @ ..] if file == "file" && command == "show" => {
            parse_revision_paths(rest, 1)
        }
        [file, command, rest @ ..] if file == "file" && command == "list" => {
            parse_revision_paths(rest, 0)
        }
        [file, command, rest @ ..] if file == "file" && command == "annotate" => {
            parse_revision_paths(rest, 1).map(|mut parsed| {
                if let Some(detail) = &mut parsed.detail {
                    detail.label = "Read jj annotate".into();
                }
                parsed
            })
        }
        [file, command, rest @ ..] if file == "file" && command == "search" => {
            parse_file_search(rest)
        }
        [command, subcommand]
            if matches!(
                (command.as_str(), subcommand.as_str()),
                ("bookmark", "list") | ("workspace", "list") | ("operation", "log")
            ) =>
        {
            rich(
                List,
                match command.as_str() {
                    "bookmark" => "List jj bookmarks",
                    "workspace" => "List jj workspaces",
                    _ => "List jj operations",
                },
            )
        }
        [workspace, root] if workspace == "workspace" && root == "root" => {
            rich(Read, "Read jj root")
        }
        [command, subcommand, flag]
            if command == "bookmark" && subcommand == "list" && flag == "--all" =>
        {
            rich(List, "List jj bookmarks")
        }
        [command, subcommand, operation] if command == "operation" && subcommand == "show" => {
            valid_value(operation)
                .and_then(|operation| rich_args(Read, "Read jj operation", [operation], false))
        }
        [command, subcommand, flag, operation]
            if command == "operation" && subcommand == "show" && flag == "-o" =>
        {
            valid_value(operation)
                .and_then(|operation| rich_args(Read, "Read jj operation", [operation], false))
        }
        _ => None,
    }
}

fn strip_global_flags(args: &[String], require_ignore: bool) -> Option<Vec<String>> {
    let mut ignored = false;
    let mut pager = false;
    let mut command = Vec::with_capacity(args.len());
    for argument in args {
        match argument.as_str() {
            // `--ignore-working-copy` affects how jj opens the repository, so
            // keep it a true leading global flag. `--no-pager` only controls
            // presentation and jj accepts it after a subcommand as well.
            "--ignore-working-copy" if !ignored && command.is_empty() => ignored = true,
            "--no-pager" if !pager => pager = true,
            "--ignore-working-copy" | "--no-pager" => return None,
            _ => command.push(argument.clone()),
        }
    }
    (ignored == require_ignore).then_some(command)
}

fn parse_diff(args: &[String]) -> Option<SafeCommandParse> {
    let (mut parsed, revisions) = parse_paths_with_options(
        args,
        0,
        &[
            "-r",
            "--revision",
            "--revisions",
            "-f",
            "--from",
            "-t",
            "--to",
        ],
        &[
            "--git",
            "--summary",
            "--stat",
            "--types",
            "--name-only",
            "--color-words",
            "-w",
            "--ignore-all-space",
            "-b",
            "--ignore-space-change",
        ],
        &["--context"],
        &[],
    )?;
    parsed.presentation = Some(SafeShellPresentation::Read);
    parsed.detail = Some(
        SafePresentationDetail::new("Read jj diff")
            .arguments(
                parsed.operands.iter().map(|operand| operand.value.clone()),
                true,
            )
            .relation("in", revisions, false),
    );
    Some(parsed)
}

fn parse_log(args: &[String]) -> Option<SafeCommandParse> {
    let (mut parsed, revisions) = parse_paths_with_options(
        args,
        0,
        &["-r", "--revision", "--revisions"],
        &[
            "-p",
            "--patch",
            "--reversed",
            "-G",
            "--no-graph",
            "--count",
            "--summary",
            "--stat",
            "--types",
            "--name-only",
            "--git",
            "--color-words",
            "--ignore-all-space",
            "--ignore-space-change",
        ],
        &["-n", "--limit", "--context"],
        &["-T", "--template"],
    )?;
    parsed.presentation = Some(SafeShellPresentation::Read);
    parsed.detail = Some(SafePresentationDetail::new("Read jj log").arguments(revisions, false));
    Some(parsed)
}

fn parse_show(args: &[String]) -> Option<SafeCommandParse> {
    let mut index = 0;
    let mut revision = None;
    while let Some(argument) = args.get(index) {
        match argument.as_str() {
            "--stat" | "--summary" | "--types" | "--name-only" | "--git" => {}
            "-r" | "--revision" => {
                index += 1;
                revision = Some(valid_value(args.get(index)?)?);
            }
            value if value.starts_with('-') => return None,
            _ if revision.is_none() => revision = Some(valid_value(argument)?),
            _ => return None,
        }
        index += 1;
    }
    revision.and_then(|revision| rich_args(Read, "Read jj", [revision], false))
}

fn parse_revision_paths(args: &[String], minimum_paths: usize) -> Option<SafeCommandParse> {
    let (mut parsed, revisions) =
        parse_paths_with_options(args, minimum_paths, &["-r", "--revision"], &[], &[], &[])?;
    let (kind, label) = if minimum_paths == 0 {
        (SafeShellPresentation::List, "List jj files")
    } else {
        (SafeShellPresentation::Read, "Read jj file")
    };
    parsed.presentation = Some(kind);
    parsed.detail = Some(
        SafePresentationDetail::new(label)
            .arguments(
                parsed.operands.iter().map(|operand| operand.value.clone()),
                true,
            )
            .relation("in", revisions, false),
    );
    Some(parsed)
}

fn parse_file_search(args: &[String]) -> Option<SafeCommandParse> {
    let mut index = 0;
    let mut positional_only = false;
    let mut pattern = None;
    let mut revision = None;
    let mut paths = Vec::new();
    while let Some(argument) = args.get(index) {
        if !positional_only && argument == "--" {
            positional_only = true;
        } else if !positional_only && matches!(argument.as_str(), "-r" | "--revision") {
            index += 1;
            revision = Some(valid_value(args.get(index)?)?);
        } else if !positional_only && matches!(argument.as_str(), "-p" | "--pattern") {
            index += 1;
            pattern = Some(valid_value(args.get(index)?)?);
        } else if !positional_only
            && matches!(argument.as_str(), "--name-only" | "-n" | "--line-number")
        {
        } else if !positional_only && argument.starts_with('-') {
            return None;
        } else {
            paths.push(argument.clone());
        }
        index += 1;
    }
    let pattern = pattern?;
    let mut parsed = paths_to_parse(paths)?;
    parsed.presentation = Some(SafeShellPresentation::Search);
    parsed.detail = Some(
        SafePresentationDetail::new("Search jj files")
            .arguments([pattern], false)
            .relation(
                "in",
                parsed.operands.iter().map(|operand| operand.value.clone()),
                true,
            )
            .relation("at", revision, false),
    );
    Some(parsed)
}

fn parse_paths_with_options(
    args: &[String],
    minimum_paths: usize,
    revision_options: &[&str],
    flags: &[&str],
    value_options: &[&str],
    literal_value_options: &[&str],
) -> Option<(SafeCommandParse, Vec<String>)> {
    let mut index = 0;
    let mut positional_only = false;
    let mut paths = Vec::new();
    let mut revisions = Vec::new();
    while let Some(argument) = args.get(index) {
        if !positional_only && argument == "--" {
            positional_only = true;
        } else if !positional_only && revision_options.contains(&argument.as_str()) {
            index += 1;
            revisions.push(valid_value(args.get(index)?)?);
        } else if !positional_only && flags.contains(&argument.as_str()) {
        } else if !positional_only && value_options.contains(&argument.as_str()) {
            index += 1;
            let value = valid_value(args.get(index)?)?;
            if !value.chars().all(|character| character.is_ascii_digit()) {
                return None;
            }
        } else if !positional_only && literal_value_options.contains(&argument.as_str()) {
            index += 1;
            valid_value(args.get(index)?)?;
        } else if !positional_only && argument.starts_with('-') {
            return None;
        } else {
            paths.push(argument.clone());
        }
        index += 1;
    }
    (paths.len() >= minimum_paths)
        .then_some(paths)
        .and_then(paths_to_parse)
        .map(|parsed| (parsed, revisions))
}

use SafeShellPresentation::{List, Read};

fn rich(kind: SafeShellPresentation, label: &str) -> Option<SafeCommandParse> {
    rich_args(kind, label, [], false)
}

fn rich_args<const N: usize>(
    kind: SafeShellPresentation,
    label: &str,
    arguments: [String; N],
    path: bool,
) -> Option<SafeCommandParse> {
    let mut parsed = SafeCommandParse::new(Some(kind));
    parsed.detail = Some(SafePresentationDetail::new(label).arguments(arguments, path));
    Some(parsed)
}

fn paths_to_parse(paths: Vec<String>) -> Option<SafeCommandParse> {
    let mut parsed = SafeCommandParse::new(None);
    for path in paths {
        valid_value(&path)?;
        parsed = parsed.path(path, "Jujutsu fileset");
    }
    Some(parsed)
}

fn valid_value(value: &str) -> Option<String> {
    (!value.is_empty() && !value.starts_with('-')).then(|| value.to_owned())
}
