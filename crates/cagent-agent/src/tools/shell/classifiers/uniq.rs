use super::super::*;
use super::parsing::*;

pub(super) fn parse_uniq(args: &[String]) -> Option<SafeCommandParse> {
    input_parse(
        parse_options(
            args,
            &grammar(
                "cdiuz",
                "fw",
                &[
                    "--count",
                    "--repeated",
                    "--all-repeated",
                    "--ignore-case",
                    "--unique",
                    "--zero-terminated",
                ],
                &["--skip-fields", "--skip-chars", "--check-chars", "--group"],
            ),
        )?,
        SafeShellPresentation::Read,
        0,
        Some(1),
        false,
    )
}
