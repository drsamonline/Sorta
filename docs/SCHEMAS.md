# On-disk formats

## `~/.sorta/sorta_history.jsonl`

One JSON object per line, append-only. Undo appends a compensating entry rather
than editing history.

```json
{
  "ts": "2026-11-19T14:32:01+00:00",
  "src": "/home/u/Downloads/invoice.pdf",
  "dst": "/home/u/Sorted/2026/PDF-Invoices/invoice.pdf",
  "rule": "keyword:invoice",
  "confidence": 0.82,
  "signals": ["keyword:invoice", "name:invoice.pdf"],
  "instance": "default",
  "hash": "b3:abc123…",
  "status": "committed"
}
```

`status` is one of `committed`, `planned`, `skipped`, `duplicate`, `quarantined`,
`deferred`, `failed`, `undone`. A malformed line is skipped on read: one bad line
must not stop the sorter.

## `<quarantine_root>/quarantine.toml`

```toml
[[items]]
path = "/home/u/Downloads/IMG_2934.pdf"
detected_type = "application/pdf"
best_guess = "PDF-Invoices"
confidence = 0.62
reason = "low_confidence"
created_at = "2026-11-19T14:00:00+00:00"
state = "new"
suggested_dest = "2026/PDF-Invoices"
```

`state`: `new` → `reviewed` → `resolved`. Only `reviewed` entries are ever
auto-archived, and only after 30 days. `reason`: `low_confidence`, `ambiguous`,
`extraction_failed`, `user_request`.

## `~/.sorta/.sorta_scan_state.json`

```json
{
  "root": "/home/u/Documents",
  "scanned": 300,
  "last_path": "/home/u/Documents/notes/2026-10.md",
  "started_at": "2026-11-19T09:00:00+00:00",
  "updated_at": "2026-11-19T09:04:11+00:00",
  "complete": false
}
```

Written every 100 files, atomically (temp file + rename). `sorta scan --resume`
skips everything up to and including `last_path`. A state file for a different root
is ignored.

## `~/.sorta/dedup_registry.json`

```json
{
  "entries": {
    "b3:abc123…": {
      "hash": "b3:abc123…",
      "canonical_path": "/home/u/Sorted/2026/PDF-Invoices/invoice.pdf",
      "size": 48123,
      "created_at": "2026-11-19T14:32:01+00:00"
    }
  }
}
```

Keyed by full BLAKE3 digest. Written atomically at the end of a run.

## `<destination_root>/_deferred/<YYYYMMDD-HHMMSS>-<name>.json`

Written when a file is locked after all five backoff attempts. The file itself is
left exactly where it was; the record is the queue entry.

```json
{
  "path": "/home/u/Downloads/locked.pdf",
  "error": "E_LOCKED_WIN: file is locked by another process: /home/u/Downloads/locked.pdf",
  "queued_at": "2026-11-19T14:32:01+00:00"
}
```

## `<destination_root>/_Duplicates/`

Where `on_duplicate = "move"` or `"keep_newest"` puts the losing copy. Names are
collision-renamed (`report (1).pdf`), never overwritten.

## `<destination_root>/_receipts/YYYY-MM-DD-HHMM.md`

Markdown. Summary table, the files that moved with rule and confidence, and a
"Left alone, and why" section. A receipt is written only when something actually
moved, and never on a dry run.

## `~/.sorta/.lock`

A single line: the owning process id. Created with `create_new`, so acquisition is
atomic. A lock whose pid is not alive is stale and is reclaimed.

## `<file>.sorta.json`

Written only when `sidecar_enabled = true`. Portable metadata that Obsidian,
DEVONthink or a script can read without Sorta.

```json
{
  "src": "/home/u/Downloads/acme.pdf",
  "dst": "/home/u/Sorted/2026/PDF-Invoices/acme.pdf",
  "rule": "keyword:invoice",
  "confidence": 0.82,
  "signals": ["keyword:invoice", "name:acme.pdf"],
  "sorted_at": "2026-11-19T14:32:01+00:00",
  "hash": "b3:abc123…",
  "version": "0.1.0"
}
```

## `~/.sorta/search_index.json`

The pure-Rust search index: one entry per sorted file, holding the name, the
detected category and up to 4 KB of extracted text. Capped at 20 000 documents
(oldest evicted). Replaced by the FTS5 + `sqlite-vec` index in phase 3.

