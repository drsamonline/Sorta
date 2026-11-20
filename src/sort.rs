//! Rules that decide where a file belongs.
//!
//! A rule is `(id, requires, priority, min_confidence, destination template)`.
//! The classifier runs them in priority order and takes the first match that
//! clears its own confidence floor. Three families exist:
//!
//! * **format** — the file family decides the folder. Always available (Tier 0).
//! * **keyword** — a case-insensitive substring from `[keyword_rules]`.
//!   Always available.
//! * **semantic** — embedding cluster or LLM intent. Tagged `Requires::Semantic`
//!   or `Requires::Llm`, so a Tier-0 build skips them instead of failing.
//!
//! Confidence is what the pipeline's `min_confidence` gate acts on. A file whose
//! best rule lands below the gate goes to quarantine rather than into a folder
//! Sorta guessed at.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::config::{Config, KeywordScope};
use crate::detect::{Detected, Kind, Method};

/// What a rule needs to be usable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Requires {
    /// Pure Rust, no model, always available.
    None,
    /// Embedding index (semantic clustering, near-duplicate detection).
    Semantic,
    /// A language model (natural-language rules, multi-step workflows).
    Llm,
}

impl Requires {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Semantic => "semantic",
            Self::Llm => "llm",
        }
    }
}

/// A single rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    pub id: String,
    pub requires: Requires,
    /// Lower runs first.
    pub priority: u8,
    /// A match below this confidence is not accepted.
    pub min_confidence: f32,
    /// Destination template, relative to `destination_root`.
    pub dest: String,
}

/// The verdict for one file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Classified {
    /// Rule that produced this verdict, e.g. `keyword:Invoice`.
    pub rule_id: String,
    pub requires: Requires,
    /// Coarse category, used by the `{category}` template token.
    pub category: String,
    /// Destination relative to the destination root.
    pub dest_rel: String,
    pub confidence: f32,
    /// Human-readable evidence, logged into history.
    pub signals: Vec<String>,
}

impl Classified {
    /// One-line explanation, templated — no model involved.
    pub fn explain(&self) -> String {
        match self.rule_id.split(':').next().unwrap_or("rule") {
            "keyword" => format!(
                "the name matched the keyword rule '{}' ({})",
                self.rule_id.split(':').nth(1).unwrap_or(""),
                self.signals.join(", ")
            ),
            "format" => format!(
                "this is a {} file, so it goes to {}",
                self.category, self.dest_rel
            ),
            "semantic" => format!(
                "it clustered with similar documents (similarity {})",
                self.signals.join(", ")
            ),
            _ => format!(
                "no rule was confident enough; best guess was {} at {:.0}%",
                self.category,
                self.confidence * 100.0
            ),
        }
    }
}

/// Everything the classifier needs at construction time.
#[derive(Debug, Clone)]
pub struct Classifier {
    /// Keyword rules, pre-lowercased for matching.
    keywords: Vec<(String, String)>,
    folder_template: String,
    min_confidence: f32,
    keyword_scope: KeywordScope,
}

