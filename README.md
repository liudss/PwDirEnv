# PwDirEnv

面向 PowerShell 7+ / Windows 的 directory environment manager（direnv 等价实现）。

核心原则（SPEC §0）：**`.envrc.ps1` 在隔离子进程中执行，仅把 env diff 应用到 Host 会话**——Host Shell 永不直接执行项目脚本，只接收结构化的环境变量 patch，保证函数、alias、prompt 等状态不受污染。

## 目录结构

```text
PwDirEnv/
├─ Cargo.toml                  # Rust workspace
├─ crates/
│  ├─ pwdir-protocol/          # 纯数据结构 + JSON schema v1（无 IO）
│  ├─ pwdir-core/              # discovery / trust / diff / fingerprint / evaluator launcher
│  └─ pwdir-cli/               # pwdir.exe 二进制
├─ powershell/
│  └─ PwDirEnv/                # Host Shell 侧 PowerShell module
├─ evaluator/                  # 隔离 evaluator（bootstrap + stdlib）
├─ tests/
│  ├─ integration/
│  └─ fixtures/
├─ docs/
│  └─ adr/                     # 架构决策记录
└─ .github/workflows/ci.yml    # Windows CI
```

## 开发命令

```powershell
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all

# PowerShell module 冒烟
pwsh -NoProfile -File tests/smoke.ps1
```

## 发现与信任（M1）

