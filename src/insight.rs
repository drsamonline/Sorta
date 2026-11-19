//! Insights: health, rule analytics, and what Sorta saved you.
//!
//! Everything here is derived from two files that already exist — the append-only
//! history and the duplicate folder. There is no separate statistics database, so
//! the numbers cannot drift from what actually happened.
//!
//! The scoring is deliberately simple and documented, because a health score
//! nobody can explain is worse than no score:
//!
//! | Signal | Penalty |
//! |---|---|
//! | each file waiting in quarantine | −2, capped at −20 |
//! | each 10 files failed or deferred | −5, capped at −25 |
//! | duplicates above 20% of sorted files | −10 |
//! | each 5 filed below 80% confidence | −1, capped at −15 |
//!
//! The caps total 70, so the score has a floor of 30 rather than 0: a folder with
//! problems should still be able to recover, and a 0 would say "catastrophic"
//! about something that is usually just an unreviewed backlog.

use std::path::Path;

use chrono::{DateTime, Duration, Local};

use crate::history::HistoryEntry;

/// Days of history the score looks at.
pub const WINDOW_DAYS: i64 = 30;

/// Seconds of human time one sorted file is assumed to save.
pub const SECONDS_SAVED_PER_FILE: u64 = 6;

/// A 0–100 score with the factors that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Health {
    pub score: u8,
    /// (label, penalty, explanation)
    pub factors: Vec<(String, i32, String)>,
}

/// How one rule has been behaving.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleStat {
    pub rule: String,
    /// Files this rule filed.
    pub committed: usize,
    /// Times the user undid a decision by this rule.
    pub overridden: usize,
}

/// What the duplicate handling has saved.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Savings {
    pub duplicates_files: usize,
    pub duplicates_bytes: u64,
    pub files_sorted: usize,
    pub seconds_saved: u64,
}

impl Savings {
    /// Seconds saved, rendered as a compact human string.
    pub fn human_time(&self) -> String {
        let minutes = self.seconds_saved / 60;
        if minutes < 60 {
            format!("{minutes} min")
        } else {
            format!("{} h {} min", minutes / 60, minutes % 60)
        }
    }

    /// Bytes held aside, rendered like the rest of the CLI.
    pub fn human_bytes(&self) -> String {
        human_bytes(self.duplicates_bytes)
    }
}

/// Computes the health score from history and the quarantine backlog.
pub fn health(history: &[HistoryEntry], quarantine_pending: usize, now: DateTime<Local>) -> Health {
    let cutoff = now - Duration::days(WINDOW_DAYS);
    let recent: Vec<&HistoryEntry> = history
        .iter()
        .filter(|entry| entry.timestamp().map(|ts| ts >= cutoff).unwrap_or(false))
        .collect();

    let mut factors: Vec<(String, i32, String)> = Vec::new();

    if quarantine_pending > 0 {
        let penalty = ((quarantine_pending as i32) * 2).min(20);
        factors.push((
            "quarantine backlog".to_string(),
            penalty,
            format!("{quarantine_pending} file(s) waiting for review"),
        ));
    }

    let trouble = recent
        .iter()
        .filter(|e| e.status == "failed" || e.status == "deferred")
        .count();
    if trouble > 0 {
        let penalty = ((((trouble + 9) / 10) as i32) * 5).min(25);
        factors.push((
            "failures".to_string(),
            penalty,
            format!("{trouble} file(s) failed or deferred in {WINDOW_DAYS} days"),
        ));
    }

    let filed = recent.iter().filter(|e| e.status == "committed").count();
    let duplicates = recent.iter().filter(|e| e.status == "duplicate").count();
    if filed > 0 && duplicates * 5 > filed {
        factors.push((
            "duplicates".to_string(),
            10,
            format!("{duplicates} duplicate(s) against {filed} filed"),
        ));
    }

    let low_confidence = recent
        .iter()
        .filter(|e| e.status == "committed" && e.confidence < 0.8)
        .count();
    if low_confidence > 0 {
        let penalty = ((low_confidence / 5) as i32).clamp(1, 15);
        factors.push((
            "weak matches".to_string(),
            penalty,
            format!("{low_confidence} file(s) filed below 80% confidence"),
        ));
    }

    let total_penalty: i32 = factors.iter().map(|(_, p, _)| *p).sum();
    let score = (100 - total_penalty).clamp(0, 100) as u8;
    Health { score, factors }
}

