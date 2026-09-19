use super::super::*;

pub(super) fn parse_tar(args: &[String]) -> Option<SafeCommandParse> {
    let mut list = false;
    let mut verbose = false;
    let mut archive = None;
    let mut occurrence = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-tf" | "-ft" | "-tvf" | "-vtf" => list = true,
            "-t" | "--list" => list = true,
            "-v" | "--verbose" => verbose = true,
            "-f" | "--file" => {
                index += 1;
                archive = Some(args.get(index)?.clone());
            }
            value if value.starts_with("--file=") => {
                archive = Some(value[7..].to_owned());
            }
            "--occurrence" => {
                index += 1;
                let value = args.get(index)?;
                if !value.chars().all(|character| character.is_ascii_digit()) {
                    return None;
                }
                occurrence = true;
            }
            value if value.starts_with("--occurrence=") => {
                let value = &value[13..];
                if value.is_empty() || !value.chars().all(|character| character.is_ascii_digit()) {
                    return None;
                }
                occurrence = true;
            }
            value
                if value.starts_with('-')
                    && value.chars().all(|character| {
                        character == '-' || character == 't' || character == 'v'
                    }) =>
            {
                list |= value.contains('t');
                verbose |= value.contains('v');
            }
            value if !value.starts_with('-') && archive.is_none() => {
                archive = Some(value.to_owned());
            }
            _ => return None,
        }
        index += 1;
    }
    let _ = (verbose, occurrence);
    (list && archive.is_some()).then(|| {
        let archive = archive.unwrap();
        let mut parsed = SafeCommandParse::new(Some(SafeShellPresentation::List))
            .path(archive.clone(), "archive");
        parsed.detail =
            Some(SafePresentationDetail::new("List archive").arguments([archive], true));
        parsed
    })
}
