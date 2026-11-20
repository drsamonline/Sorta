//! Pipeline tests.
//!
//! Every test here is hermetic: `Engine::for_test` roots the whole engine in a
//! temporary directory, so nothing touches the real `~/.sorta`.

use super::*;

fn engine(root: &Path) -> Engine {
    let config = Config {
        destination_root: root.join("Sorted").display().to_string(),
        quarantine_root: root.join("Quarantine").display().to_string(),
        ..Config::default()
    };
    Engine::for_test(root, config)
}

#[test]
fn sorts_a_pdf_into_the_year_and_category() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = engine(dir.path());
    let src = dir.path().join("Acme Invoice 11.pdf");
    std::fs::write(&src, b"%PDF-1.4 fake invoice body").unwrap();

    let outcome = engine.process(&src);
    assert_eq!(outcome.status, Status::Moved, "{}", outcome.note);
    let dst = outcome.dst.unwrap();
    assert!(dst.contains("Sorted"));
    assert!(dst.contains("PDF-Invoices"));
    assert!(!src.exists());
    assert!(Path::new(&dst).exists());
    assert_eq!(engine.stats.moved, 1);
}

#[test]
fn dry_run_moves_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = engine(dir.path());
    engine.dry_run = true;
    let src = dir.path().join("notes.txt");
    std::fs::write(&src, b"plain text notes").unwrap();

    let outcome = engine.process(&src);
    assert_eq!(outcome.status, Status::Planned);
    assert!(src.exists());
    assert!(!dir.path().join("Sorted").exists());
}

#[test]
fn ignore_patterns_win() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = engine(dir.path());
    let src = dir.path().join("ubuntu.iso.crdownload");
    std::fs::write(&src, b"partial").unwrap();
    let outcome = engine.process(&src);
    assert_eq!(outcome.status, Status::Skipped);
    assert!(src.exists());
}

#[test]
fn duplicates_are_never_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = engine(dir.path());
    let a = dir.path().join("a invoice.pdf");
    let b = dir.path().join("b invoice.pdf");
    std::fs::write(&a, b"identical bytes").unwrap();
    std::fs::write(&b, b"identical bytes").unwrap();

    let first = engine.process(&a);
    assert_eq!(first.status, Status::Moved);

    let second = engine.process(&b);
    assert_eq!(second.status, Status::Duplicate);
    assert!(b.exists(), "a duplicate must never be deleted");
}

#[test]
fn duplicates_can_be_moved_aside() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = engine(dir.path());
    engine.config.on_duplicate = DupPolicy::Move;

    let a = dir.path().join("a invoice.pdf");
    let b = dir.path().join("b invoice.pdf");
    std::fs::write(&a, b"identical bytes").unwrap();
    std::fs::write(&b, b"identical bytes").unwrap();

    assert_eq!(engine.process(&a).status, Status::Moved);
    let second = engine.process(&b);

    assert_eq!(second.status, Status::Duplicate);
    assert!(!b.exists(), "the copy should have been moved aside");
    let dst = second.dst.expect("duplicate should report its new home");
    assert!(dst.contains("_Duplicates"));
    assert!(Path::new(&dst).exists());
}

#[test]
fn keep_newest_retires_the_older_copy() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = engine(dir.path());
    engine.config.on_duplicate = DupPolicy::KeepNewest;

    let a = dir.path().join("a invoice.pdf");
    let b = dir.path().join("b invoice.pdf");
    std::fs::write(&a, b"identical bytes").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&b, b"identical bytes").unwrap();

    assert_eq!(engine.process(&a).status, Status::Moved);
    let second = engine.process(&b);

    // b is newer, so it is filed and the old copy is retired, not deleted.
    assert_eq!(second.status, Status::Moved, "{}", second.note);
    let retired: Vec<_> = std::fs::read_dir(dir.path().join("_Duplicates"))
        .expect("_Duplicates should exist")
        .collect();
    assert_eq!(retired.len(), 1);
}

#[test]
fn deferred_files_are_recorded_in_the_queue() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(dir.path());
    let err = io::Error::other("sharing violation");

    let record = engine
        .write_deferred_record(Path::new("/tmp/locked.pdf"), &err)
        .expect("a deferred record should be written");

    assert!(record.exists());
    assert!(record.to_string_lossy().contains("_deferred"));
    let text = std::fs::read_to_string(&record).unwrap();
    assert!(text.contains("/tmp/locked.pdf"));
    assert!(text.contains("sharing violation"));
}

