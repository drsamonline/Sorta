//! Embeddings from a local engine.
//!
//! Sorta ships no model files, so it cannot compute an embedding by itself. The
//! blueprint's answer was a bundled ONNX encoder via `rten` — still the plan, and
//! still blocked on having weights to embed. This module is the honest middle:
//!
//! * if the user has a local embedding tool installed, semantic features light up;
//! * if they do not, nothing changes and every core feature still works.
//!
//! The reference engine is CrispEmbed (`crispembed -m all-MiniLM-L6-v2 "text"`),
//! a C++/ggml binary with a CLI and a Rust front-end. It is *not* linked into
//! Sorta — it is spawned, exactly like the Tier-1 local LLM probe, because linking
//! a C++ runtime would break the pure-Rust promise for everyone who never turns
//! semantic sorting on.
//!
//! Accepted output is deliberately forgiving: the first run of numbers anywhere in
//! stdout wins, whether it arrives as a JSON array, a bracketed list or plain
//! whitespace/comma separated values.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::config::{EmbeddingConfig, EmbeddingEngine};

/// Width of the default encoder (all-MiniLM-L6-v2).
pub const DEFAULT_DIMENSIONS: usize = 384;

/// Where vectors come from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    /// No engine: semantic features stay dark.
    Off,
    /// Spawn this program, with the text appended as the final argument.
    Command {
        program: String,
        args: Vec<String>,
    },
}

/// Computes text embeddings, or explains why it cannot.
#[derive(Debug, Clone)]
pub struct Embedder {
    provider: Provider,
    dimensions: usize,
    threshold: f32,
}

impl Embedder {
    /// Builds the embedder described by the config.
    ///
    /// `auto` looks for `crispembed` on `PATH` and falls back to `off`; `command`
    /// uses the configured command line as-is; `off` never spawns anything.
    pub fn from_config(config: &EmbeddingConfig) -> Self {
        let provider = match config.engine {
            EmbeddingEngine::Off => Provider::Off,
            EmbeddingEngine::Command => match parse_command(&config.command) {
                Some(provider) => provider,
                None => Provider::Off,
            },
            EmbeddingEngine::Auto => {
                let explicit = parse_command(&config.command);
                match explicit {
                    Some(provider) if program_available(&provider) => provider,
                    _ => match find_on_path("crispembed") {
                        Some(path) => Provider::Command {
                            program: path,
                            args: vec!["-m".to_string(), "all-MiniLM-L6-v2".to_string()],
                        },
                        None => Provider::Off,
                    },
                }
            }
        };

        Self {
            provider,
            dimensions: if config.dimensions == 0 {
                DEFAULT_DIMENSIONS
            } else {
                config.dimensions
            },
            threshold: config.near_duplicate_threshold.clamp(0.5, 0.999),
        }
    }

    pub fn provider(&self) -> &Provider {
        &self.provider
    }

    pub fn dimensions(&self) -> usize {
        self.dimensions
    }

    /// Cosine similarity above which two documents are treated as the same one.
    pub fn threshold(&self) -> f32 {
        self.threshold
    }

    pub fn is_available(&self) -> bool {
        !matches!(self.provider, Provider::Off)
    }

    /// One line for `sorta status`.
    pub fn describe(&self) -> String {
        match &self.provider {
            Provider::Off => "off (no local embedding engine found)".to_string(),
            Provider::Command { program, args } => {
                format!("{} {} ({} dims)", program, args.join(" "), self.dimensions)
            }
        }
    }

    /// Embeds one piece of text. `None` means "no engine, or it failed" — never an
    /// error the caller has to handle, because a missing embedding is not a fault.
    pub fn embed(&self, text: &str) -> Option<Vec<f32>> {
        let Provider::Command { program, args } = &self.provider else {
            return None;
        };
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return None;
        }

        let output = Command::new(program).args(args).arg(trimmed).output().ok()?;
        if !output.status.success() {
            return None;
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut vector = parse_vector(&stdout)?;
        if vector.len() != self.dimensions {
            // A different width than configured is still usable, but the caller
            // must not compare it against vectors of another width.
            vector.truncate(self.dimensions.min(vector.len()));
        }
        Some(vector)
    }
}

/// Cosine similarity. Returns 0 for mismatched or empty vectors.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a.sqrt() * norm_b.sqrt())
}

/// Extracts the longest run of numbers from a command's output.
///
/// Longest, not first: engines often print a header (`"384 dims:"`) before the
/// vector, and taking the first number there would return a one-element vector.
/// A run is a sequence of numbers separated only by whitespace, commas or
/// brackets; any other character ends it.
pub fn parse_vector(stdout: &str) -> Option<Vec<f32>> {
    let mut runs: Vec<Vec<f32>> = Vec::new();
    let mut run: Vec<f32> = Vec::new();
    let mut token = String::new();

    for ch in stdout.chars() {
        let is_number_char = ch.is_ascii_digit() || matches!(ch, '-' | '+' | '.' | 'e' | 'E');
        if is_number_char {
            token.push(ch);
            continue;
        }

        let is_separator = matches!(
            ch,
            ' ' | '\t' | '\n' | '\r' | ',' | '[' | ']' | '(' | ')' | '"' | ';'
        );

        if !token.is_empty() {
            if let Ok(value) = token.parse::<f32>() {
                if value.is_finite() {
                    run.push(value);
                }
            }
            token.clear();
        }
        if !is_separator {
            // A letter, a colon, a brace: the run is over.
            if run.len() >= 2 {
                runs.push(std::mem::take(&mut run));
            } else {
                run.clear();
            }
        }
    }

    if !token.is_empty() {
        if let Ok(value) = token.parse::<f32>() {
            if value.is_finite() {
                run.push(value);
            }
        }
    }
    if run.len() >= 2 {
        runs.push(run);
    }

    runs.into_iter().max_by_key(|candidate| candidate.len())
}

