//! Command-line interface.
//!
//! The CLI is the phase-1 face of the engine: the tray, the dropbox and the chat
//! console all call the same [`crate::pipeline::Engine`]. Argument parsing uses
//! `argh` rather than `clap` (~340 KB smaller, per the spec's size budget).

use std::path::{Path, PathBuf};

use crate::config::{Config, LoadedConfig};
use crate::digest;
use crate::history::{parse_date, parse_meaning, History};
use crate::instance::InstanceLock;
use crate::llm::LlmRouter;
use crate::paths;
use crate::pipeline::{Engine, EngineOptions, Outcome, Status};
use crate::platform;
use crate::quarantine::Quarantine;
use crate::receipt::Receipt;
use crate::retention;
use crate::scan;
use crate::search::Index;
use crate::sort::{catalogue, is_available};
use crate::{banner, extract};

/// Sorta — ultra-light semantic document sorter.
#[derive(argh::FromArgs, Debug)]
pub struct Cli {
    /// index and suggest only; never move a file
    #[argh(switch, short = 'r')]
    pub read_only: bool,

    /// plan and report; never move a file
    #[argh(switch)]
    pub dry_run: bool,

    /// preview into ~/.sorta/sandbox/ instead of the real destination
    #[argh(switch)]
    pub sandbox: bool,

    /// path to the config file
    #[argh(option)]
    pub config: Option<String>,

    /// instance name (default: default)
    #[argh(option)]
    pub instance: Option<String>,

    /// print the version and exit
    #[argh(switch)]
    pub version: bool,

    #[argh(subcommand)]
    pub cmd: Option<Command>,
}

/// Subcommands.
#[derive(argh::FromArgs, Debug)]
#[argh(subcommand)]
pub enum Command {
    Sort(SortCmd),
    Scan(ScanCmd),
    Undo(UndoCmd),
    History(HistoryCmd),
    Why(WhyCmd),
    Status(StatusCmd),
    ConfigCmd(ConfigCmd),
    Watch(WatchCmd),
    Quarantine(QuarantineCmd),
    Rules(RulesCmd),
    Search(SearchCmd),
    Retention(RetentionCmd),
    Digest(DigestCmd),
}

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
}

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

/// Watch folders in the foreground and sort what settles.
#[derive(argh::FromArgs, Debug)]
#[argh(subcommand, name = "watch")]
pub struct WatchCmd {
    /// folders to watch (default: watch_folders from the config)
    #[argh(positional, greedy)]
    pub roots: Vec<String>,
}

/// Inspect or clear the quarantine queue.
#[derive(argh::FromArgs, Debug)]
#[argh(subcommand, name = "quarantine")]
pub struct QuarantineCmd {
    /// action and optional path: list | review <path> | resolve <path>
    #[argh(positional, greedy)]
    pub args: Vec<String>,
}

/// Show the rule catalogue and which rules this build can run.
#[derive(argh::FromArgs, Debug)]
#[argh(subcommand, name = "rules")]
pub struct RulesCmd {
    /// hide rules the current tier cannot run
    #[argh(switch)]
    pub available: bool,
}

/// Search sorted files by name and extracted text.
#[derive(argh::FromArgs, Debug)]
#[argh(subcommand, name = "search")]
pub struct SearchCmd {
    /// words to look for (all of them must match)
    #[argh(positional, greedy)]
    pub query: Vec<String>,

    /// maximum number of hits
    #[argh(option, default = "20")]
    pub limit: usize,
}

/// Show or apply the retention policy.
#[derive(argh::FromArgs, Debug)]
#[argh(subcommand, name = "retention")]
pub struct RetentionCmd {
    /// actually delete the files; without this it is a dry run
    #[argh(switch)]
    pub apply: bool,
}

/// Write a Markdown digest of recent sorts.
#[derive(argh::FromArgs, Debug)]
#[argh(subcommand, name = "digest")]
pub struct DigestCmd {
    /// how many days to cover
    #[argh(option, default = "7")]
    pub days: i64,
}

