//! Everything else: rules, quarantine, search, retention, digest, insights, audit,
//! embed, schedule, packs, export, pause.

use super::*;
use crate::audit::Audit;
use crate::benchmark;
use crate::config::{Config, LoadedConfig};
use crate::digest;
use crate::embedding::Embedder;
use crate::extract;
use crate::forget;
use crate::history::History;
use crate::insight;
use crate::instance;
use crate::interop;
use crate::ipc;
use crate::llm::LlmRouter;
use crate::models;
use crate::packs;
use crate::paths;
use crate::quarantine::Quarantine;
use crate::retention;
use crate::scan;
use crate::schedule;
use crate::search::Index;
use crate::sort::{catalogue, conflicts, is_available, preview_name};
use crate::vault;
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

/// Show which Tier-0 models are present, pinned and missing.
#[derive(argh::FromArgs, Debug)]
#[argh(subcommand, name = "models")]
pub struct ModelsCmd {
    /// models directory (default: ./models)
    #[argh(option)]
    pub dir: Option<String>,

    /// verify pinned digests
    #[argh(switch)]
    pub verify: bool,
}

/// Open the window (needs a build with --features gui).
#[derive(argh::FromArgs, Debug)]
#[argh(subcommand, name = "tray")]
pub struct TrayCmd {}

/// Measure extraction quality over a folder of real documents.
///
/// Put options before the folders: the folder list is greedy and swallows anything
/// after the first one.
#[derive(argh::FromArgs, Debug)]
#[argh(subcommand, name = "bench-extract")]
pub struct BenchExtractCmd {
    /// folders to measure
    #[argh(positional, greedy)]
    pub roots: Vec<String>,

    /// stop after this many files
    #[argh(option, default = "50")]
    pub limit: usize,

    /// only these extensions, comma separated (e.g. pdf,docx)
    #[argh(option)]
    pub only: Option<String>,
}

/// Measure classification over a folder: what would be filed, what would not.
#[derive(argh::FromArgs, Debug)]
#[argh(subcommand, name = "bench-classify")]
pub struct BenchClassifyCmd {
    /// folders to measure
    #[argh(positional, greedy)]
    pub roots: Vec<String>,

    /// stop after this many files
    #[argh(option, default = "100")]
    pub limit: usize,

    /// only these extensions, comma separated
    #[argh(option)]
    pub only: Option<String>,
}

/// Ask a running instance whether it is alive, and what it is doing.
#[derive(argh::FromArgs, Debug)]
#[argh(subcommand, name = "ping")]
pub struct PingCmd {}

/// Manage the encrypted vault: init, lock, unlock, status.
#[derive(argh::FromArgs, Debug)]
#[argh(subcommand, name = "vault")]
pub struct VaultCmd {
    /// init | lock <file> | unlock <file> | status
    #[argh(positional, greedy)]
    pub args: Vec<String>,

    /// keep the plaintext after locking
    #[argh(switch)]
    pub keep: bool,

    /// where to write the decrypted copy (default: alongside, without the suffix)
    #[argh(option)]
    pub out: Option<String>,

    /// init: also store the key in the OS keychain (needs the vault-keyring feature)
    #[argh(switch)]
    pub keychain: bool,
}

/// Erase Sorta's own records about a file, or all of them.
#[derive(argh::FromArgs, Debug)]
#[argh(subcommand, name = "forget")]
pub struct ForgetCmd {
    /// file path to forget
    #[argh(positional)]
    pub path: Option<String>,

    /// erase every record Sorta keeps (your documents are not touched)
    #[argh(switch)]
    pub all: bool,

    /// also remove the path from the history log, rewriting it
    #[argh(switch)]
    pub history: bool,

    /// confirm --all
    #[argh(switch)]
    pub yes: bool,
}

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
// models
// ---------------------------------------------------------------------------

