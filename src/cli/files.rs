//! Commands that move files: sort, scan, rescan, watch.

use super::*;
use crate::config::LoadedConfig;
use crate::instance::{self, Coordinator, InstanceLock, Registry};
use crate::quarantine::Quarantine;
use crate::paths;
use crate::pipeline::Status;
use crate::platform;
use crate::scan;
use crate::schedule;
use std::path::PathBuf;

/// Sort one or more files or folders.
#[derive(argh::FromArgs, Debug)]
#[argh(subcommand, name = "sort")]
pub struct SortCmd {
    /// files or folders to sort
    #[argh(positional, greedy)]
    pub paths: Vec<String>,
}

/// Sort every file under a folder, with crash-safe checkpointing.
#[derive(argh::FromArgs, Debug)]
#[argh(subcommand, name = "scan")]
pub struct ScanCmd {
    /// folder to scan (default: watch_folders from the config)
    #[argh(positional)]
    pub root: Option<String>,

    /// continue a previous scan instead of starting over
    #[argh(switch)]
    pub resume: bool,

    /// only run when the configured schedule says it is due
    #[argh(switch)]
    pub if_due: bool,

    /// run even when run_when_idle is set and the machine is busy
    #[argh(switch)]
    pub force: bool,
}

/// Watch folders in the foreground and sort what settles.
#[derive(argh::FromArgs, Debug)]
#[argh(subcommand, name = "watch")]
pub struct WatchCmd {
    /// folders to watch (default: watch_folders from the config)
    #[argh(positional, greedy)]
    pub roots: Vec<String>,
}

/// Re-sort files whose content changed since they were indexed.
#[derive(argh::FromArgs, Debug)]
#[argh(subcommand, name = "rescan")]
pub struct RescanCmd {}

// ---------------------------------------------------------------------------
// sort
// ---------------------------------------------------------------------------

pub(super) fn cmd_sort(cli: &Cli, loaded: &LoadedConfig, cmd: &SortCmd) -> anyhow::Result<i32> {
    if cmd.paths.is_empty() {
        eprintln!("sorta sort: no files given");
        return Ok(2);
    }

    let mut engine = engine_for(cli, loaded)?;
    let mut files: Vec<PathBuf> = Vec::new();
    for given in &cmd.paths {
        let path = PathBuf::from(given);
        if path.is_dir() {
            files.extend(scan::collect_files(&path));
        } else if path.exists() {
            files.push(path);
        } else {
            eprintln!("sorta sort: no such file or folder: {given}");
        }
    }

    if files.is_empty() {
        println!("Nothing to sort.");
        return Ok(0);
    }

    let outcomes = engine.process_all(&files);
    for outcome in &outcomes {
        print_outcome(outcome);
    }
    print_summary(&engine.stats, outcomes.len());

    finish_run(&mut engine, &outcomes)?;
    let failed = outcomes
        .iter()
        .filter(|o| o.status == Status::Failed)
        .count();
    Ok(if failed > 0 { 1 } else { 0 })
}

// ---------------------------------------------------------------------------
// scan
// ---------------------------------------------------------------------------

pub(super) fn cmd_scan(cli: &Cli, loaded: &LoadedConfig, cmd: &ScanCmd) -> anyhow::Result<i32> {
    let roots: Vec<PathBuf> = match &cmd.root {
        Some(root) => vec![PathBuf::from(root)],
        None => loaded.watch_folders(),
    };

    if roots.is_empty() {
        eprintln!(
            "sorta scan: no folder given, and watch_folders is empty in {}",
            loaded
                .path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "(built-in defaults)".to_string())
        );
        return Ok(2);
    }

    if cmd.if_due {
        if loaded.config.schedule.trim().is_empty() {
            eprintln!("sorta scan --if-due: no schedule configured");
            return Ok(2);
        }
        let cron = schedule::Cron::parse(&loaded.config.schedule)?;
        if !due_now(&cron) {
            println!("Not due (schedule '{}').", cron.expression);
            return Ok(0);
        }
    }

    if loaded.config.run_when_idle && !cmd.force && !platform::is_idle() {
        println!("Machine is busy and run_when_idle is set; skipping. Pass --force to override.");
        return Ok(0);
    }

    let mut engine = engine_for(cli, loaded)?;
    let state_path = paths::scan_state_path();
    let mut exit = 0;

    for root in roots {
        if !root.exists() {
            eprintln!("sorta scan: no such folder: {}", root.display());
            exit = 2;
            continue;
        }
        scan::validate_root(&root, &engine.dest_root)?;

        println!("Scanning {} ...", root.display());
        let summary = scan::run(&mut engine, &root, &state_path, cmd.resume)?;

        let moved = summary
            .outcomes
            .iter()
            .filter(|o| matches!(o.status, Status::Moved | Status::Planned))
            .count();
        println!(
            "Found {} files; {} would move or moved.",
            summary.found, moved
        );
        for (label, count) in summary.counts() {
            println!("  {label}: {count}");
        }

        finish_run(&mut engine, &summary.outcomes)?;
    }

    let _ = std::fs::write(last_scan_path(), chrono::Local::now().to_rfc3339());
    Ok(exit)
}

