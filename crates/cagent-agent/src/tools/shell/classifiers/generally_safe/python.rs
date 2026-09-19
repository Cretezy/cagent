pub(super) fn python(args: &[String]) -> bool {
    if !matches!(args.first().map(String::as_str), Some("-m")) {
        return false;
    }
    match args.get(1).map(String::as_str) {
        Some("pytest" | "black") => true,
        Some("build") => python_build(&args[2..]),
        Some("ruff") => ruff(&args[2..]),
        Some("isort") => isort(&args[2..]),
        _ => false,
    }
}

fn python_build(args: &[String]) -> bool {
    let mut index = 0;
    let mut source_count = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        if matches!(
            arg,
            "--sdist" | "-s" | "--wheel" | "-w" | "--no-isolation" | "--skip-dependency-check"
        ) {
            index += 1;
        } else if matches!(arg, "--outdir" | "-o") {
            if !args.get(index + 1).is_some_and(|value| safe_value(value)) {
                return false;
            }
            index += 2;
        } else if arg == "--installer" {
            if !args
                .get(index + 1)
                .is_some_and(|value| matches!(value.as_str(), "pip" | "uv"))
            {
                return false;
            }
            index += 2;
        } else if arg.starts_with("--outdir=") {
            if arg
                .strip_prefix("--outdir=")
                .is_none_or(|value| !safe_value(value))
            {
                return false;
            }
            index += 1;
        } else if arg.starts_with("--installer=") {
            if !matches!(arg.strip_prefix("--installer="), Some("pip" | "uv")) {
                return false;
            }
            index += 1;
        } else if arg.starts_with('-') || !safe_value(arg) || source_count == 1 {
            return false;
        } else {
            source_count += 1;
            index += 1;
        }
    }
    true
}

fn safe_value(value: &str) -> bool {
    !value.is_empty() && !value.starts_with('-')
}

pub(super) fn ruff(args: &[String]) -> bool {
    matches!(args.first().map(String::as_str), Some("format" | "check"))
        && !args[1..].iter().any(|arg| {
            matches!(arg.as_str(), "--fix" | "--unsafe-fixes" | "--config")
                || arg.starts_with("--fix=")
                || arg.starts_with("--config=")
        })
}

pub(super) fn isort(args: &[String]) -> bool {
    let mut index = 0;
    let mut has_input = false;
    while index < args.len() {
        let arg = &args[index];
        if matches!(
            arg.as_str(),
            "--check" | "--check-only" | "-c" | "--diff" | "--atomic" | "--filter-files"
        ) {
            index += 1;
        } else if arg == "--profile" {
            if args
                .get(index + 1)
                .is_none_or(|value| value.starts_with('-'))
            {
                return false;
            }
            index += 2;
        } else if matches!(arg.as_str(), "--line-length" | "-l") {
            if args
                .get(index + 1)
                .is_none_or(|value| !is_positive_integer(value))
            {
                return false;
            }
            index += 2;
        } else if arg.starts_with("--profile=") {
            if arg.ends_with('=') {
                return false;
            }
            index += 1;
        } else if let Some(value) = arg.strip_prefix("--line-length=") {
            if !is_positive_integer(value) {
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

fn is_positive_integer(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|character| character.is_ascii_digit()) && value != "0"
}
