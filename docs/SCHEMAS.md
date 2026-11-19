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

## Reserved, not yet written

`~/.sorta/index.db` (SQLite WAL with `files`, `files_fts` FTS5, `vec_files`
sqlite-vec, `instances`, `dedup_registry`), `<file>.sorta.json` sidecars, and
`<name>.sortapack` archives are specified in the blueprint and land in phases 3–5.
