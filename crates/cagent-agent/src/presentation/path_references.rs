use std::ops::Range;

use pulldown_cmark::{Event, Options, Parser, Tag};

/// A local path reference embedded in assistant-authored Markdown.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PathReference {
    /// Byte range of the complete reference, including `@` and any line suffix.
    pub span: Range<usize>,
    /// The path without `@`, braces, or a line suffix.
    pub path: String,
    /// Optional positive, one-based logical line.
    pub line: Option<usize>,
}

/// Finds path references in Markdown prose and inline code.
///
/// Fenced and indented code blocks and ordinary Markdown link labels are
/// intentionally excluded. Returned spans address the original source.
#[must_use]
pub fn parse_path_references(source: &str) -> Vec<PathReference> {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut excluded = Vec::new();
    for (event, range) in Parser::new_ext(source, options).into_offset_iter() {
        if let Event::Start(Tag::CodeBlock(_) | Tag::Link { .. }) = event {
            excluded.push(range);
        }
    }
    parse_plain_path_references(source)
        .into_iter()
        .filter(|reference| {
            !excluded
                .iter()
                .any(|range| reference.span.start >= range.start && reference.span.end <= range.end)
        })
        .collect()
}

/// Removes display-only path-reference markers while preserving Markdown.
#[must_use]
pub fn strip_path_reference_markers(source: &str) -> String {
    let references = parse_path_references(source);
    if references.is_empty() {
        return source.to_owned();
    }
    let mut output = String::with_capacity(source.len());
    let mut offset = 0;
    for reference in references {
        output.push_str(&source[offset..reference.span.start]);
        let display = &source[reference.span.clone()];
        if let Some(braced) = display.strip_prefix("@{") {
            let close = braced
                .find('}')
                .expect("parsed braced path references contain a closing brace");
            output.push_str(&braced[..close]);
            output.push_str(&braced[close + 1..]);
        } else {
            output.push_str(display.strip_prefix('@').unwrap_or(display));
        }
        offset = reference.span.end;
    }
    output.push_str(&source[offset..]);
    output
}

pub(super) fn parse_plain_path_references(source: &str) -> Vec<PathReference> {
    let bytes = source.as_bytes();
    let mut references = Vec::new();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] != b'@' || !token_boundary(source, index) {
            index += source[index..].chars().next().map_or(1, char::len_utf8);
            continue;
        }
        if bytes.get(index + 1) == Some(&b'@') {
            index += 2;
            continue;
        }
        let parsed = if bytes.get(index + 1) == Some(&b'{') {
            parse_braced(source, index)
        } else {
            parse_unbraced(source, index)
        };
        if let Some(reference) = parsed {
            index = reference.span.end;
            references.push(reference);
        } else {
            index += 1;
        }
    }
    references
}

fn token_boundary(source: &str, index: usize) -> bool {
    if index == 0 {
        return true;
    }
    source[..index]
        .chars()
        .next_back()
        .is_some_and(|character| {
            !character.is_alphanumeric() && character != '_' && character != '@'
        })
}

fn parse_braced(source: &str, start: usize) -> Option<PathReference> {
    let path_start = start + 2;
    let relative_end = source[path_start..].find('}')?;
    let path_end = path_start + relative_end;
    let path = &source[path_start..path_end];
    if path.is_empty() || path.contains(['\n', '\r', '{', '}']) {
        return None;
    }
    let (end, line) = parse_optional_location(source, path_end + 1)?;
    Some(PathReference {
        span: start..end,
        path: path.to_owned(),
        line,
    })
}

fn parse_unbraced(source: &str, start: usize) -> Option<PathReference> {
    let path_start = start + 1;
    let mut end = path_start;
    for (relative, character) in source[path_start..].char_indices() {
        if character.is_whitespace()
            || matches!(
                character,
                '`' | '<' | '>' | '"' | '\'' | '|' | '*' | '[' | ']'
            )
        {
            break;
        }
        end = path_start + relative + character.len_utf8();
    }
    while end > path_start
        && source[..end].chars().next_back().is_some_and(|character| {
            matches!(character, '.' | ',' | ';' | ':' | '!' | '?' | ')' | '}')
        })
    {
        end -= source[..end].chars().next_back().unwrap().len_utf8();
    }
    if end == path_start {
        return None;
    }
    let token = &source[path_start..end];
    let (path, line) = split_location(token)?;
    valid_unbraced_path(path).then(|| PathReference {
        span: start..end,
        path: path.to_owned(),
        line,
    })
}

fn parse_optional_location(source: &str, path_end: usize) -> Option<(usize, Option<usize>)> {
    if source.as_bytes().get(path_end) != Some(&b':') {
        return Some((path_end, None));
    }
    let end = source[path_end + 1..]
        .char_indices()
        .take_while(|(_, character)| character.is_ascii_digit() || matches!(character, ':' | '-'))
        .last()
        .map_or(path_end + 1, |(index, character)| {
            path_end + 1 + index + character.len_utf8()
        });
    if let Some(line) = parse_location_suffix(&source[path_end + 1..end]) {
        Some((end, Some(line)))
    } else {
        Some((path_end, None))
    }
}

