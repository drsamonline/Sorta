//! Linux specifics.

use std::path::{Path, PathBuf};

/// Network filesystem types that need the slower polling cadence.
const NETWORK_FS: [&str; 9] = [
    "nfs",
    "nfs4",
    "cifs",
    "smbfs",
    "smb3",
    "sshfs",
    "fuse.sshfs",
    "9p",
    "afs",
];

/// True when `path` sits on a network mount.
///
/// Reads `/proc/mounts`, takes the longest mount point that is a prefix of the
/// path, and checks its filesystem type. This is the `statfs` check without
/// pulling in `libc`.
pub fn is_network_path(path: &Path) -> bool {
    let Ok(mounts) = std::fs::read_to_string("/proc/mounts") else {
        return false;
    };
    let absolute = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());

    let mut best: Option<(usize, &str)> = None;
    for line in mounts.lines() {
        let mut fields = line.split_whitespace();
        let (Some(_device), Some(mount_point), Some(fs_type)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        // /proc/mounts escapes spaces as \040.
        let mount_point = mount_point.replace("\\040", " ");
        if absolute.starts_with(&mount_point) {
            let len = mount_point.len();
            if best.map(|(l, _)| len > l).unwrap_or(true) {
                best = Some((len, fs_type));
            }
        }
    }

    match best {
        Some((_, fs_type)) => NETWORK_FS.contains(&fs_type),
        None => false,
    }
}

/// Linux has no 260-character limit, so the path is returned unchanged.
pub fn long_path(path: &Path) -> PathBuf {
    path.to_path_buf()
}

/// True when the process exists.
pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    Path::new(&format!("/proc/{pid}")).exists()
}

/// Lowers this process's scheduling priority (`nice +10`).
///
/// Implemented by re-executing `renice` on our own pid, which avoids a `libc`
/// dependency for a one-line syscall. Failure is not an error: a slightly
/// higher-priority sorter is better than a sorter that refuses to start.
pub fn set_background_priority() -> std::io::Result<()> {
    let pid = std::process::id().to_string();
    match std::process::Command::new("renice")
        .args(["-n", "10", "-p", &pid])
        .output()
    {
        Ok(_) => Ok(()),
        Err(_) => Ok(()),
    }
}
