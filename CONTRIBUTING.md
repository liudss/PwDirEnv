# Contributing

Keep changes focused and describe observable behavior before and after the
change. Use English for documentation, comments, and diagnostics. Non-English
fixtures are retained to test Unicode handling.

## Development and testing

Use Windows, PowerShell 7.2+ (`pwsh` on PATH), and Rust stable with rustfmt
and Clippy. A minimum Rust version has not been validated across dependencies.

```powershell
cargo build --workspace --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --locked
pwsh -NoProfile -File tests/smoke.ps1
pwsh -NoProfile -File tests/e2e_trust.ps1
pwsh -NoProfile -File tests/e2e_hook.ps1
pwsh -NoProfile -File tests/e2e_path_torture.ps1
```

Run PowerShell integration scripts in separate processes: they change process
environment variables and use temporary trust stores. Rust integration tests
require a real `pwsh`. The standalone E2E scripts use the debug executable,
so build it first. Install Pester 5.9.1, then run:

```powershell
Install-Module Pester -RequiredVersion 5.9.1 -Scope CurrentUser -Repository PSGallery -Force
pwsh -NoProfile -File tests/run-pester.ps1
```

The runner rejects empty, skipped, and failed runs. Use a fresh process to
avoid conflicts with an installed PwDirEnv module.

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

## GitHub workflow

Open a GitHub issue for bugs, questions, or feature proposals. For larger changes,
discuss the use case before implementing. Create a branch and submit a pull
request against `main`, describing the resulting behavior and validation.
Use synthetic RC fixtures; do not include actual project secrets.

Required review evidence is a green Windows test job, dependency audit, and
full-history secret scan. CI also runs weekly and can be started manually.
Dependabot proposes Cargo and GitHub Actions updates; review and test those
updates like other changes. Actions are pinned to commits for reviewability.

For local dependency checks, install cargo-deny and run
`cargo deny --locked check advisories licenses bans sources`. The allowlist
in `deny.toml` is a dependency policy, not a substitute for reviewing license
obligations when distributing binaries. Do not suppress a real vulnerability
or secret finding merely to make CI pass. Rotate exposed credentials before
coordinating history cleanup; any justified exception needs an explanation.

Maintainers follow [the release process](docs/releasing.md). Security reports
use [GitHub vulnerability reporting](SECURITY.md).
