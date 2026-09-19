#![allow(clippy::redundant_closure)] // The closure keeps the candidate-to-project conversion visible in this rule table.
//! Narrow classifiers for widely used development commands outside the core ecosystems.

fn parsed_args(args: &[String], flags: &[&str], options: &[&str], requires_input: bool) -> bool {
    let mut index = 0;
    let mut has_input = false;
    let mut positional_only = false;
    while index < args.len() {
        let arg = &args[index];
        if positional_only {
            has_input = true;
            index += 1;
        } else if arg == "--" {
            positional_only = true;
            index += 1;
        } else if flags.contains(&arg.as_str()) {
            index += 1;
        } else if let Some((option, value)) = arg.split_once('=') {
            if !options.contains(&option) || value.is_empty() {
                return false;
            }
            index += 1;
        } else if options.contains(&arg.as_str()) {
            if args
                .get(index + 1)
                .is_none_or(|value| value.starts_with('-'))
            {
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
    !requires_input || has_input
}

fn positive_integer(value: &str) -> bool {
    value != "0" && !value.is_empty() && value.chars().all(|character| character.is_ascii_digit())
}

fn safe_value(value: &str) -> bool {
    !value.is_empty() && !value.starts_with('-')
}

fn project_identifier(value: &str) -> bool {
    safe_value(value)
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character))
}

pub(super) fn clang_format(args: &[String]) -> bool {
    parsed_args(
        args,
        &[
            "-i",
            "--dry-run",
            "--Werror",
            "--verbose",
            "--sort-includes",
        ],
        &["--style", "--fallback-style", "--assume-filename"],
        true,
    )
}

pub(super) fn shfmt(args: &[String]) -> bool {
    parsed_args(
        args,
        &["-w", "-d", "--diff", "-l", "-s", "-mn", "-bn", "-ci", "-sr"],
        &["-i", "--filename", "--language-dialect"],
        true,
    )
}

pub(super) fn stylua(args: &[String]) -> bool {
    parsed_args(
        args,
        &["--check", "--verify", "--search-parent-directories"],
        &["--color", "--glob", "--config-path"],
        true,
    )
}

pub(super) fn taplo(args: &[String]) -> bool {
    matches!(args.first().map(String::as_str), Some("fmt" | "format"))
        && parsed_args(
            &args[1..],
            &["--check", "--diff", "--stdin"],
            &["--option"],
            false,
        )
}

pub(super) fn terraform(args: &[String]) -> bool {
    if !matches!(args.first().map(String::as_str), Some("fmt")) {
        return false;
    }
    parsed_args(
        &args[1..],
        &[
            "-check",
            "-diff",
            "-recursive",
            "-list=true",
            "-list=false",
            "-write=true",
            "-write=false",
            "-no-color",
        ],
        &[],
        false,
    )
}

pub(super) fn dotnet(args: &[String]) -> bool {
    let Some((command, rest)) = args.split_first() else {
        return false;
    };
    match command.as_str() {
        "test" => parsed_args(
            rest,
            &[
                "--no-restore",
                "--no-build",
                "--no-logo",
                "--blame",
                "--list-tests",
                "--no-dependencies",
            ],
            &[
                "--filter",
                "--configuration",
                "--framework",
                "--logger",
                "--verbosity",
            ],
            false,
        ),
        "build" => parsed_args(
            rest,
            &[
                "--no-restore",
                "--no-incremental",
                "--no-logo",
                "--no-dependencies",
            ],
            &[
                "--configuration",
                "-c",
                "--framework",
                "-f",
                "--runtime",
                "-r",
                "--verbosity",
                "-v",
                "--output",
                "-o",
            ],
            false,
        ),
        "format" => {
            let rest = match rest.first().map(String::as_str) {
                Some("style" | "analyzers") => &rest[1..],
                _ => rest,
            };
            parsed_args(
                rest,
                &["--verify-no-changes", "--no-restore", "--include-generated"],
                &["--severity", "--verbosity", "--diagnostics"],
                false,
            )
        }
        _ => false,
    }
}

pub(super) fn gradle(args: &[String]) -> bool {
    let (tasks, options) = args.split_at(
        args.iter()
            .position(|arg| arg.starts_with('-'))
            .unwrap_or(args.len()),
    );
    !tasks.is_empty()
        && tasks.iter().all(|task| gradle_task_is_safe(task))
        && parsed_args(
            options,
            &[
                "--offline",
                "--no-daemon",
                "--stacktrace",
                "--full-stacktrace",
                "--info",
                "--warn",
                "--quiet",
                "--continue",
                "--non-interactive",
                "--parallel",
                "--no-parallel",
                "--no-build-cache",
                "--rerun-tasks",
                "--dry-run",
                "-m",
            ],
            &["--console", "--tests", "--max-workers", "--warning-mode"],
            false,
        )
}

fn gradle_task_is_safe(task: &str) -> bool {
    if matches!(task, "test" | "check" | "build") {
        return true;
    }
    let Some((modules, operation)) = task.rsplit_once(':') else {
        return false;
    };
    matches!(operation, "test" | "check" | "build")
        && modules.starts_with(':')
        && modules[1..]
            .split(':')
            .all(|module| project_identifier(module))
}

pub(super) fn maven(args: &[String]) -> bool {
    let mut index = 0;
    let mut has_goal = false;
    while index < args.len() {
        let arg = args[index].as_str();
        if matches!(
            arg,
            "-q" | "-B" | "-ntp" | "-o" | "--offline" | "-am" | "-amd"
        ) {
            index += 1;
        } else if matches!(arg, "-pl" | "--projects") {
            if !args
                .get(index + 1)
                .is_some_and(|value| maven_projects_are_safe(value))
            {
                return false;
            }
            index += 2;
        } else if arg == "-T" {
            if !args
                .get(index + 1)
                .is_some_and(|value| positive_integer(value))
            {
                return false;
            }
            index += 2;
        } else if let Some(value) = arg.strip_prefix("-T") {
            if !positive_integer(value) {
                return false;
            }
            index += 1;
        } else if matches!(arg, "-DskipTests" | "-Dmaven.test.skip=true") {
            index += 1;
        } else if matches!(arg, "test" | "compile" | "test-compile" | "package") && !has_goal {
            has_goal = true;
            index += 1;
        } else {
            return false;
        }
    }
    has_goal
}

fn maven_projects_are_safe(value: &str) -> bool {
    !value.is_empty()
        && value.split(',').all(|project| {
            let project = project.strip_prefix(['!', '+', '-']).unwrap_or(project);
            project_identifier(project)
        })
}

pub(super) fn dart(args: &[String]) -> bool {
    let Some((command, rest)) = args.split_first() else {
        return false;
    };
    match command.as_str() {
        "format" => parsed_args(rest, &["--set-exit-if-changed", "-o"], &[], true),
        "analyze" => parsed_args(rest, &[], &["--fatal-infos", "--fatal-warnings"], false),
        "test" => parsed_args(
            rest,
            &["--chain-stack-traces"],
            &["--reporter", "--name"],
            false,
        ),
        "compile" => dart_compile(rest),
        _ => false,
    }
}

fn dart_compile(args: &[String]) -> bool {
    let Some((format, rest)) = args.split_first() else {
        return false;
    };
    if !matches!(
        format.as_str(),
        "exe" | "aot-snapshot" | "jit-snapshot" | "kernel" | "js" | "wasm"
    ) {
        return false;
    }
    let mut index = 0;
    let mut has_input = false;
    while index < rest.len() {
        let arg = rest[index].as_str();
        if matches!(
            arg,
            "--enable-asserts" | "--no-sound-null-safety" | "-O0" | "-O1" | "-O2" | "-O3" | "-O4"
        ) {
            index += 1;
        } else if matches!(arg, "-o" | "--output" | "-D" | "--define") {
            if !rest.get(index + 1).is_some_and(|value| safe_value(value)) {
                return false;
            }
            index += 2;
        } else if arg.starts_with("-D") && arg.len() > 2 {
            index += 1;
        } else if arg.starts_with('-') || !safe_value(arg) || has_input {
            return false;
        } else {
            has_input = true;
            index += 1;
        }
    }
    has_input
}

pub(super) fn flutter(args: &[String]) -> bool {
    let Some((command, rest)) = args.split_first() else {
        return false;
    };
    match command.as_str() {
        "analyze" => parsed_args(
            rest,
            &["--no-fatal-infos", "--no-fatal-warnings"],
            &[],
            false,
        ),
        "test" => parsed_args(
            rest,
            &["--coverage", "--concurrency"],
            &["--name", "--plain-name", "--reporter"],
            false,
        ),
        "build" => flutter_build(rest),
        _ => false,
    }
}

fn flutter_build(args: &[String]) -> bool {
    let Some((target, rest)) = args.split_first() else {
        return false;
    };
    if !matches!(
        target.as_str(),
        "apk" | "appbundle" | "aar" | "web" | "linux" | "macos" | "windows" | "ios"
    ) {
        return false;
    }
    let mut index = 0;
    while index < rest.len() {
        let arg = rest[index].as_str();
        if matches!(
            arg,
            "--debug"
                | "--profile"
                | "--release"
                | "--no-pub"
                | "--no-tree-shake-icons"
                | "--no-codesign"
        ) {
            index += 1;
        } else if matches!(
            arg,
            "--flavor" | "--target" | "-t" | "--build-name" | "--build-number" | "--dart-define"
        ) {
            if !rest.get(index + 1).is_some_and(|value| safe_value(value)) {
                return false;
            }
            index += 2;
        } else if [
            "--flavor=",
            "--target=",
            "--build-name=",
            "--build-number=",
            "--dart-define=",
        ]
        .iter()
        .any(|prefix| arg.strip_prefix(prefix).is_some_and(safe_value))
        {
            index += 1;
        } else {
            return false;
        }
    }
    target != "ios" || rest.iter().any(|arg| arg == "--no-codesign")
}

pub(super) fn mix(args: &[String]) -> bool {
    let Some((task, rest)) = args.split_first() else {
        return false;
    };
    match task.as_str() {
        "format" => parsed_args(rest, &["--check-formatted", "--dry-run"], &[], false),
        "test" => parsed_args(
            rest,
            &["--stale", "--warnings-as-errors"],
            &["--only", "--exclude"],
            false,
        ),
        "credo" => parsed_args(rest, &["--strict"], &[], false),
        "compile" => parsed_args(
            rest,
            &[
                "--force",
                "--warnings-as-errors",
                "--verbose",
                "--all-warnings",
            ],
            &[],
            false,
        ),
        _ => false,
    }
}

pub(super) fn zig(args: &[String]) -> bool {
    match args {
        [command, rest @ ..] if command == "fmt" => {
            parsed_args(rest, &["--check", "--ast-check"], &[], true)
        }
        [command, rest @ ..] if command == "test" => parsed_args(rest, &[], &["--name"], true),
        [build, rest @ ..] if build == "build" => zig_build(rest),
        _ => false,
    }
}

fn zig_build(args: &[String]) -> bool {
    let mut index = 0;
    let mut has_step = false;
    while index < args.len() {
        let arg = args[index].as_str();
        if matches!(
            arg,
            "--release" | "--verbose" | "--prominent-compile-errors"
        ) {
            index += 1;
        } else if matches!(arg, "-j" | "--jobs") {
            if !args
                .get(index + 1)
                .is_some_and(|value| positive_integer(value))
            {
                return false;
            }
            index += 2;
        } else if matches!(arg, "--summary" | "--color") {
            if !args.get(index + 1).is_some_and(|value| safe_value(value)) {
                return false;
            }
            index += 2;
        } else if matches!(arg, "test") && !has_step {
            has_step = true;
            index += 1;
        } else if matches!(
            arg,
            "-Doptimize=Debug"
                | "-Doptimize=ReleaseSafe"
                | "-Doptimize=ReleaseFast"
                | "-Doptimize=ReleaseSmall"
        ) || arg.strip_prefix("-Dtarget=").is_some_and(safe_value)
        {
            index += 1;
        } else {
            return false;
        }
    }
    true
}

pub(super) fn rspec(args: &[String]) -> bool {
    parsed_args(
        args,
        &["--format", "--fail-fast"],
        &["--tag", "--example"],
        false,
    )
}

pub(super) fn rubocop(args: &[String]) -> bool {
    parsed_args(
        args,
        &[
            "--autocorrect",
            "--autocorrect-all",
            "--parallel",
            "--fail-level",
        ],
        &["--only", "--except", "--format"],
        false,
    )
}

pub(super) fn standardrb(args: &[String]) -> bool {
    parsed_args(args, &["--fix", "--no-fix"], &["--format"], false)
}

pub(super) fn phpunit(args: &[String]) -> bool {
    parsed_args(
        args,
        &["--testdox", "--fail-on-warning", "--fail-on-risky"],
        &["--filter", "--testsuite"],
        false,
    )
}

pub(super) fn php_cs_fixer(args: &[String]) -> bool {
    matches!(args.first().map(String::as_str), Some("fix"))
        && parsed_args(&args[1..], &["--dry-run", "--diff"], &[], false)
}

pub(super) fn pint(args: &[String]) -> bool {
    parsed_args(args, &["--test", "--dirty", "--repair"], &[], false)
}

pub(super) fn swift(args: &[String]) -> bool {
    match args {
        [command, rest @ ..] if command == "test" => {
            parsed_args(rest, &["--skip-build", "--parallel"], &["--filter"], false)
        }
        [command, rest @ ..] if command == "format" => {
            parsed_args(rest, &["--in-place", "--lint"], &[], false)
        }
        [command, rest @ ..] if command == "build" => swift_build(rest),
        _ => false,
    }
}

fn swift_build(args: &[String]) -> bool {
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        if matches!(
            arg,
            "--verbose" | "-v" | "--skip-update" | "--disable-sandbox"
        ) {
            index += 1;
        } else if matches!(arg, "--configuration" | "-c") {
            if !args
                .get(index + 1)
                .is_some_and(|value| matches!(value.as_str(), "debug" | "release"))
            {
                return false;
            }
            index += 2;
        } else if matches!(arg, "--product" | "--target") {
            if !args
                .get(index + 1)
                .is_some_and(|value| project_identifier(value))
            {
                return false;
            }
            index += 2;
        } else if matches!(arg, "--jobs" | "-j") {
            if !args
                .get(index + 1)
                .is_some_and(|value| positive_integer(value))
            {
                return false;
            }
            index += 2;
        } else {
            return false;
        }
    }
    true
}
