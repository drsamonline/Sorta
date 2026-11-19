//! Interop: getting the history out of Sorta.
//!
//! The log is already a file on disk, but "already a file" is not the same as
//! "usable elsewhere". CSV opens in a spreadsheet; JSON feeds a script. Both
//! exports are lossless — every field in the history entry appears in both.

use std::path::{Path, PathBuf};

use crate::history::HistoryEntry;

/// Columns of the CSV export, in order.
pub const CSV_COLUMNS: [&str; 9] = [
    "ts",
    "status",
    "src",
    "dst",
    "rule",
    "confidence",
    "signals",
    "instance",
    "hash",
];

/// Renders history as CSV, quoting anything that needs it.
pub fn history_csv(entries: &[HistoryEntry]) -> String {
    let mut out = String::new();
    out.push_str(&CSV_COLUMNS.join(","));
    out.push('\n');

    for entry in entries {
        let fields = [
            entry.ts.clone(),
            entry.status.clone(),
            entry.src.clone(),
            entry.dst.clone(),
            entry.rule.clone(),
            format!("{:.4}", entry.confidence),
            entry.signals.join("; "),
            entry.instance.clone(),
            entry.hash.clone(),
        ];
        let row: Vec<String> = fields.iter().map(|f| csv_field(f)).collect();
        out.push_str(&row.join(","));
        out.push('\n');
    }
    out
}

/// Renders history as a JSON array.
pub fn history_json(entries: &[HistoryEntry]) -> anyhow::Result<String> {
    Ok(serde_json::to_string_pretty(entries)?)
}

/// Writes an export, or prints it when `out` is `None`.
///
/// Returns the path written, if any.
pub fn write(text: &str, out: Option<&Path>) -> anyhow::Result<Option<PathBuf>> {
    match out {
        Some(path) => {
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    crate::paths::ensure_dir(parent)?;
                }
            }
            std::fs::write(path, text)?;
            Ok(Some(path.to_path_buf()))
        }
        None => {
            print!("{text}");
            Ok(None)
        }
    }
}

/// Escapes one CSV field per RFC 4180.
fn csv_field(value: &str) -> String {
    let needs_quotes =
        value.contains(',') || value.contains('"') || value.contains('\n') || value.contains('\r');
    if needs_quotes {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(src: &str, dst: &str, rule: &str) -> HistoryEntry {
        HistoryEntry {
            ts: "2026-11-19T14:32:01+00:00".to_string(),
            src: src.to_string(),
            dst: dst.to_string(),
            rule: rule.to_string(),
            confidence: 0.94,
            signals: vec!["keyword".to_string(), "name:x.pdf".to_string()],
            instance: "default".to_string(),
            hash: "b3:abc".to_string(),
            status: "committed".to_string(),
        }
    }

    #[test]
    fn csv_has_a_header_and_one_row_per_entry() {
        let csv = history_csv(&[entry("/a/x.pdf", "/b/x.pdf", "keyword:invoice")]);
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("ts,status,src,dst"));
        assert!(lines[1].contains("keyword:invoice"));
        assert!(lines[1].contains("0.9400"));
        assert!(lines[1].contains("keyword; name:x.pdf"));
    }

    #[test]
    fn csv_quotes_commas_and_quotes() {
        let csv = history_csv(&[entry(
            "/a/odd, name.pdf",
            "/b/\"quoted\".pdf",
            "format:text",
        )]);
        assert!(csv.contains("\"/a/odd, name.pdf\""));
        assert!(csv.contains("\"/b/\"\"quoted\"\".pdf\""));
    }

    #[test]
    fn json_round_trips() {
        let entries = vec![entry("/a/x.pdf", "/b/x.pdf", "keyword:invoice")];
        let json = history_json(&entries).unwrap();
        let back: Vec<HistoryEntry> = serde_json::from_str(&json).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].rule, "keyword:invoice");
    }

    #[test]
    fn writes_to_a_file_when_asked() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out/history.csv");
        let written = write("a,b\n", Some(&path)).unwrap();
        assert_eq!(written, Some(path.clone()));
        assert!(path.exists());
    }

    #[test]
    fn empty_history_still_has_a_header() {
        assert_eq!(history_csv(&[]).lines().count(), 1);
    }
}
