# Privacy

Version 1.0 · effective 20 Nov 2026 · owner: the Sorta maintainers · changes:
first version.

## The short version

Sorta is a program that runs on your computer. **The developer receives nothing.**
There is no account, no telemetry, no analytics, no crash reporting and no update
check in this build. Everything Sorta knows is written to files on your own disk,
listed below, and you can delete them at any time.

## What Sorta reads

Only the folders you point it at — `sorta sort`, `sorta scan` and `sorta watch`
never walk anywhere else. It reads file names, file metadata, and the content it
needs to classify a file: up to 256 KB of plain text, the text layer of a PDF, or
the text parts of a DOCX/XLSX/PPTX.

## What Sorta writes, and where

| File | What it holds | Delete it to remove |
|---|---|---|
| `~/.sorta/sorta_history.jsonl` | source and destination paths, rule, confidence, signals, content hash | `rm` the file |
| `~/.sorta/search_index.json` | path, file name, detected category, up to 4 KB of extracted text per file | `rm` the file |
| `~/.sorta/vectors.json` | embeddings of up to 2 000 recent documents, written only if you configure a local embedding engine | `rm` the file |
| `~/.sorta/dedup_registry.json` | content hashes and the path of the canonical copy | `rm` the file |
| `~/.sorta/audit.jsonl` | paths, rule and chained hashes, only if `audit_enabled = true` | `rm` the file |
| `~/.sorta/instances.json` | instance name, process id, watch roots, heartbeat | `rm` the file |
| `~/.sorta/.sorta_scan_state.json`, `last_scan`, `paused`, `.lock`, `.coordinator.lock` | run state: how far a scan got, when it last ran, whether it is paused, who holds a lock | `rm` the files |
| `<destination>/_receipts/*.md` | a Markdown receipt of each run | `rm` the folder |
| `<destination>/_deferred/*.json`, `Quarantine/quarantine.toml` | why a file was deferred or is waiting for review | `rm` the folder |
| `<file>.sorta.json` | per-file sidecar metadata, only if `sidecar_enabled = true` | `rm` the sidecar |

Extracted text is the most sensitive thing here: if you sort an invoice, up to 4 KB
of its text is in `search_index.json`. That is a deliberate trade — it is what makes
`sorta search` work offline — and deleting that one file removes all of it.

## Erasing it

```
sorta forget /path/to/a/document        # one file: index, embeddings, duplicates, quarantine
sorta forget /path --history            # and rewrite the history log
sorta forget --all                      # every record Sorta keeps (asks for --yes)
```

`--all` lists what it will delete and does nothing until you add `--yes`. It removes
only Sorta's own records — **your documents are never touched**.

One deliberate exception: the audit chain (`audit.jsonl`, only written when
`audit_enabled = true`) is not rewritten, because editing it would make every later
link unverifiable and destroy the only property it has. `sorta forget` says so and
tells you the file to delete if you want it gone too.

## What leaves your machine

**Nothing, in this build.** Two paths exist in the code that could, and both are off
by default:

1. **A cloud language model.** `llm_mode = "byok"` plus `llm_send_content = true`
   would permit sending document text to the provider you name. The request path is
   **not implemented yet**: in this build `complete()` returns nothing and no
   connection is opened. When it is implemented, this document changes first.
2. **A local embedding engine.** `[embedding]` runs a program you install, on your
   own machine, with text passed as an argument. Sorta makes no network call for
   this. The reference engine, CrispEmbed, **may download model files from the
   internet on first use** — that is that program's behaviour, not Sorta's, and it
   is worth knowing before you enable it.

Two other things worth naming:

* **Hooks.** `[hooks] on_sort` runs a shell command you write. That command can send
  anything anywhere; it is your command and your responsibility. Sorta quotes every
  substituted value so a filename cannot inject one, and that is the limit of what
  it can guarantee.
* **Your operating system.** Sorta writes files; your OS, backups and sync clients
  may copy them elsewhere. That is outside Sorta's control.

## The encrypted vault

`sorta vault lock <file>` stores a file as AES-256-GCM ciphertext in
`<destination>/_Sensitive/` and (unless `--keep`) removes the plaintext, after
decrypting the copy once to prove it round-trips. `sorta vault unlock` restores it.

The key comes from the first of: `SORTA_VAULT_KEY` (64 hex characters), a
`[vault] key_command` that prints it (any secret store you already use —
`secret-tool`, `pass`, `op`, `security`), or `~/.sorta/vault.key` written `0600` by
`sorta vault init`. There is no `keyring` dependency: its Linux backend needs a
Secret Service that headless machines lack, and a keychain that is absent fails
mysteriously where a command that fails says why. If you set `key_command`, Sorta
writes no key file at all.

What that means in plain terms: the vault protects the *contents* of a file — from
a backup, a sync client, a colleague with read access, a stolen disk image. It does
not protect against someone who already has your user account, because the key is on
the same machine. If you need that, keep the key somewhere else and pass it through
`SORTA_VAULT_KEY`.

`[vault] auto_lock = true` locks a file as it is filed, when the rule that placed it
sends it into `_Sensitive/`. The order matters and is deliberate: the file is
encrypted **before** it is indexed, so a locked file's text never reaches
`search_index.json` and no embedding is stored for it — only its name stays
searchable. The lock is verified by decrypting the copy in memory and comparing it
with the original before the plaintext is removed; if the key is missing the file
stays filed and readable, and the log records `vault:failed:`.

Files locked by an earlier `sorta vault lock` are not re-indexed, so an index entry
from before the lock may still hold text; `sorta forget <path>` clears it.

## If you sort other people's documents

If you point Sorta at client, patient or employee documents, you are the one
deciding to process that data, on your own device. Sorta is a tool you run, not a
service that receives data. If you later enable a cloud model, the provider you name
becomes a recipient of that text and your own obligations follow.

## Legal framing

Written plainly, because the honest answer is short:

* The developer collects no personal data, so there is no notice-and-consent duty to
  discharge at the developer's end under the **Digital Personal Data Protection Act,
  2023** (application and exemptions: ss. 3 and 17; checked 20 Nov 2026).
* If you process other people's personal data with Sorta, **you** are the Data
  Fiduciary and the Act's obligations are yours. Sorta does not change them.
* The **IT Act, 2000** s. 43A and the SPDI Rules, 2011 bind bodies corporate handling
  sensitive personal data. Nothing here is collected by the developer, so those
  duties do not attach to the developer; they may attach to you as the operator.
* No claim of compliance is made or implied by this document. It describes what the
  code does.

This is a description of the software, not legal advice, and it should be reviewed
by a qualified advocate before it is relied on.
