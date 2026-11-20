//! Configuration: resolution order, the full key set, and defaults.
//!
//! Resolution order (first hit wins):
//! 1. an explicit `--config` path
//! 2. `./sorta.toml` (portable mode, beside the binary or the cwd)
//! 3. `~/.config/sorta/<instance>.toml` (`%APPDATA%\sorta\...` on Windows)
//! 4. built-in defaults
//!
//! Relative paths inside the file (`destination_root`, `quarantine_root`,
//! `backup_root`) are resolved against the directory holding the config file,
//! which is what makes portable mode actually portable.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::paths;

/// What to do when the destination already holds the same name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum CollisionPolicy {
    /// Append ` (1)`, ` (2)`, ... until free. Default.
    #[default]
    Rename,
    /// Leave the source file where it is.
    Skip,
    /// Overwrite the destination.
    Replace,
    /// Keep both, disambiguating with a timestamp.
    KeepBoth,
}

impl CollisionPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rename => "rename",
            Self::Skip => "skip",
            Self::Replace => "replace",
            Self::KeepBoth => "keep_both",
        }
    }
}

impl FromStr for CollisionPolicy {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "rename" => Ok(Self::Rename),
            "skip" => Ok(Self::Skip),
            "replace" => Ok(Self::Replace),
            "keep_both" => Ok(Self::KeepBoth),
            other => Err(format!("unknown collision policy: {other}")),
        }
    }
}

/// How archives are treated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ArchiveMode {
    /// Sort the archive as a single file. Default.
    #[default]
    AsFile,
    /// Extract, then sort the contents.
    Extract,
    /// Ask before extracting.
    Ask,
}

/// What to do with a byte-identical duplicate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum DupPolicy {
    /// Leave it where it is and log the match. Default.
    #[default]
    Skip,
    /// Move it into `duplicates_root`.
    Move,
    /// Keep the newest copy; the older copy goes to `duplicates_root`.
    KeepNewest,
}

impl DupPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Skip => "skip",
            Self::Move => "move",
            Self::KeepNewest => "keep_newest",
        }
    }
}

/// Which language-model tier the user allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum LlmMode {
    /// Never touch an LLM.
    Off,
    /// Use a local server if one is listening; otherwise stay at Tier 0.
    Auto,
    /// Download and run the bundled SmolLM2.
    Bundled,
    /// Bring your own key.
    Byok,
    /// Auto, then bundled, then Tier 0. Default.
    #[default]
    AutoWithFallback,
}

impl LlmMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Auto => "auto",
            Self::Bundled => "bundled",
            Self::Byok => "byok",
            Self::AutoWithFallback => "auto_with_fallback",
        }
    }
}

/// Ignore globs, kept in a sub-table so `[ignore_patterns]` reads cleanly.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct IgnorePatterns {
    pub patterns: Vec<String>,
}

impl Default for IgnorePatterns {
    fn default() -> Self {
        Self {
            patterns: vec![
                "*.crdownload".to_string(),
                "*.part".to_string(),
                "*.tmp".to_string(),
                "~$*".to_string(),
                ".DS_Store".to_string(),
            ],
        }
    }
}

/// Where keyword rules look for a match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum KeywordScope {
    /// File name only. The default, because a body match is a weak signal.
    #[default]
    Name,
    /// File name, then the extracted text.
    NameAndText,
}

/// Encrypted vault settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct VaultConfig {
    /// Command that prints the vault key as 64 hex characters, e.g.
    /// `secret-tool lookup service sorta` or `pass show sorta/vault`.
    ///
    /// This is how the OS keychain is reached without a `keyring` dependency: the
    /// user already has a tool that talks to it. Empty means "use the key file".
    pub key_command: String,

    /// Lock files that the rules send into `_Sensitive/`, as they are filed.
    pub auto_lock: bool,
}

/// Shell hooks run after a successful sort.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HooksConfig {
    /// Command run after a file is moved. Tokens: `{path}`, `{src}`, `{rule}`,
    /// `{confidence}`. Empty means no hook.
    pub on_sort: String,
}

/// Where text embeddings come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum EmbeddingEngine {
    /// Never embed anything.
    Off,
    /// Look for `crispembed` on PATH; fall back to off. Default.
    #[default]
    Auto,
    /// Use the configured command line, whatever it is.
    Command,
}

impl EmbeddingEngine {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Auto => "auto",
            Self::Command => "command",
        }
    }
}

/// What to do when a document is semantically near-duplicate of a filed one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum NearDuplicateAction {
    /// Ignore similarity entirely.
    Off,
    /// File it as usual, and record the match in the log. Default.
    #[default]
    Suggest,
    /// Send it to quarantine with reason `possible_duplicate` (the spec's
    /// behaviour; not the default because a false positive costs a review).
    Quarantine,
}

