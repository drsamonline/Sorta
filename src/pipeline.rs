//! The pipeline.
//!
//! One file, start to finish:
//!
//! ```text
//! detect → hash → extract → classify → dry-check → commit → verify → log → receipt
//! ```
//!
//! Every step can end the file's journey, and every ending is logged with a
//! reason. The two invariants:
//!
//! * a move that does not verify is rolled back;
//! * a decision below `min_confidence` goes to quarantine instead of a folder
//!   Sorta invented.
//!
//! Nothing here deletes a file. The strongest destructive action is `replace`,
//! which only fires when the user configured it and the destination is a
//! byte-identical collision candidate.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::{CollisionPolicy, Config, DupPolicy, LoadedConfig, NearDuplicateAction};
use crate::audit::Audit;
use crate::dedupe::Deduper;
use crate::detect;
use crate::embedding::Embedder;
use crate::errors::SortaError;
use crate::extract;
use crate::hash::{self, ContentHash};
use crate::history::{History, HistoryEntry};
use crate::hooks::{HookContext, HookRunner};
use crate::paths;
use crate::quarantine::{Quarantine, QuarantineItem, QuarantineReason};
use crate::safety::{self, Resolution, StabilityChecker};
use crate::search::{Index, SearchDoc, TEXT_CAP};
use crate::sort::{Classified, Classifier};
use crate::vectors::{VectorEntry, VectorStore};

/// How a file's journey ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Moved and verified.
    Moved,
    /// Would have moved (dry run or read-only).
    Planned,
    /// Deliberately untouched.
    Skipped,
    /// Byte-identical copy of a file already known.
    Duplicate,
    /// Confidence below the gate.
    Quarantined,
    /// Locked after all retries; queued in `_deferred/`.
    Deferred,
    /// Something went wrong; nothing changed.
    Failed,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Self::Moved => "Moved",
            Self::Planned => "Planned",
            Self::Skipped => "Skipped",
            Self::Duplicate => "Duplicate",
            Self::Quarantined => "Quarantined",
            Self::Deferred => "Deferred",
            Self::Failed => "Failed",
        }
    }
}

/// The result for one file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Outcome {
    pub src: String,
    pub dst: Option<String>,
    pub rule: String,
    pub confidence: f32,
    pub signals: Vec<String>,
    pub status: Status,
    pub note: String,
}

impl Outcome {
    fn new(src: &Path, status: Status, note: &str) -> Self {
        Self {
            src: src.display().to_string(),
            dst: None,
            rule: String::new(),
            confidence: 0.0,
            signals: Vec::new(),
            status,
            note: note.to_string(),
        }
    }
}

/// `<file>.sorta.json`: portable metadata beside a sorted file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sidecar {
    pub src: String,
    pub dst: String,
    pub rule: String,
    pub confidence: f32,
    pub signals: Vec<String>,
    pub sorted_at: String,
    pub hash: String,
    pub version: String,
}

/// Written into `_deferred/` when a file could not be moved after every retry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeferredRecord {
    pub path: String,
    pub error: String,
    pub queued_at: String,
}

/// Counters for the run, used by the receipt and the tray.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Stats {
    pub considered: usize,
    pub moved: usize,
    pub bytes_moved: u64,
    pub duplicates: usize,
    pub quarantined: usize,
    pub skipped: usize,
    pub failed: usize,
    pub deferred: usize,
}

/// What a content-change re-sort found.
#[derive(Debug, Clone, Default)]
pub struct RescanSummary {
    /// Files still present and hashed.
    pub checked: usize,
    /// Files whose content differs from the index.
    pub changed: Vec<String>,
    /// Files that disappeared; dropped from the index.
    pub missing: Vec<String>,
    /// Outcomes for the changed files that were re-sorted or quarantined.
    pub resorted: Vec<Outcome>,
}

/// Runtime switches that come from the CLI rather than the config file.
#[derive(Debug, Clone, Default)]
pub struct EngineOptions {
    /// Index and suggest, never move.
    pub read_only: bool,
    /// Plan and report, never move.
    pub dry_run: bool,
    /// Preview into this directory instead of the real destination.
    pub sandbox: Option<PathBuf>,
}

/// The engine. One per process; the tray and the CLI drive the same type.
pub struct Engine {
    pub config: Config,
    pub base_dir: PathBuf,
    pub dest_root: PathBuf,
    pub quarantine_root: PathBuf,
    pub duplicates_root: PathBuf,
    pub read_only: bool,
    pub dry_run: bool,
    pub sandbox: Option<PathBuf>,
    pub history: History,
    pub dedup: Deduper,
    pub classifier: Classifier,
    pub stability: StabilityChecker,
    pub quarantined: Quarantine,
    pub search_index: Index,
    pub index_path: PathBuf,
    pub embedder: Embedder,
    pub vectors: VectorStore,
    pub vectors_path: PathBuf,
    pub hooks: HookRunner,
    pub audit: Audit,
    pub stats: Stats,
}

