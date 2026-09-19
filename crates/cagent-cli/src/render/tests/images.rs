//! Submitted image attachment behavior.

use super::*;

#[test]
fn submitted_image_rows_keep_their_own_preview_target() {
    let image = cagent_agent::protocol::ImageAttachment {
        id: cagent_agent::protocol::ImageAttachmentId::new(),
        number: 1,
        sha256: "first".into(),
        mime_type: "image/png".into(),
        width: 10,
        height: 20,
        size_bytes: 30,
        blob_id: cagent_agent::protocol::BlobId::new(),
    };
    let user =
        crate::app::local_transcript_block(cagent_agent::protocol::TranscriptBlockKind::User {
            label: None,
            text: "[Image #1]".into(),
            attachments: Vec::new(),
            images: vec![image.clone()],
            image_chips: vec![cagent_agent::protocol::ImageChipRange {
                image_id: image.id,
                start: 0,
                end: 10,
            }],
        });
    let rows = crate::render::transcript::history_item_rows(&user, Path::new("/workspace"), 80);
    let target = rows
        .iter()
        .flat_map(|row| row.images())
        .next()
        .expect("image target");
    assert_eq!(target.image.id, image.id);
}

#[test]
fn literal_image_lookalikes_do_not_become_preview_targets() {
    let image = cagent_agent::protocol::ImageAttachment {
        id: cagent_agent::protocol::ImageAttachmentId::new(),
        number: 1,
        sha256: "first".into(),
        mime_type: "image/png".into(),
        width: 10,
        height: 20,
        size_bytes: 30,
        blob_id: cagent_agent::protocol::BlobId::new(),
    };
    let image_id = image.id;
    let text = "literal [Image #1] real [Image #1]";
    let user =
        crate::app::local_transcript_block(cagent_agent::protocol::TranscriptBlockKind::User {
            label: None,
            text: text.into(),
            attachments: Vec::new(),
            images: vec![image],
            image_chips: vec![cagent_agent::protocol::ImageChipRange {
                image_id,
                start: 24,
                end: 34,
            }],
        });

    let targets = crate::render::transcript::history_item_rows(&user, Path::new("/workspace"), 80)
        .into_iter()
        .flat_map(|row| row.images().to_vec())
        .collect::<Vec<_>>();
    assert_eq!(targets.len(), 1);
}

#[test]
fn only_semantic_image_ranges_become_submitted_chips() {
    let text = "literal [Image #1]";
    assert!(super::submitted_attachment_labels(text, &[], &[]).is_empty());

    let chips = [cagent_agent::protocol::ImageChipRange {
        image_id: cagent_agent::protocol::ImageAttachmentId::new(),
        start: 8,
        end: 18,
    }];
    let substitutions = super::submitted_attachment_labels(text, &[], &chips);
    assert_eq!(substitutions.len(), 1);
    assert_eq!(substitutions[0].label, "[Image #1]");
}
