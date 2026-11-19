//! Office Open XML extraction (DOCX, XLSX, PPTX).
//!
//! An `.docx` is a ZIP of XML parts; so is every other OOXML format. That means
//! text extraction needs a ZIP reader and nothing else — no LibreOffice, no C
//! library, no model. The XML is walked by a small tag stripper rather than a
//! full parser: the only structure that matters for classification is where
//! paragraphs, rows and cells end, and a parser would be a large dependency for
//! that.
//!
//! What is deliberately *not* extracted: styles, comments, tracked changes,
//! images, charts, embedded objects. For sorting, the words are the signal.

use std::io::Read;
use std::path::Path;

use super::Extracted;

/// Cap on extracted text, enough for classification and the search index.
pub const OFFICE_TEXT_CAP: usize = 64 * 1024;

/// Extracts text from a DOCX, XLSX or PPTX container.
pub fn extract(path: &Path) -> Option<Extracted> {
    let extension = crate::detect::extension_of(path)?;
    let file = std::fs::File::open(path).ok()?;
    let mut archive = zip::ZipArchive::new(file).ok()?;

    let names: Vec<String> = (0..archive.len())
        .filter_map(|index| {
            archive
                .by_index(index)
                .ok()
                .map(|entry| entry.name().to_string())
        })
        .collect();

    let wanted = wanted_parts(&extension, &names);
    if wanted.is_empty() {
        return None;
    }

    let mut text = String::new();
    for part in &wanted {
        let Ok(mut entry) = archive.by_name(part) else {
            continue;
        };
        let mut xml = String::new();
        if entry.read_to_string(&mut xml).is_err() {
            continue;
        }
        text.push_str(&xml_text(&xml));
        text.push('\n');
        if text.chars().count() >= OFFICE_TEXT_CAP {
            break;
        }
    }

    if text.trim().is_empty() {
        return None;
    }
    let truncated = text.chars().count() > OFFICE_TEXT_CAP;
    let text: String = text.chars().take(OFFICE_TEXT_CAP).collect();
    Some(Extracted {
        text,
        method: "ooxml",
        truncated,
    })
}

/// The XML parts that carry human-visible text for this format.
fn wanted_parts(extension: &str, names: &[String]) -> Vec<String> {
    match extension {
        "docx" => names
            .iter()
            .filter(|n| *n == "word/document.xml")
            .cloned()
            .collect(),
        "xlsx" => {
            let mut parts: Vec<String> = names
                .iter()
                .filter(|n| n.as_str() == "xl/sharedStrings.xml")
                .cloned()
                .collect();
            parts.extend(
                names
                    .iter()
                    .filter(|n| n.starts_with("xl/worksheets/sheet") && n.ends_with(".xml"))
                    .cloned(),
            );
            parts
        }
        "pptx" => names
            .iter()
            .filter(|n| n.starts_with("ppt/slides/slide") && n.ends_with(".xml"))
            .cloned()
            .collect(),
        _ => Vec::new(),
    }
}

/// Strips XML tags, keeping text and turning block boundaries into newlines.
pub fn xml_text(xml: &str) -> String {
    let mut out = String::with_capacity(xml.len() / 4);
    let mut chars = xml.chars();

    while let Some(c) = chars.next() {
        if c != '<' {
            if c != '>' {
                out.push(c);
            }
            continue;
        }

        // Consume the tag, and let the block-level ones break the line.
        let mut tag = String::new();
        for t in chars.by_ref() {
            if t == '>' {
                break;
            }
            tag.push(t);
        }
        let tag = tag.trim().to_ascii_lowercase();
        let closing = tag.starts_with('/');
        let name = tag.trim_start_matches('/').trim_end_matches('/');

        if name.starts_with("w:p")
            || name.starts_with("a:p")
            || name.starts_with("w:tr")
            || name.starts_with("row")
            || name == "w:tab"
        {
            out.push('\n');
        } else if name.starts_with("w:br") {
            out.push(' ');
        } else if closing && name.starts_with("w:tc") {
            out.push('\t');
        }
    }

    decode_entities(&out)
}

