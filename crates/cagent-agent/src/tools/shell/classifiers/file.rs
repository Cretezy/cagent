use super::super::*;
use super::parsing::*;

pub(super) fn parse_file(args: &[String]) -> Option<SafeCommandParse> {
    input_parse(
        parse_options(
            args,
            &grammar(
                "bikLNrsv0",
                "eF",
                &[
                    "--brief",
                    "--mime",
                    "--mime-type",
                    "--mime-encoding",
                    "--keep-going",
                    "--dereference",
                    "--no-dereference",
                    "--raw",
                    "--special-files",
                    "--print0",
                ],
                &["--exclude", "--separator"],
            ),
        )?,
        SafeShellPresentation::Read,
        1,
        None,
        false,
    )
    .map(|parsed| label_paths(parsed, "Read file metadata"))
}
