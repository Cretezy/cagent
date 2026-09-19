use super::super::*;
use super::parsing::*;

pub(super) fn parse_nl(args: &[String]) -> Option<SafeCommandParse> {
    input_parse(
        parse_options(
            args,
            &grammar(
                "p",
                "bdfhinlpsvw",
                &["--no-renumber"],
                &[
                    "--body-numbering",
                    "--section-delimiter",
                    "--footer-numbering",
                    "--header-numbering",
                    "--join-blank-lines",
                    "--number-format",
                    "--number-separator",
                    "--starting-line-number",
                    "--line-increment",
                    "--width",
                ],
            ),
        )?,
        SafeShellPresentation::Read,
        0,
        None,
        false,
    )
}
