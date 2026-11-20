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
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

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
    fn registry_registers_and_prunes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("instances.json");
        let now = chrono::Utc::now().timestamp();

        let mut registry = Registry::default();
        registry.touch("work", &["/data/inbox".to_string()], now);
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.instances[0].instance, "work");
        assert_eq!(registry.instances[0].pid, std::process::id());
        assert!(registry.others().is_empty());

        // A dead pid is pruned.
        registry.instances.push(InstanceRecord {
            instance: "ghost".to_string(),
            pid: 0,
            started_at: "2026-01-01T00:00:00+00:00".to_string(),
            last_heartbeat: now,
            watch_roots: Vec::new(),
        });
        assert_eq!(registry.prune(now), 1);
        assert_eq!(registry.len(), 1);

        // A stale heartbeat is pruned too.
        registry.instances.push(InstanceRecord {
            instance: "stale".to_string(),
            pid: 1,
            started_at: "2026-01-01T00:00:00+00:00".to_string(),
            last_heartbeat: now - HEARTBEAT_STALE_SECONDS - 5,
            watch_roots: Vec::new(),
        });
        assert_eq!(registry.prune(now), 1);

        registry.save(&path).unwrap();
        let mut reloaded = Registry::load(&path);
        assert_eq!(reloaded.len(), 1);
        reloaded.forget_self();
        assert!(reloaded.is_empty());
    }

    #[test]
    fn only_one_instance_becomes_coordinator() {
        let dir = tempfile::tempdir().unwrap();

        let first = Coordinator::elect(dir.path());
        assert!(first.is_coordinator());
        assert_eq!(first.holder(), Some(std::process::id()));

        let second = Coordinator::elect(dir.path());
        assert!(!second.is_coordinator(), "the lock is already held");

        drop(first);
        let third = Coordinator::elect(dir.path());
        assert!(third.is_coordinator(), "the lock should be free again");
    }

    #[test]
    fn a_dead_coordinator_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(COORDINATOR_LOCK), "0\n").unwrap();

        let elected = Coordinator::elect(dir.path());
        assert!(elected.is_coordinator(), "a dead pid should be reclaimed");
        assert_eq!(elected.holder(), Some(std::process::id()));
    }

    #[test]
    fn pause_flag_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_paused(dir.path()));

        assert!(!set_paused(dir.path(), true).unwrap());
        assert!(is_paused(dir.path()));

        assert!(set_paused(dir.path(), false).unwrap(), "it was paused");
        assert!(!is_paused(dir.path()));

        // Clearing an already-clear flag is a no-op, not an error.
        assert!(!set_paused(dir.path(), false).unwrap());
    }

    #[test]
    fn destination_locks_exclude_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("Sorted/2026");

        let first = DestinationLock::acquire(&folder).expect("first lock");
        assert!(first.path().exists());

        let second = DestinationLock::acquire(&folder);
        assert!(second.is_err(), "the second lock must be refused");

        drop(first);
        let third = DestinationLock::acquire(&folder).expect("lock released");
        assert!(third.path().exists());
    }

    #[test]
    fn a_stale_destination_lock_is_reclaimed() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("Sorted");
        std::fs::create_dir_all(&folder).unwrap();

        // Create the lock, then age its mtime well past the stale window.
        let lock = DestinationLock::acquire(&folder).unwrap();
        let path = lock.path().to_path_buf();
        std::mem::forget(lock);

        let old = SystemTime::now() - Duration::from_secs(STALE_LOCK_SECONDS + 30);
        let file = fs::File::options().write(true).open(&path).unwrap();
        file.set_modified(old).unwrap();
        drop(file);

        let reclaimed = DestinationLock::acquire(&folder).expect("stale lock should be reclaimed");
        assert!(reclaimed.path().exists());
    }

    #[test]
    fn unheld_instances_do_nothing() {
        let lock = InstanceLock::unheld();
        assert!(!lock.path().exists());
    }
}

