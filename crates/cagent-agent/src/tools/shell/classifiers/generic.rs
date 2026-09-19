use super::super::*;

pub(super) fn parse_no_args(args: &[String]) -> Option<SafeCommandParse> {
    args.is_empty().then(|| SafeCommandParse::new(None))
}

pub(super) fn parse_date(args: &[String]) -> Option<SafeCommandParse> {
    rich_no_args(args, "Read date")
}

pub(super) fn parse_whoami(args: &[String]) -> Option<SafeCommandParse> {
    rich_no_args(args, "Read identity")
}

fn rich_no_args(args: &[String], label: &str) -> Option<SafeCommandParse> {
    args.is_empty().then(|| {
        let mut parsed = SafeCommandParse::new(Some(SafeShellPresentation::Read));
        parsed.detail = Some(SafePresentationDetail::new(label));
        parsed
    })
}
