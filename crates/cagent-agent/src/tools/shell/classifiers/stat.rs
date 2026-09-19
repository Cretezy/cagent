use super::super::*;
use super::parsing::*;

pub(super) fn parse_stat(args: &[String]) -> Option<SafeCommandParse> {
    input_parse(
        parse_options(
            args,
            &grammar(
                "Lftc",
                "",
                &["--dereference", "--file-system", "--terse"],
                &["--format", "--printf"],
            ),
        )?,
        SafeShellPresentation::Read,
        1,
        None,
        false,
    )
    .map(|parsed| label_paths(parsed, "Read file status"))
}
