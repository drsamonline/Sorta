//! Sorta — ultra-light semantic document sorter.
//!
//! The crate is deliberately split so that the engine can be driven by the CLI
//! today and by the tray/dropbox UI later without changes:
//!
//! * [`config`] — configuration resolution and the full key set
//! * [`detect`] — what a file is (magic bytes → extension → content sniff)
//! * [`safety`] — everything that can destroy data if it is wrong
//! * [`sort`] — rules that decide where a file belongs
//! * [`pipeline`] — the transactional move, start to receipt
//! * [`history`] — append-only log, undo, "why here"
//!
//! Nothing in the default build performs network I/O.

pub mod cli;
pub mod config;
pub mod dedupe;
pub mod detect;
pub mod errors;
pub mod extract;
pub mod hash;
pub mod history;
pub mod instance;
pub mod llm;
pub mod paths;
pub mod pipeline;
pub mod platform;
pub mod quarantine;
pub mod receipt;
pub mod rename;
pub mod safety;
pub mod scan;
pub mod sort;
pub mod ui;
pub mod watcher;

/// Crate version, surfaced by `sorta --version` and the receipt footer.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Formats the crate version as a single line banner.
pub fn banner() -> String {
    format!("sorta {VERSION}")
}
