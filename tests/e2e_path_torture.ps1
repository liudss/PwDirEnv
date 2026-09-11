#requires -Version 7.2
$ErrorActionPreference = 'Stop'
$repoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).ProviderPath
$exe = Join-Path $repoRoot 'target\debug\pwdir.exe'
if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) {
    throw "pwdir.exe not found at $exe; run cargo build --workspace first"
}
function Step($m) { Write-Output "[step] $m" }
$tempBase = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
$root = Join-Path $tempBase ("pwdir-path-test-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $root | Out-Null
$loc = Join-Path $tempBase ("pwdir-path-data-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $loc | Out-Null
$savedLocalAppData = $env:LOCALAPPDATA
$env:LOCALAPPDATA = $loc
Push-Location

try {
    $proj = Join-Path $root "repo with spaces' and uni-用户 [1] `$x"
    Step "create project: $proj"
    New-Item -ItemType Directory -Path $proj | Out-Null
    $rc = Join-Path $proj '.envrc.ps1'
    Set-Content -LiteralPath $rc -Value '$env:PWDIR_DOD13 = ''ok-with-tricky-path''' -Encoding utf8

    Step 'allow'
    & $exe allow $rc | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "allow failed: $LASTEXITCODE" }

    Step 'set-location proj'
    Set-Location -LiteralPath $proj
    Step 'export'
    $json = & $exe export --format json | ConvertFrom-Json
    Step ("export action=" + $json.action)
    if ($json.action -ne 'apply' -or $json.patch.set.PWDIR_DOD13 -ne 'ok-with-tricky-path') {
        throw "export did not apply tricky-path rc: action=$($json.action)"
    }

    Step 'import module'
    Import-Module (Join-Path $repoRoot 'powershell\PwDirEnv\PwDirEnv.psd1') -Force
    Step 'enable'
    Enable-PwDirEnv -PwDirPath $exe | Out-Null
    Step 'hook'
    Invoke-PwDirEnvHook
    Step ("env=" + $env:PWDIR_DOD13)
    if ($env:PWDIR_DOD13 -ne 'ok-with-tricky-path') { throw "hook did not load tricky-path rc" }

    $outside = Join-Path $root 'outside'
    New-Item -ItemType Directory -Path $outside | Out-Null
    Step 'cd outside'
    Set-Location -LiteralPath $outside
    Invoke-PwDirEnvHook
    if ($null -ne $env:PWDIR_DOD13) { throw "outside did not restore/remove var" }

    Disable-PwDirEnv | Out-Null
    Write-Output 'E2E PATH-TORTURE OK'
}
finally {
    if (Get-Module PwDirEnv) { Disable-PwDirEnv | Out-Null }
    Pop-Location
    $env:LOCALAPPDATA = $savedLocalAppData
    foreach ($directory in @($root, $loc)) {
        $absolute = [IO.Path]::GetFullPath($directory)
        $allowedPrefix = $tempBase.TrimEnd('\') + '\'
        if (-not $absolute.StartsWith($allowedPrefix, [StringComparison]::OrdinalIgnoreCase)) {
            throw "Refusing to clean a directory outside the temporary root: $absolute"
        }
        Remove-Item -LiteralPath $absolute -Recurse -Force -ErrorAction SilentlyContinue
    }
}