fn split_location(token: &str) -> Option<(&str, Option<usize>)> {
    let Some(colon) = token.char_indices().find_map(|(index, character)| {
        (character == ':' && !is_windows_drive_colon(token, index)).then_some(index)
    }) else {
        return Some((token, None));
    };
    let path = &token[..colon];
    if path.is_empty() {
        return None;
    }
    let line = parse_location_suffix(&token[colon + 1..])?;
    Some((path, Some(line)))
}

fn parse_location_suffix(suffix: &str) -> Option<usize> {
    let separator = suffix
        .char_indices()
        .find(|(_, character)| matches!(character, ':' | '-'));
    let (line, trailing) = separator.map_or((suffix, None), |(index, separator)| {
        (&suffix[..index], Some((separator, &suffix[index + 1..])))
    });
    let line = parse_positive_number(line)?;
    if let Some((separator, trailing)) = trailing {
        let trailing = parse_positive_number(trailing)?;
        if separator == '-' && trailing < line {
            return None;
        }
    }
    Some(line)
}

fn parse_positive_number(value: &str) -> Option<usize> {
    (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| value.parse::<usize>().ok())
        .flatten()
        .filter(|value| *value > 0)
}

fn is_windows_drive_colon(path: &str, index: usize) -> bool {
    index == 1
        && path.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && path
            .as_bytes()
            .get(2)
            .is_some_and(|byte| matches!(byte, b'\\' | b'/'))
}

fn valid_unbraced_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with([':', '{', '(', '#'])
        && !path.contains(['\n', '\r'])
        && path
            .char_indices()
            .filter(|(index, character)| *character == ':' && !is_windows_drive_colon(path, *index))
            .count()
            == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_braced_absolute_home_windows_and_lines() {
        let source =
            "@src/lib.rs @Cargo.toml:42 @{path with spaces/ü.rs}:7 @/tmp/a @~/a @C:\\code\\a.rs:9";
        let found = parse_path_references(source);
        assert_eq!(
            found
                .iter()
                .map(|item| (item.path.as_str(), item.line))
                .collect::<Vec<_>>(),
            vec![
                ("src/lib.rs", None),
                ("Cargo.toml", Some(42)),
                ("path with spaces/ü.rs", Some(7)),
                ("/tmp/a", None),
                ("~/a", None),
                (r"C:\code\a.rs", Some(9)),
            ]
        );
    }

    #[test]
    fn braced_reference_can_be_followed_by_prose() {
        let found = parse_plain_path_references("@{path with spaces.rs}:2 in a list");
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].path, "path with spaces.rs");
        assert_eq!(found[0].line, Some(2));
    }

    #[test]
    fn respects_boundaries_punctuation_escapes_and_invalid_locations() {
        let source =
            "mail@example.com, (@src/a.rs), @@escaped @x:0 @x:2:0 @x:4-2 @x:2:3:4 @x:2-4-6";
        let found = parse_path_references(source);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, "src/a.rs");
        assert_eq!(&source[found[0].span.clone()], "@src/a.rs");
    }

    #[test]
    fn parses_columns_and_line_ranges_at_their_first_line() {
        let source = "@SPEC.md:2:3 @SPEC.md:2-4 @{path with spaces}:7:9 @{other path}:8-10";
        let found = parse_path_references(source);
        assert_eq!(
            found
                .iter()
                .map(|item| (&source[item.span.clone()], item.path.as_str(), item.line))
                .collect::<Vec<_>>(),
            vec![
                ("@SPEC.md:2:3", "SPEC.md", Some(2)),
                ("@SPEC.md:2-4", "SPEC.md", Some(2)),
                ("@{path with spaces}:7:9", "path with spaces", Some(7)),
                ("@{other path}:8-10", "other path", Some(8)),
            ]
        );
    }

    #[test]
    fn includes_inline_code_but_excludes_code_blocks_and_link_labels() {
        let source = "`@src/inline.rs:3` [@src/label.rs](https://example.com)\n\n```text\n@src/fenced.rs\n```\n\n    @src/indented.rs\n";
        let found = parse_path_references(source);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, "src/inline.rs");
        assert_eq!(found[0].line, Some(3));
    }

    #[test]
    fn copy_stripping_preserves_markdown_and_code_blocks() {
        let source = "See **@src/lib.rs:4** and `@{path with spaces}:8-10`, then @src/main.rs:2:3.\n\n```\n@keep/me\n```";
        assert_eq!(
            strip_path_reference_markers(source),
            "See **src/lib.rs:4** and `path with spaces:8-10`, then src/main.rs:2:3.\n\n```\n@keep/me\n```"
        );
    }

    #[test]
    fn supports_trailing_colons_without_treating_them_as_locations() {
        let source = "See @TEST.md: (@TEST.md:), @TEST.md:2: and @{TEST.md}:";
        let found = parse_path_references(source);
        assert_eq!(
            found
                .iter()
                .map(|item| (&source[item.span.clone()], item.path.as_str(), item.line))
                .collect::<Vec<_>>(),
            vec![
                ("@TEST.md", "TEST.md", None),
                ("@TEST.md", "TEST.md", None),
                ("@TEST.md:2", "TEST.md", Some(2)),
                ("@{TEST.md}", "TEST.md", None),
            ]
        );
        assert_eq!(
            strip_path_reference_markers(source),
            "See TEST.md: (TEST.md:), TEST.md:2: and TEST.md:"
        );
    }
}
