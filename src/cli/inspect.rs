//! Commands that answer questions about what happened: status, config, history,
//! why, undo.

use super::*;
use crate::audit::Audit;
use crate::config::{Config, LoadedConfig};
use crate::embedding::Embedder;
use crate::history::{parse_date, parse_meaning, snapshot_at, History};
use crate::instance::{Coordinator, Registry};
use crate::llm::LlmRouter;
use crate::paths;
use crate::platform;
use crate::quarantine::Quarantine;
use crate::sort::catalogue;
use crate::{banner, extract};
use std::path::{Path, PathBuf};

/// Undo previous sorts.
#[derive(argh::FromArgs, Debug)]
#[argh(subcommand, name = "undo")]
pub struct UndoCmd {
    /// undo the N most recent sorts
    #[argh(option)]
    pub last: Option<usize>,

    /// undo by description, e.g. "yesterday's PDFs"
    #[argh(option)]
    pub meaning: Option<String>,

    /// only entries at or after this date (YYYY-MM-DD)
    #[argh(option)]
    pub since: Option<String>,

    /// only entries decided by this rule, e.g. keyword:invoice
    #[argh(option)]
    pub rule: Option<String>,

    /// list what would be undone, change nothing
    #[argh(switch)]
    pub plan: bool,
}

/// Show the sort log.
#[derive(argh::FromArgs, Debug)]
#[argh(subcommand, name = "history")]
pub struct HistoryCmd {
    /// how many entries to show
    #[argh(option, default = "20")]
    pub limit: usize,

    /// show where files were on this date (YYYY-MM-DD)
    #[argh(option)]
    pub as_of: Option<String>,

    /// only show entries that moved a file
    #[argh(switch)]
    pub committed: bool,
}

/// Explain why a file is where it is.
#[derive(argh::FromArgs, Debug)]
#[argh(subcommand, name = "why")]
pub struct WhyCmd {
    /// the file to explain
    #[argh(positional)]
    pub path: String,
}

/// Show what this instance is doing.
#[derive(argh::FromArgs, Debug)]
#[argh(subcommand, name = "status")]
pub struct StatusCmd {}

/// Inspect or create the configuration.
#[derive(argh::FromArgs, Debug)]
#[argh(subcommand, name = "config")]
pub struct ConfigCmd {
    /// path | show | init (default: path)
    #[argh(positional)]
    pub action: Option<String>,
}

// ---------------------------------------------------------------------------
// undo
// ---------------------------------------------------------------------------

pub(super) fn cmd_undo(loaded: &LoadedConfig, cmd: &UndoCmd) -> anyhow::Result<i32> {
    let history = History::new(&paths::history_path());

    let mut filter = cmd
        .meaning
        .as_deref()
        .map(parse_meaning)
        .unwrap_or_default();
    if let Some(since) = &cmd.since {
        match parse_date(since) {
            Some(date) => filter.since = Some(date),
            None => {
                eprintln!("sorta undo: could not parse --since '{since}' (use YYYY-MM-DD)");
                return Ok(2);
            }
        }
    }
    if let Some(rule) = &cmd.rule {
        filter.rule = Some(rule.clone());
    }

    let filtered = cmd.meaning.is_some() || cmd.since.is_some() || cmd.rule.is_some();
    let entries = if filtered {
        if let Some(meaning) = &cmd.meaning {
            println!(
                "Interpreting \"{meaning}\" as: {}",
                describe_filter(&filter)
            );
        }
        history.matching(&filter)
    } else if let Some(n) = cmd.last {
        let mut committed = history.committed();
        let start = committed.len().saturating_sub(n);
        committed.split_off(start)
    } else {
        eprintln!("sorta undo: give --last N, --meaning \"...\", --since DATE or --rule ID");
        return Ok(2);
    };

    if entries.is_empty() {
        println!("Nothing matches; nothing to undo.");
        return Ok(0);
    }

    if cmd.plan {
        println!("Would undo {} sort(s):", entries.len());
        for entry in &entries {
            println!("  {} -> {}", entry.dst, entry.src);
        }
        return Ok(0);
    }

    let outcomes = history.undo(&entries);
    let mut failures = 0;
    for outcome in &outcomes {
        if outcome.ok {
            println!("restored {}", outcome.src);
        } else {
            failures += 1;
            println!("skipped  {} ({})", outcome.src, outcome.note);
        }
    }
    println!(
        "Undone: {} of {}. Config in use: {}",
        outcomes.len() - failures,
        outcomes.len(),
        loaded
            .path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "built-in defaults".to_string())
    );
    Ok(if failures > 0 { 1 } else { 0 })
}

