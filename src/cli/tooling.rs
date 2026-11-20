//! Everything else: rules, quarantine, search, retention, digest, insights, audit,
//! embed, schedule, packs, export, pause.

use super::*;
use crate::instance;
use crate::audit::Audit;
use crate::config::{Config, LoadedConfig};
use crate::digest;
use crate::embedding::Embedder;
use crate::history::History;
use crate::insight;
use crate::interop;
use crate::llm::LlmRouter;
use crate::packs;
use crate::paths;
use crate::quarantine::Quarantine;
use crate::retention;
use crate::schedule;
use crate::search::Index;
use crate::sort::{catalogue, conflicts, is_available, preview_name};
use std::path::PathBuf;

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

    /// check the rule set for conflicts instead of listing it
    #[argh(switch)]
    pub check: bool,
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
    /// actually act on the files; without this it is a dry run
    #[argh(switch)]
    pub apply: bool,

    /// gzip the files instead of deleting them
    #[argh(switch)]
    pub compress: bool,
}

/// Folder health, rule analytics and what sorting saved.
#[derive(argh::FromArgs, Debug)]
#[argh(subcommand, name = "insights")]
pub struct InsightsCmd {}

/// Write a Markdown digest of recent sorts.
#[derive(argh::FromArgs, Debug)]
#[argh(subcommand, name = "digest")]
pub struct DigestCmd {
    /// how many days to cover
    #[argh(option, default = "7")]
    pub days: i64,
}

/// Stop sorting until resumed.
#[derive(argh::FromArgs, Debug)]
#[argh(subcommand, name = "pause")]
pub struct PauseCmd {}

/// Resume sorting.
#[derive(argh::FromArgs, Debug)]
#[argh(subcommand, name = "resume")]
pub struct ResumeCmd {}

/// Verify or inspect the tamper-evident audit chain.
#[derive(argh::FromArgs, Debug)]
#[argh(subcommand, name = "audit")]
pub struct AuditCmd {
    /// verify (default) | show [N]
    #[argh(positional, greedy)]
    pub args: Vec<String>,
}

/// Compute an embedding with the configured local engine.
#[derive(argh::FromArgs, Debug)]
#[argh(subcommand, name = "embed")]
pub struct EmbedCmd {
    /// text to embed
    #[argh(positional, greedy)]
    pub text: Vec<String>,
}

/// Show when the configured schedule next runs.
#[derive(argh::FromArgs, Debug)]
#[argh(subcommand, name = "schedule")]
pub struct ScheduleCmd {
    /// a cron expression to explain instead of the configured one
    #[argh(positional, greedy)]
    pub expression: Vec<String>,
}

/// Show what Sorta would do with a filename.
#[derive(argh::FromArgs, Debug)]
#[argh(subcommand, name = "test-rule")]
pub struct TestRuleCmd {
    /// the filename to test
    #[argh(positional, greedy)]
    pub names: Vec<String>,
}

/// Export or import a rule bundle (`.sortapack`).
#[derive(argh::FromArgs, Debug)]
#[argh(subcommand, name = "packs")]
pub struct PacksCmd {
    /// action and target: export <name> | show <file> | import <file>
    #[argh(positional, greedy)]
    pub args: Vec<String>,

    /// write the merged config (import only)
    #[argh(switch)]
    pub apply: bool,

    /// description stored in the pack (export only)
    #[argh(option)]
    pub description: Option<String>,
}

/// Export the history log as CSV or JSON.
#[derive(argh::FromArgs, Debug)]
#[argh(subcommand, name = "export")]
pub struct ExportCmd {
    /// csv | json (default: csv)
    #[argh(option)]
    pub format: Option<String>,

    /// write to this file instead of stdout
    #[argh(option)]
    pub out: Option<String>,
}

// ---------------------------------------------------------------------------
// quarantine
// ---------------------------------------------------------------------------

