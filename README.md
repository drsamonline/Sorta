# Sorta

![CI](https://github.com/drsamonline/Sorta/actions/workflows/ci.yml/badge.svg)

Ultra-light, offline-first semantic document sorter. One binary, no service, no
telemetry, no cloud. Drop a file on it (or point it at a folder) and it lands in
a folder you can predict, with a log entry that explains why.

- **Language / platforms:** Rust 2021 · Windows 10/11 · Linux x86_64 (no macOS)
- **Targets:** core binary < 20 MB · idle RAM < 40 MB · cold start < 300 ms
- **Measured today:** release binary 1.2 MB (Linux) / 915 KB (Windows), 88 unit +
  9 integration tests green on both, no C/C++ runtime linked
- **Default posture:** everything works with zero network access. Models are
  local, embeddings are not an LLM, and a cloud provider is strictly opt-in.

## Status

This repository is at **Phase 1: the Tier-0 engine**. The parts marked *working*
are implemented and exercised by the test suite; the parts marked *scaffold* have
their module, config keys, feature flags and error codes in place, with the
implementation landing in a later phase. Nothing is faked — a scaffolded path
returns an explicit "not built in this build" error rather than pretending to
work.

| Area | State |
|---|---|
| Config load/resolve, portable mode, named instances | working |
| Detection: magic bytes → extension → 8 KB UTF-8 sniff | working |
| Filename sanitising, reserved names, ignore patterns | working |
| Stability check, collision policy (rename/skip/replace/keep_both) | working |
| BLAKE3 hashing, 4 KB pre-hash + Bloom fast rejection | working |
| Rules: format → keyword → confidence gate → quarantine | working |
| Transactional move with verify + rollback | working |
| History JSONL, undo (last / by meaning), "why here" | working |
| Duplicate policy: skip / `_Duplicates/` / keep newest | working |
| Deferred queue (`_deferred/`) for locked files | working |
| Bulk scan with crash-safe checkpointing, dry-run | working |
| Sandbox mode (`--sandbox`), quarantine + rules commands | working |
| `sorta search` over names + extracted text (pure-Rust index) | working |
| `sorta insights`: health score, rule analytics, savings | working |
| Rule harness (`test-rule`), conflict check (`rules --check`) | working |
| Time-travel view (`history --as-of`) | working |
| Sorta Packs (`.sortapack` export/show/import), CSV/JSON export | working |
| Content-change re-sort (`rescan`), cron scheduling, idle gating | working |
| Retention compression (gzip in place, never overwrites) | working |
| Local embedding engine (`crispembed` or any command), cosine similarity | working |
| Near-duplicate detection (cosine ≥ 0.92 → suggest or quarantine) | working |
| Post-sort hooks (shell-quoted, spawned, never awaited) | working |
| Tamper-evident audit chain (`sorta audit verify`) | working |
| Erasure (`sorta forget <path>`, `--history`, `--all`) | working |
| Encrypted vault (AES-256-GCM; key from env, `key_command`, or file) | working |
| Vault auto-lock for `_Sensitive/` (encrypt before indexing) | working |
| Multi-instance: destination locks, instance registry, 20 files/s limiter | working |
| Coordinator election, heartbeats, `sorta pause` / `sorta resume` | working |
| Bundled ONNX models via `rten` (intent, embeddings, NER, OCR) | scaffold — runtime chosen, weights absent |
| Retention policy (age limits, dry run by default) | working |
| `.sorta.json` sidecars (opt-in), weekly digest | working |
| Windows network-drive detection (`GetDriveTypeW`) | working |
| Receipts (Markdown) | working |
| Single-instance lock, watcher (notify, coalesced) | working |
| Extraction: PDF text, DOCX/XLSX/PPTX text | working |
| Extraction: OCR of images and scanned PDFs | scaffold |
| Bundled models (rten: intent/embeddings/NER/OCR/extract) | scaffold |
| LLM tiers (Ollama/LM Studio/llama.cpp/LocalAI probe) | probe working, generation scaffold |
| egui dropbox + tray, chat console | scaffold |
| sqlite-vec index, FTS5 search, semantic clustering | scaffold |
| Auto-Pilot learning, context signals, packs, WASM plugins | scaffold |
| Backup engine, vault, retention, insights | scaffold |

`docs/ROADMAP.md` has the phase plan and the reasoning behind each deferral.

## Build

```sh
cargo build --release
# size-lean build (what CI ships)
cargo build --release --no-default-features \
  --features format-basic,format-ocr
```

Windows and Linux only. `cargo test` runs the unit and integration suites; the
tests use temporary directories and never touch your real `~/.sorta`.

## Use

```sh
sorta sort ~/Downloads/invoice-acme-nov.pdf          # sort one file
sorta sort ~/Downloads --dry-run                     # plan only, show the moves
sorta scan ~/Documents --dry-run                     # bulk preview, checkpointed
sorta scan ~/Documents                                # commit the bulk scan
sorta history --limit 20                             # what happened
sorta why ~/Sorted/2026/Documents/invoice-acme-nov.pdf   # why is this here
sorta undo --last 5                                  # put them back
sorta undo --meaning "yesterday's PDFs"              # undo by description
sorta status                                         # instance, queue, config path
sorta config path                                    # which config am I using
sorta search acme invoice                            # find it by name or body text
sorta insights                                       # health score, rule analytics, savings
sorta test-rule "Acme Invoice 2026.pdf"              # what would happen to this name
sorta rules --check                                  # conflicts in the rule set
sorta history --as-of 2026-11-01                     # where files were on that date
sorta packs export freelance                         # portable .sortapack of the rules
sorta export --format csv --out history.csv          # take the log elsewhere
sorta rescan                                         # re-sort files whose content changed
sorta schedule "0 18 * * *"                          # when would this next run
sorta retention --apply --compress                    # gzip old files instead of deleting
sorta embed "Acme invoice November"                   # check the local embedding engine
sorta audit verify                                   # has the log been edited
sorta vault init                                     # create the AES-256-GCM key
sorta vault lock ~/Docs/passport.pdf                 # encrypt into _Sensitive/
sorta forget --all                                   # erase every record Sorta keeps
sorta pause                                          # defer everything, keep the queue
sorta resume                                         # carry on
sorta digest --days 7                                # Markdown summary of the week
sorta retention                                      # what the age policy would delete
sorta undo --since 2026-11-18 --rule keyword:invoice # undo by date and rule
sorta quarantine                                     # what is waiting for review
sorta quarantine review /path/to/queued.pdf          # mark it reviewed
sorta rules                                          # rule catalogue + what this build can run
sorta --sandbox sort ~/Downloads                     # preview into ~/.sorta/sandbox/
```

`--read-only` indexes and suggests but never moves a byte. `--dry-run` prints the
plan without touching disk.

## Configuration

Config resolution, in order:

1. `--config <path>` if given
2. `./sorta.toml` next to the binary or in the working directory (portable mode)
3. `~/.config/sorta/<instance>.toml` (`%APPDATA%\sorta\<instance>.toml` on Windows)
4. built-in defaults

The full key list, with defaults and semantics, is in `docs/CONFIG.md`.

## Where things live

| Path | What |
|---|---|
| `~/.sorta/sorta_history.jsonl` | append-only sort log |
| `~/.sorta/.sorta_scan_state.json` | bulk-scan checkpoint |
| `~/.sorta/.lock` | single-instance lock (pid) |
| `~/.sorta/index.db` | search/vector index (phase 3) |
| `<dest>/Quarantine/quarantine.toml` | low-confidence queue |
| `<dest>/_receipts/YYYY-MM-DD-HHMM.md` | per-run receipt |
| `<file>.sorta.json` | sidecar metadata, when `sidecar_enabled` |
| `~/.sorta/search_index.json` | name + text head per sorted file |
| `~/.sorta/digest-YYYY-MM-DD.md` | weekly digest output |

## Design rules that are not negotiable

1. **Never auto-delete.** Duplicates are ranked and suggested, never removed.
2. **Every move is reversible.** Append-only log, undo by meaning, rollback on a
   failed verify.
3. **Offline by default.** No request leaves the machine unless you name a
   provider and set `llm_send_content = true`.
4. **Structural changes need consent.** A new top-level folder is proposed, not
   created.
5. **Fail loudly, degrade quietly.** A permission error is reported with the
   path; a missing model drops a tier and continues.

## Docs

- `docs/ARCHITECTURE.md` — modules, data flow, pipeline state machine
- `docs/CONFIG.md` — every config key and default
- `docs/ERROR_CODES.md` — the `E_*` taxonomy and how each is handled
- `docs/SCHEMAS.md` — history, quarantine, scan-state, receipt formats
- `docs/ROADMAP.md` — phases, deferrals, and what "done" means

## Licence and paperwork

MIT — see `LICENSE`. Third-party licences, including the BSD-3 notice that must
travel with a binary, are in `NOTICE`. What Sorta stores on disk, and how to erase
it, is in `PRIVACY.md`; how to report a vulnerability is in `SECURITY.md`. If you
are an agent working in this repo, read `AGENTS.md` first.
