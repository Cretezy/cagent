use super::super::*;
use super::parsing::*;

pub(super) fn parse_cut(args: &[String]) -> Option<SafeCommandParse> {
    input_parse(
        parse_options(
            args,
            &grammar(
                "nsz",
                "bcdf",
                &["--complement", "--only-delimited", "--zero-terminated"],
                &[
                    "--bytes",
                    "--characters",
                    "--delimiter",
                    "--fields",
                    "--output-delimiter",
                ],
            ),
        )?,
        SafeShellPresentation::Read,
        0,
        None,
        false,
    )
}
