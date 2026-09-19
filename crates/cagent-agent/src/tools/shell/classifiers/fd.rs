use super::super::*;
use super::parsing::*;

pub(super) fn parse_fd(args: &[String]) -> Option<SafeCommandParse> {
    let grammar = OptionGrammar {
        short_flags: "aHIuig0p",
        short_values: "teEdS",
        long_flags: &[
            "--hidden",
            "--no-ignore",
            "--ignore-case",
            "--case-sensitive",
            "--glob",
            "--fixed-strings",
            "--absolute-path",
            "--print0",
            "--print-zero",
            "--full-path",
            "--list-details",
            "--strip-cwd-prefix",
            "--one-file-system",
        ],
        long_values: &[
            "--type",
            "--extension",
            "--exclude",
            "--max-depth",
            "--min-depth",
            "--size",
            "--changed-within",
            "--changed-before",
            "--owner",
            "--format",
            "--path-separator",
            "--color",
            "--threads",
            "--max-results",
            "--hyperlink",
        ],
        path_long_values: &[
            ("--base-directory", "base directory"),
            ("--ignore-file", "ignore file"),
        ],
        path_short_values: EMPTY_PATH_SHORT,
    };
    let parsed = parse_options(args, &grammar)?;
    let targets = fd_display_targets(args, &parsed);
    let mut result = SafeCommandParse::new(Some(SafeShellPresentation::List));
    result.targets = targets;
    for (value, source) in parsed.path_values {
        result = result.path(value, source);
    }
    for root in parsed.positionals.iter().skip(1) {
        result = result.path(root.clone(), "search root");
    }
    if parsed.positionals.len() <= 1 {
        result = result.path(".", "default search root");
    }
    if !parsed.options.iter().any(|option| {
        matches!(
            option.as_str(),
            "-0" | "--print0"
                | "--print-zero"
                | "--format"
                | "--list-details"
                | "--color"
                | "--hyperlink"
                | "--path-separator"
        )
    }) {
        result.path_list_output = Some(SafePathListOutput::Lines);
    }
    Some(result)
}

fn fd_display_targets(args: &[String], parsed: &ParsedArgs) -> Vec<String> {
    let pattern = parsed
        .positionals
        .first()
        .filter(|pattern| pattern.as_str() != ".")
        .cloned();
    if pattern.is_some() {
        return pattern.into_iter().collect();
    }

    let mut extensions = Vec::new();
    let mut index = 0;
    let mut options = true;
    while index < args.len() {
        let arg = &args[index];
        if options && arg == "--" {
            options = false;
            index += 1;
            continue;
        }
        let value = if options && arg == "--extension" {
            index += 1;
            args.get(index).cloned()
        } else if options && let Some(value) = arg.strip_prefix("--extension=") {
            Some(value.to_owned())
        } else if options && arg == "-e" {
            index += 1;
            args.get(index).cloned()
        } else if options {
            arg.strip_prefix("-e")
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        } else {
            None
        };
        if let Some(value) = value {
            let value = value.trim_start_matches('.');
            if !value.is_empty() {
                extensions.push(format!("*.{value}"));
            }
        }
        index += 1;
    }
    extensions
}
