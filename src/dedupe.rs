//! Duplicate detection.
//!
//! Two questions, two mechanisms:
//!
//! * **Exact** — BLAKE3 digest, looked up in a persisted registry. Configurable
//!   action: skip, move to `_Duplicates/`, or keep the newest.
//! * **Near** — cosine similarity above 0.92 on the embedding index (phase 3).
//!   Until the index exists, near-duplicate detection is simply unavailable, and
//!   says so rather than guessing.
//!
//! Sorta **never deletes a file**. The strongest thing it does is move one into
//! `_Duplicates/` and report how much space the user could reclaim.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::hash::{ContentHash, FastRejector};

/// What to do with an exact duplicate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum DupAction {
    /// Leave it where it is and log the match.
    #[default]
    Skip,
    /// Move it to `_Duplicates/`.
    MoveToDuplicates,
    /// Keep the newest copy, quarantine the older one for review.
    KeepNewest,
}

/// One known file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DedupEntry {
    pub hash: String,
    pub canonical_path: String,
    pub size: u64,
    pub created_at: String,
}

/// On-disk registry: `~/.sorta/dedup_registry.json`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Registry {
    #[serde(default)]
    pub entries: BTreeMap<String, DedupEntry>,
}

/// The duplicate detector the pipeline talks to.
pub struct Deduper {
    path: PathBuf,
    registry: Registry,
    fast: FastRejector,
    dirty: bool,
}

impl Deduper {
    /// Loads the registry, or starts an empty one.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let registry = if path.exists() {
            let text = std::fs::read_to_string(path)?;
            serde_json::from_str(&text).unwrap_or_default()
        } else {
            Registry::default()
        };

        let mut fast = FastRejector::new(registry.entries.len().max(64));
        for entry in registry.entries.values() {
            fast.record(pre_of(&entry.hash), &ContentHash(entry.hash.clone()));
        }

        Ok(Self {
            path: path.to_path_buf(),
            registry,
            fast,
            dirty: false,
        })
    }

    /// An in-memory detector, for tests and read-only runs.
    pub fn in_memory() -> Self {
        Self {
            path: PathBuf::new(),
            registry: Registry::default(),
            fast: FastRejector::new(64),
            dirty: false,
        }
    }

    /// Looks up a digest.
    pub fn lookup(&self, hash: &ContentHash) -> Option<&DedupEntry> {
        self.registry.entries.get(hash.as_str())
    }

    /// Cheap pre-filter: false means definitely new.
    pub fn might_be_duplicate(&self, pre_hash: u64) -> bool {
        !self.fast.candidates(pre_hash).is_empty()
    }

    /// Records a file as canonical.
    pub fn record(&mut self, hash: &ContentHash, path: &Path, size: u64) {
        let entry = DedupEntry {
            hash: hash.as_str().to_string(),
            canonical_path: path.display().to_string(),
            size,
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        self.fast.record(pre_of(hash.as_str()), hash);
        self.registry
            .entries
            .insert(hash.as_str().to_string(), entry);
        self.dirty = true;
    }

    /// Drops a path from the registry (used when a canonical copy is undone).
    pub fn forget_path(&mut self, path: &Path) {
        let shown = path.display().to_string();
        let before = self.registry.entries.len();
        self.registry
            .entries
            .retain(|_, entry| entry.canonical_path != shown);
        if self.registry.entries.len() != before {
            self.dirty = true;
        }
    }

    /// Total bytes held by duplicate copies that were moved aside.
    pub fn known_count(&self) -> usize {
        self.registry.entries.len()
    }

    /// Writes the registry back to disk if anything changed.
    pub fn save(&mut self) -> anyhow::Result<()> {
        if !self.dirty || self.path.as_os_str().is_empty() {
            return Ok(());
        }
        if let Some(parent) = self.path.parent() {
            crate::paths::ensure_dir(parent)?;
        }
        let text = serde_json::to_string_pretty(&self.registry)?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &self.path)?;
        self.dirty = false;
        Ok(())
    }
}

/// Derives the 4 KB pre-hash key from a stored digest.
///
/// The registry stores full digests; the pre-hash map only needs a stable key, so
/// the leading 8 bytes of the digest serve exactly as well as a re-read of the
/// file — and cost nothing.
fn pre_of(hash: &str) -> u64 {
    let hex = hash.strip_prefix("b3:").unwrap_or(hash);
    let mut out = [0u8; 8];
    for (i, byte) in out.iter_mut().enumerate() {
        let start = i * 2;
        if start + 2 <= hex.len() {
            if let Ok(v) = u8::from_str_radix(&hex[start..start + 2], 16) {
                *byte = v;
            }
        }
    }
    u64::from_le_bytes(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_finds() {
        let mut deduper = Deduper::in_memory();
        let hash = ContentHash("b3:00112233445566778899aabbccddeeff".to_string());
        assert!(deduper.lookup(&hash).is_none());
        deduper.record(&hash, Path::new("/tmp/a.pdf"), 10);
        assert!(deduper.lookup(&hash).is_some());
        assert_eq!(deduper.known_count(), 1);
    }

    #[test]
    fn survives_a_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dedup.json");
        {
            let mut deduper = Deduper::load(&path).unwrap();
            deduper.record(
                &ContentHash("b3:aa".to_string()),
                Path::new("/tmp/x.pdf"),
                42,
            );
            deduper.save().unwrap();
        }
        let reloaded = Deduper::load(&path).unwrap();
        assert!(reloaded.lookup(&ContentHash("b3:aa".to_string())).is_some());
    }

    #[test]
    fn forgetting_a_path_drops_it() {
        let mut deduper = Deduper::in_memory();
        deduper.record(
            &ContentHash("b3:bb".to_string()),
            Path::new("/tmp/y.pdf"),
            1,
        );
        deduper.forget_path(Path::new("/tmp/y.pdf"));
        assert_eq!(deduper.known_count(), 0);
    }
}
