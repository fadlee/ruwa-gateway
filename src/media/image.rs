//! Image preparation (GOWA `prepareImageForSend`, `FitThumbnail`).

use std::io::Cursor;

use image::{DynamicImage, GenericImageView, ImageFormat, imageops::FilterType};

/// GOWA `ThumbnailMaxWidth` x `ThumbnailMaxHeight`.
const THUMB_MAX_W: u32 = 100;
const THUMB_MAX_H: u32 = 512;
/// GOWA `compressedImageMaxEdge` / `hdImageMaxEdge`.
const COMPRESSED_MAX_W: u32 = 600;
const HD_MAX_EDGE: u32 = 2560;
/// Refuse to decode rasters above this many pixels (decompression bombs).
const MAX_PIXELS: u64 = 64_000_000;

pub struct PreparedImage {
    pub bytes: Vec<u8>,
    pub mime: String,
    pub width: u32,
    pub height: u32,
    /// JPEG thumbnail, absent when the image could not be decoded.
    pub thumbnail: Option<Vec<u8>>,
}

/// Shrink to fit inside `max_w` x `max_h`, never enlarging.
fn fit_box(img: &DynamicImage, max_w: u32, max_h: u32) -> DynamicImage {
    let (w, h) = img.dimensions();
    if w <= max_w && h <= max_h {
        return img.clone();
    }
    img.resize(max_w, max_h, FilterType::Lanczos3)
}

fn encode(img: &DynamicImage, format: ImageFormat) -> anyhow::Result<Vec<u8>> {
    let mut out = Cursor::new(Vec::new());
    match format {
        // JPEG has no alpha channel.
        ImageFormat::Jpeg => DynamicImage::ImageRgb8(img.to_rgb8()).write_to(&mut out, format)?,
        _ => img.write_to(&mut out, format)?,
    }
    Ok(out.into_inner())
}

pub fn thumbnail(img: &DynamicImage) -> anyhow::Result<Vec<u8>> {
    encode(&fit_box(img, THUMB_MAX_W, THUMB_MAX_H), ImageFormat::Jpeg)
}

fn decode(bytes: &[u8]) -> anyhow::Result<(DynamicImage, ImageFormat)> {
    let reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let format = reader
        .format()
        .ok_or_else(|| anyhow::anyhow!("unknown image format"))?;
    let (w, h) = reader.into_dimensions()?;
    if u64::from(w) * u64::from(h) > MAX_PIXELS {
        anyhow::bail!("image too large: {w}x{h}");
    }
    Ok((image::load_from_memory_with_format(bytes, format)?, format))
}

/// GOWA `SendImage` preparation: WebP is converted to PNG, `compress`
/// fits the image in 600x2560, `hd` caps the long edge at 2560 as JPEG.
pub fn prepare(bytes: Vec<u8>, compress: bool, hd: bool) -> anyhow::Result<PreparedImage> {
    let (img, format) = decode(&bytes)?;
    let thumbnail = Some(thumbnail(&img)?);

    let (out, out_format) = if hd {
        let fitted = fit_box(&img, HD_MAX_EDGE, HD_MAX_EDGE);
        (encode(&fitted, ImageFormat::Jpeg)?, ImageFormat::Jpeg)
    } else if compress {
        let target = if format == ImageFormat::Jpeg {
            ImageFormat::Jpeg
        } else {
            ImageFormat::Png
        };
        (
            encode(&fit_box(&img, COMPRESSED_MAX_W, HD_MAX_EDGE), target)?,
            target,
        )
    } else if format == ImageFormat::WebP {
        (encode(&img, ImageFormat::Png)?, ImageFormat::Png)
    } else {
        (bytes, format)
    };

    let (width, height) = image::ImageReader::new(Cursor::new(&out))
        .with_guessed_format()?
        .into_dimensions()?;
    Ok(PreparedImage {
        mime: out_format.to_mime_type().to_owned(),
        bytes: out,
        width,
        height,
        thumbnail,
    })
}

/// GOWA `MaxGroupPhotoDimension` / `GroupPhotoQuality`.
const GROUP_PHOTO_EDGE: u32 = 640;
const GROUP_PHOTO_QUALITY: u8 = 80;
/// GOWA `ValidateGroupPhotoFormat` input cap.
const GROUP_PHOTO_MAX_INPUT: usize = 10 * 1024 * 1024;

