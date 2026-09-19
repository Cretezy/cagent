use super::super::*;
use super::parsing::*;

pub(super) fn parse_tree(args: &[String]) -> Option<SafeCommandParse> {
    let parsed = parse_options(
        args,
        &grammar(
            "adfiNpqshugDF",
            "LPI",
            &[
                "--all",
                "--dirsfirst",
                "--noreport",
                "--prune",
                "--gitignore",
            ],
            &[
                "--level",
                "--pattern",
                "--ignore",
                "--filelimit",
                "--sort",
                "--charset",
            ],
        ),
    )?;
    let depth = parsed.option_value(&["-L", "--level"])?;
    let depth = depth.parse::<u8>().ok()?;
    (1..=8).contains(&depth).then_some(())?;
    input_parse(parsed, SafeShellPresentation::List, 0, Some(1), true)
}
