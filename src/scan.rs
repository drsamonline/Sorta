//! Initial bulk scan.
//!
//! First run over a big folder is the only operation that can take minutes, so it
//! is the only one that checkpoints. State goes to `.sorta_scan_state.json` every
//! 100 files; a crash resumes instead of restarting. `--dry-run` prints the plan
//! and writes nothing, which is the recommended way to meet a new folder.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::pipeline::{Engine, Outcome};
use crate::safety;

/// Directories that are never walked.
const SKIP_DIRS: [&str; 7] = [
    "_deferred",
    "_Duplicates",
    "_receipts",
    "_sorta_backup",
    "Quarantine",
    ".git",
    "$RECYCLE.BIN",
];

/// Crash-safe progress marker.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScanState {
    /// Root being scanned.
    pub root: String,
    /// How many files were processed.
    pub scanned: usize,
    /// Where the last checkpoint stopped.
    pub last_path: String,
    pub started_at: String,
    pub updated_at: String,
    /// True once the walk finished.
    pub complete: bool,
}

impl ScanState {
    /// Loads a checkpoint, if one exists for this root.
    pub fn load(path: &Path, root: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        let state: ScanState = serde_json::from_str(&text).ok()?;
        if state.root == root.display().to_string() {
            Some(state)
        } else {
            None
        }
    }

    /// Writes the checkpoint atomically.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            crate::paths::ensure_dir(parent)?;
        }
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

/// Summary of a bulk run.
#[derive(Debug, Clone, Default)]
pub struct ScanSummary {
    pub root: String,
    pub found: usize,
    pub outcomes: Vec<Outcome>,
    pub resumed_from: usize,
}

impl ScanSummary {
    /// Counts per status label, for the CLI summary line.
    pub fn counts(&self) -> Vec<(String, usize)> {
        let mut out: Vec<(String, usize)> = Vec::new();
        for outcome in &self.outcomes {
            let label = outcome.status.label().to_string();
            match out.iter_mut().find(|(l, _)| *l == label) {
                Some((_, n)) => *n += 1,
                None => out.push((label, 1)),
            }
        }
        out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        out
    }
}

/// Walks a tree and returns regular files, sorted for determinism.
///
/// Symlinked directories are not followed: a scan that walks into a symlink loop
/// is a scan that never finishes.
pub fn collect_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    walk(root, &mut files, 0);
    files.sort();
    files
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 64 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();

        if SKIP_DIRS.iter().any(|s| s.eq_ignore_ascii_case(&name)) {
            continue;
        }
        // Symlinked directories are skipped to avoid cycles.
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            if meta.file_type().is_symlink() {
                continue;
            }
            walk(&path, out, depth + 1);
        } else if meta.is_file() {
            out.push(path);
        }
    }
}

/// Scans a root through the engine, checkpointing every 100 files.
pub fn run(
    engine: &mut Engine,
    root: &Path,
    state_path: &Path,
    resume: bool,
) -> anyhow::Result<ScanSummary> {
    let files = collect_files(root);
    let mut summary = ScanSummary {
        root: root.display().to_string(),
        found: files.len(),
        outcomes: Vec::new(),
        resumed_from: 0,
    };

    let mut state = if resume {
        ScanState::load(state_path, root).unwrap_or_else(|| ScanState {
            root: root.display().to_string(),
            started_at: chrono::Utc::now().to_rfc3339(),
            ..Default::default()
        })
    } else {
        ScanState {
            root: root.display().to_string(),
            started_at: chrono::Utc::now().to_rfc3339(),
            ..Default::default()
        }
    };

    // Skip what a previous run already handled.
    let start_at = if resume && !state.last_path.is_empty() {
        files
            .iter()
            .position(|p| p.display().to_string() == state.last_path)
            .map(|i| i + 1)
            .unwrap_or(0)
    } else {
        0
    };
    summary.resumed_from = start_at;

    for (index, file) in files.iter().enumerate().skip(start_at) {
        summary.outcomes.push(engine.process(file));
        state.scanned += 1;
        state.last_path = file.display().to_string();
        state.updated_at = chrono::Utc::now().to_rfc3339();

        // Checkpoint every 100 files, and always at the end.
        if state.scanned % 100 == 0 || index + 1 == files.len() {
            state.save(state_path)?;
        }
    }

    state.complete = true;
    state.updated_at = chrono::Utc::now().to_rfc3339();
    state.save(state_path)?;
    Ok(summary)
}

/// True when a path looks like a network mount (polling cadence changes).
pub fn is_network_path(path: &Path) -> bool {
    crate::platform::is_network_path(path)
}

/// Refuses to scan a root that would swallow the destination tree.
pub fn validate_root(root: &Path, destination: &Path) -> anyhow::Result<()> {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let destination = destination
        .canonicalize()
        .unwrap_or_else(|_| destination.to_path_buf());
    if destination.starts_with(&root) {
        anyhow::bail!(
            "scan root {} contains the destination {}; point one of them elsewhere",
            root.display(),
            destination.display()
        );
    }
    if !safety::is_writable_dir(&destination) {
        anyhow::bail!(
            "destination {} is not writable",
            destination.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn collect_skips_internal_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.txt"), b"a").unwrap();
        std::fs::create_dir_all(root.join("_Duplicates")).unwrap();
        std::fs::write(root.join("_Duplicates").join("b.txt"), b"b").unwrap();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub").join("c.txt"), b"c").unwrap();

        let files = collect_files(root);
        let names: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert!(names.contains(&"a.txt".to_string()));
        assert!(names.contains(&"c.txt".to_string()));
        assert!(!names.contains(&"b.txt".to_string()));
    }

    #[test]
    fn scan_checkpoints_and_completes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("inbox");
        std::fs::create_dir_all(&root).unwrap();
        for i in 0..3 {
            std::fs::write(root.join(format!("note{i}.txt")), b"hello").unwrap();
        }

        let mut engine = Engine::for_test(dir.path(), Config::default());
        let state_path = dir.path().join("scan_state.json");
        let summary = run(&mut engine, &root, &state_path, false).unwrap();

        assert_eq!(summary.found, 3);
        assert_eq!(summary.outcomes.len(), 3);
        assert!(state_path.exists());

        let state = ScanState::load(&state_path, &root).unwrap();
        assert!(state.complete);
        assert_eq!(state.scanned, 3);
    }

    #[test]
    fn validate_refuses_nested_destination() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("inbox");
        std::fs::create_dir_all(&root).unwrap();
        let destination = root.join("Sorted");
        std::fs::create_dir_all(&destination).unwrap();
        assert!(validate_root(&root, &destination).is_err());
    }
}
