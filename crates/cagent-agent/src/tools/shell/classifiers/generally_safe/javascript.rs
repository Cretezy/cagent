pub(super) fn package_manager(command: &str, args: &[String]) -> bool {
    match command {
        "npm" => npm_script(args),
        "pnpm" => pnpm_script(args),
        "yarn" => yarn_script(args),
        "bun" if args.first().is_some_and(|arg| arg == "run") => {
            package_script_tail(&args[1..], ScriptManager::Bun)
        }
        "bun" => bun_build(args),
        _ => false,
    }
}

#[derive(Clone, Copy)]
enum ScriptManager {
    Npm,
    Pnpm,
    Yarn,
    Bun,
}

fn npm_script(args: &[String]) -> bool {
    args.first().is_some_and(|arg| arg == "run")
        && package_script_tail(&args[1..], ScriptManager::Npm)
}

fn pnpm_script(args: &[String]) -> bool {
    let Some(index) = consume_package_selectors(args, ScriptManager::Pnpm) else {
        return false;
    };
    let args = if args.get(index).is_some_and(|arg| arg == "run") {
        &args[index + 1..]
    } else {
        &args[index..]
    };
    package_script_tail(args, ScriptManager::Pnpm)
}

fn yarn_script(args: &[String]) -> bool {
    let args = if args.first().is_some_and(|arg| arg == "workspace") {
        let [_, name, rest @ ..] = args else {
            return false;
        };
        if !identifier_is_safe(name) {
            return false;
        }
        rest
    } else {
        args
    };
    let args = if args.first().is_some_and(|arg| arg == "run") {
        &args[1..]
    } else {
        args
    };
    package_script_tail(args, ScriptManager::Yarn)
}

fn package_script_tail(args: &[String], manager: ScriptManager) -> bool {
    let Some(script) = args.first().map(String::as_str) else {
        return false;
    };
    if !matches!(
        script,
        "build" | "format" | "format:check" | "test" | "test:check" | "lint" | "lint:check"
    ) {
        return false;
    }
    let mut index = 1;
    let mut forwarded = false;
    while index < args.len() {
        let arg = args[index].as_str();
        if forwarded {
            index += 1;
        } else if arg == "--" {
            forwarded = true;
            index += 1;
        } else if matches!(manager, ScriptManager::Npm)
            && matches!(arg, "--silent" | "--ignore-scripts" | "--if-present")
        {
            index += 1;
        } else if matches!(manager, ScriptManager::Npm | ScriptManager::Bun)
            && matches!(arg, "--workspace" | "-w")
        {
            if !args
                .get(index + 1)
                .is_some_and(|value| identifier_is_safe(value))
            {
                return false;
            }
            index += 2;
        } else if matches!(manager, ScriptManager::Pnpm | ScriptManager::Bun)
            && matches!(arg, "--filter" | "-F")
        {
            if !args
                .get(index + 1)
                .is_some_and(|value| selector_is_safe(value))
            {
                return false;
            }
            index += 2;
        } else if (matches!(manager, ScriptManager::Pnpm | ScriptManager::Bun)
            && (arg.starts_with("--filter=") || arg.starts_with("-F=")))
            || (matches!(manager, ScriptManager::Npm) && arg.starts_with("--workspace="))
        {
            if arg
                .split_once('=')
                .is_none_or(|(_, value)| !selector_is_safe(value))
            {
                return false;
            }
            index += 1;
        } else {
            return false;
        }
    }
    true
}

fn consume_package_selectors(args: &[String], manager: ScriptManager) -> Option<usize> {
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        if matches!(manager, ScriptManager::Pnpm) && matches!(arg, "-r" | "--recursive") {
            index += 1;
        } else if matches!(arg, "--filter" | "-F") {
            if !args
                .get(index + 1)
                .is_some_and(|value| selector_is_safe(value))
            {
                return None;
            }
            index += 2;
        } else if arg.starts_with("--filter=") || arg.starts_with("-F=") {
            if arg
                .split_once('=')
                .is_none_or(|(_, value)| !selector_is_safe(value))
            {
                return None;
            }
            index += 1;
        } else {
            break;
        }
    }
    Some(index)
}

