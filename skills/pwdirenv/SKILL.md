---
name: pwdirenv
description: Configure or troubleshoot PwDirEnv (pwdir), the Windows PowerShell directory environment tool, and its .envrc.ps1 files.
metadata:
  product: PwDirEnv
  version: "0.1.0"
---

# PwDirEnv

Requires Windows, PowerShell 7.2+, and an installed pwdir. RC scripts execute in a child process; only
environment changes return to the host. It is not a security sandbox.

Before `allow`, read the RC and its source chain and explain the effects.
Obtain approval unless the conversation already authorizes those reviewed
contents. Installation alone does not authorize project scripts. A top-level
edit requires renewed approval; sourced scripts have no separate trust check.

Use `pwdir status` for native state and `Get-PwDirEnvStatus` for this shell.
Do not expose environment values or `PWDIRENV_CONTEXT` in reports.

Load only the reference needed for the task:

- Writing RCs or choosing helpers: [rc-authoring.md](references/rc-authoring.md).
- CLI actions or hook setup: [cli.md](references/cli.md); use subcommand `--help` for full syntax.
- Blocked RCs, reload, restoration, or startup failures: [troubleshooting.md](references/troubleshooting.md).
