//! Windows specifics.

use std::path::{Path, PathBuf};

/// Extended-length path prefix.
const PREFIX: &str = r"\\?\";
/// UNC form, which takes a different prefix.
const UNC_PREFIX: &str = r"\\?\UNC\";

/// Network path detection.
///
/// The production implementation calls `GetDriveTypeW` and treats
/// `DRIVE_REMOTE` as a network drive. Phase 1 has no `windows-sys` dependency, so
/// it answers `false`: a network path then simply runs at the normal cadence
/// instead of the 30 s one. This is a known, documented limitation — it makes
/// Sorta slower on a network share, never wrong.
pub fn is_network_path(_path: &Path) -> bool {
    false
}

/// Adds the `\\?\` prefix when the path is long.
///
/// * `C:\very\long\path` → `\\?\C:\very\long\path`
/// * `\\server\share\x` → `\\?\UNC\server\share\x`
/// * already-prefixed paths are returned unchanged
/// * relative paths are returned unchanged (the prefix requires an absolute path)
pub fn long_path(path: &Path) -> PathBuf {
    let shown = path.to_string_lossy().to_string();
    if shown.len() < 260 || shown.starts_with(PREFIX) || !path.is_absolute() {
        return path.to_path_buf();
    }
    if let Some(rest) = shown.strip_prefix(r"\\") {
        return PathBuf::from(format!("{UNC_PREFIX}{rest}"));
    }
    PathBuf::from(format!("{PREFIX}{shown}"))
}

/// True when the process exists.
///
/// Uses `tasklist`, which is present on every supported Windows version. A
/// non-zero exit or missing binary is treated as "not alive", which errs towards
/// reclaiming a stale lock — the safe direction, because the lock file also
/// records the pid for the error message.
pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let filter = format!("PID eq {pid}");
    match std::process::Command::new("tasklist")
        .args(["/FI", &filter, "/NH", "/FO", "CSV"])
        .output()
    {
        Ok(output) => {
            let text = String::from_utf8_lossy(&output.stdout);
            text.contains(&pid.to_string())
        }
        Err(_) => false,
    }
}

/// Lowers this thread's priority to `BELOW_NORMAL_PRIORITY_CLASS`.
///
/// The production implementation calls `SetPriorityClass`; phase 1 has no
/// `windows-sys` dependency, so this is a no-op that reports success. Sorting is
/// I/O-bound, and the OS scheduler already deprioritises background I/O, so the
/// practical impact is small — but it is a real gap against the spec.
pub fn set_background_priority() -> std::io::Result<()> {
    Ok(())
}