pub(super) fn cmd_models(cmd: &ModelsCmd) -> anyhow::Result<i32> {
    let directory = cmd
        .dir
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("models"));

    let statuses = models::status(&directory);
    let summary = models::summarise(&statuses);

    println!("models in {}", directory.display());
    println!(
        "  {} known, {} present, {} verified, {} mismatched ({} bytes)\n",
        summary.known, summary.present, summary.verified, summary.mismatched, summary.bytes
    );

    for status in &statuses {
        println!(
            "{:<18} {:<24} {:<8} {}",
            status.spec.id,
            status.label(),
            status.bytes,
            status.spec.purpose
        );
        if cmd.verify && status.digest_ok == Some(false) {
            println!(
                "  expected {}, found {}",
                status.spec.digest,
                models::digest_of(&status.path).unwrap_or_else(|_| "unreadable".to_string())
            );
        }
        if !status.present {
            println!(
                "  drop it at {} and rebuild with --features {}",
                status.path.display(),
                status.spec.feature
            );
        }
    }

    if summary.present == 0 {
        println!(
            "\nNo weights. Sorta files by format, keyword and confidence until you add them;"
        );
        println!("see models/README.md for what each file is and how to convert it.");
    }

    Ok(if summary.mismatched > 0 { 1 } else { 0 })
}

// ---------------------------------------------------------------------------
// tray
// ---------------------------------------------------------------------------

pub(super) fn cmd_tray(loaded: &LoadedConfig) -> anyhow::Result<i32> {
    match crate::ui::run_dropbox(loaded, None) {
        Ok(()) => Ok(0),
        Err(error) => {
            eprintln!("sorta tray: {error}");
            Ok(1)
        }
    }
}

// ---------------------------------------------------------------------------
// bench-classify
// ---------------------------------------------------------------------------

pub(super) fn cmd_bench_classify(
    loaded: &LoadedConfig,
    cmd: &BenchClassifyCmd,
) -> anyhow::Result<i32> {
    if cmd.roots.is_empty() {
        eprintln!("sorta bench-classify: give one or more folders");
        return Ok(2);
    }

    let mut files = Vec::new();
    for root in &cmd.roots {
        let path = PathBuf::from(root);
        if path.is_dir() {
            files.extend(scan::collect_files(&path));
        } else if path.is_file() {
            files.push(path);
        }
    }
    if let Some(only) = &cmd.only {
        let wanted: Vec<String> = only
            .split(',')
            .map(|ext| ext.trim().trim_start_matches('.').to_ascii_lowercase())
            .filter(|ext| !ext.is_empty())
            .collect();
        files.retain(|file| {
            file.extension()
                .map(|ext| wanted.contains(&ext.to_string_lossy().to_ascii_lowercase()))
                .unwrap_or(false)
        });
    }
    files.sort();
    files.truncate(cmd.limit.max(1));

    let classifier = crate::sort::Classifier::new(&loaded.config);
    let gate = loaded.config.min_confidence;
    let year = chrono::Local::now().format("%Y").to_string();
    let mut reports = Vec::with_capacity(files.len());

    for file in &files {
        let Ok(detected) = crate::detect::detect(file) else {
            continue;
        };
        let text = extract::extract(file, &detected).map(|extracted| extracted.text);
        let name = file
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let verdict = classifier.classify(&detected, &name, text.as_deref(), &year);

        reports.push(benchmark::ClassReport {
            path: file.display().to_string(),
            extension: file
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default(),
            rule: verdict.rule_id.clone(),
            category: verdict.category.clone(),
            confidence: verdict.confidence,
            gate,
            sorts: verdict.confidence >= gate,
        });
    }

    let summary = benchmark::summarise_classification(&reports);
    print!("{}", benchmark::render_classification(&reports, &summary));
    Ok(0)
}

// ---------------------------------------------------------------------------
// bench-extract
// ---------------------------------------------------------------------------

pub(super) fn cmd_bench_extract(cmd: &BenchExtractCmd) -> anyhow::Result<i32> {
    if cmd.roots.is_empty() {
        eprintln!("sorta bench-extract: give one or more folders");
        return Ok(2);
    }

    let mut files = Vec::new();
    for root in &cmd.roots {
        let path = PathBuf::from(root);
        if path.is_dir() {
            files.extend(scan::collect_files(&path));
        } else if path.is_file() {
            files.push(path);
        }
    }
    if let Some(only) = &cmd.only {
        let wanted: Vec<String> = only
            .split(',')
            .map(|ext| ext.trim().trim_start_matches('.').to_ascii_lowercase())
            .filter(|ext| !ext.is_empty())
            .collect();
        files.retain(|file| {
            file.extension()
                .map(|ext| wanted.contains(&ext.to_string_lossy().to_ascii_lowercase()))
                .unwrap_or(false)
        });
    }
    files.sort();
    files.truncate(cmd.limit.max(1));

    let mut reports = Vec::with_capacity(files.len());
    for file in &files {
        let bytes = std::fs::metadata(file).map(|m| m.len()).unwrap_or(0);
        let detected = match crate::detect::detect(file) {
            Ok(detected) => detected,
            Err(error) => {
                eprintln!("skip {}: {error}", file.display());
                continue;
            }
        };
        let extracted = extract::extract(file, &detected);
        let (text, method) = match &extracted {
            Some(extracted) => (Some(extracted.text.as_str()), extracted.method),
            None => (None, "none"),
        };
        reports.push(benchmark::report(file, bytes, text, method));
    }

    let summary = benchmark::summarise(&reports);
    print!("{}", benchmark::render(&reports, &summary));
    Ok(0)
}

