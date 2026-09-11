# Troubleshooting

## Blocked configuration

Run `pwdir status`, inspect the RC and its source chain, then `pwdir allow`
if you trust it. A top-level byte change invalidates approval; `reload` does
not grant authorization.

## Hook not running

Run `pwdir doctor` and `Get-PwDirEnvStatus`. Ensure `bin` is on PATH or set
`PWDIRENV_EXE`. Use `Enable-PwDirEnv -PwDirPath PATH` to select explicitly.
Enable after other prompt tools if they replace its wrapper. `pwsh -NoProfile`
does not load the installed hook. Non-filesystem locations unload the environment.

## Old values after reload or failure

Same-project reload inherits applied values. Leave/reenter or disable/reenable
for a clean baseline. Dotenv needs an explicit watch and preserves existing
values unless `-Overload` is intentional. Same-project errors and blocked
results keep the last successful environment. Denying an RC does not erase
values already loaded in another shell. See [configuration](configuration.md).

## Missing module or stale installation

Rerun the installer and open a new shell. `Import-Module PwDirEnv` needs an
appropriate PSModulePath; the installed hook uses an absolute module path.

## Slow commands or missing output

Full evaluation starts PowerShell. `exec` buffers output until its target exits.
Avoid background children inheriting evaluator output handles. There is no
measured latency promise. Report OS/PowerShell versions and synthetic examples,
not raw export JSON or `PWDIRENV_CONTEXT`.
