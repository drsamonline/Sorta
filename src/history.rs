//! The append-only sort log, undo, and "why is this file here?".
//!
//! `sorta_history.jsonl` is the single source of truth for what Sorta did. It is
//! never rewritten, only appended to — including when an undo happens, which
//! appends a compensating entry rather than deleting the original. That is what
//! makes "undo yesterday's PDFs" auditable a month later.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Local, TimeZone};
use serde::{Deserialize, Serialize};

/// One line of `sorta_history.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// RFC 3339, UTC.
    pub ts: String,
    /// Original path.
    pub src: String,
    /// Destination path.
    pub dst: String,
    /// Rule that decided, e.g. `keyword:Invoice`.
    pub rule: String,
    pub confidence: f32,
    /// Evidence: `["keyword", "semantic:0.91", "calendar:Q4-Budget"]`.
    #[serde(default)]
    pub signals: Vec<String>,
    #[serde(default = "default_instance")]
    pub instance: String,
    #[serde(default)]
    pub hash: String,
    /// `committed | planned | skipped | duplicate | quarantined | undone | failed`.
    pub status: String,
}

fn default_instance() -> String {
    "default".to_string()
}

impl HistoryEntry {
    /// Builds a committed entry with the current timestamp.
    pub fn committed(
        src: &Path,
        dst: &Path,
        rule: &str,
        confidence: f32,
        signals: Vec<String>,
        instance: &str,
        hash: &str,
    ) -> Self {
        Self {
            ts: chrono::Utc::now().to_rfc3339(),
            src: src.display().to_string(),
            dst: dst.display().to_string(),
            rule: rule.to_string(),
            confidence,
            signals,
            instance: instance.to_string(),
            hash: hash.to_string(),
            status: "committed".to_string(),
        }
    }

    /// Parsed timestamp, when the line is well-formed.
    pub fn timestamp(&self) -> Option<DateTime<Local>> {
        DateTime::parse_from_rfc3339(&self.ts)
            .ok()
            .map(|dt| dt.with_timezone(&Local))
    }

    /// Plain-English explanation, templated. No model is consulted.
    pub fn explain(&self) -> String {
        let when = self
            .timestamp()
            .map(|t| t.format("%d %b %Y %H:%M").to_string())
            .unwrap_or_else(|| self.ts.clone());
        format!(
            "Sorted on {when} by rule '{}' at {:.0}% confidence. Evidence: {}.",
            self.rule,
            self.confidence * 100.0,
            if self.signals.is_empty() {
                "none recorded".to_string()
            } else {
                self.signals.join(", ")
            }
        )
    }
}

/// Handle to the log file.
#[derive(Debug, Clone)]
pub struct History {
    path: PathBuf,
}

impl History {
    pub fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends one entry as a single JSONL line.
    pub fn append(&self, entry: &HistoryEntry) -> anyhow::Result<()> {
        use std::io::Write;
        if let Some(parent) = self.path.parent() {
            crate::paths::ensure_dir(parent)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        let line = serde_json::to_string(entry)?;
        writeln!(file, "{line}")?;
        Ok(())
    }

    /// Reads every well-formed line. A corrupt line is skipped, not fatal —
    /// losing one log entry must not stop the sorter.
    pub fn read_all(&self) -> Vec<HistoryEntry> {
        let Ok(text) = std::fs::read_to_string(&self.path) else {
            return Vec::new();
        };
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str::<HistoryEntry>(l).ok())
            .collect()
    }

    /// Only entries that actually moved a file.
    pub fn committed(&self) -> Vec<HistoryEntry> {
        self.read_all()
            .into_iter()
            .filter(|e| e.status == "committed")
            .collect()
    }

    /// The last `n` entries, newest last.
    pub fn tail(&self, n: usize) -> Vec<HistoryEntry> {
        let all = self.read_all();
        let start = all.len().saturating_sub(n);
        all[start..].to_vec()
    }

    /// The most recent entry that produced `dst`.
    pub fn find_by_dst(&self, dst: &Path) -> Option<HistoryEntry> {
        let shown = dst.display().to_string();
        self.read_all()
            .into_iter()
            .rev()
            .find(|e| e.dst == shown && e.status == "committed")
    }