/// Decodes the five XML entities plus numeric character references.
fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let after = &rest[start..];
        let Some(end) = after.find(';').filter(|e| *e <= 10) else {
            out.push('&');
            rest = &after[1..];
            continue;
        };
        let entity = &after[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| {
                    entity
                        .strip_prefix('#')
                        .and_then(|dec| dec.parse::<u32>().ok())
                })
                .and_then(char::from_u32),
        };
        match decoded {
            Some(ch) => out.push(ch),
            None => out.push_str(&after[..=end]),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal STORED-entry ZIP writer, so the tests do not depend on the
    /// crate's writer API (and the fixtures stay readable).
    fn make_zip(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();
        let mut central: Vec<u8> = Vec::new();

        for (name, content) in entries {
            let offset = out.len() as u32;
            let data = content.as_bytes();
            let crc = crc32(data);
            let name_bytes = name.as_bytes();

            out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
            out.extend_from_slice(&20u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(name_bytes);
            out.extend_from_slice(data);

            central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&crc.to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u32.to_le_bytes());
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(name_bytes);
        }

        let cd_offset = out.len() as u32;
        let cd_size = central.len() as u32;
        out.extend_from_slice(&central);

        out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&cd_size.to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for byte in data {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
            }
        }
        !crc
    }

    #[test]
    fn strips_tags_and_keeps_text() {
        let xml = "<w:document><w:body><w:p><w:r><w:t>Invoice</w:t></w:r>\
                   <w:r><w:t xml:space=\"preserve\"> from Acme</w:t></w:r></w:p>\
                   <w:p><w:t>Total 48123</w:t></w:p></w:body></w:document>";
        let text = xml_text(xml);
        assert!(text.contains("Invoice"));
        assert!(text.contains("from Acme"));
        assert!(text.contains("Total 48123"));
        assert!(text.contains('\n'));
        assert!(!text.contains('<'));
    }

    #[test]
    fn decodes_entities() {
        assert_eq!(
            decode_entities("A &amp; B &lt;x&gt; &quot;q&quot;"),
            "A & B <x> \"q\""
        );
        assert_eq!(decode_entities("caf&#233;"), "café");
        assert_eq!(decode_entities("broken & entity"), "broken & entity");
    }

    #[test]
    fn reads_a_docx() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("contract.docx");
        let xml = "<w:document><w:body><w:p><w:t>Service Contract</w:t></w:p>\
                   <w:p><w:t>between Acme and Beta</w:t></w:p></w:body></w:document>";
        std::fs::write(&path, make_zip(&[("word/document.xml", xml)])).unwrap();

        let extracted = extract(&path).expect("docx text should extract");
        assert_eq!(extracted.method, "ooxml");
        assert!(extracted.text.contains("Service Contract"));
        assert!(extracted.text.contains("Acme"));
    }

    #[test]
    fn reads_an_xlsx_shared_strings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("budget.xlsx");
        let shared = "<sst><si><t>Q4 Budget</t></si><si><t>Marketing</t></si></sst>";
        let sheet =
            "<worksheet><sheetData><row><c t=\"s\"><v>0</v></c></row></sheetData></worksheet>";
        std::fs::write(
            &path,
            make_zip(&[
                ("xl/sharedStrings.xml", shared),
                ("xl/worksheets/sheet1.xml", sheet),
            ]),
        )
        .unwrap();

        let extracted = extract(&path).expect("xlsx text should extract");
        assert!(extracted.text.contains("Q4 Budget"));
        assert!(extracted.text.contains("Marketing"));
    }

    #[test]
    fn reads_pptx_slides() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deck.pptx");
        let slide = "<p:sld><p:cSld><a:p><a:t>Roadmap 2027</a:t></a:p></cSld></p:sld>";
        std::fs::write(
            &path,
            make_zip(&[
                ("ppt/slides/slide1.xml", slide),
                ("ppt/slides/slide2.xml", "<p:sld><a:t>Pricing</a:t></p:sld>"),
            ]),
        )
        .unwrap();

        let extracted = extract(&path).expect("pptx text should extract");
        assert!(extracted.text.contains("Roadmap 2027"));
        assert!(extracted.text.contains("Pricing"));
    }

    #[test]
    fn the_public_router_reaches_this_backend() {
        use crate::detect::{Detected, Kind, Method};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("contract.docx");
        let xml =
            "<w:document><w:body><w:p><w:t>Service Contract</w:t></w:p></w:body></w:document>";
        std::fs::write(&path, make_zip(&[("word/document.xml", xml)])).unwrap();

        let detected = Detected {
            mime: "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
                .to_string(),
            extension: Some("docx".to_string()),
            kind: Kind::Document,
            method: Method::Magic,
            is_text: false,
        };
        let extracted = crate::extract::extract(&path, &detected)
            .expect("the format-basic router should reach the office backend");
        assert!(extracted.text.contains("Service Contract"));
    }

    #[test]
    fn a_zip_that_is_not_ooxml_yields_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("archive.docx");
        std::fs::write(&path, make_zip(&[("readme.txt", "hello")])).unwrap();
        assert!(extract(&path).is_none());
    }

    #[test]
    fn unsupported_extension_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, b"plain").unwrap();
        assert!(extract(&path).is_none());
    }
}