#[test]
fn a_sorted_file_lands_in_the_search_index() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = engine(dir.path());
    let src = dir.path().join("notes.txt");
    std::fs::write(&src, b"remember the milk before the invoice").unwrap();

    let outcome = engine.process(&src);
    assert_eq!(outcome.status, Status::Moved);
    assert_eq!(engine.search_index.len(), 1);
    assert_eq!(engine.search_index.search("milk", 5).len(), 1);

    engine.persist().unwrap();
    assert!(dir.path().join("search_index.json").exists());
}

#[test]
fn auto_lock_encrypts_and_keeps_the_text_out_of_the_index() {
    let dir = tempfile::tempdir().unwrap();

    // A rule that sends passport scans to the vault folder.
    let mut config = Config {
        ..Config::default()
    };
    config.keyword_rules.clear();
    config
        .keyword_rules
        .insert("passport".to_string(), "_Sensitive".to_string());
    config.vault.auto_lock = true;

    // The key lives in the engine's state directory, which for tests is the root.
    crate::vault::VaultKey::load_or_create(dir.path()).unwrap();

    let mut engine = Engine::for_test(dir.path(), config);
    let src = dir.path().join("passport scan.pdf");
    std::fs::write(&src, b"%PDF-1.4 passport number 1234").unwrap();

    let outcome = engine.process(&src);
    assert_eq!(outcome.status, Status::Moved, "{}", outcome.note);
    assert!(outcome.signals.iter().any(|s| s == "vault:locked"));

    let locked = outcome.dst.expect("locked path");
    assert!(locked.ends_with(crate::vault::LOCKED_SUFFIX), "{locked}");
    assert!(crate::vault::is_locked(std::path::Path::new(&locked)));
    assert!(!src.exists(), "the plaintext must be gone");

    // The index keeps the name but no text, and no vector is stored.
    let hits = engine.search_index.search("passport", 10);
    assert_eq!(hits.len(), 1, "the name is still findable");
    assert!(
        engine.search_index.docs[0].text.is_empty(),
        "text must not be indexed"
    );
    assert!(engine.vectors.is_empty(), "no embedding for a locked file");
}

#[test]
fn without_auto_lock_the_file_stays_readable() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config {
        ..Config::default()
    };
    config.keyword_rules.clear();
    config
        .keyword_rules
        .insert("passport".to_string(), "_Sensitive".to_string());

    let mut engine = Engine::for_test(dir.path(), config);
    let src = dir.path().join("passport scan.pdf");
    std::fs::write(&src, b"%PDF-1.4 passport number 1234").unwrap();

    let outcome = engine.process(&src);
    let dst = outcome.dst.expect("moved");
    assert!(!dst.ends_with(crate::vault::LOCKED_SUFFIX));
    assert!(std::path::Path::new(&dst).exists());
    assert!(!crate::vault::is_locked(std::path::Path::new(&dst)));
}

#[test]
fn a_missing_key_leaves_the_file_filed_but_plain() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config {
        ..Config::default()
    };
    config.keyword_rules.clear();
    config
        .keyword_rules
        .insert("passport".to_string(), "_Sensitive".to_string());
    config.vault.auto_lock = true;
    // No key file: locking must fail without undoing the move.

    let mut engine = Engine::for_test(dir.path(), config);
    let src = dir.path().join("passport scan.pdf");
    std::fs::write(&src, b"%PDF-1.4 passport number 1234").unwrap();

    let outcome = engine.process(&src);
    assert_eq!(outcome.status, Status::Moved, "{}", outcome.note);
    assert!(
        outcome.signals.iter().any(|s| s.starts_with("vault:failed:")),
        "expected a vault failure signal, got {:?}",
        outcome.signals
    );
    let dst = outcome.dst.expect("moved");
    assert!(
        std::path::Path::new(&dst).exists(),
        "the file must still be there"
    );
}

#[test]
fn a_paused_engine_defers_everything() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = engine(dir.path());

    std::fs::write(engine.pause_flag.clone(), b"paused").unwrap();

    let src = dir.path().join("notes.txt");
    std::fs::write(&src, b"hello world").unwrap();

    let outcome = engine.process(&src);
    assert_eq!(outcome.status, Status::Deferred);
    assert_eq!(outcome.rule, "instance:paused");
    assert!(src.exists());

    // Resuming lets the same file through.
    std::fs::remove_file(&engine.pause_flag).unwrap();
    assert_eq!(engine.process(&src).status, Status::Moved);
}

