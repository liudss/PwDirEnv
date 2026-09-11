# Installation

## From source

Use Windows, PowerShell 7.2+, and Rust stable. Ensure `cargo` and `pwsh` are
on PATH. Windows PowerShell 5.1 is unsupported. From the repository root:

```powershell
pwsh -NoProfile -File packaging/install.ps1
```

The installer builds a release executable, assembles `dist/pwdir`, then copies
it to `%LOCALAPPDATA%\Programs\PwDirEnv`. Use `-InstallRoot PATH` for another
location, or `-SkipBuild` after building release yourself. It includes the
module, skill, license, and documentation. It does not change PATH, module
search paths, or profiles. No winget package is published.

## Enable

For the current session:

```powershell
$installRoot = Join-Path $env:LOCALAPPDATA 'Programs\PwDirEnv'
$env:Path = (Join-Path $installRoot 'bin') + ';' + $env:Path
Import-Module (Join-Path $installRoot 'modules\PwDirEnv\PwDirEnv.psd1')
Enable-PwDirEnv
pwdir doctor
```

For future sessions, keep `bin` on PATH through your PowerShell profile or
Windows user PATH, then run `pwdir hook install --yes`. It adds a marked block
to `$PROFILE.CurrentUserAllHosts`, importing the installed module by absolute
path. `--path FILE` selects another profile. Without `--yes`, it asks before
editing when stdin is an interactive terminal; noninteractive input does not
prompt. Open a new PowerShell window afterward.

Another tool replacing `prompt` after PwDirEnv can bypass its wrapper. Enable
PwDirEnv after that tool's prompt initialization if needed.

## Update and remove

Run `Disable-PwDirEnv`, rebuild, and rerun the installer to update. Start a
new shell afterward. Embedded-script changes invalidate cache fingerprints;
existing RC authorizations remain valid for unchanged RC content.

Run `pwdir hook uninstall` to remove the profile block, and `Disable-PwDirEnv`
in active shells. Remove any PATH entry you added and delete only your chosen
installation directory. `%LOCALAPPDATA%\PwDirEnv` retains trust/cache data;
remove it separately only if you want to reset all authorizations.

## Agent skill

The installer places the skill in `<InstallRoot>\skills\pwdirenv`. Copy that
directory into your agent's skill directory if desired. Shell installation
does not automatically overwrite registered skills.