    /// Moves files back. Every successful undo appends a compensating entry, so
    /// the log still tells the whole story.
    pub fn undo(&self, entries: &[HistoryEntry]) -> Vec<UndoOutcome> {
        let mut outcomes = Vec::new();
        for entry in entries.iter().rev() {
            let src = Path::new(&entry.src);
            let dst = Path::new(&entry.dst);

            if !dst.exists() {
                outcomes.push(UndoOutcome {
                    src: entry.src.clone(),
                    dst: entry.dst.clone(),
                    ok: false,
                    note: "destination no longer exists; nothing to undo".to_string(),
                });
                continue;
            }
            if src.exists() {
                outcomes.push(UndoOutcome {
                    src: entry.src.clone(),
                    dst: entry.dst.clone(),
                    ok: false,
                    note: "original path is occupied; left both in place".to_string(),
                });
                continue;
            }
            if let Some(parent) = src.parent() {
                let _ = crate::paths::ensure_dir(parent);
            }

            match move_back(dst, src) {
                Ok(()) => {
                    let compensating = HistoryEntry {
                        ts: chrono::Utc::now().to_rfc3339(),
                        src: entry.dst.clone(),
                        dst: entry.src.clone(),
                        rule: format!("undo:{}", entry.rule),
                        confidence: entry.confidence,
                        signals: vec!["undo".to_string()],
                        instance: entry.instance.clone(),
                        hash: entry.hash.clone(),
                        status: "undone".to_string(),
                    };
                    let _ = self.append(&compensating);
                    outcomes.push(UndoOutcome {
                        src: entry.src.clone(),
                        dst: entry.dst.clone(),
                        ok: true,
                        note: "restored".to_string(),
                    });
                }
                Err(e) => outcomes.push(UndoOutcome {
                    src: entry.src.clone(),
                    dst: entry.dst.clone(),
                    ok: false,
                    note: format!("move failed: {e}"),
                }),
            }
        }
        outcomes
    }

    /// Entries matching a natural-language filter, newest last.
    pub fn matching(&self, filter: &UndoFilter) -> Vec<HistoryEntry> {
        let mut hits: Vec<HistoryEntry> = self
            .committed()
            .into_iter()
            .filter(|e| filter.matches(e))
            .collect();
        if let Some(limit) = filter.limit {
            if hits.len() > limit {
                let start = hits.len() - limit;
                hits = hits[start..].to_vec();
            }
        }
        hits
    }
}

fn move_back(from: &Path, to: &Path) -> std::io::Result<()> {
    match std::fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(_) => {
            std::fs::copy(from, to)?;
            std::fs::remove_file(from)?;
            Ok(())
        }
    }
}

/// Result of one undo attempt.
#[derive(Debug, Clone)]
pub struct UndoOutcome {
    pub src: String,
    pub dst: String,
    pub ok: bool,
    pub note: String,
}

/// Parses `YYYY-MM-DD` as local midnight.
pub fn parse_date(text: &str) -> Option<DateTime<Local>> {
    let date = chrono::NaiveDate::parse_from_str(text.trim(), "%Y-%m-%d").ok()?;
    let midnight = date.and_hms_opt(0, 0, 0)?;
    Local.from_local_datetime(&midnight).single()
}

/// A filter parsed from a sentence like "undo yesterday's PDFs".
#[derive(Debug, Clone, Default)]
pub struct UndoFilter {
    /// Only entries at or after this instant.
    pub since: Option<DateTime<Local>>,
    /// Only entries at or before this instant.
    pub until: Option<DateTime<Local>>,
    /// Extension filter, without the dot.
    pub extension: Option<String>,
    /// Only entries decided by this rule, e.g. `keyword:invoice`.
    pub rule: Option<String>,
    /// Cap on how many entries to undo.
    pub limit: Option<usize>,
}

impl UndoFilter {
    fn matches(&self, entry: &HistoryEntry) -> bool {
        if let Some(since) = self.since {
            match entry.timestamp() {
                Some(ts) if ts >= since => {}
                _ => return false,
            }
        }
        if let Some(until) = self.until {
            match entry.timestamp() {
                Some(ts) if ts <= until => {}
                _ => return false,
            }
        }
        if let Some(rule) = &self.rule {
            if !entry.rule.eq_ignore_ascii_case(rule) {
                return false;
            }
        }
        if let Some(ext) = &self.extension {
            let path = Path::new(&entry.dst);
            let actual = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if actual != ext.to_ascii_lowercase() {
                return false;
            }
        }
        true
    }
}

/// Recognised file extensions for meaning parsing.
const KNOWN_EXTENSIONS: [&str; 14] = [
    "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "png", "jpg", "jpeg", "txt", "csv", "zip",
    "md",
];

