//! Sorta Packs: portable rule bundles.
//!
//! A `.sortapack` is a ZIP holding a `manifest.json` and a `sorta.toml`. It carries
//! the parts of a configuration that describe *behaviour* — keyword rules, the
//! folder template, the confidence gate, ignore patterns — and deliberately not
//! the parts that describe *this machine* — watch folders, destination roots,
//! instance names, credentials.
//!
//! Importing is a merge, never a blind overwrite: the caller gets the merged
//! config back, shows the difference, and writes it only when asked.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::Config;

/// Conventional file extension.
pub const EXTENSION: &str = "sortapack";
/// Manifest entry name inside the archive.
pub const MANIFEST: &str = "manifest.json";
/// Config entry name inside the archive.
pub const CONFIG_ENTRY: &str = "sorta.toml";

/// What the pack says about itself.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub name: String,
    pub description: String,
    /// Pack format version, so an old import can refuse a newer pack.
    pub pack_version: u32,
    /// Sorta version that produced it.
    pub sorta_version: String,
    pub created_at: String,
    pub keyword_rules: usize,
}

/// The current pack format.
pub const PACK_VERSION: u32 = 1;

/// Writes a pack and returns its path.
pub fn export(
    config: &Config,
    directory: &Path,
    name: &str,
    description: &str,
) -> anyhow::Result<PathBuf> {
    crate::paths::ensure_dir(directory)?;

    let portable = portable_config(config);
    let manifest = Manifest {
        name: name.to_string(),
        description: description.to_string(),
        pack_version: PACK_VERSION,
        sorta_version: crate::VERSION.to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
        keyword_rules: portable.keyword_rules.len(),
    };

    let path = directory.join(format!("{}.{EXTENSION}", sanitize(name)));
    let file = std::fs::File::create(&path)?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    zip.start_file(MANIFEST, options)?;
    zip.write_all(serde_json::to_string_pretty(&manifest)?.as_bytes())?;

    zip.start_file(CONFIG_ENTRY, options)?;
    zip.write_all(portable.to_toml_string()?.as_bytes())?;

    zip.finish()?;
    Ok(path)
}

/// Reads the manifest without importing anything.
pub fn read_manifest(pack: &Path) -> anyhow::Result<Manifest> {
    let mut archive = open(pack)?;
    let mut text = String::new();
    archive
        .by_name(MANIFEST)
        .map_err(|_| anyhow::anyhow!("{MANIFEST} is missing from {}", pack.display()))?
        .read_to_string(&mut text)?;
    Ok(serde_json::from_str(&text)?)
}

/// Reads the config stored in a pack.
pub fn read_config(pack: &Path) -> anyhow::Result<Config> {
    let mut archive = open(pack)?;
    let mut text = String::new();
    archive
        .by_name(CONFIG_ENTRY)
        .map_err(|_| anyhow::anyhow!("{CONFIG_ENTRY} is missing from {}", pack.display()))?
        .read_to_string(&mut text)?;
    Config::from_toml_str(&text)
}

/// The behaviour-carrying subset of a config.
pub fn portable_config(config: &Config) -> Config {
    Config {
        keyword_rules: config.keyword_rules.clone(),
        folder_template: config.folder_template.clone(),
        min_confidence: config.min_confidence,
        on_collision: config.on_collision,
        on_duplicate: config.on_duplicate,
        ignore_patterns: config.ignore_patterns.clone(),
        retention: config.retention.clone(),
        sidecar_enabled: config.sidecar_enabled,
        ..Config::default()
    }
}

/// Merges a pack's behaviour into `base`, returning the result.
///
/// Keyword rules and ignore patterns are replaced wholesale: a pack is a unit, and
/// merging rule sets item by item is how you get two rules fighting over the same
/// file without anyone noticing.
pub fn merge(base: &Config, pack: &Config) -> Config {
    let mut merged = base.clone();
    merged.keyword_rules = pack.keyword_rules.clone();
    merged.folder_template = pack.folder_template.clone();
    merged.min_confidence = pack.min_confidence;
    merged.on_collision = pack.on_collision;
    merged.on_duplicate = pack.on_duplicate;
    merged.ignore_patterns = pack.ignore_patterns.clone();
    merged.retention = pack.retention.clone();
    merged.sidecar_enabled = pack.sidecar_enabled;
    merged.normalise();
    merged
}

