use super::super::*;
use super::parsing::*;

pub(super) fn parse_paste(args: &[String]) -> Option<SafeCommandParse> {
    input_parse(
        parse_options(
            args,
            &grammar(
                "sz",
                "d",
                &["--serial", "--zero-terminated"],
                &["--delimiters"],
            ),
        )?,
        SafeShellPresentation::Read,
        0,
        None,
        false,
    )
}
