use super::super::*;
use super::parsing::*;

pub(super) fn parse_base64(args: &[String]) -> Option<SafeCommandParse> {
    input_parse(
        parse_options(
            args,
            &grammar("di", "w", &["--decode", "--ignore-garbage"], &["--wrap"]),
        )?,
        SafeShellPresentation::Read,
        0,
        Some(1),
        false,
    )
}