- **发现**：`pwdir` 从当前目录向上查找**最近**的一份 `.envrc.ps1`（不隐式叠加父级 RC，需要父级配置时用 `source_up`）。目录遍历在盘符根 / UNC 根正确终止；当前目录被删除时不 panic。
- **信任**：`pwdir allow [path]` 读取 RC 内容并计算 SHA-256，写入 `%LOCALAPPDATA%\PwDirEnv\allow\`。record 文件名 = canonical 路径的 SHA-256（规避文件名编码/长度问题），写入采用「临时文件 + rename」原子替换，并发安全。
- **失效**：trust 基于内容 hash 而非 mtime（ADR-005）——RC 改动任意一个 byte 即重新 blocked，必须重新 `allow`。
- **撤销**：`pwdir deny [path]` 幂等删除 record；无 RC 时输出 `nothing to deny`。
- **状态**：`pwdir status [--json]` 显示 cwd、发现的 RC、trust 状态（`allowed` / `blocked` / `none`）、RC 当前内容的 SHA-256 与存储目录（allow/cache/state）。`--json` 输出稳定 schema（顶层 `cwd` / `discovered_rc` / `trust_status` / `rc_sha256` / `dirs`）。
- **退出码**（SPEC §14.1）：`0` 成功；`2` 找不到 RC；`3` IO error；`1` 其它错误。

手动 E2E 演示（建临时 RC → allow → 改 byte → blocked → deny）：

```powershell
pwsh -NoProfile -File tests/e2e_trust.ps1
```

## Evaluator 隔离执行模型（M2）

- **隔离**：`.envrc.ps1` 永不在 Host Shell 执行。每次评估 spawn
  `pwsh -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File bootstrap.ps1`，子进程内严格按
  SPEC §17 十步序列运行：初始化 `$script:__PwDirEnvState` → 加载 stdlib → `Set-Location` 项目根 → dot-source RC →
  枚举 Process environment。bootstrap 与 stdlib 在编译期嵌入二进制，运行时解包到一次性 temp 目录（用后清理），
  路径经 argv 传递，空格 / Unicode / 特殊字符天然安全。
- **结果通道**：结构化结果只写入 native core 分配的结果文件（同目录随机名 `.tmp` + `Move` 原子替换，UTF-8 无 BOM）。
  RC 的 `Write-Output` / `Write-Host` / `Write-Warning` 全部留在 stdout/stderr，绝不污染协议（SPEC §7.5）；
  native core 用两个独立线程并发排空 stdout/stderr，避免管道写满导致假超时/死锁。
- **超时**：默认 10s（`EvaluatorConfig.timeout`，SPEC §7.6）。到点后 `taskkill /PID <pid> /T /F` 杀整棵进程树
  （RC 可能启动了自己的子进程，仅杀 pwsh 会留孤儿），报告 `timed_out=true` 且不产出 result。
- **错误语义**：RC 抛出终止错误时，bootstrap 写 `{ok:false, error:{kind, message, exception, stack}}` 并 exit 1；
  结果文件缺失时（崩溃 / RC 自行 exit），native core 依据 exit code 与捕获的 stderr 构造 `no-result` 兜底错误。
  RC 中定义同名函数也不会劫持结果写入通道（bootstrap 预先捕获写入函数引用）。
- **实现备注**：SPEC §7.4 的 `. -LiteralPath $RcPath` 语法 pwsh 实际不接受（dot 运算符无命名参数），
  bootstrap 以 `Get-Item -LiteralPath` 做字面量解析后 dot-source 其 FullName —— 语义等价且保留 `$PSScriptRoot`。

## 环境 Diff 与 export 协议（M3）

`pwdir export --format json` 是 PowerShell module 与 native core 的核心协议入口（SPEC §14.7）：
输入隐式为**当前 process environment + cwd**，输出环境 patch 与元数据。

- **baseline**：export 时点 pwdir.exe 进程继承的环境 —— 它就是 Host pwsh 的当前环境，与 evaluator 继承的一致
  （SPEC §8.1/§7.3），diff 即 `baseline → desired`。
- **大小写不敏感 key**（SPEC §8.2）：Windows 环境变量名大小写不敏感，`PATH`/`Path`/`path` 是同一 key。
  内部以 ASCII invariant uppercase（canonical）归并比较；输出保留 display casing——
  `set` 的 key 用 desired 侧的大小写，`unset` 用 baseline 侧的大小写。
- **action 语义**：
  - `noop`：无 RC、或 RC 评估成功但 diff 为空（module 自行决定是否 unload 旧环境）；
  - `apply`：patch 非空，module 按 `patch.set` / `patch.unset` 应用到 Process scope；
  - `blocked`：RC 未授权或内容已变（trust 基于内容 hash）。**patch 恒为空，RC 内容绝不执行**；
  - `error`：RC 抛错/超时，diagnostics 携带结构化错误，patch 恒为空；
  - `unload`：预留给 module 侧离开项目的语义。
- **协议语义 ≠ CLI 失败**：`blocked` / `error` 时 `pwdir export` 仍输出完整 JSON 并 exit 0；
  只有基础设施故障（pwsh 缺失、IO 错误等）才以非 0 退出码失败。
- **managed_keys**：`patch.set` 的 key ∪ `patch.unset`（排序去重）—— PwDirEnv "owns" 的变量集合，
  module 离开项目时据此恢复 baseline（SPEC §8.4/§8.5）。
- **PATH 空值三态**（SPEC §9.4）：变量不存在 / `PATH=''` / 非空是三个不同状态。
  空串是"存在但空"：baseline 无 PATH + desired `PATH=''` → set 空串；
  baseline `PATH=''` + desired `'abc'` → set `'abc'`；baseline 有（含空串）+ desired 无 → unset。

## Host Module 与 Hook（M4）

`powershell/PwDirEnv` 是 Host Shell 侧的唯一入口（SPEC §15）：

```powershell
Import-Module PwDirEnv            # 或显式 -Path powershell\PwDirEnv\PwDirEnv.psd1
Enable-PwDirEnv [-PwDirPath <pwdir.exe>]
# ... 正常工作，hook 在每次 prompt 渲染前自动执行
Disable-PwDirEnv                  # 完整恢复现场
```

- **安装 hook**：`Enable-PwDirEnv` 保存用户现有 `prompt` 的 ScriptBlock（须定义在
  global 作用域，即 `$PROFILE` —— 模块按自身作用域链查找，看不到脚本局部作用域的 prompt）
  并安装 wrapper：先执行 `Invoke-PwDirEnvHook`，再回调原 prompt。`Disable-PwDirEnv`
  恢复原 prompt（无原 prompt 时删除 wrapper）。幂等。
- **加载与卸载**：hook 每次 prompt 都调用 `pwdir export --format json`（评估发生在 pwdir.exe
  的隔离子进程，Host 从不下钻执行 RC），只把返回的结构化 patch 逐 key 应用到
  **Process scope**（`[Environment]::SetEnvironmentVariable`，**禁止 Invoke-Expression**，
  SPEC §15.1/§27），绝不触碰 User/Machine（ADR-006）。
- **恢复模型**（SPEC §8.4/§8.5）：Host 记录每个 managed key 应用前的原值
  （`Existed`/`Value`，不用哨兵值）。切换 RC A→B 时先按 A 的 baseline 恢复到干净
  Host 环境，再从该干净环境重新 export B 再应用；离开项目（进入无 RC 目录、cwd 不可用、
  Disable）时按 baseline 恢复——用户对 managed key 的手动修改一并丢弃（PwDirEnv owns it）。
- **错误语义**：hook 永不抛错（catch 一切，SPEC §13.4）。同项目内 RC 评估失败/超时保持
  last-known-good；RC blocked（内容 hash 变化）保持 last-known-good 并提示 `pwdir allow`；
  切换到新项目失败则旧环境已恢复、保持干净 baseline。
- **日志**（SPEC §19）：`pwdir: loading/unloading <rc>`、`pwdir: export +NEW ~CHANGED -GONE`
  （+ 新增 / ~ 修改 / - 删除，对照 baseline，值绝不打印）。error/warn 带颜色。
- **状态**：`Get-PwDirEnvStatus` 返回 `Version / Enabled / PwDirExe / ActiveRc /
  ManagedKeys / HookRunning / BaselineCount / Fingerprint`（M4 恒为 `$null`，M6 启用）。

真实验收（A→child→B→outside 恢复 + blocked + prompt 保留，release blocker）：

```powershell
pwsh -NoProfile -File tests/e2e_hook.ps1
```

## Evaluator stdlib helpers（M5）

`evaluator/stdlib.ps1` 在 RC dot-source 之前由 bootstrap 注入 evaluator（SPEC §10）。helper 直接读写
evaluator 进程环境（`$env:` / `$script:__PwDirEnvState`），其 effect 随 evaluator 结束被 core 捕获进
desired environment。**所有相对路径一律以 project root 为基准解析**（而非 `$PWD` —— RC 可能再
`Set-Location`，语义必须稳定指向项目根），且允许路径尚不存在（不做 `Resolve-Path`）。

| Helper | 语义 |
|---|---|
| `PATH_add <path>` | 相对路径解析到 project root 后 **prepend** 到 `$env:PATH`；等价项（Windows 语义：大小写不敏感、`/`→`\`、尾分隔符无关，纯字符串比较）去重后移到最前；PATH 缺失 / 为空视为空 PATH。 |
| `dotenv [path]` | 解析 `.env`（默认）并设置变量；文件不存在 → **throw，evaluation 失败**。 |
| `dotenv_if_exists [path]` | 同 `dotenv`，但文件不存在时静默跳过。 |
| `dotenv -Overload` | dotenv / dotenv_if_exists 均支持：强制覆盖已存在的环境变量。 |
| `watch_file <path> [...]` | 注册 file watch 依赖（支持多参数；相同 kind + 等价路径去重）。 |
| `watch_dir <path> [...]` | 注册 dir watch 依赖（同上）。 |
| `source_env <path>` | 在 evaluator 当前 scope dot-source 指定 `.ps1`（`$PSScriptRoot` 语义保留）；不存在 → throw；成功后自动纳入 `watches` 与 `sourced_files`（fingerprint 元数据，SPEC §10.1）。 |
| `source_up [filename]` | 从 project root 的**父目录**向上搜索最近的 `<filename>`（默认 `.envrc.ps1`，不含 project root 自身）；找到即 `source_env`，未找到为 no-op。 |

**dotenv 语法子集**（SPEC §11 最小语义）：UTF-8（BOM 自动剥离）、CRLF/LF、空行与 `#` 注释行忽略、
`export ` 前缀剥离（只认小写）、`KEY=value` / `KEY="value"`（最小 escape：`\"` `\\` `\n` `\r` `\t`，
其余 `\x` 保留为 `\` + 字面量）/ `KEY='value'`（全字面量）/ 无引号（`=` 后剩余文本，前后空白 trim）。
key 不合规或无 `=` 的行静默跳过；**不做** command substitution 与 `${VAR}` 展开；不支持行内 `#`
注释与跨行值。**默认不覆盖已存在的环境变量**（direnv 兼容），`-Overload` 强制覆盖。