/// Parses arguments and runs the requested command. Returns a process exit code.
pub fn run() -> anyhow::Result<i32> {
    let cli: Cli = argh::from_env();

    if cli.version {
        println!("{}", banner());
        return Ok(0);
    }

    let loaded = load_config(&cli)?;

    // Matched by reference: `cli` is still needed for its global switches.
    match &cli.cmd {
        Some(Command::Sort(cmd)) => cmd_sort(&cli, &loaded, cmd),
        Some(Command::Scan(cmd)) => cmd_scan(&cli, &loaded, cmd),
        Some(Command::Undo(cmd)) => cmd_undo(&loaded, cmd),
        Some(Command::History(cmd)) => cmd_history(&loaded, cmd),
        Some(Command::Why(cmd)) => cmd_why(&loaded, cmd),
        Some(Command::Status(cmd)) => cmd_status(&cli, &loaded, cmd),
        Some(Command::ConfigCmd(cmd)) => cmd_config(&cli, &loaded, cmd),
        Some(Command::Watch(cmd)) => cmd_watch(&cli, &loaded, cmd),
        Some(Command::Quarantine(cmd)) => cmd_quarantine(&loaded, cmd),
        Some(Command::Rules(cmd)) => cmd_rules(&loaded, cmd),
        Some(Command::Search(cmd)) => cmd_search(cmd),
        Some(Command::Retention(cmd)) => cmd_retention(&loaded, cmd),
        Some(Command::Digest(cmd)) => cmd_digest(cmd),
        None => {
            println!("{}", banner());
            println!();
            println!("Run `sorta --help` for commands, or `sorta status` to see this instance.");
            Ok(0)
        }
    }
}

fn load_config(cli: &Cli) -> anyhow::Result<LoadedConfig> {
    let instance = cli
        .instance
        .clone()
        .unwrap_or_else(|| "default".to_string());
    let path = Config::discover(cli.config.as_deref().map(Path::new), &instance);
    Config::load(&path)
}

fn engine_for(cli: &Cli, loaded: &LoadedConfig) -> anyhow::Result<Engine> {
    let opts = EngineOptions {
        read_only: cli.read_only,
        dry_run: cli.dry_run,
        sandbox: if cli.sandbox {
            Some(paths::state_dir().join("sandbox"))
        } else {
            None
        },
    };
    Engine::new(loaded, opts)
}

// ---------------------------------------------------------------------------
// sort
// ---------------------------------------------------------------------------

