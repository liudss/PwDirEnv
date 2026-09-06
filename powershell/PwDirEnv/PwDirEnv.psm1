#Requires -Version 7.2
# PwDirEnv Host Shell module（M4，SPEC §15）。
# 安全边界（SPEC §15.1/§27）：
#   1. 只修改 Process scope 环境变量，绝不触碰 User/Machine（ADR-006）；
#   2. 禁止对 native 输出 Invoke-Expression —— pwdir.exe 的输出是「数据」（env patch）而非代码，
#      必须解析 JSON 后逐 key 用 [Environment]::SetEnvironmentVariable 应用。
Set-StrictMode -Version Latest

$script:ModuleVersion = '0.1.0'
$script:ProtocolSchema = 1

# 模块卸载（含 Import-Module -Force 重载）时尽力恢复现场：wrapper 是模块创建的 global 函数，
# 会被模块清理机制一并移除 —— 必须赶在卸载前按 baseline 恢复环境、还原用户原 prompt
# （恢复正确性优先；否则 -Force 重载后用户 prompt 与 managed 环境都会泄漏）。
$MyInvocation.MyCommand.ScriptBlock.Module.OnRemove = {
    if ($script:PwDirState.Enabled) {
        Restore-PwDirEnvironment
        if ($null -ne $script:PwDirState.OriginalPrompt) {
            Set-Item -LiteralPath 'function:global:prompt' -Value $script:PwDirState.OriginalPrompt
        }
        elseif (Get-Item -LiteralPath 'function:global:prompt' -ErrorAction Ignore) {
            Remove-Item -LiteralPath 'function:global:prompt' -Force -ErrorAction Ignore
        }
        $script:PwDirState.Enabled = $false
        $script:PwDirState.OriginalPrompt = $null
    }
}

# Host 会话状态（SPEC §8.4 恢复模型）。
$script:PwDirState = @{
    ActiveRc       = $null    # 当前生效的 .envrc.ps1 绝对路径（或 $null）
    Baseline       = @{}      # managed key -> [pscustomobject]@{ Existed=[bool]; Value=[string] }
                              #   应用 patch 前的原值；用 Existed/Value 而非哨兵值，避免与真实值碰撞
    Applied        = @{}      # managed key -> 当前已应用的值（或 $script:AbsentMarker）
    Fingerprint    = $null    # M6 fingerprint 快速路径用；M4 恒为 $null
    HookRunning    = $false   # reentrancy guard（SPEC §13.3）
    Enabled        = $false
    OriginalPrompt = $null    # Enable 前用户 prompt 的 ScriptBlock
    PwDirExe       = $null    # pwdir.exe 绝对路径
    LastStderr     = ''       # 最近 native stderr（诊断用，不参与协议）
}

# Applied 中表示「该 key 当前不存在」的标记（随机 GUID，避免与任何真实值碰撞）。
$script:AbsentMarker = [Guid]::NewGuid().ToString()

# 安装的 wrapper prompt 正文包含此标记，用于识别「现有 prompt 是否已是本模块 wrapper」
# （典型场景：模块被 Import-Module -Force 重载导致 script 状态丢失后再次 Enable）。
$script:PromptMarker = '__PwDirEnv_prompt_wrapper__'

# ---------------------------------------------------------------------------
# 内部函数（不导出）
# ---------------------------------------------------------------------------

