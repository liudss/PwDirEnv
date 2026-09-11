# CLI operations

`pwdir allow [PATH]` approves current bytes; `deny [PATH]` revokes approval.
A directory searches upward; an explicit file is used directly. No path uses cwd.
`status --json` reports native discovery/trust/cache, not host session memory.
`reload` invalidates cache only; it neither authorizes nor evaluates immediately.

`pwdir exec [--require-rc] DIR COMMAND [ARGS...]` passes arguments directly.
Place `--require-rc` before DIR. Without an RC, the default is inherited env.
Output is buffered until exit; it is not an interactive terminal proxy.
The RC timeout is 60 seconds; the target has no timeout. Target exit code propagates.

`export --format json [--force]` is the host protocol. Force never bypasses
trust. Exit 0 may contain an `error` or `blocked` action; inspect JSON.
General errors use 1, missing RC uses 2, and I/O errors use 3.

## Hook setup

For a standard installation:

```powershell
$installRoot = Join-Path $env:LOCALAPPDATA 'Programs\PwDirEnv'
$env:Path = (Join-Path $installRoot 'bin') + ';' + $env:Path
Import-Module (Join-Path $installRoot 'modules\PwDirEnv\PwDirEnv.psd1')
Enable-PwDirEnv
```

`hook install --yes` adds a marked block to `$PROFILE.CurrentUserAllHosts`;
`--path FILE` selects another profile. Keep bin on PATH in future sessions.
`hook uninstall` removes the block; `Disable-PwDirEnv` disables this session
and restores variables/prompt. Changes to installation or profile should stay
within the user's requested setup scope.

`doctor` checks installation/runtime. `Enable-PwDirEnv -PwDirPath PATH` or
`PWDIRENV_EXE` can select an executable without changing persistent PATH.