// ---------------------------------------------------------------------------
// ping
// ---------------------------------------------------------------------------

pub(super) fn cmd_ping() -> anyhow::Result<i32> {
    let state_dir = paths::state_dir();
    match ipc::call(&state_dir, &ipc::Request::Ping) {
        Ok(ipc::Response::Pong { pid }) => {
            println!("An instance is listening (pid {pid}).");
            match ipc::call(&state_dir, &ipc::Request::Status) {
                Ok(ipc::Response::Status {
                    paused,
                    instances,
                    indexed,
                    quarantined,
                    ..
                }) => {
                    println!("  paused        {paused}");
                    println!("  instances     {instances}");
                    println!("  indexed files {indexed}");
                    println!("  quarantine    {quarantined} waiting");
                }
                Ok(other) => println!("  status        {other:?}"),
                Err(error) => println!("  status        unavailable: {error}"),
            }
            Ok(0)
        }
        Ok(other) => {
            println!("Unexpected answer: {other:?}");
            Ok(1)
        }
        Err(error) => {
            println!("{error}");
            Ok(1)
        }
    }
}

// ---------------------------------------------------------------------------
// vault
// ---------------------------------------------------------------------------

pub(super) fn cmd_vault(loaded: &LoadedConfig, cmd: &VaultCmd) -> anyhow::Result<i32> {
    let state_dir = paths::state_dir();
    let action = cmd.args.first().map(String::as_str).unwrap_or("status");
    let target = cmd.args.get(1).map(String::as_str);

    let key_command = loaded.config.vault.key_command.as_str();

    match action {
        "init" => {
            let (key, created) =
                vault::VaultKey::load_or_create_with(&state_dir, Some(key_command))?;
            if created {
                println!(
                    "Created {} (0600). Key fingerprint {}.",
                    state_dir.join(vault::KEY_FILE).display(),
                    key.fingerprint()
                );
                #[cfg(feature = "vault-keyring")]
                if cmd.keychain {
                    match key.store_in_keychain() {
                        Ok(()) => println!("Stored in the OS keychain as sorta/vault."),
                        Err(error) => println!("Could not use the OS keychain: {error}"),
                    }
                }
                println!(
                    "Back this file up, or set {} / [vault] key_command to keep the key elsewhere.",
                    vault::KEY_ENV
                );
            } else if !key_command.trim().is_empty() {
                println!(
                    "Key comes from [vault] key_command (fingerprint {}); no key file written.",
                    key.fingerprint()
                );
            } else {
                println!(
                    "Key already exists: {} (fingerprint {}).",
                    state_dir.join(vault::KEY_FILE).display(),
                    key.fingerprint()
                );
            }
            Ok(0)
        }
        "status" => {
            let key_file = state_dir.join(vault::KEY_FILE);
            println!(
                "key source     {}",
                vault::VaultKey::provider(Some(key_command), key_file.exists())
            );
            match vault::VaultKey::load_with(&state_dir, Some(key_command)) {
                Ok(key) => println!("key            present (fingerprint {})", key.fingerprint()),
                Err(_) => println!("key            none ({})", key_file.display()),
            }
            let folder = loaded.destination_root().join(vault::VAULT_DIR);
            println!("vault folder   {}", folder.display());
            println!(
                "auto-lock      off (the sort pipeline does not lock files yet; use `sorta vault lock`)"
            );
            Ok(0)
        }
        "lock" => {
            let Some(source) = target else {
                eprintln!("sorta vault lock: give a file to lock");
                return Ok(2);
            };
            let source_path = PathBuf::from(source);
            if !source_path.exists() {
                eprintln!("sorta vault lock: no such file: {source}");
                return Ok(2);
            }
            if vault::is_locked(&source_path) {
                println!("{source} is already locked.");
                return Ok(0);
            }

            let key = vault::VaultKey::load_with(&state_dir, Some(key_command))?;
            let name = source_path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "file".to_string());
            let folder = loaded.destination_root().join(vault::VAULT_DIR);
            let out = folder.join(format!("{}{}", safety_name(&name), vault::LOCKED_SUFFIX));

            let size = vault::encrypt_file(&key, &source_path, &out)?;
            // Verify before removing anything: a lock that loses the plaintext is
            // worse than no lock at all.
            vault::decrypt_file(&key, &out, &source_path.with_extension("verify"))?;
            std::fs::remove_file(source_path.with_extension("verify")).ok();

            println!("Locked {source} -> {} ({size} bytes)", out.display());
            if cmd.keep {
                println!("Plaintext kept (--keep).");
            } else {
                std::fs::remove_file(&source_path)?;
                println!("Plaintext removed.");
            }
            Ok(0)
        }
        "unlock" => {
            let Some(locked) = target else {
                eprintln!("sorta vault unlock: give a locked file");
                return Ok(2);
            };
            let locked_path = PathBuf::from(locked);
            if !vault::is_locked(&locked_path) {
                eprintln!("sorta vault unlock: not a locked Sorta file: {locked}");
                return Ok(2);
            }

            let key = vault::VaultKey::load_with(&state_dir, Some(key_command))?;
            let out = match &cmd.out {
                Some(path) => PathBuf::from(path),
                None => {
                    let stem = locked.trim_end_matches(vault::LOCKED_SUFFIX);
                    PathBuf::from(format!("{stem}.unlocked"))
                }
            };
            vault::decrypt_file(&key, &locked_path, &out)?;
            println!("Unlocked {locked} -> {}", out.display());
            Ok(0)
        }
        other => {
            eprintln!("sorta vault: unknown action '{other}' (init | lock | unlock | status)");
            Ok(2)
        }
    }
}

