use super::super::*;
use super::parsing::*;

pub(super) fn parse_uname(args: &[String]) -> Option<SafeCommandParse> {
    let parsed = parse_options(
        args,
        &grammar(
            "asnrvmpio",
            "",
            &[
                "--all",
                "--kernel-name",
                "--nodename",
                "--kernel-release",
                "--kernel-version",
                "--machine",
                "--processor",
                "--hardware-platform",
                "--operating-system",
            ],
            EMPTY,
        ),
    )?;
    parsed.positionals.is_empty().then(|| {
        let mut result = SafeCommandParse::new(Some(SafeShellPresentation::Read));
        result.detail = Some(SafePresentationDetail::new("Read system"));
        result
    })
}