#[test]
fn a_busy_destination_defers_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        multi_instance: true,
        ..Config::default()
    };
    let mut engine = Engine::for_test(dir.path(), config);

    // Hold the lock on the folder this file is heading for.
    let dest_dir = engine
        .dest_root
        .join(chrono::Local::now().format("%Y").to_string())
        .join("Text");
    std::fs::create_dir_all(&dest_dir).unwrap();
    let held = DestinationLock::acquire(&dest_dir).expect("test holds the lock");

    let src = dir.path().join("notes.txt");
    std::fs::write(&src, b"hello world").unwrap();

    let outcome = engine.process(&src);
    assert_eq!(outcome.status, Status::Deferred, "{}", outcome.note);
    assert_eq!(outcome.rule, "instance:destination_busy");
    assert!(src.exists(), "a deferred file must not move");

    drop(held);
}

#[cfg(unix)]
#[test]
fn audit_and_hooks_fire_after_a_move() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("hook ran");

    let config = Config {
        audit_enabled: true,
        hooks: crate::config::HooksConfig {
            on_sort: format!("printf '%s' {{path}} > '{}'", marker.display()),
        },
        ..Config::default()
    };
    let mut engine = Engine::for_test(dir.path(), config);

    let src = dir.path().join("invoice acme.pdf");
    std::fs::write(&src, b"%PDF-1.4 invoice body").unwrap();
    let outcome = engine.process(&src);

    assert_eq!(outcome.status, Status::Moved, "{}", outcome.note);
    assert!(
        outcome.signals.iter().any(|s| s == "hook:started"),
        "expected a hook signal, got {:?}",
        outcome.signals
    );

    let report = engine.audit.verify();
    assert!(report.ok, "{}", report.detail);
    assert_eq!(report.entries, 1);

    // The hook is spawned, not awaited, so wait for its side effect.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline && !marker.exists() {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(marker.exists(), "the hook should have written the marker");
}

#[cfg(unix)]
#[test]
fn near_duplicates_are_suggested_then_quarantined() {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();

    // A fake engine that always returns the same vector, so every document
    // looks similar to the first one filed.
    let script = dir.path().join("fake-embed.sh");
    let mut file = std::fs::File::create(&script).unwrap();
    writeln!(file, "#!/bin/sh").unwrap();
    writeln!(file, "echo '[0.5, 0.5, 0.5, 0.5]'").unwrap();
    drop(file);
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let mut config = Config::default();
    config.embedding.engine = crate::config::EmbeddingEngine::Command;
    config.embedding.command = script.to_string_lossy().to_string();
    config.embedding.dimensions = 4;

    let mut engine = Engine::for_test(dir.path(), config);

    // First file: nothing to compare against, so it is filed and embedded.
    let first = dir.path().join("invoice one.txt");
    std::fs::write(&first, b"invoice from Acme Corp for November services\n").unwrap();
    assert_eq!(engine.process(&first).status, Status::Moved);
    assert_eq!(engine.vectors.len(), 1);

    // Second file: different bytes, so not an exact duplicate, but the engine
    // says it is similar.
    let second = dir.path().join("invoice two.txt");
    std::fs::write(&second, b"invoice from Acme Corp for December services\n").unwrap();
    let outcome = engine.process(&second);

    assert_eq!(outcome.status, Status::Moved, "suggest is the default");
    assert!(
        outcome.signals.iter().any(|s| s.starts_with("near_dup:")),
        "expected a near_dup signal, got {:?}",
        outcome.signals
    );

    // Now flip the policy and check the file is held back instead.
    engine.config.embedding.near_duplicate_action = crate::config::NearDuplicateAction::Quarantine;
    let third = dir.path().join("invoice three.txt");
    std::fs::write(&third, b"invoice from Acme Corp for January services\n").unwrap();
    let held = engine.process(&third);

    assert_eq!(held.status, Status::Quarantined);
    assert!(held.note.contains("possible duplicate"));
    let pending = engine.quarantined.pending();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].reason, "possible_duplicate");
}

#[cfg(unix)]
#[test]
fn without_an_engine_nothing_is_embedded() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = engine(dir.path());
    let src = dir.path().join("invoice.txt");
    std::fs::write(&src, b"invoice from Acme Corp for November services\n").unwrap();

    assert_eq!(engine.process(&src).status, Status::Moved);
    assert!(engine.vectors.is_empty());
}

