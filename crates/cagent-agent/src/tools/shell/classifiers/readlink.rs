use super::super::*;
use super::parsing::*;

pub(super) fn parse_readlink(args: &[String]) -> Option<SafeCommandParse> {
    input_parse(
        parse_options(
            args,
            &grammar(
                "efmnqsvz",
                "",
                &[
                    "--canonicalize",
                    "--canonicalize-existing",
                    "--canonicalize-missing",
                    "--no-newline",
                    "--quiet",
                    "--silent",
                    "--verbose",
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
    .map(|parsed| label_paths(parsed, "Read link"))
}
