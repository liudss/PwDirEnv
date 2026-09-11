# RC authoring

The nearest `.envrc.ps1` wins; parents do not implicitly stack. Helpers resolve
paths relative to that RC's directory even if the script changes location.
Functions, aliases, and module imports do not transfer to the host.

```powershell
$env:APP_ENV = 'development'
PATH_add '.\tools\bin'
dotenv_if_exists '.env.local'
watch_file '.env.local'
```

| Helper | Decision-relevant behavior |
| --- | --- |
| `PATH_add PATH` | Prepend and deduplicate equivalent Windows paths; nonexistent paths allowed. |
| `dotenv [PATH] [-Overload]` | Default `.env`; fail if absent; preserve existing variables unless Overload. |
| `dotenv_if_exists [PATH] [-Overload]` | Same parser, skip absence. |
| `watch_file PATH...` | Track size/mtime, including not-yet-created files. |
| `watch_dir PATH...` | Directory metadata only, not recursive content. |
| `source_env PATH` | Source and track a script; fail if absent. No separate approval check. |
| `source_up [FILENAME]` | Nearest matching ancestor above project root; default `.envrc.ps1`; no match is a no-op. |

Dotenv does not watch automatically. Explicit PowerShell reads need watches too.
Dotenv accepts UTF-8/BOM, optional lowercase `export`, literal single quotes,
and basic double-quote escapes (quote, slash, newline, CR, tab). No expansion,
substitution, or multiline values. Invalid lines are skipped; unclosed quotes
are literal. Inline `#` is part of unquoted values.

Same-project reload inherits applied values; removed assignments can persist.
Leave/reenter or disable/reenable for a clean baseline. Export evaluation
defaults to 10 seconds; avoid long-running or background initialization.
