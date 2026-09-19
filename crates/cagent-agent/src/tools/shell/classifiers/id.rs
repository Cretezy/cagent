use super::super::*;
use super::parsing::*;

pub(super) fn parse_id(args: &[String]) -> Option<SafeCommandParse> {
    let parsed = parse_options(
        args,
        &grammar(
            "ugGnrzZ",
            "",
            &[
                "--user", "--group", "--groups", "--name", "--real", "--zero",
            ],
            EMPTY,
        ),
    )?;
    (parsed.positionals.len() <= 1).then(|| {
        let mut result = SafeCommandParse::new(Some(SafeShellPresentation::Read));
        result.detail =
            Some(SafePresentationDetail::new("Read identity").arguments(parsed.positionals, false));
        result
    })
}