> ADR-007：dotenv 解析器用 PowerShell 实现（`Read-PwDirDotEnv`），是对 SPEC §11「优先 Rust
> parser」的有意偏离 —— helper 与进程 env 在同一 evaluator 内，走 native parser 需要跨进程
> 回传 KV 且 dotenv 调用低频，复杂度不划算。

## MVP 里程碑状态

| 里程碑 | 内容 | 状态 |
|---|---|---|
| M0 | Scaffold（workspace / protocol schema / CLI 骨架 / PS module / ADR / CI） | ✅ |
| M1 | Discovery + Trust（向上发现、SHA-256 trust store、allow/deny/status） | ✅ |
| M2 | Evaluator（spawn pwsh、bootstrap、env capture、timeout） | ✅ |
| M3 | Diff + Export（大小写不敏感 diff、`pwdir export --format json`） | ✅ |
| M4 | PowerShell Module + Hook（Enable/Disable/hook、apply/restore） | ✅ |
| M5 | stdlib（PATH_add / dotenv / watch_file / source_env / source_up） | ✅ |
| M6 | Cache + Reload（fingerprint 快速路径） | ✅ |
| M7 | exec + doctor + 打包 | ✅ |

## Cache 与快速路径（M6）

每次 hook，module 都调用 `pwdir export`（native 进程，不 spawn pwsh）；core 依据磁盘 cache
判定指纹是否未变（SPEC §12/§22）：

