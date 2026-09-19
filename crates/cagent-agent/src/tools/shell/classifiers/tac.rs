use super::super::*;
use super::parsing::*;

pub(super) fn parse_tac(args: &[String]) -> Option<SafeCommandParse> {
    input_parse(
        parse_options(
            args,
            &grammar("br", "s", &["--before", "--regex"], &["--separator"]),
        )?,
        SafeShellPresentation::Read,
        0,
        None,
        false,
    )
}
