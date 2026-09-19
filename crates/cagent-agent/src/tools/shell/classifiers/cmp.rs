use super::super::*;
use super::parsing::*;

pub(super) fn parse_cmp(args: &[String]) -> Option<SafeCommandParse> {
    input_parse(
        parse_options(
            args,
            &grammar(
                "blsn",
                "i",
                &["--print-bytes", "--verbose", "--silent", "--quiet"],
                &["--ignore-initial", "--bytes"],
            ),
        )?,
        SafeShellPresentation::Read,
        2,
        Some(2),
        false,
    )
}