// ---------------------------------------------------------------------------
// watch
// ---------------------------------------------------------------------------

pub(super) fn cmd_watch(cli: &Cli, loaded: &LoadedConfig, cmd: &WatchCmd) -> anyhow::Result<i32> {
    let roots: Vec<PathBuf> = if cmd.roots.is_empty() {
        loaded.watch_folders()
    } else {
        cmd.roots.iter().map(PathBuf::from).collect()
    };

    if roots.is_empty() {
        eprintln!("sorta watch: no folders to watch; set watch_folders or pass a folder");
        return Ok(2);
    }

    if let Some(warning) = crate::watcher::inotify_warning() {
        eprintln!("warning: {warning}");
    }

    let _lock = if loaded.config.multi_instance {
        InstanceLock::unheld()
    } else {
        InstanceLock::acquire(&loaded.config.instance_name)?
    };

    let state_dir = paths::state_dir();
    let roots_shown: Vec<String> = roots
        .iter()
        .map(|root| root.display().to_string())
        .collect();

    // Announce ourselves, elect a coordinator, and keep the heartbeat fresh.
    let registry_path = state_dir.join("instances.json");
    let mut registry = Registry::load(&registry_path);
    registry.touch(
        &loaded.config.instance_name,
        &roots_shown,
        chrono::Utc::now().timestamp(),
    );
    registry.save(&registry_path)?;

    let coordinator = if loaded.config.multi_instance {
        Coordinator::elect(&state_dir)
    } else {
        Coordinator::follower()
    };
    if coordinator.is_coordinator() {
        println!("This instance is the coordinator.");
    }

    let heartbeat = spawn_heartbeat(
        registry_path.clone(),
        loaded.config.instance_name.clone(),
        roots_shown,
    );

    // Answer `sorta ping` while we are running. Failure is not fatal: a watch that
    // sorts files but cannot be queried is still a useful watch.
    let serve_state = state_dir.clone();
    let serve_quarantine = loaded.quarantine_root();
    let serve_index = state_dir.join("search_index.json");
    match crate::ipc::serve(&serve_state, move |request| match request {
        crate::ipc::Request::Ping => crate::ipc::Response::Pong {
            pid: std::process::id(),
        },
        crate::ipc::Request::Status => crate::ipc::Response::Status {
            pid: std::process::id(),
            paused: instance::is_paused(&serve_state),
            instances: Registry::load(&serve_state.join("instances.json")).len(),
            indexed: crate::search::Index::load(&serve_index).len(),
            quarantined: Quarantine::new(&serve_quarantine).pending().len(),
        },
    }) {
        Ok(_thread) => {}
        Err(error) => {
            eprintln!("sorta watch: no IPC channel ({error}); `sorta ping` will not answer")
        }
    }

    let mut engine = engine_for(cli, loaded)?;
    let (_handle, rx) = crate::watcher::spawn(&roots, crate::watcher::DEFAULT_QUIET_PERIOD)?;

    println!("Watching {} folder(s). Ctrl+C to stop.", roots.len());
    for path in rx {
        // Sorta's own output folders are never inputs.
        if crate::watcher::is_internal(&path) {
            continue;
        }
        let outcome = engine.process(&path);
        print_outcome(&outcome);
    }

    engine.persist()?;
    heartbeat.store(true, std::sync::atomic::Ordering::Relaxed);
    let mut registry = Registry::load(&registry_path);
    registry.forget_self();
    let _ = registry.save(&registry_path);
    Ok(0)
}

// ---------------------------------------------------------------------------
// rescan
// ---------------------------------------------------------------------------

pub(super) fn cmd_rescan(cli: &Cli, loaded: &LoadedConfig) -> anyhow::Result<i32> {
    let mut engine = engine_for(cli, loaded)?;
    let summary = engine.rescan();

    println!("Checked {} indexed file(s).", summary.checked);

    if !summary.missing.is_empty() {
        println!(
            "\n{} file(s) are gone from disk; dropped from the index:",
            summary.missing.len()
        );
        for path in &summary.missing {
            println!("  {path}");
        }
    }

    if summary.changed.is_empty() {
        println!("No content changed.");
    } else {
        println!("\n{} file(s) changed content:", summary.changed.len());
        for outcome in &summary.resorted {
            print_outcome(outcome);
        }
    }

    engine.persist()?;
    Ok(0)
}
