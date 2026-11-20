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
  global 20 files/s limiter, coordinator election, pause/resume flag
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

## Data safety, tested (20 Nov 2026)

Six more tests through the real binary, each one a way a sorter loses a file:

| Test | What it proves |
|---|---|
| `two_files_with_the_same_name_both_survive` | same name, different bytes: both land, each with its own content |
| `a_duplicate_is_handled_by_the_configured_policy` | `on_duplicate = "skip"` files one copy and leaves the other untouched |
| `a_dry_run_moves_nothing` | `--dry-run` writes nothing anywhere |
| `a_paused_sorta_defers_and_resume_sorts` | `pause` defers, `resume` files |
| `a_filed_file_is_findable_by_search` | the index finds a filed file by name |
| `a_low_confidence_file_goes_to_quarantine` | an unknown format goes to quarantine, not to a guessed folder |

Two of these failures were **my wrong premises, not code bugs**, and worth recording:
a plain text file is *not* low-confidence (`format:text` scores 0.9, so filing it under
`Text/` is correct), and `_Sensitive` is a user-facing folder, not bookkeeping.

One real defect came out of it: `quarantine list` said "Quarantine is empty" when the
sort had used a `--config` pointing elsewhere — the list now says to pass the same
`--config`.

## Undo, tested rather than assumed (20 Nov 2026)

"Sorta never deletes a user file" only holds if a move is reversible. Four tests run
the real binary and check it:

| Test | What it proves |
|---|---|
| `undo_restores_every_file_byte_for_byte` | three files of different kinds come back identical |
| `undo_plan_changes_nothing` | `--plan` moves nothing |
| `undo_leaves_both_files_when_the_original_path_is_taken` | a new file at the old path is never overwritten; exits 1 |
| `undo_of_an_auto_locked_file_gives_back_the_plaintext` | the vault round trip restores the original bytes |

The fourth test found a real defect: `undo` moved the ciphertext back under the
original name, leaving an unopenable `.sortvault` in the inbox. `cmd_undo` now
decrypts any locked destination first, and refuses with a reason when there is no
key — an unopenable file at the original path would be worse than a refusal.

## Measured: performance budget (20 Nov 2026)

The blueprint promises a core binary under 20 MB, idle RAM under 40 MB and a cold
start under 300 ms. Measured on a GitHub runner, Linux x86_64, release build:

| Promise | Target | Measured | Headroom |
|---|---|---|---|
| binary size | < 20 MB | **2.80 MB** | 7× |
| cold start (`--version`, `status`) | < 300 ms | **2 ms** average of 20 runs | 150× |
| peak RSS (`status`) | — | 4.3 MB | — |
| idle RAM (`watch`, 5 s after start) | < 40 MB | **4 MB** | 10× |

Two caveats worth stating rather than hiding: the runner has a warm filesystem cache,
so a first run on a cold laptop will be slower than 2 ms (the headroom absorbs it);
and `watch` was measured idle, not with a large backlog queued.

## Measured: classification (20 Nov 2026)

`sorta bench-classify` over 123 real files from the fleet's own repos:

| Metric | Result |
|---|---|
| would be filed | 122 (99%) |
| would be quarantined | 1 (`.nojekyll`, an empty file — correctly not guessed) |
| confidence | 108 at 0.90+, 14 at 0.70–0.79, 1 below |
| rules that fired | `format:text` 52, `format:code` 34, `format:document` 17, `keyword:contract` 14, `format:image` 3, `format:archive` 2, `fallback:unknown` 1 |

**The 14 `keyword:contract` hits were a defect, not a success.** They were licences
and contributing guides whose *body* mentions the word "contract", filed into
`Contracts`. Body-text keyword matching is now opt-in (`keyword_scope`, default
`name`), so those files go to `Documents` where they belong. That is what measuring
is for: 99% filed sounds healthy until you look at *which* rule did the filing.

## Measured: extraction quality (20 Nov 2026)

`sorta bench-extract` over 120 real files from the fleet's own repos (16 PDFs, 6
DOCX, plus text and code):

| Metric | Result |
|---|---|
| files | 120 (17.6 MB in, 3.04 M characters out) |
| usable text | 95% |
| PDF | 16 / 16 yielded text, 1.26 M characters, density 0.02–0.33 |
| DOCX | 6 / 6 yielded text, 126 k characters |
| no text | 6 — 3 images, 2 `.npz`, 1 empty `.nojekyll` (no OCR, so this is expected) |
| garbled | 0 |
| errors | 0 |

That is yield and failure modes, not fidelity: no ground-truth transcription exists
for these files, so "the right characters came out" is still unproven. What is proven
is that the extractor returns real text from every PDF and DOCX in the corpus, and
that nothing panicked or produced mojibake.

## Audit status (20 Nov 2026)

A release-scale review found the code ready and the paperwork missing. Closed:
`NOTICE` (per-crate licences, BSD-3 text for `argh`), `SECURITY.md`, `PRIVACY.md`
(written from the code, not from a template), `AGENTS.md`, and the last panic path in
library code. Since then: `cli.rs` and `pipeline.rs` were split (1 654 → 291+282+414+712, and
1 407 → 882+446+95), `v0.1.0-rc.1` was tagged and `release.yml` ran green on both
targets, and `sorta forget` now makes erasure a command rather than a table in a
document. The vault landed as `sorta vault` (AES-256-GCM via RustCrypto, key file `0600` or
`SORTA_VAULT_KEY`) with two deviations from the spec, both documented in
`PRIVACY.md`: `aes-gcm` instead of `ring` (no C/asm, keeps the pure-Rust build), and
a key file instead of the OS keychain (`keyring` needs a Secret Service on Linux,
which cannot be verified here). The keychain is still reachable through
`[vault] key_command`, which covers `secret-tool`, `pass`, `op` and `security`
without a dependency and is covered by tests. Auto-lock is wired: a rule that routes a file into `_Sensitive/` gets it encrypted
before it is indexed, so its text never reaches the index. The control channel is a line-delimited JSON protocol over **loopback TCP**, with the
port published in `~/.sorta/port` — one transport for both platforms, borrowed from
CLDR, which retired the Windows named pipe from the plan entirely. The vault gained
an opt-in OS keychain (`--features vault-keyring`, `keyring` 3 with the feature names
DayDoc builds with), falling back to `key_command` and then the key file.
The window is a first cut: `sorta tray` with `--features gui` opens a panel that
reads the history tail and quarantine queue, writes the pause flag, and sorts dropped
files on a worker thread. It compiles on Linux and Windows in a non-gating CI job and
has **never been looked at**; the tray icon, global hotkey and time-travel slider are
not built. Still open: and the tray/dropbox plus the bundled
models, which remain scaffolds.

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
