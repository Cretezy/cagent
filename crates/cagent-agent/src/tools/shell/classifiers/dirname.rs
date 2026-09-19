use super::super::*;
use super::parsing::*;

pub(super) fn parse_dirname(args: &[String]) -> Option<SafeCommandParse> {
    let parsed = parse_options(args, &grammar("z", "", &["--zero"], EMPTY))?;
    (!parsed.positionals.is_empty()).then(|| SafeCommandParse::new(None))
}
