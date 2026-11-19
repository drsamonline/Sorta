//! The tray and dropbox GUI (egui).
//!
//! Not in this build. The engine is complete enough to drive a UI, so the shape is
//! fixed here and the phase-3 implementation drops in behind it:
//!
//! ```text
//! tray start  →  watcher spawn  →  dropbox lazy-load on first interaction
//! ```
//!
//! Tray-first matters: the tray must appear in under 300 ms, so nothing heavy may
//! run before it. The dropbox is a frameless, always-on-top 120×120 px window
//! with a collapsible panel (recent sorts, quarantine queue, time-travel slider,
//! search), a global `Ctrl+Shift+S` toggle, and multi-monitor placement.
//!
//! Calling [`run_dropbox`] in this build returns an error that says exactly that,
//! rather than opening a blank window.

use std::path::PathBuf;

use crate::config::LoadedConfig;

/// Whether a GUI is compiled into this build.
pub fn available() -> bool {
    false
}

/// Starts the tray and dropbox.
///
/// Returns an error in phase 1: the egui/tray-icon dependency is deliberately not
/// pulled in until the engine surface is stable, so the binary stays under the
/// 20 MB budget while the core is being built.
pub fn run_dropbox(_loaded: &LoadedConfig, _sandbox: Option<PathBuf>) -> anyhow::Result<()> {
    anyhow::bail!(
        "the tray/dropbox GUI is not built in this build (phase 3); \
         use `sorta sort`, `sorta scan` and `sorta watch` instead"
    )
}

/// A panel section the dropbox will render. Kept here so the data contract is
/// written down before the UI exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Panel {
    RecentSorts,
    QuarantineQueue,
    TimeTravel,
    Search,
}

impl Panel {
    /// Sections in the order they collapse.
    pub fn all() -> [Panel; 4] {
        [
            Panel::RecentSorts,
            Panel::QuarantineQueue,
            Panel::TimeTravel,
            Panel::Search,
        ]
    }

    pub fn title(&self) -> &'static str {
        match self {
            Panel::RecentSorts => "Recent sorts",
            Panel::QuarantineQueue => "Needs review",
            Panel::TimeTravel => "Time travel",
            Panel::Search => "Search",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gui_is_honestly_unavailable() {
        assert!(!available());
    }

    #[test]
    fn panels_have_titles() {
        for panel in Panel::all() {
            assert!(!panel.title().is_empty());
        }
    }
}
