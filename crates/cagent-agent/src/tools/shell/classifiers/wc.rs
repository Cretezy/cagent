use super::super::*;
use super::parsing::*;

pub(super) fn parse_wc(args: &[String]) -> Option<SafeCommandParse> {
    input_parse(
        parse_options(
            args,
            &grammar(
                "clmwL",
                "",
                &[
                    "--bytes",
                    "--chars",
                    "--lines",
                    "--max-line-length",
                    "--words",
                ],
                EMPTY,
            ),
        )?,
        SafeShellPresentation::Read,
        0,
        None,
        false,
    )
}
