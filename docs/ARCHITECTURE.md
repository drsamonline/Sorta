# Architecture

## Shape

One binary. No daemon, no service, no database server. Three entry surfaces share
one engine:

```
        ┌──────────────┐   ┌──────────────┐   ┌──────────────┐
        │  CLI (argh)  │   │  tray/drop   │   │ chat console │
        └──────┬───────┘   └──────┬───────┘   └──────┬───────┘
               └──────────────────┼──────────────────┘
                        ┌─────────▼─────────┐
                        │  pipeline::Engine │
                        └─────────┬─────────┘
     ┌──────────┬───────────┬─────┴─────┬──────────┬───────────┐
 detect.rs  safety.rs   sort.rs     hash.rs   dedupe.rs   history.rs
     │          │           │           │         │            │
  magic/     sanitise     rules +    BLAKE3    registry     JSONL
  ext/sniff  stability    confidence  pre-hash  + bloom      + undo
```

Startup order (spec requirement, phase 3 for the UI parts): tray appears first,
watcher spawns in the background, dropbox lazy-loads on first interaction. Nothing
heavy is allowed on the startup path.

## Modules

| Module | Responsibility |
|---|---|
| `main.rs` | platform gate, priority hint, exit codes |
| `cli.rs` | argument parsing, dispatch, human output |
| `config.rs` | resolution order, full key set, defaults, path resolution |
| `paths.rs` | every directory the app touches |
| `detect.rs` | magic bytes → extension → 8 KB UTF-8 sniff |
| `safety.rs` | sanitising, ignore globs, stability, collision policy, backup copy |
| `hash.rs` | streaming BLAKE3, 4 KB pre-hash, Bloom filter, fast rejector |
| `sort.rs` | rule catalogue, classifier, destination templates, confidence |
| `rename.rs` | `{client}_{doctype}_{date}` proposals |
| `dedupe.rs` | exact-duplicate registry, duplicate policy |
| `quarantine.rs` | low-confidence queue (`quarantine.toml`) |
| `pipeline.rs` | the transactional move, verify, rollback, logging |
| `history.rs` | append-only log, undo, "why here", meaning parser |
| `receipt.rs` | Markdown receipt per run |
| `scan.rs` | tree walk, checkpointing, dry-run |
| `watcher.rs` | notify + per-path coalescing, inotify limit check |
| `instance.rs` | single-instance lock |
| `extract/` | text extraction, feature-gated backends |
| `llm/` | tier ladder, local endpoint probe, graceful degradation |
| `platform/` | long paths, network mounts, pid liveness |
| `ui/` | tray/dropbox contract (phase 3) |
| `errors.rs` | the `E_*` taxonomy |

## Pipeline state machine

```
[Detected] → [Hashed] → [Extracted] → [Classified] → [DryChecked]
   → [Committed] → [Verified] → [Logged] → [Receipted]

   ↓ skip: system noise / ignore pattern / empty / directory
   ↓ duplicate: byte-identical to a known file (never deleted)
   ↓ quarantined: confidence below min_confidence
   ↓ deferred: locked after 5 backoff attempts
   ↓ failed: nothing changed
```

Terminal states are logged with a reason. A move that fails verification is rolled
back to the source before the outcome is recorded, so the log never claims a move
that did not happen.

## Invariants

1. **Verify after every move.** Size at the destination must match size at the
   source, or the file goes back.
2. **No deletes.** `_Duplicates/` and quarantine are moves, not removals.
3. **Append-only log.** Undo appends a compensating entry; nothing is rewritten.
4. **Confidence gate before action.** Below `min_confidence` (default 0.7) the
   file waits for a human.
5. **No network by default.** Tier 0 is complete; everything above it is an extra.

## Size discipline

`argh` over `clap`, `rten` over `ort` (no ~14.5 MB C++ runtime), pure-Rust crates
only in the default build, `--no-default-features` everywhere, `opt-level = "z"`,
fat LTO, `panic = "abort"`, strip. Models are zstd-compressed at build time and
decompressed in memory. Optional subsystems (`plugins` ≈ 12 MB, `preview` ≈ 9.7 MB,
`tts` ≈ 50 MB of voice data) are off by default so they never count against the
core budget.
