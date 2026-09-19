use super::super::*;
use super::parsing::*;

pub(super) fn parse_diff(args: &[String]) -> Option<SafeCommandParse> {
    input_parse(
        parse_options(
            args,
            &grammar(
                "abBcCdefhiIlnNpqsTtUuwWxyz",
                "CIFLUW",
                &[
                    "--text",
                    "--ignore-space-change",
                    "--ignore-all-space",
                    "--ignore-blank-lines",
                    "--brief",
                    "--report-identical-files",
                    "--recursive",
                    "--new-file",
                    "--unidirectional-new-file",
                    "--minimal",
                    "--speed-large-files",
                    "--strip-trailing-cr",
                ],
                &[
                    "--context",
                    "--ignore-matching-lines",
                    "--label",
                    "--unified",
                    "--width",
                    "--tabsize",
                    "--horizon-lines",
                ],
            ),
        )?,
        SafeShellPresentation::Read,
        2,
        Some(2),
        false,
    )
}
