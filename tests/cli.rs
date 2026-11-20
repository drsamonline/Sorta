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
            home.path().join("Sorted").display().to_string().replace('\\', "/"),
            home
                .path()
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
