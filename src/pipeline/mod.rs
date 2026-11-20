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

use serde::{Deserialize, Serialize};

use crate::audit::Audit;
use crate::config::{CollisionPolicy, Config, DupPolicy, LoadedConfig, NearDuplicateAction};
use crate::dedupe::Deduper;
use crate::detect;
use crate::embedding::Embedder;
use crate::errors::SortaError;
use crate::extract;
use crate::hash::{self, ContentHash};
use crate::history::{History, HistoryEntry};
use crate::hooks::{HookContext, HookRunner};
use crate::instance::DestinationLock;
use crate::paths;
use crate::quarantine::{Quarantine, QuarantineItem, QuarantineReason};
use crate::rate::RateLimiter;
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
    pub limiter: RateLimiter,
    /// File whose presence pauses this engine (`~/.sorta/paused`).
    pub pause_flag: PathBuf,
    /// Sorta's state directory; the vault key lives here.
    pub state_dir: PathBuf,
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
            limiter: RateLimiter::new(config.max_files_per_second),
            pause_flag: paths::state_dir().join("paused"),
            state_dir: paths::state_dir(),
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
            limiter: RateLimiter::new(loaded.config.max_files_per_second),
            pause_flag: root.join("paused"),
            state_dir: root.to_path_buf(),
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

        // A paused Sorta looks at nothing: every file is deferred, not skipped, so a
        // bulk scan can be resumed without losing its place.
        if self.pause_flag.exists() {
            let mut outcome = Outcome::new(path, Status::Deferred, "Sorta is paused");
            outcome.rule = "instance:paused".to_string();
            return self.finish(outcome);
        }

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

        // Throttle, then take the folder lock when multi-instance is on.
        self.limiter.acquire();
        let _destination_lock = if self.config.multi_instance {
            match DestinationLock::acquire(&dest_dir) {
                Ok(lock) => Some(lock),
                Err(e) => {
                    let mut outcome = Outcome::new(path, Status::Deferred, &e.to_string());
                    outcome.rule = "instance:destination_busy".to_string();
                    self.log(&outcome, &content_hash);
                    return self.finish(outcome);
                }
            }
        } else {
            None
        };

        match move_with_retry(path, &dest_path, 5) {
            Ok(()) => {
                self.stats.moved += 1;
                self.stats.bytes_moved += size;
                self.dedup.record(&content_hash, &dest_path, size);
                outcome.status = Status::Moved;

                // A file the rules sent to `_Sensitive/` is encrypted before it is
                // indexed, so its text never reaches the search index or the vectors.
                let mut locked_path: Option<PathBuf> = None;
                if self.config.vault.auto_lock && self.is_vault_path(&dest_path) {
                    match self.lock_in_place(&dest_path) {
                        Ok(path) => {
                            outcome.signals.push("vault:locked".to_string());
                            outcome.dst = Some(path.display().to_string());
                            locked_path = Some(path);
                        }
                        Err(error) => {
                            // The move succeeded; a failed lock must not undo it.
                            outcome.signals.push(format!("vault:failed:{error}"));
                        }
                    }
                }

                let indexed_path = locked_path.clone().unwrap_or_else(|| dest_path.clone());
                if let Some(vector) = embedding {
                    if locked_path.is_none() {
                        self.vectors.upsert(VectorEntry {
                            path: indexed_path.display().to_string(),
                            hash: content_hash.as_str().to_string(),
                            dim: vector.len(),
                            values: vector,
                            at: chrono::Utc::now().timestamp(),
                        });
                    }
                }
                let index_text = if locked_path.is_some() {
                    None
                } else {
                    extracted.as_deref()
                };
                self.record_in_index(
                    &indexed_path,
                    &name,
                    &detected,
                    index_text,
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

    /// True when a destination path contains the vault folder.
    ///
    /// Component matching, not a prefix test: the folder template usually puts a year
    /// in front (`2026/_Sensitive`), so the vault directory is rarely the first
    /// segment.
    fn is_vault_path(&self, dest: &Path) -> bool {
        dest.components()
            .any(|component| component.as_os_str() == crate::vault::VAULT_DIR)
    }

    /// Encrypts a filed document in place, verifying before it removes anything.
    ///
    /// The plaintext is read once, encrypted, decrypted again in memory and compared
    /// with the original. Only then is the plaintext removed: a lock that loses the
    /// file is worse than no lock.
    fn lock_in_place(&self, path: &Path) -> anyhow::Result<PathBuf> {
        let key = crate::vault::VaultKey::load_with(
            &self.state_dir,
            Some(self.config.vault.key_command.as_str()),
        )?;

        let plaintext = std::fs::read(path)?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".to_string());
        let locked = path.with_file_name(format!(
            "{}{}",
            crate::safety::sanitize_filename(&name),
            crate::vault::LOCKED_SUFFIX
        ));

        crate::vault::encrypt_file(&key, path, &locked)?;

        let payload = std::fs::read(&locked)?;
        let check = crate::vault::decrypt_bytes(&key, &payload)?;
        if check != plaintext {
            let _ = std::fs::remove_file(&locked);
            anyhow::bail!("the locked copy did not decrypt to the original; plaintext kept");
        }

        std::fs::remove_file(path)?;
        Ok(locked)
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

mod moves;
#[cfg(test)]
mod tests;

use moves::{is_lock_error, is_newer_than};
pub use moves::{move_with_retry, move_with_verify};
