use super::super::*;
use super::parsing::*;

pub(super) fn parse_which(args: &[String]) -> Option<SafeCommandParse> {
    let parsed = parse_options(
        args,
        &grammar(
            "as",
            "",
            &[
                "--all",
                "--read-alias",
                "--skip-alias",
                "--read-functions",
                "--skip-functions",
                "--show-dot",
                "--skip-dot",
                "--show-tilde",
                "--skip-tilde",
                "--tty-only",
            ],
            EMPTY,
        ),
    )?;
    (!parsed.positionals.is_empty() && parsed.positionals.iter().all(|name| !name.contains('/')))
        .then(|| {
            let mut result = SafeCommandParse::new(Some(SafeShellPresentation::Read));
            result.detail = Some(
                SafePresentationDetail::new("Locate command").arguments(parsed.positionals, false),
            );
            result
        })
}
