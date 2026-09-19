use super::super::*;
use super::parsing::*;

pub(super) fn parse_cat(args: &[String]) -> Option<SafeCommandParse> {
    input_parse(
        parse_options(
            args,
            &grammar(
                "AbeEnstTuv",
                "",
                &[
                    "--show-all",
                    "--number-nonblank",
                    "--show-ends",
                    "--number",
                    "--squeeze-blank",
                    "--show-tabs",
                    "--show-nonprinting",
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