impl Engine {
    /// Builds an engine from a loaded config plus CLI switches.
    pub fn new(loaded: &LoadedConfig, opts: EngineOptions) -> anyhow::Result<Self> {
        let config = loaded.config.clone();
        let dest_root = opts
            .sandbox
            .clone()
            .unwrap_or_else(|| loaded.destination_root());
        let quarantine_root = loaded.quarantine_root();
        let duplicates_root = loaded.base_dir.join(&config.duplicates_root);
        let dedup_path = paths::state_dir().join("dedup_registry.json");
        let index_path = paths::state_dir().join("search_index.json");
        let vectors_path = paths::state_dir().join("vectors.json");

        Ok(Self {
            classifier: Classifier::new(&config),
            history: History::new(&paths::history_path()),
            dedup: Deduper::load(&dedup_path)?,
            quarantined: Quarantine::new(&quarantine_root),
            search_index: Index::load(&index_path),
            index_path,
            embedder: Embedder::from_config(&config.embedding),
            vectors: VectorStore::load(&vectors_path),
            vectors_path,
            hooks: HookRunner::from_config(&config.hooks),
            audit: Audit::new(&paths::state_dir().join("audit.jsonl")),
            stability: StabilityChecker::default(),
            config,
            base_dir: loaded.base_dir.clone(),
            dest_root,
            quarantine_root,
            duplicates_root,
            read_only: opts.read_only,
            dry_run: opts.dry_run,
            sandbox: opts.sandbox,
            stats: Stats::default(),
        })
    }

    /// A hermetic engine rooted in `root`, for tests.
    #[doc(hidden)]
    pub fn for_test(root: &Path, config: Config) -> Self {
        let loaded = LoadedConfig {
            config,
            path: None,
            base_dir: root.to_path_buf(),
        };
        Self {
            classifier: Classifier::new(&loaded.config),
            history: History::new(&root.join("sorta_history.jsonl")),
            dedup: Deduper::in_memory(),
            quarantined: Quarantine::new(&root.join("Quarantine")),
            search_index: Index::new(),
            index_path: root.join("search_index.json"),
            embedder: Embedder::from_config(&loaded.config.embedding),
            vectors: VectorStore::new(),
            vectors_path: root.join("vectors.json"),
            hooks: HookRunner::from_config(&loaded.config.hooks),
            audit: Audit::new(&root.join("audit.jsonl")),
            stability: StabilityChecker::instant(),
            config: loaded.config.clone(),
            base_dir: root.to_path_buf(),
            dest_root: root.join("Sorted"),
            quarantine_root: root.join("Quarantine"),
            duplicates_root: root.join("_Duplicates"),
            read_only: false,
            dry_run: false,
            sandbox: None,
            stats: Stats::default(),
        }
    }

    /// Flushes the duplicate registry and the search index.
    pub fn persist(&mut self) -> anyhow::Result<()> {
        self.dedup.save()?;
        self.search_index.save(&self.index_path)?;
        self.vectors.save(&self.vectors_path)?;
        Ok(())
    }

    /// Sorts one file.
    pub fn process(&mut self, path: &Path) -> Outcome {
        self.stats.considered += 1;

        let Some(os_name) = path.file_name() else {
            return self.finish(Outcome::new(path, Status::Skipped, "not a file path"));
        };
        let name = os_name.to_string_lossy().to_string();

        // 1. Never touch OS bookkeeping or files still being written.
        if detect::is_system_noise(&name) {
            return self.finish(Outcome::new(path, Status::Skipped, "system noise"));
        }
        if safety::is_ignored(&name, &self.config.ignore_list()) {
            return self.finish(Outcome::new(
                path,
                Status::Skipped,
                "matched ignore pattern",
            ));
        }

        let metadata = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(e) => return self.fail(path, &e),
        };
        if metadata.is_dir() {
            return self.finish(Outcome::new(path, Status::Skipped, "directory"));
        }
        if metadata.len() == 0 {
            // An empty file carries no signal at all; filing it would be a guess.
            return self.finish(Outcome::new(path, Status::Skipped, "empty file"));
        }
        let size = metadata.len();

        // 2. Wait for the writer to finish.
        if let Err(e) = self.stability.wait_until_stable(path) {
            return self.finish(Outcome::new(
                path,
                Status::Deferred,
                &format!("never settled: {e}"),
            ));
        }

        // 3. What is it?
        let detected = match detect::detect(path) {
            Ok(d) => d,
            Err(e) => return self.fail(path, &e),
        };

        // 4. Have we seen these bytes before?
        let content_hash = match hash::hash_file(path) {
            Ok(h) => h,
            Err(e) => return self.fail(path, &e),
        };
        if let Some(existing) = self
            .dedup
            .lookup(&content_hash)
            .map(|e| e.canonical_path.clone())
        {
            self.stats.duplicates += 1;

            match self.config.on_duplicate {
                DupPolicy::Skip => {
                    return self.duplicate_outcome(path, &content_hash, &existing, false);
                }
                DupPolicy::Move => {
                    return self.duplicate_outcome(path, &content_hash, &existing, true);
                }
                DupPolicy::KeepNewest => {
                    let known = PathBuf::from(&existing);
                    if is_newer_than(path, &known) {
                        // This copy wins: retire the old one and keep sorting this file.
                        if known.exists() {
                            let _ = self.move_to_duplicates(&known);
                        }
                        self.dedup.forget_path(&known);
                    } else {
                        return self.duplicate_outcome(path, &content_hash, &existing, true);
                    }
                }
            }
        }

        // 5. Text, when a backend exists for this format.
        let extracted = extract::extract_text(path, &detected);

