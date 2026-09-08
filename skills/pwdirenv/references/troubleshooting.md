# PwDirEnv 排障

按现象查表；诊断第一步通常是 `pwdir doctor`（9 项自检）和 `pwdir status --json`。

## 现象与处理

| 现象 | 原因 / 处理 |
|---|---|
| `.envrc.ps1 is blocked; run 'pwdir allow'` | 正常安全机制。**先审阅 RC 内容**（`Get-Content .envrc.ps1`），向用户说明后执行 `pwdir allow`。刚改过 RC 也会重新 blocked（内容 SHA-256 失效）。 |
| hook 完全不触发（cd 进目录没反应） | ① 当前会话执行过 `Enable-PwDirEnv` 吗？② `pwdir doctor` 的 `profile-hook` 项；③ 持久化：`pwdir hook install --yes` 后重启 pwsh。④ `Get-PwDirEnvStatus` 看 `Enabled`。 |
| 改了 RC 以外的文件，环境没更新 | 该文件必须在 RC 里用 `watch_file`/`watch_dir` 注册才会触发重评估；临时手动一次用 `pwdir reload`。 |
| 环境变量疑似残留/没恢复 | `Get-PwDirEnvStatus` 查看 `ManagedKeys`/`ActiveRc`；`Disable-PwDirEnv` 全量恢复到启用前；`pwdir deny` 后重新 allow 可强制干净重载。 |
| `pwdir` 命令找不到 | 安装目录 `bin` 不在 PATH：把 `<InstallRoot>\bin` 加 PATH，或设 `$env:PWDIRENV_EXE = '<InstallRoot>\bin\pwdir.exe'` 后再 `Enable-PwDirEnv -PwDirPath $env:PWDIRENV_EXE`。 |
| RC 评估报错 / 超时 | hook 会保留上次成功环境（last-known-good）并提示。复现错误：在项目目录开一个 `pwsh -NoProfile`，先 `pwdir allow`，再直接运行 RC 看报错；注意 RC 默认 10s 超时（`pwdir exec` 60s），不要放交互/长驻命令。 |
| `evaluation failed; keeping previous environment` | RC 在 evaluator 子进程抛了终止错误。常见：`dotenv` 指定的文件不存在（改用 `dotenv_if_exists`）、`source_env` 路径错、RC 语法错误。 |
| 进入新终端首次进项目没加载 | 多 shell 共享 cache：新 shell 首次进入应由模块传 `--force`。若手动调 `pwdir export` 调试，记得加 `--force`。 |
| PowerShell 5.1 下不可用 | 明确不支持。安装 PowerShell 7（`pwsh`），`doctor` 会对 <7.2 报 Warn/Fail。 |
| `[FAIL] localappdata` / 数据目录不可写 | `%LOCALAPPDATA%` 指向异常位置或权限问题；`pwdir status --json` 的 `dirs` 字段查看实际路径。 |
| ExecutionPolicy 警告 | evaluator 以 `-ExecutionPolicy Bypass` 仅作用于自身进程，一般可忽略；企业策略阻止 Bypass 时 doctor 会体现。 |
| 路径含空格/引号/Unicode/方括号出问题 | 均为受支持场景（有 E2E 覆盖）；手工 `pwdir exec` 时注意参数由 OS argv 传递，不要自己拼字符串。 |

## 诊断命令顺序

```powershell
pwdir doctor                 # 安装/环境健康度
pwdir status --json          # 当前目录发现的 RC、trust、cache 状态
Get-PwDirEnvStatus           # Host 模块侧状态（Enabled/ActiveRc/ManagedKeys）
pwdir reload                 # 强制下次重评估
```

## 重置

```powershell
Disable-PwDirEnv             # 恢复当前会话环境与原 prompt
pwdir deny                   # 撤销当前目录 RC 授权
Remove-Item -Recurse -Force (Join-Path $env:LOCALAPPDATA 'PwDirEnv')   # 全量重置 trust/cache/state
```

## 不会发生 / 不要尝试的事

- RC 里的函数、alias、`Import-Module`、prompt 修改**不会**进入用户 shell（隔离设计）；需要生效的只有环境变量。
- PwDirEnv 不修改 User/Machine 环境变量；如果发现系统级变量被改，那是 RC 自己显式调用了系统 API（应在审阅时拦截）。
- 日志和 `status` 默认不输出环境变量值（防 secrets 泄漏）；不要为了调试把变量值写进日志或提交物。