fn parse_command(command: &str) -> Option<Provider> {
    let mut parts = command.split_whitespace();
    let program = parts.next()?.to_string();
    let args: Vec<String> = parts.map(|part| part.to_string()).collect();
    Some(Provider::Command { program, args })
}

/// True when the program resolves to something on `PATH` or an absolute path.
fn program_available(provider: &Provider) -> bool {
    let Provider::Command { program, .. } = provider else {
        return false;
    };
    if program.contains('/') || program.contains('\\') {
        return Path::new(program).is_file();
    }
    find_on_path(program).is_some()
}

/// Minimal `which`: walks `PATH` looking for an executable file.
pub fn find_on_path(name: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate: PathBuf = dir.join(name);
        if is_executable(&candidate) {
            return Some(candidate.to_string_lossy().to_string());
        }
        #[cfg(windows)]
        {
            let exe = dir.join(format!("{name}.exe"));
            if is_executable(&exe) {
                return Some(exe.to_string_lossy().to_string());
            }
        }
    }
    None
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match std::fs::metadata(path) {
            Ok(meta) => meta.is_file() && meta.permissions().mode() & 0o111 != 0,
            Err(_) => false,
        }
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(engine: EmbeddingEngine, command: &str) -> EmbeddingConfig {
        EmbeddingConfig {
            engine,
            command: command.to_string(),
            ..EmbeddingConfig::default()
        }
    }

    #[test]
    fn off_engine_never_spawns() {
        let embedder = Embedder::from_config(&config(EmbeddingEngine::Off, "definitely-not-a-program"));
        assert!(!embedder.is_available());
        assert!(embedder.embed("hello").is_none());
        assert!(embedder.describe().starts_with("off"));
    }

    #[test]
    fn parses_json_and_plain_output() {
        assert_eq!(
            parse_vector("[0.1, 0.2, -0.3]").unwrap(),
            vec![0.1, 0.2, -0.3]
        );
        assert_eq!(parse_vector("0.5 -0.5 1").unwrap(), vec![0.5, -0.5, 1.0]);
        assert!(parse_vector("no numbers here").is_none());
        assert!(parse_vector("7").is_none(), "a single number is not a vector");
    }

    #[test]
    fn cosine_is_sane() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        let c = vec![0.0, 1.0, 0.0];
        assert!((cosine(&a, &b) - 1.0).abs() < 1e-6);
        assert!(cosine(&a, &c).abs() < 1e-6);
        assert_eq!(cosine(&a, &[1.0]), 0.0);
        assert_eq!(cosine(&[], &[]), 0.0);
    }

    #[test]
    fn a_missing_program_is_unavailable() {
        let embedder = Embedder::from_config(&config(
            EmbeddingEngine::Command,
            "/nonexistent/embedding-tool --flag",
        ));
        assert!(embedder.embed("text").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn spawns_a_command_and_reads_its_vector() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-embed.sh");
        let mut file = std::fs::File::create(&script).unwrap();
        // Prints a fixed 4-dim vector regardless of input.
        writeln!(file, "#!/bin/sh").unwrap();
        writeln!(file, "echo '[0.25, 0.25, 0.25, 0.25]'").unwrap();
        drop(file);
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut cfg = config(
            EmbeddingEngine::Command,
            script.to_str().expect("temp path is utf-8"),
        );
        cfg.dimensions = 4;
        let embedder = Embedder::from_config(&cfg);

        assert!(embedder.is_available());
        let vector = embedder.embed("anything").expect("script should answer");
        assert_eq!(vector, vec![0.25, 0.25, 0.25, 0.25]);
        assert!((cosine(&vector, &vector) - 1.0).abs() < 1e-6);
        assert_eq!(embedder.dimensions(), 4);
        assert!((embedder.threshold() - 0.92).abs() < 1e-6);
    }

    #[cfg(unix)]
    #[test]
    fn a_failing_command_yields_nothing() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fail.sh");
        let mut file = std::fs::File::create(&script).unwrap();
        writeln!(file, "#!/bin/sh").unwrap();
        writeln!(file, "echo 'model not found' >&2").unwrap();
        writeln!(file, "exit 3").unwrap();
        drop(file);
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let embedder = Embedder::from_config(&config(
            EmbeddingEngine::Command,
            script.to_str().expect("temp path is utf-8"),
        ));
        assert!(embedder.embed("text").is_none());
    }

    #[test]
    fn empty_text_is_not_embedded() {
        let embedder = Embedder::from_config(&config(EmbeddingEngine::Command, "crispembed"));
        assert!(embedder.embed("   ").is_none());
    }
}
