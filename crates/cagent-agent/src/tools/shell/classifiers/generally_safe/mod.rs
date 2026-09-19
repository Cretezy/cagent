//! Static development-command classifiers.

use super::super::*;

fn unavailable_parser(_: &[String]) -> Option<SafeCommandParse> {
    None
}

macro_rules! generally_safe_spec {
    ($name:literal) => {
        SafeCommandSpec {
            canonical: $name,
            aliases: &[],
            platform: SafeCommandPlatform::All,
            builtin: false,
            parser: unavailable_parser,
            examples: &[],
            restrictions: "strict generally-safe development grammar",
            category: SafeCommandCategory::Generic,
            recommends_over: None,
            hardening: SafeCommandHardening::None,
            tier: ShellSafetyTier::Level2,
        }
    };
}

pub static GENERALLY_SAFE_COMMAND_REGISTRY: &[SafeCommandSpec] = &[
    generally_safe_spec!("cargo"),
    generally_safe_spec!("cargo-nextest"),
    generally_safe_spec!("rustfmt"),
    generally_safe_spec!("npm"),
    generally_safe_spec!("pnpm"),
    generally_safe_spec!("yarn"),
    generally_safe_spec!("bun"),
    generally_safe_spec!("prettier"),
    generally_safe_spec!("biome"),
    generally_safe_spec!("eslint"),
    generally_safe_spec!("tsc"),
    generally_safe_spec!("vitest"),
    generally_safe_spec!("jest"),
    generally_safe_spec!("deno"),
    generally_safe_spec!("go"),
    generally_safe_spec!("gofmt"),
    generally_safe_spec!("goimports"),
    generally_safe_spec!("staticcheck"),
    generally_safe_spec!("golangci-lint"),
    generally_safe_spec!("pytest"),
    generally_safe_spec!("python"),
    generally_safe_spec!("python3"),
    generally_safe_spec!("ruff"),
    generally_safe_spec!("black"),
    generally_safe_spec!("isort"),
    generally_safe_spec!("flake8"),
    generally_safe_spec!("pylint"),
    generally_safe_spec!("clang-format"),
    generally_safe_spec!("shfmt"),
    generally_safe_spec!("stylua"),
    generally_safe_spec!("taplo"),
    generally_safe_spec!("terraform"),
    generally_safe_spec!("tofu"),
    generally_safe_spec!("dotnet"),
    generally_safe_spec!("gradle"),
    generally_safe_spec!("gradlew"),
    generally_safe_spec!("mvn"),
    generally_safe_spec!("dart"),
    generally_safe_spec!("flutter"),
    generally_safe_spec!("mix"),
    generally_safe_spec!("zig"),
    generally_safe_spec!("rspec"),
    generally_safe_spec!("rubocop"),
    generally_safe_spec!("standardrb"),
    generally_safe_spec!("phpunit"),
    generally_safe_spec!("php-cs-fixer"),
    generally_safe_spec!("pint"),
    generally_safe_spec!("swift"),
    generally_safe_spec!("jj"),
];

mod additional;
mod go;
mod javascript;
mod python;
mod rust;

