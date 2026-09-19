use super::super::*;
use super::parsing::*;

pub(super) fn parse_numfmt(args: &[String]) -> Option<SafeCommandParse> {
    let _parsed = parse_options(
        args,
        &grammar(
            "z",
            "d",
            &[
                "--debug",
                "--header",
                "--invalid",
                "--padding",
                "--round",
                "--suffix",
                "--grouping",
                "--zero-terminated",
            ],
            &[
                "--delimiter",
                "--field",
                "--format",
                "--from",
                "--from-unit",
                "--to",
                "--to-unit",
            ],
        ),
    )?;
    Some(SafeCommandParse::new(None))
}