/// Per-rule behaviour, best-performing first.
pub fn rule_stats(history: &[HistoryEntry]) -> Vec<RuleStat> {
    let mut stats: Vec<RuleStat> = Vec::new();

    for entry in history {
        if entry.status == "committed" {
            bump(&mut stats, &entry.rule, false);
        } else if entry.status == "undone" {
            // An undo entry carries `undo:<rule>`; attribute it to the original.
            let rule = entry
                .rule
                .strip_prefix("undo:")
                .unwrap_or(&entry.rule)
                .to_string();
            bump(&mut stats, &rule, true);
        }
    }

    stats.sort_by(|a, b| {
        b.committed
            .cmp(&a.committed)
            .then(a.overridden.cmp(&b.overridden))
            .then(a.rule.cmp(&b.rule))
    });
    stats
}

fn bump(stats: &mut Vec<RuleStat>, rule: &str, overridden: bool) {
    match stats.iter_mut().find(|s| s.rule == rule) {
        Some(stat) => {
            if overridden {
                stat.overridden += 1;
            } else {
                stat.committed += 1;
            }
        }
        None => stats.push(RuleStat {
            rule: rule.to_string(),
            committed: usize::from(!overridden),
            overridden: usize::from(overridden),
        }),
    }
}

/// Counts what the duplicate folder holds and how much time sorting saved.
pub fn savings(duplicates_root: &Path, history: &[HistoryEntry]) -> Savings {
    let (files, bytes) = folder_totals(duplicates_root);
    let files_sorted = history.iter().filter(|e| e.status == "committed").count();
    Savings {
        duplicates_files: files,
        duplicates_bytes: bytes,
        files_sorted,
        seconds_saved: (files_sorted as u64) * SECONDS_SAVED_PER_FILE,
    }
}

fn folder_totals(dir: &Path) -> (usize, u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (0, 0);
    };
    let mut files = 0usize;
    let mut bytes = 0u64;
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_dir() {
            let (sub_files, sub_bytes) = folder_totals(&entry.path());
            files += sub_files;
            bytes += sub_bytes;
        } else {
            files += 1;
            bytes += meta.len();
        }
    }
    (files, bytes)
}

