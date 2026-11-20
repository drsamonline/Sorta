//! Extraction quality, measured rather than asserted.
//!
//! "Extraction works" is a claim; this turns it into a number. Point
//! `sorta bench-extract` at a folder of real documents and it reports, per file,
//! how many characters came out of how many bytes, and classifies the result.
//!
//! What this measures and what it does not:
//!
//! * **Measures** text yield, empty extractions, and garbling (replacement
//!   characters from broken encodings). Those are the three failure modes that
//!   matter for classification: no text means a scanned page, and garbled text means
//!   a font the PDF extractor could not map.
//! * **Does not measure** fidelity against a ground-truth transcription. Without
//!   hand-checked text for every file, a yield number is the honest ceiling — a file
//!   can yield plenty of characters that are the wrong characters.
//!
//! Thresholds are deliberately crude and documented here rather than tuned per
//! format, because a benchmark nobody can reproduce is worse than a rough one.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// A file under 200 characters with at least this many bytes is probably a scan.
pub const SPARSE_BYTES: u64 = 20_000;
/// Fewer characters than this, with a large file, counts as sparse.
pub const SPARSE_CHARS: usize = 200;
/// Share of replacement characters above which the text is called garbled.
pub const GARBLED_SHARE: f64 = 0.05;

/// How an extraction came out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Plenty of text came out.
    Text,
    /// A little text from a big file — usually a scan with a thin text layer.
    Sparse,
    /// Nothing came out at all.
    NoText,
    /// Text came out, but a lot of it is replacement characters.
    Garbled,
    /// The extractor returned an error.
    Error,
}

impl Verdict {
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Text => "text",
            Verdict::Sparse => "sparse",
            Verdict::NoText => "no text",
            Verdict::Garbled => "garbled",
            Verdict::Error => "error",
        }
    }
}

/// One file's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileReport {
    pub path: String,
    pub extension: String,
    pub bytes: u64,
    pub chars: usize,
    pub replacement_chars: usize,
    pub method: String,
    pub verdict: Verdict,
}

impl FileReport {
    /// Characters per byte, as a rough text density.
    pub fn density(&self) -> f64 {
        if self.bytes == 0 {
            0.0
        } else {
            self.chars as f64 / self.bytes as f64
        }
    }
}

/// Classifies one extraction.
pub fn classify(bytes: u64, text: Option<&str>) -> Verdict {
    let Some(text) = text else {
        return Verdict::NoText;
    };
    let chars = text.chars().count();

    if chars == 0 {
        return Verdict::NoText;
    }

    let replacement = text.chars().filter(|c| *c == '\u{FFFD}').count();
    if chars >= 20 && (replacement as f64) / (chars as f64) > GARBLED_SHARE {
        return Verdict::Garbled;
    }

    if bytes >= SPARSE_BYTES && chars < SPARSE_CHARS {
        return Verdict::Sparse;
    }
    Verdict::Text
}

/// Builds a report from a path and the text the extractor produced.
pub fn report(path: &Path, bytes: u64, text: Option<&str>, method: &str) -> FileReport {
    let extension = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    FileReport {
        path: path.display().to_string(),
        extension,
        bytes,
        chars: text.map(|t| t.chars().count()).unwrap_or(0),
        replacement_chars: text
            .map(|t| t.chars().filter(|c| *c == '\u{FFFD}').count())
            .unwrap_or(0),
        method: method.to_string(),
        verdict: classify(bytes, text),
    }
}

/// Totals for a run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    pub files: usize,
    pub bytes: u64,
    pub chars: usize,
    pub text: usize,
    pub sparse: usize,
    pub no_text: usize,
    pub garbled: usize,
    pub error: usize,
}

impl Summary {
    /// Share of files that yielded usable text.
    pub fn text_share(&self) -> f64 {
        if self.files == 0 {
            0.0
        } else {
            self.text as f64 / self.files as f64
        }
    }
}

/// Sums a set of reports.
pub fn summarise(reports: &[FileReport]) -> Summary {
    let mut summary = Summary {
        files: reports.len(),
        ..Summary::default()
    };
    for report in reports {
        summary.bytes += report.bytes;
        summary.chars += report.chars;
        match report.verdict {
            Verdict::Text => summary.text += 1,
            Verdict::Sparse => summary.sparse += 1,
            Verdict::NoText => summary.no_text += 1,
            Verdict::Garbled => summary.garbled += 1,
            Verdict::Error => summary.error += 1,
        }
    }
    summary
}

/// Per-extension rollup, so "how do PDFs do" is one line.
pub fn by_extension(reports: &[FileReport]) -> Vec<(String, Summary)> {
    let mut groups: std::collections::BTreeMap<String, Vec<FileReport>> = Default::default();
    for report in reports {
        let key = if report.extension.is_empty() {
            "(none)".to_string()
        } else {
            report.extension.clone()
        };
        groups.entry(key).or_default().push(report.clone());
    }
    groups
        .into_iter()
        .map(|(ext, group)| (ext, summarise(&group)))
        .collect()
}

