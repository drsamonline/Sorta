//! Error taxonomy.
//!
//! Every failure Sorta can produce carries a stable `E_*` code so the log, the
//! receipt and the UI can agree on what happened without parsing prose. The
//! mapping from code to handling policy is in `docs/ERROR_CODES.md`.

use std::io;
use std::path::Path;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SortaError {
    /// Windows os error 32/33 — another process holds the file open.
    #[error("E_LOCKED_WIN: file is locked by another process: {path}")]
    LockedWin { path: String },

    /// Permission denied, ACL refusal, read-only volume.
    #[error("E_PERM: permission denied: {path}")]
    Permission { path: String },

    /// The path lives on a network mount; polling cadence changes.
    #[error("E_NETWORK: network path detected: {path}")]
    Network { path: String },

    /// `fs.inotify.max_user_watches` is below the safe floor.
    #[error("E_INOTIFY_LIMIT: inotify watch limit reached")]
    InotifyLimit,

    /// Path longer than the platform's comfortable limit.
    #[error("E_LONG_PATH: path exceeds platform limits: {path}")]
    LongPath { path: String },

    /// Filename contained characters or reserved words that cannot be kept.
    #[error("E_BAD_NAME: illegal filename: {name}")]
    BadName { name: String },

    /// Two names differ only by case on a case-insensitive filesystem.
    #[error("E_CASE_COLLISION: case-insensitive collision on {path}")]
    CaseCollision { path: String },

    /// SQLite write lock held longer than the busy timeout.
    #[error("E_DB_BUSY: index database is locked")]
    DbBusy,

    /// Coordinator heartbeat lost; workers must re-elect.
    #[error("E_COORD_DEAD: coordinator heartbeat lost")]
    CoordinatorDead,

    /// A model artifact failed to load; the tier is dropped.
    #[error("E_MODEL_LOAD: model failed to load: {model}")]
    ModelLoad { model: String },

    /// Named pipe / Unix socket failure.
    #[error("E_IPC: ipc failure: {detail}")]
    Ipc { detail: String },

    /// Anything else that came from the filesystem.
    #[error("E_IO: {detail}")]
    Io { detail: String },
}

impl SortaError {
    /// Stable machine-readable code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::LockedWin { .. } => "E_LOCKED_WIN",
            Self::Permission { .. } => "E_PERM",
            Self::Network { .. } => "E_NETWORK",
            Self::InotifyLimit => "E_INOTIFY_LIMIT",
            Self::LongPath { .. } => "E_LONG_PATH",
            Self::BadName { .. } => "E_BAD_NAME",
            Self::CaseCollision { .. } => "E_CASE_COLLISION",
            Self::DbBusy => "E_DB_BUSY",
            Self::CoordinatorDead => "E_COORD_DEAD",
            Self::ModelLoad { .. } => "E_MODEL_LOAD",
            Self::Ipc { .. } => "E_IPC",
            Self::Io { .. } => "E_IO",
        }
    }

    /// Whether the operation is worth retrying after a delay.
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::LockedWin { .. } | Self::DbBusy)
    }

    /// Converts an `io::Error` into the right taxonomy entry.
    ///
    /// On Windows, os error 32 (`ERROR_SHARING_VIOLATION`) and 33
    /// (`ERROR_LOCK_VIOLATION`) are the two lock errors that the retry/backoff
    /// path handles; everything else keeps its own classification.
    pub fn from_io(err: &io::Error, path: &Path) -> Self {
        let shown = path.display().to_string();
        if let Some(code) = err.raw_os_error() {
            #[cfg(windows)]
            if code == 32 || code == 33 {
                return Self::LockedWin { path: shown };
            }
            #[cfg(not(windows))]
            let _ = code;
        }
        match err.kind() {
            io::ErrorKind::PermissionDenied => Self::Permission { path: shown },
            _ => Self::Io {
                detail: format!("{shown}: {err}"),
            },
        }
    }
}

/// Convenience alias for fallible engine operations.
pub type Result<T> = std::result::Result<T, SortaError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_stable() {
        assert_eq!(SortaError::DbBusy.code(), "E_DB_BUSY");
        assert_eq!(SortaError::InotifyLimit.code(), "E_INOTIFY_LIMIT");
    }

    #[test]
    fn permission_errors_are_classified() {
        let io_err = io::Error::from(io::ErrorKind::PermissionDenied);
        let err = SortaError::from_io(&io_err, Path::new("/tmp/x"));
        assert_eq!(err.code(), "E_PERM");
    }
}
