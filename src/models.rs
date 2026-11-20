//! The model registry: what Sorta expects to find, and whether it is there.
//!
//! Sorta ships no weights. The tier ladder in [`crate::llm`] is inert until a model
//! file exists, and that is deliberate — a sorter that guesses at intent is worse
//! than one that files by format and says so.
//!
//! What this module adds is the part that does not need weights: a manifest of the
//! six Tier-0 models, each with its purpose, input shape, licence and source, so that
//! dropping a converted file into `models/` is the only step left. `sorta models`
//! reports what is present, what is pinned, and what is missing.
//!
//! Digests are BLAKE3, like every other hash in this crate, rather than the SHA-256
//! the blueprint mentioned: one hash function in a codebase is one fewer thing to get
//! wrong, and BLAKE3 is already a dependency.
//!
//! A model with an empty digest is *unpinned*: it can be present and usable, but
//! `--verify` will say it was never checked against anything. That is honest about
//! the current state instead of pretending the files were vetted.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One model Sorta knows how to use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSpec {
    /// Stable id, used in the feature name and in the report.
    pub id: String,
    /// File name expected inside the models directory.
    pub file: String,
    /// What it is for.
    pub purpose: String,
    /// Input tensor shape, in words.
    pub input: String,
    /// Output, in words.
    pub output: String,
    /// BLAKE3 of the file, or empty when nobody has pinned it yet.
    pub digest: String,
    /// Licence of the upstream weights.
    pub licence: String,
    /// Where the weights come from.
    pub source: String,
    /// Cargo feature that compiles the code path for this model.
    pub feature: String,
}

/// The models the blueprint calls for, in the order they matter.
pub fn builtin_specs() -> Vec<ModelSpec> {
    vec![
        ModelSpec {
            id: "intent-deberta".to_string(),
            file: "intent-deberta.rten".to_string(),
            purpose: "document intent (invoice, contract, statement, letter)".to_string(),
            input: "text, up to 512 tokens".to_string(),
            output: "label + confidence".to_string(),
            digest: String::new(),
            licence: "MIT".to_string(),
            source: "microsoft/deberta-v3-xsmall via Optimum + rten-convert".to_string(),
            feature: "model-intent-deberta".to_string(),
        },
        ModelSpec {
            id: "embed-minilm".to_string(),
            file: "embed-minilm.rten".to_string(),
            purpose: "384-dimension embeddings for clustering and near-duplicates".to_string(),
            input: "text, up to 256 tokens".to_string(),
            output: "384 f32".to_string(),
            digest: String::new(),
            licence: "Apache-2.0".to_string(),
            source: "sentence-transformers/all-MiniLM-L6-v2".to_string(),
            feature: "model-embed-minilm".to_string(),
        },
        ModelSpec {
            id: "ner-gliner".to_string(),
            file: "ner-gliner.rten".to_string(),
            purpose: "entities: client, date, amount, reference".to_string(),
            input: "text + label set".to_string(),
            output: "spans per label".to_string(),
            digest: String::new(),
            licence: "Apache-2.0".to_string(),
            source: "urchade/gliner_multi-v2.1".to_string(),
            feature: "model-ner-gliner".to_string(),
        },
        ModelSpec {
            id: "ocr-det".to_string(),
            file: "ocr-det.rten".to_string(),
            purpose: "find text boxes in a scanned page".to_string(),
            input: "image, 960 px long side".to_string(),
            output: "boxes + scores".to_string(),
            digest: String::new(),
            licence: "Apache-2.0".to_string(),
            source: "PP-OCRv4 det (PaddleOCR)".to_string(),
            feature: "model-ocr-rapid".to_string(),
        },
        ModelSpec {
            id: "ocr-rec".to_string(),
            file: "ocr-rec.rten".to_string(),
            purpose: "read a text box".to_string(),
            input: "cropped image, 48 px height".to_string(),
            output: "text + score".to_string(),
            digest: String::new(),
            licence: "Apache-2.0".to_string(),
            source: "PP-OCRv4 rec (PaddleOCR)".to_string(),
            feature: "model-ocr-rapid".to_string(),
        },
        ModelSpec {
            id: "extract-sanga".to_string(),
            file: "extract-sanga.rten".to_string(),
            purpose: "field extraction for known document types".to_string(),
            input: "text".to_string(),
            output: "field map".to_string(),
            digest: String::new(),
            licence: "to be confirmed".to_string(),
            source: "Sanga, see docs/ROADMAP.md".to_string(),
            feature: "model-extract-sanga".to_string(),
        },
    ]
}

/// What state one model is in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelStatus {
    pub spec: ModelSpec,
    pub path: PathBuf,
    pub present: bool,
    pub bytes: u64,
    /// `Some(true)` when a pinned digest matches, `Some(false)` when it does not,
    /// `None` when the spec has no digest to compare against.
    pub digest_ok: Option<bool>,
}

impl ModelStatus {
    pub fn label(&self) -> &'static str {
        match (self.present, self.digest_ok) {
            (false, _) => "missing",
            (true, Some(true)) => "present, verified",
            (true, Some(false)) => "present, DIGEST MISMATCH",
            (true, None) => "present, unpinned",
        }
    }
}

