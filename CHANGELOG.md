# Changelog

All notable changes to Sorta. Dates are ISO. Versions follow semver, with the
pre-1.0 convention that a minor bump may change behaviour.

## 0.2.1 — 2026-11-21

Two defects in the 0.2.0 artifacts, both found by downloading and running the release
rather than trusting the pipeline:

- `sorta --version` reported **0.1.0**. The manifest bump never applied — a silent
  string replacement — so the tag and the code disagreed. The release workflow now
  refuses to publish a tag that does not match `Cargo.toml`.
- The Linux binary required **glibc 2.39** (built on Ubuntu 24.04), which Debian 12 and
  Ubuntu 22.04 do not have. Linux releases build on 22.04 now, for a 2.35 floor.

## 0.2.0 — 2026-11-21

Everything since `v0.1.0-rc.1`. Two behaviour changes worth reading before you upgrade
are marked **changed**.

### Added

- **Vault** — `sorta vault init|lock|unlock|status`, AES-256-GCM, key from
  `SORTA_VAULT_KEY`, the OS keychain (`--features vault-keyring`) or `~/.sorta/vault.key`.
- **Auto-lock** — `[vault] auto_lock = true` encrypts a file the rules send into
  `_Sensitive/`, before it is indexed, so its text never reaches the search index.
- **Key command** — `[vault] key_command` runs any command that prints the key as 64
  hex characters (`secret-tool`, `pass`, `op`, `security`).
- **Control channel** — `sorta ping` asks a running `watch` for its state over loopback
  TCP; `~/.sorta/port` publishes the port and pid.
- **Model registry** — `sorta models [--verify]` lists the six Tier-0 models with
  purpose, shape, licence, source and BLAKE3 digest state. No weights are shipped;
  `models/README.md` is the conversion recipe.
- **Window** — `sorta tray` with `--features gui`: pause/resume, quarantine count,
  recent sorts, and a drop target. **Compiles on both platforms and has never been run
  on a screen.**
- **Benchmarks** — `sorta bench-extract` measures text yield per file; `sorta
  bench-classify` measures the classifier's gate and rule histogram.
- **Diagnostics** — `sorta why` answers for either end of a move; `sorta quarantine`
  and `sorta audit verify` explain what happened.

### Changed

- **`retention --apply` no longer deletes.** Expired files move into
  `<destination>/_Expired/`, keeping their place in the tree. Permanent removal needs
  `--apply --delete`. This makes "Sorta never deletes a user file" true.
- **Body-text keyword matching is opt-in** (`keyword_scope`, default `name`). Measured
  over 123 real files, matching inside document bodies filed 14 licences into
  `Contracts`. `name_and_text` restores the old behaviour at a lower confidence.
- A missing destination folder is created on first scan instead of being reported as
  unwritable.
- `scan`'s refusals all exit 2, with the reason on stderr.
- A Windows path in `sorta.toml` that fails to parse now gets a hint about escaping.
- `sorta why` and history lookups match verbatim (`\\?\`) Windows paths.

### Fixed

- `undo` of an auto-locked file gave back an unopenable `.sortvault`; it now decrypts
  first, and refuses with a reason when there is no key.
- `sorta quarantine list` said "Quarantine is empty" when a `--config` was in play.
- Duplicate registry and audit entries recorded the plaintext path after auto-lock.
- `is_vault_path` never matched, so auto-lock silently did nothing.
- A key command that fails is an error, not a silent fall back to the key file.

### Measured

| Claim | Result |
|---|---|
| binary < 20 MB | 2.80 MB |
| cold start < 300 ms | 2 ms |
| idle RAM < 40 MB | 4 MB |
| extraction quality | 95% usable text; 16/16 PDFs, 6/6 DOCX, 0 garbled |
| classification | 99% filed, one rule defect found and fixed |

### Tests

239 unit, 38 through the real binary, 9 integration — on Linux and Windows.

## 0.1.0-rc.1 — 2026-11-20

First tagged build: the engine, safety rules, dedupe, history and undo, quarantine,
receipts, scan, watch, search, retention, embeddings, hooks, the audit chain,
schedules, packs, insights, digest, interop, forget, multi-instance coordination and
the CLI.