function Resolve-PwDirExe {
    <# 解析 pwdir.exe 路径。优先级：-PwDirPath > $env:PWDIRENV_EXE > PATH 上的 pwdir.exe > 开发目录探测。 #>
    param([string]$PwDirPath)

    $candidates = @()
    if (-not [string]::IsNullOrWhiteSpace($PwDirPath)) {
        # 显式指定时只用它：找不到就抛错，不静默回退
        $candidates += $PwDirPath
    }
    elseif (-not [string]::IsNullOrWhiteSpace($env:PWDIRENV_EXE)) {
        $candidates += $env:PWDIRENV_EXE
    }
    else {
        $cmd = Get-Command -Name 'pwdir.exe' -CommandType Application -ErrorAction Ignore
        if ($cmd) { $candidates += @($cmd)[0].Source }
        # 开发仓库常见产物路径（模块位于 <repo>\powershell\PwDirEnv\）
        $candidates += (Join-Path $PSScriptRoot '..\..\target\debug\pwdir.exe')
        $candidates += (Join-Path $PSScriptRoot '..\..\target\release\pwdir.exe')
    }

    foreach ($candidate in $candidates) {
        if (-not [string]::IsNullOrWhiteSpace($candidate) -and (Test-Path -LiteralPath $candidate -PathType Leaf)) {
            return (Resolve-Path -LiteralPath $candidate).ProviderPath
        }
    }
    throw "PwDirEnv: pwdir.exe not found. Install or set -PwDirPath / `$env:PWDIRENV_EXE, or run 'cargo build --workspace'."
}

function Get-PwDirCurrentFileSystemPath {
    <# 返回当前 FileSystem provider 的 cwd 绝对路径；不可用时返回 $null（SPEC §5.4/§20.4）。 #>
    try {
        $loc = Get-Location
    }
    catch {
        # location 状态异常（如 PSDrive 被移除）：按 cwd 不可用处理，由 hook 卸载已加载环境
        return $null
    }
    if ($loc.Provider.Name -ne 'FileSystem') { return $null }
    $path = $loc.ProviderPath
    if (-not [IO.Directory]::Exists($path)) { return $null }   # cwd 被删除（SPEC §20.4）
    return $path
}

function Invoke-PwDirNative {
    <#
        执行 `pwdir export --format json`，返回解析后的 ExportOutput 对象。
        -Force 透传 `--force`：跳过 core 的 fingerprint 快速路径强制评估 ——
        rc 与 ActiveRc 不一致（新 shell 首次进入 / 切换目录）时必须传，
        否则 core 会命中其他会话留下的有效 cache 返回空 patch 而漏掉加载（M6）。
        stdout 是 pretty-print 多行 JSON，整体拼接后一次性 ConvertFrom-Json（SPEC §15.1：parse，绝不执行）；
        stderr 仅记录到 LastStderr 用于诊断。基础设施故障（exe 缺失 / 无输出）抛异常，由 hook 的 catch 兜底。
    #>
    param([switch]$Force)

    $exe = $script:PwDirState.PwDirExe
    if ([string]::IsNullOrEmpty($exe) -or -not (Test-Path -LiteralPath $exe -PathType Leaf)) {
        throw "pwdir.exe is missing at '$exe'. Install or set -PwDirPath / `$env:PWDIRENV_EXE."
    }

    # 防御性关闭 PSNativeCommandUseErrorActionPreference（7.3+）：stderr 经 2>&1 重定向后以
    # ErrorRecord 进入输出流，若用户会话把它当终止错误会误伤 hook（SPEC §13.4）
    $savedEap = $ErrorActionPreference
    $nativeVar = Get-Variable -Name 'PSNativeCommandUseErrorActionPreference' -ErrorAction Ignore
    $savedNative = if ($nativeVar) { $nativeVar.Value } else { $null }
    $ErrorActionPreference = 'Continue'
    if ($nativeVar) { $PSNativeCommandUseErrorActionPreference = $false }
    try {
        $nativeArgs = @('export', '--format', 'json')
        if ($Force) { $nativeArgs += '--force' }
        $output = & $exe @nativeArgs 2>&1
    }
    finally {
        $ErrorActionPreference = $savedEap
        if ($nativeVar) { $PSNativeCommandUseErrorActionPreference = $savedNative }
    }

    $stdout = [System.Collections.Generic.List[string]]::new()
    $stderr = [System.Collections.Generic.List[string]]::new()
    foreach ($item in @($output)) {
        if ($item -is [System.Management.Automation.ErrorRecord]) {
            $stderr.Add([string]$item)
        }
        else {
            $stdout.Add([string]$item)
        }
    }

    # 保留最近 stderr（截断到 8KB）供诊断；协议 JSON 只认 stdout
    $stderrText = ($stderr -join "`n").Trim()
    $script:PwDirState.LastStderr = (($script:PwDirState.LastStderr + "`n" + $stderrText).Trim())
    if ($script:PwDirState.LastStderr.Length -gt 8192) {
        $script:PwDirState.LastStderr = $script:PwDirState.LastStderr.Substring($script:PwDirState.LastStderr.Length - 8192)
    }

    $jsonText = ($stdout -join "`n").Trim()
    if ([string]::IsNullOrWhiteSpace($jsonText)) {
        throw "pwdir export produced no JSON (exit $LASTEXITCODE). stderr: $stderrText"
    }
    $result = $jsonText | ConvertFrom-Json
    if ($result.schema -ne $script:ProtocolSchema) {
        throw "unsupported export protocol schema $($result.schema) (expected $script:ProtocolSchema)"
    }
    return $result
}

