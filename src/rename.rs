//! Semantic auto-rename.
//!
//! The target shape is `{client}_{doctype}_{date}`. Renaming is *proposed*, not
//! imposed: a name is only rendered when every non-optional token has a value
//! from a real signal (keyword rule, calendar event, client domain, extracted
//! field). A half-empty name like `_invoice_2026-11-19.pdf` is worse than the
//! original, so it is never produced.

use serde::{Deserialize, Serialize};

use crate::safety::sanitize_filename;

/// Default rename pattern, per the spec.
pub const DEFAULT_PATTERN: &str = "{client}_{doctype}_{date}";

/// Values available to a rename pattern.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RenameContext {
    /// Client or organisation, from a keyword rule, calendar event or domain.
    pub client: Option<String>,
    /// Document type, from a keyword rule or extracted field.
    pub doctype: Option<String>,
    /// ISO date, `YYYY-MM-DD`.
    pub date: String,
    /// Original stem, always available.
    pub stem: String,
    /// Original extension without the dot.
    pub ext: String,
}

impl RenameContext {
    pub fn new(stem: &str, ext: &str) -> Self {
        Self {
            client: None,
            doctype: None,
            date: chrono::Local::now().format("%Y-%m-%d").to_string(),
            stem: stem.to_string(),
            ext: ext.to_string(),
        }
    }

    fn value(&self, token: &str) -> Option<String> {
        match token {
            "client" => self.client.clone(),
            "doctype" => self.doctype.clone(),
            "date" => Some(self.date.clone()),
            "year" => Some(self.date.chars().take(4).collect()),
            "month" => Some(self.date.chars().skip(5).take(2).collect()),
            "day" => Some(self.date.chars().skip(8).take(2).collect()),
            "stem" => Some(self.stem.clone()),
            "ext" => Some(self.ext.clone()),
            _ => None,
        }
    }
}

/// Renders a rename pattern.
///
/// Returns `None` when a required token has no value — the caller then keeps the
/// original name. Empty optional tokens collapse instead of leaving `__` behind.
pub fn render_pattern(pattern: &str, ctx: &RenameContext) -> Option<String> {
    let mut out = pattern.to_string();
    for token in [
        "client", "doctype", "date", "year", "month", "day", "stem", "ext",
    ] {
        let placeholder = format!("{{{token}}}");
        if !out.contains(&placeholder) {
            continue;
        }
        match ctx.value(token) {
            Some(v) if !v.trim().is_empty() => out = out.replace(&placeholder, v.trim()),
            // `stem` and `ext` are always present, so this branch is a missing
            // semantic token: refuse to rename rather than produce a stub.
            _ => return None,
        }
    }

    // Collapse leftover separators and empty groups.
    let collapsed: Vec<&str> = out
        .split('_')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if collapsed.is_empty() {
        return None;
    }
    Some(collapsed.join("_"))
}

/// Full filename for a file: `acme_invoice_2026-11-19.pdf`.
pub fn propose_filename(
    pattern: &str,
    ctx: &RenameContext,
    keep_extension: bool,
) -> Option<String> {
    let base = render_pattern(pattern, ctx)?;
    let name = if keep_extension && !ctx.ext.is_empty() {
        format!("{base}.{}", ctx.ext)
    } else {
        base
    };
    let safe = sanitize_filename(&name);
    if safe == "unnamed" {
        None
    } else {
        Some(safe)
    }
}

/// Whether a proposed name would actually change anything.
pub fn is_meaningful(proposed: &str, original: &str) -> bool {
    proposed.to_lowercase() != original.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> RenameContext {
        RenameContext {
            client: Some("Acme".to_string()),
            doctype: Some("Invoice".to_string()),
            date: "2026-11-19".to_string(),
            stem: "scan0001".to_string(),
            ext: "pdf".to_string(),
        }
    }

    #[test]
    fn default_pattern_renders() {
        assert_eq!(
            render_pattern(DEFAULT_PATTERN, &ctx()).unwrap(),
            "Acme_Invoice_2026-11-19"
        );
    }

    #[test]
    fn missing_client_refuses_to_rename() {
        let mut c = ctx();
        c.client = None;
        assert!(render_pattern(DEFAULT_PATTERN, &c).is_none());
    }

    #[test]
    fn extension_is_preserved_when_asked() {
        assert_eq!(
            propose_filename(DEFAULT_PATTERN, &ctx(), true).unwrap(),
            "Acme_Invoice_2026-11-19.pdf"
        );
    }

    #[test]
    fn empty_tokens_collapse() {
        let mut c = ctx();
        c.client = Some("   ".to_string());
        assert!(render_pattern("{stem}_{client}_{date}", &c).is_none());
    }

    #[test]
    fn meaningful_only_when_it_changes() {
        assert!(!is_meaningful("scan0001.pdf", "SCAN0001.pdf"));
        assert!(is_meaningful("Acme_Invoice_2026-11-19.pdf", "scan0001.pdf"));
    }
}
