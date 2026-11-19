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
| `[keyword_rules]` | table | `Invoice`, `Contract` | case-insensitive keyword → folder |
| `[ignore_patterns]` | table with `patterns` | download/temp globs | never touched |

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
