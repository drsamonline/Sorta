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
- rule harness (`test-rule`), conflict detection (`rules --check`), time-travel
  view (`history --as-of`), Sorta Packs, CSV/JSON export
- content-change re-sort (`rescan`), cron scheduling (`schedule`, `scan --if-due`),
  idle gating (`run_when_idle`), retention compression
- local embedding engine + near-duplicate detection (`vectors.json`, cosine 0.92)
- post-sort hooks, tamper-evident audit chain (`sorta audit verify`)
- multi-instance groundwork: destination locks, instance registry with heartbeats,
  global 20 files/s limiter
- watcher with per-path coalescing, inotify limit warning
- single-instance pid lock with stale reclaim
- CLI: `sort`, `scan`, `undo`, `history`, `why`, `status`, `config`, `watch`
- plain-text extraction; every other backend reports itself as absent

Known gaps in phase 1, stated plainly:

| Gap | Consequence |
|---|---|
| PDF/DOCX/XLSX/PPTX text extraction | working (pdf-extract + a ZIP/XML reader); OCR and media are still absent |
| No embedding index | no semantic clustering, no near-duplicate detection, no virtual folders |
| No bundled models | Tier 1–3 exist as policy and probe; generation returns nothing |
| No GUI | `sorta ui` paths return an explicit "not built" error |
| Windows network-drive detection returns false | a network path runs at normal cadence (slower, never wrong) |
| Windows priority class is a no-op | no `SetPriorityClass` call yet |
| `notify-debouncer-full` not used | the built-in coalescer covers the same contract with no extra dependency |

## Phase 2 — Real extraction

**PDF and Office text landed** behind `format-basic`: `pdf-extract` for PDF text
layers, and a ZIP + XML reader for DOCX/XLSX/PPTX (no LibreOffice, no C runtime).
Still to do here:
Structured-field extraction for invoices and receipts (vendor, date, amount) with
the DGPL Sanga path. BLAKE3-keyed extraction cache (LRU, 50 MB). Archive mode
(`as_file | extract | ask`).

## Phase 3 — Semantic layer and UI

**Database decision.** The blueprint called for `rusqlite` + FTS5 + `sqlite-vec`.
Both pull in C (bundled SQLite, and `sqlite-vec` is literally FFI bindings to a C
extension), which contradicts the no-C-runtime rule. Evaluated alternatives:

| Candidate | Verdict |
|---|---|
| `rusqlite` (bundled) + `sqlite-vec` | rejected: C toolchain, C extension |
| `tursodatabase/turso` (MIT, Rust SQLite rewrite) | **spiked and working**: 0.9.0-pre.5 compiles on stable and passes its tests on Linux and Windows behind `index-turso`; 0.8.4-pre.3 does *not* compile on stable (its own fts module) |
| `dicklesworthstone/frankensqlite` | rejected: nightly-only toolchain, non-MIT licence, no FTS5 or vector story |

**Runtime and embedding decisions.** `robertknight/rten` is confirmed as the
in-process runtime (pure Rust, ONNX, f32 + int8 weights, CPU/SIMD, stable Rust) —
it needs exported `.onnx` weights, which this repo does not have, so the model tier
stays scaffolded. `CrispStrobe/CrispEmbed` is the opposite trade: one C++/ggml
binary that already ships embeddings, OCR, NER and rerankers, but it is C++, it
auto-downloads models, and it is not a Rust crate. It is therefore supported as an
*external local engine* (`[embedding] command`, spawned like the Tier-1 LLM probe),
never as a linked dependency.

So the SQL index is `turso` when `index-turso` is enabled, with the JSON index as
the default until turso publishes a stable release. Next steps there: evaluate the
`fts` feature (tantivy-backed, upstream calls it experimental), then exact vector
search over the 384-dim embeddings once a model exists.

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
