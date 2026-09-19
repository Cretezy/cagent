use super::super::*;
use super::parsing::*;

pub(super) fn parse_realpath(args: &[String]) -> Option<SafeCommandParse> {
    input_parse(
        parse_options(
            args,
            &grammar(
                "eLmsz",
                "",
                &[
                    "--canonicalize-existing",
                    "--logical",
                    "--canonicalize-missing",
                    "--strip",
                    "--zero",
                ],
                EMPTY,
            ),
        )?,
        SafeShellPresentation::Read,
        1,
        None,
        false,
    )
    .map(|parsed| label_paths(parsed, "Read path"))
}