impl Classifier {
    pub fn new(cfg: &Config) -> Self {
        let mut keywords: Vec<(String, String)> = cfg
            .keyword_rules
            .iter()
            .map(|(k, v)| (k.to_lowercase(), v.clone()))
            .collect();
        // Deterministic order: longest keyword first, so "purchase order" beats "order".
        keywords.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(a.0.cmp(&b.0)));
        Self {
            keywords,
            folder_template: cfg.folder_template.clone(),
            min_confidence: cfg.min_confidence,
            keyword_scope: cfg.keyword_scope,
        }
    }

    pub fn min_confidence(&self) -> f32 {
        self.min_confidence
    }

    /// Classifies a file from its detection result, name, optional extracted text
    /// and the current year (used by the `{year}` template token).
    pub fn classify(
        &self,
        detected: &Detected,
        file_name: &str,
        text: Option<&str>,
        year: &str,
    ) -> Classified {
        let name_lower = file_name.to_lowercase();

        // 1. Keyword in the filename — strongest cheap signal.
        for (keyword, folder) in &self.keywords {
            if name_lower.contains(keyword.as_str()) {
                return self.build(
                    format!("keyword:{}", keyword),
                    Requires::None,
                    folder.clone(),
                    0.82,
                    vec![format!("keyword:{keyword}"), format!("name:{file_name}")],
                    detected,
                    year,
                );
            }
        }

        // 2. Keyword in the extracted text — weaker, because text is noisier, and off
        // by default.
        //
        // A measured reason: over 123 real files, the default `Contract` rule matched
        // the word inside licence and contributing files and filed 14 of them into
        // `Contracts`. Body matching is useful, but it has to be asked for.
        let body_text = if self.keyword_scope == KeywordScope::NameAndText {
            text
        } else {
            None
        };
        if let Some(body) = body_text {
            let body_lower = body.to_lowercase();
            for (keyword, folder) in &self.keywords {
                if body_lower.contains(keyword.as_str()) {
                    return self.build(
                        format!("keyword:{}", keyword),
                        Requires::None,
                        folder.clone(),
                        0.74,
                        vec![format!("keyword:{keyword}"), "text:matched".to_string()],
                        detected,
                        year,
                    );
                }
            }
        }

        // 3. Format family — reliable, but says nothing about meaning.
        if detected.kind != Kind::Other {
            return self.build(
                format!("format:{}", kind_token(detected.kind)),
                Requires::None,
                detected.kind.category().to_string(),
                0.9,
                vec![
                    format!("format:{}", kind_token(detected.kind)),
                    format!("method:{}", method_token(detected)),
                ],
                detected,
                year,
            );
        }

        // 4. Nothing matched. Below any sane gate, so this lands in quarantine.
        self.build(
            "fallback:unknown".to_string(),
            Requires::None,
            Kind::Other.category().to_string(),
            0.45,
            vec!["format:unknown".to_string()],
            detected,
            year,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build(
        &self,
        rule_id: String,
        requires: Requires,
        category: String,
        confidence: f32,
        signals: Vec<String>,
        detected: &Detected,
        year: &str,
    ) -> Classified {
        let month = chrono::Local::now().format("%m").to_string();
        let dest_rel = render_template(
            &self.folder_template,
            &TemplateVars {
                year,
                month: &month,
                category: &category,
                kind: kind_token(detected.kind),
                ext: detected.extension.as_deref().unwrap_or(""),
            },
        );
        Classified {
            rule_id,
            requires,
            category,
            dest_rel,
            confidence,
            signals,
        }
    }
}

fn kind_token(kind: Kind) -> &'static str {
    match kind {
        Kind::Document => "document",
        Kind::Spreadsheet => "spreadsheet",
        Kind::Presentation => "presentation",
        Kind::Image => "image",
        Kind::Audio => "audio",
        Kind::Video => "video",
        Kind::Archive => "archive",
        Kind::Text => "text",
        Kind::Code => "code",
        Kind::Other => "unknown",
    }
}

fn method_token(detected: &Detected) -> &'static str {
    use crate::detect::Method::*;
    match detected.method {
        Magic => "magic",
        Extension => "extension",
        ContentSniff => "content_sniff",
        Unknown => "unknown",
    }
}

/// Values available to a destination template.
pub struct TemplateVars<'a> {
    pub year: &'a str,
    pub month: &'a str,
    pub category: &'a str,
    pub kind: &'a str,
    pub ext: &'a str,
}

/// Renders `{year}/{category}` style templates.
///
/// Unknown tokens are left as-is rather than blanked, so a typo in a template is
/// visible in the dry-run instead of silently creating a folder called `""`.
pub fn render_template(template: &str, vars: &TemplateVars<'_>) -> String {
    let mut out = template.to_string();
    for (token, value) in [
        ("{year}", vars.year),
        ("{month}", vars.month),
        ("{category}", vars.category),
        ("{kind}", vars.kind),
        ("{ext}", vars.ext),
    ] {
        out = out.replace(token, value);
    }
    // Collapse accidental empty path segments: "2026//Documents" -> "2026/Documents".
    let parts: Vec<&str> = out
        .split(['/', '\\'])
        .filter(|p| !p.trim().is_empty())
        .collect();
    parts.join("/")
}

/// The rule catalogue as it would appear in the UI, including rules that the
/// current tier cannot run. Hidden rules are the reason the UI needs a badge.
pub fn catalogue(cfg: &Config) -> Vec<Rule> {
    let mut rules = Vec::new();
    let mut priority = 0u8;
    for (keyword, folder) in cfg.keyword_rules.iter() {
        rules.push(Rule {
            id: format!("keyword:{keyword}"),
            requires: Requires::None,
            priority,
            min_confidence: cfg.min_confidence,
            dest: folder.clone(),
        });
        priority = priority.saturating_add(10);
    }
    for kind in [
        Kind::Document,
        Kind::Spreadsheet,
        Kind::Presentation,
        Kind::Image,
        Kind::Audio,
        Kind::Video,
        Kind::Archive,
        Kind::Text,
        Kind::Code,
    ] {
        rules.push(Rule {
            id: format!("format:{}", kind_token(kind)),
            requires: Requires::None,
            priority: priority.saturating_add(1),
            min_confidence: cfg.min_confidence,
            dest: kind.category().to_string(),
        });
    }
    rules
}

