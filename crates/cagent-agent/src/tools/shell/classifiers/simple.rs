use super::super::*;
use super::parsing::*;

pub(super) fn parse_simple_inputs(args: &[String]) -> Option<SafeCommandParse> {
    input_parse(
        parse_options(args, &grammar("", "", EMPTY, EMPTY))?,
        SafeShellPresentation::Read,
        0,
        None,
        false,
    )
}
