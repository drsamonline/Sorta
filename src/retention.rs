//! Retention policy.
//!
//! The rule from the spec: per-folder age limits, and **never** touch anything
//! unclassified or quarantined. That is enforced structurally rather than by
//! config: the walk starts only in folders the user named, and refuses to descend
//! into the folders Sorta owns or that hold unreviewed files.
//!
//! Deleting is the one irreversible thing Sorta can do, so the CLI defaults to a
//! dry run and requires `--apply`.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Local};

use crate::config::RetentionConfig;

/// Folders that are never subject to retention.
pub const PROTECTED: [&str; 6] = [
    "Quarantine",
    "Unsorted",
    "_deferred",
    "_Duplicates",
    "_receipts",
    "_sorta_backup",
];

/// One file that is past its age limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub path: PathBuf,
    pub age_days: u64,
    pub size: u64,
}

impl Candidate {
    /// `12.3 MB` style size, for the report.
    pub fn human_size(&self) -> String {
        human_bytes(self.size)
    }
}

/// Lists files older than the configured age.
///
/// An empty `folders` list means the whole destination tree (still protected).
pub fn plan(
    destination_root: &Path,
    cfg: &RetentionConfig,
    now: DateTime<Local>,
) -> Vec<Candidate> {
    if !cfg.enabled || cfg.max_age_days == 0 {
        return Vec::new();
    }

    let mut roots: Vec<PathBuf> = if cfg.folders.is_empty() {
        vec![destination_root.to_path_buf()]
    } else {
        cfg.folders
            .iter()
            .map(|f| destination_root.join(f))
            .collect()
    };
    roots.retain(|r| r.exists());

    let mut candidates = Vec::new();
    for root in roots {
        walk(&root, cfg.max_age_days, now, &mut candidates, 0);
    }
    candidates.sort_by(|a, b| b.age_days.cmp(&a.age_days).then(a.path.cmp(&b.path)));
    candidates
}

fn walk(
    dir: &Path,
    max_age_days: u32,
    now: DateTime<Local>,
    out: &mut Vec<Candidate>,
    depth: usize,
) {
    if depth > 64 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if PROTECTED.iter().any(|p| p.eq_ignore_ascii_case(&name)) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };

        if meta.is_dir() {
            walk(&path, max_age_days, now, out, depth + 1);
            continue;
        }
        let Some(age_days) = age_days(&meta, now) else {
            continue;
        };
        if age_days >= u64::from(max_age_days) {
            out.push(Candidate {
                path,
                age_days,
                size: meta.len(),
            });
        }
    }
}

fn age_days(meta: &std::fs::Metadata, now: DateTime<Local>) -> Option<u64> {
    let modified: SystemTime = meta.modified().ok()?;
    let modified: DateTime<chrono::Utc> = modified.into();
    let delta = now.signed_duration_since(modified.with_timezone(&Local));
    if delta.num_seconds() < 0 {
        // A file from the future (clock skew, a synced mtime) is not old.
        return Some(0);
    }
    u64::try_from(delta.num_days()).ok()
}

/// Gzips the planned files and removes the originals.
///
/// Returns (count, bytes before compression). A file whose `.gz` already exists is
/// skipped rather than overwritten, so a re-run is safe.
pub fn compress(candidates: &[Candidate]) -> (usize, u64) {
    let mut compressed = 0usize;
    let mut original_bytes = 0u64;

    for candidate in candidates {
        let target = PathBuf::from(format!("{}.gz", candidate.path.display()));
        if target.exists() {
            continue;
        }
        let Ok(mut input) = std::fs::File::open(&candidate.path) else {
            continue;
        };
        let Ok(output) = std::fs::File::create(&target) else {
            continue;
        };
        let mut encoder = flate2::write::GzEncoder::new(output, flate2::Compression::default());
        if std::io::copy(&mut input, &mut encoder).is_err() {
            let _ = std::fs::remove_file(&target);
            continue;
        }
        if encoder.finish().is_err() {
            let _ = std::fs::remove_file(&target);
            continue;
        }
        // Only drop the original once the archive is closed and on disk.
        if std::fs::remove_file(&candidate.path).is_err() {
            let _ = std::fs::remove_file(&target);
            continue;
        }
        compressed += 1;
        original_bytes += candidate.size;
    }

    (compressed, original_bytes)
}

/// Deletes the planned files. Returns (count, bytes freed).
/// Folder expired files are moved into, so nothing is lost by default.
pub const EXPIRED_DIR: &str = "_Expired";

/// Moves expired files into `root/_Expired/`, keeping their place in the tree.
///
/// This is what `retention --apply` does. The blueprint promises Sorta never deletes
/// a user file, and a policy that unlinks files on a flag is still a policy that
/// unlinks files: moving them is reversible with `sorta undo` or a file manager, and
/// `--delete` remains for people who really do want the space back.
pub fn archive(candidates: &[Candidate], root: &Path) -> (usize, u64) {
    let mut moved = 0usize;
    let mut bytes = 0u64;

    for candidate in candidates {
        let relative = candidate
            .path
            .strip_prefix(root)
            .map(|path| path.to_path_buf())
            .unwrap_or_else(|_| PathBuf::from(candidate.path.file_name().unwrap_or_default()));
        let target = root.join(EXPIRED_DIR).join(relative);

        if let Some(parent) = target.parent() {
            if crate::paths::ensure_dir(parent).is_err() {
                continue;
            }
        }
        if std::fs::rename(&candidate.path, &target).is_ok() {
            moved += 1;
            bytes += candidate.size;
        }
    }
    (moved, bytes)
}

