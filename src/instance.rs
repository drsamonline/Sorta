//! Single-instance handling.
//!
//! Two Sortas watching the same folder is how you get two files moved into two
//! places from one drop. The default is therefore one instance, enforced with a
//! lock file holding the owner's pid:
//!
//! * the lock is created with `create_new`, which is atomic on both platforms;
//! * a lock whose pid is dead is stale and reclaimed;
//! * `--instance <name>` gets its own lock, so deliberate multi-instance works.
//!
//! Named mutex (`Global\SortaInstance`) on Windows and `flock` on Linux are the
//! production primitives; the pid lock file is what both reduce to and is what
//! phase 1 ships.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::paths;

/// Held for the lifetime of a process.
pub struct InstanceLock {
    path: PathBuf,
    held: bool,
}

impl InstanceLock {
    /// Takes the lock for `name`, or explains who holds it.
    pub fn acquire(name: &str) -> anyhow::Result<Self> {
        let path = paths::instance_lock_path(name);
        if let Some(parent) = path.parent() {
            paths::ensure_dir(parent)?;
        }

        if path.exists() {
            let existing = fs::read_to_string(&path).unwrap_or_default();
            let pid: u32 = existing.trim().parse().unwrap_or(0);
            if pid != 0 && crate::platform::pid_alive(pid) {
                anyhow::bail!(
                    "another Sorta instance is running (pid {pid}, lock {})",
                    path.display()
                );
            }
            // Stale lock from a crash: reclaim it.
            let _ = fs::remove_file(&path);
        }

        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)?;
        writeln!(file, "{}", std::process::id())?;
        Ok(Self { path, held: true })
    }

    /// An instance that deliberately does not take a lock (tests, read-only runs).
    pub fn unheld() -> Self {
        Self {
            path: PathBuf::new(),
            held: false,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        if self.held {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_instance_is_refused() {
        // Serialise on the real lock name so the test cannot fight the suite.
        let name = format!("test-{}", std::process::id());
        let first = InstanceLock::acquire(&name).expect("first lock should succeed");
        let second = InstanceLock::acquire(&name);
        assert!(second.is_err(), "second lock must be refused");
        drop(first);

        // After release, the lock is free again.
        let third = InstanceLock::acquire(&name).expect("lock should be reclaimable");
        drop(third);
    }

    #[test]
    fn stale_locks_are_reclaimed() {
        let name = format!("stale-{}", std::process::id());
        let path = paths::instance_lock_path(&name);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // pid 0 is never alive, so this is a guaranteed-stale lock.
        std::fs::write(&path, "0\n").unwrap();
        let lock = InstanceLock::acquire(&name).expect("stale lock should be reclaimed");
        drop(lock);
    }

    #[test]
    fn unheld_instances_do_nothing() {
        let lock = InstanceLock::unheld();
        assert!(!lock.path().exists());
    }
}