impl NearDuplicateAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Suggest => "suggest",
            Self::Quarantine => "quarantine",
        }
    }
}

/// Local embedding engine settings.
///
/// Sorta ships no weights, so this describes an *external* local tool rather than
/// a bundled model. With no engine configured, semantic features stay dark and
/// every core feature still works.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EmbeddingConfig {
    /// off | auto | command.
    pub engine: EmbeddingEngine,
    /// Command line used by `auto` (if it resolves) and by `command`.
    pub command: String,
    /// Vector width the engine produces.
    pub dimensions: usize,
    /// Cosine similarity at or above which two documents are near-duplicates.
    pub near_duplicate_threshold: f32,
    /// What to do about them.
    pub near_duplicate_action: NearDuplicateAction,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            engine: EmbeddingEngine::default(),
            command: "crispembed -m all-MiniLM-L6-v2".to_string(),
            dimensions: 384,
            near_duplicate_threshold: 0.92,
            near_duplicate_action: NearDuplicateAction::default(),
        }
    }
}

/// Retention policy: age limits for the folders the user names.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RetentionConfig {
    /// Off unless asked for. Deleting is irreversible.
    pub enabled: bool,
    /// Files at or past this age are candidates. 0 disables the policy.
    pub max_age_days: u32,
    /// Folders (relative to `destination_root`) the policy covers. Empty means
    /// the whole destination tree, still skipping the protected folders.
    pub folders: Vec<String>,
    /// Gzip old files instead of deleting them.
    pub compress_instead: bool,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_age_days: 365,
            folders: Vec::new(),
            compress_instead: false,
        }
    }
}

/// The whole configuration surface.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Folders the watcher and `scan` cover.
    pub watch_folders: Vec<String>,
    /// Root of the sorted tree.
    pub destination_root: String,
    /// Where low-confidence files wait for review.
    pub quarantine_root: String,
    /// Sub-folder template, relative to `destination_root`.
    /// Tokens: `{year}`, `{month}`, `{category}`, `{kind}`, `{ext}`.
    pub folder_template: String,
    /// Collision policy when the destination name is taken.
    pub on_collision: CollisionPolicy,
    /// Archive handling.
    pub archive_mode: ArchiveMode,
    /// What to do with a byte-identical duplicate: skip | move | keep_newest.
    pub on_duplicate: DupPolicy,
    /// Where duplicates are moved when the policy says so.
    pub duplicates_root: String,
    /// Copy the file into the backup root before moving it.
    pub backup_before_move: bool,
    /// Backup root (phase 5).
    pub backup_root: String,
    /// Below this confidence a file goes to quarantine instead of a folder.
    pub min_confidence: f32,
    /// Only work when the machine is idle (phase 2).
    pub run_when_idle: bool,
    /// Cron expression for scheduled scans (phase 2).
    pub schedule: String,
    /// Allow more than one Sorta process.
    pub multi_instance: bool,
    /// Allow instances on a network share to coordinate (phase 6).
    pub network_multi_instance: bool,
    /// Name of this instance.
    pub instance_name: String,
    /// Accept workers running a different version.
    pub allow_mixed_versions: bool,
    /// LLM tier policy.
    pub llm_mode: LlmMode,
    /// Explicit endpoint, overriding auto-probe.
    pub llm_endpoint: String,
    /// `openai | anthropic | groq | openrouter | gemini | openai-compatible`.
    pub llm_provider: String,
    /// Send document text to the provider. Off by default, deliberately.
    pub llm_send_content: bool,
    /// Use the embedding index for dedupe/clustering.
    pub semantic_enabled: bool,
    /// Learn from manual corrections.
    pub autopilot_enabled: bool,
    /// Use calendar/email signals.
    pub context_enabled: bool,
    /// Predictive cleanup suggestions.
    pub predictive_enabled: bool,
    /// WASM plugin host.
    pub plugins_enabled: bool,
    /// Feather document preview.
    pub preview_enabled: bool,
    /// Case-insensitive keyword → folder rules.
    pub keyword_rules: BTreeMap<String, String>,
    /// Globs that are never touched.
    pub ignore_patterns: IgnorePatterns,
    /// Write `<file>.sorta.json` beside every sorted file.
    pub sidecar_enabled: bool,
    /// Age-based retention (see [`RetentionConfig`]).
    pub retention: RetentionConfig,
    /// Local embedding engine (see [`EmbeddingConfig`]).
    pub embedding: EmbeddingConfig,
    /// Post-sort shell hooks (see [`HooksConfig`]).
    pub hooks: HooksConfig,
    /// Encrypted vault (see [`VaultConfig`]).
    pub vault: VaultConfig,
    /// Whether keyword rules look in file names only, or in the text too.
    pub keyword_scope: KeywordScope,
    /// Append a tamper-evident audit chain alongside the history log.
    pub audit_enabled: bool,
    /// Global ceiling on moves per second; 0 disables it.
    pub max_files_per_second: u32,
}

