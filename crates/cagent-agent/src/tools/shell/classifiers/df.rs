use super::super::*;
use super::parsing::*;

pub(super) fn parse_df(args: &[String]) -> Option<SafeCommandParse> {
    input_parse(
        parse_options(
            args,
            &grammar(
                "ahiklmPT",
                "t x".replace(' ', "").as_str(),
                &[
                    "--all",
                    "--human-readable",
                    "--si",
                    "--inodes",
                    "--local",
                    "--portability",
                    "--print-type",
                    "--total",
                    "--no-sync",
                ],
                &["--block-size", "--type", "--exclude-type", "--output"],
            ),
        )?,
        SafeShellPresentation::Read,
        0,
        None,
        false,
    )
}