/// Sanitises a file name for the vault folder.
fn safety_name(name: &str) -> String {
    crate::safety::sanitize_filename(name)
}

// ---------------------------------------------------------------------------
// forget
// ---------------------------------------------------------------------------

pub(super) fn cmd_forget(loaded: &LoadedConfig, cmd: &ForgetCmd) -> anyhow::Result<i32> {
    let state_dir = paths::state_dir();

    if cmd.all {
        let quarantine_root = loaded.quarantine_root();
        if !cmd.yes {
            println!("This would delete every record Sorta keeps:");
            for name in forget::STATE_FILES {
                println!("  {}/{}", state_dir.display(), name);
            }
            println!("  {}/quarantine.toml", quarantine_root.display());
            println!("\nYour documents are not touched. Re-run with --yes to erase.");
            return Ok(2);
        }

        let removed = forget::forget_everything(&state_dir, &quarantine_root);
        let bytes: u64 = removed.iter().map(|(_, size)| *size).sum();
        for (path, size) in &removed {
            println!(
                "  removed {} ({})",
                path.display(),
                forget::human_bytes(*size)
            );
        }
        println!(
            "\nErased {} record file(s), {}. Your documents were not touched.",
            removed.len(),
            forget::human_bytes(bytes)
        );
        return Ok(0);
    }

    let Some(target) = cmd.path.as_deref() else {
        eprintln!("sorta forget: give a path, or --all to erase everything");
        return Ok(2);
    };

    let report = forget::forget_path(&state_dir, &loaded.quarantine_root(), target, cmd.history)?;

    if report.is_empty() && !report.audit_retained {
        println!("Nothing recorded for {target}.");
        return Ok(0);
    }

    println!("Forgot {target}:");
    println!(
        "  index entry      {}",
        if report.index_entry {
            "removed"
        } else {
            "none"
        }
    );
    println!(
        "  embedding        {}",
        if report.vector { "removed" } else { "none" }
    );
    println!("  duplicate rows   {}", report.dedup_entries);
    println!("  quarantine rows  {}", report.quarantine_entries);
    println!(
        "  history lines    {}",
        if cmd.history {
            report.history_lines.to_string()
        } else {
            "kept (pass --history to rewrite the log)".to_string()
        }
    );
    if report.audit_retained {
        println!(
            "  audit chain      still records this path; delete {} if you need it gone",
            state_dir.join("audit.jsonl").display()
        );
    }
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
