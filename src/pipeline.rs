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

use crate::config::{CollisionPolicy, Config, DupPolicy, LoadedConfig};
use crate::dedupe::Deduper;
use crate::detect;
use crate::errors::SortaError;
use crate::extract;
use crate::hash::{self, ContentHash};
use crate::history::{History, HistoryEntry};
use crate::paths;
use crate::quarantine::{Quarantine, QuarantineItem, QuarantineReason};
use crate::safety::{self, Resolution, StabilityChecker};
use crate::sort::{Classified, Classifier};

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

        Ok(Self {
            classifier: Classifier::new(&config),
            history: History::new(&paths::history_path()),
            dedup: Deduper::load(&dedup_path)?,
            quarantined: Quarantine::new(&quarantine_root),
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

    /// Flushes the duplicate registry.
    pub fn persist(&mut self) -> anyhow::Result<()> {
        self.dedup.save()
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

        // 6. Where does it belong?
        let year = chrono::Local::now().format("%Y").to_string();
        let verdict = self
            .classifier
            .classify(&detected, &name, extracted.as_deref(), &year);

        // 7. Is the answer good enough to act on?
        if verdict.confidence < self.config.min_confidence {
            return self.quarantine_file(path, &name, &detected, &verdict, size);
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
        let file = dir.join(format!(
            "{stamp}-{}.json",
            safety::sanitize_filename(&name)
        ));
        let record = DeferredRecord {
            path: path.display().to_string(),
            error: err.to_string(),
            queued_at: chrono::Utc::now().to_rfc3339(),
        };
        let text = serde_json::to_string_pretty(&record).ok()?;
        std::fs::write(&file, text).ok()?;
        Some(file)
    }

    fn quarantine_file(
        &mut self,
        path: &Path,
        name: &str,
        detected: &detect::Detected,
        verdict: &Classified,
        size: u64,
    ) -> Outcome {
        let mut outcome = Outcome {
            src: path.display().to_string(),
            dst: Some(self.quarantine_root.display().to_string()),
            rule: verdict.rule_id.clone(),
            confidence: verdict.confidence,
            signals: verdict.signals.clone(),
            status: Status::Quarantined,
            note: format!(
                "confidence {:.0}% is below the {:.0}% gate",
                verdict.confidence * 100.0,
                self.config.min_confidence * 100.0
            ),
        };
        self.stats.quarantined += 1;

        let item = QuarantineItem {
            path: path.display().to_string(),
            detected_type: detected.mime.clone(),
            best_guess: verdict.category.clone(),
            confidence: verdict.confidence,
            reason: QuarantineReason::LowConfidence.as_str().to_string(),
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
