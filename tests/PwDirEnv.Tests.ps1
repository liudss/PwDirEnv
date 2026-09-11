#requires -Version 7.2

# Optional Pester 5 coverage; standalone E2E scripts do not require Pester.

$pesterModule = Get-Module -ListAvailable Pester |
    Sort-Object -Property Version -Descending |
    Select-Object -First 1
if ($null -eq $pesterModule -or $pesterModule.Version.Major -lt 5) {
    $found = if ($null -ne $pesterModule) { $pesterModule.Version.ToString() } else { 'none' }
    Write-Output "PwDirEnv Pester tests skipped: Pester 5 required, found '$found'. Run tests/e2e_hook.ps1 for the mandatory E2E instead."
    return
}

$pwdirExeForGuard = Join-Path (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).ProviderPath 'target\debug\pwdir.exe'
if (-not (Test-Path -LiteralPath $pwdirExeForGuard)) {
    Write-Output "PwDirEnv Pester tests skipped: pwdir.exe not found at $pwdirExeForGuard. Run 'cargo build --workspace' first."
    return
}

Describe 'PwDirEnv module' {
    BeforeAll {
        $script:RepoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).ProviderPath
        $script:PwDirExePath = Join-Path $script:RepoRoot 'target\debug\pwdir.exe'
        Import-Module (Join-Path $script:RepoRoot 'powershell\PwDirEnv\PwDirEnv.psd1') -Force

        function global:prompt { 'pester-prompt> ' }
    }

    AfterAll {
        Disable-PwDirEnv -ErrorAction Ignore
        Remove-Item -LiteralPath 'function:global:prompt' -Force -ErrorAction Ignore
    }

    AfterEach {

        InModuleScope PwDirEnv { Restore-PwDirEnvironment }
        Remove-Item -Path 'Env:PWDIR_PESTER_*' -ErrorAction Ignore
    }

    Context 'Enable / Disable lifecycle' {
        It 'Enable preserves the original prompt and is idempotent' {
            Enable-PwDirEnv -PwDirPath $script:PwDirExePath
            (prompt) | Should -Be 'pester-prompt> '
            Enable-PwDirEnv
            (Get-PwDirEnvStatus).Enabled | Should -BeTrue
            (prompt) | Should -Be 'pester-prompt> '
        }

        It 'Disable restores the prompt, clears state, and is idempotent' {
            Enable-PwDirEnv -PwDirPath $script:PwDirExePath
            Disable-PwDirEnv
            (prompt) | Should -Be 'pester-prompt> '
            $status = Get-PwDirEnvStatus
            $status.Enabled | Should -BeFalse
            $status.ActiveRc | Should -BeNullOrEmpty
            $status.BaselineCount | Should -Be 0
            Disable-PwDirEnv
        }
    }

    Context 'reentrancy guard' {
        It 'HookRunning causes an immediate return before checking cwd' {
            Enable-PwDirEnv -PwDirPath $script:PwDirExePath
            Mock Get-PwDirCurrentFileSystemPath { return $null } -ModuleName PwDirEnv -Verifiable
            InModuleScope PwDirEnv {
                $script:PwDirState.HookRunning = $true
                Invoke-PwDirEnvHook
                $script:PwDirState.HookRunning = $false
            }
            Should -Invoke Get-PwDirCurrentFileSystemPath -ModuleName PwDirEnv -Exactly -Times 0
        }

        It 'a cleared guard permits cwd checks' {
            Enable-PwDirEnv -PwDirPath $script:PwDirExePath
            Mock Get-PwDirCurrentFileSystemPath { return $null } -ModuleName PwDirEnv -Verifiable
            Invoke-PwDirEnvHook
            Should -Invoke Get-PwDirCurrentFileSystemPath -ModuleName PwDirEnv -Exactly -Times 1
        }
    }

    Context 'Get-PwDirCurrentFileSystemPath' {
        It 'returns ProviderPath for the FileSystem provider' {
            Push-Location $TestDrive
            try {
                InModuleScope PwDirEnv {
                    Get-PwDirCurrentFileSystemPath | Should -Be (Get-Location).ProviderPath
                }
            }
            finally { Pop-Location }
        }

        It 'returns null for the registry provider' {
            Push-Location HKLM:\
            try {
                InModuleScope PwDirEnv { Get-PwDirCurrentFileSystemPath } | Should -BeNullOrEmpty
            }
            finally { Pop-Location }
        }
    }

    Context 'Apply / Restore baseline' {
        It 'captures baseline and applies set and unset' {
            $env:PWDIR_PESTER_EXISTING = 'orig'
            InModuleScope PwDirEnv {
                $result = [pscustomobject]@{
                    rc    = 'C:\fake\.envrc.ps1'
                    patch = [pscustomobject]@{
                        set   = [pscustomobject]@{ PWDIR_PESTER_EXISTING = 'new'; PWDIR_PESTER_NEW = 'added' }
                        unset = @('PWDIR_PESTER_EXISTING')
                    }
                }
                Apply-PwDirPatch $result
                (Get-PwDirEnvStatus).ActiveRc | Should -Be 'C:\fake\.envrc.ps1'
                (Get-PwDirEnvStatus).BaselineCount | Should -Be 2
            }
            $env:PWDIR_PESTER_EXISTING | Should -BeNullOrEmpty
            $env:PWDIR_PESTER_NEW | Should -Be 'added'
            InModuleScope PwDirEnv { Restore-PwDirEnvironment }
            $env:PWDIR_PESTER_EXISTING | Should -Be 'orig'
            $env:PWDIR_PESTER_NEW | Should -BeNullOrEmpty
        }

        It 'restore removes keys that were originally absent' {
            InModuleScope PwDirEnv {
                $result = [pscustomobject]@{
                    rc    = 'C:\fake\.envrc.ps1'
                    patch = [pscustomobject]@{
                        set   = [pscustomobject]@{ PWDIR_PESTER_FRESH = 'x' }
                        unset = @()
                    }
                }
                Apply-PwDirPatch $result
                $entry = $script:PwDirState.Baseline['PWDIR_PESTER_FRESH']
                $entry.Existed | Should -BeFalse
                $entry.Value | Should -BeNullOrEmpty
            }
            $env:PWDIR_PESTER_FRESH | Should -Be 'x'
            InModuleScope PwDirEnv { Restore-PwDirEnvironment }
            $env:PWDIR_PESTER_FRESH | Should -BeNullOrEmpty
        }

        It 'different casing records a single baseline entry' {
            InModuleScope PwDirEnv {
                $first = [pscustomobject]@{
                    rc    = 'C:\fake\.envrc.ps1'
                    patch = [pscustomobject]@{
                        set   = [pscustomobject]@{ pwdir_pester_case = 'x' }
                        unset = @()
                    }
                }
                Apply-PwDirPatch $first
                $second = [pscustomobject]@{
                    rc    = $first.rc
                    patch = [pscustomobject]@{
                        set   = [pscustomobject]@{ PWDIR_PESTER_CASE = 'y' }
                        unset = @()
                    }
                }
                Apply-PwDirPatch $second
                (Get-PwDirEnvStatus).BaselineCount | Should -Be 1
            }
            $env:PWDIR_PESTER_CASE | Should -Be 'y'
        }
    }

    Context 'error handling' {
        It 'a missing executable does not break the prompt' {
            Enable-PwDirEnv -PwDirPath $script:PwDirExePath
            InModuleScope PwDirEnv {
                $script:PwDirState.PwDirExe = 'Z:\__definitely_missing__\pwdir.exe'
                { Invoke-PwDirEnvHook } | Should -Not -Throw
            }
            (prompt) | Should -Be 'pester-prompt> '
        }
    }

    Context 'Get-PwDirEnvStatus contract' {
        It 'returns session state fields' {
            $status = Get-PwDirEnvStatus
            $status.Version | Should -Not -BeNullOrEmpty
            foreach ($field in 'Enabled', 'PwDirExe', 'ActiveRc', 'ManagedKeys', 'HookRunning', 'BaselineCount', 'Fingerprint') {
                $status.PSObject.Properties.Name | Should -Contain $field
            }
            $status.Fingerprint | Should -BeNullOrEmpty
        }
    }
}