/// GOWA `ValidateGroupPhotoFormat`: declared type, size, and sniffed format.
/// The error text is what GOWA puts after `Image validation failed: `.
pub fn validate_group_photo(content_type: &str, bytes: &[u8]) -> Result<(), String> {
    if !content_type.is_empty()
        && !matches!(
            content_type,
            "image/jpeg" | "image/jpg" | "image/png" | "image/webp"
        )
    {
        return Err(format!(
            "unsupported image format: {content_type} (supported: JPEG, PNG, WebP)"
        ));
    }
    if bytes.len() > GROUP_PHOTO_MAX_INPUT {
        return Err(format!(
            "image file too large: {} bytes (max 10MB for processing)",
            bytes.len()
        ));
    }
    let reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("invalid image file: {e}"))?;
    let format = reader
        .format()
        .ok_or_else(|| "invalid image file: image: unknown format".to_owned())?;
    let (w, h) = reader
        .into_dimensions()
        .map_err(|e| format!("invalid image file: {e}"))?;
    if u64::from(w) * u64::from(h) > MAX_PIXELS {
        return Err(format!("invalid image file: image too large: {w}x{h}"));
    }
    if !matches!(
        format,
        ImageFormat::Jpeg | ImageFormat::Png | ImageFormat::WebP
    ) {
        return Err(format!(
            "unsupported image format detected: {}",
            format
                .extensions_str()
                .first()
                .copied()
                .unwrap_or("unknown")
        ));
    }
    Ok(())
}

/// GOWA `ProcessGroupPhoto`: centre-crop to a square, shrink to 640px,
/// re-encode as JPEG (quality 80).
pub fn group_photo(bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
    let (img, _) = decode(bytes)?;
    let (w, h) = img.dimensions();
    let side = w.min(h);
    let square = img.crop_imm((w - side) / 2, (h - side) / 2, side, side);
    let square = if side > GROUP_PHOTO_EDGE {
        square.resize_exact(GROUP_PHOTO_EDGE, GROUP_PHOTO_EDGE, FilterType::Lanczos3)
    } else {
        square
    };
    let mut out = Cursor::new(Vec::new());
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, GROUP_PHOTO_QUALITY)
        .encode_image(&DynamicImage::ImageRgb8(square.to_rgb8()))?;
    Ok(out.into_inner())
}

/// Best-effort JPEG thumbnail of arbitrary image bytes (documents, links).
pub fn thumbnail_of(bytes: &[u8]) -> Option<Vec<u8>> {
    decode(bytes).ok().and_then(|(img, _)| thumbnail(&img).ok())
}

#[cfg(test)]
pub(crate) fn sample_png(w: u32, h: u32) -> Vec<u8> {
    let img = DynamicImage::new_rgba8(w, h);
    encode(&img, ImageFormat::Png).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compress_fits_width() {
        let p = prepare(sample_png(1200, 800), true, false).unwrap();
        assert_eq!((p.width, p.height), (600, 400));
        assert_eq!(p.mime, "image/png");
        let thumb = image::load_from_memory(&p.thumbnail.unwrap()).unwrap();
        assert_eq!(thumb.width(), 100);
    }

    #[test]
    fn hd_is_jpeg_capped() {
        let p = prepare(sample_png(3000, 1000), false, true).unwrap();
        assert_eq!(p.mime, "image/jpeg");
        assert_eq!(p.width, 2560);
    }

    #[test]
    fn untouched_when_not_compressed() {
        let png = sample_png(50, 40);
        let p = prepare(png.clone(), false, false).unwrap();
        assert_eq!(p.bytes, png);
        assert_eq!((p.width, p.height), (50, 40));
    }

    #[test]
    fn group_photo_is_a_capped_square_jpeg() {
        let jpeg = group_photo(&sample_png(1600, 900)).unwrap();
        let img = image::load_from_memory(&jpeg).unwrap();
        assert_eq!((img.width(), img.height()), (640, 640));
        assert_eq!(
            image::guess_format(&jpeg).unwrap(),
            image::ImageFormat::Jpeg
        );
        let small = image::load_from_memory(&group_photo(&sample_png(100, 60)).unwrap()).unwrap();
        assert_eq!((small.width(), small.height()), (60, 60));
    }

    #[test]
    fn group_photo_validation_messages() {
        let png = sample_png(8, 8);
        assert!(validate_group_photo("image/png", &png).is_ok());
        assert!(validate_group_photo("", &png).is_ok());
        assert_eq!(
            validate_group_photo("image/gif", &png).unwrap_err(),
            "unsupported image format: image/gif (supported: JPEG, PNG, WebP)"
        );
        assert!(
            validate_group_photo("image/png", b"nope")
                .unwrap_err()
                .starts_with("invalid image file")
        );
        let big = vec![0u8; 10 * 1024 * 1024 + 1];
        assert_eq!(
            validate_group_photo("image/png", &big).unwrap_err(),
            "image file too large: 10485761 bytes (max 10MB for processing)"
        );
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(prepare(b"not an image".to_vec(), true, false).is_err());
    }
}