        // 5b. A vector, when a local engine is configured.
        let embedding = self.embed_text(extracted.as_deref());
        let current_path = path.display().to_string();
        let near_duplicate = embedding.as_ref().and_then(|vector| {
            self.vectors.nearest(
                vector,
                self.embedder.threshold(),
                Some(current_path.as_str()),
            )
        });

        // 6. Where does it belong?
        let year = chrono::Local::now().format("%Y").to_string();
        let mut verdict = self
            .classifier
            .classify(&detected, &name, extracted.as_deref(), &year);
        if let Some((other, score)) = &near_duplicate {
            verdict.signals.push(format!("near_dup:{score:.2}"));
            verdict.signals.push(format!("near_dup_of:{other}"));
        }

        // 7. Is the answer good enough to act on?
        if verdict.confidence < self.config.min_confidence {
            return self.quarantine_file(
                path,
                &name,
                &detected,
                &verdict,
                size,
                QuarantineReason::LowConfidence,
                None,
            );
        }

        // 7b. Near-duplicate handling, when the engine said so.
        if let Some((other, score)) = &near_duplicate {
            if self.config.embedding.near_duplicate_action == NearDuplicateAction::Quarantine {
                return self.quarantine_file(
                    path,
                    &name,
                    &detected,
                    &verdict,
                    size,
                    QuarantineReason::PossibleDuplicate,
                    Some(format!("possible duplicate of {other} (cosine {score:.2})")),
                );
            }
        }

        // 8. Plan the move.
        let dest_dir = self.dest_root.join(&verdict.dest_rel);
        let safe_name = safety::sanitize_filename(&name);
        let resolution =
            match safety::resolve_collision(&dest_dir, &safe_name, self.config.on_collision) {
                Ok(r) => r,
                Err(e) => return self.fail(path, &e),
            };

        // Captured before the match consumes `resolution`.
        let replacing = matches!(resolution, Resolution::Replace(_));
        let final_name = match resolution {
            Resolution::Free(n) | Resolution::Renamed(n) | Resolution::Replace(n) => n,
            Resolution::Skip => {
                let mut outcome =
                    Outcome::new(path, Status::Skipped, "destination name already taken");
                outcome.rule = verdict.rule_id.clone();
                outcome.confidence = verdict.confidence;
                self.log(&outcome, &content_hash);
                return self.finish(outcome);
            }
        };
        let dest_path = dest_dir.join(&final_name);

        // A file that is already where it belongs is not a collision.
        if dest_path == path {
            let mut outcome = Outcome::new(path, Status::Skipped, "already in place");
            outcome.rule = verdict.rule_id.clone();
            outcome.confidence = verdict.confidence;
            self.log(&outcome, &content_hash);
            return self.finish(outcome);
        }

        let mut outcome = Outcome {
            src: path.display().to_string(),
            dst: Some(dest_path.display().to_string()),
            rule: verdict.rule_id.clone(),
            confidence: verdict.confidence,
            signals: verdict.signals.clone(),
            status: Status::Planned,
            note: verdict.explain(),
        };

        // 9. Stop here if this is a preview.
        if self.dry_run || self.read_only {
            outcome.note = format!(
                "{}{}",
                if self.dry_run {
                    "dry run: "
                } else {
                    "read-only: "
                },
                outcome.note
            );
            self.log(&outcome, &content_hash);
            return self.finish(outcome);
        }

        // 10. Commit.
        if replacing && dest_path.exists() {
            if let Err(e) = std::fs::remove_file(&dest_path) {
                return self.fail(path, &e);
            }
        }
        if self.config.backup_before_move {
            let backup_root = self.base_dir.join(&self.config.backup_root);
            let _ = safety::backup_file(path, &backup_root);
        }

