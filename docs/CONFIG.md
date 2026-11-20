# Configuration reference

Resolution order (first hit wins):

1. `--config <path>`
2. `./sorta.toml` (portable mode)
3. `~/.config/sorta/<instance>.toml` — `%APPDATA%\sorta\<instance>.toml` on Windows
4. built-in defaults

Create one with `sorta config init`; inspect the effective values with
`sorta config show`; find the path in use with `sorta config path`.

Relative paths (`destination_root`, `quarantine_root`, `backup_root`) resolve
against the directory holding the config file — that is what makes portable mode
portable.

## Keys

| Key | Type | Default | Meaning |
|---|---|---|---|
| `watch_folders` | array of paths | `[]` | folders `scan` and `watch` cover |
| `destination_root` | path | `./Sorted` | root of the sorted tree |
| `quarantine_root` | path | `./Quarantine` | where low-confidence files wait |
| `folder_template` | string | `{year}/{category}` | sub-path under the destination; tokens `{year} {month} {category} {kind} {ext}` |
| `on_collision` | `rename` \| `skip` \| `replace` \| `keep_both` | `rename` | what to do when the name is taken |
| `archive_mode` | `as_file` \| `extract` \| `ask` | `as_file` | archive handling |
| `on_duplicate` | `skip` \| `move` \| `keep_newest` | `skip` | what to do with a byte-identical duplicate |
| `duplicates_root` | path | `./_Duplicates` | where `move` / `keep_newest` put the loser |
| `backup_before_move` | bool | `false` | copy to `backup_root` before moving |
| `backup_root` | path | `./_sorta_backup` | backup destination |
| `min_confidence` | float 0–1 | `0.7` | below this, quarantine instead of filing |
| `run_when_idle` | bool | `false` | only work when the machine is idle |
| `schedule` | cron string | `""` | scheduled scans |
| `multi_instance` | bool | `false` | allow more than one process |
| `network_multi_instance` | bool | `false` | coordinate across a network share |
| `instance_name` | string | `default` | this instance's name |
| `allow_mixed_versions` | bool | `false` | accept workers on other versions |
| `llm_mode` | `off` \| `auto` \| `bundled` \| `byok` \| `auto_with_fallback` | `auto_with_fallback` | LLM tier policy |
| `llm_endpoint` | string | `""` | explicit endpoint (overrides probing) |
| `llm_provider` | string | `""` | `openai` \| `anthropic` \| `groq` \| `openrouter` \| `gemini` \| `openai-compatible` |
| `llm_send_content` | bool | `false` | allow document text to leave the machine |
| `semantic_enabled` | bool | `true` | embedding index for dedupe/clustering |
| `autopilot_enabled` | bool | `true` | learn from manual corrections |
| `context_enabled` | bool | `false` | calendar/email signals |
| `predictive_enabled` | bool | `false` | cleanup suggestions |
| `plugins_enabled` | bool | `false` | WASM plugin host |
| `preview_enabled` | bool | `false` | document preview |
| `sidecar_enabled` | bool | `false` | write `<file>.sorta.json` beside each sorted file |
| `[keyword_rules]` | table | `Invoice`, `Contract` | case-insensitive keyword → folder |
| `[ignore_patterns]` | table with `patterns` | download/temp globs | never touched |
| `[retention] enabled` | bool | `false` | age-based cleanup, off unless asked |
| `[retention] max_age_days` | int | `365` | files at or past this age are candidates |
| `[retention] folders` | array | `[]` | folders covered; empty means the whole tree, protected folders always skipped |
| `[retention] compress_instead` | bool | `false` | gzip old files instead of deleting them |
| `[embedding] engine` | `off` \| `auto` \| `command` | `auto` | where text embeddings come from |
| `[embedding] command` | string | `crispembed -m all-MiniLM-L6-v2` | command line, text appended as the last argument |
| `[embedding] dimensions` | int | `384` | vector width the engine produces |
| `[embedding] near_duplicate_threshold` | float | `0.92` | cosine at or above which two documents are near-duplicates |
| `[embedding] near_duplicate_action` | `off` \| `suggest` \| `quarantine` | `suggest` | what to do when it fires |
| `audit_enabled` | bool | `false` | append a tamper-evident hash chain to `~/.sorta/audit.jsonl` |
| `[hooks] on_sort` | string | `""` | shell command run after a successful move |
| `max_files_per_second` | int | `20` | global ceiling on moves; `0` disables it |

## Collision policies

| Policy | Behaviour |
|---|---|
| `rename` | `report.pdf` → `report (1).pdf`, `report (2).pdf`, … |
| `skip` | leave the source where it is, log `skipped` |
| `replace` | remove the existing destination, then move |
| `keep_both` | `report_20261119-143201.pdf` |

Collision detection is case-insensitive on every platform, because NTFS cannot
hold `Invoice.pdf` and `invoice.pdf` side by side and a case-sensitive check would
create two files that later collide.

## Duplicate policies

