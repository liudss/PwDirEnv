#requires -Version 7.2
# PwDirEnv evaluator bootstrap（SPEC §17）。
# 由 native core 以 `pwsh -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File` 启动。
# 协议结果只写入 ResultPath 指向的结果文件（临时文件 + atomic rename），绝不与 stdout/stderr 混用（SPEC §7.5）。
param(
    # RC 文件绝对路径（trust 已由 native core 在启动前校验通过）
    [Parameter(Mandatory = $true)]
    [string]$RcPath,

    # RC 所属项目 root，evaluator 执行 RC 前的 cwd
    [Parameter(Mandatory = $true)]
    [string]$ProjectRoot,

    # native core 创建的结果文件路径，bootstrap 写入结构化 JSON result
    [Parameter(Mandatory = $true)]
    [string]$ResultPath,

    # stdlib 脚本路径（native core 从嵌入资源解包到一次性 temp 目录）
    [Parameter(Mandatory = $true)]
    [string]$StdlibPath,

    # 预留（M6+）：watch/source 元数据旁路文件。本里程碑忽略。
    [Parameter(Mandatory = $false)]
    [string]$MetadataPath
)

# 步骤 1：全局终止错误策略 —— RC / stdlib 中任何未捕获错误都进入 catch 分支（SPEC §17）
$ErrorActionPreference = 'Stop'

# 用 -LiteralPath 语义 dot-source 一个真实存在的脚本文件。
# 说明：dot 运算符不接受命名参数（`. -LiteralPath x` 会把 `-LiteralPath` 当命令名解析失败），
# 这里先用 Get-Item -LiteralPath 做字面量解析（不存在时快速失败、通配符不展开），
# 再 dot-source 其 FullName；dot-source 语义保证当前 scope 与 $PSScriptRoot 行为不变。
function Resolve-PwDirEnvLiteralScript {
    param([Parameter(Mandatory = $true)][string]$Path)
    return (Get-Item -LiteralPath $Path).FullName
}

# 结果写入：同目录随机名 .tmp + UTF-8 无 BOM + 原子替换（SPEC §7.5 / §17 步骤 8-9）。
# UTF-8 无 BOM 是结果文件契约的一半：native core 按 UTF-8 解析。
function Write-PwDirEnvResult {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][object]$Payload
    )

    $dir = Split-Path -Parent $Path
    $tmp = Join-Path -Path $dir -ChildPath ([IO.Path]::GetRandomFileName() + '.tmp')
    $json = $Payload | ConvertTo-Json -Depth 20 -Compress
    # UTF8Encoding($false) = 无 BOM；WriteAllText 不追加换行，保证字节级确定性
    [IO.File]::WriteAllText($tmp, $json, [Text.UTF8Encoding]::new($false))
    # .NET Core 3+ 三参 Move 重载：目标存在时原子覆盖替换
    [IO.File]::Move($tmp, $Path, $true)
}

try {
    # 步骤 2：internal metadata 命名空间（SPEC §17；M5 stdlib helper 会写入这两个集合）
    $script:__PwDirEnvState = @{ Watches = @(); SourcedFiles = @() }

    # 捕获私有函数引用：RC 里定义同名函数也不得劫持结果写入通道
    $__PwDirEnvResultWriterFn = ${function:Write-PwDirEnvResult}

    # 步骤 3：加载 stdlib（stdlib 内部做兜底状态初始化）
    . (Resolve-PwDirEnvLiteralScript -Path $StdlibPath)

    # 步骤 4：RC 的相对路径以 project root 为基准（SPEC §7.4）
    Set-Location -LiteralPath $ProjectRoot

    # 步骤 5：dot-source RC
    . (Resolve-PwDirEnvLiteralScript -Path $RcPath)

    # 步骤 6：枚举 Process environment，转成按 key 排序的有序 map（输出确定性）
    $vars = [ordered]@{}
    foreach ($entry in [System.Environment]::GetEnvironmentVariables('Process').GetEnumerator() | Sort-Object -Property Key) {
        $vars[[string]$entry.Key] = [string]$entry.Value
    }

    # 步骤 6.5：剔除 `-ExecutionPolicy Bypass` 启动参数导致的人工产物（SPEC §7.2）：
    # pwsh 会把 PSExecutionPolicyPreference 写入自身 Process environment 供子 pwsh 继承，
    # 这是启动参数的人工产物，Host 里通常没有该变量，原样上报会被 diff 当成"新增变量"
    # 污染用户会话 —— 不是 RC 的意图，必须在构造 JSON result 前从上报环境中剔除。
    # [ordered]@{} 的 Remove 行为依赖底层比较器，这里无论其是否区分大小写，
    # 都遍历 key 做大小写不敏感比较后删除（先物化匹配 key，避免枚举中修改集合抛异常）。
    $policyArtifactKeys = @(
        $vars.Keys | Where-Object { [string]$_ -ieq 'PSExecutionPolicyPreference' }
    )
    foreach ($artifactKey in $policyArtifactKeys) {
        $null = $vars.Remove([string]$artifactKey)
    }

    # 步骤 7：构造 JSON result（字段名与 pwdir-protocol::EvaluationResult 对齐；schema/ok 为外层信封）
    $payload = [ordered]@{
        schema        = 1
        ok            = $true
        environment   = [ordered]@{ vars = $vars }
        watches       = @($script:__PwDirEnvState.Watches)
        sourced_files = @($script:__PwDirEnvState.SourcedFiles)
        diagnostics   = @()
    }

    & $__PwDirEnvResultWriterFn -Path $ResultPath -Payload $payload
    # 步骤 10：RC 的任何 stdout/stderr 输出都已流过管道，绝不进入结果文件
    exit 0
}
catch {
    # 捕获异常：结构化 error result，保留 exception type / message / script stack trace（SPEC §17）
    $errorPayload = [ordered]@{
        schema = 1
        ok     = $false
        error  = [ordered]@{
            kind      = 'evaluation-failed'
            message   = [string]$_.Exception.Message
            exception = [string]$_.Exception.GetType().FullName
            stack     = [string]$_.ScriptStackTrace
        }
    }
    try {
        & $__PwDirEnvResultWriterFn -Path $ResultPath -Payload $errorPayload
    }
    catch {
        # 结果文件本身写失败：只能以非 0 退出，native core 走 stdout/stderr 兜底分支
    }
    exit 1
}
