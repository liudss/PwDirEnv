#requires -Version 7.2

#

if ($null -eq $script:__PwDirEnvState) {
    $script:__PwDirEnvState = @{
        ProjectRoot  = (Get-Location).ProviderPath
        Watches      = @()
        SourcedFiles = @()
    }
}

# ---------------------------------------------------------------------------

# ---------------------------------------------------------------------------

function ConvertTo-PwDirNormalizedPathString {
    <#
    .SYNOPSIS
        Normalize separators and trailing slashes without resolving filesystem objects; preserve drive roots.
    #>

    param([Parameter(Mandatory = $true)][AllowEmptyString()][string]$Path)

    $s = $Path.Replace('/', '\')

    if ($s -ne '' -and $s.TrimEnd('\') -eq '') { return $s }
    $trimmed = $s.TrimEnd('\')

    if ($trimmed -match '^[A-Za-z]:$') { return "$trimmed\" }
    return $trimmed
}

function Test-PwDirPathEquivalent {
    <#
    .SYNOPSIS
        Compare normalized Windows paths case-insensitively without filesystem access.
    #>

    param(
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$A,
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$B
    )
    return ((ConvertTo-PwDirNormalizedPathString -Path $A) -ieq (ConvertTo-PwDirNormalizedPathString -Path $B))
}

function ConvertTo-PwDirAbsolutePath {
    <#
    .SYNOPSIS
        Resolve relative paths against ProjectRoot, independent of later Set-Location calls.
    #>

    param([Parameter(Mandatory = $true)][string]$Path)

    $root = [string]$script:__PwDirEnvState.ProjectRoot
    if ([string]::IsNullOrWhiteSpace($root)) {
        throw 'PwDirEnv stdlib: ProjectRoot is not initialized'
    }
    $candidate = if ([IO.Path]::IsPathRooted($Path)) { $Path } else { Join-Path -Path $root -ChildPath $Path }

    return [IO.Path]::GetFullPath($candidate)
}

function Add-PwDirWatch {
    <#
    .SYNOPSIS
        Register an absolute dependency, deduplicated by kind and equivalent path.
    #>

    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][ValidateSet('file', 'dir')][string]$Kind
    )
    $absolute = ConvertTo-PwDirAbsolutePath -Path $Path
    foreach ($existing in @($script:__PwDirEnvState.Watches)) {
        if ($existing.kind -eq $Kind -and (Test-PwDirPathEquivalent -A ([string]$existing.path) -B $absolute)) {
            return
        }
    }
    $script:__PwDirEnvState.Watches += [ordered]@{ kind = $Kind; path = $absolute }
}

function Add-PwDirSourcedFile {
    <#
    .SYNOPSIS
        Record a sourced script for fingerprint validation, without granting separate authorization.
    #>

    param([Parameter(Mandatory = $true)][string]$Path)
    $absolute = ConvertTo-PwDirAbsolutePath -Path $Path
    foreach ($existing in @($script:__PwDirEnvState.SourcedFiles)) {
        if (Test-PwDirPathEquivalent -A ([string]$existing) -B $absolute) {
            return
        }
    }
    $script:__PwDirEnvState.SourcedFiles += $absolute
}

# ---------------------------------------------------------------------------

# ---------------------------------------------------------------------------

function ConvertFrom-PwDirDotEnvDoubleQuoted {
    <#
    .SYNOPSIS
        Decode the supported quote, slash, newline, carriage-return, and tab escapes; preserve unknown escapes.
    #>

    param([Parameter(Mandatory = $true)][AllowEmptyString()][string]$Text)

    $sb = [Text.StringBuilder]::new()
    $i = 0
    while ($i -lt $Text.Length) {
        $c = $Text[$i]
        if ($c -eq '\' -and ($i + 1) -lt $Text.Length) {
            $next = $Text[$i + 1]
            if ($next -eq '"') { $null = $sb.Append('"') }
            elseif ($next -eq '\') { $null = $sb.Append('\') }
            elseif ($next -eq 'n') { $null = $sb.Append([char]10) }
            elseif ($next -eq 'r') { $null = $sb.Append([char]13) }
            elseif ($next -eq 't') { $null = $sb.Append([char]9) }
            else {

                $null = $sb.Append('\')
                $null = $sb.Append($next)
            }
            $i += 2
        }
        else {
            $null = $sb.Append($c)
            $i += 1
        }
    }
    return $sb.ToString()
}

function ConvertFrom-PwDirDotEnvLine {
    <#
    .SYNOPSIS
        Parse a permissive dotenv line. No variable expansion or command substitution; malformed lines are skipped.
    #>

    param([Parameter(Mandatory = $true)][AllowEmptyString()][string]$Line)

    $line = $Line.Trim()
    if ($line -eq '' -or $line.StartsWith('#')) { return $null }

    $line = $line -creplace '^export\s+', ''

    $eq = $line.IndexOf('=')
    if ($eq -lt 1) { return $null }
    $key = $line.Substring(0, $eq).Trim()
    if ($key -notmatch '^[A-Za-z_][A-Za-z0-9_]*$') { return $null }
    $raw = $line.Substring($eq + 1).Trim()

    $value =
        if ($raw.StartsWith('"') -and $raw.Length -ge 2 -and $raw.EndsWith('"')) {
            ConvertFrom-PwDirDotEnvDoubleQuoted -Text $raw.Substring(1, $raw.Length - 2)
        }
        elseif ($raw.StartsWith("'") -and $raw.Length -ge 2 -and $raw.EndsWith("'")) {
            $raw.Substring(1, $raw.Length - 2)
        }
        else {
            $raw
        }

    return [pscustomobject]@{ Key = $key; Value = $value }
}

function Read-PwDirDotEnv {
    <#
    .SYNOPSIS
        Read UTF-8 dotenv entries; later duplicate keys win. BOM and common line endings are accepted.
    #>

    param([Parameter(Mandatory = $true)][string]$Path)

    $text = [IO.File]::ReadAllText($Path, [Text.UTF8Encoding]::new($false))
    $result = [ordered]@{}
    foreach ($line in ($text -split '\r\n|\n|\r')) {
        $parsed = ConvertFrom-PwDirDotEnvLine -Line $line
        if ($null -ne $parsed) {
            $result[$parsed.Key] = $parsed.Value
        }
    }
    return $result
}

function Set-PwDirDotEnvVars {
    <#
    .SYNOPSIS
        Preserve existing variables, including empty values, unless Overload is requested.
    #>

    param(
        [Parameter(Mandatory = $true)][string]$File,
        [switch]$Overload
    )
    $pairs = Read-PwDirDotEnv -Path $File
    foreach ($key in @($pairs.Keys)) {
        $value = [string]$pairs[$key]
        if (-not $Overload) {
            $existing = [Environment]::GetEnvironmentVariable($key, 'Process')
            if ($null -ne $existing) { continue }
        }
        Set-Item -LiteralPath "Env:$key" -Value $value
    }
}

# ---------------------------------------------------------------------------

# ---------------------------------------------------------------------------

function PATH_add {
    <#
    .SYNOPSIS
        Prepend a project-relative directory and remove equivalent PATH entries. Preserve unrelated empty segments.
    #>

    param([Parameter(Mandatory = $true, Position = 0)][string]$Path)

    $dir = ConvertTo-PwDirAbsolutePath -Path $Path
    $separator = [IO.Path]::PathSeparator
    $current = $env:PATH

    if ([string]::IsNullOrEmpty($current)) {
        $env:PATH = $dir
        return
    }

    $items = $current -split ([regex]::Escape($separator))
    $kept = @($items | Where-Object { -not (Test-PwDirPathEquivalent -A $_ -B $dir) })
    $env:PATH = (@($dir) + $kept) -join $separator
}

function dotenv {
    <#
    .SYNOPSIS
        Load a required dotenv file without automatically registering a watch.
    #>

    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $false, Position = 0)][string]$Path = '.env',
        [switch]$Overload
    )
    $file = ConvertTo-PwDirAbsolutePath -Path $Path
    if (-not (Test-Path -LiteralPath $file -PathType Leaf)) {
        throw "dotenv: file not found: $Path (resolved to $file)"
    }
    Set-PwDirDotEnvVars -File $file -Overload:$Overload
}

function dotenv_if_exists {
    <#
    .SYNOPSIS
        Load an optional dotenv file; missing files are ignored. Register watches explicitly.
    #>

    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $false, Position = 0)][string]$Path = '.env',
        [switch]$Overload
    )
    $file = ConvertTo-PwDirAbsolutePath -Path $Path
    if (-not (Test-Path -LiteralPath $file -PathType Leaf)) {
        return
    }
    Set-PwDirDotEnvVars -File $file -Overload:$Overload
}

