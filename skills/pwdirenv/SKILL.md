---
name: pwdirenv
description: PwDirEnv（命令名 pwdir）是 Windows / PowerShell 7+ 上的 direnv 等价物——进入含 .envrc.ps1 的目录时自动加载其环境变量（set/unset/PATH），离开目录精确恢复。当用户提到 pwdir、PwDirEnv、.envrc.ps1、Windows 上的 direnv、进目录自动设置环境变量、"pwdir allow/deny/status/reload/exec/doctor"、PATH_add/dotenv/watch_file/source_env/source_up、或看到 ".envrc.ps1 is blocked" 提示时使用。
compatibility: Windows 10/11 + PowerShell 7.2+（pwsh）；需已安装 PwDirEnv（pwdir.exe 在 PATH 或设置了 PWDIRENV_EXE）。不支持 Windows PowerShell 5.1、macOS、Linux。
metadata:
  product: PwDirEnv
  version: "0.1.0"
---

# PwDirEnv — Windows 目录环境管理

PwDirEnv 把 direnv 的模型搬到 PowerShell：每个项目根放一个 **`.envrc.ps1`**（合法 PowerShell 脚本），
用户 `cd` 进目录时 hook 在**隔离子进程**中执行它，只把环境变量 diff 应用回当前 shell；离开目录时恢复原值。
函数、alias、模块导入不会泄漏回当前 shell。

## 安全模型（最重要，先读）

- `.envrc.ps1` 是**任意 PowerShell 代码**，PwDirEnv **不是沙箱**。未授权的 RC **一行都不会执行**。
- 首次进入项目会看到：`pwdir: .envrc.ps1 is blocked; run 'pwdir allow'`。
- **执行 `pwdir allow` 之前必须先读一遍 RC 内容**（`Get-Content .envrc.ps1`），向用户说明它会做什么，征得同意后再 allow。
- trust 基于文件内容 **SHA-256**：RC 改动任意一个字节就会重新 blocked，必须再次审阅并 `pwdir allow`（这是设计，不是故障）。
- `pwdir deny` 撤销授权；数据都在 `%LOCALAPPDATA%\PwDirEnv\{allow,cache,state}`，删除该目录等于全部重置。

## 环境检查与启用

```powershell
pwdir --version        # 未找到命令 → 未安装或 bin 不在 PATH（设 $env:PWDIRENV_EXE 或加 PATH）
pwdir doctor           # 9 项自检：pwsh 版本/模块/profile hook/LOCALAPPDATA/长路径等；有 [FAIL] 先修
```

要求 PowerShell **7.2+**（推荐 7.4+）；Windows PowerShell 5.1 不支持。

```powershell
Import-Module PwDirEnv      # 或 Import-Module <安装目录>\modules\PwDirEnv\PwDirEnv.psd1
Enable-PwDirEnv             # 当前会话立即生效
pwdir hook install --yes    # 持久化到 $PROFILE（幂等，带 marker；pwdir hook uninstall 可完整移除）
```

## 常用任务速查

| 任务 | 命令 / 做法 |
|---|---|
| 首次使用某项目 | 先 `Get-Content .envrc.ps1` 审阅 → 说明给用户 → `pwdir allow` |
| RC 改动后重新 blocked | 审阅 diff → `pwdir allow` |
| 强制重评估（不改 trust） | `pwdir reload` |
| 不进目录用项目环境跑命令 | `pwdir exec <目录> <命令> [参数...]`（argv 安全，退出码原样传播；未 allow 拒绝执行） |
| 查看当前状态 | `pwdir status [--json]`；模块侧 `Get-PwDirEnvStatus` |
| 关闭并恢复全部环境 | `Disable-PwDirEnv`（原 prompt 完整保留） |
| 撤销授权 | `pwdir deny [path]` |

最小 `.envrc.ps1` 示例：

```powershell
$env:NODE_ENV = 'development'
PATH_add '.\node_modules\.bin'      # prepend PATH 并去重；路径可尚不存在
dotenv_if_exists '.env.local'       # 存在才加载
watch_file 'package.json'           # 该文件变化 → 自动重新评估
```

## 深入参考（按需加载）

- **写/改 `.envrc.ps1`**（helper 全集、dotenv 语法、隔离执行约束、source_up/source_env、超时）：
  见 [references/rc-authoring.md](references/rc-authoring.md)
- **CLI 完整参考**（allow/deny/status/reload/exec/doctor/hook 的参数、退出码、JSON schema）：
  见 [references/cli.md](references/cli.md)
- **排障**（blocked、hook 不触发、环境残留、超时、5.1 等）：
  见 [references/troubleshooting.md](references/troubleshooting.md)

## 三条铁律

1. allow 前必须审阅 RC —— 它是任意代码，不是沙箱。
2. 只有环境变量会带回当前 shell；RC 里的 function/alias/模块导入都不生效，别建议用户那样做。
3. 日志只打印变量名（`+新增 ~修改 -删除`），**不要尝试打印环境变量值**（可能含 secrets）。