impl Default for Config {
    fn default() -> Self {
        let mut keyword_rules = BTreeMap::new();
        keyword_rules.insert("Invoice".to_string(), "PDF-Invoices".to_string());
        keyword_rules.insert("Contract".to_string(), "Contracts".to_string());

        Self {
            watch_folders: Vec::new(),
            destination_root: "./Sorted".to_string(),
            quarantine_root: "./Quarantine".to_string(),
            folder_template: "{year}/{category}".to_string(),
            on_collision: CollisionPolicy::default(),
            archive_mode: ArchiveMode::default(),
            on_duplicate: DupPolicy::default(),
            duplicates_root: "./_Duplicates".to_string(),
            backup_before_move: false,
            backup_root: "./_sorta_backup".to_string(),
            min_confidence: 0.7,
            run_when_idle: false,
            schedule: String::new(),
            multi_instance: false,
            network_multi_instance: false,
            instance_name: "default".to_string(),
            allow_mixed_versions: false,
            llm_mode: LlmMode::default(),
            llm_endpoint: String::new(),
            llm_provider: String::new(),
            llm_send_content: false,
            semantic_enabled: true,
            autopilot_enabled: true,
            context_enabled: false,
            predictive_enabled: false,
            plugins_enabled: false,
            preview_enabled: false,
            keyword_rules,
            ignore_patterns: IgnorePatterns::default(),
            sidecar_enabled: false,
            retention: RetentionConfig::default(),
            embedding: EmbeddingConfig::default(),
            hooks: HooksConfig::default(),
            vault: VaultConfig::default(),
            keyword_scope: KeywordScope::default(),
            audit_enabled: false,
            max_files_per_second: crate::rate::DEFAULT_PER_SECOND,
        }
    }
}

/// A config plus the directory it came from, so relative paths can be resolved.
#[derive(Debug, Clone)]
pub struct LoadedConfig {
    pub config: Config,
    pub path: Option<PathBuf>,
    pub base_dir: PathBuf,
}

impl LoadedConfig {
    /// Destination root, resolved.
    pub fn destination_root(&self) -> PathBuf {
        resolve_relative(&self.base_dir, &self.config.destination_root)
    }

    /// Quarantine root, resolved.
    pub fn quarantine_root(&self) -> PathBuf {
        resolve_relative(&self.base_dir, &self.config.quarantine_root)
    }

    /// Backup root, resolved.
    pub fn backup_root(&self) -> PathBuf {
        resolve_relative(&self.base_dir, &self.config.backup_root)
    }

    /// Watch folders, resolved. Missing entries are kept as written.
    pub fn watch_folders(&self) -> Vec<PathBuf> {
        self.config
            .watch_folders
            .iter()
            .map(|w| resolve_relative(&self.base_dir, w))
            .collect()
    }
}

/// Resolves `value` against `base` unless it is already absolute.
pub fn resolve_relative(base: &Path, value: &str) -> PathBuf {
    let p = Path::new(value);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    }
}

impl Config {
    /// Default config file for an instance name.
    pub fn default_path(instance: &str) -> PathBuf {
        let dir = paths::config_dir();
        if instance == "default" || instance.is_empty() {
            dir.join("sorta.toml")
        } else {
            dir.join(format!("{instance}.toml"))
        }
    }

    /// Parses TOML text. Kept separate so tests do not touch the filesystem.
    pub fn from_toml_str(text: &str) -> anyhow::Result<Self> {
        let mut cfg: Config = toml::from_str(text)?;
        cfg.normalise();
        Ok(cfg)
    }

    /// Serialises to TOML text.
    pub fn to_toml_string(&self) -> anyhow::Result<String> {
        Ok(toml::to_string_pretty(self)?)
    }

    /// Clamps values that a hand-edited file can get wrong.
    pub fn normalise(&mut self) {
        if self.min_confidence.is_nan() {
            self.min_confidence = 0.7;
        }
        self.min_confidence = self.min_confidence.clamp(0.0, 1.0);
        if self.folder_template.trim().is_empty() {
            self.folder_template = "{year}/{category}".to_string();
        }
        if self.instance_name.trim().is_empty() {
            self.instance_name = "default".to_string();
        }
        if self.duplicates_root.trim().is_empty() {
            self.duplicates_root = "./_Duplicates".to_string();
        }
        if self.ignore_patterns.patterns.is_empty() {
            self.ignore_patterns = IgnorePatterns::default();
        }
    }

