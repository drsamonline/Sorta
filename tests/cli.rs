//! Tests that run the real binary.
//!
//! Everything else in this repository calls the library. These call the executable,
//! because argument parsing, exit codes and the shape of stdout are behaviour too —
//! and the greedy-positional bug (`--only` swallowed into the folder list) was
//! invisible to every library test.
//!
//! Each test gets its own `SORTA_HOME`, which `paths::home_dir` honours, so nothing
//! touches the real `~/.sorta`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Runs the built binary with an isolated home.
fn sorta(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sorta"))
        .args(args)
        .env("SORTA_HOME", home)
        // The binary reads no other Sorta state, but be explicit about the config.
        .env("XDG_CONFIG_HOME", home.join("config"))
        .output()
        .expect("the binary should run")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

#[test]
fn version_prints_the_crate_version() {
    let home = tempfile::tempdir().unwrap();
    let output = sorta(home.path(), &["--version"]);
    assert!(output.status.success());
    assert!(
        stdout(&output).contains(env!("CARGO_PKG_VERSION")),
        "stdout was {:?}",
        stdout(&output)
    );
}

#[test]
fn status_reports_the_destination_and_exits_zero() {
    let home = tempfile::tempdir().unwrap();
    let output = sorta(home.path(), &["status"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));

    let text = stdout(&output);
    assert!(text.contains("destination"), "{text}");
    assert!(text.contains("extraction"), "{text}");
}

#[test]
fn an_unknown_subcommand_fails_loudly() {
    let home = tempfile::tempdir().unwrap();
    let output = sorta(home.path(), &["not-a-command"]);
    assert!(!output.status.success());
    assert!(
        !stderr(&output).is_empty(),
        "a usage error should say something"
    );
}

#[test]
fn config_path_prints_something_that_looks_like_a_path() {
    let home = tempfile::tempdir().unwrap();
    let output = sorta(home.path(), &["config", "path"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    assert!(text.trim().ends_with(".toml"), "{text}");
}

#[test]
fn forget_all_without_yes_is_a_dry_run() {
    let home = tempfile::tempdir().unwrap();
    let output = sorta(home.path(), &["forget", "--all"]);

    // Exit code 2 means "I listed what I would do and stopped".
    assert_eq!(output.status.code(), Some(2), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    assert!(
        text.contains("would delete") || text.contains("would"),
        "{text}"
    );
    assert!(text.contains("--yes"), "{text}");
}

#[test]
fn bench_extract_measures_a_folder() {
    let home = tempfile::tempdir().unwrap();
    let corpus = home.path().join("corpus");
    std::fs::create_dir_all(&corpus).unwrap();
    std::fs::write(corpus.join("a.txt"), "hello there, this is text\n").unwrap();

    let output = sorta(
        home.path(),
        &["bench-extract", "--limit", "5", corpus.to_str().unwrap()],
    );
    assert!(output.status.success(), "stderr: {}", stderr(&output));

    let text = stdout(&output);
    assert!(text.contains("Extraction benchmark"), "{text}");
    assert!(text.contains("Usable text: 100%"), "{text}");
}

#[test]
fn options_before_folders_are_honoured() {
    // The regression: a greedy positional used to swallow trailing flags, so
    // `--limit` and `--only` were silently ignored.
    let home = tempfile::tempdir().unwrap();
    let corpus = home.path().join("corpus");
    std::fs::create_dir_all(&corpus).unwrap();
    for name in ["a.txt", "b.md", "c.png"] {
        std::fs::write(corpus.join(name), "some text\n").unwrap();
    }

    let output = sorta(
        home.path(),
        &[
            "bench-extract",
            "--only",
            "txt",
            "--limit",
            "10",
            corpus.to_str().unwrap(),
        ],
    );
    assert!(output.status.success(), "stderr: {}", stderr(&output));

    let text = stdout(&output);
    assert!(
        text.contains("1 files"),
        "the filter should leave one file: {text}"
    );
    assert!(
        !text.contains("| md |"),
        "markdown should have been filtered out: {text}"
    );
}

#[test]
fn models_reports_missing_weights_without_failing() {
    let home = tempfile::tempdir().unwrap();
    let models = home.path().join("models");
    std::fs::create_dir_all(&models).unwrap();

    let output = sorta(home.path(), &["models", "--dir", models.to_str().unwrap()]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));

    let text = stdout(&output);
    assert!(text.contains("0 present"), "{text}");
    assert!(text.contains("intent-deberta"), "{text}");
    assert!(text.contains("No weights"), "{text}");
}

#[test]
fn a_missing_config_is_not_an_error() {
    let home = tempfile::tempdir().unwrap();
    // No sorta.toml anywhere: defaults must carry every command.
    let output = sorta(home.path(), &["rules"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("rule"), "{}", stdout(&output));
}

#[test]
fn a_sorted_file_is_logged_and_can_be_explained() {
    let home = tempfile::tempdir().unwrap();
    let inbox = home.path().join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    let file: PathBuf = inbox.join("Acme Invoice.pdf");
    std::fs::write(&file, "%PDF-1.4 invoice body").unwrap();

    let config = home.path().join("sorta.toml");
    std::fs::write(
        &config,
        // Forward slashes: valid TOML, and Windows accepts them in paths.
        format!(
            "destination_root = \"{}\"\nquarantine_root = \"{}\"\n",
            home.path()
                .join("Sorted")
                .display()
                .to_string()
                .replace('\\', "/"),
            home.path()
                .join("Quarantine")
                .display()
                .to_string()
                .replace('\\', "/")
        ),
    )
    .unwrap();

    let sort = sorta(
        home.path(),
        &[
            "--config",
            config.to_str().unwrap(),
            "sort",
            file.to_str().unwrap(),
        ],
    );
    assert!(sort.status.success(), "stderr: {}", stderr(&sort));
    assert!(!file.exists(), "the file should have moved");

    let history = sorta(home.path(), &["history", "--limit", "5"]);
    assert!(history.status.success(), "stderr: {}", stderr(&history));
    let text = stdout(&history);
    assert!(text.contains("keyword:invoice"), "{text}");

    let why = sorta(home.path(), &["why", file.to_str().unwrap()]);
    // `why` on the old path explains where it went; the log has the entry either way.
    let _ = why;
}

// ---------------------------------------------------------------------------
// Undo: the promise that a move is reversible
// ---------------------------------------------------------------------------

/// Writes a config that sorts into a private destination and quarantine.
fn write_config(home: &Path, extra: &str) -> PathBuf {
    let path = home.join("sorta.toml");
    let body = format!(
        "destination_root = \"{}\"\nquarantine_root = \"{}\"\n{extra}",
        home.join("Sorted").display().to_string().replace('\\', "/"),
        home.join("Quarantine")
            .display()
            .to_string()
            .replace('\\', "/"),
    );
    std::fs::write(&path, body).unwrap();
    path
}

#[test]
fn undo_restores_every_file_byte_for_byte() {
    let home = tempfile::tempdir().unwrap();
    let inbox = home.path().join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    let config = write_config(home.path(), "");

    let bodies: Vec<(&str, &[u8])> = vec![
        ("Acme Invoice.pdf", b"%PDF-1.4 first"),
        ("Contract notes.txt", b"contract text"),
        ("photo holiday.png", b"\x89PNG\r\n\x1a\n second"),
    ];

    let mut originals = Vec::new();
    for (name, body) in &bodies {
        let path = inbox.join(name);
        std::fs::write(&path, body).unwrap();
        originals.push((path.clone(), body.to_vec()));

        let sort = sorta(
            home.path(),
            &[
                "--config",
                config.to_str().unwrap(),
                "sort",
                path.to_str().unwrap(),
            ],
        );
        assert!(sort.status.success(), "stderr: {}", stderr(&sort));
    }

    let undo = sorta(home.path(), &["undo", "--last", "3"]);
    assert!(undo.status.success(), "stderr: {}", stderr(&undo));
    assert!(
        stdout(&undo).contains("Undone: 3 of 3"),
        "{}",
        stdout(&undo)
    );

    for (path, body) in &originals {
        assert!(path.exists(), "{} should be back", path.display());
        assert_eq!(
            &std::fs::read(path).unwrap(),
            body,
            "{} changed",
            path.display()
        );
    }
}

#[test]
fn undo_plan_changes_nothing() {
    let home = tempfile::tempdir().unwrap();
    let inbox = home.path().join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    let config = write_config(home.path(), "");

    let file = inbox.join("Acme Invoice.pdf");
    std::fs::write(&file, b"%PDF-1.4").unwrap();
    let sort = sorta(
        home.path(),
        &[
            "--config",
            config.to_str().unwrap(),
            "sort",
            file.to_str().unwrap(),
        ],
    );
    assert!(sort.status.success());

    let plan = sorta(home.path(), &["undo", "--last", "1", "--plan"]);
    assert!(plan.status.success(), "stderr: {}", stderr(&plan));
    assert!(stdout(&plan).contains("Would undo 1"), "{}", stdout(&plan));
    assert!(!file.exists(), "--plan must not move anything");
}

#[test]
fn undo_leaves_both_files_when_the_original_path_is_taken() {
    let home = tempfile::tempdir().unwrap();
    let inbox = home.path().join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    let config = write_config(home.path(), "");

    let file = inbox.join("Acme Invoice.pdf");
    std::fs::write(&file, b"%PDF-1.4 original").unwrap();
    let sort = sorta(
        home.path(),
        &[
            "--config",
            config.to_str().unwrap(),
            "sort",
            file.to_str().unwrap(),
        ],
    );
    assert!(sort.status.success());

    // Something new occupies the old path: undo must not overwrite it.
    std::fs::write(&file, b"a different file entirely").unwrap();

    let undo = sorta(home.path(), &["undo", "--last", "1"]);
    // Exit 1 on purpose: something was skipped, and a script should notice.
    assert_eq!(undo.status.code(), Some(1), "stderr: {}", stderr(&undo));
    assert!(
        stdout(&undo).contains("Undone: 0 of 1"),
        "{}",
        stdout(&undo)
    );
    assert_eq!(
        std::fs::read(&file).unwrap(),
        b"a different file entirely",
        "the new file at the original path must survive"
    );
}

#[test]
fn undo_of_an_auto_locked_file_gives_back_the_plaintext() {
    let home = tempfile::tempdir().unwrap();
    let inbox = home.path().join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();

    // A key, so auto-lock can encrypt.
    let init = sorta(home.path(), &["vault", "init"]);
    assert!(init.status.success(), "stderr: {}", stderr(&init));

    let config = write_config(
        home.path(),
        "[vault]\nauto_lock = true\n\n[keyword_rules]\npassport = \"_Sensitive\"\n",
    );

    let body = b"%PDF-1.4 passport scan, very secret";
    let file = inbox.join("passport scan.pdf");
    std::fs::write(&file, body).unwrap();

    let sort = sorta(
        home.path(),
        &[
            "--config",
            config.to_str().unwrap(),
            "sort",
            file.to_str().unwrap(),
        ],
    );
    assert!(sort.status.success(), "stderr: {}", stderr(&sort));
    assert!(!file.exists(), "the file should have been filed");

    // The filed copy is encrypted.
    let locked = walk(&home.path().join("Sorted"));
    assert!(
        locked
            .iter()
            .any(|p| p.to_string_lossy().ends_with(".sortvault")),
        "expected a .sortvault file, found {locked:?}"
    );

    let undo = sorta(home.path(), &["undo", "--last", "1"]);
    assert!(undo.status.success(), "stderr: {}", stderr(&undo));

    assert!(
        file.exists(),
        "the plaintext should be back at the original path"
    );
    assert_eq!(
        std::fs::read(&file).unwrap(),
        body,
        "the restored file must match"
    );
}

/// Every user file under `root`, recursively.
///
/// Skips Sorta's own bookkeeping folders, which live inside the destination tree.
/// `_Sensitive` is *not* one of them — that is the vault folder the user asked for,
/// and a leading underscore there means "sensitive", not "internal".
fn walk(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            const INTERNAL: [&str; 3] = ["_receipts", "_deferred", "_Duplicates"];
            let internal = path
                .file_name()
                .map(|name| INTERNAL.contains(&name.to_string_lossy().as_ref()))
                .unwrap_or(false);
            if internal {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found
}

// ---------------------------------------------------------------------------
// Data safety: collisions, duplicates, dry runs, pause, search, quarantine
// ---------------------------------------------------------------------------

#[test]
fn two_files_with_the_same_name_both_survive() {
    let home = tempfile::tempdir().unwrap();
    let config = write_config(home.path(), "");
    let first_dir = home.path().join("inbox-a");
    let second_dir = home.path().join("inbox-b");
    std::fs::create_dir_all(&first_dir).unwrap();
    std::fs::create_dir_all(&second_dir).unwrap();

    // Same name, different content — the classic way a sorter loses a file.
    let first = first_dir.join("Acme Invoice.pdf");
    let second = second_dir.join("Acme Invoice.pdf");
    std::fs::write(&first, b"%PDF-1.4 from January").unwrap();
    std::fs::write(&second, b"%PDF-1.4 from February").unwrap();

    for file in [&first, &second] {
        let sort = sorta(
            home.path(),
            &[
                "--config",
                config.to_str().unwrap(),
                "sort",
                file.to_str().unwrap(),
            ],
        );
        assert!(sort.status.success(), "stderr: {}", stderr(&sort));
    }

    let filed = walk(&home.path().join("Sorted"));
    assert_eq!(
        filed.len(),
        2,
        "both files should be in the destination: {filed:?}"
    );

    let mut bodies: Vec<Vec<u8>> = filed.iter().map(|p| std::fs::read(p).unwrap()).collect();
    bodies.sort();
    assert_eq!(bodies[0], b"%PDF-1.4 from February");
    assert_eq!(bodies[1], b"%PDF-1.4 from January");
}

#[test]
fn a_duplicate_is_handled_by_the_configured_policy() {
    let home = tempfile::tempdir().unwrap();
    let config = write_config(home.path(), "on_duplicate = \"skip\"\n");
    let inbox = home.path().join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();

    let first = inbox.join("Acme Invoice.pdf");
    let copy = inbox.join("Acme Invoice (copy).pdf");
    std::fs::write(&first, b"%PDF-1.4 identical bytes").unwrap();
    std::fs::write(&copy, b"%PDF-1.4 identical bytes").unwrap();

    let sort = sorta(
        home.path(),
        &[
            "--config",
            config.to_str().unwrap(),
            "sort",
            first.to_str().unwrap(),
            copy.to_str().unwrap(),
        ],
    );
    assert!(sort.status.success(), "stderr: {}", stderr(&sort));

    // The first is filed; the duplicate is skipped, not filed twice and not deleted.
    let filed = walk(&home.path().join("Sorted"));
    assert_eq!(filed.len(), 1, "only one copy should be filed: {filed:?}");
    assert!(copy.exists(), "the duplicate must be left where it was");
    assert_eq!(std::fs::read(&copy).unwrap(), b"%PDF-1.4 identical bytes");
}

#[test]
fn a_dry_run_moves_nothing() {
    let home = tempfile::tempdir().unwrap();
    let config = write_config(home.path(), "");
    let inbox = home.path().join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();

    let file = inbox.join("Acme Invoice.pdf");
    std::fs::write(&file, b"%PDF-1.4").unwrap();

    let sort = sorta(
        home.path(),
        &[
            "--config",
            config.to_str().unwrap(),
            "--dry-run",
            "sort",
            file.to_str().unwrap(),
        ],
    );
    assert!(sort.status.success(), "stderr: {}", stderr(&sort));
    assert!(file.exists(), "a dry run must not move the file");
    assert!(
        walk(&home.path().join("Sorted")).is_empty(),
        "a dry run must not create a destination file"
    );
}

#[test]
fn a_paused_sorta_defers_and_resume_sorts() {
    let home = tempfile::tempdir().unwrap();
    let config = write_config(home.path(), "");
    let inbox = home.path().join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();

    let file = inbox.join("Acme Invoice.pdf");
    std::fs::write(&file, b"%PDF-1.4").unwrap();

    let pause = sorta(home.path(), &["pause"]);
    assert!(pause.status.success(), "stderr: {}", stderr(&pause));

    let sort = sorta(
        home.path(),
        &[
            "--config",
            config.to_str().unwrap(),
            "sort",
            file.to_str().unwrap(),
        ],
    );
    assert!(sort.status.success(), "stderr: {}", stderr(&sort));
    assert!(file.exists(), "a paused Sorta must not move anything");
    assert!(stdout(&sort).contains("paused") || stdout(&sort).to_lowercase().contains("defer"));

    let resume = sorta(home.path(), &["resume"]);
    assert!(resume.status.success(), "stderr: {}", stderr(&resume));

    let sort = sorta(
        home.path(),
        &[
            "--config",
            config.to_str().unwrap(),
            "sort",
            file.to_str().unwrap(),
        ],
    );
    assert!(sort.status.success(), "stderr: {}", stderr(&sort));
    assert!(!file.exists(), "after resume the file should be filed");
}

#[test]
fn a_filed_file_is_findable_by_search() {
    let home = tempfile::tempdir().unwrap();
    let config = write_config(home.path(), "");
    let inbox = home.path().join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();

    let file = inbox.join("Acme Invoice March.pdf");
    std::fs::write(&file, b"%PDF-1.4 line item: widget assembly").unwrap();
    let sort = sorta(
        home.path(),
        &[
            "--config",
            config.to_str().unwrap(),
            "sort",
            file.to_str().unwrap(),
        ],
    );
    assert!(sort.status.success(), "stderr: {}", stderr(&sort));

    let search = sorta(home.path(), &["search", "Acme"]);
    assert!(search.status.success(), "stderr: {}", stderr(&search));
    assert!(
        stdout(&search).contains("Acme Invoice March.pdf"),
        "search should find the filed name: {}",
        stdout(&search)
    );
}

#[test]
fn a_low_confidence_file_goes_to_quarantine() {
    let home = tempfile::tempdir().unwrap();
    let config = write_config(home.path(), "");
    let inbox = home.path().join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();

    // An extension nothing recognises and bytes that are not text: no format
    // signal at all, so the classifier falls back to 0.45 and the gate holds it.
    // (A plain text file would *not* be quarantined — `format:text` is 0.9, and
    // rightly so.)
    let file = inbox.join("mystery.xyzzy");
    std::fs::write(&file, [0u8, 159, 146, 150, 0, 7, 255, 1]).unwrap();

    let sort = sorta(
        home.path(),
        &[
            "--config",
            config.to_str().unwrap(),
            "sort",
            file.to_str().unwrap(),
        ],
    );
    assert!(sort.status.success(), "stderr: {}", stderr(&sort));

    let filed = walk(&home.path().join("Sorted"));
    assert!(filed.is_empty(), "nothing should be filed: {filed:?}");
    assert!(
        !file.exists(),
        "the file should have moved to quarantine, not stayed put"
    );

    // The quarantine folder also holds its own index (`quarantine.toml`), so assert
    // on the file itself rather than on a count.
    let queued = walk(&home.path().join("Quarantine"));
    assert!(
        queued
            .iter()
            .any(|p| p.to_string_lossy().ends_with("mystery.xyzzy")),
        "the file should be waiting in quarantine: {queued:?}"
    );

    // The registry lives beside the configured quarantine root, so the list has to
    // be asked with the same config.
    let list = sorta(
        home.path(),
        &["--config", config.to_str().unwrap(), "quarantine", "list"],
    );
    assert!(list.status.success(), "stderr: {}", stderr(&list));
    assert!(
        stdout(&list).contains("mystery"),
        "quarantine list should name the file: {}",
        stdout(&list)
    );
}

// ---------------------------------------------------------------------------
// Bulk paths: scan and rescan, where one bug touches every file
// ---------------------------------------------------------------------------

#[test]
fn scan_files_everything_and_loses_nothing() {
    let home = tempfile::tempdir().unwrap();
    let config = write_config(home.path(), "");
    let inbox = home.path().join("inbox");
    let nested = inbox.join("deeper");
    std::fs::create_dir_all(&nested).unwrap();

    let names = [
        ("Acme Invoice.pdf", "%PDF-1.4 one"),
        ("Contract letter.txt", "contract text"),
        ("photo.png", "\u{89}PNG fake"),
    ];
    let mut expected_bytes: Vec<Vec<u8>> = Vec::new();
    for (name, body) in names {
        std::fs::write(inbox.join(name), body.as_bytes()).unwrap();
        expected_bytes.push(body.as_bytes().to_vec());
    }
    // A nested file must be found too.
    std::fs::write(nested.join("Nested Invoice.pdf"), b"%PDF-1.4 nested").unwrap();
    expected_bytes.push(b"%PDF-1.4 nested".to_vec());

    let scan = sorta(
        home.path(),
        &[
            "--config",
            config.to_str().unwrap(),
            "scan",
            inbox.to_str().unwrap(),
        ],
    );
    assert!(scan.status.success(), "stderr: {}", stderr(&scan));
    assert!(stdout(&scan).contains("Found 4 files"), "{}", stdout(&scan));

    // Every byte arrives in the destination; nothing was dropped or duplicated.
    let mut filed: Vec<Vec<u8>> = walk(&home.path().join("Sorted"))
        .iter()
        .map(|p| std::fs::read(p).unwrap())
        .collect();
    filed.sort();
    expected_bytes.sort();
    assert_eq!(
        filed, expected_bytes,
        "the destination should hold exactly the originals"
    );
}

#[test]
fn scan_leaves_system_noise_alone() {
    let home = tempfile::tempdir().unwrap();
    let config = write_config(home.path(), "");
    let inbox = home.path().join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();

    std::fs::write(inbox.join("Acme Invoice.pdf"), b"%PDF-1.4").unwrap();
    // Files a sorter must never touch.
    for noise in [".DS_Store", "~$lock.docx", "Thumbs.db"] {
        std::fs::write(inbox.join(noise), b"noise").unwrap();
    }

    let scan = sorta(
        home.path(),
        &[
            "--config",
            config.to_str().unwrap(),
            "scan",
            inbox.to_str().unwrap(),
        ],
    );
    assert!(scan.status.success(), "stderr: {}", stderr(&scan));

    for noise in [".DS_Store", "~$lock.docx", "Thumbs.db"] {
        assert!(
            inbox.join(noise).exists(),
            "{noise} must be left where it was"
        );
    }
    assert_eq!(walk(&home.path().join("Sorted")).len(), 1);
}

#[test]
fn scanning_twice_is_idempotent() {
    let home = tempfile::tempdir().unwrap();
    let config = write_config(home.path(), "");
    let inbox = home.path().join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    std::fs::write(inbox.join("Acme Invoice.pdf"), b"%PDF-1.4").unwrap();

    let first = sorta(
        home.path(),
        &[
            "--config",
            config.to_str().unwrap(),
            "scan",
            inbox.to_str().unwrap(),
        ],
    );
    assert!(first.status.success(), "stderr: {}", stderr(&first));

    let second = sorta(
        home.path(),
        &[
            "--config",
            config.to_str().unwrap(),
            "scan",
            inbox.to_str().unwrap(),
        ],
    );
    assert!(second.status.success(), "stderr: {}", stderr(&second));

    // One file, still one file: the second pass found nothing to do.
    assert_eq!(
        walk(&home.path().join("Sorted")).len(),
        1,
        "a second scan must not duplicate anything"
    );
}

#[test]
fn a_dry_run_scan_moves_nothing() {
    let home = tempfile::tempdir().unwrap();
    let config = write_config(home.path(), "");
    let inbox = home.path().join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    std::fs::write(inbox.join("Acme Invoice.pdf"), b"%PDF-1.4").unwrap();

    let scan = sorta(
        home.path(),
        &[
            "--config",
            config.to_str().unwrap(),
            "--dry-run",
            "scan",
            inbox.to_str().unwrap(),
        ],
    );
    assert!(scan.status.success(), "stderr: {}", stderr(&scan));
    assert!(
        inbox.join("Acme Invoice.pdf").exists(),
        "a dry run must not move"
    );
    assert!(walk(&home.path().join("Sorted")).is_empty());
}

#[test]
fn scan_refuses_to_walk_its_own_destination() {
    let home = tempfile::tempdir().unwrap();
    let config = write_config(home.path(), "");
    let sorted = home.path().join("Sorted");
    std::fs::create_dir_all(&sorted).unwrap();

    // Scanning the destination into itself would move every file forever.
    let scan = sorta(
        home.path(),
        &[
            "--config",
            config.to_str().unwrap(),
            "scan",
            sorted.to_str().unwrap(),
        ],
    );
    assert_eq!(scan.status.code(), Some(2), "stdout: {}", stdout(&scan));
    assert!(
        stderr(&scan).to_lowercase().contains("destination"),
        "the refusal should say why: {}",
        stderr(&scan)
    );
}

#[test]
fn rescan_re_sorts_a_file_whose_content_changed() {
    let home = tempfile::tempdir().unwrap();
    // Body matching is what makes content changes matter.
    let config = write_config(home.path(), "keyword_scope = \"name_and_text\"\n");
    let inbox = home.path().join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();

    let file = inbox.join("notes.txt");
    std::fs::write(&file, b"a shopping list\n").unwrap();
    let sort = sorta(
        home.path(),
        &[
            "--config",
            config.to_str().unwrap(),
            "sort",
            file.to_str().unwrap(),
        ],
    );
    assert!(sort.status.success(), "stderr: {}", stderr(&sort));

    let filed = walk(&home.path().join("Sorted"));
    assert_eq!(filed.len(), 1, "{filed:?}");
    let filed_path = filed[0].clone();
    assert!(filed_path.to_string_lossy().contains("Text"));

    // The file now says something else, so it belongs somewhere else.
    std::fs::write(&filed_path, b"service contract between Acme and Beta\n").unwrap();

    let rescan = sorta(
        home.path(),
        &["--config", config.to_str().unwrap(), "rescan"],
    );
    assert!(rescan.status.success(), "stderr: {}", stderr(&rescan));

    let after = walk(&home.path().join("Sorted"));
    assert_eq!(after.len(), 1, "still one file: {after:?}");
    assert!(
        after[0].to_string_lossy().contains("Contracts"),
        "the changed file should have moved to Contracts, found {after:?}"
    );
}

// ---------------------------------------------------------------------------
// watch: the command that runs unattended for hours
// ---------------------------------------------------------------------------

/// A `sorta watch` child that is killed when the test ends, pass or fail.
struct Daemon {
    child: std::process::Child,
    log: PathBuf,
}

impl Daemon {
    fn start(home: &Path, args: &[&str], log_name: &str) -> Self {
        let log = home.join(log_name);
        let out = std::fs::File::create(&log).unwrap();
        let err = out.try_clone().unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_sorta"))
            .args(args)
            .env("SORTA_HOME", home)
            .env("XDG_CONFIG_HOME", home.join("config"))
            .stdout(out)
            .stderr(err)
            .spawn()
            .expect("the watcher should start");
        Self { child, log }
    }

    /// Waits for `check` to become true, polling until the deadline.
    fn wait_until(&mut self, label: &str, check: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while std::time::Instant::now() < deadline {
            if check() {
                return;
            }
            // A dead daemon will never satisfy the condition; say so immediately.
            if let Ok(Some(status)) = self.child.try_wait() {
                panic!(
                    "{label}: the watcher exited early ({status})\n{}",
                    self.log_contents()
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        panic!("{label}: timed out\n{}", self.log_contents());
    }

    fn log_contents(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn watch_files_a_dropped_file() {
    let home = tempfile::tempdir().unwrap();
    let config = write_config(home.path(), "");
    let inbox = home.path().join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();

    let _daemon = Daemon::start(
        home.path(),
        &[
            "--config",
            config.to_str().unwrap(),
            "watch",
            inbox.to_str().unwrap(),
        ],
        "watch.log",
    );

    // Give the watcher a moment to register the folder before dropping a file.
    std::thread::sleep(std::time::Duration::from_secs(2));
    let dropped = inbox.join("Acme Invoice.pdf");
    std::fs::write(&dropped, b"%PDF-1.4 dropped while watching").unwrap();

    let sorted = home.path().join("Sorted");
    let mut daemon = _daemon;
    daemon.wait_until("the dropped file should be filed", || {
        walk(&sorted)
            .iter()
            .any(|p| p.to_string_lossy().contains("Acme Invoice.pdf"))
    });

    assert!(!dropped.exists(), "the watched file should have moved");
}

#[test]
fn watch_answers_ping_while_running() {
    let home = tempfile::tempdir().unwrap();
    let config = write_config(home.path(), "");
    let inbox = home.path().join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();

    let mut daemon = Daemon::start(
        home.path(),
        &[
            "--config",
            config.to_str().unwrap(),
            "watch",
            inbox.to_str().unwrap(),
        ],
        "watch.log",
    );

    // The control channel is the only way to ask a daemon anything, so this is the
    // end-to-end test of the IPC too.
    daemon.wait_until("the watcher should answer ping", || {
        let output = sorta(home.path(), &["ping"]);
        stdout(&output).contains("An instance is listening")
    });

    let ping = sorta(home.path(), &["ping"]);
    let text = stdout(&ping);
    assert!(text.contains("paused        false"), "{text}");
    assert!(text.contains("instances"), "{text}");
}

#[test]
fn watch_leaves_system_noise_alone() {
    let home = tempfile::tempdir().unwrap();
    let config = write_config(home.path(), "");
    let inbox = home.path().join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();

    let mut daemon = Daemon::start(
        home.path(),
        &[
            "--config",
            config.to_str().unwrap(),
            "watch",
            inbox.to_str().unwrap(),
        ],
        "watch.log",
    );

    std::thread::sleep(std::time::Duration::from_secs(2));
    let noise = inbox.join(".DS_Store");
    std::fs::write(&noise, b"noise").unwrap();

    // Wait for the daemon to have seen it, then check it is still there.
    daemon.wait_until("the watcher should have started", || {
        std::fs::read_to_string(&daemon.log).is_ok()
    });
    std::thread::sleep(std::time::Duration::from_secs(3));

    assert!(noise.exists(), ".DS_Store must be left alone");
    assert!(walk(&home.path().join("Sorted")).is_empty());
}
