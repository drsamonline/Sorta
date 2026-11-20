//! Erasure: `sorta forget`.
//!
//! Sorta keeps records about files — paths, extracted text, embeddings, hashes,
//! quarantine entries, an optional audit chain. `PRIVACY.md` describes them; this
//! module is the command that removes them, because "here is a table, delete the
//! files yourself" is not an erasure procedure.
//!
//! Two scopes, and one deliberate exception:
//!
//! * **one file** — drops its index entry, its vector, its duplicate-registry entry
//!   and its quarantine entry. The history log is append-only, so by default the
//!   log keeps its lines; `--history` rewrites it and says how many lines went.
//! * **everything** — removes every file Sorta writes under `~/.sorta`, listed
//!   explicitly rather than by deleting the directory.
//!
//! The exception is the **audit chain**. Rewriting it would make every later link
//! unverifiable, which is the one property it has. So erasure leaves it alone and
//! tells the user to delete `audit.jsonl` themselves if they want that too.
//!
//! Nothing here touches the user's documents. Erasure is about Sorta's records.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::dedupe::Deduper;
use crate::quarantine::Quarantine;
use crate::search::Index;
use crate::vectors::VectorStore;

/// What one erasure removed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErasureReport {
    pub path: String,
    pub index_entry: bool,
    pub vector: bool,
    pub dedup_entries: usize,
    pub quarantine_entries: usize,
    pub history_lines: usize,
    /// True when the audit chain still mentions the path.
    pub audit_retained: bool,
}

impl ErasureReport {
    /// True when nothing at all was found.
    pub fn is_empty(&self) -> bool {
        !self.index_entry
            && !self.vector
            && self.dedup_entries == 0
            && self.quarantine_entries == 0
            && self.history_lines == 0
    }
}

/// The files Sorta writes under its state directory.
pub const STATE_FILES: [&str; 9] = [
    "sorta_history.jsonl",
    "search_index.json",
    "vectors.json",
    "dedup_registry.json",
    "instances.json",
    "audit.jsonl",
    ".sorta_scan_state.json",
    "last_scan",
    "paused",
];

/// Forgets one path across every derived store.
pub fn forget_path(
    state_dir: &Path,
    quarantine_root: &Path,
    target: &str,
    include_history: bool,
) -> anyhow::Result<ErasureReport> {
    let mut report = ErasureReport {
        path: target.to_string(),
        ..ErasureReport::default()
    };

    // Search index.
    let index_path = state_dir.join("search_index.json");
    let mut index = Index::load(&index_path);
    if index.remove(target) {
        index.save(&index_path)?;
        report.index_entry = true;
    }

    // Embeddings.
    let vectors_path = state_dir.join("vectors.json");
    let mut vectors = VectorStore::load(&vectors_path);
    if vectors.remove(target) {
        vectors.save(&vectors_path)?;
        report.vector = true;
    }

    // Duplicate registry.
    let dedup_path = state_dir.join("dedup_registry.json");
    let mut deduper = Deduper::load(&dedup_path)?;
    let before = deduper.known_count();
    deduper.forget_path(Path::new(target));
    report.dedup_entries = before.saturating_sub(deduper.known_count());
    if report.dedup_entries > 0 {
        deduper.save()?;
    }

    // Quarantine.
    let queue = Quarantine::new(quarantine_root);
    if queue.resolve(target)? {
        report.quarantine_entries = 1;
    }

    // History, only when asked.
    if include_history {
        report.history_lines = rewrite_history(&state_dir.join("sorta_history.jsonl"), target)?;
    }

    report.audit_retained = audit_mentions(&state_dir.join("audit.jsonl"), target);

    Ok(report)
}

/// Rewrites the history log without the lines that mention `target`.
///
/// Returns how many lines were dropped. The log is append-only by design, so this
/// only ever runs when the user asked for it explicitly, and it writes through a
/// temporary file so a crash cannot truncate the log.
fn rewrite_history(path: &Path, target: &str) -> anyhow::Result<usize> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(0);
    };

    let mut kept = String::with_capacity(text.len());
    let mut dropped = 0usize;

    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let mentions = serde_json::from_str::<serde_json::Value>(line)
            .map(|value| {
                ["src", "dst"]
                    .iter()
                    .any(|field| value.get(*field).and_then(|v| v.as_str()) == Some(target))
            })
            .unwrap_or_else(|_| line.contains(target));

        if mentions {
            dropped += 1;
        } else {
            kept.push_str(line);
            kept.push('\n');
        }
    }

    if dropped > 0 {
        let tmp = path.with_extension("jsonl.tmp");
        std::fs::write(&tmp, kept)?;
        std::fs::rename(&tmp, path)?;
    }
    Ok(dropped)
}

fn audit_mentions(path: &Path, target: &str) -> bool {
    std::fs::read_to_string(path)
        .map(|text| text.contains(target))
        .unwrap_or(false)
}

/// Deletes every record Sorta keeps, and reports what went.
///
/// The files are listed, not globbed: a stray directory in the state folder is not
/// ours to remove.
pub fn forget_everything(state_dir: &Path, quarantine_root: &Path) -> Vec<(PathBuf, u64)> {
    let mut removed = Vec::new();

    for name in STATE_FILES {
        removed.extend(remove_file(&state_dir.join(name)));
    }

    // Lock files are named per instance, so they are globbed by prefix.
    if let Ok(entries) = std::fs::read_dir(state_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(".lock") || name.starts_with("digest-") {
                removed.extend(remove_file(&entry.path()));
            }
        }
    }

    removed.extend(remove_file(&quarantine_root.join("quarantine.toml")));
    removed
}