pub(super) fn describe_filter(filter: &crate::history::UndoFilter) -> String {
    let mut parts = Vec::new();
    if let Some(since) = filter.since {
        parts.push(format!("since {}", since.format("%d %b %Y %H:%M")));
    }
    if let Some(until) = filter.until {
        parts.push(format!("until {}", until.format("%d %b %Y %H:%M")));
    }
    if let Some(ext) = &filter.extension {
        parts.push(format!(".{ext} files"));
    }
    if let Some(rule) = &filter.rule {
        parts.push(format!("rule {rule}"));
    }
    if let Some(limit) = filter.limit {
        parts.push(format!("at most {limit}"));
    }
    if parts.is_empty() {
        "everything (no filters understood — pass --last N to be explicit)".to_string()
    } else {
        parts.join(", ")
    }
}

// ---------------------------------------------------------------------------
// history
// ---------------------------------------------------------------------------

pub(super) fn cmd_history(_loaded: &LoadedConfig, cmd: &HistoryCmd) -> anyhow::Result<i32> {
    let history = History::new(&paths::history_path());

    if let Some(as_of) = &cmd.as_of {
        let Some(day) = parse_date(as_of) else {
            eprintln!("sorta history: could not parse --as-of '{as_of}' (use YYYY-MM-DD)");
            return Ok(2);
        };
        // The end of that day, so the date is inclusive.
        let moment = day + chrono::Duration::days(1) - chrono::Duration::seconds(1);
        let snapshot = snapshot_at(&history.read_all(), moment);
        if snapshot.is_empty() {
            println!("Nothing had been sorted by {}.", day.format("%d %b %Y"));
            return Ok(0);
        }
        println!(
            "{} file(s) as they stood on {}:",
            snapshot.len(),
            day.format("%d %b %Y")
        );
        for item in &snapshot {
            println!("  {}  ({}, from {})", item.path, item.rule, item.original);
        }
        return Ok(0);
    }

    let entries = if cmd.committed {
        history.committed()
    } else {
        history.read_all()
    };
    let start = entries.len().saturating_sub(cmd.limit);

    if entries.is_empty() {
        println!("No history yet.");
        return Ok(0);
    }

    for entry in &entries[start..] {
        println!(
            "{}  {:<10}  {:.0}%  {}  ->  {}",
            entry
                .timestamp()
                .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|| entry.ts.clone()),
            entry.status,
            entry.confidence * 100.0,
            entry.rule,
            entry.dst
        );
    }
    println!(
        "\n{} of {} entries (log: {})",
        entries.len() - start,
        entries.len(),
        history.path().display()
    );
    Ok(0)
}

// ---------------------------------------------------------------------------
// why
// ---------------------------------------------------------------------------

