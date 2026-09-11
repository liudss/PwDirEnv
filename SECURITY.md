# Security

## Execution boundary

`.envrc.ps1` is arbitrary PowerShell code. Authorization permits execution
with your account's permissions, including filesystem and network access.
A separate process protects host functions, aliases, and module state; it
does not restrict what the script can do to your machine.

The native core checks the top-level RC's SHA-256 before evaluation and again
before accepting an export result or starting an `exec` command. This detects
ordinary edits during evaluation, but it is not an atomic snapshot of the file
being executed and does not eliminate file-replacement races.

`source_env` and `source_up` do not authorize dependencies separately.
Review the source chain before `allow`. Metadata changes can trigger reloads,
but changes to sourced scripts do not require reauthorizing an unchanged RC.
Native imports and external commands are also the approved script's responsibility.

## Data handling

Trust and fingerprint cache files under `%LOCALAPPDATA%\PwDirEnv` contain
no environment values. Temporary evaluator results do contain values and are
removed on a best-effort basis. Files inherit directory permissions; no custom
ACL setup is performed.

`PWDIRENV_CONTEXT` contains Base64 JSON with baseline and applied values for
nested shells. **Base64 is not encryption.** Child processes inherit it. Do
not share it or raw export JSON. It is restoration metadata, not authorization.

Normal patch summaries list variable names only. Script errors and explicit
output can contain secrets; inspect diagnostics before sharing them.

## Reporting

During the private preview, report suspected vulnerabilities privately to
the repository owner through an existing private contact channel. Include
the version, a reproduction using synthetic data, and the impact. Do not
include credentials or real environment dumps. There is no dedicated
security-response SLA for this preview.