/// Classifies a literal development command independently from read-safe
/// inspection commands. These commands may execute project code or modify
/// workspace files, so they never receive read/list/search presentation.
pub(super) fn classify_generally_safe_segment(
    segment: &super::super::super::ShellSegment,
) -> Option<ShellSafetyTier> {
    if segment.opaque
        || segment.words.is_empty()
        || (segment.words[0].contains(['/', '=']) && segment.words[0] != "./gradlew")
    {
        return None;
    }
    let Ok(parsed) = analyze_shell(&segment.raw) else {
        return None;
    };
    if parsed.has_assignment
        || (parsed.has_redirection && !parsed.only_read_safe_redirections)
        || parsed.opaque
        || parsed.segments.len() != 1
    {
        return None;
    }
    let normalized_words;
    let words = if segment.words.last().is_some_and(|arg| arg == "--help") {
        normalized_words = segment.words[..segment.words.len() - 1].to_vec();
        &normalized_words
    } else {
        &segment.words
    };
    if words.is_empty() {
        return None;
    }
    if words[1..]
        .iter()
        .any(|arg| arg.contains(['$', '`', '\n', '\r']) || has_outside_path_selector(arg))
    {
        return None;
    }
    let accepted = match words[0].as_str() {
        "jj" => super::jj::parse_jj_level1(&words[1..]).is_some(),
        "cargo" => rust::cargo(&words[1..]),
        "cargo-nextest" => rust::nextest(&words[1..]),
        "rustfmt" => rust::rustfmt(&words[1..]),
        "npm" | "pnpm" | "yarn" | "bun" => {
            javascript::package_manager(words[0].as_str(), &words[1..])
        }
        "prettier" => javascript::prettier(&words[1..]),
        "biome" => javascript::biome(&words[1..]),
        "eslint" => javascript::eslint(&words[1..]),
        "tsc" => javascript::tsc(&words[1..]),
        "vitest" | "jest" => javascript::test_runner(&words[1..]),
        "deno" => javascript::deno(&words[1..]),
        "go" => go::go(&words[1..]),
        "gofmt" | "staticcheck" => true,
        "goimports" => go::goimports(&words[1..]),
        "golangci-lint" => go::golangci_lint(&words[1..]),
        "pytest" => true,
        "python" | "python3" => python::python(&words[1..]),
        "ruff" => python::ruff(&words[1..]),
        "isort" => python::isort(&words[1..]),
        "black" | "flake8" | "pylint" => true,
        "clang-format" => additional::clang_format(&words[1..]),
        "shfmt" => additional::shfmt(&words[1..]),
        "stylua" => additional::stylua(&words[1..]),
        "taplo" => additional::taplo(&words[1..]),
        "terraform" | "tofu" => additional::terraform(&words[1..]),
        "dotnet" => additional::dotnet(&words[1..]),
        "gradle" | "gradlew" | "./gradlew" => additional::gradle(&words[1..]),
        "mvn" => additional::maven(&words[1..]),
        "dart" => additional::dart(&words[1..]),
        "flutter" => additional::flutter(&words[1..]),
        "mix" => additional::mix(&words[1..]),
        "zig" => additional::zig(&words[1..]),
        "rspec" => additional::rspec(&words[1..]),
        "rubocop" => additional::rubocop(&words[1..]),
        "standardrb" => additional::standardrb(&words[1..]),
        "phpunit" => additional::phpunit(&words[1..]),
        "php-cs-fixer" => additional::php_cs_fixer(&words[1..]),
        "pint" => additional::pint(&words[1..]),
        "swift" => additional::swift(&words[1..]),
        _ => false,
    };
    if !accepted {
        return None;
    }
    Some(effective_tier(words))
}

fn effective_tier(words: &[String]) -> ShellSafetyTier {
    let command = words[0].as_str();
    let args = &words[1..];
    if command == "jj" {
        return ShellSafetyTier::Level1;
    }
    let check_flag = args
        .iter()
        .any(|a| matches!(a.as_str(), "--check" | "--check-only" | "-c" | "--diff"));
    let write_flag = args.iter().any(|a| {
        matches!(
            a.as_str(),
            "--write" | "-w" | "--fix" | "--apply" | "format" | "fmt" | "fix"
        )
    });
    if matches!(
        command,
        "rustfmt"
            | "gofmt"
            | "goimports"
            | "prettier"
            | "black"
            | "isort"
            | "clang-format"
            | "shfmt"
            | "stylua"
            | "php-cs-fixer"
            | "pint"
    ) {
        return if check_flag {
            ShellSafetyTier::Level1
        } else {
            ShellSafetyTier::Level3
        };
    }
    if command == "cargo" && args.first().is_some_and(|a| a == "fmt") {
        return if check_flag {
            ShellSafetyTier::Level1
        } else {
            ShellSafetyTier::Level3
        };
    }
    let formatter_operation = match command {
        "biome" | "ruff" => write_flag || args.first().is_some_and(|arg| arg == "format"),
        "deno" => args.first().is_some_and(|arg| arg == "fmt"),
        "taplo" => args
            .first()
            .is_some_and(|arg| matches!(arg.as_str(), "fmt" | "format")),
        _ => false,
    };
    if formatter_operation {
        return if check_flag {
            ShellSafetyTier::Level1
        } else {
            ShellSafetyTier::Level3
        };
    }
    ShellSafetyTier::Level2
}

fn has_outside_path_selector(argument: &str) -> bool {
    let value = argument
        .rsplit_once('=')
        .map_or(argument, |(_, value)| value);
    let has_parent_component = value.split(['/', '\\']).any(|component| component == "..");
    let windows_absolute = value.as_bytes().get(1) == Some(&b':')
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphabetic)
        && value
            .as_bytes()
            .get(2)
            .is_some_and(|separator| matches!(separator, b'/' | b'\\'));
    value.starts_with('/')
        || value.starts_with("~/")
        || value.starts_with("~\\")
        || has_parent_component
        || windows_absolute
}
