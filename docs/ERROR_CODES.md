# Error codes

Every failure carries a stable `E_*` code. Codes appear in `outcome.rule` for
failed outcomes and in the history entry's `rule` field, so a log line is greppable
without parsing prose.

| Code | Meaning | Handling |
|---|---|---|
| `E_LOCKED_WIN` | Windows os error 32/33 — file held open | exponential backoff 1s→2s→4s→8s→16s, 5 attempts, then `Deferred` |
| `E_PERM` | permission denied / ACL refusal | report the exact path, skip, never retry silently |
| `E_NETWORK` | path is on a network mount | switch that root to 30 s mtime polling |
| `E_INOTIFY_LIMIT` | `fs.inotify.max_user_watches` too low | warn once, fall back to polling |
| `E_LONG_PATH` | path past the platform limit | apply `\\?\` prefix on Windows |
| `E_BAD_NAME` | illegal characters or reserved name | sanitise before the move |
| `E_CASE_COLLISION` | name differs only by case | apply `on_collision` policy |
| `E_DB_BUSY` | index lock held past the busy timeout | degrade to read-only, notify |
| `E_COORD_DEAD` | coordinator heartbeat lost | re-elect within 30 s |
| `E_MODEL_LOAD` | model artifact failed to load | drop a tier, log once, continue |
| `E_IPC` | named pipe / Unix socket failure | retry 3×, then single-instance mode |
| `E_IO` | anything else from the filesystem | report and skip |

## What the user sees

`sorta sort` prints one line per file. A failure prints the code and the path:

```
failed   /home/u/Downloads/locked.pdf  (E_LOCKED_WIN: file is locked by another process: /home/u/Downloads/locked.pdf)
```

Exit codes: `0` success, `1` at least one file failed, `2` usage error (no such
file, no folder given, unsupported platform).

## Which errors are retryable

`SortaError::is_retryable` returns true for `E_LOCKED_WIN` and `E_DB_BUSY` only.
Everything else is a decision, not a transient condition: retrying a permission
error just wastes the user's time with the same outcome.

## Invariant

No error path deletes a file. The worst case is a file left exactly where it was,
with an entry in the log saying why.