// ---------------------------------------------------------------------------
// Instance registry
// ---------------------------------------------------------------------------

/// Seconds after which a heartbeat is considered dead.
pub const HEARTBEAT_STALE_SECONDS: i64 = 90;

/// One running Sorta process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceRecord {
    pub instance: String,
    pub pid: u32,
    pub started_at: String,
    /// Unix seconds; refreshed on every heartbeat.
    pub last_heartbeat: i64,
    pub watch_roots: Vec<String>,
}

/// `~/.sorta/instances.json`: who else is running right now.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Registry {
    #[serde(default)]
    pub instances: Vec<InstanceRecord>,
}

impl Registry {
    /// Loads the registry; missing or corrupt reads as empty.
    pub fn load(path: &Path) -> Self {
        let Ok(text) = fs::read_to_string(path) else {
            return Self::default();
        };
        serde_json::from_str(&text).unwrap_or_default()
    }

    /// Writes the registry atomically.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            crate::paths::ensure_dir(parent)?;
        }
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, text)?;
        fs::rename(&tmp, path)
    }

    /// Registers or refreshes this process, after pruning dead entries.
    pub fn touch(&mut self, instance: &str, watch_roots: &[String], now: i64) {
        self.prune(now);

        let pid = std::process::id();
        match self.instances.iter_mut().find(|r| r.pid == pid) {
            Some(record) => {
                record.instance = instance.to_string();
                record.last_heartbeat = now;
                record.watch_roots = watch_roots.to_vec();
            }
            None => self.instances.push(InstanceRecord {
                instance: instance.to_string(),
                pid,
                started_at: chrono::Utc::now().to_rfc3339(),
                last_heartbeat: now,
                watch_roots: watch_roots.to_vec(),
            }),
        }
    }

    /// Drops records whose process is gone or whose heartbeat is too old.
    pub fn prune(&mut self, now: i64) -> usize {
        let before = self.instances.len();
        self.instances.retain(|record| {
            if record.pid == std::process::id() {
                return true;
            }
            crate::platform::pid_alive(record.pid)
                && now.saturating_sub(record.last_heartbeat) <= HEARTBEAT_STALE_SECONDS
        });
        before - self.instances.len()
    }

    /// Removes this process's record.
    pub fn forget_self(&mut self) {
        let pid = std::process::id();
        self.instances.retain(|record| record.pid != pid);
    }

    /// Records for other running processes.
    pub fn others(&self) -> Vec<&InstanceRecord> {
        let pid = std::process::id();
        self.instances.iter().filter(|r| r.pid != pid).collect()
    }

    pub fn len(&self) -> usize {
        self.instances.len()
    }

    pub fn is_empty(&self) -> bool {
        self.instances.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Destination locks
// ---------------------------------------------------------------------------

/// Seconds after which an abandoned destination lock is reclaimed.
pub const STALE_LOCK_SECONDS: u64 = 60;

/// Held while one process moves a file into a destination folder.
///
/// Two Sorta instances writing into the same folder can race on the same name, so
/// the folder is locked for the duration of a move. The lock is a file created with
/// `create_new` (atomic on both platforms), named after the folder, and reclaimed
/// when it is older than [`STALE_LOCK_SECONDS`] — a crashed process must not wedge
/// a folder forever.
pub struct DestinationLock {
    path: PathBuf,
    held: bool,
}

impl DestinationLock {
    /// Takes the lock for `directory`, reclaiming a stale one once.
    pub fn acquire(directory: &Path) -> anyhow::Result<Self> {
        crate::paths::ensure_dir(directory)?;
        let path = directory.join(format!(".sorta_lock_{}", short_hash(directory)));

        match fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
        {
            Ok(mut file) => {
                let _ = writeln!(file, "{}", std::process::id());
                Ok(Self { path, held: true })
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if is_stale(&path) {
                    let _ = fs::remove_file(&path);
                    return Self::acquire(directory);
                }
                anyhow::bail!(
                    "destination is busy: {} (lock {})",
                    directory.display(),
                    path.display()
                )
            }
            Err(e) => Err(e.into()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for DestinationLock {
    fn drop(&mut self) {
        if self.held {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn is_stale(path: &Path) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    match SystemTime::now().duration_since(modified) {
        Ok(age) => age > Duration::from_secs(STALE_LOCK_SECONDS),
        // A timestamp in the future means the clock moved; treat it as fresh.
        Err(_) => false,
    }
}

/// Short stable name for a folder, so the lock file is not the folder's path.
fn short_hash(directory: &Path) -> String {
    let canonical = fs::canonicalize(directory).unwrap_or_else(|_| directory.to_path_buf());
    let digest = blake3::hash(canonical.to_string_lossy().as_bytes());
    digest.to_hex().chars().take(8).collect()
}

// ---------------------------------------------------------------------------
// Coordinator election
// ---------------------------------------------------------------------------

/// Coordinator lock file name, inside the state directory.
pub const COORDINATOR_LOCK: &str = ".coordinator.lock";

/// Who is in charge when several instances run.
///
/// Election is a race with one winner: the first process to create
/// `.coordinator.lock` coordinates, and a lock whose owner is dead is reclaimed. No
/// voting, no messages — a file created with `create_new` is atomic on both
/// platforms, which is all an election over a shared directory needs.
pub struct Coordinator {
    path: PathBuf,
    held: bool,
}

impl Coordinator {
    /// Tries to become the coordinator for `state_dir`.
    pub fn elect(state_dir: &Path) -> Self {
        let path = state_dir.join(COORDINATOR_LOCK);
        let _ = crate::paths::ensure_dir(state_dir);

        if path.exists() {
            let existing = fs::read_to_string(&path).unwrap_or_default();
            let pid: u32 = existing.trim().parse().unwrap_or(0);
            if pid != 0 && crate::platform::pid_alive(pid) {
                return Self { path, held: false };
            }
            let _ = fs::remove_file(&path);
        }

        match fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
        {
            Ok(mut file) => {
                let _ = writeln!(file, "{}", std::process::id());
                Self { path, held: true }
            }
            // Lost the race between the check and the create: we are a worker.
            Err(_) => Self { path, held: false },
        }
    }

    /// A coordinator that deliberately holds nothing (read-only runs, tests).
    pub fn follower() -> Self {
        Self {
            path: PathBuf::new(),
            held: false,
        }
    }

    pub fn is_coordinator(&self) -> bool {
        self.held
    }

    /// The pid of whoever holds the lock, if anyone.
    pub fn holder(&self) -> Option<u32> {
        let text = fs::read_to_string(&self.path).ok()?;
        text.trim().parse().ok()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Coordinator {
    fn drop(&mut self) {
        if self.held {
            let _ = fs::remove_file(&self.path);
        }
    }
}

// ---------------------------------------------------------------------------
// Pause flag
// ---------------------------------------------------------------------------

/// Marks the whole process tree as paused.
///
/// A flag file rather than a message: pause has to work across processes, across
/// platforms and across a crash, and a file is the only one of those three that
/// survives all of them. The IPC transport, when it lands, will set this same file.
pub fn is_paused(state_dir: &Path) -> bool {
    state_dir.join("paused").exists()
}

/// Sets or clears the pause flag. Returns the previous state.
pub fn set_paused(state_dir: &Path, paused: bool) -> std::io::Result<bool> {
    let path = state_dir.join("paused");
    let previous = path.exists();

    if paused {
        crate::paths::ensure_dir(state_dir)?;
        fs::write(&path, chrono::Utc::now().to_rfc3339())?;
    } else if previous {
        fs::remove_file(&path)?;
    }
    Ok(previous)
}
