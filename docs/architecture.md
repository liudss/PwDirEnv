# Architecture

This describes the implementation, not a future specification.

```text
PowerShell prompt -> host module -> pwdir export -> discovery/trust/cache
                                                     |
                                              evaluator pwsh
                                                     |
                                            temporary JSON result
                                                     |
Host Process environment <- JSON patch <- native environment diff
```

## Export flow

1. Discover the nearest `.envrc.ps1` and verify its SHA-256 authorization.
2. Unless forced, compare cached RC/script hashes and dependency metadata.
   An unchanged fingerprint returns `noop` without starting an evaluator.
3. Capture the inherited environment only when full evaluation is required.
   Unpack embedded bootstrap/stdlib scripts into a temporary directory.
4. Start `pwsh -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File ...`.
   The bootstrap sets its PowerShell location to the RC directory and sources
   the RC. Result JSON uses a separate file; stdout/stderr are drained independently.
5. Recheck authorization before accepting the result. On success, write
   fingerprint metadata and return the environment diff.

Export evaluation defaults to 10 seconds. Windows cleanup attempts
`taskkill /T /F` and directly kills the evaluator as fallback. Cleanup has
additional waiting windows; the evaluation timeout is not a total deadline.
Output is buffered. See [limitations](limitations.md).

## Host state

The host records each managed key's original existence and value, and applies
`set`/`unset` only to Process scope. On switching projects, it restores the old
baseline and obtains a patch for the new project. A fresh shell cannot apply
an empty disk-cache hit, so it forces evaluation when needed.

Same-project errors or blocked results keep the last successful environment.
On switching, the old environment is restored first. Same-project reevaluation
inherits applied values rather than reconstructing the original baseline.

`PWDIRENV_CONTEXT` carries baseline and applied values as Base64 JSON into
nested shells, allowing them to restore the inherited project environment.
It is not encrypted and is not an authorization credential.

The module wraps the global prompt, guards reentrancy, preserves native exit
status, and restores the saved prompt on disable/removal. It is not a daemon.

## Storage

Data lives under `%LOCALAPPDATA%\PwDirEnv` in `allow`, `cache`, and `state`.
The state directory is created but does not persist session state. Trust/cache
filenames hash a lexically normalized absolute RC path. Paths are not resolved
through junctions or symlinks, and `file_id` is null.

Cache stores hashes, dependency paths, size/mtime, and an evaluation timestamp,
never full environment values. Trust/cache writes replace files through
same-directory temporary files and rename. Missing, malformed, or unknown-schema
cache entries are misses; other cache I/O failures propagate.

Dependencies use size/mtime, not hashes. A missing watch can stay unchanged
while absent. Missing sourced files and metadata errors invalidate the cache.
Directory watches inspect directory metadata, not descendants. Changes to the
inherited environment are not fingerprint inputs.

## Protocol

Schema 1 exports contain `schema`, `action`, `rc`, `patch` (`set` map and
`unset` array), `managed_keys`, `fingerprint`, and `diagnostics`. Actions are
`noop`, `apply`, `blocked`, `error`, and reserved `unload`. Export does not
currently emit `unload`; the host handles it defensively.

Blocked/error patches are empty. `noop` can mean no RC, an empty evaluated diff,
or a cache hit, so callers must also inspect `rc`. Fingerprints include RC and
embedded-script hashes plus watch/source states.

Native diff compares names using ASCII uppercase normalization and preserves
display casing. JSON uses Unicode strings with lossy conversion for invalid
OS strings. The host parses data and sets values directly, never evaluates
generated source. See `crates/pwdir-protocol/src/lib.rs` for field types.
