//! Turso-backed index — experimental, behind the `index-turso` feature.
//!
//! # Why this exists
//!
//! The JSON index in [`crate::search`] is a flat file: fine to ~20 000 documents,
//! then it starts to hurt (every load parses the whole thing, every search scans
//! it). The blueprint's answer was SQLite + FTS5 + `sqlite-vec`, which needs
//! `rusqlite` (bundled C SQLite) and the `sqlite-vec` C extension — both of which
//! break the "no C runtime" rule this project set for itself.
//!
//! Turso (`tursodatabase/turso`, MIT) is an in-process SQL database written in
//! Rust, SQLite-file-format compatible, that compiles on stable Rust and supports
//! Windows. It is the only candidate found so far that gives SQL + full-text
//! search + vector functions without a C toolchain.
//!
//! # Why it is not the default
//!
//! * Every published version is a pre-release (`0.8.4-pre.3` at the time of
//!   writing), so the API can move under us.
//! * It is async and needs a runtime; Sorta's engine is deliberately synchronous.
//!   This module wraps it in a current-thread runtime, which is honest but not free.
//! * The full-text feature is backed by tantivy and marked experimental upstream.
//!   This spike therefore searches with `LIKE` and leaves FTS to a later step.
//!
//! So: default stays the JSON index, `index-turso` is opt-in, and nothing else in
//! the crate knows this module exists.

use std::path::Path;

use crate::search::{Hit, SearchDoc};

/// Schema, mirroring the JSON index field for field.
const SCHEMA: &str = "\
CREATE TABLE IF NOT EXISTS files (
    path  TEXT PRIMARY KEY,
    name  TEXT NOT NULL,
    text  TEXT NOT NULL DEFAULT '',
    kind  TEXT NOT NULL DEFAULT '',
    size  INTEGER NOT NULL DEFAULT 0,
    mtime INTEGER NOT NULL DEFAULT 0,
    hash  TEXT NOT NULL DEFAULT ''
)";

/// A Turso-backed document index.
pub struct TursoIndex {
    runtime: tokio::runtime::Runtime,
    /// Held so the database outlives every connection taken from it.
    _database: turso::Database,
    connection: turso::Connection,
}

impl TursoIndex {
    /// Opens (or creates) the database and applies the schema.
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                crate::paths::ensure_dir(parent)?;
            }
        }

        let location = path.to_string_lossy().to_string();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;

        let (database, connection) = runtime.block_on(async {
            let database = turso::Builder::new_local(&location).build().await?;
            let connection = database.connect()?;
            connection.execute(SCHEMA, ()).await?;
            Ok::<_, anyhow::Error>((database, connection))
        })?;

        Ok(Self {
            runtime,
            _database: database,
            connection,
        })
    }

    /// Inserts or replaces one document.
    pub fn upsert(&self, doc: &SearchDoc) -> anyhow::Result<()> {
        self.runtime.block_on(async {
            self.connection
                .execute(
                    "INSERT INTO files (path, name, text, kind, size, mtime, hash)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                     ON CONFLICT(path) DO UPDATE SET
                        name = excluded.name,
                        text = excluded.text,
                        kind = excluded.kind,
                        size = excluded.size,
                        mtime = excluded.mtime,
                        hash = excluded.hash",
                    (
                        doc.path.as_str(),
                        doc.name.as_str(),
                        doc.text.as_str(),
                        doc.kind.as_str(),
                        doc.size as i64,
                        doc.mtime,
                        doc.hash.as_str(),
                    ),
                )
                .await?;
            Ok(())
        })
    }

    /// Substring search over name and body, best (shortest name) first.
    pub fn search(&self, query: &str, limit: usize) -> anyhow::Result<Vec<Hit>> {
        let cleaned = query.trim().to_lowercase().replace('%', "").replace('_', "");
        if cleaned.is_empty() {
            return Ok(Vec::new());
        }
        let needle = format!("%{cleaned}%");
        let limit = limit.max(1) as i64;

        self.runtime.block_on(async {
            let mut rows = self
                .connection
                .query(
                    "SELECT path, name, text FROM files
                     WHERE lower(name) LIKE ?1 OR lower(text) LIKE ?1
                     ORDER BY length(name) ASC
                     LIMIT ?2",
                    (needle.as_str(), limit),
                )
                .await?;

            let mut hits: Vec<Hit> = Vec::new();
            while let Some(row) = rows.next().await? {
                let path = row.get_value(0)?;
                let name = row.get_value(1)?;
                let text = row.get_value(2)?;

                let path = path.as_text().cloned().unwrap_or_default();
                let name = name.as_text().cloned().unwrap_or_default();
                let body = text.as_text().cloned().unwrap_or_default();

                let score = if name.to_lowercase().contains(&cleaned) {
                    3
                } else {
                    1
                };
                let snippet: String = body.chars().take(120).collect();

                hits.push(Hit {
                    path,
                    score,
                    snippet,
                });
            }
            Ok(hits)
        })
    }

    /// Number of indexed documents.
    pub fn len(&self) -> anyhow::Result<usize> {
        self.runtime.block_on(async {
            let mut rows = self
                .connection
                .query("SELECT count(*) FROM files", ())
                .await?;
            let mut count = 0usize;
            while let Some(row) = rows.next().await? {
                let value = row.get_value(0)?;
                if let Some(number) = value.as_integer() {
                    count = *number as usize;
                }
            }
            Ok(count)
        })
    }

    /// True when nothing is indexed.
    pub fn is_empty(&self) -> anyhow::Result<bool> {
        Ok(self.len()? == 0)
    }
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
            mtime: 1_700_000_000,
            kind: "Documents".to_string(),
            hash: "b3:aa".to_string(),
        }
    }

    #[test]
    fn stores_and_finds_documents() {
        let dir = tempfile::tempdir().unwrap();
        let index = TursoIndex::open(&dir.path().join("index.db")).unwrap();

        index
            .upsert(&doc("/s/acme.pdf", "acme.pdf", "invoice from Acme Corp"))
            .unwrap();
        index
            .upsert(&doc("/s/lease.pdf", "lease.pdf", "lease agreement"))
            .unwrap();
        assert_eq!(index.len().unwrap(), 2);
        assert!(!index.is_empty().unwrap());

        let hits = index.search("acme", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "/s/acme.pdf");
        assert!(hits[0].snippet.contains("Acme"));
    }

    #[test]
    fn upsert_replaces_the_row() {
        let dir = tempfile::tempdir().unwrap();
        let index = TursoIndex::open(&dir.path().join("index.db")).unwrap();

        index.upsert(&doc("/s/a.pdf", "a.pdf", "first body")).unwrap();
        index.upsert(&doc("/s/a.pdf", "a.pdf", "second body")).unwrap();

        assert_eq!(index.len().unwrap(), 1);
        assert!(index.search("first", 10).unwrap().is_empty());
        assert_eq!(index.search("second", 10).unwrap().len(), 1);
    }

    #[test]
    fn an_empty_query_matches_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let index = TursoIndex::open(&dir.path().join("index.db")).unwrap();
        index.upsert(&doc("/s/a.pdf", "a.pdf", "body")).unwrap();
        assert!(index.search("   ", 10).unwrap().is_empty());
    }
}