        match move_with_retry(path, &dest_path, 5) {
            Ok(()) => {
                self.stats.moved += 1;
                self.stats.bytes_moved += size;
                self.dedup.record(&content_hash, &dest_path, size);
                outcome.status = Status::Moved;
                if let Some(vector) = embedding {
                    self.vectors.upsert(VectorEntry {
                        path: dest_path.display().to_string(),
                        hash: content_hash.as_str().to_string(),
                        dim: vector.len(),
                        values: vector,
                        at: chrono::Utc::now().timestamp(),
                    });
                }
                self.record_in_index(
                    &dest_path,
                    &name,
                    &detected,
                    extracted.as_deref(),
                    size,
                    &content_hash,
                );
                if self.config.audit_enabled {
                    let _ = self.audit.append(
                        &outcome.src,
                        &dest_path.display().to_string(),
                        &outcome.rule,
                        content_hash.as_str(),
                    );
                }
                if self.hooks.is_enabled() {
                    let context = HookContext {
                        path: dest_path.display().to_string(),
                        src: outcome.src.clone(),
                        rule: outcome.rule.clone(),
                        confidence: outcome.confidence,
                    };
                    let started = self.hooks.run(&context);
                    outcome.signals.push(format!(
                        "hook:{}",
                        if started { "started" } else { "failed" }
                    ));
                }
                if self.config.sidecar_enabled {
                    let _ = self.write_sidecar(&outcome, &content_hash);
                }
                self.log(&outcome, &content_hash);
                self.finish(outcome)
            }
            Err(e) if is_lock_error(&e) => {
                let deferred = self.defer(path, &e);
                self.log(&deferred, &content_hash);
                self.finish(deferred)
            }
            Err(e) => self.fail(path, &e),
        }
    }

    /// Runs the pipeline over many files, in order.
    pub fn process_all(&mut self, files: &[PathBuf]) -> Vec<Outcome> {
        let mut outcomes = Vec::with_capacity(files.len());
        for file in files {
            outcomes.push(self.process(file));
        }
        outcomes
    }

    /// Embeds text when a local engine is configured and available.
    ///
    /// A missing engine, a failing engine and a short document all collapse to
    /// `None`: semantic features are an extra, never a dependency.
    fn embed_text(&self, text: Option<&str>) -> Option<Vec<f32>> {
        if !self.embedder.is_available() {
            return None;
        }
        let body = text?;
        if body.trim().chars().count() < 16 {
            // Too short to be a reliable signal; embedders produce noise here.
            return None;
        }
        self.embedder.embed(body)
    }

    /// Adds a sorted file to the search index (name, category, text head).
    fn record_in_index(
        &mut self,
        dest: &Path,
        name: &str,
        detected: &detect::Detected,
        text: Option<&str>,
        size: u64,
        hash: &ContentHash,
    ) {
        let doc = SearchDoc {
            path: dest.display().to_string(),
            name: name.to_string(),
            text: text.unwrap_or("").chars().take(TEXT_CAP).collect(),
            size,
            mtime: chrono::Utc::now().timestamp(),
            kind: detected.kind.category().to_string(),
            hash: hash.as_str().to_string(),
        };
        self.search_index.upsert(doc);
    }

    /// Re-sorts files whose content changed since they were indexed.
    ///
    /// This is the diff-aware half of the spec: an invoice that was a draft when it
    /// was filed and is a signed contract now belongs somewhere else. Files that
    /// vanished are dropped from the index; files whose bytes are unchanged are
    /// left completely alone.
    pub fn rescan(&mut self) -> RescanSummary {
        let known: Vec<SearchDoc> = self.search_index.docs.clone();
        let mut summary = RescanSummary::default();

        for doc in known {
            let path = PathBuf::from(&doc.path);
            if !path.exists() {
                self.search_index.remove(&doc.path);
                self.vectors.remove(&doc.path);
                summary.missing.push(doc.path.clone());
                continue;
            }

            summary.checked += 1;
            let Ok(current) = hash::hash_file(&path) else {
                continue;
            };

            if doc.hash.is_empty() {
                // Indexed before digests were stored: record one, do not re-sort.
                self.refresh_index_entry(&path, &current);
                continue;
            }
            if doc.hash == current.as_str() {
                continue;
            }

            summary.changed.push(doc.path.clone());
            let outcome = self.process(&path);
            if matches!(outcome.status, Status::Moved | Status::Quarantined) {
                summary.resorted.push(outcome);
            }
        }

        summary
    }

    /// Updates the digest and timestamp of an existing index entry.
    fn refresh_index_entry(&mut self, path: &Path, hash: &ContentHash) {
        let shown = path.display().to_string();
        let mtime = chrono::Utc::now().timestamp();
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        if let Some(doc) = self.search_index.docs.iter_mut().find(|d| d.path == shown) {
            doc.hash = hash.as_str().to_string();
            doc.mtime = mtime;
            doc.size = size;
        }
    }

    /// Writes `<file>.sorta.json` beside a sorted file.
    fn write_sidecar(&self, outcome: &Outcome, hash: &ContentHash) -> Option<PathBuf> {
        let dst = outcome.dst.as_ref()?;
        let path = PathBuf::from(format!("{dst}.sorta.json"));
        let sidecar = Sidecar {
            src: outcome.src.clone(),
            dst: dst.clone(),
            rule: outcome.rule.clone(),
            confidence: outcome.confidence,
            signals: outcome.signals.clone(),
            sorted_at: chrono::Utc::now().to_rfc3339(),
            hash: hash.as_str().to_string(),
            version: crate::VERSION.to_string(),
        };
        let text = serde_json::to_string_pretty(&sidecar).ok()?;
        std::fs::write(&path, text).ok()?;
        Some(path)
    }

    /// Builds the outcome for a byte-identical file, optionally moving it aside.
    fn duplicate_outcome(
        &mut self,
        path: &Path,
        hash: &ContentHash,
        existing: &str,
        move_aside: bool,
    ) -> Outcome {
        let mut outcome = Outcome::new(
            path,
            Status::Duplicate,
            &format!("byte-identical to {existing}"),
        );
        outcome.rule = "dedupe:exact".to_string();
        outcome.confidence = 1.0;
        outcome.signals = vec![format!("blake3:{}", hash.as_str())];

        if move_aside && !self.dry_run && !self.read_only {
            if let Some(dest) = self.move_to_duplicates(path) {
                outcome.note = format!("{}; moved to {}", outcome.note, dest.display());
                outcome.dst = Some(dest.display().to_string());
            }
        }

        self.log(&outcome, hash);
        self.finish(outcome)
    }

    /// Moves a file into `duplicates_root`, collision-safe. Never deletes.
    fn move_to_duplicates(&self, src: &Path) -> Option<PathBuf> {
        let name = src.file_name()?.to_string_lossy().to_string();
        let dir = self.duplicates_root.clone();
        let safe = safety::sanitize_filename(&name);
        let final_name = match safety::resolve_collision(&dir, &safe, CollisionPolicy::Rename) {
            Ok(Resolution::Free(n)) | Ok(Resolution::Renamed(n)) | Ok(Resolution::Replace(n)) => n,
            _ => return None,
        };
        let dest = dir.join(final_name);
        move_with_retry(src, &dest, 2).ok().map(|()| dest)
    }

    fn finish(&mut self, outcome: Outcome) -> Outcome {
        match outcome.status {
            Status::Skipped => self.stats.skipped += 1,
            Status::Failed => self.stats.failed += 1,
            Status::Deferred => self.stats.deferred += 1,
            Status::Quarantined => self.stats.quarantined += 1,
            Status::Duplicate => {}
            Status::Moved | Status::Planned => {}
        }
        outcome
    }

    fn fail(&mut self, path: &Path, err: &io::Error) -> Outcome {
        let classified = SortaError::from_io(err, path);
        let mut outcome = Outcome::new(path, Status::Failed, &classified.to_string());
        outcome.rule = classified.code().to_string();
        self.stats.failed += 1;
        outcome
    }

    fn defer(&mut self, path: &Path, err: &io::Error) -> Outcome {
        let mut outcome = Outcome::new(
            path,
            Status::Deferred,
            &format!("locked after retries: {err}"),
        );
        outcome.rule = SortaError::LockedWin {
            path: String::new(),
        }
        .code()
        .to_string();

        if !self.dry_run && !self.read_only {
            if let Some(queue) = self.write_deferred_record(path, err) {
                outcome.note = format!("locked after retries; queued in {}", queue.display());
            }
        }

        self.stats.deferred += 1;
        outcome
    }

    /// Records a deferred file in `<destination_root>/_deferred/`.
    fn write_deferred_record(&self, path: &Path, err: &io::Error) -> Option<PathBuf> {
        let dir = self.dest_root.join("_deferred");
        crate::paths::ensure_dir(&dir).ok()?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".to_string());
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let file = dir.join(format!("{stamp}-{}.json", safety::sanitize_filename(&name)));
        let record = DeferredRecord {
            path: path.display().to_string(),
            error: err.to_string(),
            queued_at: chrono::Utc::now().to_rfc3339(),
        };
        let text = serde_json::to_string_pretty(&record).ok()?;
        std::fs::write(&file, text).ok()?;
        Some(file)
    }

    // The arguments are the whole quarantine request; a struct here would only
    // move the same fields one level down.
    #[allow(clippy::too_many_arguments)]
    fn quarantine_file(
        &mut self,
        path: &Path,
        name: &str,
        detected: &detect::Detected,
        verdict: &Classified,
        size: u64,
        reason: QuarantineReason,
        detail: Option<String>,
    ) -> Outcome {
        let mut outcome = Outcome {
            src: path.display().to_string(),
            dst: Some(self.quarantine_root.display().to_string()),
            rule: verdict.rule_id.clone(),
            confidence: verdict.confidence,
            signals: verdict.signals.clone(),
            status: Status::Quarantined,
            note: detail.unwrap_or_else(|| {
                format!(
                    "confidence {:.0}% is below the {:.0}% gate",
                    verdict.confidence * 100.0,
                    self.config.min_confidence * 100.0
                )
            }),
        };
        self.stats.quarantined += 1;

        let item = QuarantineItem {
            path: path.display().to_string(),
            detected_type: detected.mime.clone(),
            best_guess: verdict.category.clone(),
            confidence: verdict.confidence,
            reason: reason.as_str().to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            state: Default::default(),
            suggested_dest: verdict.dest_rel.clone(),
        };

        if !self.dry_run && !self.read_only {
            // The registry is written even if the move fails, so the file is
            // never lost track of.
            let _ = self.quarantined.add(item);
            let dest_dir = self.quarantine_root.clone();
            if let Ok(Resolution::Free(n))
            | Ok(Resolution::Renamed(n))
            | Ok(Resolution::Replace(n)) = safety::resolve_collision(
                &dest_dir,
                &safety::sanitize_filename(name),
                CollisionPolicy::Rename,
            ) {
                let dest = dest_dir.join(n);
                if move_with_retry(path, &dest, 2).is_ok() {
                    outcome.dst = Some(dest.display().to_string());
                    self.stats.bytes_moved += size;
                }
            }
        } else {
            outcome.note = format!("would quarantine: {}", outcome.note);
        }

        self.log(&outcome, &ContentHash(String::new()));
        outcome
    }

    fn log(&self, outcome: &Outcome, hash: &ContentHash) {
        let entry = HistoryEntry {
            ts: chrono::Utc::now().to_rfc3339(),
            src: outcome.src.clone(),
            dst: outcome.dst.clone().unwrap_or_default(),
            rule: outcome.rule.clone(),
            confidence: outcome.confidence,
            signals: outcome.signals.clone(),
            instance: self.config.instance_name.clone(),
            hash: hash.as_str().to_string(),
            status: match outcome.status {
                Status::Moved => "committed".to_string(),
                Status::Planned => "planned".to_string(),
                Status::Skipped => "skipped".to_string(),
                Status::Duplicate => "duplicate".to_string(),
                Status::Quarantined => "quarantined".to_string(),
                Status::Deferred => "deferred".to_string(),
                Status::Failed => "failed".to_string(),
            },
        };
        // A log failure must never abort a sort; the file still moved.
        let _ = self.history.append(&entry);
    }
}

