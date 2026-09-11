# Known limitations and implementation audit

Reviewed against the source on 2026-09-11. These are current constraints,
not a development schedule.

| Area | Implemented behavior and limits |
| --- | --- |
| Platform | Windows/PowerShell 7.2+ declared; the entire version range has not been validated locally. Windows PowerShell 5.1 is unsupported. |
| Distribution | Source installer and release build. No published winget package; placeholder manifests removed. |
| Performance | Cache hits avoid evaluator startup, but a native process still runs at each prompt. Old latency targets were not measured. |
| Dotenv | PowerShell parser; no automatic watch registration. Existing values win by default. |
| Reload | Same-RC evaluation inherits applied values. Deleted assignments can remain until unload. Leave/reenter for a clean evaluation. |
| Dependencies | Size/mtime can miss edits preserving both. Directory watches are not recursive. Evaluation does not snapshot dependencies. |
| Authorization | Top-level RC only; sourced scripts are not separately authorized. Pre/post checks do not eliminate all replacement races. |
| Path identity | Lexical normalization; no Windows file-ID hardening or general path-alias unification. |
| Nested shells | `PWDIRENV_CONTEXT` includes restoration values, potentially secrets, and increases environment size. |
| Empty values | Protocol distinguishes empty/absent; runtime behavior across older supported PowerShell/.NET versions needs validation. |
| Exec | Direct argument passing; stdout/stderr buffered until exit. Interactive terminal applications are not validated. |
| Timeouts | Cleanup takes extra time; normal-exit draining can wait on descendants retaining output handles. |
| Protocol | Host checks export schema; evaluator-result parser currently routes on `ok` without rejecting unknown envelope schemas. |

## Documentation migration

The initial specification and milestone plan mixed requirements, agent-dispatch
instructions, unimplemented ideas, and completed work. They were removed from
the current tree; Git history retains them. Current behavior is in
[architecture](architecture.md), [configuration](configuration.md), and
[CLI reference](cli.md). Decisions are in [ADRs](adr/README.md).

Removed claims include a Rust dotenv parser, ready-to-use winget distribution,
measured prompt-latency guarantees, file-ID hardening, and broad compatibility
validation. The empty Rust dotenv module was removed as misleading scaffolding.

## Private local trial

Local validation for this migration used PowerShell 7.6.5 and Rust 1.98.1 on
Windows: 139 Rust tests, Clippy with warnings denied, formatting checks, all
four standalone PowerShell smoke/E2E scripts, and skill validation passed.
The local Pester is 3.4.0, so the optional Pester 5 suite was not run.
These results do not establish compatibility with every supported version.

Check A-to-B-to-outside transitions, PATH restoration, nested shells, edits and
denials, source dependencies, reload failures, and existing prompt tools over
several days. Record names and sanitized errors rather than environment values.
Run `pwdir doctor` for installation reports. Trial results should inform any
later decision to make the repository public.