- **快速路径**：RC 未变且未传 `--force` 时，若磁盘 cache（`%LOCALAPPDATA%\PwDirEnv\cache\`）
  中的指纹仍与当前现实一致 → 直接返回 `Noop`（`patch` 为空、`fingerprint` 携带缓存指纹、
  diagnostics 含 `fingerprint unchanged (fast path)`），**不 spawn evaluator**。
  实测稳态单次 export ≈ 20-30 ms（含 pwdir.exe 进程启动），evaluator 冷启动约 500-600 ms。
- **指纹组成**（`Fingerprint`）：RC 内容 SHA-256（内容级，与 trust 同源，不依赖 mtime）；
  `source_env`/`source_up` 的 sourced 文件 size+mtime；`watch_file`/`watch_dir` 注册的
  watch 目标 size+mtime（`watch_states`，与 `watches` 按下标对齐）；嵌入 bootstrap/stdlib
  的内容 SHA-256（脚本行为变化即全量失效）。任一项状态获取失败（文件消失等）一律按
  「已变化」降级为重新评估 —— 宁可多评估一次，绝不漏掉一次变化。
- **force 语义**：`pwdir export --force` 跳过快速路径强制评估。cache 是跨会话共享的，
  module 在「rc 与 `ActiveRc` 不一致」（新 shell 首次进入 / 切换目录）时必须传 force，
  否则 core 会命中上一个会话留下的有效 cache 返回空 patch 而漏掉加载。
- **reload**：`pwdir reload` 使当前 RC 的 cache 失效（幂等删除），下一次 export 强制重评估；
  找不到 RC 时输出 `nothing to reload` 并成功退出。
- **cache 不存环境变量值**（SPEC §12.4 的保守选择，ADR 取舍）：条目只含 schema、rc 路径、
  指纹与评估时间戳。环境变量值可能含 secrets，持久化到磁盘会扩大泄漏面；新 shell 首次
  进入由 module 传 force 强制重评估，天然拿到完整环境。写入采用「临时文件 + rename」
  原子替换（与 trust store 同源），损坏 / 未识别 schema 的条目按未命中安全降级。
- **状态**：`pwdir status [--json]` 新增 `cache_status`（`none` = 无条目；`fresh` = 指纹与
  当前现实一致；`stale` = 有条目但已失效）。status 只读呈现，不评估、不写 cache。

## exec / doctor / hook install / packaging（M7）

### `pwdir exec <DIR> <COMMAND> [ARGS...]`（SPEC §14.5）

以解析后的项目环境启动任意子进程：

- **trust 边界**：RC 未 allow → `rc file is not allowed: run \`pwdir allow\``（exit 1），
  **RC 内容一行都不执行**，COMMAND 也不会被 spawn；
- **argv-safe**：`Command::new(command).args(args).current_dir(dir)` 直接走 OS 进程模型，
  绝不经 shell 字符串拼接（SPEC §27），参数中的空格 / 引号 / `$` 天然安全；
- **环境注入**：diff（pwdir 进程环境 → evaluator resulting environment）通过
  `Command::env` / `env_remove` 叠加到子进程继承环境上，User/Machine scope 永不触碰；
