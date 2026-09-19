pub(super) fn cargo(args: &[String]) -> bool {
    let Some((outer, forwarded)) = split_forwarded_args(args) else {
        return false;
    };
    let Some(command) = outer.first().map(String::as_str) else {
        return false;
    };
    match command {
        "build" => {
            forwarded.is_empty() && cargo_command_args_are_safe(&outer[1..], CargoCommand::Build)
        }
        "test" => {
            cargo_command_args_are_safe(&outer[1..], CargoCommand::Test)
                && forwarded_test_args_are_safe(forwarded)
        }
        "check" | "clippy" => {
            cargo_command_args_are_safe(&outer[1..], CargoCommand::Build)
                && forwarded_lint_args_are_safe(forwarded)
        }
        "fmt" => cargo_fmt_args_are_safe(&outer[1..], forwarded),
        "nextest" => nextest(&outer[1..]) && forwarded_test_args_are_safe(forwarded),
        _ => false,
    }
}

pub(super) fn nextest(args: &[String]) -> bool {
    let Some((outer, forwarded)) = split_forwarded_args(args) else {
        return false;
    };
    matches!(outer.first().map(String::as_str), Some("run"))
        && nextest_args_are_safe(&outer[1..])
        && forwarded_test_args_are_safe(forwarded)
}

pub(super) fn rustfmt(args: &[String]) -> bool {
    let mut index = 0;
    let mut has_input = false;
    let mut positional_only = false;
    while index < args.len() {
        let arg = &args[index];
        if positional_only {
            has_input = true;
            index += 1;
            continue;
        }
        if arg == "--" {
            positional_only = true;
            index += 1;
        } else if matches!(
            arg.as_str(),
            "--check" | "--backup" | "--verbose" | "--quiet"
        ) {
            index += 1;
        } else if let Some((name, value)) = arg.split_once('=') {
            if !rustfmt_option_value_is_safe(name, value) {
                return false;
            }
            index += 1;
        } else if matches!(
            arg.as_str(),
            "--edition" | "--style-edition" | "--emit" | "--color" | "--config" | "--config-path"
        ) {
            let Some(value) = args.get(index + 1) else {
                return false;
            };
            if !rustfmt_option_value_is_safe(arg, value) {
                return false;
            }
            index += 2;
        } else if arg.starts_with('-') {
            return false;
        } else {
            has_input = true;
            index += 1;
        }
    }
    has_input
}

fn rustfmt_option_value_is_safe(option: &str, value: &str) -> bool {
    match option {
        "--edition" | "--style-edition" => matches!(value, "2015" | "2018" | "2021" | "2024"),
        "--emit" => matches!(value, "files" | "stdout"),
        "--color" => matches!(value, "always" | "never" | "auto"),
        "--config" | "--config-path" => !value.is_empty() && !value.starts_with('-'),
        _ => false,
    }
}

fn split_forwarded_args(args: &[String]) -> Option<(&[String], &[String])> {
    let Some(index) = args.iter().position(|arg| arg == "--") else {
        return Some((args, &[]));
    };
    let forwarded = &args[index + 1..];
    (!forwarded.iter().any(|arg| arg == "--")).then_some((&args[..index], forwarded))
}