function Remove-PwDirProcessEnvVar {
    <#
        从 Process 环境块真正删除一个变量（仅 Process scope，SPEC §15.1/§15.2）。
        陷阱（实测 pwsh 7.6 / .NET 10）：PowerShell 把 $null marshal 给 .NET string 参数时
        会变成空串 —— [Environment]::SetEnvironmentVariable($k, $null, 'Process') 实际是
        「设为空」而非删除，会在环境块留下 VAR= 空条目并被子进程（含 pwdir.exe）继承，
        污染「不存在」语义（SPEC §8.4/§9.4）。[NullString]::Value 才是真 null，真删除。
    #>
    param([string]$Name)
    [Environment]::SetEnvironmentVariable($Name, [NullString]::Value, 'Process')
}

function Apply-PwDirPatch {
    <#
        把 ExportOutput.patch 应用到 Process scope（SPEC §15.1）。
        禁止 Invoke-Expression：逐 key SetEnvironmentVariable，native 输出永远只是数据。
        每个 managed key 在首次接管时记录 baseline（Existed/Value），restore 据此回滚（SPEC §8.4）。
    #>
    param($Result)

    foreach ($prop in $Result.patch.set.PSObject.Properties) {
        $key = [string]$prop.Name
        if ([string]::IsNullOrEmpty($key)) { continue }
        $value = [string]$prop.Value
        if (-not $script:PwDirState.Baseline.ContainsKey($key)) {
            # 只在首次接管该 key 时记录原值；同 rc 后续 reload 不覆盖 baseline
            $current = [Environment]::GetEnvironmentVariable($key, 'Process')
            $script:PwDirState.Baseline[$key] = [pscustomobject]@{
                Existed = ($null -ne $current)
                Value   = $current
            }
        }
        # set 含「存在但空」三态（SPEC §9.4）：显式 set 空串保留环境块中的空值条目
        [Environment]::SetEnvironmentVariable($key, $value, 'Process')
        $script:PwDirState.Applied[$key] = $value
    }

    foreach ($name in @($Result.patch.unset)) {
        $key = [string]$name
        if ([string]::IsNullOrEmpty($key)) { continue }
        if (-not $script:PwDirState.Baseline.ContainsKey($key)) {
            $current = [Environment]::GetEnvironmentVariable($key, 'Process')
            $script:PwDirState.Baseline[$key] = [pscustomobject]@{
                Existed = ($null -ne $current)
                Value   = $current
            }
        }
        Remove-PwDirProcessEnvVar $key
        $script:PwDirState.Applied[$key] = $script:AbsentMarker
    }

    $script:PwDirState.ActiveRc = $Result.rc
}

function Restore-PwDirEnvironment {
    <#
        按 Baseline 恢复所有 managed key（SPEC §8.4/§8.5）：
        原来存在 → 恢复原值；原来不存在 → 删除（用户手动改动一并丢弃，PwDirEnv owns it）。
        完成后清空 Baseline/Applied 与 ActiveRc。幂等：无 baseline 时空操作。
    #>
    foreach ($key in @($script:PwDirState.Baseline.Keys)) {
        $entry = $script:PwDirState.Baseline[$key]
        if ($entry.Existed) {
            # 原存在 → 恢复原值（含「存在但空」：SetEnvironmentVariable 空串即空值条目）
            [Environment]::SetEnvironmentVariable($key, $entry.Value, 'Process')
        }
        else {
            # 原不存在 → 真删除（用户手动改动一并丢弃，PwDirEnv owns it，SPEC §8.5）
            Remove-PwDirProcessEnvVar $key
        }
    }
    $script:PwDirState.Baseline.Clear()
    $script:PwDirState.Applied.Clear()
    $script:PwDirState.ActiveRc = $null
}

