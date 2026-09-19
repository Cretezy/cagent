use super::super::*;
use super::parsing::*;

pub(super) fn parse_checksum(args: &[String]) -> Option<SafeCommandParse> {
    input_parse(
        parse_options(
            args,
            &grammar("bzt", "", &["--binary", "--text", "--tag", "--zero"], EMPTY),
        )?,
        SafeShellPresentation::Read,
        0,
        None,
        false,
    )
    .map(|parsed| label_paths(parsed, "Read checksum"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_files_and_stdin() {
        let file = parse_checksum(&["README.md".into()]).expect("file checksum");
        assert_eq!(file.operands.len(), 1);

        let stdin = parse_checksum(&[]).expect("stdin checksum");
        assert!(stdin.operands.is_empty());
        assert!(stdin.transparent);
        assert!(stdin.presentation.is_none());
    }

    #[test]
    fn rejects_check_mode() {
        assert!(parse_checksum(&["-c".into(), "checksums".into()]).is_none());
        assert!(parse_checksum(&["--check".into(), "checksums".into()]).is_none());
    }
}
