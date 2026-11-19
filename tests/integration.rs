//! End-to-end tests through the public API.
//!
//! These exercise the promises the README makes: a file lands somewhere
//! predictable, the log explains it, undo puts it back, duplicates are never
//! deleted, and a dry run touches nothing.

use std::path::{Path, PathBuf};

use sorta::config::Config;
use sorta::history::History;
use sorta::pipeline::{Engine, Status};
use sorta::receipt::Receipt;
use sorta::scan;

fn engine(root: &Path) -> Engine {
    Engine::for_test(root, Config::default())
}

fn write(dir: &Path, name: &str, body: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    path
}

#[test]
fn a_pdf_is_filed_logged_and_undoable() {
    let tmp = tempfile::tempdir().unwrap();
    let inbox = tmp.path().join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();

    let mut engine = engine(tmp.path());
    let src = write(&inbox, "Acme Invoice 2026-11.pdf", b"%PDF-1.4 invoice");

    let outcome = engine.process(&src);
    assert_eq!(outcome.status, Status::Moved, "note: {}", outcome.note);
    let dst = PathBuf::from(outcome.dst.clone().unwrap());
    assert!(dst.exists());
    assert!(!src.exists());
    assert!(dst.to_string_lossy().contains("PDF-Invoices"));

    // The log explains it.
    let entry = engine.history.find_by_dst(&dst).expect("logged");
    assert_eq!(entry.rule, "keyword:invoice");
    assert!(entry.explain().contains("keyword:invoice"));

    // Undo puts it back and appends a compensating entry.
    let outcomes = engine.history.undo(&[entry]);
    assert!(outcomes[0].ok);
    assert!(src.exists());
    assert!(!dst.exists());
    assert!(engine
        .history
        .read_all()
        .iter()
        .any(|e| e.status == "undone"));
}

#[test]
fn a_bulk_scan_sorts_every_file_and_writes_a_receipt() {
    let tmp = tempfile::tempdir().unwrap();
    let inbox = tmp.path().join("inbox");
    std::fs::create_dir_all(inbox.join("nested")).unwrap();
    write(&inbox, "invoice-one.pdf", b"%PDF-1.4 one");
    write(&inbox, "contract-two.pdf", b"%PDF-1.4 two");
    write(
        &inbox.join("nested"),
        "photo.png",
        b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR",
    );
    write(&inbox.join("nested"), "notes.md", b"# notes\n");

    let mut engine = engine(tmp.path());
    let state_path = tmp.path().join("scan_state.json");
    let summary = scan::run(&mut engine, &inbox, &state_path, false).unwrap();

    assert_eq!(summary.found, 4);
    let moved = summary
        .outcomes
        .iter()
        .filter(|o| o.status == Status::Moved)
        .count();
    assert_eq!(moved, 4, "every file should have moved: {summary:?}");

    let receipt = Receipt::new(&engine.dest_root, summary.outcomes.clone(), false);
    let path = receipt.write(&engine.dest_root).unwrap();
    let text = std::fs::read_to_string(path).unwrap();
    assert!(text.contains("# Sorta receipt"));
    assert!(text.contains("| Moved | 4 |"));
}

#[test]
fn dry_run_is_a_promise_not_a_side_effect() {
    let tmp = tempfile::tempdir().unwrap();
    let inbox = tmp.path().join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    let src = write(&inbox, "invoice.pdf", b"%PDF-1.4 x");

    let mut engine = engine(tmp.path());
    engine.dry_run = true;
    let outcome = engine.process(&src);

    assert_eq!(outcome.status, Status::Planned);
    assert!(src.exists());
    assert!(
        !engine.dest_root.exists(),
        "dry run must not create folders"
    );
    assert_eq!(engine.stats.moved, 0);
}

#[test]
fn read_only_never_moves() {
    let tmp = tempfile::tempdir().unwrap();
    let inbox = tmp.path().join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    let src = write(&inbox, "invoice.pdf", b"%PDF-1.4 x");

    let mut engine = engine(tmp.path());
    engine.read_only = true;
    let outcome = engine.process(&src);

    assert_eq!(outcome.status, Status::Planned);
    assert!(src.exists());
}

#[test]
fn duplicate_content_is_reported_and_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let inbox = tmp.path().join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    let a = write(&inbox, "a invoice.pdf", b"%PDF-1.4 same bytes");
    let b = write(&inbox, "b invoice.pdf", b"%PDF-1.4 same bytes");

    let mut engine = engine(tmp.path());
    assert_eq!(engine.process(&a).status, Status::Moved);

    let second = engine.process(&b);
    assert_eq!(second.status, Status::Duplicate);
    assert!(b.exists(), "a duplicate must never be deleted");
    assert_eq!(engine.stats.duplicates, 1);
}

#[test]
fn an_unknown_blob_is_quarantined_with_a_registry_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let inbox = tmp.path().join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    let src = write(&inbox, "mystery.dat", &[0u8, 9, 8, 7, 6, 5]);

    let mut engine = engine(tmp.path());
    let outcome = engine.process(&src);

    assert_eq!(outcome.status, Status::Quarantined);
    let registry = engine.quarantined.registry_path();
    assert!(registry.exists());
    let pending = engine.quarantined.pending();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].reason, "low_confidence");
}

#[test]
fn in_flight_downloads_are_left_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let inbox = tmp.path().join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    let src = write(&inbox, "ubuntu.iso.crdownload", b"partial");

    let mut engine = engine(tmp.path());
    assert_eq!(engine.process(&src).status, Status::Skipped);
    assert!(src.exists());
}

#[test]
fn config_round_trips_through_a_file() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("sorta.toml");
    Config::write_default(&path).unwrap();

    let loaded = Config::load(&path).unwrap();
    assert!(loaded.path.is_some());
    assert_eq!(loaded.config.destination_root, "./Sorted");

    // Relative destinations resolve against the config's own directory.
    assert_eq!(loaded.destination_root(), tmp.path().join("./Sorted"));

    // Refuses to clobber.
    assert!(Config::write_default(&path).is_err());
}

#[test]
fn history_survives_a_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("sorta_history.jsonl");
    let inbox = tmp.path().join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    let src = write(&inbox, "invoice.pdf", b"%PDF-1.4 x");

    {
        let mut engine = engine(tmp.path());
        engine.history = History::new(&log);
        engine.process(&src);
    }

    let reopened = History::new(&log);
    assert_eq!(reopened.read_all().len(), 1);
    assert_eq!(reopened.committed().len(), 1);
}
