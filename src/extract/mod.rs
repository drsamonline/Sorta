//! Text extraction.
//!
//! Extraction is what turns "a PDF" into "an invoice from Acme, dated November".
//! It is also the part with the largest dependency surface, so it is behind
//! feature flags and every backend is optional:
//!
//! | Flag | Formats | Backend |
//! |---|---|---|
//! | `format-basic` | PDF, DOCX, XLSX, PPTX, TXT | xberg |
//! | `format-ocr` | images, scanned PDFs | RapidOCR PP-OCRv4 |
//! | `format-media` | audio, video | transcripts |
//! | `format-archive` | ZIP, 7z, RAR | listing or extraction |
//!
//! Phase 1 ships the plain-text path only (no dependency, always available) and
//! reports which backends are compiled in. A missing backend is not an error: the
//! classifier simply runs without text, and a file that needed text lands in
//! quarantine instead of being guessed at.

use std::path::Path;

use crate::detect::{Detected, Kind};

/// How much text is read from a plain-text file.
pub const TEXT_LIMIT: usize = 256 * 1024;

/// Result of extraction.
#[derive(Debug, Clone)]
pub struct Extracted {
    /// Extracted body, possibly truncated.
    pub text: String,
    /// Which backend produced it, for the history signals.
    pub method: &'static str,
    /// True when the body hit the size cap.
    pub truncated: bool,
}

/// Extracts text, returning `None` when no backend covers this format.
pub fn extract(path: &Path, detected: &Detected) -> Option<Extracted> {
    // Always available: plain text, read straight off the disk.
    if matches!(detected.kind, Kind::Text | Kind::Code) && detected.is_text {
        let head = crate::detect::read_head(path, TEXT_LIMIT).ok()?;
        let truncated = head.len() == TEXT_LIMIT;
        let text = String::from_utf8_lossy(&head).to_string();
        return Some(Extracted {
            text,
            method: "plain",
            truncated,
        });
    }

    #[cfg(feature = "format-basic")]
    {
        if let Some(extracted) = basic::extract(path, detected) {
            return Some(extracted);
        }
    }

    #[cfg(feature = "format-ocr")]
    {
        if let Some(extracted) = ocr::extract(path, detected) {
            return Some(extracted);
        }
    }

    None
}

/// Convenience wrapper: just the text.
pub fn extract_text(path: &Path, detected: &Detected) -> Option<String> {
    extract(path, detected).map(|e| e.text)
}

/// Backends compiled into this build, for `sorta status`.
pub fn backends() -> Vec<&'static str> {
    let mut out = vec!["plain"];
    if cfg!(feature = "format-basic") {
        out.push("xberg-basic");
    }
    if cfg!(feature = "format-ocr") {
        out.push("rapidocr");
    }
    if cfg!(feature = "format-media") {
        out.push("media");
    }
    if cfg!(feature = "format-archive") {
        out.push("archive");
    }
    out
}

/// Why a format could not be read, for the quarantine reason.
pub fn unsupported_reason(detected: &Detected) -> &'static str {
    match detected.kind {
        Kind::Document => "no document backend compiled in (needs format-basic)",
        Kind::Spreadsheet => "no spreadsheet backend compiled in (needs format-basic)",
        Kind::Presentation => "no presentation backend compiled in (needs format-basic)",
        Kind::Image => "no OCR backend compiled in (needs format-ocr)",
        Kind::Audio | Kind::Video => "no media backend compiled in (needs format-media)",
        Kind::Archive => "no archive backend compiled in (needs format-archive)",
        Kind::Text | Kind::Code => "not valid UTF-8 text",
        Kind::Other => "unknown format",
    }
}

/// Document formats handled by the `format-basic` flag.
#[cfg(feature = "format-basic")]
mod basic {
    use super::Extracted;
    use crate::detect::Detected;
    use std::path::Path;

    /// Placeholder for the xberg router.
    ///
    /// Phase 2 lands the real call here:
    /// `xberg::Document::open(path)?.text()` with per-format adapters. Until
    /// then this returns `None` so the classifier behaves exactly as it does
    /// without the feature, instead of pretending it read the file.
    pub fn extract(_path: &Path, _detected: &Detected) -> Option<Extracted> {
        None
    }
}

/// OCR for images and scanned pages.
#[cfg(feature = "format-ocr")]
mod ocr {
    use super::Extracted;
    use crate::detect::Detected;
    use std::path::Path;

    /// Placeholder for the RapidOCR backend (phase 3).
    pub fn extract(_path: &Path, _detected: &Detected) -> Option<Extracted> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Method, Detected};

    fn text_file(name: &str, body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, body).unwrap();
        (dir, path)
    }

    #[test]
    fn plain_text_is_read() {
        let (_dir, path) = text_file("notes.txt", "hello world");
        let detected = Detected {
            mime: "text/plain".to_string(),
            extension: Some("txt".to_string()),
            kind: Kind::Text,
            method: Method::ContentSniff,
            is_text: true,
        };
        let extracted = extract(&path, &detected).expect("plain text should extract");
        assert_eq!(extracted.text, "hello world");
        assert_eq!(extracted.method, "plain");
        assert!(!extracted.truncated);
    }

    #[test]
    fn binary_formats_report_no_text_in_phase_one() {
        let (_dir, path) = text_file("doc.pdf", "%PDF-1.4");
        let detected = Detected {
            mime: "application/pdf".to_string(),
            extension: Some("pdf".to_string()),
            kind: Kind::Document,
            method: Method::Magic,
            is_text: false,
        };
        assert!(extract(&path, &detected).is_none());
        assert!(backends().contains(&"plain"));
    }

    #[test]
    fn reasons_are_specific() {
        let detected = Detected {
            mime: "image/png".to_string(),
            extension: Some("png".to_string()),
            kind: Kind::Image,
            method: Method::Magic,
            is_text: false,
        };
        assert!(unsupported_reason(&detected).contains("OCR"));
    }
}
