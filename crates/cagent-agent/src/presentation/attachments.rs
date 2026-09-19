use crate::parse_attachment_references;
use crate::protocol::{AttachmentSpec, CapturedAttachment};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubmittedAttachmentKind {
    File,
    Directory,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubmittedAttachment {
    pub start: usize,
    pub end: usize,
    pub spec: AttachmentSpec,
    pub kind: SubmittedAttachmentKind,
}

/// Matches submitted attachment syntax to the snapshots retained with a user message.
#[must_use]
pub fn project_submitted_attachments(
    text: &str,
    captured: &[CapturedAttachment],
) -> Vec<SubmittedAttachment> {
    let Ok(references) = parse_attachment_references(text) else {
        return Vec::new();
    };
    references
        .into_iter()
        .filter_map(|reference| {
            let attachment = captured
                .iter()
                .find(|attachment| attachment.path.ends_with(&reference.spec.path))?;
            Some(SubmittedAttachment {
                start: reference.start,
                end: reference.end,
                spec: reference.spec,
                kind: if attachment.content.starts_with("Directory ") {
                    SubmittedAttachmentKind::Directory
                } else {
                    SubmittedAttachmentKind::File
                },
            })
        })
        .collect()
}

/// Projects attachment references when only their durable specs are
/// available. Older or synthetic transcript entries may not retain captured
/// content, so these entries conservatively use the file presentation kind.
#[must_use]
pub fn project_submitted_attachment_specs(
    text: &str,
    specs: &[AttachmentSpec],
) -> Vec<SubmittedAttachment> {
    let Ok(references) = parse_attachment_references(text) else {
        return Vec::new();
    };
    references
        .into_iter()
        .filter_map(|reference| {
            specs
                .iter()
                .any(|spec| spec == &reference.spec)
                .then_some(SubmittedAttachment {
                    start: reference.start,
                    end: reference.end,
                    spec: reference.spec,
                    kind: SubmittedAttachmentKind::File,
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn projects_only_captured_references_and_preserves_source_ranges() {
        let text = "Review @src/lib.rs and @missing.rs";
        let captured = [CapturedAttachment {
            path: PathBuf::from("/workspace/src/lib.rs"),
            start_line: 1,
            end_line: 3,
            sha256: "hash".into(),
            size_bytes: 12,
            content: "fn main() {}".into(),
        }];

        let projected = project_submitted_attachments(text, &captured);

        assert_eq!(projected.len(), 1);
        assert_eq!(&text[projected[0].start..projected[0].end], "@src/lib.rs");
        assert_eq!(projected[0].kind, SubmittedAttachmentKind::File);
        assert_eq!(projected[0].spec.path, PathBuf::from("src/lib.rs"));
    }

    #[test]
    fn projects_durable_specs_when_captured_content_is_unavailable() {
        let text = "Review @src/lib.rs:2-3";
        let specs = [AttachmentSpec {
            path: PathBuf::from("src/lib.rs"),
            start_line: Some(2),
            end_line: Some(3),
        }];

        let projected = project_submitted_attachment_specs(text, &specs);

        assert_eq!(projected.len(), 1);
        assert_eq!(
            &text[projected[0].start..projected[0].end],
            "@src/lib.rs:2-3"
        );
        assert_eq!(projected[0].kind, SubmittedAttachmentKind::File);
    }
}