/// Human-readable summary of what a merge would change.
pub fn describe_merge(base: &Config, merged: &Config) -> Vec<String> {
    let mut changes = Vec::new();

    if base.keyword_rules != merged.keyword_rules {
        changes.push(format!(
            "keyword rules: {} -> {}",
            base.keyword_rules.len(),
            merged.keyword_rules.len()
        ));
    }
    if base.folder_template != merged.folder_template {
        changes.push(format!(
            "folder template: '{}' -> '{}'",
            base.folder_template, merged.folder_template
        ));
    }
    if (base.min_confidence - merged.min_confidence).abs() > f32::EPSILON {
        changes.push(format!(
            "min confidence: {:.0}% -> {:.0}%",
            base.min_confidence * 100.0,
            merged.min_confidence * 100.0
        ));
    }
    if base.on_collision != merged.on_collision {
        changes.push(format!(
            "collision policy: {} -> {}",
            base.on_collision.as_str(),
            merged.on_collision.as_str()
        ));
    }
    if base.on_duplicate != merged.on_duplicate {
        changes.push(format!(
            "duplicate policy: {} -> {}",
            base.on_duplicate.as_str(),
            merged.on_duplicate.as_str()
        ));
    }
    if base.ignore_patterns.patterns != merged.ignore_patterns.patterns {
        changes.push(format!(
            "ignore patterns: {} -> {}",
            base.ignore_patterns.patterns.len(),
            merged.ignore_patterns.patterns.len()
        ));
    }
    if base.sidecar_enabled != merged.sidecar_enabled {
        changes.push(format!(
            "sidecars: {} -> {}",
            base.sidecar_enabled, merged.sidecar_enabled
        ));
    }
    if base.retention.enabled != merged.retention.enabled
        || base.retention.max_age_days != merged.retention.max_age_days
    {
        changes.push(format!(
            "retention: {} / {} days -> {} / {} days",
            base.retention.enabled,
            base.retention.max_age_days,
            merged.retention.enabled,
            merged.retention.max_age_days
        ));
    }
    changes
}

fn open(pack: &Path) -> anyhow::Result<zip::ZipArchive<std::fs::File>> {
    let file = std::fs::File::open(pack)?;
    Ok(zip::ZipArchive::new(file)?)
}

fn sanitize(name: &str) -> String {
    crate::safety::sanitize_filename(name).replace(' ', "-")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_rules() -> Config {
        let mut config = Config {
            min_confidence: 0.8,
            folder_template: "{year}/{category}".to_string(),
            ..Config::default()
        };
        config.keyword_rules.clear();
        config
            .keyword_rules
            .insert("Invoice".to_string(), "PDF-Invoices".to_string());
        config
            .keyword_rules
            .insert("Payslip".to_string(), "Payslips".to_string());
        config
    }

    #[test]
    fn exports_and_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_with_rules();
        let path = export(&config, dir.path(), "freelance", "client paperwork").unwrap();

        assert!(path.exists());
        assert!(path.to_string_lossy().ends_with(".sortapack"));

        let manifest = read_manifest(&path).unwrap();
        assert_eq!(manifest.name, "freelance");
        assert_eq!(manifest.keyword_rules, 2);
        assert_eq!(manifest.pack_version, PACK_VERSION);

        let loaded = read_config(&path).unwrap();
        assert_eq!(loaded.keyword_rules, config.keyword_rules);
        assert!((loaded.min_confidence - 0.8).abs() < f32::EPSILON);
    }

    #[test]
    fn packs_carry_behaviour_not_machine_paths() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config_with_rules();
        config.watch_folders = vec!["/home/u/Downloads".to_string()];
        config.destination_root = "/home/u/Sorted".to_string();
        config.instance_name = "work".to_string();

        let path = export(&config, dir.path(), "p", "").unwrap();
        let loaded = read_config(&path).unwrap();

        assert!(loaded.watch_folders.is_empty());
        assert_eq!(loaded.destination_root, "./Sorted");
        assert_eq!(loaded.instance_name, "default");
        assert_eq!(loaded.keyword_rules, config.keyword_rules);
    }

    #[test]
    fn merge_replaces_rules_and_keeps_local_paths() {
        let base = Config {
            destination_root: "/data/Sorted".to_string(),
            watch_folders: vec!["/data/inbox".to_string()],
            ..Config::default()
        };

        let pack = config_with_rules();
        let merged = merge(&base, &pack);

        assert_eq!(merged.destination_root, "/data/Sorted");
        assert_eq!(merged.watch_folders, vec!["/data/inbox".to_string()]);
        assert_eq!(merged.keyword_rules, pack.keyword_rules);
        assert!((merged.min_confidence - 0.8).abs() < f32::EPSILON);
    }

    #[test]
    fn describe_merge_lists_the_differences() {
        let base = Config::default();
        let merged = merge(&base, &config_with_rules());
        let changes = describe_merge(&base, &merged);
        assert!(changes.iter().any(|c| c.starts_with("keyword rules")));
        assert!(changes.iter().any(|c| c.starts_with("min confidence")));
        assert!(describe_merge(&base, &base).is_empty());
    }

    #[test]
    fn a_non_pack_file_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.sortapack");
        std::fs::write(&path, b"not a zip").unwrap();
        assert!(read_manifest(&path).is_err());
        assert!(read_config(&path).is_err());
    }
}
