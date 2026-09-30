# Writing configuration

The nearest `.envrc.ps1` in the current directory or its parents is selected.
Parents are not implicitly layered. Helpers resolve relative paths from the
selected RC's directory, even if the script changes location.

```powershell
$env:APP_ENV = 'development'
Remove-Item Env:LEGACY_OPTION -ErrorAction Ignore
PATH_add '.\tools\bin'
dotenv_if_exists '.env.local'
watch_file '.env.local'
```

Read the RC and its dependencies before `pwdir allow`. Functions, aliases,
imports, and ordinary variables stay in the evaluator. Only environment
differences are applied to the host.

## Helpers

| Helper | Behavior |
| --- | --- |
| `PATH_add PATH` | Resolve and prepend a directory, removing equivalent entries using case-insensitive Windows path comparison. The path need not exist. |
| `dotenv [PATH] [-Overload]` | Read `.env` by default; fail if missing. Existing variables win unless `-Overload` is supplied. |
| `dotenv_if_exists [PATH] [-Overload]` | Same parser; a missing file is ignored. |
| `watch_file PATH...` | Register size/mtime dependencies, including files that do not exist yet. |
| `watch_dir PATH...` | Watch directory metadata, not recursive content. |
| `source_env PATH` | Dot-source a script in the evaluator and record its dependency; fail if missing. |
| `source_up [FILENAME]` | Source the nearest matching file above the current RC or sourced script; nested calls continue upward; default `.envrc.ps1`; no match is a no-op. |

Dotenv helpers do **not** register watches automatically: use `watch_file`.
Direct PowerShell reads are also untracked. Source helpers do not perform
separate trust checks; see [security](../SECURITY.md).

## Dotenv syntax

The PowerShell parser accepts UTF-8, optional BOM, and common line endings.
Blank/comment lines, invalid keys, and lines without `=` are ignored. Keys
match `[A-Za-z_][A-Za-z0-9_]*`. A lowercase `export ` prefix is accepted.

Unquoted values are trimmed; inline `#` remains literal. Single-quoted values
are literal. Double-quoted values decode `\"`, `\\`, `\n`, `\r`, and `\t`;
unknown escapes remain literal. Unclosed quotes are treated as literal text.
There is no command substitution, variable expansion, or multiline syntax.
Later duplicate keys replace earlier values in the parsed file.

## Reload and restoration

Checks run at prompts, not in a background watcher. `pwdir reload` invalidates
the cache; evaluation happens at the next prompt.

Same-RC reevaluation currently inherits applied values. Removing an assignment
does not remove its previous value; default dotenv loading also preserves
existing values. Leave and reenter, or disable and reenable, for a clean
evaluation from the original baseline.

On leaving, managed keys return to their saved values; originally absent
keys are removed. Manual changes to managed keys are discarded on restoration.
Unrelated keys are retained.
