use super::super::*;

pub(super) fn parse_test(args: &[String]) -> Option<SafeCommandParse> {
    let [predicate, path] = args else {
        return None;
    };
    (predicate == "-f").then(|| {
        let mut parsed = SafeCommandParse::new(Some(SafeShellPresentation::Read))
            .path(path.clone(), "file predicate");
        parsed.detail =
            Some(SafePresentationDetail::new("Check file").arguments([path.clone()], true));
        parsed
    })
}
