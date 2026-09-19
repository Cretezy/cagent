use super::super::*;
use super::parsing::*;
use super::tail::normalize_line_count_args;

pub(super) fn parse_head(args: &[String]) -> Option<SafeCommandParse> {
    let normalized = normalize_line_count_args(args);
    input_parse(
        parse_options(
            &normalized,
            &grammar(
                "qvzc",
                "nc",
                &["--quiet", "--silent", "--verbose", "--zero-terminated"],
                &["--lines", "--bytes"],
            ),
        )?,
        SafeShellPresentation::Read,
        0,
        None,
        false,
    )
}
