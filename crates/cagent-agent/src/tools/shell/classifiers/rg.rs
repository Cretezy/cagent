#![allow(clippy::collapsible_if, clippy::while_let_on_iterator)] // The parser's staged checks retain command-line grammar boundaries.

use super::super::*;
use super::parsing::*;

pub(super) fn parse_rg(args: &[String]) -> Option<SafeCommandParse> {
    let grammar = OptionGrammar {
        short_flags: "ivwxyFnHhloqscLbuaINpU0S",
        short_values: "eABCgmtrTM",
        long_flags: &[
            "--files",
            "--hidden",
            "--no-hidden",
            "--ignore-case",
            "--case-sensitive",
            "--smart-case",
            "--invert-match",
            "--word-regexp",
            "--line-regexp",
            "--fixed-strings",
            "--line-number",
            "--with-filename",
            "--no-filename",
            "--only-matching",
            "--column",
            "--no-column",
            "--heading",
            "--no-heading",
            "--quiet",
            "--stats",
            "--trim",
            "--max-columns-preview",
            "--glob-case-insensitive",
            "--ignore-file-case-insensitive",
            "--type-list",
            "--count",
            "--count-matches",
            "--files-with-matches",
            "--files-without-match",
            "--byte-offset",
            "--text",
            "--binary",
            "--no-ignore",
            "--no-ignore-vcs",
            "--no-ignore-parent",
            "--multiline",
            "--multiline-dotall",
            "--crlf",
            "--null",
            "--null-data",
            "--pcre2",
            "--unicode",
            "--no-unicode",
            "--sort-files",
        ],
        long_values: &[
            "--regexp",
            "--after-context",
            "--before-context",
            "--context",
            "--max-count",
            "--max-columns",
            "--threads",
            "--glob",
            "--iglob",
            "--type",
            "--type-not",
            "--type-add",
            "--max-depth",
            "--max-filesize",
            "--color",
            "--colors",
            "--sort",
            "--sortr",
            "--engine",
            "--encoding",
            "--context-separator",
            "--field-context-separator",
            "--field-match-separator",
        ],
        path_long_values: &[("--file", "pattern file"), ("--ignore-file", "ignore file")],
        path_short_values: &[('f', "pattern file")],
    };
    let mut parsed = parse_options(args, &grammar)?;
    let files = parsed.options.contains("--files");
    let type_list = parsed.options.contains("--type-list");
    let plain_path_output = files
        && parsed.options.iter().all(|option| {
            matches!(
                option.as_str(),
                "--files"
                    | "--hidden"
                    | "--no-hidden"
                    | "--no-ignore"
                    | "--no-ignore-vcs"
                    | "--no-ignore-parent"
                    | "--glob"
                    | "--iglob"
                    | "--glob-case-insensitive"
                    | "--ignore-file"
                    | "--ignore-file-case-insensitive"
                    | "--type"
                    | "--type-not"
                    | "--max-depth"
                    | "--max-filesize"
                    | "--sort"
                    | "--sortr"
                    | "--threads"
                    | "-g"
                    | "-t"
                    | "-T"
            )
        });
    let glob_patterns = if files {
        Some(rg_glob_patterns(args)?)
    } else {
        None
    };
    let explicit_pattern = parsed.options.contains("-e")
        || parsed.options.contains("--regexp")
        || parsed.options.contains("-f")
        || parsed.options.contains("--file");
    let query = if explicit_pattern {
        parsed.option_value(&["-e", "--regexp"])
    } else {
        parsed.positionals.first().cloned()
    };
    if (files || type_list) && explicit_pattern {
        return None;
    }
    if !files && !type_list && !explicit_pattern {
        if parsed.positionals.is_empty() {
            return None;
        }
        parsed.positionals.remove(0);
    }
    input_parse(
        parsed,
        if files || type_list {
            SafeShellPresentation::List
        } else {
            SafeShellPresentation::Search
        },
        0,
        None,
        true,
    )
    .map(|mut result| {
        result.targets = glob_patterns.unwrap_or_default();
        result.query = query;
        if plain_path_output {
            result.path_list_output = Some(SafePathListOutput::Lines);
        }
        result
    })
}

/// Returns a command-line form that makes a quoted, otherwise-unrecognized
/// leading-dash pattern unambiguous to ripgrep.
///
/// Shell quotes preserve the argument but do not stop `rg` from interpreting
/// it as an option.  We only normalize an *unrecognized* option: recognized
/// options, including those with a following value, retain their usual
/// meaning.
pub(crate) struct NormalizedRgInvocation {
    pub words: Vec<String>,
    pub source_words: Vec<String>,
}

pub(crate) fn normalize_quoted_leading_dash_pattern(
    words: &[String],
    source_words: &[String],
) -> Option<NormalizedRgInvocation> {
    let args = words.get(1..)?;
    let source_args = source_words.get(1..)?;
    let mut index = 0;

    while index < args.len() {
        let arg = &args[index];
        if arg == "--" || !arg.starts_with('-') || arg == "-" {
            return None;
        }
        if let Some(width) = rg_option_width(args, index) {
            index += width;
            continue;
        } else if arg.starts_with("--") {
            if quoted(source_args.get(index)?) {
                return normalize_quoted_pattern_at(words, source_words, index + 1);
            }
            return None;
        } else {
            return None;
        }
    }
    None
}