```json
{
  "docs": [
    {
      "path": "/home/u/Sorted/2026/PDF-Invoices/acme.pdf",
      "name": "acme.pdf",
      "text": "Invoice from Acme Corp, November 2026 …",
      "size": 48123,
      "mtime": 1763551921,
      "kind": "PDF-Invoices"
    }
  ]
}
```

## `~/.sorta/instances.json`

Who is running right now. Pruned on read: a record survives only while its pid is
alive and its heartbeat is under 90 s old.

```json
{
  "instances": [
    {
      "instance": "default",
      "pid": 4021,
      "started_at": "2026-11-20T02:41:00+00:00",
      "last_heartbeat": 1763606460,
      "watch_roots": ["/home/u/Downloads"],
      "version": "0.1.0",
      "host": "workstation"
    }
  ]
}
```

## `~/.sorta/.coordinator.lock` and `~/.sorta/paused`

Both hold a pid or a timestamp respectively. The coordinator lock elects one
process among several; `paused` is the flag `sorta pause` sets and `sorta resume`
clears.

## `<destination>/<folder>/.sorta_lock_<hash>`

Held only while a move is in flight, and only when `multi_instance = true`. Created
with `create_new` so acquisition is atomic. Two lines: the owning pid, then the
machine name — a pid means nothing on another computer. Reclaimed when its mtime is
more than 60 s old locally, or 300 s on a network share.

## `~/.sorta/sorta.sock`

Unix domain socket, created by `sorta watch`. One JSON object per line, both ways:

```json
{"request":"status"}
{"response":"status","pid":4021,"paused":false,"instances":1,"indexed":412,"quarantined":3}
```

Removed on start if a previous run left one behind, because binding fails on an
existing path. Not created on Windows yet — a named pipe is pending, and nothing
depends on the channel: pause/resume is a flag file.

## `~/.sorta/audit.jsonl`

Append-only, one link per moved file. `prev` is the previous line's `chain`; the
first entry uses `genesis`. `chain` is BLAKE3 over `prev\0ts\0src\0dst\0rule\0hash`.

```json
{
  "ts": "2026-11-20T02:41:00+00:00",
  "src": "/home/u/Downloads/acme.pdf",
  "dst": "/home/u/Sorted/2026/PDF-Invoices/acme.pdf",
  "rule": "keyword:invoice",
  "hash": "b3:abc123…",
  "prev": "genesis",
  "chain": "b3:9f2c…"
}
```

## `~/.sorta/vectors.json`

Embeddings for the most recent 2 000 filed documents, written only when a local
embedding engine is configured. Disposable: losing it costs one re-embed per file.

```json
{
  "entries": [
    {
      "path": "/home/u/Sorted/2026/PDF-Invoices/acme.pdf",
      "hash": "b3:abc123…",
      "dim": 384,
      "values": [0.0312, -0.0088, …],
      "at": 1763551921
    }
  ]
}
```

## `~/.sorta/digest-YYYY-MM-DD.md`

Markdown, generated by `sorta digest`: outcomes, folders, deciding rules, and a
"needs a look" section when anything failed or was deferred.

## `<name>.sortapack`

A ZIP holding `manifest.json` and `sorta.toml`. Only behaviour travels: keyword
rules, folder template, confidence gate, collision and duplicate policy, ignore
patterns, retention, sidecar flag. Machine-specific keys (watch folders,
destination roots, instance name, credentials) are reset to defaults, so importing
a pack never repoints someone else's paths at your disk.

```json
{
  "name": "freelance",
  "description": "client paperwork",
  "pack_version": 1,
  "sorta_version": "0.1.0",
  "created_at": "2026-11-19T17:40:00+00:00",
  "keyword_rules": 2
}
```

`sorta packs import <file>` is a dry run: it prints the diff. `--apply` backs the
existing config up to `<config>.toml.bak` and then writes the merged one.

## `~/.sorta/last_scan`

A single RFC 3339 timestamp: when `scan` last ran. `--if-due` uses it so a minute
that fires twice (two schedulers, a manual run, a retry) still scans once.

## Reserved, not yet written

`~/.sorta/index.db` (SQLite WAL with `files`, `files_fts` FTS5, `vec_files`
sqlite-vec, `instances`, `dedup_registry`) and `<name>.sortapack` archives are
specified in the blueprint and land in phases 3–5.