/// True when `a` was modified later than `b`. Unreadable timestamps lose.
fn is_newer_than(a: &Path, b: &Path) -> bool {
    let (Ok(a_meta), Ok(b_meta)) = (std::fs::metadata(a), std::fs::metadata(b)) else {
        return false;
    };
    let (Ok(a_time), Ok(b_time)) = (a_meta.modified(), b_meta.modified()) else {
        return false;
    };
    a_time > b_time
}

/// True when an `io::Error` is a Windows sharing/lock violation.
fn is_lock_error(err: &io::Error) -> bool {
    #[cfg(windows)]
    {
        matches!(err.raw_os_error(), Some(32) | Some(33))
    }
    #[cfg(not(windows))]
    {
        let _ = err;
        false
    }
}

/// Moves a file, retrying lock violations with exponential backoff.
///
/// 1s → 2s → 4s → 8s → 16s, five attempts, then the caller defers the file.
/// The sleeps only happen on a genuine lock error, never on the happy path.
pub fn move_with_retry(src: &Path, dst: &Path, max_attempts: usize) -> io::Result<()> {
    let mut delay = Duration::from_secs(1);
    let mut last: Option<io::Error> = None;

    for attempt in 0..max_attempts.max(1) {
        match move_with_verify(src, dst) {
            Ok(()) => return Ok(()),
            Err(e) => {
                if !is_lock_error(&e) {
                    return Err(e);
                }
                last = Some(e);
                if attempt + 1 < max_attempts {
                    std::thread::sleep(delay);
                    delay *= 2;
                }
            }
        }
    }

    Err(last.unwrap_or_else(|| io::Error::other("move failed")))
}

