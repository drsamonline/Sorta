//! Platform path resolution.
//!
//! All directory discovery happens here so the rest of the crate never reads
//! environment variables directly. `SORTA_HOME` overrides the home directory,
//! which is what the tests and the portable build use.

use std::path::{Path, PathBuf};

/// The user's home directory.
pub fn home_dir() -> PathBuf {
    if let Some(h) = std::env::var_os("SORTA_HOME") {
        if !h.is_empty() {
            return PathBuf::from(h);
        }
    }
    #[cfg(windows)]
    {
        if let Some(p) = std::env::var_os("USERPROFILE") {
            if !p.is_empty() {
                return PathBuf::from(p);
            }
        }
    }
    if let Some(h) = std::env::var_os("HOME") {
        if !h.is_empty() {
            return PathBuf::from(h);
        }
    }
    PathBuf::from(".")
}

/// Per-user config directory: `~/.config/sorta` or `%APPDATA%\sorta`.
pub fn config_dir() -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            if !appdata.is_empty() {
                return PathBuf::from(appdata).join("sorta");
            }
        }
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("sorta");
        }
    }
    home_dir().join(".config").join("sorta")
}

/// Runtime state directory: always `~/.sorta` on both platforms, per the spec.
pub fn state_dir() -> PathBuf {
    home_dir().join(".sorta")
}

/// Append-only sort log.
pub fn history_path() -> PathBuf {
    state_dir().join("sorta_history.jsonl")
}

/// Bulk-scan checkpoint.
pub fn scan_state_path() -> PathBuf {
    state_dir().join(".sorta_scan_state.json")
}

/// Single-instance lock for the default instance.
pub fn instance_lock_path(name: &str) -> PathBuf {
    if name == "default" || name.is_empty() {
        state_dir().join(".lock")
    } else {
        state_dir().join(format!(".lock.{name}"))
    }
}

/// Shared search/vector index (phase 3).
pub fn index_db_path() -> PathBuf {
    state_dir().join("index.db")
}

/// Creates a directory and its parents if missing.
pub fn ensure_dir(dir: &Path) -> std::io::Result<()> {
    if dir.as_os_str().is_empty() {
        return Ok(());
    }
    std::fs::create_dir_all(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_dir_is_dot_sorta() {
        assert!(state_dir().ends_with(".sorta"));
    }

    #[test]
    fn named_instance_locks_are_distinct() {
        assert_ne!(instance_lock_path("default"), instance_lock_path("work"));
    }
}