function Write-PwDirLog {
    <# 简洁日志前缀输出（SPEC §19）。M4 使用 info/warn/error 文案；值绝不打印（secrets 泄漏防护）。 #>
    param(
        [string]$Level = 'info',
        [string]$Message = ''
    )
    $line = "pwdir: $Message"
    switch ($Level) {
        'error' { Write-Host $line -ForegroundColor Red }
        'warn'  { Write-Host $line -ForegroundColor Yellow }
        default { Write-Host $line }
    }
}

function Write-PwDirExportSummary {
    <# `pwdir: export +NEW ~CHANGED -GONE` 风格摘要；+ / ~ 依据 Baseline 判断应用前存在性，值不打印（SPEC §3.2/§19）。 #>
    param($Result)

    $parts = @()
    foreach ($prop in $Result.patch.set.PSObject.Properties) {
        $entry = $script:PwDirState.Baseline[$prop.Name]
        $mark = if ($entry -and $entry.Existed) { '~' } else { '+' }
        $parts += "$mark$($prop.Name)"
    }
    foreach ($name in @($Result.patch.unset)) {
        $parts += "-$name"
    }
    if ($parts.Count -gt 0) {
        Write-PwDirLog info ('export ' + ($parts -join ' '))
    }
}

# ---------------------------------------------------------------------------
# 公开函数
# ---------------------------------------------------------------------------

function Enable-PwDirEnv {
    <#
    .SYNOPSIS
        在当前会话启用 PwDirEnv。

    .DESCRIPTION
        解析 pwdir.exe，保存用户现有 prompt 函数（须定义在 global 作用域，如 $PROFILE）
        并安装 wrapper，使每次 prompt 渲染前执行 Invoke-PwDirEnvHook。幂等。
    #>
    [CmdletBinding()]
    param(
        # pwdir.exe 显式路径；不指定时按 PWDIRENV_EXE > PATH > 开发目录探测
        [string]$PwDirPath = ''
    )

    if ($script:PwDirState.Enabled) {
        Write-PwDirLog info 'already enabled'
        return
    }

    # 先解析 exe：解析失败抛终止错误，此时尚未改动 prompt，无副作用
    $script:PwDirState.PwDirExe = Resolve-PwDirExe -PwDirPath $PwDirPath

    # 保存用户原 prompt。注意：function: 查找沿模块作用域链进行，只能看到 global 作用域的
    # prompt —— 与「prompt 定义在 $PROFILE」的真实场景一致。若现有 prompt 已是本模块 wrapper
    # （模块被 -Force 重载后 script 状态丢失），不能把它当作用户 prompt 保存，
    # 从全局镜像 $global:PwDirEnvSavedPrompt 恢复。
    $existing = Get-Item -LiteralPath 'function:prompt' -ErrorAction Ignore
    if ($existing -and ("$($existing.ScriptBlock)" -like "*$script:PromptMarker*")) {
        if ($null -eq $script:PwDirState.OriginalPrompt) {
            $mirror = Get-Variable -Name 'PwDirEnvSavedPrompt' -Scope Global -ErrorAction Ignore
            if ($mirror) { $script:PwDirState.OriginalPrompt = $mirror.Value }
        }
    }
    elseif ($existing) {
        $script:PwDirState.OriginalPrompt = $existing.ScriptBlock
        $global:PwDirEnvSavedPrompt = $script:PwDirState.OriginalPrompt
    }
    else {
        # prompt 函数不存在：可能是模块被 -Force 重载时 wrapper 已被模块清理机制移除。
        # 全局镜像只在曾经保存过用户原 prompt 时存在 —— 用它恢复链路，保证本次 Disable 能还原。
        $mirror = Get-Variable -Name 'PwDirEnvSavedPrompt' -Scope Global -ErrorAction Ignore
        if ($mirror) { $script:PwDirState.OriginalPrompt = $mirror.Value }
    }

    # 安装 wrapper。必须落在 global 作用域，host 的 prompt 解析才能看到它；
    # scriptblock 保留模块亲和性，内部 $script: 仍指向本模块状态。
    function global:prompt {
        $__pwdirWrapper = $script:PromptMarker   # marker：识别「是否已是本模块 wrapper」
        Invoke-PwDirEnvHook
        if ($null -ne $script:PwDirState.OriginalPrompt) {
            & $script:PwDirState.OriginalPrompt
        }
        else {
            "PS $($executionContext.SessionState.Path.CurrentLocation)$('>' * ($nestedPromptLevel + 1)) "
        }
    }

    $script:PwDirState.Enabled = $true
}