pub(super) fn cmd_why(_loaded: &LoadedConfig, cmd: &WhyCmd) -> anyhow::Result<i32> {
    let history = History::new(&paths::history_path());
    match history.find_by_dst(Path::new(&cmd.path)) {
        Some(entry) => {
            println!("{}", entry.explain());
            println!();
            println!("  from:  {}", entry.src);
            println!("  to:    {}", entry.dst);
            println!("  rule:  {}", entry.rule);
            println!("  hash:  {}", entry.hash);
            Ok(0)
        }
        None => {
            println!(
                "No sort recorded for {}. It may have been moved by hand, or sorted by another instance.",
                cmd.path
            );
            Ok(1)
        }
    }
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

pub(super) fn cmd_status(
    cli: &Cli,
    loaded: &LoadedConfig,
    _cmd: &StatusCmd,
) -> anyhow::Result<i32> {
    let history = History::new(&paths::history_path());
    let quarantine = Quarantine::new(&loaded.quarantine_root());
    let mut router = LlmRouter::new(&loaded.config);
    let tier = router.probe();

    println!("{}", banner());
    println!(
        "  platform      {} ({})",
        platform::name(),
        std::env::consts::ARCH
    );
    println!(
        "  config        {}",
        loaded
            .path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "built-in defaults".to_string())
    );
    println!("  instance      {}", loaded.config.instance_name);
    println!("  destination   {}", loaded.destination_root().display());
    println!("  quarantine    {}", loaded.quarantine_root().display());
    println!(
        "  min confidence {:.0}%",
        loaded.config.min_confidence * 100.0
    );
    println!("  llm           {}", router.describe());
    println!("  extraction    {}", extract::backends().join(", "));
    println!(
        "  embedding     {}",
        Embedder::from_config(&loaded.config.embedding).describe()
    );
    if !loaded.config.hooks.on_sort.trim().is_empty() {
        println!("  hook          {}", loaded.config.hooks.on_sort);
    }
    let state_dir = paths::state_dir();
    if instance::is_paused(&state_dir) {
        println!("  state         PAUSED (sorta resume to continue)");
    }
    if loaded.config.multi_instance {
        let coordinator = Coordinator::follower();
        match coordinator.holder() {
            Some(pid) => println!("  coordinator   pid {pid}"),
            None => println!("  coordinator   none elected"),
        }
    }
    let registry = Registry::load(&state_dir.join("instances.json"));
    let others = registry.others();
    if !others.is_empty() {
        let described: Vec<String> = others
            .iter()
            .map(|record| format!("{} (pid {})", record.instance, record.pid))
            .collect();
        println!("  other instances {}", described.join(", "));
    }
    if loaded.config.audit_enabled {
        let log = Audit::new(&paths::state_dir().join("audit.jsonl"));
        let report = log.verify();
        println!(
            "  audit         {} ({} entries)",
            if report.ok { "intact" } else { "BROKEN" },
            report.entries
        );
    }
    println!(
        "  rules         {} compiled",
        catalogue(&loaded.config).len()
    );
    println!("  history       {} entries", history.read_all().len());
    println!("  quarantine    {} pending", quarantine.pending().len());
    println!(
        "  read-only     {}   dry-run {}",
        if cli.read_only { "on" } else { "off" },
        if cli.dry_run { "on" } else { "off" }
    );
    if let Some(warning) = crate::watcher::inotify_warning() {
        println!("  warning       {warning}");
    }
    if tier == crate::llm::Tier::Zero {
        println!("  note          Tier 0: rules only, every core feature still works");
    }
    Ok(0)
}

// ---------------------------------------------------------------------------
// config
// ---------------------------------------------------------------------------

pub(super) fn cmd_config(cli: &Cli, loaded: &LoadedConfig, cmd: &ConfigCmd) -> anyhow::Result<i32> {
    let action = cmd.action.as_deref().unwrap_or("path");
    match action {
        "path" => {
            println!(
                "{}",
                loaded
                    .path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| Config::default_path(
                        cli.instance.as_deref().unwrap_or("default")
                    )
                    .display()
                    .to_string())
            );
            Ok(0)
        }
        "show" => {
            print!("{}", loaded.config.to_toml_string()?);
            Ok(0)
        }
        "init" => {
            let instance = cli
                .instance
                .clone()
                .unwrap_or_else(|| "default".to_string());
            let path = cli
                .config
                .as_deref()
                .map(PathBuf::from)
                .unwrap_or_else(|| Config::default_path(&instance));
            Config::write_default(&path)?;
            println!("Wrote {}", path.display());
            Ok(0)
        }
        other => {
            eprintln!("sorta config: unknown action '{other}' (path | show | init)");
            Ok(2)
        }
    }
}