| Policy | Behaviour |
|---|---|
| `skip` | leave the duplicate where it is, log the match, report space saved |
| `move` | move the duplicate into `duplicates_root`, collision-renamed |
| `keep_newest` | compare mtime: the newer copy is filed, the older one moves to `duplicates_root` |

No policy deletes anything. `keep_newest` on a missing or unreadable timestamp
falls back to `skip`, because "which is newer" is then unknowable.

## Multi-instance

`multi_instance = true` lets more than one Sorta process run, and changes two
things: a destination folder is locked for the duration of each move
(`.sorta_lock_<hash>`, `create_new`, reclaimed when older than 60 s, so a crashed
process cannot wedge a folder), and a busy folder defers the file instead of racing
for the name. Running instances are listed in `~/.sorta/instances.json` and shown by
`sorta status`; a record is pruned when its pid is gone or its heartbeat is over 90 s
old.

`max_files_per_second` caps the whole process, so a bulk scan cannot saturate a disk
or a share. It is a fixed one-second window, not a token bucket: the goal is "do not
hammer the filesystem", and a window does that without a background thread.

## Hooks

```toml
[hooks]
on_sort = "notify-send 'Sorted' {path}"
```

Tokens: `{path}` (destination), `{src}`, `{rule}`, `{confidence}`. Every value is
shell-quoted, so a filename containing `; rm -rf ~` stays data. The hook is
**spawned, not awaited**: output is discarded and the exit code is not read, so a
slow or hanging hook cannot stall a sort. Empty means no hook, which is the
default.

## Audit chain

`audit_enabled = true` writes `~/.sorta/audit.jsonl`, where each line carries the
chain hash of the line before it. `sorta audit verify` walks it and reports the
first entry that was edited, reordered or removed; `sorta audit show 20` lists the
tail. It detects editing by anyone who does not recompute the chain — it is not a
signature, and the docs say so.

## Embeddings

Sorta ships no model weights, so it cannot embed text by itself. `[embedding]`
points at a **local** tool instead — the reference is CrispEmbed
(`crispembed -m all-MiniLM-L6-v2 "text"`), spawned as a subprocess, never linked.
Output is read as the longest run of numbers in stdout, so a JSON array or a plain
list both work.

Near-duplicates: with an engine present, every filed document's vector is kept in
`~/.sorta/vectors.json` (2 000 most recent) and compared with brute-force cosine.
`near_duplicate_action = "suggest"` files the document and records the match;
`"quarantine"` holds it for review with reason `possible_duplicate`. The spec calls
for quarantine; the default is suggest because a false positive costs a review
step, and the threshold is a property of the engine, not of the user's taste.

`engine = "auto"` uses `command` when it resolves, otherwise looks for
`crispembed` on `PATH`, otherwise stays off. With no engine, semantic features stay
dark and everything else works unchanged. Check it with `sorta embed "some text"`.

## Scheduling and idle gating

`schedule` is a five-field cron expression (`minute hour day month weekday`,
supporting `*`, `a`, `a-b`, `*/n`, `a-b/n` and comma lists). Sorta runs no daemon:
the OS scheduler calls `sorta scan --if-due`, which evaluates the expression
itself and refuses to run twice in the same minute. `sorta schedule` prints the
next three firing times and the exact line to install.

`run_when_idle = true` makes `scan` check the one-minute load average first
(Linux; Windows currently reports idle) and skip with a message when the machine
is busy. `--force` overrides it.

## Retention

`sorta retention` prints what the policy would delete; `sorta retention --apply`
deletes it. The walk never enters `Quarantine`, `Unsorted`, `_deferred`,
`_Duplicates`, `_receipts` or `_sorta_backup`, whatever the config says — an
unreviewed file is never a cleanup candidate.

## Example

```toml
destination_root = "/home/u/Sorted"
quarantine_root = "/home/u/Sorted/Quarantine"
folder_template = "{year}/{category}"
on_collision = "rename"
min_confidence = 0.75
watch_folders = ["/home/u/Downloads", "/home/u/Desktop"]
llm_mode = "auto"
semantic_enabled = true

[keyword_rules]
"Invoice" = "PDF-Invoices"
"Contract" = "Contracts"
"Payslip" = "Payslips"

[ignore_patterns]
patterns = ["*.crdownload", "*.part", "*.tmp", "~$*", ".DS_Store", "*.iso"]
```

## Notes on individual keys

**`folder_template`** is an extension beyond the original key list. The default
`{year}/{category}` reproduces the spec's example layout
(`Sorted/2026/PDF-Invoices/…`). Set it to `{category}` for a flat tree, or to
`{year}/{month}-{category}` for the spec's `11-PDF-Invoices` form.

**`min_confidence`** interacts with every rule: a keyword match in the filename
scores 0.82, in extracted text 0.74, a format match 0.90, and an unrecognised file
0.45. Raising the gate to 0.85 sends text-matched files to review; lowering it to
0.5 files unknown blobs by format instead of asking.

**`llm_send_content`** is the only key that permits a request carrying document
content, and it only takes effect at Tier 3 with `byok`. `sorta status` prints
`offline` whenever it is false.