/// What is wrong with a rule set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictKind {
    /// Two keywords differ only by case, so one can never win.
    CaseInsensitiveDuplicate,
    /// A keyword points at no folder.
    EmptyDestination,
    /// A keyword short enough to match inside unrelated words.
    VeryShortKeyword,
}

impl ConflictKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::CaseInsensitiveDuplicate => "duplicate keyword",
            Self::EmptyDestination => "empty destination",
            Self::VeryShortKeyword => "over-broad keyword",
        }
    }
}

/// One problem in the rule set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub kind: ConflictKind,
    /// Rule ids involved.
    pub rules: Vec<String>,
    pub detail: String,
}

/// Finds problems in the keyword rules before they start losing files.
///
/// This is the check the spec asks for: a rule that can never fire, or one that
/// fires on everything, is a bug the user should see in the dry run rather than in
/// a mis-sorted invoice three weeks later.
pub fn conflicts(cfg: &Config) -> Vec<Conflict> {
    let mut out: Vec<Conflict> = Vec::new();
    let mut seen: BTreeMap<String, String> = BTreeMap::new();

    for (keyword, destination) in &cfg.keyword_rules {
        let lowered = keyword.to_lowercase();

        if let Some(previous) = seen.get(&lowered) {
            out.push(Conflict {
                kind: ConflictKind::CaseInsensitiveDuplicate,
                rules: vec![format!("keyword:{previous}"), format!("keyword:{keyword}")],
                detail: format!(
                    "'{previous}' and '{keyword}' match the same text; only one of them can win"
                ),
            });
        } else {
            seen.insert(lowered.clone(), keyword.clone());
        }

        if destination.trim().is_empty() {
            out.push(Conflict {
                kind: ConflictKind::EmptyDestination,
                rules: vec![format!("keyword:{keyword}")],
                detail: format!("'{keyword}' points at an empty folder name"),
            });
        }

        let length = lowered.chars().count();
        if length < 3 {
            out.push(Conflict {
                kind: ConflictKind::VeryShortKeyword,
                rules: vec![format!("keyword:{keyword}")],
                detail: format!(
                    "'{keyword}' is {length} character(s) and will match inside unrelated words"
                ),
            });
        }
    }

    out
}

