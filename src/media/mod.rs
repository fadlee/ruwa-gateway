//! Media preparation for sends and incoming downloads, mirroring GOWA
//! `usecase/send.go` helpers and `pkg/utils` (image bounds, ffmpeg, MIME).

pub mod fetch;
pub mod ffmpeg;
pub mod image;

/// GOWA `config/settings.go` defaults.
pub const MAX_IMAGE_SIZE: usize = 20_000_000;
pub const MAX_FILE_SIZE: usize = 50_000_000;
pub const MAX_VIDEO_SIZE: usize = 100_000_000;
pub const MAX_DOWNLOAD_SIZE: u64 = 500_000_000;

/// An uploaded multipart file, or a file fetched from a URL.
#[derive(Debug, Clone, Default)]
pub struct Upload {
    pub filename: String,
    /// Declared `Content-Type` (multipart part header), if any.
    pub content_type: String,
    pub bytes: Vec<u8>,
}

/// Go `http.DetectContentType`-style sniffing, falling back to the file name.
pub fn detect_mime(bytes: &[u8], filename: &str) -> String {
    if let Some(kind) = infer::get(bytes) {
        return kind.mime_type().to_owned();
    }
    if let Some(mime) = mime_guess::from_path(filename).first() {
        return mime.essence_str().to_owned();
    }
    if std::str::from_utf8(&bytes[..bytes.len().min(512)]).is_ok() {
        return "text/plain; charset=utf-8".to_owned();
    }
    "application/octet-stream".to_owned()
}

/// GOWA `resolveDocumentMIME`: extension first (documents are often zip
/// containers that sniff as `application/zip`), then content.
pub fn document_mime(filename: &str, bytes: &[u8]) -> String {
    match mime_guess::from_path(filename).first() {
        Some(mime) => mime.essence_str().to_owned(),
        None => detect_mime(bytes, filename),
    }
}

/// Extension (with dot) for a saved download: original name first, then MIME.
pub fn extension_for(filename: &str, mime: &str) -> String {
    if let Some(ext) = std::path::Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
    {
        return format!(".{}", ext.to_ascii_lowercase());
    }
    let essence = mime.split(';').next().unwrap_or_default().trim();
    let preferred = match essence {
        "image/jpeg" => Some("jpg"),
        "audio/ogg" => Some("ogg"),
        "audio/mpeg" => Some("mp3"),
        "video/mp4" => Some("mp4"),
        "image/webp" => Some("webp"),
        _ => None,
    };
    preferred
        .or_else(|| mime_guess::get_mime_extensions_str(essence).and_then(|e| e.first().copied()))
        .map(|e| format!(".{e}"))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_png_and_falls_back_to_name() {
        let png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";
        assert_eq!(detect_mime(png, "x.bin"), "image/png");
        assert_eq!(
            detect_mime(b"\x00\x01\x02\xff\xfe", "a.pdf"),
            "application/pdf"
        );
        assert_eq!(detect_mime(b"hello", "noext"), "text/plain; charset=utf-8");
    }

    #[test]
    fn document_mime_prefers_extension() {
        assert_eq!(
            document_mime("a.docx", b"PK\x03\x04"),
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        );
    }

    #[test]
    fn extensions() {
        assert_eq!(extension_for("Report.PDF", "application/pdf"), ".pdf");
        assert_eq!(extension_for("", "image/jpeg"), ".jpg");
        assert_eq!(extension_for("", "audio/ogg; codecs=opus"), ".ogg");
        assert_eq!(extension_for("", "application/x-unknown-thing"), "");
    }
}
