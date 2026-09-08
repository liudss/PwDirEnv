# pwdir CLI 完整参考

二进制 `pwdir.exe`（命令名 `pwdir`）。数据目录：`%LOCALAPPDATA%\PwDirEnv\{allow,cache,state}`。

退出码约定：`0` 成功；`2` 找不到 RC；`3` IO 错误；`1` 其它错误。
例外：`pwdir export` 在 `blocked`/评估失败时仍 exit 0 并输出完整 JSON（这是模块协议，非人工命令）。

## pwdir allow [PATH]

- 省略 PATH 时从当前目录向上发现 RC；显式 PATH 时授权指定文件。
- 读取 RC 字节计算 SHA-256，trust record 原子写入 allow 目录。
- 已 allow 的内容无变化时重复执行无害。
- 内容变化后旧 hash 失效，状态回到 blocked，需重新 allow。

## pwdir deny [PATH]

- 删除匹配 trust record，幂等（无 record 输出 `nothing to deny`，exit 0）。
- 若该 RC 当前已加载，下一次 hook 卸载其环境。

## pwdir status [--json]

人类输出包含：cwd、发现的 RC、trust 状态（`allowed`/`blocked`/`none`）、RC 内容 SHA-256、
cache 状态（`none`/`fresh`/`stale`）、数据目录。`--json` 为稳定 schema：

```json
{
  "cwd": "C:\\src\\app",
  "discovered_rc": { "project_root": "C:\\src\\app", "rc_path": "C:\\src\\app\\.envrc.ps1" },
  "trust_status": "allowed",
  "rc_sha256": "…64 hex…",
  "cache_status": "fresh",
  "dirs": { "allow": "…", "cache": "…", "state": "…" }
}
```

`status` 只读，不触发评估、不写 cache。

## pwdir reload

使当前 RC 的 cache 失效（幂等删除），下一次 hook/export 强制重新评估。
找不到 RC 时输出 `nothing to reload` 并 exit 0。

## pwdir exec <DIR> <COMMAND> [ARGS...]

以解析后的项目环境启动子进程：

1. 发现 DIR 的 RC → trust 检查（未 allow：报错 exit 1，**RC 一行不执行**，COMMAND 不启动）；
2. 隔离子进程评估 RC（超时 60s）；
3. 环境 diff 叠加到子进程（只影响该子进程，不碰 User/Machine）；
4. argv 直接走 OS 进程模型（`Command::new` + args），**不经 shell 拼接**，参数中空格/引号/`$` 安全；
5. COMMAND 的退出码原样成为 `pwdir exec` 的退出码；stdout/stderr 字节级透传。

无 RC 时默认继承当前环境直接执行；加 `--require-rc` 则报 RcNotFound（exit 2）。

示例：

```powershell
pwdir exec .\backend pwsh -NoProfile -Command 'npm test'
pwdir exec C:\src\app git status
```

## pwdir doctor [--json]

逐项自检，单项失败不中断；人类输出 `[OK]/[WARN]/[FAIL] name: message`，
`--json` 输出 `{ "checks": [{name,status,message}], "all_ok": bool }`；存在 FAIL 时 exit 1。

检查项：os、pwsh 可用性、PowerShell 版本（主版本 <7 Fail；7.0/7.1 Warn，目标 ≥7.2）、
module 安装位置、profile hook marker、LOCALAPPDATA 可写、TEMP 可写、ExecutionPolicy、
长路径（LongPathsEnabled）。

## pwdir hook

- `pwdir hook pwsh`：输出两行 bootstrap（`Import-Module '<psd1>'` + `Enable-PwDirEnv`），供手工 profile 使用。
- `pwdir hook install [--yes] [--path <file>]`：向 profile 写入带
  `# >>> PwDirEnv (begin) >>>` / `# <<< PwDirEnv (end) <<<` marker 的块。
  默认目标 `$PROFILE.CurrentUserAllHosts`；已安装则幂等提示；非交互直接写，tty 下无 `--yes` 先确认。
- `pwdir hook uninstall [--path <file>]`：按 marker 整块移除，块外内容原样保留；无 marker 输出
  `nothing to uninstall`。

## pwdir export --format json [--force]

PowerShell 模块与 native core 的协议入口（人工一般不直接用）。输入隐式为当前进程环境 + cwd，输出：

```json
{
  "schema": 1,
  "action": "apply",
  "rc": "C:\\src\\app\\.envrc.ps1",
  "patch": { "set": { "FOO": "bar" }, "unset": ["OLD"] },
  "managed_keys": ["FOO", "OLD"],
  "fingerprint": "…",
  "diagnostics": []
}
```

- `action`：`noop`（无 RC 或 diff 为空）/ `apply`（有 patch）/ `blocked`（未授权，patch 恒空）/
  `error`（RC 抛错或超时，patch 恒空）/ `unload`（预留）。
- `--force`：跳过指纹快速路径强制评估。模块在 RC 与上次 ActiveRc 不一致（新 shell 首次进入、切目录）时必传，
  否则可能命中别的会话留下的 cache 返回空 patch。
- 快速路径：RC 内容 hash + watch/source 文件 size+mtime + 内嵌 bootstrap/stdlib hash 全部未变时，
  不 spawn evaluator 直接返回 noop（稳态约 20-30ms vs 冷启动约 500ms）。
- cache 只存指纹/元数据，**不持久化环境变量值**。

## PowerShell 模块函数

```powershell
Enable-PwDirEnv [-PwDirPath <pwdir.exe>]   # 安装 prompt wrapper（保留原 prompt）
Disable-PwDirEnv                            # 移除 wrapper、恢复原 prompt、恢复全部环境
Invoke-PwDirEnvHook                         # 手动触发一次 hook（一般由 prompt wrapper 自动调）
Get-PwDirEnvStatus                          # Version/Enabled/PwDirExe/ActiveRc/ManagedKeys/Fingerprint 等
```

模块只用 `EnvironmentVariableTarget.Process`；对 native 输出 parse JSON 后逐 key 应用，
**禁止 Invoke-Expression**。
