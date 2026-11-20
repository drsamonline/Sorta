//! The quarantine queue.
//!
//! A file whose best rule lands below `min_confidence` is not filed by guesswork.
//! It goes to `Quarantine/` and an entry appears in `quarantine.toml` with the
//! best guess, the confidence and the reason, so the user can review it once and
//! teach the rule.
//!
//! States: `new` → `reviewed` → `resolved`. Only `reviewed` entries are ever
//! auto-archived, and only after 30 days.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Lifecycle of a quarantined file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum QuarantineState {
    #[default]
    New,
    Reviewed,
    Resolved,
}

/// Why the file is here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuarantineReason {
    LowConfidence,
    Ambiguous,
    ExtractionFailed,
    UserRequest,
    /// Semantically very close to a file already filed.
    PossibleDuplicate,
}

impl QuarantineReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LowConfidence => "low_confidence",
            Self::Ambiguous => "ambiguous",
            Self::ExtractionFailed => "extraction_failed",
            Self::UserRequest => "user_request",
            Self::PossibleDuplicate => "possible_duplicate",
        }
    }
}

/// One queued file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuarantineItem {
    pub path: String,
    pub detected_type: String,
    pub best_guess: String,
    pub confidence: f32,
    pub reason: String,
    pub created_at: String,
    #[serde(default)]
    pub state: QuarantineState,
    pub suggested_dest: String,
}

/// The registry file.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct QuarantineFile {
    #[serde(default)]
    pub items: Vec<QuarantineItem>,
}

/// Handle to `<quarantine_root>/quarantine.toml`.
#[derive(Debug, Clone)]
pub struct Quarantine {
    root: PathBuf,
}

impl Quarantine {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn registry_path(&self) -> PathBuf {
        self.root.join("quarantine.toml")
    }

    /// Reads the registry; a missing or corrupt file reads as empty rather than
    /// blocking the sort. Corruption is reported by `sorta status`, not by
    /// refusing to work.
    pub fn load(&self) -> QuarantineFile {
        let path = self.registry_path();
        if !path.exists() {
            return QuarantineFile::default();
        }
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).unwrap_or_default(),
            Err(_) => QuarantineFile::default(),
        }
    }

    /// Appends an entry, creating the registry if needed.
    pub fn add(&self, item: QuarantineItem) -> anyhow::Result<()> {
        let mut file = self.load();
        file.items.push(item);
        self.save(&file)
    }

    /// Writes the whole registry atomically.
    pub fn save(&self, file: &QuarantineFile) -> anyhow::Result<()> {
        crate::paths::ensure_dir(&self.root)?;
        let text = toml::to_string_pretty(file)?;
        let path = self.registry_path();
        let tmp = self.root.join("quarantine.toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Entries still waiting for a human.
    pub fn pending(&self) -> Vec<QuarantineItem> {
        self.load()
            .items
            .into_iter()
            .filter(|i| i.state == QuarantineState::New)
            .collect()
    }

    /// Marks an entry reviewed (the only state eligible for auto-archive).
    pub fn mark_reviewed(&self, path: &str) -> anyhow::Result<bool> {
        let mut file = self.load();
        let mut changed = false;
        for item in file.items.iter_mut() {
            if item.path == path && item.state == QuarantineState::New {
                item.state = QuarantineState::Reviewed;
                changed = true;
            }
        }
        if changed {
            self.save(&file)?;
        }
        Ok(changed)
    }

    /// Removes an entry entirely (the file was filed or deleted).
    pub fn resolve(&self, path: &str) -> anyhow::Result<bool> {
        let mut file = self.load();
        let before = file.items.len();
        file.items.retain(|i| i.path != path);
        let changed = file.items.len() != before;
        if changed {
            self.save(&file)?;
        }
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(path: &str) -> QuarantineItem {
        QuarantineItem {
            path: path.to_string(),
            detected_type: "application/pdf".to_string(),
            best_guess: "invoice".to_string(),
            confidence: 0.62,
            reason: QuarantineReason::LowConfidence.as_str().to_string(),
            created_at: "2026-11-19T14:00:00Z".to_string(),
            state: QuarantineState::New,
            suggested_dest: "2026/PDF-Invoices/Unknown".to_string(),
        }
    }

    #[test]
    fn adds_and_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let q = Quarantine::new(dir.path());
        q.add(item("/tmp/a.pdf")).unwrap();
        q.add(item("/tmp/b.pdf")).unwrap();

        let pending = q.pending();
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].reason, "low_confidence");
        assert_eq!(pending[0].state, QuarantineState::New);
    }

    #[test]
    fn review_then_resolve() {
        let dir = tempfile::tempdir().unwrap();
        let q = Quarantine::new(dir.path());
        q.add(item("/tmp/a.pdf")).unwrap();

        assert!(q.mark_reviewed("/tmp/a.pdf").unwrap());
        assert!(q.pending().is_empty());

        assert!(q.resolve("/tmp/a.pdf").unwrap());
        assert!(q.load().items.is_empty());
    }

    #[test]
    fn missing_registry_reads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let q = Quarantine::new(dir.path());
        assert!(q.load().items.is_empty());
    }
}
