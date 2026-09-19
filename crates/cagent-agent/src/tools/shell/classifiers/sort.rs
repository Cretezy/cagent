use super::super::*;
use super::parsing::*;

pub(super) fn parse_sort(args: &[String]) -> Option<SafeCommandParse> {
    input_parse(
        parse_options(
            args,
            &grammar(
                "bcdfghimnRrsuVz",
                "kSt",
                &[
                    "--check",
                    "--merge",
                    "--stable",
                    "--unique",
                    "--version-sort",
                    "--zero-terminated",
                    "--dictionary-order",
                    "--ignore-case",
                    "--ignore-nonprinting",
                    "--general-numeric-sort",
                    "--human-numeric-sort",
                    "--month-sort",
                    "--numeric-sort",
                    "--random-sort",
                    "--reverse",
                ],
                &[
                    "--key",
                    "--field-separator",
                    "--buffer-size",
                    "--batch-size",
                ],
            ),
        )?,
        SafeShellPresentation::Read,
        0,
        None,
        false,
    )
}
