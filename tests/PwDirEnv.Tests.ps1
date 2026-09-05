#requires -Version 7.2
# PwDirEnv PowerShell module 单元测试（Pester 5 语法，SPEC §24.2）。
#
# 兼容性守卫：本机 Pester < 5（如 Windows 内置 3.4.0）时直接跳过，不会跑挂 ——
# tests/e2e_hook.ps1 才是必跑的真实验收（SPEC §24.6 release blocker）。
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

Describe 'PwDirEnv module (M4)' {
    BeforeAll {
        $script:RepoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).ProviderPath
        $script:PwDirExePath = Join-Path $script:RepoRoot 'target\debug\pwdir.exe'
        Import-Module (Join-Path $script:RepoRoot 'powershell\PwDirEnv\PwDirEnv.psd1') -Force
        # prompt 定义在 global 作用域（与 $PROFILE 场景一致），模块才能保存/恢复它
        function global:prompt { 'pester-prompt> ' }
    }

    AfterAll {
        Disable-PwDirEnv -ErrorAction Ignore
        Remove-Item -LiteralPath 'function:global:prompt' -Force -ErrorAction Ignore
    }

    AfterEach {
        # 清理：恢复环境状态与测试用环境变量
        InModuleScope PwDirEnv { Restore-PwDirEnvironment }
        Remove-Item -Path 'Env:PWDIR_PESTER_*' -ErrorAction Ignore
    }

    Context 'Enable / Disable 生命周期' {
        It 'Enable 保留用户原 prompt 且幂等' {
            Enable-PwDirEnv -PwDirPath $script:PwDirExePath
            (prompt) | Should -Be 'pester-prompt> '
            Enable-PwDirEnv   # 幂等：第二次调用不抛错、状态不变
            (Get-PwDirEnvStatus).Enabled | Should -BeTrue
            (prompt) | Should -Be 'pester-prompt> '
        }

        It 'Disable 恢复原 prompt、清空环境状态且幂等' {
            Enable-PwDirEnv -PwDirPath $script:PwDirExePath
            Disable-PwDirEnv
            (prompt) | Should -Be 'pester-prompt> '
            $status = Get-PwDirEnvStatus
            $status.Enabled | Should -BeFalse
            $status.ActiveRc | Should -BeNullOrEmpty
            $status.BaselineCount | Should -Be 0
            Disable-PwDirEnv   # 幂等
        }
    }

    Context 'reentrancy guard（SPEC §13.3）' {
        It 'HookRunning=true 时 hook 立即返回，不进入 cwd 检查' {
            Enable-PwDirEnv -PwDirPath $script:PwDirExePath
            Mock Get-PwDirCurrentFileSystemPath { return $null } -ModuleName PwDirEnv -Verifiable
            InModuleScope PwDirEnv {
                $script:PwDirState.HookRunning = $true
                Invoke-PwDirEnvHook   # guard 生效：立即 return，mock 不应被调用
                $script:PwDirState.HookRunning = $false
            }
            Should -Invoke Get-PwDirCurrentFileSystemPath -ModuleName PwDirEnv -Exactly -Times 0
        }

        It 'guard 复位后 hook 正常执行 cwd 检查' {
            Enable-PwDirEnv -PwDirPath $script:PwDirExePath
            Mock Get-PwDirCurrentFileSystemPath { return $null } -ModuleName PwDirEnv -Verifiable
            Invoke-PwDirEnvHook
            Should -Invoke Get-PwDirCurrentFileSystemPath -ModuleName PwDirEnv -Exactly -Times 1
        }
    }

    Context 'Get-PwDirCurrentFileSystemPath' {
        It 'FileSystem provider 下返回 ProviderPath' {
            Push-Location $TestDrive
            try {
                InModuleScope PwDirEnv {
                    Get-PwDirCurrentFileSystemPath | Should -Be (Get-Location).ProviderPath
                }
            }
            finally { Pop-Location }
        }

        It '非 FileSystem provider（注册表）返回 $null' {
            Push-Location HKLM:\
            try {
                InModuleScope PwDirEnv { Get-PwDirCurrentFileSystemPath } | Should -BeNullOrEmpty
            }
            finally { Pop-Location }
        }
    }

    Context 'Apply / Restore baseline（SPEC §8.4）' {
        It 'Apply-PwDirPatch 记录 baseline 并应用 set/unset' {
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
            $env:PWDIR_PESTER_EXISTING | Should -BeNullOrEmpty   # unset 生效
            $env:PWDIR_PESTER_NEW | Should -Be 'added'
            InModuleScope PwDirEnv { Restore-PwDirEnvironment }
            $env:PWDIR_PESTER_EXISTING | Should -Be 'orig'       # baseline 恢复原值
            $env:PWDIR_PESTER_NEW | Should -BeNullOrEmpty        # 新增 key 恢复为不存在
        }

        It 'restore 对原本不存在的 key 恢复为不存在（Existed/Value 语义）' {
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

        It '同 key 大小写不同的引用只记录一份 baseline（Windows 大小写不敏感）' {
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

    Context '错误语义（SPEC §13.4）' {
        It 'pwdir.exe 缺失时 hook 不抛错、prompt 仍可用' {
            Enable-PwDirEnv -PwDirPath $script:PwDirExePath
            InModuleScope PwDirEnv {
                $script:PwDirState.PwDirExe = 'Z:\__definitely_missing__\pwdir.exe'
                { Invoke-PwDirEnvHook } | Should -Not -Throw
            }
            (prompt) | Should -Be 'pester-prompt> '   # 错误不破坏 prompt
        }
    }

    Context 'Get-PwDirEnvStatus 契约' {
        It '返回 M4 状态字段' {
            $status = Get-PwDirEnvStatus
            $status.Version | Should -Not -BeNullOrEmpty
            foreach ($field in 'Enabled', 'PwDirExe', 'ActiveRc', 'ManagedKeys', 'HookRunning', 'BaselineCount', 'Fingerprint') {
                $status.PSObject.Properties.Name | Should -Contain $field
            }
            $status.Fingerprint | Should -BeNullOrEmpty   # M4 恒为 $null（M6 启用）
        }
    }
}
