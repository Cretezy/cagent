use super::super::*;

pub(super) fn parse_unzip(args: &[String]) -> Option<SafeCommandParse> {
    (args.len() == 2 && matches!(args[0].as_str(), "-l" | "-v" | "-Z" | "-t" | "-p")).then(|| {
        let mut parsed = SafeCommandParse::new(Some(SafeShellPresentation::List))
            .path(args[1].clone(), "archive");
        parsed.detail =
            Some(SafePresentationDetail::new("List archive").arguments([args[1].clone()], true));
        parsed
    })
}
