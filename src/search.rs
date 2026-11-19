//! Full-text search over the sorted tree.
//!
//! Phase 1 has no SQLite and no vector index, so this is a deliberately small
//! pure-Rust index: for every file Sorta moves, it stores the name, the detected
//! category and up to 4 KB of extracted text. `sorta search` scans that in memory.
//!
//! That is enough to answer "where did that invoice from Acme go?" without a
//! dependency, a server or a background process. It is **not** the final design:
//! the blueprint's FTS5 + `sqlite-vec` index replaces this when the database
//! lands, and keeps this file format as the fallback for a portable run.
//!
//! Index rules:
//! * matching is case-insensitive and token-based; every query token must appear
//! * a hit in the file name scores 3, a hit in the body scores 1 per occurrence
//!   (capped at 10, so a repeated word cannot drown the name signal)
//! * the index is capped at 20 000 documents; the oldest are dropped first

use std::path::Path;

use serde::{Deserialize, Serialize};

/// How much of a document's text is kept.
pub const TEXT_CAP: usize = 4096;

/// Hard cap on documents in the index.
pub const DOC_CAP: usize = 20_000;

/// One indexed file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchDoc {
    /// Absolute path after sorting.
    pub path: String,
    /// File name, kept separately so name hits are cheap.
    pub name: String,
    /// Extracted body, truncated to [`TEXT_CAP`].
    pub text: String,
    pub size: u64,
    /// Unix seconds; used only for eviction order.
    pub mtime: i64,
    /// Detected category, for display.
    pub kind: String,
}

/// The whole index, as stored on disk.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Index {
    #[serde(default)]
    pub docs: Vec<SearchDoc>,
}

/// One search result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub path: String,
    pub score: u32,
    pub snippet: String,
}

impl Index {
    pub fn new() -> Self {
        Self::default()
    }

    /// Loads the index; a missing or corrupt file reads as empty.
    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        serde_json::from_str(&text).unwrap_or_default()
    }

    /// Writes the index atomically.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            crate::paths::ensure_dir(parent)?;
        }
        let text = serde_json::to_string(self)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.docs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    /// Adds or replaces a document, evicting the oldest when full.
    pub fn upsert(&mut self, doc: SearchDoc) {
        match self.docs.iter_mut().find(|d| d.path == doc.path) {
            Some(existing) => *existing = doc,
            None => {
                if self.docs.len() >= DOC_CAP {
                    // Drop the least recently modified entry.
                    if let Some((idx, _)) =
                        self.docs.iter().enumerate().min_by_key(|(_, d)| d.mtime)
                    {
                        self.docs.remove(idx);
                    }
                }
                self.docs.push(doc);
            }
        }
    }

    /// Removes a path from the index. Returns true when something was removed.
    pub fn remove(&mut self, path: &str) -> bool {
        let before = self.docs.len();
        self.docs.retain(|d| d.path != path);
        self.docs.len() != before
    }

    /// Searches names and bodies. Returns hits, best first.
    pub fn search(&self, query: &str, limit: usize) -> Vec<Hit> {
        let tokens = tokenize(query);
        if tokens.is_empty() {
            return Vec::new();
        }

        let mut hits: Vec<Hit> = Vec::new();
        for doc in &self.docs {
            let name = doc.name.to_lowercase();
            let body = doc.text.to_lowercase();

            // Every token must appear somewhere; otherwise this is not a match.
            if !tokens
                .iter()
                .all(|t| name.contains(t.as_str()) || body.contains(t.as_str()))
            {
                continue;
            }

            let mut score = 0u32;
            for token in &tokens {
                if name.contains(token.as_str()) {
                    score += 3;
                }
                let occurrences = count_occurrences(&body, token).min(10);
                score += occurrences;
            }

            let snippet = match tokens.iter().find(|t| body.contains(t.as_str())) {
                Some(token) => snippet_around(&doc.text, token),
                None => doc.name.clone(),
            };

            hits.push(Hit {
                path: doc.path.clone(),
                score,
                snippet,
            });
        }

        hits.sort_by(|a, b| b.score.cmp(&a.score).then(a.path.cmp(&b.path)));
        hits.truncate(limit.max(1));
        hits
    }
}

