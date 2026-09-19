use super::super::*;
use super::parsing::*;

pub(super) fn parse_tr(args: &[String]) -> Option<SafeCommandParse> {
    let parsed = parse_options(
        args,
        &grammar(
            "cdst",
            "",
            &[
                "--complement",
                "--delete",
                "--squeeze-repeats",
                "--truncate-set1",
            ],
            EMPTY,
        ),
    )?;
    (1..=2)
        .contains(&parsed.positionals.len())
        .then(|| SafeCommandParse::new(None))
}
