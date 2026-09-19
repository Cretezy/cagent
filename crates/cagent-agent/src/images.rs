//! Provider-neutral clipboard-image normalization and message preparation.

use std::collections::{HashMap, HashSet};
use std::io::Cursor;

use base64::Engine as _;
use image::GenericImageView as _;
use sha2::{Digest as _, Sha256};

use crate::protocol::{ImageAttachment, ImageAttachmentId, ImageChipRange, UserDraft};
use crate::provider::ModelContentPart;
use crate::{BlobId, RuntimeError};

pub const IMAGE_MIME_TYPE: &str = "image/png";
pub const DEFAULT_MAX_TRANSPORT_DIMENSION: u32 = 2048;
pub const MAX_IMAGE_DIMENSION: u32 = 16_384;
const MAX_IMAGE_DECODE_BYTES: u64 = 256 * 1024 * 1024;

fn decode_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    limits.max_alloc = Some(MAX_IMAGE_DECODE_BYTES);
    limits
}

#[must_use]
pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Decode raster clipboard data and encode it as a canonical PNG.
pub fn normalize_to_png(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32), RuntimeError> {
    let mut reader = image::ImageReader::new(Cursor::new(bytes));
    reader = reader.with_guessed_format().map_err(|error| {
        RuntimeError::InvalidOption(format!("clipboard image is invalid: {error}"))
    })?;
    reader.limits(decode_limits());
    let image = reader.decode().map_err(|error| {
        RuntimeError::InvalidOption(format!("clipboard image is invalid: {error}"))
    })?;
    let (width, height) = image.dimensions();
    let mut png = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|error| {
            RuntimeError::InvalidOption(format!("could not encode clipboard image: {error}"))
        })?;
    Ok((png, width, height))
}

#[must_use]
pub fn metadata(
    number: u64,
    blob_id: BlobId,
    png: &[u8],
    width: u32,
    height: u32,
) -> ImageAttachment {
    ImageAttachment {
        id: ImageAttachmentId::new(),
        number,
        sha256: sha256(png),
        mime_type: IMAGE_MIME_TYPE.into(),
        width,
        height,
        size_bytes: u64::try_from(png.len()).unwrap_or(u64::MAX),
        blob_id,
    }
}

/// Prepare ordered text/image parts. The binary part follows only the first
/// occurrence of each unique image; repeated chips remain text references.
pub fn prepare_content_parts(
    draft: &UserDraft,
    blobs: &HashMap<BlobId, Vec<u8>>,
    resize_images: bool,
) -> Result<Vec<ModelContentPart>, RuntimeError> {
    validate_user_draft(draft)?;
    let images = draft
        .images
        .iter()
        .map(|image| (image.id, image))
        .collect::<HashMap<_, _>>();
    let mut chips = draft.image_chips.clone();
    chips.sort_by_key(|chip| chip.start);
    validate_chip_ranges(&draft.text, &chips, &images)?;
    let mut parts = Vec::new();
    let mut cursor = 0;
    let mut emitted = HashSet::new();
    for chip in chips {
        if cursor < chip.end {
            parts.push(ModelContentPart::Text {
                text: draft.text[cursor..chip.end].to_owned(),
            });
        }
        cursor = chip.end;
        if emitted.insert(chip.image_id) {
            let image = images[&chip.image_id];
            let original = blobs.get(&image.blob_id).ok_or_else(|| {
                RuntimeError::InvalidOption(format!("image blob {} is missing", image.blob_id))
            })?;
            let (png, width, height) = transport_png(original, resize_images)?;
            parts.push(ModelContentPart::Image {
                mime_type: IMAGE_MIME_TYPE.into(),
                sha256: image.sha256.clone(),
                data: base64::engine::general_purpose::STANDARD.encode(png),
                width,
                height,
            });
        }
    }
    if cursor < draft.text.len() {
        parts.push(ModelContentPart::Text {
            text: draft.text[cursor..].to_owned(),
        });
    }
    Ok(parts)
}