pub(super) fn cmd_quarantine(loaded: &LoadedConfig, cmd: &QuarantineCmd) -> anyhow::Result<i32> {
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

pub(super) fn cmd_rules(loaded: &LoadedConfig, cmd: &RulesCmd) -> anyhow::Result<i32> {
    if cmd.check {
        let found = conflicts(&loaded.config);
        if found.is_empty() {
            println!(
                "No conflicts: {} keyword rule(s) checked.",
                loaded.config.keyword_rules.len()
            );
            return Ok(0);
        }
        println!("{} conflict(s):", found.len());
        for conflict in &found {
            println!(
                "  {:<20} {}  ({})",
                conflict.kind.label(),
                conflict.detail,
                conflict.rules.join(", ")
            );
        }
        return Ok(1);
    }

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

pub(super) fn cmd_search(cmd: &SearchCmd) -> anyhow::Result<i32> {
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

pub(super) fn cmd_retention(loaded: &LoadedConfig, cmd: &RetentionCmd) -> anyhow::Result<i32> {
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

    let compressing = cmd.compress || config.compress_instead;
    if !cmd.apply {
        println!(
            "\nDry run. Re-run with --apply to {} these files.",
            if compressing { "compress" } else { "delete" }
        );
        return Ok(0);
    }

    if compressing {
        let (count, original) = retention::compress(&plan);
        println!(
            "\nCompressed {count} file(s); {} now gzipped in place.",
            human_bytes(original)
        );
    } else {
        let (count, freed) = retention::apply(&plan);
        println!("\nDeleted {count} file(s), freed {}.", human_bytes(freed));
    }
    Ok(0)
}

// ---------------------------------------------------------------------------
// digest
// ---------------------------------------------------------------------------

pub(super) fn cmd_digest(cmd: &DigestCmd) -> anyhow::Result<i32> {
    let history = History::new(&paths::history_path());
    let report = digest::build(&history.read_all(), cmd.days, chrono::Local::now());
    print!("{}", report.markdown);

    let path = report.write(&paths::state_dir())?;
    println!("\nWritten to {}", path.display());
    Ok(0)
}

// ---------------------------------------------------------------------------
// insights
// ---------------------------------------------------------------------------

pub(super) fn cmd_insights(loaded: &LoadedConfig) -> anyhow::Result<i32> {
    let history = History::new(&paths::history_path());
    let entries = history.read_all();
    let queue = Quarantine::new(&loaded.quarantine_root());

    let health = insight::health(&entries, queue.pending().len(), chrono::Local::now());
    let rules = insight::rule_stats(&entries);
    let duplicates_root = loaded.base_dir.join(&loaded.config.duplicates_root);
    let savings = insight::savings(&duplicates_root, &entries);

    print!("{}", insight::render(&health, &rules, &savings));
    Ok(0)
}

// ---------------------------------------------------------------------------
// test-rule
// ---------------------------------------------------------------------------

pub(super) fn cmd_test_rule(loaded: &LoadedConfig, cmd: &TestRuleCmd) -> anyhow::Result<i32> {
    let name = cmd.names.join(" ");
    if name.trim().is_empty() {
        eprintln!("sorta test-rule: give a filename to test");
        return Ok(2);
    }

    let verdict = preview_name(&loaded.config, &name, None);
    let gate = loaded.config.min_confidence;

    println!("name          {name}");
    println!("rule          {}", verdict.rule_id);
    println!("category      {}", verdict.category);
    println!("destination   {}", verdict.dest_rel);
    println!(
        "confidence    {:.0}% (gate {:.0}%)",
        verdict.confidence * 100.0,
        gate * 100.0
    );
    println!(
        "would         {}",
        if verdict.confidence >= gate {
            "sort"
        } else {
            "quarantine"
        }
    );
    println!("why           {}", verdict.explain());
    Ok(0)
}

// ---------------------------------------------------------------------------
// packs
// ---------------------------------------------------------------------------

pub(super) fn cmd_packs(loaded: &LoadedConfig, cmd: &PacksCmd) -> anyhow::Result<i32> {
    let action = cmd.args.first().map(String::as_str).unwrap_or("show");
    let target = cmd.args.get(1).map(String::as_str);

    match action {
        "export" => {
            let Some(name) = target else {
                eprintln!("sorta packs export: give a pack name");
                return Ok(2);
            };
            let description = cmd.description.as_deref().unwrap_or("");
            let path = packs::export(&loaded.config, &loaded.base_dir, name, description)?;
            println!("Wrote {}", path.display());
            Ok(0)
        }
        "show" | "import" => {
            let Some(file) = target else {
                eprintln!("sorta packs {action}: give a .sortapack path");
                return Ok(2);
            };
            let pack_path = PathBuf::from(file);
            let manifest = packs::read_manifest(&pack_path)?;
            println!(
                "{} (pack v{}, {} rule(s), built by sorta {})",
                manifest.name,
                manifest.pack_version,
                manifest.keyword_rules,
                manifest.sorta_version
            );
            if !manifest.description.is_empty() {
                println!("  {}", manifest.description);
            }

            let pack_config = packs::read_config(&pack_path)?;
            let merged = packs::merge(&loaded.config, &pack_config);
            let changes = packs::describe_merge(&loaded.config, &merged);

            if changes.is_empty() {
                println!("\nNo change to this instance's behaviour.");
                return Ok(0);
            }

            println!("\nChanges:");
            for change in &changes {
                println!("  {change}");
            }

            if action == "show" || !cmd.apply {
                println!("\nDry run. Re-run with --apply to write the merged config.");
                return Ok(0);
            }

            let config_path = loaded
                .path
                .clone()
                .unwrap_or_else(|| Config::default_path(&loaded.config.instance_name));
            if config_path.exists() {
                let backup = config_path.with_extension("toml.bak");
                std::fs::copy(&config_path, &backup)?;
                println!("\nBacked up to {}", backup.display());
            }
            if let Some(parent) = config_path.parent() {
                paths::ensure_dir(parent)?;
            }
            std::fs::write(&config_path, merged.to_toml_string()?)?;
            println!("Wrote {}", config_path.display());
            Ok(0)
        }
        other => {
            eprintln!("sorta packs: unknown action '{other}' (export | show | import)");
            Ok(2)
        }
    }
}

// ---------------------------------------------------------------------------
// export
// ---------------------------------------------------------------------------

pub(super) fn cmd_export(cmd: &ExportCmd) -> anyhow::Result<i32> {
    let history = History::new(&paths::history_path());
    let entries = history.read_all();
    let format = cmd.format.as_deref().unwrap_or("csv").to_lowercase();

    let text = match format.as_str() {
        "csv" => interop::history_csv(&entries),
        "json" => interop::history_json(&entries)?,
        other => {
            eprintln!("sorta export: unknown format '{other}' (csv | json)");
            return Ok(2);
        }
    };

    let out = cmd.out.as_deref().map(PathBuf::from);
    if let Some(path) = interop::write(&text, out.as_deref())? {
        println!("Wrote {} ({} entries)", path.display(), entries.len());
    }
    Ok(0)
}

// ---------------------------------------------------------------------------
// schedule
// ---------------------------------------------------------------------------

pub(super) fn cmd_schedule(loaded: &LoadedConfig, cmd: &ScheduleCmd) -> anyhow::Result<i32> {
    let expression = if cmd.expression.is_empty() {
        loaded.config.schedule.clone()
    } else {
        cmd.expression.join(" ")
    };

    if expression.trim().is_empty() {
        println!(
            "No schedule set. Add `schedule = \"0 18 * * *\"` to the config, or pass an expression."
        );
        return Ok(0);
    }

    let cron = schedule::Cron::parse(&expression)?;
    println!("schedule      {}", cron.expression);

    let runs = cron.upcoming(chrono::Local::now(), 3);
    if runs.is_empty() {
        println!("This expression never fires.");
        return Ok(1);
    }
    println!("next runs:");
    for run in &runs {
        println!("  {}", run.format("%a %d %b %Y %H:%M"));
    }

    println!("\nSorta runs no daemon. Let the OS call it, and let --if-due decide:");
    println!("  cron:     {expression} sorta scan --if-due");
    println!(
        "  Windows:  schtasks /create /tn Sorta /sc minute /mo 15 /tr \"sorta scan --if-due\""
    );
    Ok(0)
}

// ---------------------------------------------------------------------------
// embed
// ---------------------------------------------------------------------------

pub(super) fn cmd_embed(loaded: &LoadedConfig, cmd: &EmbedCmd) -> anyhow::Result<i32> {
    let text = cmd.text.join(" ");
    if text.trim().is_empty() {
        eprintln!("sorta embed: give some text");
        return Ok(2);
    }

    let embedder = Embedder::from_config(&loaded.config.embedding);
    println!("engine        {}", embedder.describe());

    match embedder.embed(&text) {
        Some(vector) => {
            let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
            let head: Vec<String> = vector.iter().take(6).map(|v| format!("{v:.4}")).collect();
            println!("dimensions    {}", vector.len());
            println!("norm          {norm:.4}");
            println!("first values  {}", head.join(", "));
            Ok(0)
        }
        None => {
            println!("No vector produced.");
            println!(
                "Set [embedding] engine = \"command\" and a command that prints numbers, or install crispembed."
            );
            Ok(1)
        }
    }
}

// ---------------------------------------------------------------------------
// audit
// ---------------------------------------------------------------------------

pub(super) fn cmd_audit(cmd: &AuditCmd) -> anyhow::Result<i32> {
    let log = Audit::new(&paths::state_dir().join("audit.jsonl"));
    let action = cmd.args.first().map(String::as_str).unwrap_or("verify");

    match action {
        "verify" => {
            let report = log.verify();
            if report.ok {
                println!("Chain intact: {}.", report.detail);
                Ok(0)
            } else {
                println!(
                    "Chain BROKEN at entry {}: {}",
                    report.first_break.unwrap_or(0),
                    report.detail
                );
                Ok(1)
            }
        }
        "show" => {
            let limit = cmd
                .args
                .get(1)
                .and_then(|raw| raw.parse::<usize>().ok())
                .unwrap_or(20);
            let entries = log.entries();
            if entries.is_empty() {
                println!("No audit entries. Enable with `audit_enabled = true`.");
                return Ok(0);
            }
            let start = entries.len().saturating_sub(limit);
            for entry in &entries[start..] {
                println!("{}  {}  {}", entry.ts, entry.rule, entry.dst);
            }
            println!(
                "\n{} of {} entries ({})",
                entries.len() - start,
                entries.len(),
                log.path().display()
            );
            Ok(0)
        }
        other => {
            eprintln!("sorta audit: unknown action '{other}' (verify | show [N])");
            Ok(2)
        }
    }
}

// ---------------------------------------------------------------------------
// pause / resume
// ---------------------------------------------------------------------------

pub(super) fn cmd_pause(paused: bool) -> anyhow::Result<i32> {
    let state_dir = paths::state_dir();
    let previous = instance::set_paused(&state_dir, paused)?;

    if paused {
        println!(
            "Paused. Every file is deferred until `sorta resume` ({}).",
            state_dir.join("paused").display()
        );
    } else if previous {
        println!("Resumed.");
    } else {
        println!("Was not paused.");
    }
    Ok(0)
}