/// Renders the whole insights page as Markdown.
pub fn render(health: &Health, rules: &[RuleStat], savings: &Savings) -> String {
    let mut md = String::new();
    md.push_str("# Sorta insights\n\n");
    md.push_str(&format!("## Folder health: {}/100\n\n", health.score));

    if health.factors.is_empty() {
        md.push_str("No deductions: nothing is waiting, failing or being guessed at.\n\n");
    } else {
        md.push_str("| Signal | Penalty | Why |\n|---|---|---|\n");
        for (label, penalty, why) in &health.factors {
            md.push_str(&format!("| {label} | -{penalty} | {why} |\n"));
        }
        md.push('\n');
    }

    if !rules.is_empty() {
        md.push_str("## Rule analytics\n\n| Rule | Filed | Undone |\n|---|---|---|\n");
        for stat in rules.iter().take(10) {
            md.push_str(&format!(
                "| {} | {} | {} |\n",
                stat.rule, stat.committed, stat.overridden
            ));
        }
        md.push('\n');
    }

    md.push_str("## Savings\n\n");
    md.push_str(&format!(
        "- Files sorted: {}\n- Duplicates held aside: {} ({} )\n- Time saved: {} (at {} s per file)\n",
        savings.files_sorted,
        savings.duplicates_files,
        savings.human_bytes(),
        savings.human_time(),
        SECONDS_SAVED_PER_FILE
    ));
    md
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(rule: &str, status: &str, confidence: f32) -> HistoryEntry {
        HistoryEntry {
            ts: chrono::Utc::now().to_rfc3339(),
            src: "/inbox/x.pdf".to_string(),
            dst: "/data/x.pdf".to_string(),
            rule: rule.to_string(),
            confidence,
            signals: Vec::new(),
            instance: "default".to_string(),
            hash: "b3:aa".to_string(),
            status: status.to_string(),
        }
    }

    #[test]
    fn a_clean_run_scores_full_marks() {
        let history = vec![entry("keyword:invoice", "committed", 0.95)];
        let health = health(&history, 0, Local::now());
        assert_eq!(health.score, 100);
        assert!(health.factors.is_empty());
    }

    #[test]
    fn backlog_and_failures_cost_points() {
        let history = vec![
            entry("keyword:invoice", "failed", 0.0),
            entry("keyword:invoice", "deferred", 0.0),
        ];
        let health = health(&history, 3, Local::now());
        assert!(health.score < 100);
        assert!(health
            .factors
            .iter()
            .any(|(l, _, _)| l == "quarantine backlog"));
        assert!(health.factors.iter().any(|(l, _, _)| l == "failures"));
    }

    #[test]
    fn penalties_are_capped() {
        let history: Vec<HistoryEntry> = (0..500)
            .map(|_| entry("fallback:unknown", "failed", 0.1))
            .collect();
        let health = health(&history, 100, Local::now());
        // Quarantine is capped at -20 and failures at -25, so 55 is the floor here.
        assert_eq!(health.score, 55);
        assert!(health.score >= 30, "caps keep the score above 30");
    }

    #[test]
    fn old_entries_are_outside_the_window() {
        let mut stale = entry("keyword:invoice", "failed", 0.0);
        stale.ts = (chrono::Utc::now() - Duration::days(WINDOW_DAYS + 5)).to_rfc3339();
        let health = health(&[stale], 0, Local::now());
        assert_eq!(health.score, 100);
    }

    #[test]
    fn rule_stats_count_commits_and_overrides() {
        let history = vec![
            entry("keyword:invoice", "committed", 0.9),
            entry("keyword:invoice", "committed", 0.9),
            entry("undo:keyword:invoice", "undone", 0.9),
            entry("format:image", "committed", 0.9),
        ];
        let stats = rule_stats(&history);
        let invoice = stats
            .iter()
            .find(|s| s.rule == "keyword:invoice")
            .expect("invoice rule should appear");
        assert_eq!(invoice.committed, 2);
        assert_eq!(invoice.overridden, 1);
        assert_eq!(stats[0].rule, "keyword:invoice");
    }

    #[test]
    fn savings_walk_the_duplicates_folder() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.pdf"), b"0123456789").unwrap();
        std::fs::create_dir_all(dir.path().join("nested")).unwrap();
        std::fs::write(dir.path().join("nested/b.pdf"), b"01234").unwrap();

        let history = vec![
            entry("keyword:invoice", "committed", 0.9),
            entry("keyword:invoice", "committed", 0.9),
            entry("keyword:invoice", "duplicate", 1.0),
        ];
        let savings = savings(dir.path(), &history);
        assert_eq!(savings.duplicates_files, 2);
        assert_eq!(savings.duplicates_bytes, 15);
        assert_eq!(savings.files_sorted, 2);
        assert_eq!(savings.seconds_saved, 12);
        assert_eq!(savings.human_time(), "0 min");
    }

    #[test]
    fn renders_markdown() {
        let history = vec![entry("keyword:invoice", "committed", 0.9)];
        let health = health(&history, 1, Local::now());
        let md = render(
            &health,
            &rule_stats(&history),
            &savings(Path::new("/nope"), &history),
        );
        assert!(md.contains("# Sorta insights"));
        assert!(md.contains("Folder health:"));
        assert!(md.contains("keyword:invoice"));
        assert!(md.contains("Time saved"));
    }
}
