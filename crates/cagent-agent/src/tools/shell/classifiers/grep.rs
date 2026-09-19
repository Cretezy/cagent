use super::super::*;
use super::parsing::*;

pub(super) fn parse_grep(args: &[String]) -> Option<SafeCommandParse> {
    let grammar = OptionGrammar {
        short_flags: "EFGPivwxnHhoqscLlbarIZ",
        short_values: "emABC",
        long_flags: &[
            "--extended-regexp",
            "--fixed-strings",
            "--basic-regexp",
            "--perl-regexp",
            "--ignore-case",
            "--invert-match",
            "--word-regexp",
            "--line-regexp",
            "--line-number",
            "--with-filename",
            "--no-filename",
            "--only-matching",
            "--quiet",
            "--silent",
            "--no-messages",
            "--line-buffered",
            "--count",
            "--files-with-matches",
            "--files-without-match",
            "--byte-offset",
            "--text",
            "--binary-files",
            "--recursive",
            "--null",
            "--null-data",
        ],
        long_values: &[
            "--regexp",
            "--max-count",
            "--after-context",
            "--before-context",
            "--context",
            "--include",
            "--exclude",
            "--exclude-dir",
            "--label",
            "--directories",
            "--devices",
            "--binary-files",
            "--color",
        ],
        path_long_values: &[
            ("--file", "pattern file"),
            ("--exclude-from", "exclude file"),
        ],
        path_short_values: &[('f', "pattern file")],
    };
    let mut parsed = parse_options(args, &grammar)?;
    let has_explicit_pattern = parsed.options.contains("-e")
        || parsed.options.contains("--regexp")
        || parsed.options.contains("-f")
        || parsed.options.contains("--file");
    let query = if has_explicit_pattern {
        parsed.option_value(&["-e", "--regexp"])
    } else {
        parsed.positionals.first().cloned()
    };
    if !has_explicit_pattern {
        if parsed.positionals.is_empty() {
            return None;
        }
        parsed.positionals.remove(0);
    }
    input_parse(parsed, SafeShellPresentation::Search, 0, None, false).map(|mut result| {
        result.query = query;
        result
    })
}