function watch_file {
    <#
    .SYNOPSIS
        Track file size and modification time; targets may be created later.
    #>

    param([Parameter(Mandatory = $true, Position = 0, ValueFromRemainingArguments = $true)][string[]]$Path)
    foreach ($p in $Path) { Add-PwDirWatch -Path $p -Kind 'file' }
}

function watch_dir {
    <#
    .SYNOPSIS
        Track directory metadata, not recursive descendant contents.
    #>

    param([Parameter(Mandatory = $true, Position = 0, ValueFromRemainingArguments = $true)][string[]]$Path)
    foreach ($p in $Path) { Add-PwDirWatch -Path $p -Kind 'dir' }
}

function source_env {
    <#
    .SYNOPSIS
        Source a script in the evaluator and track its metadata. The top-level approval covers this code path; there is no separate trust check.
    #>

    param([Parameter(Mandatory = $true, Position = 0)][string]$Path)

    $absolute = ConvertTo-PwDirAbsolutePath -Path $Path
    if (-not (Test-Path -LiteralPath $absolute -PathType Leaf)) {
        throw "source_env: file not found: $Path (resolved to $absolute)"
    }

    $previousSource = $script:__PwDirEnvState.CurrentSourcePath
    $script:__PwDirEnvState.CurrentSourcePath = $absolute
    try {
        . (Get-Item -LiteralPath $absolute).FullName
    }
    finally {
        $script:__PwDirEnvState.CurrentSourcePath = $previousSource
    }
    Add-PwDirWatch -Path $absolute -Kind 'file'
    Add-PwDirSourcedFile -Path $absolute
}

function source_up {
    <#
    .SYNOPSIS
        Source the nearest matching file strictly above the current source directory. No match is a no-op.
    #>

    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $false, Position = 0)][string]$FileName = '.envrc.ps1'
    )
    $sourceDir = if ($script:__PwDirEnvState.CurrentSourcePath) {
        [IO.Path]::GetDirectoryName([string]$script:__PwDirEnvState.CurrentSourcePath)
    } else {
        [string]$script:__PwDirEnvState.ProjectRoot
    }
    $dir = [IO.Path]::GetDirectoryName($sourceDir)
    while (-not [string]::IsNullOrEmpty($dir)) {
        $candidate = Join-Path -Path $dir -ChildPath $FileName
        if (Test-Path -LiteralPath $candidate -PathType Leaf) {
            source_env -Path $candidate
            return
        }
        $dir = [IO.Path]::GetDirectoryName($dir)
    }

}
