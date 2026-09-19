use super::super::*;
use super::parsing::*;

/// Parses jq forms that only transform standard input or explicit input files.
///
/// jq modules can load arbitrary files through `include` and `import`, so the
/// filter scanner rejects those keywords. Options that load a program, module,
/// or data file are deliberately not part of this grammar.
pub(super) fn parse_jq(args: &[String]) -> Option<SafeCommandParse> {
    let parsed = parse_options(
        args,
        &grammar(
            "nRsrcCMaSej",
            "",
            &[
                "--null-input",
                "--raw-input",
                "--slurp",
                "--raw-output",
                "--compact-output",
                "--color-output",
                "--monochrome-output",
                "--ascii-output",
                "--sort-keys",
                "--exit-status",
                "--join-output",
            ],
            EMPTY,
        ),
    )?;
    let (filter, inputs) = parsed.positionals.split_first()?;
    if filter_loads_module(filter) {
        return None;
    }
    if parsed.options.contains("-n") || parsed.options.contains("--null-input") {
        inputs.is_empty().then_some(SafeCommandParse::new(None))
    } else if inputs.is_empty() {
        Some(SafeCommandParse {
            transparent: true,
            ..SafeCommandParse::new(None)
        })
    } else {
        Some(inputs.iter().fold(
            SafeCommandParse::new(Some(SafeShellPresentation::Read)),
            |parsed, input| parsed.path(input.clone(), "positional input"),
        ))
    }
}

/// Reports module-loading keywords outside jq strings and `#` comments.
fn filter_loads_module(filter: &str) -> bool {
    let bytes = filter.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => {
                index += 1;
                while index < bytes.len() {
                    match bytes[index] {
                        b'\\' => index += 2,
                        b'"' => {
                            index += 1;
                            break;
                        }
                        _ => index += 1,
                    }
                }
            }
            b'#' => {
                index += 1;
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            byte if byte.is_ascii_alphabetic() || byte == b'_' => {
                let start = index;
                index += 1;
                while index < bytes.len()
                    && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
                {
                    index += 1;
                }
                if matches!(&filter[start..index], "include" | "import") {
                    return true;
                }
            }
            _ => index += 1,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_literal_filters_and_explicit_inputs() {
        for args in [
            vec![".".into(), "package.json".into()],
            vec!["-r".into(), ".name".into(), "package.json".into()],
            vec!["--null-input".into(), ".".into()],
        ] {
            assert!(parse_jq(&args).is_some(), "{args:?}");
        }
    }

    #[test]
    fn rejects_file_loading_options_and_modules() {
        for args in [
            vec!["--from-file".into(), "filter.jq".into()],
            vec!["-L".into(), "modules".into(), ".".into()],
            vec!["include \"helper\"; .".into(), "data.json".into()],
            vec!["import \"helper\" as helper; .".into(), "data.json".into()],
            vec!["-n".into(), ".".into(), "data.json".into()],
        ] {
            assert!(parse_jq(&args).is_none(), "{args:?}");
        }
    }

    #[test]
    fn does_not_treat_strings_or_comments_as_module_loads() {
        assert!(!filter_loads_module(r#""include helper""#));
        assert!(!filter_loads_module(". # import helper"));
    }
}