/// Parses "undo yesterday's PDFs" into a filter.
///
/// Deliberately literal: it understands day words, a file extension, and a count.
/// Anything it does not understand is ignored rather than guessed at, and the CLI
/// prints the filter it applied so the user can see what was understood.
pub fn parse_meaning(query: &str) -> UndoFilter {
    let q = query.to_lowercase();
    let now = Local::now();
    let today = now.date_naive().and_hms_opt(0, 0, 0).unwrap();
    let today = Local.from_local_datetime(&today).single().unwrap_or(now);

    let mut filter = UndoFilter::default();

    if q.contains("yesterday") {
        filter.since = Some(today - Duration::days(1));
        filter.until = Some(today - Duration::seconds(1));
    } else if q.contains("today") {
        filter.since = Some(today);
    } else if q.contains("this week") || q.contains("last week") || q.contains("past week") {
        filter.since = Some(today - Duration::days(7));
    } else if q.contains("this month") || q.contains("last month") {
        filter.since = Some(today - Duration::days(31));
    }

    for ext in KNOWN_EXTENSIONS {
        if q.contains(ext) {
            filter.extension = Some(ext.to_string());
            break;
        }
    }

    // "undo the last 5" — the first bare number wins.
    let digits: String = q
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if let Ok(n) = digits.parse::<usize>() {
        if n > 0 {
            filter.limit = Some(n);
        }
    }

    filter
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(src: &str, dst: &str, status: &str) -> HistoryEntry {
        HistoryEntry {
            ts: chrono::Utc::now().to_rfc3339(),
            src: src.to_string(),
            dst: dst.to_string(),
            rule: "keyword:Invoice".to_string(),
            confidence: 0.94,
            signals: vec!["keyword".to_string()],
            instance: "default".to_string(),
            hash: "b3:abc".to_string(),
            status: status.to_string(),
        }
    }

    #[test]
    fn appends_and_reads() {
        let dir = tempfile::tempdir().unwrap();
        let history = History::new(&dir.path().join("sorta_history.jsonl"));
        history
            .append(&entry("/a/x.pdf", "/b/x.pdf", "committed"))
            .unwrap();
        history
            .append(&entry("/a/y.pdf", "/b/y.pdf", "committed"))
            .unwrap();

        assert_eq!(history.read_all().len(), 2);
        assert_eq!(history.committed().len(), 2);
        assert_eq!(history.tail(1).len(), 1);
    }

    #[test]
    fn corrupt_lines_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sorta_history.jsonl");
        std::fs::write(&path, "not json\n").unwrap();
        let history = History::new(&path);
        history
            .append(&entry("/a/x.pdf", "/b/x.pdf", "committed"))
            .unwrap();
        assert_eq!(history.read_all().len(), 1);
    }

    #[test]
    fn finds_the_reason_for_a_destination() {
        let dir = tempfile::tempdir().unwrap();
        let history = History::new(&dir.path().join("sorta_history.jsonl"));
        history
            .append(&entry(
                "/a/inv.pdf",
                "/b/2026/PDF-Invoices/inv.pdf",
                "committed",
            ))
            .unwrap();
        let hit = history
            .find_by_dst(Path::new("/b/2026/PDF-Invoices/inv.pdf"))
            .expect("entry should be found");
        assert_eq!(hit.rule, "keyword:Invoice");
        assert!(hit.explain().contains("keyword:Invoice"));
    }

    #[test]
    fn undo_restores_and_logs() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("inbox/inv.pdf");
        let dst = dir.path().join("sorted/2026/PDF-Invoices/inv.pdf");
        std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        std::fs::write(&dst, b"data").unwrap();

        let history = History::new(&dir.path().join("sorta_history.jsonl"));
        let e = entry(
            &src.display().to_string(),
            &dst.display().to_string(),
            "committed",
        );
        history.append(&e).unwrap();

        let outcomes = history.undo(&[e]);
        assert!(outcomes[0].ok);
        assert!(src.exists());
        assert!(!dst.exists());

        let all = history.read_all();
        assert_eq!(all.len(), 2);
        assert_eq!(all[1].status, "undone");
    }

    #[test]
    fn meaning_parser_understands_yesterday_pdfs() {
        let filter = parse_meaning("undo yesterday's PDFs");
        assert_eq!(filter.extension.as_deref(), Some("pdf"));
        assert!(filter.since.is_some());
        assert!(filter.until.is_some());
    }

    #[test]
    fn meaning_parser_reads_a_count() {
        let filter = parse_meaning("undo the last 5 files");
        assert_eq!(filter.limit, Some(5));
        assert!(filter.since.is_none());
    }

    #[test]
    fn date_parsing() {
        let parsed = parse_date("2026-11-18").expect("ISO date should parse");
        assert_eq!(parsed.format("%Y-%m-%d").to_string(), "2026-11-18");
        assert!(parse_date("not a date").is_none());
    }

    #[test]
    fn rule_filter_matches_exactly() {
        let filter = UndoFilter {
            rule: Some("keyword:invoice".to_string()),
            ..Default::default()
        };
        assert!(filter.matches(&entry("/a/x.pdf", "/b/x.pdf", "committed")));

        let other = UndoFilter {
            rule: Some("keyword:contract".to_string()),
            ..Default::default()
        };
        assert!(!other.matches(&entry("/a/x.pdf", "/b/x.pdf", "committed")));
    }

    #[test]
    fn extension_filter_is_case_insensitive() {
        let filter = UndoFilter {
            extension: Some("PDF".to_string()),
            ..Default::default()
        };
        assert!(filter.matches(&entry("/a/x.pdf", "/b/X.PDF", "committed")));
        assert!(!filter.matches(&entry("/a/x.docx", "/b/X.docx", "committed")));
    }
}
