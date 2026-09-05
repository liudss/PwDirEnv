#requires -Version 7.2
# PwDirEnv stdlib（M5，SPEC §10）：在 RC dot-source 之前由 bootstrap.ps1 加载，为 RC 提供 direnv 风格 helper。
# 所有 helper 运行在隔离 evaluator 进程内，直接读写 $env: 与 $script:__PwDirEnvState；
# 其 effect 随 evaluator 结束被 native core 捕获进 desired environment（SPEC §7.1/§8.1）。
# 内部 metadata 变量使用 __PwDirEnvState 命名空间，避免与 RC 用户变量冲突（SPEC §17）。
#
# ADR-007：dotenv 解析器用 PowerShell 实现于本文件（Read-PwDirDotEnv），是对 SPEC §11
# 「优先 Rust parser」的有意偏离 —— helper 与进程 env 在同一 evaluator 内，走 native parser
# 需要把 pwdir.exe 路径传进 evaluator 并跨进程回传 KV，而 dotenv 调用低频，复杂度不划算。

# 兜底初始化：bootstrap 步骤 2 已初始化 $script:__PwDirEnvState（dot-source 共享同一 script scope），
# 这里只防独立加载 / REPL 调试场景。注意真实流程中 stdlib 加载发生在 bootstrap Set-Location 之前，
# 此兜底的 ProjectRoot（当前 PS 位置）不是项目根 —— 真实流程必须依赖 bootstrap 注入的值。
if ($null -eq $script:__PwDirEnvState) {
    $script:__PwDirEnvState = @{
        ProjectRoot  = (Get-Location).ProviderPath
        Watches      = @()
        SourcedFiles = @()
    }
}

# ---------------------------------------------------------------------------
# 内部工具（approved verbs；不面向 RC 作者）
# ---------------------------------------------------------------------------