/// Classifies a bare filename, for `sorta test-rule`.
///
/// Detection is inferred from the extension and the optional body text, which is
/// exactly what the harness promises: "given this name, what would Sorta do?".
pub fn preview_name(cfg: &Config, file_name: &str, body: Option<&str>) -> Classified {
    let extension = std::path::Path::new(file_name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    let kind = extension
        .as_deref()
        .map(crate::detect::kind_from_extension)
        .unwrap_or(Kind::Other);

    let detected = Detected {
        mime: "application/octet-stream".to_string(),
        extension,
        kind,
        method: Method::Extension,
        is_text: body.is_some(),
    };

    let year = chrono::Local::now().format("%Y").to_string();
    Classifier::new(cfg).classify(&detected, file_name, body, &year)
}

/// True when a rule can run at the given tier.
pub fn is_available(requires: Requires, semantic_enabled: bool, llm_available: bool) -> bool {
    match requires {
        Requires::None => true,
        Requires::Semantic => semantic_enabled,
        Requires::Llm => llm_available,
    }
}

/// Grouped view of the keyword rules, for `sorta config show`.
pub fn keyword_table(cfg: &Config) -> BTreeMap<String, String> {
    cfg.keyword_rules.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Detected, Method};

    fn detected(kind: Kind, ext: Option<&str>) -> Detected {
        Detected {
            mime: "application/pdf".to_string(),
            extension: ext.map(|e| e.to_string()),
            kind,
            method: Method::Magic,
            is_text: false,
        }
    }

    #[test]
    fn keyword_in_name_wins() {
        let cfg = Config::default();
        let classifier = Classifier::new(&cfg);
        let verdict = classifier.classify(
            &detected(Kind::Document, Some("pdf")),
            "Acme Invoice 2026-11.pdf",
            None,
            "2026",
        );
        assert_eq!(verdict.rule_id, "keyword:invoice");
        assert_eq!(verdict.dest_rel, "2026/PDF-Invoices");
        assert!(verdict.confidence > 0.8);
    }

    #[test]
    fn format_family_is_the_fallback() {
        let cfg = Config::default();
        let classifier = Classifier::new(&cfg);
        let verdict = classifier.classify(
            &detected(Kind::Image, Some("png")),
            "screenshot.png",
            None,
            "2026",
        );
        assert_eq!(verdict.rule_id, "format:image");
        assert_eq!(verdict.dest_rel, "2026/Images");
        assert!((verdict.confidence - 0.9).abs() < f32::EPSILON);
    }

    #[test]
    fn unknown_files_score_below_the_gate() {
        let cfg = Config::default();
        let classifier = Classifier::new(&cfg);
        let verdict = classifier.classify(&detected(Kind::Other, None), "blob", None, "2026");
        assert!(verdict.confidence < cfg.min_confidence);
        assert_eq!(verdict.rule_id, "fallback:unknown");
    }

    #[test]
    fn body_text_is_ignored_by_default() {
        // Measured reason: with body matching on, the default `Contract` rule filed
        // 14 of 123 corpus files (licences, contributing guides) into `Contracts`.
        let cfg = Config::default();
        let classifier = Classifier::new(&cfg);
        let verdict = classifier.classify(
            &detected(Kind::Document, Some("pdf")),
            "scan0001.pdf",
            Some("This page is about a contract between parties"),
            "2026",
        );
        assert_eq!(verdict.rule_id, "format:document");
        assert_eq!(verdict.category, "Documents");
    }

    #[test]
    fn text_keyword_matches_are_weaker_than_name_matches() {
        let cfg = Config {
            keyword_scope: KeywordScope::NameAndText,
            ..Config::default()
        };
        let classifier = Classifier::new(&cfg);
        let verdict = classifier.classify(
            &detected(Kind::Document, Some("pdf")),
            "scan0001.pdf",
            Some("This page is about a contract between parties"),
            "2026",
        );
        assert_eq!(verdict.rule_id, "keyword:contract");
        assert!(verdict.confidence < 0.8);
    }

    #[test]
    fn templates_render_and_collapse_empty_segments() {
        let vars = TemplateVars {
            year: "2026",
            month: "11",
            category: "Images",
            kind: "image",
            ext: "png",
        };
        assert_eq!(render_template("{year}/{category}", &vars), "2026/Images");
        assert_eq!(render_template("{year}//{category}/", &vars), "2026/Images");
        assert_eq!(render_template("{unknown}", &vars), "{unknown}");
    }

    #[test]
    fn conflicts_are_found() {
        let mut cfg = Config::default();
        cfg.keyword_rules.clear();
        cfg.keyword_rules
            .insert("Invoice".to_string(), "PDF-Invoices".to_string());
        cfg.keyword_rules
            .insert("invoice".to_string(), "Elsewhere".to_string());
        cfg.keyword_rules.insert("X".to_string(), "".to_string());

        let found = conflicts(&cfg);
        let kinds: Vec<ConflictKind> = found.iter().map(|c| c.kind).collect();
        assert!(kinds.contains(&ConflictKind::CaseInsensitiveDuplicate));
        assert!(kinds.contains(&ConflictKind::VeryShortKeyword));
        assert!(kinds.contains(&ConflictKind::EmptyDestination));
    }

    #[test]
    fn a_clean_rule_set_has_no_conflicts() {
        let cfg = Config::default();
        assert!(conflicts(&cfg).is_empty());
    }

    #[test]
    fn preview_answers_what_would_happen() {
        let cfg = Config::default();
        let verdict = preview_name(&cfg, "Acme Invoice 2026.pdf", None);
        assert_eq!(verdict.rule_id, "keyword:invoice");
        assert!(verdict.dest_rel.contains("PDF-Invoices"));

        let unknown = preview_name(&cfg, "blob.bin", None);
        assert!(unknown.confidence < cfg.min_confidence);
    }

    #[test]
    fn tier_availability() {
        assert!(is_available(Requires::None, false, false));
        assert!(!is_available(Requires::Semantic, false, false));
        assert!(is_available(Requires::Semantic, true, false));
        assert!(!is_available(Requires::Llm, true, false));
        assert!(is_available(Requires::Llm, true, true));
    }
}