fn identifier_is_safe(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('-')
        && !["..", "\\", "$", "`"]
            .iter()
            .any(|pattern| value.contains(*pattern))
        && value
            .strip_prefix('@')
            .map_or(!value.contains('/'), |scoped| {
                let mut parts = scoped.split('/');
                parts.next().is_some_and(|part| !part.is_empty())
                    && parts.next().is_some_and(|part| !part.is_empty())
                    && parts.next().is_none()
            })
}

fn selector_is_safe(value: &str) -> bool {
    !value.is_empty()
        && !["..", "$", "`", "\n", "\r"]
            .iter()
            .any(|pattern| value.contains(*pattern))
        && !value.starts_with('/')
        && !value.starts_with('~')
}

fn bun_build(args: &[String]) -> bool {
    if !args.first().is_some_and(|arg| arg == "build") {
        return false;
    }
    let mut index = 1;
    let mut has_entrypoint = false;
    while index < args.len() {
        let arg = args[index].as_str();
        if matches!(arg, "--minify" | "--splitting" | "--no-bundle") {
            index += 1;
        } else if matches!(
            arg,
            "--outdir"
                | "--outfile"
                | "--target"
                | "--format"
                | "--sourcemap"
                | "--packages"
                | "--external"
                | "--entry-naming"
                | "--chunk-naming"
                | "--asset-naming"
        ) {
            if !args
                .get(index + 1)
                .is_some_and(|value| build_value_is_safe(value))
            {
                return false;
            }
            index += 2;
        } else if [
            "--outdir=",
            "--outfile=",
            "--target=",
            "--format=",
            "--sourcemap=",
            "--packages=",
            "--external=",
            "--entry-naming=",
            "--chunk-naming=",
            "--asset-naming=",
        ]
        .iter()
        .any(|prefix| arg.strip_prefix(prefix).is_some_and(build_value_is_safe))
        {
            index += 1;
        } else if arg.starts_with('-') || !build_value_is_safe(arg) {
            return false;
        } else {
            has_entrypoint = true;
            index += 1;
        }
    }
    has_entrypoint
}

fn build_value_is_safe(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('-')
        && !["$", "`", "\n", "\r"]
            .iter()
            .any(|pattern| value.contains(*pattern))
        && !value.contains("://")
}