function ConvertTo-PwDirNormalizedPathString {
    <# Windows 路径字符串规范化（与 pwdir-core::pathutil::normalize_path_str 同语义，SPEC §9.3）：
       `/` → `\`，去尾分隔符（盘符根如 `C:\` 除外）。纯字符串操作，不做 Resolve-Path ——
       路径可能不存在、是 UNC 或受网络状态影响，比较不能有文件系统副作用。 #>
    param([Parameter(Mandatory = $true)][AllowEmptyString()][string]$Path)

    $s = $Path.Replace('/', '\')
    # 病态输入（全部分隔符，如 `\`、`\\`）原样返回，避免被 trim 成空串
    if ($s -ne '' -and $s.TrimEnd('\') -eq '') { return $s }
    $trimmed = $s.TrimEnd('\')
    # 盘符根（`C:`）还原尾分隔符；UNC 根（`\\server\share`）无此问题
    if ($trimmed -match '^[A-Za-z]:$') { return "$trimmed\" }
    return $trimmed
}

function Test-PwDirPathEquivalent {
    <# Windows 语义等价路径比较（SPEC §9.3）：规范化后大小写不敏感比较。
       不用 Resolve-Path：路径允许不存在，也避免 UNC/网络路径的解析副作用。 #>
    param(
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$A,
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$B
    )
    return ((ConvertTo-PwDirNormalizedPathString -Path $A) -ieq (ConvertTo-PwDirNormalizedPathString -Path $B))
}

function ConvertTo-PwDirAbsolutePath {
    <# 相对路径一律以 ProjectRoot 为基准解析为绝对路径（SPEC §9.2/§10.1），而非 $PWD ——
       RC 可能再 Set-Location，helper 语义必须稳定指向项目根。
       不用 Resolve-Path：路径允许尚不存在（PATH_add / watch 预创建场景），且避免 UNC /
       网络路径的解析副作用（SPEC §9.3）。GetFullPath 只做 `.`/`..`/分隔符的字符串级规范化。 #>
    param([Parameter(Mandatory = $true)][string]$Path)

    $root = [string]$script:__PwDirEnvState.ProjectRoot
    if ([string]::IsNullOrWhiteSpace($root)) {
        throw 'PwDirEnv stdlib: ProjectRoot is not initialized'
    }
    $candidate = if ([IO.Path]::IsPathRooted($Path)) { $Path } else { Join-Path -Path $root -ChildPath $Path }
    # Join-Path(绝对 root, 相对路径) 的结果必为 rooted 路径，GetFullPath 不会回退到 .NET
    # 进程 CWD（Set-Location 不同步它，不能依赖）
    return [IO.Path]::GetFullPath($candidate)
}

function Add-PwDirWatch {
    <# 去重注册 watch 依赖：kind + 等价路径相同则跳过。
       项形状 [ordered]@{ kind; path } 与 pwdir-protocol::WatchDependency JSON 对齐。 #>
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
    <# 去重登记 sourced 文件（evaluation fingerprint 元数据，SPEC §12.2）。 #>
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
# dotenv 解析（ADR-007；SPEC §11 最小子集）
# ---------------------------------------------------------------------------

function ConvertFrom-PwDirDotEnvDoubleQuoted {
    <# 双引号值的最小 escape 展开：\" \\ \n \r \t；其余 `\x` 保留为 `\` + 字面量 x（SPEC §11）。 #>
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
                # 最小 escape 集合之外的转义不解释，保留原样
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
    <# 单行解析（SPEC §11）：空行 / `#` 注释 / 无 `=` / key 不合规 → $null（调用方跳过该行，lenient）。
       支持 `export ` 前缀剥离、KEY=value / "双引号"（最小 escape）/ '单引号'（全字面量）/
       无引号（`=` 后剩余文本，前后空白 trim；引号不闭合时按字面量处理，确定性优先）。
       不做 command substitution 与 ${VAR} 展开。 #>
    param([Parameter(Mandatory = $true)][AllowEmptyString()][string]$Line)

    $line = $Line.Trim()
    if ($line -eq '' -or $line.StartsWith('#')) { return $null }
    # export 前缀剥离（-creplace 区分大小写：只认小写 export，与 sh 语义一致）
    $line = $line -creplace '^export\s+', ''

    $eq = $line.IndexOf('=')
    if ($eq -lt 1) { return $null }   # 无 `=`，或 `=` 前没有 key
    $key = $line.Substring(0, $eq).Trim()
    if ($key -notmatch '^[A-Za-z_][A-Za-z0-9_]*$') { return $null }   # key 不合规 → 跳过
    $raw = $line.Substring($eq + 1).Trim()

    $value =
        if ($raw.StartsWith('"') -and $raw.Length -ge 2 -and $raw.EndsWith('"')) {
            ConvertFrom-PwDirDotEnvDoubleQuoted -Text $raw.Substring(1, $raw.Length - 2)
        }
        elseif ($raw.StartsWith("'") -and $raw.Length -ge 2 -and $raw.EndsWith("'")) {
            $raw.Substring(1, $raw.Length - 2)   # 单引号：全字面量
        }
        else {
            $raw   # 无引号：`=` 后剩余文本（已 trim）
        }

    return [pscustomobject]@{ Key = $key; Value = $value }
}

