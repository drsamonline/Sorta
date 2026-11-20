//! Stored embeddings, and the near-duplicate lookup that uses them.
//!
//! Vectors live in `~/.sorta/vectors.json`, capped at [`CAP`] entries: this is a
//! working set for "is this the same document as one I already filed?", not an
//! archive. The JSON index stays the system of record for paths and text; this file
//! is disposable, and losing it costs one re-embed per file.
//!
//! Comparison is brute-force cosine over the stored vectors. At 2 000 documents and
//! 384 dimensions that is 768 000 multiply-adds per file — well under a millisecond
//! — so an approximate index would be complexity for nothing until the cap rises by
//! two orders of magnitude.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::embedding::cosine;

/// How many vectors are kept.
pub const CAP: usize = 2_000;

/// One stored vector.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorEntry {
    /// Path the vector belongs to.
    pub path: String,
    /// Content digest at embed time, so a changed file is re-embedded.
    pub hash: String,
    /// Width of `values`.
    pub dim: usize,
    /// The embedding itself.
    pub values: Vec<f32>,
    /// Unix seconds; used only for eviction order.
    pub at: i64,
}

/// The whole store.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VectorStore {
    #[serde(default)]
    pub entries: Vec<VectorEntry>,
}

impl VectorStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Loads the store; a missing or corrupt file reads as empty.
    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        serde_json::from_str(&text).unwrap_or_default()
    }

    /// Writes the store atomically.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                crate::paths::ensure_dir(parent)?;
            }
        }
        let text = serde_json::to_string(self)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Adds or replaces the vector for a path, evicting the oldest when full.
    pub fn upsert(&mut self, entry: VectorEntry) {
        match self.entries.iter_mut().find(|e| e.path == entry.path) {
            Some(existing) => *existing = entry,
            None => {
                if self.entries.len() >= CAP {
                    if let Some((index, _)) =
                        self.entries.iter().enumerate().min_by_key(|(_, e)| e.at)
                    {
                        self.entries.remove(index);
                    }
                }
                self.entries.push(entry);
            }
        }
    }

    /// Drops a path. Returns true when something was removed.
    pub fn remove(&mut self, path: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| e.path != path);
        self.entries.len() != before
    }

    /// The most similar stored document above `threshold`.
    ///
    /// `exclude` skips one path, which is what the pipeline uses so a file never
    /// matches itself after a re-sort.
    pub fn nearest(
        &self,
        vector: &[f32],
        threshold: f32,
        exclude: Option<&str>,
    ) -> Option<(String, f32)> {
        if vector.is_empty() {
            return None;
        }

        let mut best: Option<(String, f32)> = None;
        for entry in &self.entries {
            if entry.dim != vector.len() {
                continue;
            }
            if exclude.map(|path| path == entry.path).unwrap_or(false) {
                continue;
            }
            let score = cosine(vector, &entry.values);
            if score < threshold {
                continue;
            }
            match &best {
                Some((_, best_score)) if *best_score >= score => {}
                _ => best = Some((entry.path.clone(), score)),
            }
        }
        best
    }

    /// Every stored path whose similarity clears `threshold`, best first.
    pub fn similar(&self, vector: &[f32], threshold: f32, limit: usize) -> Vec<(String, f32)> {
        if vector.is_empty() {
            return Vec::new();
        }
        let mut hits: Vec<(String, f32)> = self
            .entries
            .iter()
            .filter(|entry| entry.dim == vector.len())
            .map(|entry| (entry.path.clone(), cosine(vector, &entry.values)))
            .filter(|(_, score)| *score >= threshold)
            .collect();
        hits.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        hits.truncate(limit.max(1));
        hits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, values: Vec<f32>, at: i64) -> VectorEntry {
        VectorEntry {
            path: path.to_string(),
            hash: "b3:aa".to_string(),
            dim: values.len(),
            values,
            at,
        }
    }

    #[test]
    fn stores_and_replaces() {
        let mut store = VectorStore::new();
        store.upsert(entry("/a.pdf", vec![1.0, 0.0], 1));
        store.upsert(entry("/a.pdf", vec![0.0, 1.0], 2));
        assert_eq!(store.len(), 1);
        assert_eq!(store.entries[0].values, vec![0.0, 1.0]);
    }

    #[test]
    fn finds_the_nearest_above_the_threshold() {
        let mut store = VectorStore::new();
        store.upsert(entry("/invoice-a.pdf", vec![1.0, 0.0, 0.0], 1));
        store.upsert(entry("/invoice-b.pdf", vec![0.99, 0.01, 0.0], 2));
        store.upsert(entry("/photo.png", vec![0.0, 1.0, 0.0], 3));

        let probe = vec![1.0, 0.0, 0.0];

        // The exact match wins, as it should.
        let (path, score) = store.nearest(&probe, 0.92, None).expect("a match");
        assert_eq!(path, "/invoice-a.pdf");
        assert!((score - 1.0).abs() < 1e-6);

        // Excluding it surfaces the next-closest, which is what the pipeline does
        // so a file never matches itself after a re-sort.
        let (path, score) = store
            .nearest(&probe, 0.92, Some("/invoice-a.pdf"))
            .expect("a second match");
        assert_eq!(path, "/invoice-b.pdf");
        assert!(score > 0.99);

        // Nothing clears the bar for an orthogonal vector.
        assert!(store.nearest(&[0.0, 0.0, 1.0], 0.92, None).is_none());
    }

    #[test]
    fn exclude_skips_itself() {
        let mut store = VectorStore::new();
        store.upsert(entry("/a.pdf", vec![1.0, 0.0], 1));
        assert!(store.nearest(&[1.0, 0.0], 0.92, Some("/a.pdf")).is_none());
        assert!(store
            .nearest(&[1.0, 0.0], 0.92, Some("/other.pdf"))
            .is_some());
    }

    #[test]
    fn mismatched_widths_are_ignored() {
        let mut store = VectorStore::new();
        store.upsert(entry("/a.pdf", vec![1.0, 0.0, 0.0], 1));
        assert!(store.nearest(&[1.0, 0.0], 0.5, None).is_none());
    }

    #[test]
    fn evicts_the_oldest_when_full() {
        let mut store = VectorStore::new();
        for i in 0..CAP {
            store.upsert(entry(&format!("/f{i}.pdf"), vec![1.0, 0.0], i as i64));
        }
        assert_eq!(store.len(), CAP);
        store.upsert(entry("/newest.pdf", vec![0.0, 1.0], i64::MAX));
        assert_eq!(store.len(), CAP);
        assert!(!store.entries.iter().any(|e| e.path == "/f0.pdf"));
        assert!(store.entries.iter().any(|e| e.path == "/newest.pdf"));
    }

    #[test]
    fn removes_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vectors.json");

        let mut store = VectorStore::new();
        store.upsert(entry("/a.pdf", vec![0.5, 0.5], 1));
        store.upsert(entry("/b.pdf", vec![0.1, 0.9], 2));
        store.save(&path).unwrap();

        let mut reloaded = VectorStore::load(&path);
        assert_eq!(reloaded.len(), 2);
        assert!(reloaded.remove("/a.pdf"));
        assert_eq!(reloaded.len(), 1);
        assert!(!reloaded.remove("/nope"));
    }

    #[test]
    fn corrupt_store_reads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vectors.json");
        std::fs::write(&path, "not json").unwrap();
        assert!(VectorStore::load(&path).is_empty());
    }

    #[test]
    fn similar_lists_best_first() {
        let mut store = VectorStore::new();
        store.upsert(entry("/exact.pdf", vec![1.0, 0.0], 1));
        store.upsert(entry("/close.pdf", vec![0.95, 0.05], 2));
        store.upsert(entry("/far.pdf", vec![0.0, 1.0], 3));

        let hits = store.similar(&[1.0, 0.0], 0.9, 5);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].0, "/exact.pdf");
    }
}
