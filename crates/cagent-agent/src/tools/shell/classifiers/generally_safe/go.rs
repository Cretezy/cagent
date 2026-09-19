pub(super) fn go(args: &[String]) -> bool {
    match args.first().map(String::as_str) {
        Some("build") => go_build(&args[1..]),
        Some("test" | "fmt" | "vet") => !args[1..].iter().any(|arg| {
            matches!(
                arg.as_str(),
                "-exec" | "-toolexec" | "-modfile" | "-vettool"
            ) || arg.starts_with("-exec=")
                || arg.starts_with("-toolexec=")
                || arg.starts_with("-modfile=")
                || arg.starts_with("-vettool=")
        }),
        _ => false,
    }
}

fn go_build(args: &[String]) -> bool {
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        if matches!(
            arg,
            "-a" | "-n"
                | "-race"
                | "-msan"
                | "-asan"
                | "-cover"
                | "-trimpath"
                | "-v"
                | "-work"
                | "-x"
        ) {
            index += 1;
        } else if matches!(arg, "-tags" | "-o" | "-buildmode") {
            if !args
                .get(index + 1)
                .is_some_and(|value| option_value_is_safe(value))
            {
                return false;
            }
            index += 2;
        } else if arg == "-p" {
            if !args
                .get(index + 1)
                .is_some_and(|value| positive_integer(value))
            {
                return false;
            }
            index += 2;
        } else if ["-tags=", "-o=", "-buildmode="]
            .iter()
            .any(|prefix| arg.strip_prefix(prefix).is_some_and(option_value_is_safe))
        {
            index += 1;
        } else if let Some(value) = arg.strip_prefix("-p=") {
            if !positive_integer(value) {
                return false;
            }
            index += 1;
        } else if arg.starts_with('-') {
            return false;
        } else {
            index += 1;
        }
    }
    true
}

fn option_value_is_safe(value: &str) -> bool {
    !value.is_empty() && !value.starts_with('-')
}

fn positive_integer(value: &str) -> bool {
    value != "0" && !value.is_empty() && value.chars().all(|character| character.is_ascii_digit())
}

pub(super) fn golangci_lint(args: &[String]) -> bool {
    matches!(args.first().map(String::as_str), Some("run"))
}

pub(super) fn goimports(args: &[String]) -> bool {
    let mut index = 0;
    let mut has_input = false;
    while index < args.len() {
        let arg = &args[index];
        if matches!(
            arg.as_str(),
            "-d" | "-e" | "-format-only" | "-l" | "-v" | "-w"
        ) {
            index += 1;
        } else if arg == "-local" || arg == "-srcdir" {
            if args
                .get(index + 1)
                .is_none_or(|value| value.starts_with('-'))
            {
                return false;
            }
            index += 2;
        } else if arg.starts_with("-local=") || arg.starts_with("-srcdir=") {
            if arg.ends_with('=') {
                return false;
            }
            index += 1;
        } else if arg.starts_with('-') {
            return false;
        } else {
            has_input = true;
            index += 1;
        }
    }
    has_input
}