function Read-PwDirDotEnv {
    <# 读入 .env 文件为有序 KV 表（后行覆盖前行）。
       UTF-8（[IO.File]::ReadAllText 遇 BOM 自动剥离，SPEC §11 允许 BOM）；CRLF / LF 均可。 #>
    param([Parameter(Mandatory = $true)][string]$Path)

    # UTF8Encoding($false)：显式无 BOM 写出语义的读取配对；BOM 由 ReadAllText 自动剥离
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
    <# 前置条件：文件已确认存在。逐 key 写入 Process env。
       默认不覆盖已存在的环境变量（direnv 兼容语义，含「存在但为空」）；-Overload 强制覆盖。 #>
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
# RC 可用的 stdlib helper（名称为 SPEC §10.1 指定的 direnv 对应名）
# ---------------------------------------------------------------------------

function PATH_add {
    # SPEC §9.2 指定名（非 approved verb，保持 direnv 心智模型）。
    <# 相对路径以 ProjectRoot 为基准解析为绝对路径（允许路径尚不存在），prepend 到 $env:PATH；
       等价项（Windows 语义）从原位置移除后插到最前，无等价项则直接插到最前（SPEC §9.2）。 #>
    param([Parameter(Mandatory = $true, Position = 0)][string]$Path)

    $dir = ConvertTo-PwDirAbsolutePath -Path $Path
    $separator = [IO.Path]::PathSeparator
    $current = $env:PATH
    # PATH 不存在 / 为空：视为空 PATH 直接 set（SPEC §9.4 三态归并）
    if ([string]::IsNullOrEmpty($current)) {
        $env:PATH = $dir
        return
    }
    # 等价项移到最前 + 去重；保留空段 —— Windows 上空 PATH 项隐含「当前目录」，
    # 不属于本 helper 的清理范围
    $items = $current -split ([regex]::Escape($separator))
    $kept = @($items | Where-Object { -not (Test-PwDirPathEquivalent -A $_ -B $dir) })
    $env:PATH = (@($dir) + $kept) -join $separator
}

function dotenv {
    # SPEC §10.1/§11 指定名（direnv 对应 helper 名）。
    <# 解析 .env（默认）并设置环境变量；文件不存在 → throw（使 evaluation 失败，SPEC §10.1）。
       默认不覆盖已存在的环境变量（direnv 兼容），-Overload 强制覆盖。 #>
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
    # SPEC §10.1 指定名（direnv 对应 helper 名）。
    <# 同 dotenv，但文件不存在时静默跳过（no-op，不报错）。 #>
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
    # SPEC §10.1 指定名（direnv 对应 helper 名）。
    <# 注册 file watch 依赖（支持多参数）；相对路径按 ProjectRoot 绝对化，相同项去重。 #>
    param([Parameter(Mandatory = $true, Position = 0, ValueFromRemainingArguments = $true)][string[]]$Path)
    foreach ($p in $Path) { Add-PwDirWatch -Path $p -Kind 'file' }
}

function watch_dir {
    # SPEC §10.1 指定名（direnv 对应 helper 名）。
    <# 注册 dir watch 依赖（支持多参数）；相对路径按 ProjectRoot 绝对化，相同项去重。 #>
    param([Parameter(Mandatory = $true, Position = 0, ValueFromRemainingArguments = $true)][string[]]$Path)
    foreach ($p in $Path) { Add-PwDirWatch -Path $p -Kind 'dir' }
}

function source_env {
    # SPEC §10.1 指定名（direnv 对应 helper 名）。
    <# 在 evaluator 当前 scope dot-source 指定 .ps1（$PSScriptRoot 语义保留）；
       文件不存在 → throw；成功后把绝对路径同时纳入 Watches（kind=file）与 SourcedFiles（SPEC §10.1）。
       sourced 文件不要求单独 allow —— 用户授权主 RC 即授权其代码路径，但必须进入 fingerprint。 #>
    param([Parameter(Mandatory = $true, Position = 0)][string]$Path)

    $absolute = ConvertTo-PwDirAbsolutePath -Path $Path
    if (-not (Test-Path -LiteralPath $absolute -PathType Leaf)) {
        throw "source_env: file not found: $Path (resolved to $absolute)"
    }
    # dot-source 到当前 scope；与 bootstrap 相同的字面量解析方式（Get-Item -LiteralPath 取 FullName）
    . (Get-Item -LiteralPath $absolute).FullName
    Add-PwDirWatch -Path $absolute -Kind 'file'
    Add-PwDirSourcedFile -Path $absolute
}

function source_up {
    # SPEC §10.1/§5.3 指定名（direnv 对应 helper 名）。
    <# 从 ProjectRoot 的父目录开始向上搜索最近的 <filename>（默认 .envrc.ps1，
       不含 ProjectRoot 自身），找到即 source_env；未找到为 no-op。 #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $false, Position = 0)][string]$FileName = '.envrc.ps1'
    )
    $dir = [IO.Path]::GetDirectoryName([string]$script:__PwDirEnvState.ProjectRoot)
    while (-not [string]::IsNullOrEmpty($dir)) {
        $candidate = Join-Path -Path $dir -ChildPath $FileName
        if (Test-Path -LiteralPath $candidate -PathType Leaf) {
            source_env -Path $candidate
            return
        }
        $dir = [IO.Path]::GetDirectoryName($dir)   # 盘符根 / UNC 根返回 null → 终止
    }
    # 未找到：no-op（SPEC §10.1）
}
