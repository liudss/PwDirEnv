# CLI and module reference

Run `pwdir --help` or `pwdir COMMAND --help` for argument syntax.

| Command | Details |
| --- | --- |
| `allow [PATH]` | Authorize current bytes. A directory searches upward; an explicit file is used directly. No path searches from cwd. |
| `deny [PATH]` | Remove authorization; no matching RC is a successful no-op. |
| `status [--json]` | Report discovery, trust, and cache. Does not inspect parent-shell memory. |
| `reload` | Invalidate the nearest RC cache without evaluating or changing trust. No RC is a successful no-op. |
| `exec [--require-rc] DIR COMMAND [ARGS...]` | Evaluate an approved RC and run COMMAND in DIR. Without an RC, inherit the environment unless `--require-rc` is set. Put this option before DIR. |
| `export --format json [--force]` | Host protocol. Force bypasses cache, never authorization. Only JSON is supported. |
| `hook pwsh` | Print the module-import and enable snippet. |
| `hook install [--yes] [--path FILE]` | Append a marked block to the all-hosts profile or FILE. |
| `hook uninstall [--path FILE]` | Remove the block; a missing file is a successful no-op. |
| `doctor [--json]` | Check OS, pwsh availability/version, module, profile, data/temp directories, execution policy, and long-path setting. Warnings do not fail the command. |

`exec` passes arguments directly without shell-string interpolation. It
inherits stdin, but buffers stdout/stderr until the command exits. It is not
an interactive terminal proxy. Its RC evaluation timeout is 60 seconds;
the target command itself has no timeout.

General errors use exit 1, missing RC uses 2, and I/O errors use 3. Argument
parser errors also use 2. `exec` propagates the target's exit code. `export`
can exit 0 with `blocked` or `error`; inspect its JSON action.

## PowerShell functions

| Function | Details |
| --- | --- |
| `Enable-PwDirEnv [-PwDirPath PATH]` | Resolve the executable and wrap the global prompt; idempotent. |
| `Disable-PwDirEnv` | Restore managed values and the saved prompt. |
| `Invoke-PwDirEnvHook` | Run a prompt check explicitly; log errors and ignore reentrant calls. |
| `Get-PwDirEnvStatus` | Report this module's active RC, managed names, enabled state, and executable. |

Resolution uses `-PwDirPath`, then `PWDIRENV_EXE`, then PATH and development
locations. Explicit paths do not silently fall back. The module's reserved
`Fingerprint` status field remains null; use native status for cache freshness.
