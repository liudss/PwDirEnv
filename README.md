# PwDirEnv

Directory-specific environments for Windows and PowerShell.

PwDirEnv loads an approved `.envrc.ps1` when you enter a project and restores
the environment when you leave. Configuration runs in a separate PowerShell
process; only environment-variable changes return to your shell.

**Status: early preview.** Windows and PowerShell 7.2+ are
the intended platform. There is no published winget package or measured
performance guarantee. See [known limitations](docs/limitations.md).

## Quick start

Build and install from a checkout with Rust and PowerShell 7 on PATH:

```powershell
pwsh -NoProfile -File packaging/install.ps1
```

Enable for the current session:

```powershell
$installRoot = Join-Path $env:LOCALAPPDATA 'Programs\PwDirEnv'
$env:Path = (Join-Path $installRoot 'bin') + ';' + $env:Path
Import-Module (Join-Path $installRoot 'modules\PwDirEnv\PwDirEnv.psd1')
Enable-PwDirEnv
```

Create `.envrc.ps1` in a project:

```powershell
$env:APP_ENV = 'development'
PATH_add '.\node_modules\.bin'
dotenv_if_exists '.env.local'
watch_file '.env.local' 'package.json'
```

Read the file and anything it sources before authorizing it:

```powershell
Get-Content .envrc.ps1
pwdir allow
```

The environment loads at the next prompt. Leaving the project restores the
saved values of managed variables. Editing the RC invalidates its approval;
review the change before running `pwdir allow` again.

**An approved script is arbitrary code running as you. The subprocess is not
a security sandbox.** See [security](SECURITY.md).

## Everyday commands

| Command | Purpose |
| --- | --- |
| `pwdir allow [PATH]` | Authorize the current configuration contents |
| `pwdir deny [PATH]` | Revoke authorization |
| `pwdir status --json` | Inspect discovery, trust, and cache |
| `pwdir reload` | Invalidate the cache for the next prompt |
| `pwdir exec DIR COMMAND [ARGS...]` | Run a command with the project environment |
| `pwdir doctor` | Check the installation and runtime |
| `pwdir hook install --yes` | Add a marked block to the all-hosts PowerShell profile |
| `pwdir hook uninstall` | Remove that profile block |

`Disable-PwDirEnv` restores managed variables and the original prompt in the
current session. `Enable-PwDirEnv` turns the hook back on.

## Documentation

- [Installation, updates, and removal](docs/installation.md)
- [CLI and module reference](docs/cli.md)
- [Writing configuration](docs/configuration.md)
- [Architecture and protocol](docs/architecture.md)
- [Limitations and implementation audit](docs/limitations.md)
- [Troubleshooting](docs/troubleshooting.md)
- [Architecture decisions](docs/adr/README.md)
- [Contributing and testing](CONTRIBUTING.md)
- [Agent skill](skills/pwdirenv/SKILL.md)

## Feedback and contributions

Use [GitHub Issues](https://github.com/liudss/PwDirEnv/issues) for bugs,
questions, and feature requests, and [Pull Requests](https://github.com/liudss/PwDirEnv/pulls)
for changes. See [CONTRIBUTING.md](CONTRIBUTING.md) for checks and review expectations.
For vulnerabilities, follow [SECURITY.md](SECURITY.md).

CI tests Windows, scans Git history for secrets, and checks dependency
advisories and licenses. Tagged versions produce a draft source release only
after these checks pass; see [the release process](docs/releasing.md).

## License

[MIT](LICENSE).
