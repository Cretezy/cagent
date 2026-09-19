use super::super::*;

pub(super) fn parse_version(args: &[String]) -> Option<SafeCommandParse> {
    matches!(args, [flag] if flag == "--version" || flag == "-V")
        .then(|| SafeCommandParse::new(None))
}
