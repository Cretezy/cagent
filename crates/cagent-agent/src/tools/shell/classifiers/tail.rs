use super::super::*;
use super::parsing::*;

pub(super) fn parse_tail(args: &[String]) -> Option<SafeCommandParse> {
    let normalized = normalize_line_count_args(args);
    input_parse(
        parse_options(
            &normalized,
            &grammar(
                "fqvz",
                "nc",
                &[
                    "--follow",
                    "--quiet",
                    "--silent",
                    "--verbose",
                    "--zero-terminated",
                ],
                &["--lines", "--bytes", "--sleep-interval"],
            ),
        )?,
        SafeShellPresentation::Read,
        0,
        None,
        false,
    )
}

pub(super) fn normalize_line_count_args(args: &[String]) -> Vec<String> {
    let mut normalized = Vec::with_capacity(args.len());
    let mut options = true;
    for arg in args {
        if options && arg == "--" {
            options = false;
            normalized.push(arg.clone());
        } else if options
            && arg.len() > 1
            && arg.starts_with('-')
            && arg[1..].chars().all(|character| character.is_ascii_digit())
        {
            normalized.push("-n".into());
            normalized.push(arg[1..].into());
        } else {
            normalized.push(arg.clone());
        }
    }
    normalized
}
