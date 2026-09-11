# Contributing

Keep changes focused and describe observable behavior before and after the
change. Use English for documentation, comments, and diagnostics. Non-English
fixtures are retained to test Unicode handling.

## Development and testing

Use Windows, PowerShell 7.2+ (`pwsh` on PATH), and Rust stable with rustfmt
and Clippy. A minimum Rust version has not been validated across dependencies.

```powershell
cargo build --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
pwsh -NoProfile -File tests/smoke.ps1
pwsh -NoProfile -File tests/e2e_trust.ps1
pwsh -NoProfile -File tests/e2e_hook.ps1
pwsh -NoProfile -File tests/e2e_path_torture.ps1
```

Run PowerShell integration scripts in separate processes: they change process
environment variables and use temporary trust stores. Rust integration tests
require a real `pwsh`. The standalone E2E scripts use the debug executable,
so build it first. With Pester 5 installed, also run
`Invoke-Pester ./tests/PwDirEnv.Tests.ps1`. Its guard skips older versions;
a skip is not a passing test.

## Structure and conventions

- `crates/pwdir-protocol`: serializable data structures.
- `crates/pwdir-core`: discovery, trust, cache, diff, evaluator, and execution.
- `crates/pwdir-cli`: arguments, output, and profile text handling.
- `powershell/PwDirEnv`: host environment restoration and prompt integration.
- `evaluator`: embedded PowerShell bootstrap and configuration helpers.
- `packaging`: source installer.

Keep native output as JSON data, never source for the host to evaluate.
Environment patches affect Process scope only. Tests must isolate trust stores
and use synthetic configurations, not authorize a contributor's actual RC.

Update relevant docs alongside behavior changes. Record significant decisions
in `docs/adr`; avoid task-dispatch plans and speculative implementation specs.
Document limitations rather than presenting planned behavior as implemented.
Do not include credentials, environment values, or raw `PWDIRENV_CONTEXT`
in bug reports. See [SECURITY.md](SECURITY.md).
