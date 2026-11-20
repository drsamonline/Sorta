# Notes for agents working in this repo

One page, so a future agent does not re-derive everything from the commit log.

## What this is

`sorta` — an offline file sorter in Rust (Windows 10/11 + Linux, no macOS). One
binary, tray-first by design but CLI-only today. `docs/ROADMAP.md` is the source of
truth for what is real and what is scaffolded; read it before believing a module
name.

## The verification loop (read this first)

The agent sandbox has **no Rust toolchain** and cannot install one. CI is the
compiler.

1. Make the change, push to `main`.
2. `gh run list --workflow=ci.yml --limit 1`, then `gh run view <id> --log-failed`
   and read the actual error.
3. Fix, push, repeat. Expect 1–3 cycles; do not claim a change works before a run
   is green.

Two gates are blocking and will fail a run for style: `cargo fmt --check` and
`cargo clippy --all-targets -- -D warnings`. To avoid a wasted cycle, push a
throwaway `.github/workflows/fmtfix.yml` (push-triggered, runs `cargo fmt`, uploads
`src/` + `tests/`), download the artifact, copy it back, delete the workflow. That
is how formatting has been applied throughout this repo.

Tests must be hermetic: use `Engine::for_test(root, config)` and `tempfile`, never
the real `~/.sorta`. Anything touching the network, a model, or a display is out.

## Commands

```
cargo build --release                       # default features
cargo build --release --no-default-features --features format-basic,format-ocr
cargo test --all-targets                    # unit + integration
cargo clippy --all-targets -- -D warnings
cargo fmt --all
cargo build --features index-turso          # experimental SQL index
```

## House rules

1. **Never delete a user file.** Duplicates, retention and quarantine are moves.
   `replace` is the only destructive path and it is opt-in per config.
2. **Verify after every move.** Size at the destination must match the source, or the
   file goes back. `move_with_verify` exists for this; do not bypass it.
3. **The log is append-only.** Undo appends a compensating entry.
4. **Offline by default.** No request leaves the machine unless the user names a
   provider and sets `llm_send_content = true`. Nothing in the current build sends
   anything.
5. **Scaffolds must say they are scaffolds.** A path that is not built returns an
   explicit error or `None`; it never pretends to work.
6. **One doc per behaviour.** A new config key gets a row in `docs/CONFIG.md`, a new
   file format gets a section in `docs/SCHEMAS.md`, and a new capability gets a line
   in `README.md` and `docs/ROADMAP.md` in the same commit.

## Adding a feature

`config.rs` (key + default + `normalise`) → the module that owns the behaviour →
`pipeline.rs` if it belongs in the sort → `cli.rs` (command + handler) → tests →
the three docs. If it needs a dependency, check its licence and add a row to
`NOTICE`.

## Traps that have actually bitten

* **argh:** only the *last* positional may be `Option`, `Vec` or defaulted; every
  struct and field needs a doc comment; subcommands need
  `#[argh(subcommand, name = "…")]`.
* **clippy `-D warnings`:** `io::Error::other`, `div_ceil`, `field_reassign_with_default`,
  `derivable_impls`, `print_literal`, `let_and_return`, `too_many_arguments`.
  Struct-update syntax (`..Config::default()`) is the usual fix.
* **`#[serde(default)]` on a config struct** means a new field must also get a
  default in `impl Default`, or every existing `sorta.toml` breaks.
* **Tests that shadow a function name** with its own result (`let plan = plan(…)`
  twice) do not compile.
* **Timestamps in tests:** a cron expectation is relative to the clock; check what
  time the test assumes before asserting a weekday.

## Layout

`config` · `detect` · `safety` · `hash` · `sort` · `pipeline` · `history` ·
`quarantine` · `dedupe` · `scan` · `watcher` · `receipt` · `rename` · `retention` ·
`search` · `vectors` · `embedding` · `hooks` · `audit` · `instance` · `rate` ·
`schedule` · `digest` · `insight` · `interop` · `packs` · `llm` · `extract/` ·
`platform/` · `ui/` (scaffold) · `cli`.
