use super::super::*;
use super::parsing::*;

pub(super) fn parse_pwd(args: &[String]) -> Option<SafeCommandParse> {
    let parsed = parse_options(args, &grammar("LP", "", EMPTY, EMPTY))?;
    parsed
        .positionals
        .is_empty()
        .then(|| SafeCommandParse::new(Some(SafeShellPresentation::List)))
}
