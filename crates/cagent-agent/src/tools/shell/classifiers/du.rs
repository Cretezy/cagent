use super::super::*;
use super::parsing::*;

pub(super) fn parse_du(args: &[String]) -> Option<SafeCommandParse> {
    input_parse(
        parse_options(
            args,
            &grammar(
                "achkPSsx",
                "d",
                &[
                    "--all",
                    "--apparent-size",
                    "--count-links",
                    "--dereference-args",
                    "--human-readable",
                    "--si",
                    "--summarize",
                    "--one-file-system",
                    "--separate-dirs",
                    "--time",
                ],
                &[
                    "--block-size",
                    "--max-depth",
                    "--threshold",
                    "--time-style",
                    "--exclude",
                ],
            ),
        )?,
        SafeShellPresentation::List,
        0,
        None,
        true,
    )
}
