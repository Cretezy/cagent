//! Agent-owned attachment syntax and history reconstruction.

use std::path::PathBuf;

use crate::{AttachmentSpec, RuntimeError};

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
        let text = self
            .composer_text
            .clone()
            .or_else(|| self.content.get("text")?.as_str().map(str::to_owned))?;
        let specs = self
            .content
            .get("attachment_specs")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_else(|| {
                self.content
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

pub fn parse_attachment_specs(text: &str) -> Result<Vec<AttachmentSpec>, RuntimeError> {
    let mut specs = Vec::new();
    for reference in parse_attachment_references(text)? {
        if !specs.contains(&reference.spec) {
            specs.push(reference.spec);
        }
    }
    Ok(specs)
}

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
            specs.push(AttachmentReference {
                spec: AttachmentSpec {
                    path: PathBuf::from(path),
                    start_line,
                    end_line,
                },
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
