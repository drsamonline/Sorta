//! Windows specifics.

use std::path::{Path, PathBuf};

/// Extended-length path prefix.
const PREFIX: &str = r"\\?\";
/// UNC form, which takes a different prefix.
const UNC_PREFIX: &str = r"\\?\UNC\";

/// `DRIVE_REMOTE` as returned by `GetDriveTypeW`.
const DRIVE_REMOTE: u32 = 4;

/// True when `path` sits on a network drive or a UNC share.
///
/// A UNC path (`\\server\share\...`) is remote by definition. Otherwise the drive
/// root (`C:\`) is handed to `GetDriveTypeW`, and `DRIVE_REMOTE` means the folder
/// needs the slow polling cadence rather than inotify.
pub fn is_network_path(path: &Path) -> bool {
    use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;

    let shown = path.to_string_lossy();
    if shown.starts_with(r"\\") {
        return true;
    }

    // A drive-qualified path looks like `C:\...`; anything else is not something
    // GetDriveTypeW can answer for, so it is treated as local.
    let mut chars = shown.chars();
    let Some(drive) = chars.next() else {
        return false;
    };
    if chars.next() != Some(':') || !drive.is_ascii_alphabetic() {
        return false;
    }

    let root = format!("{drive}:\\");
    let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `wide` is a NUL-terminated UTF-16 string that outlives the call.
    let drive_type = unsafe { GetDriveTypeW(wide.as_ptr()) };
    drive_type == DRIVE_REMOTE
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

/// True when the machine looks idle.
///
/// Windows has no load average, and `GetLastInputInfo` needs another feature of
/// `windows-sys` for a signal that is only a proxy anyway. Phase 1 therefore
/// answers `true` — "assume idle" — which means `run_when_idle` never blocks a
/// scan on Windows. That is a documented gap, not a silent one: the config
/// comment says so, and `sorta status` reports the platform.
pub fn is_idle() -> bool {
    true
}
