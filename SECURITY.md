# Security policy

Version 1.0 · effective 20 Nov 2026.

## Supported versions

Sorta is pre-1.0. Only the latest release gets fixes; there are no backports.

| Version | Supported |
|---|---|
| 0.1.x (latest) | yes |
| anything older | no |

## Reporting a vulnerability

Please do **not** open a public issue for a vulnerability.

1. **Preferred:** GitHub → the repository's **Security** tab → *Report a
   vulnerability* (private advisory). This keeps the report, the fix and the
   eventual disclosure in one place.
2. **If that is unavailable:** email **sohilmomin2000@gmail.com** with `Sorta
   security` in the subject.

Include what you have: affected version or commit, platform, a description, and the
smallest reproduction you can manage — a file, a folder layout, or the exact
command. A crash with a backtrace is more useful than a theory.

## What to expect

Best effort, by one maintainer, with no SLA and no bug bounty:

* acknowledgement within about 7 days,
* an assessment of severity and a fix or mitigation plan after that,
* credit in the release notes if you want it.

## In scope

The `sorta` binary and the crates in this repository. Specific areas where a bug
would be serious:

* **Path handling** — traversal, symlinks, `..`, Windows device names, long paths,
  case-insensitive collisions, anything that moves a file somewhere it should not.
* **Data loss** — a move that loses bytes, a rollback that does not roll back, a
  `replace` policy that destroys a file it should have kept, retention deleting
  something it promised not to touch.
* **Hook quoting** — a filename that escapes the quoting in `[hooks] on_sort` and
  runs a command of its own.
* **Locks** — a stale lock that wedges a folder forever, or two processes racing into
  the same destination name.
* **Audit chain** — an edit to `audit.jsonl` that `sorta audit verify` reports as
  intact.
* **Parsing** — a crafted file that panics the binary, or that makes it allocate
  without bound.
* **Secrets** — anything that leaks a key you configured.
* **The control channel** — a loopback TCP listener whose port is published in
  `~/.sorta/port`. It answers `ping` and `status` only, reads nothing from the caller
  beyond the request, and is bound to `127.0.0.1`. Any local process can query it;
  that is the same trust level as the state directory itself. If a future version adds a
  command that changes state, it must authenticate first; a report that it does not
  is a valid finding.

## Out of scope

* Bugs in dependencies (report them upstream — `turso`, `pdf-extract`, `notify` and
  the rest; tell us too if it affects Sorta).
* The quality or safety of classification decisions, and the output of any embedding
  or language model you configure. Sorta never deletes a file on the strength of a
  guess; a wrong folder is a bug, not a vulnerability.
* Hooks you write yourself, and anything the program you install as an embedding
  engine does.
* Vulnerabilities that require an attacker to already have write access to your home
  directory.

## Good-faith research

Testing on your own machine, with your own files, is fine. Do not run tests against
other people's systems, do not access data that is not yours, and give us a
reasonable window to fix a finding before publishing it.

## A note on the audit chain

`audit_enabled = true` writes a BLAKE3 hash chain, which detects an edit by anyone
who does not recompute the chain. It is not a signature: someone with write access
who recomputes every link can produce a consistent forgery. If you need that
guarantee, sign or ship the log off the machine.