/// Lowercases and splits a query into tokens, dropping duplicates.
fn tokenize(query: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    for raw in query.to_lowercase().split(|c: char| !c.is_alphanumeric()) {
        if raw.is_empty() {
            continue;
        }
        let token = raw.to_string();
        if !tokens.contains(&token) {
            tokens.push(token);
        }
    }
    tokens
}

fn count_occurrences(haystack: &str, needle: &str) -> u32 {
    if needle.is_empty() {
        return 0;
    }
    haystack.matches(needle).count() as u32
}

/// A 120-character window around the first occurrence of `needle`.
///
/// Works on `char` vectors so a multi-byte character can never be sliced in half.
fn snippet_around(text: &str, needle: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let lower: Vec<char> = text.to_lowercase().chars().collect();
    let target: Vec<char> = needle.to_lowercase().chars().collect();

    let position = if target.is_empty() {
        None
    } else {
        lower
            .windows(target.len())
            .position(|window| window == target.as_slice())
    };

    let start = position
        .map(|p| p.saturating_sub(40))
        .unwrap_or(0)
        .min(chars.len());
    let end = (start + 120).min(chars.len());
    chars[start..end]
        .iter()
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(path: &str, name: &str, text: &str) -> SearchDoc {
        SearchDoc {
            path: path.to_string(),
            name: name.to_string(),
            text: text.to_string(),
            size: text.len() as u64,
            mtime: 0,
            kind: "Documents".to_string(),
        }
    }

    fn index() -> Index {
        let mut index = Index::new();
        index.upsert(doc(
            "/data/2026/PDF-Invoices/acme.pdf",
            "acme invoice.pdf",
            "Invoice from Acme Corp, November 2026, total 48123",
        ));
        index.upsert(doc(
            "/data/2026/Contracts/lease.pdf",
            "lease.pdf",
            "Lease agreement between the parties",
        ));
        index
    }

    #[test]
    fn finds_by_body_text() {
        let hits = index().search("acme corp", 10);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].path.contains("acme.pdf"));
        assert!(hits[0].snippet.contains("Acme"));
    }

    #[test]
    fn finds_by_name_and_scores_it_higher() {
        let hits = index().search("lease", 10);
        assert_eq!(hits.len(), 1);
        // name hit (3) plus one body occurrence (1)
        assert_eq!(hits[0].score, 4);
    }

    #[test]
    fn every_token_must_match() {
        assert!(index().search("acme lease", 10).is_empty());
    }

    #[test]
    fn empty_query_matches_nothing() {
        assert!(index().search("   ", 10).is_empty());
        assert!(index().search("", 10).is_empty());
    }

    #[test]
    fn upsert_replaces_and_remove_deletes() {
        let mut index = index();
        index.upsert(doc(
            "/data/2026/PDF-Invoices/acme.pdf",
            "acme invoice.pdf",
            "replaced body",
        ));
        assert_eq!(index.len(), 2);
        assert!(index.search("november", 10).is_empty());

        assert!(index.remove("/data/2026/PDF-Invoices/acme.pdf"));
        assert_eq!(index.len(), 1);
        assert!(!index.remove("/nope"));
    }

    #[test]
    fn survives_a_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("search_index.json");
        index().save(&path).unwrap();
        let reloaded = Index::load(&path);
        assert_eq!(reloaded.len(), 2);
        assert_eq!(reloaded.search("lease", 5).len(), 1);
    }

    #[test]
    fn corrupt_index_reads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("search_index.json");
        std::fs::write(&path, "not json").unwrap();
        assert!(Index::load(&path).is_empty());
    }

    #[test]
    fn snippets_do_not_panic_on_multibyte_text() {
        let mut index = Index::new();
        index.upsert(doc(
            "/data/a.txt",
            "a.txt",
            &format!("{}invoice{}", "é".repeat(300), "ü".repeat(300)),
        ));
        let hits = index.search("invoice", 1);
        assert_eq!(hits.len(), 1);
        assert!(!hits[0].snippet.is_empty());
    }
}
