# Troubleshooting

Start with `pwdir doctor` for installation issues, `pwdir status` for native
trust/cache, or `Get-PwDirEnvStatus` for the current module. Do not dump env.

| Symptom | Check or action |
| --- | --- |
| Blocked | Inspect the selected RC and source chain before approval. `reload` does not authorize. |
| Hook absent | Check executable resolution and enabled state. NoProfile skips startup configuration. Another prompt tool may overwrite the wrapper; enable after its initialization. |
| Old values after reload | Same-project evaluation inherits applied values. Leave/reenter for a clean baseline; dotenv also needs explicit watches and preserves existing values by default. |
| Values remain after failure/deny | Same-project errors retain last-known-good values. Disable or leave to restore. Deny does not immediately alter other running shells. |
| Import by name fails | Standard installation uses an absolute module path; the module directory may not be on PSModulePath. |
| Slow/quiet exec | Output is buffered until target exit. Avoid background children retaining evaluator output handles. |
| Unicode/path issue | Preserve literal paths; use -LiteralPath for PowerShell filesystem operations. Retain the actual failing path in synthetic tests. |

Cache contains metadata, but evaluator temporary results and Base64
`PWDIRENV_CONTEXT` contain values. Use sanitized errors and variable names
in reports. Do not reset all user trust data to fix a single blocked RC.