/// Moves a file and verifies the byte count survived.
///
/// `rename` is atomic within a filesystem; across filesystems it fails, and the
/// fallback is copy-then-remove. Either way the size is checked at the
/// destination, and a mismatch moves the file back.
pub fn move_with_verify(src: &Path, dst: &Path) -> io::Result<()> {
    // Windows needs the `\\?\` prefix past 260 characters; identity elsewhere.
    let src_os = safety::long_path(src);
    let dst_os = safety::long_path(dst);

    if let Some(parent) = dst_os.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let expected = std::fs::metadata(&src_os)?.len();

    match std::fs::rename(&src_os, &dst_os) {
        Ok(()) => {}
        Err(_) => {
            std::fs::copy(&src_os, &dst_os)?;
            std::fs::remove_file(&src_os)?;
        }
    }

    let actual = std::fs::metadata(&dst_os)?.len();
    if actual != expected {
        let _ = std::fs::rename(&dst_os, &src_os);
        return Err(io::Error::other(format!(
            "size mismatch after move ({} -> {} bytes); rolled back",
            expected, actual
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine(root: &Path) -> Engine {
        let config = Config {
            destination_root: root.join("Sorted").display().to_string(),
            quarantine_root: root.join("Quarantine").display().to_string(),
            ..Config::default()
        };
        Engine::for_test(root, config)
    }

    #[test]
    fn sorts_a_pdf_into_the_year_and_category() {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = engine(dir.path());
        let src = dir.path().join("Acme Invoice 11.pdf");
        std::fs::write(&src, b"%PDF-1.4 fake invoice body").unwrap();

        let outcome = engine.process(&src);
        assert_eq!(outcome.status, Status::Moved, "{}", outcome.note);
        let dst = outcome.dst.unwrap();
        assert!(dst.contains("Sorted"));
        assert!(dst.contains("PDF-Invoices"));
        assert!(!src.exists());
        assert!(Path::new(&dst).exists());
        assert_eq!(engine.stats.moved, 1);
    }

    #[test]
    fn dry_run_moves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = engine(dir.path());
        engine.dry_run = true;
        let src = dir.path().join("notes.txt");
        std::fs::write(&src, b"plain text notes").unwrap();

        let outcome = engine.process(&src);
        assert_eq!(outcome.status, Status::Planned);
        assert!(src.exists());
        assert!(!dir.path().join("Sorted").exists());
    }

    #[test]
    fn ignore_patterns_win() {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = engine(dir.path());
        let src = dir.path().join("ubuntu.iso.crdownload");
        std::fs::write(&src, b"partial").unwrap();
        let outcome = engine.process(&src);
        assert_eq!(outcome.status, Status::Skipped);
        assert!(src.exists());
    }

    #[test]
    fn duplicates_are_never_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = engine(dir.path());
        let a = dir.path().join("a invoice.pdf");
        let b = dir.path().join("b invoice.pdf");
        std::fs::write(&a, b"identical bytes").unwrap();
        std::fs::write(&b, b"identical bytes").unwrap();

        let first = engine.process(&a);
        assert_eq!(first.status, Status::Moved);

        let second = engine.process(&b);
        assert_eq!(second.status, Status::Duplicate);
        assert!(b.exists(), "a duplicate must never be deleted");
    }

    #[test]
    fn duplicates_can_be_moved_aside() {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = engine(dir.path());
        engine.config.on_duplicate = DupPolicy::Move;

        let a = dir.path().join("a invoice.pdf");
        let b = dir.path().join("b invoice.pdf");
        std::fs::write(&a, b"identical bytes").unwrap();
        std::fs::write(&b, b"identical bytes").unwrap();

        assert_eq!(engine.process(&a).status, Status::Moved);
        let second = engine.process(&b);

        assert_eq!(second.status, Status::Duplicate);
        assert!(!b.exists(), "the copy should have been moved aside");
        let dst = second.dst.expect("duplicate should report its new home");
        assert!(dst.contains("_Duplicates"));
        assert!(Path::new(&dst).exists());
    }

    #[test]
    fn keep_newest_retires_the_older_copy() {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = engine(dir.path());
        engine.config.on_duplicate = DupPolicy::KeepNewest;

        let a = dir.path().join("a invoice.pdf");
        let b = dir.path().join("b invoice.pdf");
        std::fs::write(&a, b"identical bytes").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&b, b"identical bytes").unwrap();

        assert_eq!(engine.process(&a).status, Status::Moved);
        let second = engine.process(&b);

        // b is newer, so it is filed and the old copy is retired, not deleted.
        assert_eq!(second.status, Status::Moved, "{}", second.note);
        let retired: Vec<_> = std::fs::read_dir(dir.path().join("_Duplicates"))
            .expect("_Duplicates should exist")
            .collect();
        assert_eq!(retired.len(), 1);
    }

    #[test]
    fn deferred_files_are_recorded_in_the_queue() {
        let dir = tempfile::tempdir().unwrap();
        let engine = engine(dir.path());
        let err = io::Error::other("sharing violation");

        let record = engine
            .write_deferred_record(Path::new("/tmp/locked.pdf"), &err)
            .expect("a deferred record should be written");

        assert!(record.exists());
        assert!(record.to_string_lossy().contains("_deferred"));
        let text = std::fs::read_to_string(&record).unwrap();
        assert!(text.contains("/tmp/locked.pdf"));
        assert!(text.contains("sharing violation"));
    }

    #[test]
    fn a_sorted_file_lands_in_the_search_index() {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = engine(dir.path());
        let src = dir.path().join("notes.txt");
        std::fs::write(&src, b"remember the milk before the invoice").unwrap();

        let outcome = engine.process(&src);
        assert_eq!(outcome.status, Status::Moved);
        assert_eq!(engine.search_index.len(), 1);
        assert_eq!(engine.search_index.search("milk", 5).len(), 1);

        engine.persist().unwrap();
        assert!(dir.path().join("search_index.json").exists());
    }

    #[cfg(unix)]
    #[test]
    fn audit_and_hooks_fire_after_a_move() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("hook ran");

        let config = Config {
            audit_enabled: true,
            hooks: crate::config::HooksConfig {
                on_sort: format!("printf '%s' {{path}} > '{}'", marker.display()),
            },
            ..Config::default()
        };
        let mut engine = Engine::for_test(dir.path(), config);

        let src = dir.path().join("invoice acme.pdf");
        std::fs::write(&src, b"%PDF-1.4 invoice body").unwrap();
        let outcome = engine.process(&src);

        assert_eq!(outcome.status, Status::Moved, "{}", outcome.note);
        assert!(
            outcome.signals.iter().any(|s| s == "hook:started"),
            "expected a hook signal, got {:?}",
            outcome.signals
        );

        let report = engine.audit.verify();
        assert!(report.ok, "{}", report.detail);
        assert_eq!(report.entries, 1);

        // The hook is spawned, not awaited, so wait for its side effect.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline && !marker.exists() {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(marker.exists(), "the hook should have written the marker");
    }

    #[cfg(unix)]
    #[test]
    fn near_duplicates_are_suggested_then_quarantined() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();

        // A fake engine that always returns the same vector, so every document
        // looks similar to the first one filed.
        let script = dir.path().join("fake-embed.sh");
        let mut file = std::fs::File::create(&script).unwrap();
        writeln!(file, "#!/bin/sh").unwrap();
        writeln!(file, "echo '[0.5, 0.5, 0.5, 0.5]'").unwrap();
        drop(file);
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut config = Config::default();
        config.embedding.engine = crate::config::EmbeddingEngine::Command;
        config.embedding.command = script.to_string_lossy().to_string();
        config.embedding.dimensions = 4;

        let mut engine = Engine::for_test(dir.path(), config);

        // First file: nothing to compare against, so it is filed and embedded.
        let first = dir.path().join("invoice one.txt");
        std::fs::write(&first, b"invoice from Acme Corp for November services\n").unwrap();
        assert_eq!(engine.process(&first).status, Status::Moved);
        assert_eq!(engine.vectors.len(), 1);

        // Second file: different bytes, so not an exact duplicate, but the engine
        // says it is similar.
        let second = dir.path().join("invoice two.txt");
        std::fs::write(&second, b"invoice from Acme Corp for December services\n").unwrap();
        let outcome = engine.process(&second);

        assert_eq!(outcome.status, Status::Moved, "suggest is the default");
        assert!(
            outcome.signals.iter().any(|s| s.starts_with("near_dup:")),
            "expected a near_dup signal, got {:?}",
            outcome.signals
        );

        // Now flip the policy and check the file is held back instead.
        engine.config.embedding.near_duplicate_action =
            crate::config::NearDuplicateAction::Quarantine;
        let third = dir.path().join("invoice three.txt");
        std::fs::write(&third, b"invoice from Acme Corp for January services\n").unwrap();
        let held = engine.process(&third);

        assert_eq!(held.status, Status::Quarantined);
        assert!(held.note.contains("possible duplicate"));
        let pending = engine.quarantined.pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].reason, "possible_duplicate");
    }

    #[cfg(unix)]
    #[test]
    fn without_an_engine_nothing_is_embedded() {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = engine(dir.path());
        let src = dir.path().join("invoice.txt");
        std::fs::write(&src, b"invoice from Acme Corp for November services\n").unwrap();

        assert_eq!(engine.process(&src).status, Status::Moved);
        assert!(engine.vectors.is_empty());
    }

    #[test]
    fn rescan_re_sorts_files_whose_content_changed() {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = engine(dir.path());

        let src = dir.path().join("notes.txt");
        std::fs::write(&src, b"shopping list\n").unwrap();
        let first = engine.process(&src);
        assert_eq!(first.status, Status::Moved);
        let dst = PathBuf::from(first.dst.expect("moved"));

        // Unchanged bytes: nothing to do.
        let quiet = engine.rescan();
        assert_eq!(quiet.checked, 1);
        assert!(quiet.changed.is_empty());

        // The file now says something else, so it belongs somewhere else.
        std::fs::write(&dst, b"service contract between Acme and Beta\n").unwrap();
        let summary = engine.rescan();

        assert_eq!(summary.changed.len(), 1);
        assert!(
            summary
                .resorted
                .iter()
                .any(|o| o.rule == "keyword:contract"),
            "expected the contract rule, got {:?}",
            summary.resorted
        );
    }

    #[test]
    fn rescan_drops_files_that_vanished() {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = engine(dir.path());
        let src = dir.path().join("note.txt");
        std::fs::write(&src, b"hello").unwrap();
        let dst = PathBuf::from(engine.process(&src).dst.expect("moved"));

        std::fs::remove_file(&dst).unwrap();
        let summary = engine.rescan();

        assert_eq!(summary.missing.len(), 1);
        assert_eq!(engine.search_index.len(), 0);
    }

    #[test]
    fn sidecars_are_written_when_enabled() {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = engine(dir.path());
        engine.config.sidecar_enabled = true;

        let src = dir.path().join("Acme Invoice.pdf");
        std::fs::write(&src, b"%PDF-1.4 invoice body").unwrap();

        let outcome = engine.process(&src);
        assert_eq!(outcome.status, Status::Moved);
        let dst = outcome.dst.expect("moved file has a destination");
        let sidecar = format!("{dst}.sorta.json");

        assert!(Path::new(&sidecar).exists());
        let text = std::fs::read_to_string(&sidecar).unwrap();
        assert!(text.contains("keyword:invoice"));
        assert!(text.contains("b3:"));
    }

    #[test]
    fn no_sidecar_without_opt_in() {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = engine(dir.path());
        let src = dir.path().join("Acme Invoice.pdf");
        std::fs::write(&src, b"%PDF-1.4 invoice body").unwrap();

        let outcome = engine.process(&src);
        let dst = outcome.dst.expect("moved file has a destination");
        assert!(!Path::new(&format!("{dst}.sorta.json")).exists());
    }

    #[test]
    fn unknown_files_are_quarantined_not_guessed() {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = engine(dir.path());
        let src = dir.path().join("blob.bin");
        std::fs::write(&src, [0u8, 1, 2, 3, 4, 5, 6, 7]).unwrap();

        let outcome = engine.process(&src);
        assert_eq!(outcome.status, Status::Quarantined);
        assert!(outcome.note.contains("gate"));
        assert!(!dir.path().join("Sorted").exists());
        assert!(dir
            .path()
            .join("Quarantine")
            .join("quarantine.toml")
            .exists());
    }

    #[test]
    fn collisions_are_renamed_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = engine(dir.path());
        let dest_dir = engine
            .dest_root
            .join(chrono::Local::now().format("%Y").to_string().as_str())
            .join("Text");
        std::fs::create_dir_all(&dest_dir).unwrap();
        std::fs::write(dest_dir.join("notes.txt"), b"old").unwrap();

        let src = dir.path().join("notes.txt");
        std::fs::write(&src, b"new").unwrap();

        let outcome = engine.process(&src);
        assert_eq!(outcome.status, Status::Moved);
        assert!(outcome.dst.as_deref().unwrap().contains("notes (1).txt"));
        assert_eq!(std::fs::read(dest_dir.join("notes.txt")).unwrap(), b"old");
    }

    #[test]
    fn history_records_every_decision() {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = engine(dir.path());
        let src = dir.path().join("Contract 2026.pdf");
        std::fs::write(&src, b"%PDF-1.4 contract").unwrap();
        engine.process(&src);

        let entries = engine.history.read_all();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].status, "committed");
        assert_eq!(entries[0].rule, "keyword:contract");
    }

    #[test]
    fn verify_rolls_back_on_missing_destination() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("x.txt");
        std::fs::write(&src, b"hello").unwrap();
        let dst = dir.path().join("nested/deep/x.txt");
        move_with_verify(&src, &dst).unwrap();
        assert!(dst.exists());
        assert!(!src.exists());
    }
}