pub(super) fn prettier(args: &[String]) -> bool {
    let mut has_input = false;
    let mut positional_only = false;
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if positional_only {
            has_input = true;
            index += 1;
        } else if arg == "--" {
            positional_only = true;
            index += 1;
        } else if matches!(
            arg.as_str(),
            "--write"
                | "-w"
                | "--check"
                | "-c"
                | "--list-different"
                | "-l"
                | "--debug-check"
                | "--ignore-unknown"
                | "--with-node-modules"
                | "--no-config"
                | "--no-editorconfig"
                | "--no-error-on-unmatched-pattern"
                | "--single-quote"
                | "--jsx-single-quote"
                | "--bracket-same-line"
                | "--bracket-spacing"
                | "--semi"
                | "--use-tabs"
                | "--require-pragma"
                | "--insert-pragma"
        ) {
            index += 1;
        } else if matches!(
            arg.as_str(),
            "--parser"
                | "--print-width"
                | "--tab-width"
                | "--range-start"
                | "--range-end"
                | "--trailing-comma"
                | "--prose-wrap"
                | "--end-of-line"
        ) {
            let Some(value) = args.get(index + 1) else {
                return false;
            };
            if value.starts_with('-') {
                return false;
            }
            index += 2;
        } else if arg.contains('=')
            && matches!(
                arg.split_once('=').map(|(name, _)| name),
                Some(
                    "--parser"
                        | "--print-width"
                        | "--tab-width"
                        | "--range-start"
                        | "--range-end"
                        | "--trailing-comma"
                        | "--prose-wrap"
                        | "--end-of-line"
                )
            )
        {
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

pub(super) fn biome(args: &[String]) -> bool {
    matches!(args.first().map(String::as_str), Some("format" | "check"))
        && formatter_args(
            &args[1..],
            &[
                "--write",
                "--unsafe",
                "--changed",
                "--no-errors-on-unmatched",
            ],
        )
}

pub(super) fn eslint(args: &[String]) -> bool {
    let mut index = 0;
    let mut has_input = false;
    while index < args.len() {
        let arg = args[index].as_str();
        if matches!(
            arg,
            "--fix"
                | "--quiet"
                | "--no-warn-ignored"
                | "--no-ignore"
                | "--no-inline-config"
                | "--report-unused-disable-directives"
        ) {
            index += 1;
        } else if arg == "--max-warnings" {
            if !args
                .get(index + 1)
                .is_some_and(|value| non_negative_integer(value))
            {
                return false;
            }
            index += 2;
        } else if let Some(value) = arg.strip_prefix("--max-warnings=") {
            if !non_negative_integer(value) {
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

fn non_negative_integer(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|character| character.is_ascii_digit())
}

pub(super) fn tsc(args: &[String]) -> bool {
    if args
        .first()
        .is_some_and(|arg| matches!(arg.as_str(), "--build" | "-b"))
    {
        return tsc_build(&args[1..]);
    }
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if matches!(
            arg.as_str(),
            "--noEmit"
                | "--pretty"
                | "--incremental"
                | "--watch=false"
                | "--diagnostics"
                | "--extendedDiagnostics"
                | "--listFiles"
                | "--listEmittedFiles"
                | "--traceResolution"
                | "--showConfig"
                | "--noEmitOnError"
        ) || arg.starts_with("--pretty=")
        {
            index += 1;
        } else if matches!(arg.as_str(), "--project" | "-p") {
            if args
                .get(index + 1)
                .is_none_or(|value| value.starts_with('-'))
            {
                return false;
            }
            index += 2;
        } else if arg.starts_with("--project=") {
            if arg.ends_with('=') {
                return false;
            }
            index += 1;
        } else {
            return false;
        }
    }
    true
}

fn tsc_build(args: &[String]) -> bool {
    args.iter().all(|arg| {
        matches!(
            arg.as_str(),
            "--force" | "--verbose" | "--dry" | "--stopBuildOnErrors" | "--pretty"
        ) || arg.starts_with("--pretty=")
            || (!arg.starts_with('-') && build_value_is_safe(arg))
    })
}

pub(super) fn test_runner(args: &[String]) -> bool {
    !args.iter().any(|arg| {
        matches!(arg.as_str(), "--watch" | "--watchAll" | "--runInBand=false")
            || arg.starts_with("--config=")
            || arg == "--config"
    })
}

pub(super) fn deno(args: &[String]) -> bool {
    let Some((command, rest)) = args.split_first() else {
        return false;
    };
    match command.as_str() {
        "fmt" => formatter_args(
            rest,
            &["--check", "--use-tabs", "--single-quote", "--quote-props"],
        ),
        "check" => formatter_args(rest, &[]),
        "test" => !rest.iter().any(|arg| {
            arg.starts_with("--allow-")
                || matches!(
                    arg.as_str(),
                    "--watch" | "--parallel" | "--no-run" | "--inspect" | "--inspect-brk"
                )
        }),
        "compile" => deno_compile(rest),
        _ => false,
    }
}

fn deno_compile(args: &[String]) -> bool {
    let mut index = 0;
    let mut has_entrypoint = false;
    let mut forwarded = false;
    while index < args.len() {
        let arg = args[index].as_str();
        if forwarded {
            index += 1;
        } else if arg == "--" && has_entrypoint {
            forwarded = true;
            index += 1;
        } else if matches!(arg, "--no-check" | "--reload" | "--quiet") {
            if has_entrypoint {
                return false;
            }
            index += 1;
        } else if matches!(arg, "--output" | "-o" | "--target" | "--include") {
            if has_entrypoint
                || !args
                    .get(index + 1)
                    .is_some_and(|value| build_value_is_safe(value))
            {
                return false;
            }
            index += 2;
        } else if ["--output=", "--target=", "--include="]
            .iter()
            .any(|prefix| arg.strip_prefix(prefix).is_some_and(build_value_is_safe))
        {
            if has_entrypoint {
                return false;
            }
            index += 1;
        } else if arg.starts_with('-') || !build_value_is_safe(arg) || has_entrypoint {
            return false;
        } else {
            has_entrypoint = true;
            index += 1;
        }
    }
    has_entrypoint
}

fn formatter_args(args: &[String], flags: &[&str]) -> bool {
    let mut has_input = false;
    for arg in args {
        if flags.contains(&arg.as_str()) {
            continue;
        }
        if arg.starts_with('-') {
            return false;
        }
        has_input = true;
    }
    has_input
}
