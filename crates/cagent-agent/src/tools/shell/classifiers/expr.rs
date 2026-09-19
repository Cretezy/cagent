use super::super::*;

pub(super) fn parse_expr(args: &[String]) -> Option<SafeCommandParse> {
    (!args.is_empty() && args.iter().all(|arg| !arg.starts_with('-')))
        .then(|| SafeCommandParse::new(None))
}