/// Renders the markdown report: a summary, then the failures, then every row.
pub fn render(reports: &[FileReport], summary: &Summary) -> String {
    let mut out = String::new();

    out.push_str("## Extraction benchmark\n\n");
    out.push_str(&format!(
        "{} files, {} bytes in, {} characters out. Usable text: {:.0}%.\n\n",
        summary.files,
        summary.bytes,
        summary.chars,
        summary.text_share() * 100.0
    ));

    out.push_str("| Verdict | Files |\n|---|---|\n");
    out.push_str(&format!("| text | {} |\n", summary.text));
    out.push_str(&format!("| sparse | {} |\n", summary.sparse));
    out.push_str(&format!("| no text | {} |\n", summary.no_text));
    out.push_str(&format!("| garbled | {} |\n", summary.garbled));
    out.push_str(&format!("| error | {} |\n\n", summary.error));

    let by_ext = by_extension(reports);
    if by_ext.len() > 1 {
        out.push_str("### By extension\n\n");
        out.push_str("| Ext | Files | Text | Sparse | No text | Garbled | Chars |\n");
        out.push_str("|---|---|---|---|---|---|---|\n");
        for (ext, group) in &by_ext {
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} |\n",
                ext,
                group.files,
                group.text,
                group.sparse,
                group.no_text,
                group.garbled,
                group.chars
            ));
        }
        out.push('\n');
    }

    let problems: Vec<&FileReport> = reports
        .iter()
        .filter(|report| {
            matches!(
                report.verdict,
                Verdict::Sparse | Verdict::NoText | Verdict::Garbled | Verdict::Error
            )
        })
        .collect();

    if !problems.is_empty() {
        out.push_str("### Needs a closer look\n\n");
        out.push_str("| Verdict | Bytes | Chars | File |\n|---|---|---|---|\n");
        for report in problems.iter().take(40) {
            out.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                report.verdict.label(),
                report.bytes,
                report.chars,
                short_path(&report.path)
            ));
        }
        if problems.len() > 40 {
            out.push_str(&format!("\n…and {} more.\n", problems.len() - 40));
        }
        out.push('\n');
    }

    out.push_str("### Every file\n\n");
    out.push_str("| Ext | Bytes | Chars | Density | Method | Verdict | File |\n");
    out.push_str("|---|---|---|---|---|---|---|\n");
    for report in reports {
        out.push_str(&format!(
            "| {} | {} | {} | {:.4} | {} | {} | {} |\n",
            report.extension,
            report.bytes,
            report.chars,
            report.density(),
            report.method,
            report.verdict.label(),
            short_path(&report.path)
        ));
    }

    out
}

/// Trims a path to its last two components, so the table stays readable.
fn short_path(path: &str) -> String {
    let parts: Vec<&str> = path.split(['/', '\\']).filter(|p| !p.is_empty()).collect();
    match parts.len() {
        0 => path.to_string(),
        1 => parts[0].to_string(),
        n => format!("{}/{}", parts[n - 2], parts[n - 1]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_healthy_extraction_is_text() {
        assert_eq!(
            classify(50_000, Some("a page of real words")),
            Verdict::Text
        );
    }

    #[test]
    fn nothing_out_of_a_big_file_is_sparse() {
        assert_eq!(classify(500_000, Some("   ")), Verdict::Sparse);
        assert_eq!(classify(500_000, None), Verdict::NoText);
        assert_eq!(classify(500_000, Some("")), Verdict::NoText);
    }

    #[test]
    fn a_small_file_with_short_text_is_still_text() {
        // A tiny text file must not be called sparse just because it is short.
        assert_eq!(classify(200, Some("hello")), Verdict::Text);
    }

    #[test]
    fn replacement_characters_make_it_garbled() {
        let garbled = "\u{FFFD}".repeat(30);
        assert_eq!(classify(10_000, Some(&garbled)), Verdict::Garbled);

        // A couple of replacement characters in a long document is normal.
        let mostly_fine = format!("{}\u{FFFD}", "word ".repeat(500));
        assert_eq!(classify(10_000, Some(&mostly_fine)), Verdict::Text);
    }

    #[test]
    fn a_report_carries_its_density() {
        let report = report(Path::new("/x/a.PDF"), 1000, Some("abcd"), "pdf");
        assert_eq!(report.extension, "pdf");
        assert_eq!(report.chars, 4);
        assert_eq!(report.method, "pdf");
        assert!((report.density() - 0.004).abs() < 1e-9);
        assert_eq!(report.verdict, Verdict::Text);
    }

    #[test]
    fn the_summary_counts_each_verdict() {
        let reports = vec![
            report(Path::new("a.pdf"), 10, Some("text"), "pdf"),
            report(Path::new("b.pdf"), 500_000, None, "pdf"),
            report(Path::new("c.pdf"), 500_000, Some("   "), "pdf"),
        ];
        let summary = summarise(&reports);
        assert_eq!(summary.files, 3);
        assert_eq!(summary.text, 1);
        assert_eq!(summary.no_text, 1);
        assert_eq!(summary.sparse, 1);
        assert!((summary.text_share() - 1.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn the_report_lists_problems_before_everything() {
        let reports = vec![
            report(
                Path::new("/a/one.pdf"),
                10,
                Some("plenty of text here"),
                "pdf",
            ),
            report(Path::new("/a/two.pdf"), 500_000, None, "pdf"),
        ];
        let summary = summarise(&reports);
        let rendered = render(&reports, &summary);
        assert!(rendered.contains("Usable text: 50%"));
        assert!(rendered.contains("Needs a closer look"));
        assert!(rendered.contains("a/two.pdf"));
        assert!(rendered.contains("### Every file"));
    }
}
