//! PDF text extraction.
//!
//! PDF is the format that matters most for this app and the one with the least
//! pleasant text layer: glyphs live in content streams, mapped through font
//! encodings that are sometimes simple and sometimes a ToUnicode CMap. Getting
//! that right is a library's job, so this module delegates to `pdf-extract`
//! (pure Rust, no C runtime) and does the two things that are ours to do:
//!
//! * cap the output so one enormous manual cannot bloat the index;
//! * fail softly. A scanned PDF, a corrupt file, an encrypted file — all return
//!   `None`, and the classifier falls back to name and format, exactly as it does
//!   when no backend is compiled in. Nothing about a failed extraction is fatal.

use std::path::Path;

use super::{Extracted, TEXT_LIMIT};

/// Extracts the text layer of a PDF.
pub fn extract(path: &Path) -> Option<Extracted> {
    let text = pdf_extract::extract_text(path).ok()?;
    if text.trim().is_empty() {
        // A scanned PDF has no text layer. OCR is a separate, opt-in backend.
        return None;
    }

    let truncated = text.chars().count() > TEXT_LIMIT;
    let text: String = text.chars().take(TEXT_LIMIT).collect();
    Some(Extracted {
        text,
        method: "pdf",
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a one-page PDF with a single text run.
    ///
    /// Written out by hand — including the cross-reference offsets — so the test
    /// needs no fixture file and proves the reader handles a real document.
    fn make_pdf(text: &str) -> Vec<u8> {
        let stream = format!("BT /F1 24 Tf 72 700 Td ({text}) Tj ET");
        let objects = [
            "<</Type/Catalog/Pages 2 0 R>>".to_string(),
            "<</Type/Pages/Kids[3 0 R]/Count 1>>".to_string(),
            "<</Type/Page/Parent 2 0 R/MediaBox[0 0 612 792]/Contents 4 0 R\
             /Resources<</Font<</F1 5 0 R>>>>>>"
                .to_string(),
            format!("<</Length {}>>stream\n{}\nendstream", stream.len(), stream),
            "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>".to_string(),
        ];

        let mut out = String::from("%PDF-1.4\n");
        let mut offsets: Vec<usize> = Vec::new();
        for (index, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.push_str(&format!("{} 0 obj\n{}\nendobj\n", index + 1, body));
        }

        let xref_offset = out.len();
        out.push_str(&format!("xref\n0 {}\n", objects.len() + 1));
        out.push_str("0000000000 65535 f \n");
        for offset in &offsets {
            out.push_str(&format!("{offset:010} 00000 n \n"));
        }
        out.push_str(&format!(
            "trailer\n<</Size {}/Root 1 0 R>>\nstartxref\n{}\n%%EOF\n",
            objects.len() + 1,
            xref_offset
        ));
        out.into_bytes()
    }

    #[test]
    fn reads_the_text_layer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("invoice.pdf");
        std::fs::write(&path, make_pdf("Invoice from Acme")).unwrap();

        let extracted = extract(&path).expect("a text PDF should extract");
        assert_eq!(extracted.method, "pdf");
        assert!(
            extracted.text.contains("Invoice"),
            "got: {:?}",
            extracted.text
        );
    }

    #[test]
    fn garbage_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.pdf");
        std::fs::write(&path, b"%PDF-1.4 not really a pdf").unwrap();
        assert!(extract(&path).is_none());
    }

    #[test]
    fn missing_file_is_not_an_error() {
        assert!(extract(Path::new("/nonexistent/none.pdf")).is_none());
    }
}