/// Status for every known model inside `directory`.
pub fn status(directory: &Path) -> Vec<ModelStatus> {
    builtin_specs()
        .into_iter()
        .map(|spec| {
            let path = directory.join(&spec.file);
            let (present, bytes) = match std::fs::metadata(&path) {
                Ok(metadata) if metadata.is_file() => (true, metadata.len()),
                _ => (false, 0),
            };

            let digest_ok = if present && !spec.digest.is_empty() {
                Some(digest_of(&path).map(|found| found == spec.digest).unwrap_or(false))
            } else {
                None
            };

            ModelStatus {
                spec,
                path,
                present,
                bytes,
                digest_ok,
            }
        })
        .collect()
}

/// BLAKE3 of a file, prefixed like every other digest in the crate.
pub fn digest_of(path: &Path) -> anyhow::Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("b3:{}", hasher.finalize().to_hex()))
}

/// Totals for the models directory.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSummary {
    pub known: usize,
    pub present: usize,
    pub verified: usize,
    pub mismatched: usize,
    pub bytes: u64,
}

pub fn summarise(statuses: &[ModelStatus]) -> ModelSummary {
    let mut summary = ModelSummary {
        known: statuses.len(),
        ..ModelSummary::default()
    };
    for status in statuses {
        if status.present {
            summary.present += 1;
            summary.bytes += status.bytes;
        }
        match status.digest_ok {
            Some(true) => summary.verified += 1,
            Some(false) => summary.mismatched += 1,
            None => {}
        }
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_spec_is_complete_enough_to_act_on() {
        for spec in builtin_specs() {
            assert!(!spec.id.is_empty(), "id");
            assert!(spec.file.ends_with(".rten"), "{} file", spec.id);
            assert!(!spec.purpose.is_empty(), "{} purpose", spec.id);
            assert!(!spec.source.is_empty(), "{} source", spec.id);
            assert!(
                spec.feature.starts_with("model-"),
                "{} feature name",
                spec.id
            );
        }
    }

    #[test]
    fn an_empty_directory_reports_everything_missing() {
        let dir = tempfile::tempdir().unwrap();
        let statuses = status(dir.path());
        assert_eq!(statuses.len(), builtin_specs().len());
        assert!(statuses.iter().all(|s| !s.present));
        assert!(statuses.iter().all(|s| s.digest_ok.is_none()));
        assert_eq!(statuses[0].label(), "missing");

        let summary = summarise(&statuses);
        assert_eq!(summary.present, 0);
        assert_eq!(summary.known, statuses.len());
    }

    #[test]
    fn a_present_model_without_a_pinned_digest_says_so() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("embed-minilm.rten"), b"not really a model").unwrap();

        let statuses = status(dir.path());
        let embed = statuses
            .iter()
            .find(|s| s.spec.id == "embed-minilm")
            .expect("the spec exists");
        assert!(embed.present);
        assert_eq!(embed.bytes, 17);
        assert_eq!(embed.digest_ok, None);
        assert_eq!(embed.label(), "present, unpinned");
    }

    #[test]
    fn a_pinned_digest_is_checked_both_ways() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("embed-minilm.rten");
        std::fs::write(&path, b"weights").unwrap();
        let good = digest_of(&path).unwrap();
        assert!(good.starts_with("b3:"));

        // Pin the correct digest, then a wrong one, by rewriting the spec list.
        let mut specs = builtin_specs();
        let index = specs.iter().position(|s| s.id == "embed-minilm").unwrap();
        specs[index].digest = good.clone();

        let matching = check(&dir, &specs[index]);
        assert_eq!(matching.digest_ok, Some(true));
        assert_eq!(matching.label(), "present, verified");

        let mut wrong = specs[index].clone();
        wrong.digest = "b3:0000".to_string();
        let mismatched = check(&dir, &wrong);
        assert_eq!(mismatched.digest_ok, Some(false));
        assert_eq!(mismatched.label(), "present, DIGEST MISMATCH");
    }

    #[test]
    fn the_summary_counts_verified_and_mismatched_separately() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ocr-det.rten");
        std::fs::write(&path, b"det").unwrap();
        let good = digest_of(&path).unwrap();

        let mut spec = builtin_specs()
            .into_iter()
            .find(|s| s.id == "ocr-det")
            .unwrap();
        spec.digest = good;
        let verified = check(&dir, &spec);

        let mut bad = spec.clone();
        bad.digest = "b3:beef".to_string();
        let mismatched = check(&dir, &bad);

        let summary = summarise(&[verified, mismatched]);
        assert_eq!(summary.present, 2);
        assert_eq!(summary.verified, 1);
        assert_eq!(summary.mismatched, 1);
        assert_eq!(summary.bytes, 6);
    }

    /// Same logic as [`status`], for one spec — so the digest paths are testable
    /// without a file on disk per model.
    fn check(directory: &Path, spec: &ModelSpec) -> ModelStatus {
        let path = directory.join(&spec.file);
        let (present, bytes) = match std::fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => (true, metadata.len()),
            _ => (false, 0),
        };
        let digest_ok = if present && !spec.digest.is_empty() {
            Some(digest_of(&path).map(|found| found == spec.digest).unwrap_or(false))
        } else {
            None
        };
        ModelStatus {
            spec: spec.clone(),
            path,
            present,
            bytes,
            digest_ok,
        }
    }
}
