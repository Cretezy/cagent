use super::super::*;
use super::parsing::*;

pub(super) fn parse_strings(args: &[String]) -> Option<SafeCommandParse> {
    input_parse(
        parse_options(
            args,
            &grammar(
                "afotTx",
                "en",
                &[
                    "--all",
                    "--print-file-name",
                    "--radix",
                    "--include-all-whitespace",
                ],
                &["--encoding", "--bytes"],
            ),
        )?,
        SafeShellPresentation::Read,
        0,
        None,
        false,
    )
    .map(|parsed| label_paths(parsed, "Read strings"))
}
