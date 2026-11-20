//! Tamper-evident audit chain.
//!
//! The history log answers "what did Sorta do". This answers "has anyone edited
//! the record". Each line carries the chain hash of the line before it, so
//! removing, reordering or altering an entry breaks every link after it.
//!
//! It is a hash chain, not a signature: it detects editing by someone who does not
//! recompute the chain, and cannot stop someone who does. That is the honest
//! guarantee, and it is why the log is append-only and the file is separate from
//! `sorta_history.jsonl`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Value used as the `prev` of the first entry.
pub const GENESIS: &str = "genesis";

/// One link in the chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub ts: String,
    pub src: String,
    pub dst: String,
    pub rule: String,
    /// Content digest of the file that moved.
    pub hash: String,
    /// Chain hash of the previous entry.
    pub prev: String,
    /// Chain hash of this entry.
    pub chain: String,
}

/// Result of walking the chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditReport {
    pub entries: usize,
    pub ok: bool,
    /// 1-based index of the first broken entry.
    pub first_break: Option<usize>,
    pub detail: String,
}

/// The audit log file.
#[derive(Debug, Clone)]
pub struct Audit {
    path: PathBuf,
}

impl Audit {
    pub fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Parses every well-formed line.
    pub fn entries(&self) -> Vec<AuditEntry> {
        let Ok(text) = std::fs::read_to_string(&self.path) else {
            return Vec::new();
        };
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .filter_map(|line| serde_json::from_str::<AuditEntry>(line).ok())
            .collect()
    }

    /// The chain hash of the last entry, or [`GENESIS`].
    pub fn last_chain(&self) -> String {
        self.entries()
            .last()
            .map(|entry| entry.chain.clone())
            .unwrap_or_else(|| GENESIS.to_string())
    }

    /// Appends one entry, linking it to the current head.
    pub fn append(&self, src: &str, dst: &str, rule: &str, hash: &str) -> anyhow::Result<()> {
        use std::io::Write;

        if let Some(parent) = self.path.parent() {
            crate::paths::ensure_dir(parent)?;
        }

        let ts = chrono::Utc::now().to_rfc3339();
        let prev = self.last_chain();
        let chain = chain_hash(&prev, &ts, src, dst, rule, hash);

        let entry = AuditEntry {
            ts,
            src: src.to_string(),
            dst: dst.to_string(),
            rule: rule.to_string(),
            hash: hash.to_string(),
            prev,
            chain,
        };

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(file, "{}", serde_json::to_string(&entry)?)?;
        Ok(())
    }

    /// Walks the chain and reports the first break.
    pub fn verify(&self) -> AuditReport {
        let entries = self.entries();

        if entries.is_empty() {
            return AuditReport {
                entries: 0,
                ok: true,
                first_break: None,
                detail: "no audit entries".to_string(),
            };
        }

        let mut expected_prev = GENESIS.to_string();
        for (index, entry) in entries.iter().enumerate() {
            if entry.prev != expected_prev {
                return AuditReport {
                    entries: entries.len(),
                    ok: false,
                    first_break: Some(index + 1),
                    detail: format!(
                        "entry {} points at {} but the chain expected {}",
                        index + 1,
                        entry.prev,
                        expected_prev
                    ),
                };
            }
            let recomputed = chain_hash(
                &entry.prev,
                &entry.ts,
                &entry.src,
                &entry.dst,
                &entry.rule,
                &entry.hash,
            );
            if recomputed != entry.chain {
                return AuditReport {
                    entries: entries.len(),
                    ok: false,
                    first_break: Some(index + 1),
                    detail: format!("entry {} was modified after it was written", index + 1),
                };
            }
            expected_prev = entry.chain.clone();
        }

        AuditReport {
            entries: entries.len(),
            ok: true,
            first_break: None,
            detail: format!("{} entries verified", entries.len()),
        }
    }
}

/// Hash of one link: previous hash plus the entry's own fields.
pub fn chain_hash(prev: &str, ts: &str, src: &str, dst: &str, rule: &str, hash: &str) -> String {
    let canonical = format!("{prev}\u{0}{ts}\u{0}{src}\u{0}{dst}\u{0}{rule}\u{0}{hash}");
    format!("b3:{}", blake3::hash(canonical.as_bytes()).to_hex())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn audit(dir: &tempfile::TempDir) -> Audit {
        Audit::new(&dir.path().join("audit.jsonl"))
    }

    #[test]
    fn an_empty_log_verifies() {
        let dir = tempfile::tempdir().unwrap();
        let report = audit(&dir).verify();
        assert!(report.ok);
        assert_eq!(report.entries, 0);
    }

    #[test]
    fn appends_link_to_the_previous_entry() {
        let dir = tempfile::tempdir().unwrap();
        let log = audit(&dir);
        log.append("/in/a.pdf", "/out/a.pdf", "keyword:invoice", "b3:a")
            .unwrap();
        log.append("/in/b.pdf", "/out/b.pdf", "format:text", "b3:b")
            .unwrap();

        let entries = log.entries();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].prev, GENESIS);
        assert_eq!(entries[1].prev, entries[0].chain);
        assert!(log.verify().ok);
    }

    #[test]
    fn editing_an_entry_breaks_the_chain() {
        let dir = tempfile::tempdir().unwrap();
        let log = audit(&dir);
        log.append("/in/a.pdf", "/out/a.pdf", "keyword:invoice", "b3:a")
            .unwrap();
        log.append("/in/b.pdf", "/out/b.pdf", "format:text", "b3:b")
            .unwrap();

        // Rewrite the first entry's destination, leaving the chain hash alone.
        let text = std::fs::read_to_string(log.path()).unwrap();
        let tampered = text.replacen("/out/a.pdf", "/elsewhere/a.pdf", 1);
        std::fs::write(log.path(), tampered).unwrap();

        let report = log.verify();
        assert!(!report.ok);
        assert_eq!(report.first_break, Some(1));
        assert!(report.detail.contains("modified"));
    }

    #[test]
    fn deleting_an_entry_breaks_the_chain() {
        let dir = tempfile::tempdir().unwrap();
        let log = audit(&dir);
        for i in 0..3 {
            log.append(
                &format!("/in/{i}.pdf"),
                &format!("/out/{i}.pdf"),
                "format:text",
                "b3:x",
            )
            .unwrap();
        }

        let text = std::fs::read_to_string(log.path()).unwrap();
        let kept: Vec<&str> = text
            .lines()
            .enumerate()
            .filter(|(i, _)| *i != 1)
            .map(|(_, l)| l)
            .collect();
        std::fs::write(log.path(), format!("{}\n", kept.join("\n"))).unwrap();

        let report = log.verify();
        assert!(!report.ok);
        assert_eq!(report.first_break, Some(2));
        assert!(report.detail.contains("points at"));
    }

    #[test]
    fn hashes_depend_on_every_field() {
        let base = chain_hash(GENESIS, "t", "s", "d", "r", "h");
        assert_ne!(base, chain_hash(GENESIS, "t2", "s", "d", "r", "h"));
        assert_ne!(base, chain_hash(GENESIS, "t", "s2", "d", "r", "h"));
        assert_ne!(base, chain_hash(GENESIS, "t", "s", "d2", "r", "h"));
        assert_ne!(base, chain_hash(GENESIS, "t", "s", "d", "r2", "h"));
        assert_ne!(base, chain_hash(GENESIS, "t", "s", "d", "r", "h2"));
        assert_ne!(base, chain_hash("other", "t", "s", "d", "r", "h"));
    }
}