- **超时**：evaluator 用 exec 专用 60s 超时（SPEC §7.6 "exec 可允许更长"），hook 的 10s
  默认值不受影响；COMMAND 本身不设超时（长任务属预期）；
- **退出码**：COMMAND 的 exit code 原样传播（`pwdir exec` 以它退出）；stdout/stderr
  双管道并发排空后字节级透传；`--require-rc` 在无 RC 时报 `RcNotFound`（exit 2），
  默认无 RC 时以继承环境直接执行；
- **错误映射**：RcNotFound→2、Io→3、NotAllowed→1、EvaluationFailed→1；
  COMMAND 不存在时给出友好提示（绝对路径 / PATH）。

### `pwdir doctor [--json]`（SPEC §14.8）

逐项自检，单项失败不中断；人类输出 `[OK]/[WARN]/[FAIL] name: message`，`--json` 输出
`{ "checks":[{name,status,message}], "all_ok": bool }`。存在 `fail` 项 → exit 1。

| 检查项 | 内容 |
|---|---|
| `os` | OS + ARCH，Windows 上 best-effort 追加 registry DisplayVersion |
| `pwsh` / `powershell-version` | pwsh 可用性；主版本 <7 Fail（5.1 不支持），7.x <7.2 Warn（目标 ≥ 7.2） |
| `module` | 相对 current_exe 依次探测 `modules\PwDirEnv\PwDirEnv.psd1`（平铺布局）、`..\modules\PwDirEnv\PwDirEnv.psd1`（packaging 标准布局，bin 与 modules 同级）或 `..\..\powershell\PwDirEnv\PwDirEnv.psd1`（开发布局）；缺失 Warn |
| `profile-hook` | `$PROFILE.CurrentUserAllHosts` 存在且含 PwDirEnv marker → Ok，否则 Warn 并提示 `pwdir hook install` |
| `localappdata` | 数据根 allow/cache/state 逐一写删探针，任一不可写 Fail |
| `temp-dir` | `%TEMP%` 可写性（evaluator 结果通道依赖） |
| `execution-policy` | Process scope 策略；Restricted/AllSigned Warn（evaluator 以 `-ExecutionPolicy Bypass` 仅作用于自身进程，SPEC §7.2） |
| `long-path` | registry `LongPathsEnabled`：1 Ok / 0 Warn |

### `pwdir hook pwsh` / `install` / `uninstall`（SPEC §3.1/§14.6）

- `pwdir hook pwsh`：输出两行极小 bootstrap（`Import-Module '<psd1 绝对路径>'` +
  `Enable-PwDirEnv`）；模块缺失时输出占位符片段并打 Warn；
- `pwdir hook install [--yes] [--path <file>]`：往目标 profile 写入
  `# >>> PwDirEnv (begin) >>>` … `# <<< PwDirEnv (end) <<<` marker 块（内含上述两行）。
  默认目标 = `$PROFILE.CurrentUserAllHosts`（对 pwsh 全部 host 生效），`--path` 覆盖；
  文件不存在则创建（UTF-8 无 BOM，行结束符沿用文件现有 CRLF/LF 风格）；
  已含 marker → `already installed` 幂等；未给 `--yes` 且 stdin 是 tty 时先确认，
  非交互直接写；文件 IO 错误 exit 3；
- `pwdir hook uninstall [--path <file>]`：按行移除 marker 块（块外内容原样保留）；
  无 marker → `nothing to uninstall`。

### packaging（M7）

```powershell
pwsh -NoProfile -File packaging\install.ps1            # 构建 + 安装到 %LOCALAPPDATA%\Programs\PwDirEnv
pwsh -NoProfile -File packaging\install.ps1 -SkipBuild # 复用现有 target\release 产物
```

脚本组装并校验 `dist\pwdir` 标准布局（`bin\pwdir.exe` + `modules\PwDirEnv\{psd1,psm1}`），
拷贝到 InstallRoot 后打印后续步骤（① bin 加 PATH 或设 `PWDIRENV_EXE`；② Import-Module；
③ `pwdir hook install --yes`）—— **不自动修改系统/用户 PATH**（最小侵入）。
winget manifest 占位见 `packaging/winget/`（正式发布流程不在仓库范围）。
