//! The move itself: rename, verify, roll back, retry on a lock.
//!
//! Kept apart from the pipeline because this is the code that can lose a
//! file, and it is the only part worth reading on its own.

use std::io;
use std::path::Path;
use std::time::Duration;

use crate::safety;

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