pub fn validate_user_draft(draft: &UserDraft) -> Result<(), RuntimeError> {
    let images = draft
        .images
        .iter()
        .map(|image| (image.id, image))
        .collect::<HashMap<_, _>>();
    if images.len() != draft.images.len() {
        return Err(RuntimeError::InvalidOption(
            "image metadata contains duplicate IDs".into(),
        ));
    }
    let numbers = draft
        .images
        .iter()
        .map(|image| image.number)
        .collect::<HashSet<_>>();
    let hashes = draft
        .images
        .iter()
        .map(|image| image.sha256.as_str())
        .collect::<HashSet<_>>();
    if numbers.len() != draft.images.len() || hashes.len() != draft.images.len() {
        return Err(RuntimeError::InvalidOption(
            "image metadata must be unique by number and content hash".into(),
        ));
    }
    let mut chips = draft.image_chips.clone();
    chips.sort_by_key(|chip| chip.start);
    validate_chip_ranges(&draft.text, &chips, &images)?;
    let referenced = chips
        .iter()
        .map(|chip| chip.image_id)
        .collect::<HashSet<_>>();
    if referenced.len() != images.len() || images.keys().any(|id| !referenced.contains(id)) {
        return Err(RuntimeError::InvalidOption(
            "image metadata contains an unreferenced image".into(),
        ));
    }
    Ok(())
}

fn validate_chip_ranges(
    text: &str,
    chips: &[ImageChipRange],
    images: &HashMap<ImageAttachmentId, &ImageAttachment>,
) -> Result<(), RuntimeError> {
    let mut previous_end = 0;
    for chip in chips {
        let image = images.get(&chip.image_id).ok_or_else(|| {
            RuntimeError::InvalidOption("image chip refers to missing metadata".into())
        })?;
        let expected = format!("[Image #{}]", image.number);
        if chip.start < previous_end
            || chip.end > text.len()
            || !text.is_char_boundary(chip.start)
            || !text.is_char_boundary(chip.end)
            || text.get(chip.start..chip.end) != Some(expected.as_str())
        {
            return Err(RuntimeError::InvalidOption(
                "invalid image chip range".into(),
            ));
        }
        previous_end = chip.end;
    }
    Ok(())
}

fn transport_png(bytes: &[u8], resize: bool) -> Result<(Vec<u8>, u32, u32), RuntimeError> {
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::Png);
    reader.limits(decode_limits());
    let image = reader.decode().map_err(|error| {
        RuntimeError::InvalidOption(format!("stored image is invalid: {error}"))
    })?;
    let (width, height) = image.dimensions();
    if !resize || width.max(height) <= DEFAULT_MAX_TRANSPORT_DIMENSION {
        return Ok((bytes.to_vec(), width, height));
    }
    let resized = image.resize(
        DEFAULT_MAX_TRANSPORT_DIMENSION,
        DEFAULT_MAX_TRANSPORT_DIMENSION,
        image::imageops::FilterType::Lanczos3,
    );
    let (width, height) = resized.dimensions();
    let mut png = Vec::new();
    resized
        .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|error| RuntimeError::InvalidOption(format!("could not resize image: {error}")))?;
    Ok((png, width, height))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = image::DynamicImage::new_rgba8(width, height);
        let mut bytes = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();
        bytes
    }

    #[test]
    fn emits_each_unique_image_once_in_chip_order() {
        let bytes = png(2, 3);
        let blob_id = BlobId::new();
        let image = metadata(1, blob_id, &bytes, 2, 3);
        let text = "[Image #1] hello [Image #1]".to_owned();
        let draft = UserDraft {
            text,
            image_chips: vec![
                ImageChipRange {
                    image_id: image.id,
                    start: 0,
                    end: 10,
                },
                ImageChipRange {
                    image_id: image.id,
                    start: 17,
                    end: 27,
                },
            ],
            images: vec![image],
            ..UserDraft::default()
        };
        let parts =
            prepare_content_parts(&draft, &HashMap::from([(blob_id, bytes)]), true).unwrap();
        assert_eq!(
            parts
                .iter()
                .filter(|part| matches!(part, ModelContentPart::Image { .. }))
                .count(),
            1
        );
        assert_eq!(parts.len(), 3);
    }

    #[test]
    fn proportionally_resizes_transport_but_not_original() {
        let bytes = png(4096, 1024);
        let (transport, width, height) = transport_png(&bytes, true).unwrap();
        assert_eq!((width, height), (2048, 512));
        assert_ne!(transport, bytes);
        assert_eq!(transport_png(&bytes, false).unwrap().1, 4096);
    }

    #[test]
    fn rejects_unreferenced_image_metadata() {
        let bytes = png(2, 3);
        let image = metadata(1, BlobId::new(), &bytes, 2, 3);
        let draft = UserDraft {
            images: vec![image],
            ..UserDraft::default()
        };

        assert!(validate_user_draft(&draft).is_err());
    }
}
