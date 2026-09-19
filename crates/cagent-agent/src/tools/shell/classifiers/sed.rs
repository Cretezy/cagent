use super::super::*;

pub(super) fn parse_sed(args: &[String]) -> Option<SafeCommandParse> {
    if args.len() < 2 || args[0] != "-n" {
        return None;
    }
    if !args[1].split(';').all(is_numeric_print_range) {
        return None;
    }
    let paths = &args[2..];
    if paths
        .iter()
        .any(|path| path != "-" && path.starts_with('-'))
    {
        return None;
    }
    let mut parsed = SafeCommandParse::new(Some(SafeShellPresentation::Read));
    for path in paths {
        parsed = parsed.path(path.clone(), "positional input");
    }
    if paths.is_empty() {
        parsed.presentation = None;
        parsed.transparent = true;
    }
    Some(parsed)
}

fn is_numeric_print_range(script: &str) -> bool {
    let Some(range) = script.strip_suffix('p') else {
        return false;
    };
    let pieces = range.split(',').collect::<Vec<_>>();
    matches!(pieces.len(), 1 | 2)
        && pieces
            .iter()
            .all(|piece| !piece.is_empty() && piece.chars().all(|c| c.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_semicolon_separated_numeric_print_ranges() {
        assert!(parse_sed(&["-n".into(), "1010,1040p;1195,1265p".into(),]).is_some());
    }

    #[test]
    fn accepts_multiple_input_files() {
        let parsed = parse_sed(&[
            "-n".into(),
            "430,525p;1820,1880p;2460,2520p;2800,2870p".into(),
            "crates/cagent-cli/src/render/activity.rs".into(),
            "crates/cagent-cli/src/render/mod.rs".into(),
        ])
        .expect("multiple explicit input files should be read-safe");

        assert_eq!(parsed.operands.len(), 2);
    }

    #[test]
    fn rejects_non_print_sed_scripts() {
        for script in ["1,2p;d", "1,2p;", "1,2p;3,4,5p"] {
            assert!(
                parse_sed(&["-n".into(), script.into()]).is_none(),
                "{script}"
            );
        }
    }

    #[test]
    fn rejects_options_after_the_script() {
        for option in ["-i", "--in-place"] {
            assert!(
                parse_sed(&["-n".into(), "1,2p".into(), option.into()]).is_none(),
                "{option}"
            );
        }
    }

    #[test]
    fn accepts_stdin_operand() {
        assert!(parse_sed(&["-n".into(), "1,2p".into(), "-".into()]).is_some());
    }
}