fn remove_file(path: &Path) -> Option<(PathBuf, u64)> {
    let size = std::fs::metadata(path).ok()?.len();
    std::fs::remove_file(path).ok()?;
    Some((path.to_path_buf(), size))
}

/// Human-readable size, matching the rest of the CLI.
pub fn human_bytes(bytes: u64) -> String {
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
    use crate::search::SearchDoc;

    fn seed(state: &Path, quarantine_root: &Path, target: &str) {
        let index_path = state.join("search_index.json");
        let mut index = Index::load(&index_path);
        index.upsert(SearchDoc {
            path: target.to_string(),
            name: "invoice.pdf".to_string(),
            text: "invoice body".to_string(),
            size: 10,
            mtime: 1,
            kind: "Documents".to_string(),
            hash: "b3:aa".to_string(),
        });
        index.save(&index_path).unwrap();

        let vectors_path = state.join("vectors.json");
        let mut vectors = VectorStore::load(&vectors_path);
        vectors.upsert(crate::vectors::VectorEntry {
            path: target.to_string(),
            hash: "b3:aa".to_string(),
            dim: 2,
            values: vec![0.5, 0.5],
            at: 1,
        });
        vectors.save(&vectors_path).unwrap();

        let mut deduper = Deduper::load(&state.join("dedup_registry.json")).unwrap();
        deduper.record(
            &crate::hash::ContentHash("b3:aa".to_string()),
            Path::new(target),
            10,
        );
        deduper.save().unwrap();

        let queue = Quarantine::new(quarantine_root);
        queue
            .add(crate::quarantine::QuarantineItem {
                path: target.to_string(),
                detected_type: "application/pdf".to_string(),
                best_guess: "invoice".to_string(),
                confidence: 0.6,
                reason: "low_confidence".to_string(),
                created_at: "2026-11-20T00:00:00+00:00".to_string(),
                state: Default::default(),
                suggested_dest: "2026".to_string(),
            })
            .unwrap();
    }

    #[test]
    fn forgetting_a_path_clears_every_derived_store() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        let quarantine_root = dir.path().join("Quarantine");
        std::fs::create_dir_all(&state).unwrap();
        let target = "/sorted/invoice.pdf";
        seed(&state, &quarantine_root, target);

        let report = forget_path(&state, &quarantine_root, target, false).unwrap();

        assert!(report.index_entry);
        assert!(report.vector);
        assert_eq!(report.dedup_entries, 1);
        assert_eq!(report.quarantine_entries, 1);
        assert_eq!(report.history_lines, 0, "history untouched without the flag");
        assert!(!report.is_empty());

        assert!(Index::load(&state.join("search_index.json")).is_empty());
        assert!(VectorStore::load(&state.join("vectors.json")).is_empty());
        assert!(Quarantine::new(&quarantine_root).pending().is_empty());
    }

    #[test]
    fn history_is_rewritten_only_when_asked() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        let history = state.join("sorta_history.jsonl");
        std::fs::write(
            &history,
            concat!(
                r#"{"ts":"t","src":"/in/a.pdf","dst":"/sorted/a.pdf","rule":"keyword:invoice","confidence":0.9,"signals":[],"instance":"default","hash":"b3:a","status":"committed"}"#,
                "\n",
                r#"{"ts":"t","src":"/in/b.pdf","dst":"/sorted/b.pdf","rule":"format:text","confidence":0.9,"signals":[],"instance":"default","hash":"b3:b","status":"committed"}"#,
                "\n"
            ),
        )
        .unwrap();

        let report = forget_path(&state, &dir.path().join("Q"), "/sorted/a.pdf", true).unwrap();
        assert_eq!(report.history_lines, 1);

        let left = std::fs::read_to_string(&history).unwrap();
        assert!(!left.contains("a.pdf"));
        assert!(left.contains("b.pdf"), "unrelated lines must survive");
    }

    #[test]
    fn an_unknown_path_is_reported_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let report = forget_path(dir.path(), &dir.path().join("Q"), "/nope.pdf", false).unwrap();
        assert!(report.is_empty());
    }

    #[test]
    fn forget_everything_removes_the_state_files_only() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        let quarantine_root = dir.path().join("Quarantine");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&quarantine_root).unwrap();
        seed(&state, &quarantine_root, "/sorted/a.pdf");
        std::fs::write(state.join("paused"), b"x").unwrap();
        std::fs::write(state.join("digest-2026-11-20.md"), b"x").unwrap();
        std::fs::write(state.join("something-else.txt"), b"x").unwrap();

        let removed = forget_everything(&state, &quarantine_root);
        assert!(!removed.is_empty());

        assert!(!state.join("sorta_history.jsonl").exists());
        assert!(!state.join("paused").exists());
        assert!(!state.join("digest-2026-11-20.md").exists());
        assert!(!quarantine_root.join("quarantine.toml").exists());
        assert!(
            state.join("something-else.txt").exists(),
            "files Sorta does not own must survive"
        );
    }

    #[test]
    fn the_audit_chain_is_never_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        let audit = state.join("audit.jsonl");
        let log = crate::audit::Audit::new(&audit);
        log.append("/in/a.pdf", "/sorted/a.pdf", "keyword:invoice", "b3:a")
            .unwrap();

        let report = forget_path(&state, &dir.path().join("Q"), "/sorted/a.pdf", true).unwrap();
        assert!(report.audit_retained, "the report must admit the chain still has it");
        assert!(log.verify().ok, "and the chain must still verify");
    }
}
