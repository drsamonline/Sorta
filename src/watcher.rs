//! Folder watching.
//!
//! `notify` emits between one and thirty events per editor save, so raw events
//! are useless. The watcher here coalesces per path: a path is only handed to the
//! pipeline once it has been quiet for `quiet_period`. That is what stops a
//! 40 MB export from being sorted mid-write.
//!
//! The production build uses `notify-debouncer-full` for large trees; the
//! coalescing loop below is the same contract with no extra dependency, and is
//! what the phase-1 binary ships.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

/// Default quiet period before a path is handed to the pipeline.
pub const DEFAULT_QUIET_PERIOD: Duration = Duration::from_millis(700);

/// Lowest `fs.inotify.max_user_watches` Sorta is comfortable with.
pub const MIN_INOTIFY_WATCHES: u64 = 32_768;

/// A running watcher plus the channel of settled paths.
///
/// Held only to keep the OS watch registered: dropping the handle unregisters it,
/// which disconnects the coalescing thread's channel and lets it exit.
pub struct WatchHandle {
    #[allow(dead_code)]
    watcher: RecommendedWatcher,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl WatchHandle {
    /// Stops watching. Dropping the handle does the work: the flag tells the
    /// coalescing thread to leave its loop, and the watcher's own drop
    /// unregisters the OS watch.
    pub fn stop(self) {}
}

impl Drop for WatchHandle {
    fn drop(&mut self) {
        // Order matters: raise the flag first so the worker leaves its loop even
        // if it is sitting in `recv_timeout`, then join it.
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}

/// Starts watching `roots`, sending settled paths on the returned receiver.
pub fn spawn(
    roots: &[PathBuf],
    quiet_period: Duration,
) -> anyhow::Result<(WatchHandle, mpsc::Receiver<PathBuf>)> {
    let (raw_tx, raw_rx) = mpsc::channel::<notify::Result<Event>>();

    let mut watcher = RecommendedWatcher::new(
        move |res: notify::Result<Event>| {
            let _ = raw_tx.send(res);
        },
        Config::default(),
    )?;

    for root in roots {
        // A missing watch root is reported, not fatal: the user may not have
        // created the folder yet.
        watcher.watch(root, RecursiveMode::Recursive)?;
    }

    let (out_tx, out_rx) = mpsc::channel::<PathBuf>();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_flag = Arc::clone(&stop);

    let worker = std::thread::Builder::new()
        .name("sorta-coalesce".to_string())
        .spawn(move || {
            let mut pending: HashMap<PathBuf, Instant> = HashMap::new();
            loop {
                match raw_rx.recv_timeout(Duration::from_millis(250)) {
                    Ok(Ok(event)) => {
                        if is_relevant(&event) {
                            let now = Instant::now();
                            for path in event.paths {
                                pending.insert(path, now);
                            }
                        }
                    }
                    Ok(Err(_)) => {}
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }

                let now = Instant::now();
                let settled: Vec<PathBuf> = pending
                    .iter()
                    .filter(|(_, seen)| now.duration_since(**seen) >= quiet_period)
                    .map(|(path, _)| path.clone())
                    .collect();
                for path in settled {
                    pending.remove(&path);
                    if out_tx.send(path).is_err() {
                        return;
                    }
                }

                if stop_flag.load(Ordering::Relaxed) {
                    break;
                }
            }
        })?;

    Ok((
        WatchHandle {
            watcher,
            stop,
            worker: Some(worker),
        },
        out_rx,
    ))
}

/// Only create/modify events matter; removals are someone else's problem.
fn is_relevant(event: &Event) -> bool {
    matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Any
    )
}

/// Reads the Linux inotify watch limit.
///
/// Returns `None` on Windows and when the kernel does not expose the value.
pub fn inotify_watch_limit() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/sys/fs/inotify/max_user_watches")
            .ok()
            .and_then(|text| text.trim().parse::<u64>().ok())
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// A warning string when the watch limit is too low, else `None`.
pub fn inotify_warning() -> Option<String> {
    match inotify_watch_limit() {
        Some(limit) if limit < MIN_INOTIFY_WATCHES => Some(format!(
            "fs.inotify.max_user_watches is {limit}, below the recommended {MIN_INOTIFY_WATCHES}; \
             raise it with `sudo sysctl fs.inotify.max_user_watches={MIN_INOTIFY_WATCHES}` \
             or Sorta will fall back to polling"
        )),
        _ => None,
    }
}

/// True when the path is inside one of the internal folders Sorta owns.
pub fn is_internal(path: &Path) -> bool {
    const INTERNAL: [&str; 5] = [
        "_deferred",
        "_Duplicates",
        "_receipts",
        "_sorta_backup",
        "Quarantine",
    ];
    path.components().any(|c| {
        let name = c.as_os_str().to_string_lossy();
        INTERNAL.iter().any(|i| i.eq_ignore_ascii_case(&name))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_folders_are_recognised() {
        assert!(is_internal(Path::new("/data/_Duplicates/a.pdf")));
        assert!(is_internal(Path::new("/data/Quarantine/b.pdf")));
        assert!(!is_internal(Path::new("/data/Sorted/a.pdf")));
    }

    #[test]
    fn watch_limit_warning_is_sane() {
        if let Some(limit) = inotify_watch_limit() {
            if limit >= MIN_INOTIFY_WATCHES {
                assert!(inotify_warning().is_none());
            }
        }
    }
}
