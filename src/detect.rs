//! What is this file?
//!
//! Three layers, cheapest first:
//! 1. **magic bytes** (`infer`) — authoritative when it fires
//! 2. **extension** — a guess, never trusted over magic
//! 3. **content sniff** — 8 KB UTF-8 heuristic, so an extension-less text file
//!    is still recognised as text
//!
//! Detection never fails on a readable file: an unknown blob is `Kind::Other`
//! with `Method::Unknown`, which the classifier turns into a low-confidence
//! guess and the pipeline turns into quarantine.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Coarse file family. Everything the sorter reasons about uses this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Document,
    Spreadsheet,
    Presentation,
    Image,
    Audio,
    Video,
    Archive,
    Text,
    Code,
    Other,
}

impl Kind {
    /// Folder category used by the default folder template.
    pub fn category(self) -> &'static str {
        match self {
            Self::Document => "Documents",
            Self::Spreadsheet => "Spreadsheets",
            Self::Presentation => "Presentations",
            Self::Image => "Images",
            Self::Audio => "Audio",
            Self::Video => "Video",
            Self::Archive => "Archives",
            Self::Text => "Text",
            Self::Code => "Code",
            Self::Other => "Unsorted",
        }
    }

    /// Whether the OCR backend is relevant.
    pub fn wants_ocr(self) -> bool {
        matches!(self, Self::Image | Self::Document)
    }
}

/// How the type was established.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    Magic,
    Extension,
    ContentSniff,
    Unknown,
}

/// Result of detection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Detected {
    /// MIME type, `application/octet-stream` when nothing matched.
    pub mime: String,
    /// Lower-cased extension without the dot, when the name has one.
    pub extension: Option<String>,
    /// Coarse family.
    pub kind: Kind,
    /// Which layer decided.
    pub method: Method,
    /// True when the first 8 KB decode as UTF-8 text.
    pub is_text: bool,
}

/// Files that are never sorted: OS bookkeeping, not documents.
pub const SYSTEM_NOISE: [&str; 4] = [".DS_Store", "Thumbs.db", "desktop.ini", "$RECYCLE.BIN"];

/// True for OS bookkeeping names and the recycle bin.
pub fn is_system_noise(name: &str) -> bool {
    SYSTEM_NOISE
        .iter()
        .any(|n| n.eq_ignore_ascii_case(name))
}

/// Extension of a path, lower-cased.
pub fn extension_of(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
}

/// Reads up to `limit` bytes from the head of a file.
pub fn read_head(path: &Path, limit: usize) -> std::io::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    let mut buf = vec![0u8; limit];
    let mut filled = 0usize;
    loop {
        if filled >= limit {
            break;
        }
        match file.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    buf.truncate(filled);
    Ok(buf)
}

/// 8 KB UTF-8 heuristic.
///
/// A NUL byte anywhere in the head disqualifies a file. Otherwise we accept it
/// if it decodes as UTF-8, or if at least 97% of the bytes are valid prefix —
/// which tolerates a multi-byte character cut in half by the read boundary.
pub fn looks_like_text(buf: &[u8]) -> bool {
    if buf.is_empty() || buf.contains(&0u8) {
        return false;
    }
    match std::str::from_utf8(buf) {
        Ok(_) => true,
        Err(err) => err.valid_up_to() * 100 / buf.len() >= 97,
    }
}

/// Maps a MIME type to a coarse family.
pub fn kind_from_mime(mime: &str) -> Kind {
    let m = mime.to_ascii_lowercase();
    if m.starts_with("image/") {
        return Kind::Image;
    }
    if m.starts_with("audio/") {
        return Kind::Audio;
    }
    if m.starts_with("video/") {
        return Kind::Video;
    }
    if m.starts_with("text/") {
        return match m.as_str() {
            "text/x-rust" | "text/x-python" | "text/x-c" | "text/html" | "text/css" => Kind::Code,
            _ => Kind::Text,
        };
    }
    match m.as_str() {
        "application/pdf" => Kind::Document,
        "application/msword" => Kind::Document,
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => Kind::Document,
        "application/rtf" => Kind::Document,
        "application/vnd.oasis.opendocument.text" => Kind::Document,
        "application/vnd.ms-excel" => Kind::Spreadsheet,
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => Kind::Spreadsheet,
        "application/vnd.oasis.opendocument.spreadsheet" => Kind::Spreadsheet,
        "text/csv" => Kind::Spreadsheet,
        "application/vnd.ms-powerpoint" => Kind::Presentation,
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => {
            Kind::Presentation
        }
        "application/vnd.oasis.opendocument.presentation" => Kind::Presentation,
        "application/zip"
        | "application/x-tar"
        | "application/gzip"
        | "application/x-7z-compressed"
        | "application/vnd.rar"
        | "application/x-bzip2"
        | "application/x-xz" => Kind::Archive,
        "application/json" | "application/xml" | "application/javascript" => Kind::Code,
        _ => Kind::Other,
    }
}