fn cmd_sort(cli: &Cli, loaded: &LoadedConfig, cmd: &SortCmd) -> anyhow::Result<i32> {
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

fn cmd_scan(cli: &Cli, loaded: &LoadedConfig, cmd: &ScanCmd) -> anyhow::Result<i32> {
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

    Ok(exit)
}

// ---------------------------------------------------------------------------
// undo
// ---------------------------------------------------------------------------

fn cmd_undo(loaded: &LoadedConfig, cmd: &UndoCmd) -> anyhow::Result<i32> {
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

fn describe_filter(filter: &crate::history::UndoFilter) -> String {
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

fn cmd_history(_loaded: &LoadedConfig, cmd: &HistoryCmd) -> anyhow::Result<i32> {
    let history = History::new(&paths::history_path());
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

fn cmd_why(_loaded: &LoadedConfig, cmd: &WhyCmd) -> anyhow::Result<i32> {
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

fn cmd_status(cli: &Cli, loaded: &LoadedConfig, _cmd: &StatusCmd) -> anyhow::Result<i32> {
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

fn cmd_config(cli: &Cli, loaded: &LoadedConfig, cmd: &ConfigCmd) -> anyhow::Result<i32> {
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

// ---------------------------------------------------------------------------
// watch
// ---------------------------------------------------------------------------

fn cmd_watch(cli: &Cli, loaded: &LoadedConfig, cmd: &WatchCmd) -> anyhow::Result<i32> {
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
    Ok(0)
}

// ---------------------------------------------------------------------------
// quarantine
// ---------------------------------------------------------------------------

fn cmd_quarantine(loaded: &LoadedConfig, cmd: &QuarantineCmd) -> anyhow::Result<i32> {
    let queue = Quarantine::new(&loaded.quarantine_root());
    let action = cmd.args.first().map(String::as_str).unwrap_or("list");
    let target = cmd.args.get(1).map(String::as_str);

    match action {
        "list" => {
            let items = queue.pending();
            if items.is_empty() {
                println!(
                    "Quarantine is empty (registry: {}).",
                    queue.registry_path().display()
                );
                return Ok(0);
            }
            println!("{:>6}  {:<12}  {:<28}  FILE", "CONF", "GUESS", "SUGGESTED");
            for item in &items {
                println!(
                    "{:>5.0}%  {:<12}  {:<28}  {}",
                    item.confidence * 100.0,
                    item.best_guess,
                    item.suggested_dest,
                    item.path
                );
            }
            println!(
                "\n{} pending. Review one with `sorta quarantine review <path>`.",
                items.len()
            );
            Ok(0)
        }
        "review" => {
            let Some(path) = target else {
                eprintln!("sorta quarantine review: give the path of a queued file");
                return Ok(2);
            };
            if queue.mark_reviewed(path)? {
                println!("marked reviewed: {path}");
            } else {
                println!("no unreviewed entry for {path}");
            }
            Ok(0)
        }
        "resolve" => {
            let Some(path) = target else {
                eprintln!("sorta quarantine resolve: give the path of a queued file");
                return Ok(2);
            };
            if queue.resolve(path)? {
                println!("removed from quarantine: {path}");
            } else {
                println!("not in the queue: {path}");
            }
            Ok(0)
        }
        other => {
            eprintln!("sorta quarantine: unknown action '{other}' (list | review | resolve)");
            Ok(2)
        }
    }
}

// ---------------------------------------------------------------------------
// rules
// ---------------------------------------------------------------------------

fn cmd_rules(loaded: &LoadedConfig, cmd: &RulesCmd) -> anyhow::Result<i32> {
    let mut router = LlmRouter::new(&loaded.config);
    let llm_available = router.probe() != crate::llm::Tier::Zero;
    let semantic = loaded.config.semantic_enabled;

    println!(
        "{:<26} {:<9} {:<10} {:<18} MIN CONF",
        "RULE", "REQUIRES", "STATUS", "DESTINATION"
    );
    let mut shown = 0usize;
    for rule in catalogue(&loaded.config) {
        let ok = is_available(rule.requires, semantic, llm_available);
        if cmd.available && !ok {
            continue;
        }
        println!(
            "{:<26} {:<9} {:<10} {:<18} {:.0}%",
            rule.id,
            rule.requires.as_str(),
            if ok { "available" } else { "skipped" },
            rule.dest,
            rule.min_confidence * 100.0
        );
        shown += 1;
    }
    println!(
        "\n{shown} rule(s); semantic index {}, local LLM {}",
        if semantic { "on" } else { "off" },
        if llm_available { "available" } else { "absent" }
    );
    Ok(0)
}

// ---------------------------------------------------------------------------
// search
// ---------------------------------------------------------------------------

fn cmd_search(cmd: &SearchCmd) -> anyhow::Result<i32> {
    let query = cmd.query.join(" ");
    if query.trim().is_empty() {
        eprintln!("sorta search: give one or more words");
        return Ok(2);
    }

    let index = Index::load(&paths::state_dir().join("search_index.json"));
    if index.is_empty() {
        println!(
            "The search index is empty. It fills as Sorta sorts files (file name plus plain-text body)."
        );
        return Ok(0);
    }

    let hits = index.search(&query, cmd.limit);
    if hits.is_empty() {
        println!(
            "No matches for \"{query}\" in {} indexed file(s).",
            index.len()
        );
        return Ok(0);
    }

    for hit in &hits {
        println!("{:>4}  {}", hit.score, hit.path);
        if !hit.snippet.is_empty() {
            println!("      {}", hit.snippet);
        }
    }
    println!(
        "\n{} hit(s) of {} indexed file(s).",
        hits.len(),
        index.len()
    );
    Ok(0)
}

// ---------------------------------------------------------------------------
// retention
// ---------------------------------------------------------------------------

fn cmd_retention(loaded: &LoadedConfig, cmd: &RetentionCmd) -> anyhow::Result<i32> {
    let root = loaded.destination_root();
    let config = &loaded.config.retention;

    if !config.enabled || config.max_age_days == 0 {
        println!(
            "Retention is off. Enable it with `[retention] enabled = true` and a max_age_days in {}.",
            loaded
                .path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "the config".to_string())
        );
        return Ok(0);
    }

    let plan = retention::plan(&root, config, chrono::Local::now());
    if plan.is_empty() {
        println!(
            "Nothing under {} is older than {} days.",
            root.display(),
            config.max_age_days
        );
        return Ok(0);
    }

    println!(
        "{} file(s), {} total, older than {} days:",
        plan.len(),
        human_bytes(retention::total_size(&plan)),
        config.max_age_days
    );
    for candidate in plan.iter().take(50) {
        println!(
            "  {:>5}d  {:>9}  {}",
            candidate.age_days,
            candidate.human_size(),
            candidate.path.display()
        );
    }
    if plan.len() > 50 {
        println!("  ... and {} more", plan.len() - 50);
    }

    if !cmd.apply {
        println!("\nDry run. Re-run with --apply to delete these files.");
        return Ok(0);
    }

    let (count, freed) = retention::apply(&plan);
    println!("\nDeleted {count} file(s), freed {}.", human_bytes(freed));
    Ok(0)
}

// ---------------------------------------------------------------------------
// digest
// ---------------------------------------------------------------------------

fn cmd_digest(cmd: &DigestCmd) -> anyhow::Result<i32> {
    let history = History::new(&paths::history_path());
    let report = digest::build(&history.read_all(), cmd.days, chrono::Local::now());
    print!("{}", report.markdown);

    let path = report.write(&paths::state_dir())?;
    println!("\nWritten to {}", path.display());
    Ok(0)
}

// ---------------------------------------------------------------------------
// shared output
// ---------------------------------------------------------------------------

fn print_outcome(outcome: &Outcome) {
    let marker = match outcome.status {
        Status::Moved => "moved  ",
        Status::Planned => "plan   ",
        Status::Skipped => "skipped",
        Status::Duplicate => "dupe   ",
        Status::Quarantined => "review ",
        Status::Deferred => "defer  ",
        Status::Failed => "failed ",
    };
    match &outcome.dst {
        Some(dst) if !dst.is_empty() => {
            println!("{marker} {}  ->  {}", outcome.src, dst);
        }
        _ => println!("{marker} {}  ({})", outcome.src, outcome.note),
    }
    if outcome.status == Status::Failed {
        println!("         {}", outcome.note);
    }
}

fn print_summary(stats: &crate::pipeline::Stats, total: usize) {
    println!(
        "\n{total} file(s): {} moved ({}), {} duplicate, {} quarantined, {} skipped, {} deferred, {} failed",
        stats.moved,
        human_bytes(stats.bytes_moved),
        stats.duplicates,
        stats.quarantined,
        stats.skipped,
        stats.deferred,
        stats.failed
    );
}

/// Writes a receipt when anything actually moved, and flushes the registry.
fn finish_run(engine: &mut Engine, outcomes: &[Outcome]) -> anyhow::Result<()> {
    engine.persist()?;
    let changed = outcomes
        .iter()
        .any(|o| matches!(o.status, Status::Moved | Status::Quarantined));
    if !changed || engine.dry_run {
        return Ok(());
    }
    let receipt = Receipt::new(&engine.dest_root, outcomes.to_vec(), engine.dry_run);
    match receipt.write(&engine.dest_root) {
        Ok(path) => println!("\nReceipt: {}", path.display()),
        Err(e) => eprintln!("could not write receipt: {e}"),
    }
    Ok(())
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_formatting() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(999), "999 B");
        assert_eq!(human_bytes(1024), "1.0 KB");
        assert_eq!(human_bytes(1024 * 1024 * 3 / 2), "1.5 MB");
    }

    #[test]
    fn version_banner_is_single_line() {
        let banner = banner();
        assert!(banner.starts_with("sorta "));
        assert!(banner.contains(crate::VERSION));
        assert!(!banner.contains('\n'));
    }
}
