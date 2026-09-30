# GitHub release process

PwDirEnv is an early preview. GitHub Issues and Pull Requests are the public
feedback and contribution channels. Releases currently distribute source;
users build with Rust and PowerShell using `packaging/install.ps1`.

## Before making the repository public

- Confirm Windows tests, Pester, installation verification, dependency checks,
  and the full-history secret scan pass for the exact revision to publish.
- Review Git history for personal or proprietary material that automated
  secret detection does not recognize. Confirm rights to publish the code.
- Change repository visibility only as a deliberate maintainer action. Then
  enable GitHub private vulnerability reporting under Settings > Security;
  verify the Report a vulnerability link in SECURITY.md works.
- Enable GitHub's available dependency alerts and secret protection features.
  Availability depends on repository visibility and the account plan.
- Where the plan permits, protect `main` with a ruleset requiring pull requests
  and the Windows tests, Dependency audit, and Full-history secret scan checks.
  A solo maintainer can review their own change without requiring another approver.

Workflow files do not configure repository visibility, reporting settings, or
branch protection. Verify those settings in GitHub; do not assume they exist
because this document describes them.

## Prepare a version

1. Open a pull request with the version and release notes. Keep Cargo.toml,
   Cargo.lock, the module manifest, and runtime version strings consistent.
2. Review known limitations and compatibility claims. Include user-visible
   fixes, changes, and upgrade notes; do not describe untested support as verified.
3. Wait for all checks on the final revision to pass and merge the pull request.
4. Create and push a tag matching the package version, for example `v0.1.0`.
   Do not move a published tag to different contents.

## Draft and publish

The Draft preview release workflow runs on `v*` tags. It reruns the complete
CI workflow for the tag, verifies Cargo and PowerShell manifest versions, and
creates a **draft prerelease** with generated notes. It does not publish it.
GitHub provides source archives when the maintainer publishes the release.
No unsigned executable archive is automatically uploaded by this workflow.

Review the draft in GitHub Releases, replace generated notes with clear upgrade
information where needed, and publish it as a prerelease. Before adding binary
assets in a future release, include third-party license notices, checksums,
and installation verification for each advertised architecture.

If checks fail, fix the code through a pull request before tagging a new
candidate. If only draft creation fails after checks pass, inspect existing
drafts and rerun failed jobs as appropriate. Never bypass the checks to publish.
