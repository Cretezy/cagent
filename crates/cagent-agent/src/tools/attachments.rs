use std::path::PathBuf;

use crate::{AttachmentSpec, RuntimeError};

/// Reconstructs an editable user draft and its confirmed attachment specs from history.
///
/// Older sessions stored only captured attachment payloads. For those rows, attachment
/// references in the original text are treated as confirmed when at least one capture exists.
#[must_use]
pub fn history_user_draft<T: HistoryDraftSource + ?Sized>(
    node: &T,
) -> Option<(String, Vec<AttachmentSpec>)> {
    node.history_user_draft()
}

pub trait HistoryDraftSource {
    fn history_user_draft(&self) -> Option<(String, Vec<AttachmentSpec>)>;
}

impl HistoryDraftSource for crate::HistoryNode {
    fn history_user_draft(&self) -> Option<(String, Vec<AttachmentSpec>)> {
        if self.kind != crate::NodeKind::UserMessage {
            return None;
        }
        let node = self;
        let text = node
            .composer_text
            .clone()
            .or_else(|| node.content.get("text")?.as_str().map(str::to_owned))?;
        let specs = node
            .content
            .get("attachment_specs")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_else(|| {
                node.content
                    .get("attachments")
                    .and_then(serde_json::Value::as_array)
                    .filter(|attachments| !attachments.is_empty())
                    .and_then(|_| parse_attachment_specs(&text).ok())
                    .unwrap_or_default()
            });
        Some((text, specs))
    }
}

impl HistoryDraftSource for crate::presentation::HistoryRow {
    fn history_user_draft(&self) -> Option<(String, Vec<AttachmentSpec>)> {
        self.user_draft
            .as_ref()
            .map(|draft| (draft.text.clone(), draft.attachment_specs.clone()))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttachmentReference {
    pub spec: AttachmentSpec,
    pub start: usize,
    pub end: usize,
}

/// Parses `@file`, inclusive `@file:start-end`, and `@{path with spaces}` references.
/// `@@` is treated as a literal at-sign.
///
/// # Errors
///
/// Returns an error for an unclosed quoted path or an invalid numeric range.
pub fn parse_attachment_specs(text: &str) -> Result<Vec<AttachmentSpec>, RuntimeError> {
    let mut specs = Vec::new();
    for reference in parse_attachment_references(text)? {
        if !specs.contains(&reference.spec) {
            specs.push(reference.spec);
        }
    }
    Ok(specs)
}

/// Parses attachment references and retains their byte spans in the original composer text.
///
/// `@@` literals are omitted. Unlike [`parse_attachment_specs`], duplicate references are retained
/// so frontends can render and edit each occurrence independently.
///
/// # Errors
///
/// Returns an error for an unclosed quoted path or an invalid numeric range.
pub fn parse_attachment_references(text: &str) -> Result<Vec<AttachmentReference>, RuntimeError> {
    let mut specs = Vec::new();
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'@' || (index > 0 && !bytes[index - 1].is_ascii_whitespace()) {
            index += 1;
            continue;
        }
        if bytes.get(index + 1) == Some(&b'@') {
            index += 2;
            continue;
        }
        let (token, next) = attachment_token(text, index)?;
        if !token.is_empty() {
            let (path, start_line, end_line) = split_attachment_range(&token)?;
            let spec = AttachmentSpec {
                path: PathBuf::from(path),
                start_line,
                end_line,
            };
            specs.push(AttachmentReference {
                spec,
                start: index,
                end: next,
            });
        }
        index = next;
    }
    Ok(specs)
}

fn attachment_token(text: &str, at: usize) -> Result<(String, usize), RuntimeError> {
    let bytes = text.as_bytes();
    if bytes.get(at + 1) == Some(&b'{') {
        let start = at + 2;
        let relative_end = text[start..]
            .find('}')
            .ok_or_else(|| RuntimeError::InvalidOption("unclosed @{...} attachment path".into()))?;
        let end = start + relative_end;
        let range_end = text[end + 1..]
            .find(char::is_whitespace)
            .map_or(text.len(), |offset| end + 1 + offset);
        Ok((
            format!("{}{}", &text[start..end], &text[end + 1..range_end]),
            range_end,
        ))
    } else {
        let start = at + 1;
        let end = text[start..]
            .find(char::is_whitespace)
            .map_or(text.len(), |offset| start + offset);
        Ok((text[start..end].to_owned(), end))
    }
}

fn split_attachment_range(token: &str) -> Result<(&str, Option<u64>, Option<u64>), RuntimeError> {
    let Some((path, range)) = token.rsplit_once(':') else {
        return Ok((token, None, None));
    };
    let Some((start, end)) = range.split_once('-') else {
        return Ok((token, None, None));
    };
    if !start.bytes().all(|byte| byte.is_ascii_digit())
        || !end.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Ok((token, None, None));
    }
    let start = start.parse::<u64>().map_err(|error| {
        RuntimeError::InvalidOption(format!("invalid attachment range start: {error}"))
    })?;
    let end = end.parse::<u64>().map_err(|error| {
        RuntimeError::InvalidOption(format!("invalid attachment range end: {error}"))
    })?;
    if start == 0 || end < start {
        return Err(RuntimeError::InvalidOption(
            "attachment ranges are one-based and inclusive".into(),
        ));
    }
    Ok((path, Some(start), Some(end)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ranges_spaces_literals_and_deduplicates() {
        let source =
            "review @src/lib.rs:20-80 and @{docs/file with spaces.md} @@literal @src/lib.rs:20-80";
        let specs = parse_attachment_specs(source).unwrap();
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].path, PathBuf::from("src/lib.rs"));
        assert_eq!(
            (specs[0].start_line, specs[0].end_line),
            (Some(20), Some(80))
        );
        assert_eq!(specs[1].path, PathBuf::from("docs/file with spaces.md"));

        let references = parse_attachment_references(source).unwrap();
        assert_eq!(references.len(), 3);
        assert_eq!(
            &source[references[0].start..references[0].end],
            "@src/lib.rs:20-80"
        );
        assert_eq!(
            &source[references[1].start..references[1].end],
            "@{docs/file with spaces.md}"
        );
        assert_eq!(references[0].spec, references[2].spec);
    }

    #[test]
    fn history_user_draft_recovers_legacy_confirmed_references() {
        let node = crate::HistoryNode {
            id: crate::NodeId::new(),
            parent_id: None,
            turn_id: Some(crate::TurnId::new()),
            owner_id: None,
            request_index: None,
            kind: crate::NodeKind::UserMessage,
            status: "completed".into(),
            role: Some("user".into()),
            summary: None,
            content: serde_json::json!({
                "text": "inspect @src/lib.rs:2-4",
                "attachments": [{"path": "/workspace/src/lib.rs"}],
            }),
            composer_text: None,
            created_at: String::new(),
            completed_at: Some(String::new()),
            active: true,
        };

        let (draft, specs) = history_user_draft(&node).unwrap();
        assert_eq!(draft, "inspect @src/lib.rs:2-4");
        assert_eq!(specs[0].path, PathBuf::from("src/lib.rs"));
        assert_eq!((specs[0].start_line, specs[0].end_line), (Some(2), Some(4)));
    }
}