pub fn apply(candidates: &[Candidate]) -> (usize, u64) {
    let mut deleted = 0usize;
    let mut freed = 0u64;
    for candidate in candidates {
        match std::fs::remove_file(&candidate.path) {
            Ok(()) => {
                deleted += 1;
                freed += candidate.size;
            }
            Err(_) => continue,
        }
    }
    (deleted, freed)
}

/// Total size of a plan.
pub fn total_size(candidates: &[Candidate]) -> u64 {
    candidates.iter().map(|c| c.size).sum()
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod archive_tests {
    use super::*;

    fn old_file(root: &Path, relative: &str, days: u64) -> Candidate {
        let path = root.join(relative);
        crate::paths::ensure_dir(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"old bytes").unwrap();
        Candidate {
            path,
            size: 9,
            age_days: days,
        }
    }

    #[test]
    fn archiving_moves_files_and_keeps_their_place() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Sorted");
        let candidate = old_file(&root, "2020/Invoices/a.pdf", 400);

        let (moved, bytes) = archive(std::slice::from_ref(&candidate), &root);
        assert_eq!(moved, 1);
        assert_eq!(bytes, 9);

        assert!(
            !candidate.path.exists(),
            "the file should have left its folder"
        );
        let archived = root.join(EXPIRED_DIR).join("2020/Invoices/a.pdf");
        assert!(
            archived.exists(),
            "it should be under _Expired, keeping the tree"
        );
        assert_eq!(std::fs::read(&archived).unwrap(), b"old bytes");
    }

    #[test]
    fn archiving_never_deletes() {
        // The promise: a file that was there is still somewhere afterwards.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Sorted");
        let candidates: Vec<Candidate> = (0..3)
            .map(|i| old_file(&root, &format!("2020/a{i}.pdf"), 400))
            .collect();

        let (moved, _) = archive(&candidates, &root);
        assert_eq!(moved, 3);

        let mut found = 0;
        for i in 0..3 {
            if root
                .join(EXPIRED_DIR)
                .join(format!("2020/a{i}.pdf"))
                .exists()
            {
                found += 1;
            }
        }
        assert_eq!(found, 3, "every file must still exist");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn cfg(days: u32) -> RetentionConfig {
        RetentionConfig {
            enabled: true,
            max_age_days: days,
            ..RetentionConfig::default()
        }
    }

    #[test]
    fn finds_only_old_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("Archive")).unwrap();
        std::fs::write(root.join("Archive/old.txt"), b"old").unwrap();
        std::fs::write(root.join("fresh.txt"), b"new").unwrap();

        // Pretend "now" is a year ahead, so both files look old.
        let future = Local::now() + Duration::days(400);
        let aged = plan(root, &cfg(365), future);
        let names: Vec<String> = aged
            .iter()
            .map(|c| c.path.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert!(names.contains(&"old.txt".to_string()));
        assert!(names.contains(&"fresh.txt".to_string()));

        // With a one-year window and a realistic now, nothing is old enough.
        let recent = plan(root, &cfg(365), Local::now());
        assert!(recent.is_empty());
    }

    #[test]
    fn protected_folders_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for protected in ["Quarantine", "Unsorted", "_Duplicates"] {
            std::fs::create_dir_all(root.join(protected)).unwrap();
            std::fs::write(root.join(protected).join("x.pdf"), b"x").unwrap();
        }
        let now = Local::now() + Duration::days(400);
        let plan = plan(root, &cfg(30), now);
        assert!(
            plan.is_empty(),
            "protected folders must never be candidates"
        );
    }

    #[test]
    fn disabled_policy_plans_nothing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"a").unwrap();
        let mut config = cfg(1);
        config.enabled = false;
        let now = Local::now() + Duration::days(400);
        assert!(plan(dir.path(), &config, now).is_empty());
    }

    #[test]
    fn compress_keeps_the_bytes_and_drops_the_original() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.log");
        let body = "line one\n".repeat(500);
        std::fs::write(&path, &body).unwrap();

        let candidates = vec![Candidate {
            path: path.clone(),
            age_days: 400,
            size: body.len() as u64,
        }];
        let (count, bytes) = compress(&candidates);

        assert_eq!(count, 1);
        assert_eq!(bytes, body.len() as u64);
        assert!(!path.exists());
        let gz = PathBuf::from(format!("{}.gz", path.display()));
        assert!(gz.exists());
        assert!(std::fs::metadata(&gz).unwrap().len() < body.len() as u64);

        // A second run must not overwrite the archive.
        let again = vec![Candidate {
            path: path.clone(),
            age_days: 400,
            size: 0,
        }];
        assert_eq!(compress(&again).0, 0);
    }

    #[test]
    fn apply_deletes_and_reports() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.txt");
        std::fs::write(&path, b"0123456789").unwrap();

        let candidates = vec![Candidate {
            path: path.clone(),
            age_days: 400,
            size: 10,
        }];
        let (count, freed) = apply(&candidates);
        assert_eq!(count, 1);
        assert_eq!(freed, 10);
        assert!(!path.exists());
        assert_eq!(total_size(&candidates), 10);
    }
}
