# Roadmap

The spec describes a finished product; this repository is being built in phases so
that every phase ends with something that runs, and nothing ships as a stub that
pretends to work.

## Phase 1 — Tier-0 engine (this repository)

Done and tested:

- config resolution, portable mode, named instances, clamping of bad values
- detection: magic bytes → extension → 8 KB UTF-8 sniff, OS noise skipped
- filename sanitising (illegal chars, reserved device names, trailing dots), ignore
  globs, case-insensitive collision handling with all four policies
- stability check (size + mtime, two consecutive stable samples)
- streaming BLAKE3, 4 KB pre-hash, Bloom filter fast rejection
- rules: format → keyword (name, then text) → confidence gate → quarantine
- transactional move with size verification and rollback, cross-device fallback,
  lock retry with 1/2/4/8/16 s backoff
- history JSONL, undo (last N and by meaning), "why here"
- bulk scan with atomic checkpoint every 100 files, `--resume`
- receipts
- watcher with per-path coalescing, inotify limit warning
- single-instance pid lock with stale reclaim
- CLI: `sort`, `scan`, `undo`, `history`, `why`, `status`, `config`, `watch`
- plain-text extraction; every other backend reports itself as absent

Known gaps in phase 1, stated plainly:

| Gap | Consequence |
|---|---|
| No `xberg` backend yet | PDFs/DOCX/XLSX are classified by name and format only, not by content |
| No embedding index | no semantic clustering, no near-duplicate detection, no virtual folders |
| No bundled models | Tier 1–3 exist as policy and probe; generation returns nothing |
| No GUI | `sorta ui` paths return an explicit "not built" error |
| Windows network-drive detection returns false | a network path runs at normal cadence (slower, never wrong) |
| Windows priority class is a no-op | no `SetPriorityClass` call yet |
| `notify-debouncer-full` not used | the built-in coalescer covers the same contract with no extra dependency |

## Phase 2 — Real extraction

`xberg` behind `format-basic`, so PDF/DOCX/XLSX/PPTX text reaches the classifier.
Structured-field extraction for invoices and receipts (vendor, date, amount) with
the DGPL Sanga path. BLAKE3-keyed extraction cache (LRU, 50 MB). Archive mode
(`as_file | extract | ask`).

## Phase 3 — Semantic layer and UI

`sqlite-vec` + FTS5 index (`~/.sqlite/index.db`), embedding cache, near-duplicate
quarantine at cosine > 0.92, virtual folders, project auto-clustering proposals.
`rten` runtime with the bundled model stack (intent DeBERTa/TinyBERT, MiniLM
embeddings, tiny NER, RapidOCR). Then the tray and the 120×120 dropbox, the chat
console with tool-call cards and the approve/cancel preview, and the command
palette.

## Phase 4 — Automation and trust

Rule test harness, conflict detection, Auto-Pilot correction learning with a
60-day half-life, NL rules on Tier 2/3, time-travel browser, `_Sensitive/` vault
(AES-256-GCM via `ring`, key in the OS keychain), audit hash-chain, retention
policies, webhook/script hooks, clipboard watcher, WASM plugins (`wasmtime`,
feature-gated).

## Phase 5 — Interop, insights, packaging

Sidecar `.sorta.json`, xattr/ADS metadata, Sorta Packs, CSV/JSON export, backup
engine with retention and restore, folder health score, weekly digest, savings
tracker, rule analytics. `cargo-wix` MSI, AppImage, `.deb`, `self_update` from
GitHub Releases, nightly `build-std` size pass.

## Phase 6 — Multi-instance and network shares

Named mutex / `flock`, coordinator election, shared SQLite WAL with a 5 s busy
timeout, destination locks, global 20 files/sec rate limiter, IPC (named pipe /
Unix socket), cross-machine advisory locking, `--force-mixed-versions`.

## Size budget checkpoints

Every phase must re-measure against the target: core binary < 20 MB, idle RAM
< 40 MB, cold start < 300 ms. Phase 1 has no model bytes and no GUI, so it is well
inside; the risk is phase 3, where the bundled models add ~60 MB if they are
embedded uncompressed. That is why models are zstd-compressed at build time and
why `--no-default-features` is the release default in CI.

## What "done" means per phase

A phase is done when: the feature works end to end from the CLI, the tests cover
the failure path as well as the happy path, the docs state the behaviour and its
limits, and the size budget still holds. Anything else is a prototype, and is
labelled as one.
