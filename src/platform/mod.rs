//! Platform-specific behaviour, one file per OS.
//!
//! Only three things genuinely differ between Windows and Linux in phase 1, and
//! each one is a place where getting it wrong loses a file or hangs a scan:
//!
//! * **long paths** — Windows needs `\\?\` past 260 characters
//! * **network paths** — a network mount needs slower polling and no inotify
//! * **pid liveness** — for the single-instance lock
//!
//! Windows drive-type detection (`GetDriveTypeW`) and Linux `statfs` are the
//! production primitives; phase 1 reads the mount table on Linux and treats
//! Windows local paths as local, which is the safe default (a network path is
//! simply polled at the normal cadence).

#[cfg(target_os = "windows")]
#[path = "win.rs"]
mod imp;

#[cfg(not(target_os = "windows"))]
#[path = "linux.rs"]
mod imp;

pub use imp::{is_idle, is_network_path, long_path, pid_alive, set_background_priority};

/// Human-readable platform name, used in receipts and `sorta status`.
pub fn name() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "windows"
    }
    #[cfg(target_os = "linux")]
    {
        "linux"
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        "unsupported"
    }
}

/// Sorta targets Windows and Linux only. Returns false on macOS and anything else.
pub fn is_supported() -> bool {
    cfg!(any(target_os = "windows", target_os = "linux"))
}

/// On Windows the extended-length prefix is needed past 260 characters; on Linux
/// there is no such limit.
pub fn needs_prefix(path: &std::path::Path) -> bool {
    cfg!(target_os = "windows") && path.to_string_lossy().chars().count() >= 260
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_name_is_known() {
        assert!(matches!(name(), "windows" | "linux" | "unsupported"));
    }

    #[test]
    fn short_paths_never_need_a_prefix() {
        assert!(!needs_prefix(std::path::Path::new("/tmp/a.pdf")));
    }

    #[test]
    fn current_process_is_alive() {
        assert!(pid_alive(std::process::id()));
    }

    #[test]
    fn pid_zero_is_never_alive() {
        assert!(!pid_alive(0));
    }
}
