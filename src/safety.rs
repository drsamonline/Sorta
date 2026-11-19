//! File safety.
//!
//! Everything in this module is about not losing data. If a function here is
//! wrong, a user loses a file, so each one is written to fail closed: when in
//! doubt, keep the original name, keep the original file, and report.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::config::CollisionPolicy;

/// Characters Windows forbids in a filename (and that confuse every shell).
pub const ILLEGAL_CHARS: [char; 9] = ['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

/// Device names that Windows reserves, with or without an extension.
pub const RESERVED_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Longest comfortable path length before the Windows `\\?\` prefix is used.
pub const WINDOWS_PATH_SOFT_LIMIT: usize = 260;

/// Makes a name safe on Windows, Linux, and in a URL.
///
/// * illegal characters become `_`
/// * control characters are dropped
/// * reserved device names get a `_` prefix
/// * trailing dots and spaces are stripped (Windows silently drops them)
/// * an empty result becomes `unnamed`
pub fn sanitize_filename(name: &str) -> String {
    let mut cleaned = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_control() {
            continue;
        }
        if ILLEGAL_CHARS.contains(&ch) {
            cleaned.push('_');
        } else {
            cleaned.push(ch);
        }
    }

    let trimmed = cleaned.trim_end_matches(['.', ' ']).trim_start().to_string();
    let mut out = if trimmed.is_empty() {
        "unnamed".to_string()
    } else {
        trimmed
    };

    // A reserved device name is reserved in the stem, whatever the extension.
    let stem = out
        .split('.')
        .next()
        .unwrap_or(&out)
        .to_ascii_uppercase();
    if RESERVED_NAMES.contains(&stem.as_str()) {
        out = format!("_{out}");
    }

    out
}

/// True when the name collides with a Windows device name.
pub fn is_reserved_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    RESERVED_NAMES.contains(&stem.as_str())
}

/// True when a path is long enough to need the extended-length prefix.
pub fn needs_long_path(path: &Path) -> bool {
    path.to_string_lossy().chars().count() >= WINDOWS_PATH_SOFT_LIMIT
}

/// Adds the `\\?\` prefix on Windows when the path is long. No-op elsewhere.
pub fn long_path(path: &Path) -> PathBuf {
    crate::platform::long_path(path)
}

/// Case-insensitive glob match supporting `*` and `?`.
///
/// Deliberately not a regex: ignore lists are globs, and a regex engine here
/// would be both slower to compile and easier to get catastrophically wrong.
pub fn wildcard_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    wildcard_inner(&p, &t)
}

fn wildcard_inner(p: &[char], t: &[char]) -> bool {
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);

    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }

    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// True when the file name matches any ignore pattern.
pub fn is_ignored(name: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|p| wildcard_match(p, name))
}

/// Finds an existing entry in `dir` that equals `name` case-insensitively.
///
/// This is the NTFS trap: `Invoice.pdf` and `invoice.pdf` cannot coexist, and a
/// plain `Path::exists()` check on Linux would happily create both.
pub fn find_collision(dir: &Path, name: &str) -> io::Result<Option<PathBuf>> {
    let target = name.to_lowercase();
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(ref e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        if entry.file_name().to_string_lossy().to_lowercase() == target {
            return Ok(Some(entry.path()));
        }
    }
    Ok(None)
}

/// Splits `name` into stem and extension (extension keeps its dot).
pub fn split_name(name: &str) -> (String, String) {
    match name.rfind('.') {
        Some(idx) if idx > 0 => (name[..idx].to_string(), name[idx..].to_string()),
        _ => (name.to_string(), String::new()),
    }
}

/// Produces a free name in `dir`: `report (1).pdf`, `report (2).pdf`, ...
pub fn uniquify(dir: &Path, name: &str) -> io::Result<String> {
    if find_collision(dir, name)?.is_none() {
        return Ok(name.to_string());
    }
    let (stem, ext) = split_name(name);
    for n in 1..10_000 {
        let candidate = format!("{stem} ({n}){ext}");
        if find_collision(dir, &candidate)?.is_none() {
            return Ok(candidate);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("could not find a free name for {name} in {}", dir.display()),
    ))
}

/// Timestamp suffix used by `keep_both`.
fn stamp() -> String {
    chrono::Local::now().format("%Y%m%d-%H%M%S").to_string()
}

/// Result of resolving a destination name against an existing file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Use this name; nothing was in the way.
    Free(String),
    /// Use this name; something was in the way and this is the disambiguated one.
    Renamed(String),
    /// Do not touch anything; the policy says skip.
    Skip,
    /// Remove the existing file, then use this name.
    Replace(String),
}

/// Applies the collision policy to a proposed destination name.
pub fn resolve_collision(
    dir: &Path,
    name: &str,
    policy: CollisionPolicy,
) -> io::Result<Resolution> {
    let existing = find_collision(dir, name)?;
    let Some(existing_path) = existing else {
        return Ok(Resolution::Free(name.to_string()));
    };
    // Same file (case-only difference on a case-sensitive volume): nothing to do.
    if existing_path.file_name().map(|n| n.to_string_lossy().to_string())
        == Some(name.to_string())
    {
        return Ok(Resolution::Free(name.to_string()));
    }

    match policy {
        CollisionPolicy::Skip => Ok(Resolution::Skip),
        CollisionPolicy::Replace => Ok(Resolution::Replace(name.to_string())),
        CollisionPolicy::Rename => Ok(Resolution::Renamed(uniquify(dir, name)?)),
        CollisionPolicy::KeepBoth => {
            let (stem, ext) = split_name(name);
            let candidate = format!("{stem}_{}{ext}", stamp());
            Ok(Resolution::Renamed(uniquify(dir, &candidate)?))
        }
    }
}

/// Waits until a file stops changing.
///
/// A file that is still being written (a browser download, an export from an
/// editor) must never be moved. The check is: sample size + mtime, require two
/// consecutive identical samples. A file that keeps changing past `max_attempts`
/// is reported as an error and left alone.
#[derive(Debug, Clone)]
pub struct StabilityChecker {
    /// Delay between samples.
    pub interval: Duration,
    /// Consecutive identical samples required.
    pub required_stable_reads: usize,
    /// Give up after this many samples.
    pub max_attempts: usize,
}

impl Default for StabilityChecker {
    fn default() -> Self {
        Self {
            interval: Duration::from_millis(500),
            required_stable_reads: 2,
            max_attempts: 20,
        }
    }
}

impl StabilityChecker {
    /// For tests: sample with no sleeping.
    pub fn instant() -> Self {
        Self {
            interval: Duration::from_millis(0),
            required_stable_reads: 2,
            max_attempts: 3,
        }
    }

    /// Polls until the file's size and mtime hold still.
    pub fn wait_until_stable(&self, path: &Path) -> io::Result<()> {
        let mut previous: Option<(u64, Option<SystemTime>)> = None;
        let mut stable = 0usize;

        for _ in 0..self.max_attempts {
            let meta = fs::metadata(path)?;
            let current = (meta.len(), meta.modified().ok());

            if previous == Some(current) {
                stable += 1;
                if stable + 1 >= self.required_stable_reads {
                    return Ok(());
                }
            } else {
                stable = 0;
            }
            previous = Some(current);

            if !self.interval.is_zero() {
                std::thread::sleep(self.interval);
            }
        }

        Err(io::Error::new(
            io::ErrorKind::Other,
            format!(
                "file never settled after {} samples: {}",
                self.max_attempts,
                path.display()
            ),
        ))
    }
}

/// True when the file is read-only or in a directory we cannot write to.
pub fn is_writable_dir(dir: &Path) -> bool {
    let probe = dir.join(".sorta_write_probe");
    match fs::write(&probe, b"") {
        Ok(()) => {
            let _ = fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Copies a file into a backup directory, preserving the name.
pub fn backup_file(src: &Path, backup_root: &Path) -> io::Result<PathBuf> {
    let name = src
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    crate::paths::ensure_dir(backup_root)?;
    let dest_name = uniquify(backup_root, &sanitize_filename(&name))?;
    let dest = backup_root.join(dest_name);
    fs::copy(src, &dest)?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn illegal_characters_are_replaced() {
        assert_eq!(sanitize_filename("in<valid>:name.pdf"), "in_valid__name.pdf");
        assert_eq!(sanitize_filename("a|b?c*d.pdf"), "a_b_c_d.pdf");
    }

    #[test]
    fn reserved_device_names_are_escaped() {
        assert_eq!(sanitize_filename("CON.txt"), "_CON.txt");
        assert_eq!(sanitize_filename("nul"), "_nul");
        assert!(is_reserved_name("COM1.log"));
        assert!(!is_reserved_name("com10.log"));
    }

    #[test]
    fn trailing_dots_and_spaces_are_stripped() {
        assert_eq!(sanitize_filename("report.pdf.  "), "report.pdf");
        assert_eq!(sanitize_filename("   "), "unnamed");
    }

    #[test]
    fn wildcards_behave_like_globs() {
        assert!(wildcard_match("*.tmp", "x.tmp"));
        assert!(wildcard_match("~$*", "~$report.docx"));
        assert!(wildcard_match("*.crdownload", "Ubuntu.ISO.crdownload"));
        assert!(!wildcard_match("*.tmp", "x.tmpx"));
        assert!(wildcard_match("data?.csv", "data1.csv"));
        assert!(wildcard_match("*.PDF", "scan.pdf"));
    }

    #[test]
    fn ignore_list_catches_downloads_in_flight() {
        let patterns = vec!["*.crdownload".to_string(), "*.part".to_string()];
        assert!(is_ignored("big.iso.crdownload", &patterns));
        assert!(!is_ignored("big.iso", &patterns));
    }

    #[test]
    fn collisions_are_case_insensitive() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Invoice.PDF"), b"x").unwrap();

        let hit = find_collision(dir.path(), "invoice.pdf").unwrap();
        assert!(hit.is_some());

        match resolve_collision(dir.path(), "invoice.pdf", CollisionPolicy::Rename).unwrap() {
            Resolution::Renamed(name) => assert_eq!(name, "invoice (1).pdf"),
            other => panic!("expected rename, got {other:?}"),
        }
        match resolve_collision(dir.path(), "invoice.pdf", CollisionPolicy::Skip).unwrap() {
            Resolution::Skip => {}
            other => panic!("expected skip, got {other:?}"),
        }
    }

    #[test]
    fn free_names_pass_through() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve_collision(dir.path(), "new.pdf", CollisionPolicy::Rename).unwrap(),
            Resolution::Free("new.pdf".to_string())
        );
    }

    #[test]
    fn stability_accepts_a_settled_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settled.txt");
        fs::write(&path, b"done").unwrap();
        StabilityChecker::instant()
            .wait_until_stable(&path)
            .expect("settled file should be stable");
    }

    #[test]
    fn long_paths_are_detected() {
        let long = format!("/tmp/{}/file.pdf", "a".repeat(300));
        assert!(needs_long_path(Path::new(&long)));
        assert!(!needs_long_path(Path::new("/tmp/short.pdf")));
    }
}
