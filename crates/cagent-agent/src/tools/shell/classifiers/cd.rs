use super::super::*;

pub(super) fn parse_cd(args: &[String]) -> Option<SafeCommandParse> {
    if args.len() != 1 || args[0] == "-" || args[0].starts_with('-') {
        return None;
    }
    let target = args[0].clone();
    Some(
        SafeCommandParse::new(None)
            .path(target.clone(), "working-directory transition")
            .with_transition(target),
    )
}
impl SafeCommandParse {
    fn with_transition(mut self, target: String) -> Self {
        self.directory_transition = Some(target);
        self
    }
}