fn normalize_quoted_pattern_at(
    words: &[String],
    source_words: &[String],
    pattern_index: usize,
) -> Option<NormalizedRgInvocation> {
    let mut prefix = words[..pattern_index].to_vec();
    let mut prefix_source = source_words[..pattern_index].to_vec();
    let mut remainder = Vec::new();
    let mut remainder_source = Vec::new();
    let mut index = pattern_index;
    while index < words.len() {
        if index != pattern_index {
            if let Some(width) = rg_option_width(words, index) {
                prefix.extend_from_slice(words.get(index..index + width)?);
                prefix_source.extend_from_slice(source_words.get(index..index + width)?);
                index += width;
                continue;
            }
        }
        remainder.push(words[index].clone());
        remainder_source.push(source_words[index].clone());
        index += 1;
    }
    prefix.push("--".into());
    prefix_source.push("--".into());
    prefix.extend(remainder);
    prefix_source.extend(remainder_source);
    Some(NormalizedRgInvocation {
        words: prefix,
        source_words: prefix_source,
    })
}

fn rg_option_width(args: &[String], index: usize) -> Option<usize> {
    let arg = args.get(index)?;
    if arg == "--" || !arg.starts_with('-') || arg == "-" {
        return None;
    }
    if let Some((name, _)) = arg.split_once('=') {
        return (RG_LONG_FLAGS.contains(&name) || RG_LONG_VALUES.contains(&name)).then_some(1);
    }
    if RG_LONG_FLAGS.contains(&arg.as_str()) {
        return Some(1);
    }
    if RG_LONG_VALUES.contains(&arg.as_str()) {
        return args.get(index + 1).map(|_| 2);
    }
    if arg.starts_with("--") {
        return None;
    }

    let mut chars = arg[1..].char_indices().peekable();
    while let Some((_offset, option)) = chars.next() {
        if RG_SHORT_FLAGS.contains(option) {
            continue;
        }
        if RG_SHORT_VALUES.contains(option) {
            return if chars.peek().is_some() {
                Some(1)
            } else {
                args.get(index + 1).map(|_| 2)
            };
        }
        return None;
    }
    Some(1)
}

const RG_SHORT_FLAGS: &str = "ivwxyFnHhloqscLbuaINpU0S";
const RG_SHORT_VALUES: &str = "eABCgmtrTM";
const RG_LONG_FLAGS: &[&str] = &[
    "--files",
    "--hidden",
    "--no-hidden",
    "--ignore-case",
    "--case-sensitive",
    "--smart-case",
    "--invert-match",
    "--word-regexp",
    "--line-regexp",
    "--fixed-strings",
    "--line-number",
    "--with-filename",
    "--no-filename",
    "--only-matching",
    "--column",
    "--no-column",
    "--heading",
    "--no-heading",
    "--quiet",
    "--stats",
    "--trim",
    "--max-columns-preview",
    "--glob-case-insensitive",
    "--ignore-file-case-insensitive",
    "--type-list",
    "--count",
    "--count-matches",
    "--files-with-matches",
    "--files-without-match",
    "--byte-offset",
    "--text",
    "--binary",
    "--no-ignore",
    "--no-ignore-vcs",
    "--no-ignore-parent",
    "--multiline",
    "--multiline-dotall",
    "--crlf",
    "--null",
    "--null-data",
    "--pcre2",
    "--unicode",
    "--no-unicode",
    "--sort-files",
];
const RG_LONG_VALUES: &[&str] = &[
    "--regexp",
    "--after-context",
    "--before-context",
    "--context",
    "--max-count",
    "--max-columns",
    "--threads",
    "--glob",
    "--iglob",
    "--type",
    "--type-not",
    "--type-add",
    "--max-depth",
    "--max-filesize",
    "--color",
    "--colors",
    "--sort",
    "--sortr",
    "--engine",
    "--encoding",
    "--context-separator",
    "--field-context-separator",
    "--field-match-separator",
    "--file",
    "--ignore-file",
];

fn quoted(source: &str) -> bool {
    source.starts_with('\'') || source.starts_with('"')
}

fn rg_glob_patterns(args: &[String]) -> Option<Vec<String>> {
    let mut patterns = Vec::new();
    let mut options = true;
    let mut index = 0;

    while index < args.len() {
        let arg = &args[index];
        if options && arg == "--" {
            options = false;
        } else if options && arg == "--glob" {
            index += 1;
            patterns.push(args.get(index)?.clone());
        } else if options && let Some(pattern) = arg.strip_prefix("--glob=") {
            if pattern.is_empty() {
                return None;
            }
            patterns.push(pattern.to_owned());
        } else if options && arg.starts_with('-') && arg != "-" {
            let mut chars = arg[1..].char_indices();
            while let Some((offset, option)) = chars.next() {
                if option == 'g' {
                    let value_start = 1 + offset + option.len_utf8();
                    let pattern = if value_start < arg.len() {
                        arg[value_start..].to_owned()
                    } else {
                        index += 1;
                        args.get(index)?.clone()
                    };
                    if pattern.is_empty() {
                        return None;
                    }
                    patterns.push(pattern);
                    break;
                }
                if "eABCgmtrT".contains(option) {
                    break;
                }
            }
        }
        index += 1;
    }

    Some(patterns)
}