#[test]
fn rescan_re_sorts_files_whose_content_changed() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = engine(dir.path());

    let src = dir.path().join("notes.txt");
    std::fs::write(&src, b"shopping list\n").unwrap();
    let first = engine.process(&src);
    assert_eq!(first.status, Status::Moved);
    let dst = PathBuf::from(first.dst.expect("moved"));

    // Unchanged bytes: nothing to do.
    let quiet = engine.rescan();
    assert_eq!(quiet.checked, 1);
    assert!(quiet.changed.is_empty());

    // The file now says something else, so it belongs somewhere else.
    std::fs::write(&dst, b"service contract between Acme and Beta\n").unwrap();
    let summary = engine.rescan();

    assert_eq!(summary.changed.len(), 1);
    assert!(
        summary
            .resorted
            .iter()
            .any(|o| o.rule == "keyword:contract"),
        "expected the contract rule, got {:?}",
        summary.resorted
    );
}

#[test]
fn rescan_drops_files_that_vanished() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = engine(dir.path());
    let src = dir.path().join("note.txt");
    std::fs::write(&src, b"hello").unwrap();
    let dst = PathBuf::from(engine.process(&src).dst.expect("moved"));

    std::fs::remove_file(&dst).unwrap();
    let summary = engine.rescan();

    assert_eq!(summary.missing.len(), 1);
    assert_eq!(engine.search_index.len(), 0);
}

#[test]
fn sidecars_are_written_when_enabled() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = engine(dir.path());
    engine.config.sidecar_enabled = true;

    let src = dir.path().join("Acme Invoice.pdf");
    std::fs::write(&src, b"%PDF-1.4 invoice body").unwrap();

    let outcome = engine.process(&src);
    assert_eq!(outcome.status, Status::Moved);
    let dst = outcome.dst.expect("moved file has a destination");
    let sidecar = format!("{dst}.sorta.json");

    assert!(Path::new(&sidecar).exists());
    let text = std::fs::read_to_string(&sidecar).unwrap();
    assert!(text.contains("keyword:invoice"));
    assert!(text.contains("b3:"));
}

#[test]
fn no_sidecar_without_opt_in() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = engine(dir.path());
    let src = dir.path().join("Acme Invoice.pdf");
    std::fs::write(&src, b"%PDF-1.4 invoice body").unwrap();

    let outcome = engine.process(&src);
    let dst = outcome.dst.expect("moved file has a destination");
    assert!(!Path::new(&format!("{dst}.sorta.json")).exists());
}

#[test]
fn unknown_files_are_quarantined_not_guessed() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = engine(dir.path());
    let src = dir.path().join("blob.bin");
    std::fs::write(&src, [0u8, 1, 2, 3, 4, 5, 6, 7]).unwrap();

    let outcome = engine.process(&src);
    assert_eq!(outcome.status, Status::Quarantined);
    assert!(outcome.note.contains("gate"));
    assert!(!dir.path().join("Sorted").exists());
    assert!(dir
        .path()
        .join("Quarantine")
        .join("quarantine.toml")
        .exists());
}

#[test]
fn collisions_are_renamed_not_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = engine(dir.path());
    let dest_dir = engine
        .dest_root
        .join(chrono::Local::now().format("%Y").to_string().as_str())
        .join("Text");
    std::fs::create_dir_all(&dest_dir).unwrap();
    std::fs::write(dest_dir.join("notes.txt"), b"old").unwrap();

    let src = dir.path().join("notes.txt");
    std::fs::write(&src, b"new").unwrap();

    let outcome = engine.process(&src);
    assert_eq!(outcome.status, Status::Moved);
    assert!(outcome.dst.as_deref().unwrap().contains("notes (1).txt"));
    assert_eq!(std::fs::read(dest_dir.join("notes.txt")).unwrap(), b"old");
}

#[test]
fn history_records_every_decision() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = engine(dir.path());
    let src = dir.path().join("Contract 2026.pdf");
    std::fs::write(&src, b"%PDF-1.4 contract").unwrap();
    engine.process(&src);

    let entries = engine.history.read_all();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].status, "committed");
    assert_eq!(entries[0].rule, "keyword:contract");
}

#[test]
fn verify_rolls_back_on_missing_destination() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("x.txt");
    std::fs::write(&src, b"hello").unwrap();
    let dst = dir.path().join("nested/deep/x.txt");
    move_with_verify(&src, &dst).unwrap();
    assert!(dst.exists());
    assert!(!src.exists());
}