    /// Loads a config file, falling back to defaults when it does not exist.
    pub fn load(path: &Path) -> anyhow::Result<LoadedConfig> {
        let base_dir = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));

        if !path.exists() {
            return Ok(LoadedConfig {
                config: Config::default(),
                path: None,
                base_dir,
            });
        }

        let text = std::fs::read_to_string(path)?;
        let config = Config::from_toml_str(&text)?;
        Ok(LoadedConfig {
            config,
            path: Some(path.to_path_buf()),
            base_dir,
        })
    }

    /// Picks the config path for this invocation.
    pub fn discover(explicit: Option<&Path>, instance: &str) -> PathBuf {
        if let Some(p) = explicit {
            return p.to_path_buf();
        }
        let portable = Path::new("sorta.toml");
        if portable.exists() {
            return portable.to_path_buf();
        }
        let candidate = Config::default_path(instance);
        if candidate.exists() {
            return candidate;
        }
        candidate
    }

    /// Writes a documented default config, refusing to clobber an existing one.
    pub fn write_default(path: &Path) -> anyhow::Result<()> {
        if path.exists() {
            anyhow::bail!("refusing to overwrite existing config: {}", path.display());
        }
        if let Some(parent) = path.parent() {
            paths::ensure_dir(parent)?;
        }
        let text = Config::default().to_toml_string()?;
        std::fs::write(path, text)?;
        Ok(())
    }

    /// Effective ignore list: config patterns plus the always-skipped noise.
    pub fn ignore_list(&self) -> Vec<String> {
        let mut out = self.ignore_patterns.patterns.clone();
        for always in [".DS_Store", "Thumbs.db", "desktop.ini"] {
            if !out.iter().any(|p| p.eq_ignore_ascii_case(always)) {
                out.push(always.to_string());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_spec() {
        let cfg = Config::default();
        assert_eq!(cfg.destination_root, "./Sorted");
        assert_eq!(cfg.on_collision, CollisionPolicy::Rename);
        assert_eq!(cfg.archive_mode, ArchiveMode::AsFile);
        assert_eq!(cfg.on_duplicate, DupPolicy::Skip);
        assert_eq!(cfg.duplicates_root, "./_Duplicates");
        assert_eq!(cfg.embedding.engine, EmbeddingEngine::Auto);
        assert_eq!(cfg.embedding.dimensions, 384);
        assert!((cfg.embedding.near_duplicate_threshold - 0.92).abs() < f32::EPSILON);
        assert_eq!(
            cfg.embedding.near_duplicate_action,
            NearDuplicateAction::Suggest
        );
        assert!(cfg.hooks.on_sort.is_empty());
        assert!(cfg.vault.key_command.is_empty());
        assert!(!cfg.vault.auto_lock);
        assert_eq!(cfg.keyword_scope, KeywordScope::Name);
        assert!(!cfg.audit_enabled);
        assert_eq!(cfg.max_files_per_second, 20);
        assert_eq!(cfg.llm_mode, LlmMode::AutoWithFallback);
        assert!(!cfg.llm_send_content);
        assert!(cfg.semantic_enabled);
        assert!(!cfg.plugins_enabled);
        assert!((cfg.min_confidence - 0.7).abs() < f32::EPSILON);
        assert_eq!(cfg.keyword_rules.get("Invoice").unwrap(), "PDF-Invoices");
    }

    #[test]
    fn partial_toml_fills_the_rest_from_defaults() {
        let cfg = Config::from_toml_str("destination_root = \"/data/Sorted\"\n").unwrap();
        assert_eq!(cfg.destination_root, "/data/Sorted");
        assert_eq!(cfg.quarantine_root, "./Quarantine");
        assert_eq!(cfg.folder_template, "{year}/{category}");
    }

    #[test]
    fn round_trips_through_toml() {
        let cfg = Config::default();
        let text = cfg.to_toml_string().unwrap();
        let back = Config::from_toml_str(&text).unwrap();
        assert_eq!(back.destination_root, cfg.destination_root);
        assert_eq!(back.on_collision, cfg.on_collision);
        assert_eq!(back.ignore_patterns.patterns, cfg.ignore_patterns.patterns);
    }

    #[test]
    fn out_of_range_confidence_is_clamped() {
        let cfg = Config::from_toml_str("min_confidence = 4.2\n").unwrap();
        assert!((cfg.min_confidence - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn relative_paths_resolve_against_the_config_dir() {
        let loaded = LoadedConfig {
            config: Config::default(),
            path: None,
            base_dir: PathBuf::from("/etc/sorta"),
        };
        assert_eq!(
            loaded.destination_root(),
            PathBuf::from("/etc/sorta/./Sorted")
        );
    }
}
