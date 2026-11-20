//! Command-line interface.
//!
//! The CLI is the phase-1 face of the engine: the tray, the dropbox and the chat
//! console all call the same [`crate::pipeline::Engine`]. Argument parsing uses
//! `argh` rather than `clap` (~340 KB smaller, per the spec's size budget).

mod files;
mod inspect;
mod tooling;

use files::*;
use inspect::*;
use tooling::*;

use crate::banner;
use crate::config::{Config, LoadedConfig};
use crate::instance::Registry;
use crate::paths;
use crate::pipeline::{Engine, EngineOptions, Outcome, Status};
use crate::receipt::Receipt;
use crate::schedule;
use std::path::{Path, PathBuf};

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
    Insights(InsightsCmd),
    TestRule(TestRuleCmd),
    Packs(PacksCmd),
    Export(ExportCmd),
    Rescan(RescanCmd),
    Schedule(ScheduleCmd),
    Embed(EmbedCmd),
    Audit(AuditCmd),
    Forget(ForgetCmd),
    Vault(VaultCmd),
    Pause(PauseCmd),
    Resume(ResumeCmd),
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
        Some(Command::Insights(_cmd)) => cmd_insights(&loaded),
        Some(Command::TestRule(cmd)) => cmd_test_rule(&loaded, cmd),
        Some(Command::Packs(cmd)) => cmd_packs(&loaded, cmd),
        Some(Command::Export(cmd)) => cmd_export(cmd),
        Some(Command::Rescan(_cmd)) => cmd_rescan(&cli, &loaded),
        Some(Command::Schedule(cmd)) => cmd_schedule(&loaded, cmd),
        Some(Command::Embed(cmd)) => cmd_embed(&loaded, cmd),
        Some(Command::Audit(cmd)) => cmd_audit(cmd),
        Some(Command::Forget(cmd)) => cmd_forget(&loaded, cmd),
        Some(Command::Vault(cmd)) => cmd_vault(&loaded, cmd),
        Some(Command::Pause(_cmd)) => cmd_pause(true),
        Some(Command::Resume(_cmd)) => cmd_pause(false),
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

/// Refreshes this instance's registry heartbeat every 30 s until `stop` is set.
fn spawn_heartbeat(
    registry_path: PathBuf,
    instance_name: String,
    roots: Vec<String>,
) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);

    std::thread::spawn(move || {
        while !flag.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_secs(30));
            if flag.load(Ordering::Relaxed) {
                break;
            }
            let mut registry = Registry::load(&registry_path);
            registry.touch(&instance_name, &roots, chrono::Utc::now().timestamp());
            let _ = registry.save(&registry_path);
        }
    });

    stop
}

fn last_scan_path() -> PathBuf {
    paths::state_dir().join("last_scan")
}

/// True when the schedule fires now and has not already run this minute.
fn due_now(cron: &schedule::Cron) -> bool {
    let now = chrono::Local::now();
    if !cron.matches(now) {
        return false;
    }
    let last = std::fs::read_to_string(last_scan_path())
        .ok()
        .and_then(|text| chrono::DateTime::parse_from_rfc3339(text.trim()).ok())
        .map(|parsed| parsed.with_timezone(&chrono::Local));

    match last {
        Some(previous) => {
            previous.format("%Y-%m-%d %H:%M").to_string()
                != now.format("%Y-%m-%d %H:%M").to_string()
        }
        None => true,
    }
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
