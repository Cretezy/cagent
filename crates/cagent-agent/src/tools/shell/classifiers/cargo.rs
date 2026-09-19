#![allow(clippy::question_mark)] // The classifier distinguishes unsupported syntax from absent options explicitly.

use super::super::*;

/// `cargo fmt --check` delegates to rustfmt's verification mode and does not
/// rewrite workspace sources. Keep this deliberately narrower than the
/// generally-safe Cargo grammar: Cargo subcommands can otherwise run build
/// scripts, tests, or arbitrary project code.
pub(super) fn parse_cargo(args: &[String]) -> Option<SafeCommandParse> {
    let [subcommand, rest @ ..] = args else {
        return None;
    };
    if subcommand != "fmt" {
        return None;
    }
    let delimiter = rest.iter().position(|argument| argument == "--");
    let (options, forwarded) = delimiter.map_or((rest, &[][..]), |index| {
        (&rest[..index], &rest[index + 1..])
    });
    let Some(manifest_paths) = parse_fmt_options(options) else {
        return None;
    };
    let forwarded_is_check = matches!(forwarded, [check] if check == "--check");
    let check = options.iter().any(|option| option == "--check") || forwarded_is_check;
    if !check
        || (!forwarded_is_check
            && forwarded
                .iter()
                .any(|argument| argument.is_empty() || argument.starts_with('-')))
        || (forwarded_is_check && options.iter().any(|option| option == "--check"))
    {
        return None;
    }
    let mut parsed = SafeCommandParse::new(None);
    for path in manifest_paths {
        parsed = parsed.path(path, "Cargo manifest");
    }
    for path in forwarded {
        if path != "--check" {
            parsed = parsed.path(path.clone(), "rustfmt input");
        }
    }
    Some(parsed)
}

fn parse_fmt_options(options: &[String]) -> Option<Vec<String>> {
    let mut manifests = Vec::new();
    let mut index = 0;
    while index < options.len() {
        match options[index].as_str() {
            "--check" | "--all" | "--quiet" | "--verbose" | "-q" => index += 1,
            "-p" | "--package" => {
                if !next_value_is_safe(options, index) {
                    return None;
                }
                index += 2;
            }
            option if option.starts_with("--package=") => {
                if !attached_value_is_safe(option, "--package=") {
                    return None;
                }
                index += 1;
            }
            "--manifest-path" => {
                if !next_value_is_safe(options, index) {
                    return None;
                }
                manifests.push(options[index + 1].clone());
                index += 2;
            }
            option if option.starts_with("--manifest-path=") => {
                if !attached_value_is_safe(option, "--manifest-path=") {
                    return None;
                }
                manifests.push(option["--manifest-path=".len()..].to_owned());
                index += 1;
            }
            "--message-format" => {
                if !matches!(
                    options.get(index + 1).map(String::as_str),
                    Some("human" | "short" | "json")
                ) {
                    return None;
                }
                index += 2;
            }
            option if option.starts_with("--message-format=") => {
                if !matches!(
                    &option["--message-format=".len()..],
                    "human" | "short" | "json"
                ) {
                    return None;
                }
                index += 1;
            }
            "--color" => {
                if !matches!(
                    options.get(index + 1).map(String::as_str),
                    Some("auto" | "always" | "never")
                ) {
                    return None;
                }
                index += 2;
            }
            option if option.starts_with("--color=") => {
                if !matches!(&option["--color=".len()..], "auto" | "always" | "never") {
                    return None;
                }
                index += 1;
            }
            _ => return None,
        }
    }
    Some(manifests)
}

fn next_value_is_safe(args: &[String], index: usize) -> bool {
    args.get(index + 1)
        .is_some_and(|value| !value.is_empty() && !value.starts_with('-'))
}

fn attached_value_is_safe(argument: &str, prefix: &str) -> bool {
    argument
        .strip_prefix(prefix)
        .is_some_and(|value| !value.is_empty() && !value.starts_with('-'))
}
