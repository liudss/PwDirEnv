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

Use GitHub for security reports:

1. Open [Report a vulnerability](https://github.com/liudss/PwDirEnv/security/advisories/new)
   when GitHub private vulnerability reporting is enabled. Include the version,
   impact, and a reproduction using synthetic data.
2. If that form is unavailable, open a [GitHub issue](https://github.com/liudss/PwDirEnv/issues/new)
   asking the maintainer to enable vulnerability reporting. Do not include
   vulnerability details, exploit code, credentials, or environment dumps in
   that public issue. Submit those details through the advisory form once enabled.

The maintainer must enable private vulnerability reporting in GitHub repository
settings when the repository becomes public. This is GitHub's confidential
reporting channel, not a personal contact requirement. There is no guaranteed
security-response SLA for this early preview. Fixes target the latest revision
on `main`; older preview releases do not have separate maintenance branches.
