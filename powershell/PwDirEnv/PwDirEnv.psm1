#Requires -Version 7.2
# PwDirEnv Host Shell module。
# 安全边界（SPEC §15）：只修改 Process scope 环境变量；禁止对 native 输出 Invoke-Expression。
Set-StrictMode -Version Latest

$script:ModuleVersion = '0.1.0'

# Host 会话状态（SPEC §8.4）。M0 仅初始化空结构；M4 填充 apply/restore 逻辑。
$script:PwDirState = @{
    ActiveRc    = $null   # 当前生效的 .envrc.ps1 绝对路径
    Baseline    = @{}     # patch 应用前的环境快照（managed keys 原值）
    Applied     = @{}     # 已应用的 managed keys（key -> 当前值）
    Fingerprint = $null   # 最近一次 evaluation 的 fingerprint
    HookRunning = $false  # reentrancy guard
}

function Enable-PwDirEnv {
    <#
    .SYNOPSIS
        在当前会话启用 PwDirEnv。

    .DESCRIPTION
        保存用户现有 prompt 函数并安装 wrapper，使每次 prompt 渲染前执行
        Invoke-PwDirEnvHook。幂等；Disable-PwDirEnv 可完整恢复原 prompt。
    #>
    [CmdletBinding()]
    param()

    # TODO(M4): 保存 function:prompt ScriptBlock 并安装 wrapper（保留用户原 prompt）；
    #           幂等处理重复 Enable。
    Write-Host 'PwDirEnv enabled (scaffold)'
}

function Disable-PwDirEnv {
    <#
    .SYNOPSIS
        在当前会话禁用 PwDirEnv。

    .DESCRIPTION
        恢复用户原 prompt 函数；若当前有已加载的项目环境，先按 baseline 恢复。
    #>
    [CmdletBinding()]
    param()

    # TODO(M4): 恢复原 prompt；先 Restore-PwDirEnvironment 再卸载 wrapper。
    Write-Host 'PwDirEnv disabled (scaffold)'
}

function Invoke-PwDirEnvHook {
    <#
    .SYNOPSIS
        Prompt hook：检查目录/依赖变化并按需加载、卸载项目环境。

    .DESCRIPTION
        错误不得破坏 prompt（SPEC §13.4）；必须带 reentrancy guard 防递归。
    #>
    [CmdletBinding()]
    param()

    # TODO(M4): reentrancy guard -> 非 FileSystem provider 时 unload 并跳过 ->
    #           discovery -> pwdir export --format json -> Apply-PwDirPatch（禁止 Invoke-Expression）。
    # TODO(M6): fingerprint 快速路径——PWD 与依赖未变化时不 spawn evaluator。
    return
}

function Get-PwDirEnvStatus {
    <#
    .SYNOPSIS
        返回当前会话的 PwDirEnv 状态概览。
    #>
    [CmdletBinding()]
    param()

    [PSCustomObject]@{
        Version = $script:ModuleVersion
        State   = [PSCustomObject]@{
            ActiveRc      = $script:PwDirState.ActiveRc
            ManagedKeys   = @($script:PwDirState.Applied.Keys).Count
            HasFingerprint = ($null -ne $script:PwDirState.Fingerprint)
            HookRunning   = $script:PwDirState.HookRunning
        }
    }
}

Export-ModuleMember -Function @(
    'Enable-PwDirEnv',
    'Disable-PwDirEnv',
    'Invoke-PwDirEnvHook',
    'Get-PwDirEnvStatus'
)
