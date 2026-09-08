# 编写 .envrc.ps1 参考

`.envrc.ps1` 是**合法 PowerShell 脚本**，默认文件名固定 `.envrc.ps1`，放在项目根（pwdir 从当前目录
向上查找**最近的一份**，不隐式叠加父级 RC）。

## 执行模型（写 RC 前必须理解）

1. RC 在 `pwsh -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass` 的**隔离子进程**中执行。
   - **用户 profile 不加载**；RC 里定义的 function/alias、`Import-Module`、PSReadLine/prompt 设置
     都**不会**带回用户当前 shell —— 只有环境变量 diff 会。
2. 执行前工作目录设为项目根；`$PSScriptRoot` 指向 RC 所在目录。
3. helper 的相对路径一律以**项目根**为基准（不是 `$PWD` —— RC 内部可能自己 `Set-Location`）。
   允许路径尚不存在，不做 `Resolve-Path`。
4. 默认评估超时 **10 秒**（`pwdir exec` 场景 60 秒）。不要在 RC 里放交互式提示（`Read-Host`）、
   长驻进程或等待输入的命令。
5. RC 的 stdout/stderr（`Write-Host`/`Write-Output`/外部命令输出）会显示给用户，但**不参与协议**；
   可以用 `Write-Host` 输出提示信息。
6. RC 抛错或超时：hook 保留上次成功环境（last-known-good），不部分应用。
7. 环境变量名大小写不敏感（`PATH`/`Path` 同一 key）；设置/删除用原生 PowerShell：

   ```powershell
   $env:FOO = 'bar'
   Remove-Item Env:LEGACY -ErrorAction Ignore
   ```

## stdlib helper 速查

RC 执行前自动注入，无需 import。

| Helper | 行为 |
|---|---|
| `PATH_add <path>` | 相对路径解析到项目根后转绝对路径，**prepend** 到 `$env:PATH`；等价项（Windows 语义：大小写不敏感、`/`→`\`、尾分隔符无关，纯字符串比较）去重后移到最前。PATH 缺失/为空视为空 PATH。 |
| `dotenv [path]` | 解析 dotenv 文件（默认 `.env`）并设置变量；**文件不存在直接 throw，评估失败**。 |
| `dotenv_if_exists [path]` | 同 `dotenv`，但文件不存在时静默跳过。 |
| `dotenv -Overload` / `dotenv_if_exists -Overload` | 强制覆盖已存在的环境变量（默认不覆盖，direnv 兼容语义）。 |
| `watch_file <path> [...]` | 注册文件依赖；任一注册文件变化 → 自动重新评估。支持多个参数，重复注册自动去重。 |
| `watch_dir <path> [...]` | 注册目录依赖（同上）。 |
| `source_env <path>` | 在评估进程当前 scope dot-source 另一个 `.ps1`（保留 `$PSScriptRoot` 语义）；文件不存在 → throw。被 source 文件自动纳入 watch 与指纹，其改动触发重评估，**无需单独 allow**。 |
| `source_up [filename]` | 从项目根的**父目录**向上搜索最近的 `<filename>`（默认 `.envrc.ps1`，不含项目根自身），找到即 `source_env`；未找到为 no-op。 |

## dotenv 语法子集

UTF-8（BOM 自动剥离）、CRLF/LF 均可。支持：

```text
KEY=value
KEY="value"
KEY='value'
export KEY=value      # export 前缀（仅小写）会被剥离
# 整行注释
（空行忽略）
```

- 双引号支持最小 escape：`\"` `\\` `\n` `\r` `\t`；其余 `\x` 保留为反斜杠 + 字面量。
- 单引号为全字面量。
- 无引号值取 `=` 后剩余文本，前后空白 trim。
- key 不合规或无 `=` 的行静默跳过。
- **不支持**：shell 命令替换（`` `...` `` / `$(...)`）、`${VAR}` 展开、行内 `#` 注释、跨行值。
- 默认不覆盖已存在变量；需要覆盖用 `-Overload`。

## 常见 RC 模式

```powershell
# 基本变量
$env:APP_PORT = '8080'

# 根据命令是否存在条件设置（PowerShell 原生，无需 has helper）
if (Get-Command fnm -ErrorAction Ignore) {
    fnm env --use-on-cd | Out-String | Invoke-Expression   # 只影响 evaluator 进程
}

# 从文件推导变量（合法 PowerShell 全可用）
$env:APP_VERSION = (Get-Content package.json | ConvertFrom-Json).version

# 复用父级目录的统一配置
source_up

# 复用任意位置的脚本
source_env '..\common\.envrc.ps1'

# 依赖文件变化自动重载
watch_file 'package.json', 'pnpm-lock.yaml'
```

## 注意事项

- `Invoke-Expression` 在 RC 里使用只影响 evaluator 进程，不会污染用户 shell 的函数/alias。
- 不要在 RC 里修改 User/Machine scope 环境变量（`[Environment]::SetEnvironmentVariable(..., 'User')`）——
  那会永久污染系统，且违背工具的进程级模型。
- 需要多个项目共享配置：用 `source_up` / `source_env` 显式组合，不要依赖自动叠加（不会发生）。
