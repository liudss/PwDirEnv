#requires -Version 7.2
$ErrorActionPreference = 'Stop'
$exe = 'C:\src\PwDirEnv\target\debug\pwdir.exe'
function Step($m) { Write-Output "[step] $m" }
$root = Join-Path $env:TEMP ("pwdir-dod13-" + [Guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Path $root | Out-Null
$loc = Join-Path $env:TEMP ("pwdir-dod13-loc-" + [Guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Path $loc | Out-Null
$env:LOCALAPPDATA = $loc

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
    Import-Module 'C:\src\PwDirEnv\powershell\PwDirEnv\PwDirEnv.psd1' -Force
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
    Write-Output 'DOD13 PATH-TORTURE OK'
}
finally {
    Remove-Item $root -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item $loc -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item Env:LOCALAPPDATA -ErrorAction SilentlyContinue
    $env:LOCALAPPDATA = [Environment]::GetEnvironmentVariable('LOCALAPPDATA','User')
}