fn cargo_fmt_args_are_safe(outer: &[String], forwarded: &[String]) -> bool {
    let mut index = 0;
    while index < outer.len() {
        match outer[index].as_str() {
            "--check" | "--all" | "--verbose" | "--quiet" | "-q" => index += 1,
            "-p" | "--package" => {
                if !next_value_is_safe(outer, index) {
                    return false;
                }
                index += 2;
            }
            option if option.starts_with("--package=") => {
                if !attached_value_is_safe(option, "--package=") {
                    return false;
                }
                index += 1;
            }
            "--manifest-path" => {
                if !next_value_is_safe(outer, index) {
                    return false;
                }
                index += 2;
            }
            option if option.starts_with("--manifest-path=") => {
                if !attached_value_is_safe(option, "--manifest-path=") {
                    return false;
                }
                index += 1;
            }
            "--message-format" => {
                if !matches!(
                    outer.get(index + 1).map(String::as_str),
                    Some("human" | "short" | "json")
                ) {
                    return false;
                }
                index += 2;
            }
            option if option.starts_with("--message-format=") => {
                if !matches!(
                    &option["--message-format=".len()..],
                    "human" | "short" | "json"
                ) {
                    return false;
                }
                index += 1;
            }
            "--color" => {
                if !color_value_is_safe(outer.get(index + 1).map(String::as_str)) {
                    return false;
                }
                index += 2;
            }
            option if option.starts_with("--color=") => {
                if !color_value_is_safe(Some(&option["--color=".len()..])) {
                    return false;
                }
                index += 1;
            }
            _ => return false,
        }
    }
    forwarded.iter().all(|arg| {
        matches!(arg.as_str(), "--check" | "--verbose" | "--quiet" | "-q")
            || (!arg.is_empty() && !arg.starts_with('-'))
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CargoCommand {
    Test,
    Build,
}

fn cargo_command_args_are_safe(args: &[String], command: CargoCommand) -> bool {
    let mut index = 0;
    while index < args.len() {
        let argument = args[index].as_str();
        match argument {
            "--workspace"
            | "--all"
            | "--lib"
            | "--bins"
            | "--examples"
            | "--tests"
            | "--benches"
            | "--all-targets"
            | "--all-features"
            | "--no-default-features"
            | "--release"
            | "-r"
            | "--locked"
            | "--offline"
            | "--frozen"
            | "--keep-going"
            | "--future-incompat-report"
            | "--ignore-rust-version"
            | "--verbose"
            | "-v"
            | "--quiet"
            | "-q" => index += 1,
            "--no-run" | "--no-fail-fast" | "--doc" if command == CargoCommand::Test => index += 1,
            "-p" | "--package" | "--exclude" | "--bin" | "--example" | "--test" | "--bench"
            | "-F" | "--features" | "--profile" | "--target" | "-m" | "--manifest-path"
            | "--target-dir" => {
                if !next_value_is_safe(args, index) {
                    return false;
                }
                index += 2;
            }
            "--jobs" | "-j" => {
                if !args
                    .get(index + 1)
                    .is_some_and(|value| positive_integer(value))
                {
                    return false;
                }
                index += 2;
            }
            "--color" => {
                if !color_value_is_safe(args.get(index + 1).map(String::as_str)) {
                    return false;
                }
                index += 2;
            }
            "--message-format" => {
                if !args
                    .get(index + 1)
                    .is_some_and(|value| cargo_message_format_is_safe(value))
                {
                    return false;
                }
                index += 2;
            }
            "--timings" => index += 1,
            value if value.starts_with("-p") && value.len() > 2 => {
                if value[2..].starts_with('-') {
                    return false;
                }
                index += 1;
            }
            value if value.starts_with("-j") && value.len() > 2 => {
                if !positive_integer(&value[2..]) {
                    return false;
                }
                index += 1;
            }
            value if value.starts_with("-F") && value.len() > 2 => {
                if value[2..].starts_with('-') {
                    return false;
                }
                index += 1;
            }
            value if cargo_attached_value_is_safe(value) => index += 1,
            "--timings=html" => index += 1,
            value if command == CargoCommand::Test && !value.starts_with('-') => index += 1,
            _ => return false,
        }
    }
    true
}

fn nextest_args_are_safe(args: &[String]) -> bool {
    let mut cargo_args = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-E" | "--filter-expr" => {
                if !next_value_is_safe(args, index) {
                    return false;
                }
                index += 2;
            }
            value if value.starts_with("--filter-expr=") => {
                if !attached_value_is_safe(value, "--filter-expr=") {
                    return false;
                }
                index += 1;
            }
            value => {
                cargo_args.push(value.to_owned());
                index += 1;
            }
        }
    }
    cargo_command_args_are_safe(&cargo_args, CargoCommand::Build)
}

fn cargo_attached_value_is_safe(argument: &str) -> bool {
    const OPTIONS: &[&str] = &[
        "--package=",
        "--exclude=",
        "--bin=",
        "--example=",
        "--test=",
        "--bench=",
        "--features=",
        "--profile=",
        "--target=",
        "--manifest-path=",
        "--target-dir=",
    ];
    if OPTIONS
        .iter()
        .any(|option| attached_value_is_safe(argument, option))
    {
        return true;
    }
    if let Some(value) = argument.strip_prefix("--jobs=") {
        return positive_integer(value);
    }
    if let Some(value) = argument.strip_prefix("--color=") {
        return color_value_is_safe(Some(value));
    }
    if let Some(value) = argument.strip_prefix("--message-format=") {
        return cargo_message_format_is_safe(value);
    }
    false
}

fn cargo_message_format_is_safe(value: &str) -> bool {
    matches!(
        value,
        "human"
            | "short"
            | "json"
            | "json-diagnostic-short"
            | "json-diagnostic-rendered-ansi"
            | "json-render-diagnostics"
    )
}

fn color_value_is_safe(value: Option<&str>) -> bool {
    matches!(value, Some("auto" | "always" | "never"))
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

fn positive_integer(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|character| character.is_ascii_digit()) && value != "0"
}

