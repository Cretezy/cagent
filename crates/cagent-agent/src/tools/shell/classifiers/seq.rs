use super::super::*;
use super::parsing::*;

pub(super) fn parse_seq(args: &[String]) -> Option<SafeCommandParse> {
    let parsed = parse_options(
        args,
        &grammar(
            "w",
            "f s".replace(' ', "").as_str(),
            &["--equal-width"],
            &["--format", "--separator"],
        ),
    )?;
    (1..=3)
        .contains(&parsed.positionals.len())
        .then(|| SafeCommandParse::new(None))
}