/// Maps an extension to a coarse family. Used when magic bytes say nothing.
pub fn kind_from_extension(ext: &str) -> Kind {
    match ext.to_ascii_lowercase().as_str() {
        "pdf" | "doc" | "docx" | "odt" | "rtf" | "pages" | "epub" => Kind::Document,
        "xls" | "xlsx" | "ods" | "csv" | "tsv" | "numbers" => Kind::Spreadsheet,
        "ppt" | "pptx" | "odp" | "key" => Kind::Presentation,
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "tif" | "tiff" | "heic" | "svg" => {
            Kind::Image
        }
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" | "opus" => Kind::Audio,
        "mp4" | "mkv" | "mov" | "avi" | "webm" | "wmv" => Kind::Video,
        "zip" | "7z" | "rar" | "tar" | "gz" | "bz2" | "xz" | "zst" => Kind::Archive,
        "txt" | "md" | "markdown" | "rst" | "log" | "org" => Kind::Text,
        "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "go" | "c" | "h" | "cpp" | "hpp" | "java"
        | "kt" | "rb" | "php" | "sh" | "bash" | "zsh" | "ps1" | "sql" | "toml" | "yaml" | "yml"
        | "json" | "xml" | "html" | "css" | "scss" => Kind::Code,
        _ => Kind::Other,
    }
}

/// Detects the type of a file on disk.
pub fn detect(path: &Path) -> std::io::Result<Detected> {
    let extension = extension_of(path);
    let head = read_head(path, 8192)?;
    let is_text = looks_like_text(&head);

    if let Some(kind) = infer::get(&head) {
        let mime = kind.mime_type().to_string();
        return Ok(Detected {
            kind: kind_from_mime(&mime),
            mime,
            extension,
            method: Method::Magic,
            is_text,
        });
    }

    if let Some(ext) = extension.clone() {
        let kind = kind_from_extension(&ext);
        if kind != Kind::Other {
            return Ok(Detected {
                mime: mime_for_extension(&ext),
                extension,
                kind,
                method: Method::Extension,
                is_text,
            });
        }
    }

    if is_text {
        return Ok(Detected {
            mime: "text/plain".to_string(),
            extension,
            kind: Kind::Text,
            method: Method::ContentSniff,
            is_text,
        });
    }

    Ok(Detected {
        mime: "application/octet-stream".to_string(),
        extension,
        kind: Kind::Other,
        method: Method::Unknown,
        is_text,
    })
}

fn mime_for_extension(ext: &str) -> String {
    match ext {
        "pdf" => "application/pdf",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "csv" => "text/csv",
        "zip" => "application/zip",
        "7z" => "application/x-7z-compressed",
        "rar" => "application/vnd.rar",
        "tar" => "application/x-tar",
        "gz" => "application/gzip",
        _ => "application/octet-stream",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn system_noise_is_recognised() {
        assert!(is_system_noise(".DS_Store"));
        assert!(is_system_noise("Thumbs.db"));
        assert!(is_system_noise("desktop.ini"));
        assert!(!is_system_noise("invoice.pdf"));
    }

    #[test]
    fn utf8_heuristic() {
        assert!(looks_like_text(b"hello world"));
        assert!(!looks_like_text(b"\x00\x01\x02binary"));
        assert!(!looks_like_text(b""));
    }

    #[test]
    fn extensions_map_to_families() {
        assert_eq!(kind_from_extension("PDF"), Kind::Document);
        assert_eq!(kind_from_extension("xlsx"), Kind::Spreadsheet);
        assert_eq!(kind_from_extension("mp4"), Kind::Video);
        assert_eq!(kind_from_extension("weird"), Kind::Other);
    }

    #[test]
    fn magic_beats_extension() {
        let dir = tempfile::tempdir().unwrap();
        // A PNG header behind a misleading .pdf extension.
        let path = dir.path().join("screenshot.pdf");
        let mut f = File::create(&path).unwrap();
        f.write_all(b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR").unwrap();
        f.flush().unwrap();

        let detected = detect(&path).unwrap();
        assert_eq!(detected.kind, Kind::Image);
        assert_eq!(detected.method, Method::Magic);
        assert_eq!(detected.mime, "image/png");
    }

    #[test]
    fn extensionless_text_is_sniffed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("NOTES");
        std::fs::write(&path, b"remember the milk\n").unwrap();
        let detected = detect(&path).unwrap();
        assert_eq!(detected.kind, Kind::Text);
        assert_eq!(detected.method, Method::ContentSniff);
    }
}
