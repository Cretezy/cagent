use super::super::*;
use super::parsing::*;

pub(super) fn parse_comm(args: &[String]) -> Option<SafeCommandParse> {
    input_parse(
        parse_options(
            args,
            &grammar(
                "123z",
                "",
                &["--check-order", "--nocheck-order", "--zero-terminated"],
                &["--output-delimiter"],
            ),
        )?,
        SafeShellPresentation::Read,
        2,
        Some(2),
        false,
    )
}