function Disable-PwDirEnv {
    <#
    .SYNOPSIS
        在当前会话禁用 PwDirEnv 并完整恢复现场。

    .DESCRIPTION
        先按 baseline 卸载已加载的项目环境，再恢复用户原 prompt（无原 prompt 时删除 wrapper，
        host 回落到内置默认 prompt）。幂等。
    #>
    [CmdletBinding()]
    param()

    if (-not $script:PwDirState.Enabled) {
        Write-PwDirLog info 'already disabled'
        return
    }

    # 先恢复环境再卸 wrapper（幂等：无 baseline 时空操作）
    Restore-PwDirEnvironment

    if ($null -ne $script:PwDirState.OriginalPrompt) {
        Set-Item -LiteralPath 'function:global:prompt' -Value $script:PwDirState.OriginalPrompt
    }
    else {
        Remove-Item -LiteralPath 'function:global:prompt' -Force -ErrorAction Ignore
    }
    $script:PwDirState.OriginalPrompt = $null
    $script:PwDirState.Enabled = $false
    Write-PwDirLog info 'disabled'
}

function Invoke-PwDirEnvHook {
    <#
    .SYNOPSIS
        Prompt hook：检查目录变化并按需加载 / 卸载项目环境。

    .DESCRIPTION
        Host 从不下钻执行 RC —— 评估一律发生在 pwdir.exe 的隔离子进程，本函数只应用
        返回的结构化 patch（SPEC §15）。每次 prompt 都调用 native export：core 依据
        磁盘 fingerprint cache 判定无变化时返回空 patch（快速路径，不 spawn pwsh，M6）；
        rc 与 ActiveRc 不一致时传 --force 强制评估。错误绝不破坏 prompt（SPEC §13.4）：
        catch 一切、只记录、保留最后成功环境。
    #>
    [CmdletBinding()]
    param()

    if ($script:PwDirState.HookRunning) { return }   # reentrancy guard（SPEC §13.3）
    if (-not $script:PwDirState.Enabled) { return }
    $script:PwDirState.HookRunning = $true
    try {
        $cwd = Get-PwDirCurrentFileSystemPath
        if ($null -eq $cwd) {
            # 非 FileSystem provider 或 cwd 被删除（SPEC §5.4/§20.4）：卸载已加载环境并跳过发现
            if ($null -ne $script:PwDirState.ActiveRc) {
                Restore-PwDirEnvironment
                Write-PwDirLog info 'current directory unavailable; unloaded managed environment'
            }
            return
        }

        $result = Invoke-PwDirNative
        $newRc = $result.rc
        $oldRc = $script:PwDirState.ActiveRc
        $rcChanged = ($newRc -ne $oldRc)

        if ($rcChanged) {
            # rc 变化（SPEC §8.4 恢复时机）：先按旧 rc 的 baseline 恢复到干净 Host 环境，
            # 再基于该干净环境重新 export 新 rc —— 保证新 rc 的 diff/_baseline 都以真实环境为准
            if ($null -ne $oldRc) {
                Restore-PwDirEnvironment
                Write-PwDirLog info "unloading $oldRc"
            }
            if ($null -ne $newRc) {
                # M6 force 语义：cache 跨会话共享，rc 与 ActiveRc 不一致（新 shell 首次进入 /
                # 切换目录）时必须强制评估，否则 core 会命中有效 cache 返回空 patch 而漏掉加载。
                # - 恢复过旧环境（oldRc 非 null）→ 必须重 export（diff 基准已变）；
                # - 首次进入（oldRc 为 null）且首次 export 已完整评估（action=apply，
                #   baseline 即当前干净环境）→ 直接应用，避免重复 spawn；
                #   action=noop（含快速路径命中）→ 补一次 force 确保真实加载。
                $needsForcedExport = ($null -ne $oldRc) -or ($result.action -eq 'noop')
                if ($needsForcedExport) {
                    $result = Invoke-PwDirNative -Force
                    $newRc = $result.rc
                }
            }
        }
        if ($null -eq $newRc) { return }   # 无 rc：环境已处于 baseline，无事可做

        switch ($result.action) {
            'apply' {
                if ($rcChanged) { Write-PwDirLog info "loading $newRc" }
                Apply-PwDirPatch $result
                Write-PwDirExportSummary $result
            }
            'noop' {
                if ($rcChanged) {
                    Write-PwDirLog info "loading $newRc"
                    # noop 无 managed keys 可应用，但必须记录 ActiveRc —— 否则后续每次
                    # prompt 都判定 rc 变化并强制重评估，空 diff rc 的快速路径稳态无法形成
                    $script:PwDirState.ActiveRc = $newRc
                }
            }
            'error' {
                # last-known-good（SPEC §20.2）：同一项目内失败 → 保留上次成功环境；
                # 切换到新项目失败 → 旧环境已按 baseline 卸载，保持干净 baseline
                $detail = (@($result.diagnostics) | ForEach-Object { $_.message }) -join '; '
                if ($rcChanged) {
                    Write-PwDirLog error "evaluation failed; keeping baseline environment$(if ($detail) { ": $detail" })"
                }
                else {
                    Write-PwDirLog error "evaluation failed; keeping previous environment$(if ($detail) { ": $detail" })"
                }
            }
            'blocked' {
                # patch 恒为空，RC 内容绝不执行；环境保持 last-known-good / baseline
                if ($rcChanged) {
                    Write-PwDirLog warn ".envrc.ps1 is blocked; run 'pwdir allow' (previous environment restored)"
                }
                else {
                    Write-PwDirLog warn ".envrc.ps1 is blocked; run 'pwdir allow' (keeping last-known-good environment)"
                }
            }
            'unload' {
                # 协议预留动作（M6+）；M4 防御性处理：卸载已加载环境
                if ($null -ne $script:PwDirState.ActiveRc) {
                    Restore-PwDirEnvironment
                    Write-PwDirLog info 'unloaded managed environment'
                }
            }
            default {
                Write-PwDirLog warn "unknown action '$($result.action)'; ignored"
            }
        }
    }
    catch {
        # hook 错误绝不破坏 prompt（SPEC §13.4）：吞掉异常、只记录、保留最后成功环境
        Write-PwDirLog error ('hook failed: ' + $_.Exception.Message)
    }
    finally {
        $script:PwDirState.HookRunning = $false
    }
}

function Get-PwDirEnvStatus {
    <#
    .SYNOPSIS
        返回当前会话的 PwDirEnv 状态概览。
    #>
    [CmdletBinding()]
    param()

    [PSCustomObject]@{
        Version       = $script:ModuleVersion
        Enabled       = $script:PwDirState.Enabled
        PwDirExe      = $script:PwDirState.PwDirExe
        ActiveRc      = $script:PwDirState.ActiveRc
        ManagedKeys   = @($script:PwDirState.Applied.Keys)
        HookRunning   = $script:PwDirState.HookRunning
        BaselineCount = $script:PwDirState.Baseline.Count
        Fingerprint   = $script:PwDirState.Fingerprint
    }
}

Export-ModuleMember -Function @(
    'Enable-PwDirEnv',
    'Disable-PwDirEnv',
    'Invoke-PwDirEnvHook',
    'Get-PwDirEnvStatus'
)