fn forwarded_test_args_are_safe(args: &[String]) -> bool {
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        match arg.as_str() {
            "--nocapture" | "--show-output" | "--ignored" | "--include-ignored" | "--exact"
            | "--list" | "--help" | "-h" => index += 1,
            "--test-threads" => {
                let Some(value) = args.get(index + 1) else {
                    return false;
                };
                if value.starts_with('-')
                    || value.is_empty()
                    || !value.chars().all(|c| c.is_ascii_digit())
                {
                    return false;
                }
                index += 2;
            }
            value if value.starts_with("--test-threads=") => {
                let value = &value["--test-threads=".len()..];
                if value.is_empty() || !value.chars().all(|character| character.is_ascii_digit()) {
                    return false;
                }
                index += 1;
            }
            "--skip" => {
                let Some(value) = args.get(index + 1) else {
                    return false;
                };
                if value.starts_with('-') || value.is_empty() {
                    return false;
                }
                index += 2;
            }
            value if value.starts_with("--skip=") => {
                if value["--skip=".len()..].is_empty() {
                    return false;
                }
                index += 1;
            }
            "--format" => {
                if !matches!(
                    args.get(index + 1).map(String::as_str),
                    Some("pretty" | "terse" | "json" | "junit")
                ) {
                    return false;
                }
                index += 2;
            }
            value if value.starts_with("--format=") => {
                if !matches!(
                    &value["--format=".len()..],
                    "pretty" | "terse" | "json" | "junit"
                ) {
                    return false;
                }
                index += 1;
            }
            "--color" => {
                if !matches!(
                    args.get(index + 1).map(String::as_str),
                    Some("auto" | "always" | "never")
                ) {
                    return false;
                }
                index += 2;
            }
            value if value.starts_with("--color=") => {
                if !matches!(&value["--color=".len()..], "auto" | "always" | "never") {
                    return false;
                }
                index += 1;
            }
            value if !value.starts_with('-') => index += 1,
            _ => return false,
        }
    }
    true
}

fn forwarded_lint_args_are_safe(args: &[String]) -> bool {
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        let takes_value = matches!(arg.as_str(), "-A" | "-W" | "-D" | "-F")
            || matches!(arg.as_str(), "--allow" | "--warn" | "--deny" | "--forbid");
        let attached = ["-A", "-W", "-D", "-F"]
            .iter()
            .any(|prefix| arg.starts_with(prefix) && arg.len() > prefix.len())
            || ["--allow=", "--warn=", "--deny=", "--forbid="]
                .iter()
                .any(|prefix| arg.starts_with(prefix) && arg.len() > prefix.len());
        if !takes_value && !attached {
            return false;
        }
        if takes_value {
            index += 1;
            if args.get(index).is_none_or(|value| value.starts_with('-')) {
                return false;
            }
        }
        index += 1;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::cargo;

    fn args(arguments: &[&str]) -> Vec<String> {
        arguments
            .iter()
            .map(|argument| (*argument).into())
            .collect()
    }

    #[test]
    fn cargo_fmt_accepts_literal_package_selectors() {
        for arguments in [
            &["fmt", "-p", "cagent-cli", "--", "--check"][..],
            &["fmt", "--package", "cagent-agent", "--check"][..],
            &["fmt", "--package=cagent-agent", "--check"][..],
        ] {
            assert!(cargo(&args(arguments)), "{arguments:?}");
        }
    }

    #[test]
    fn cargo_fmt_rejects_missing_package_values() {
        for arguments in [
            &["fmt", "-p", "--", "--check"][..],
            &["fmt", "--package", "--check"][..],
            &["fmt", "--package=", "--check"][..],
        ] {
            assert!(!cargo(&args(arguments)), "{arguments:?}");
        }
    }

    #[test]
    fn cargo_commands_accept_curated_common_options() {
        for arguments in [
            &["test", "-p", "cagent-agent", "-p", "cagent-cli", "--no-run"][..],
            &["test", "--no-fail-fast", "--doc"][..],
            &[
                "test",
                "--workspace",
                "--exclude",
                "slow",
                "--all-features",
                "--locked",
            ][..],
            &["test", "-pfoo", "-j4", "filter"][..],
            &[
                "check",
                "--manifest-path",
                "crates/foo/Cargo.toml",
                "--target-dir=target/check",
            ][..],
            &[
                "check",
                "--features=serde,tokio",
                "--target",
                "x86_64-unknown-linux-gnu",
            ][..],
            &[
                "check",
                "--examples",
                "--example",
                "demo",
                "-Fserde",
                "-r",
                "--future-incompat-report",
                "--ignore-rust-version",
            ][..],
            &[
                "clippy",
                "--all-targets",
                "--message-format=json",
                "--",
                "-D",
                "warnings",
            ][..],
            &["clippy", "--timings=html", "--color", "always"][..],
            &["nextest", "run", "-p", "foo", "-E", "test(foo)"][..],
        ] {
            assert!(cargo(&args(arguments)), "{arguments:?}");
        }
    }

    #[test]
    fn cargo_commands_reject_unknown_or_malformed_options() {
        for arguments in [
            &["test", "--unknown"][..],
            &["check", "--package", "--release"][..],
            &["check", "--jobs=0"][..],
            &["clippy", "--config", "build.rustflags=[]"][..],
            &["test", "-Zunstable-options"][..],
            &["nextest", "archive"][..],
            &["nextest", "run", "--filter-expr="][..],
        ] {
            assert!(!cargo(&args(arguments)), "{arguments:?}");
        }
    }

    #[test]
    fn direct_nextest_accepts_one_forwarded_test_argument_region() {
        assert!(super::nextest(&args(&[
            "run",
            "--workspace",
            "--",
            "--nocapture"
        ])));
        assert!(!super::nextest(&args(&[
            "run",
            "--",
            "--nocapture",
            "--",
            "--exact"
        ])));
    }
}
