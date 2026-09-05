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

## MVP 里程碑状态

| 里程碑 | 内容 | 状态 |
|---|---|---|
| M0 | Scaffold（workspace / protocol schema / CLI 骨架 / PS module / ADR / CI） | ✅ |
| M1 | Discovery + Trust（向上发现、SHA-256 trust store、allow/deny/status） | ✅ |
| M2 | Evaluator（spawn pwsh、bootstrap、env capture、timeout） | ✅ |
| M3 | Diff + Export（大小写不敏感 diff、`pwdir export --format json`） | ✅ |
| M4 | PowerShell Module + Hook（Enable/Disable/hook、apply/restore） | ✅ |
| M5 | stdlib（PATH_add / dotenv / watch_file / source_env / source_up） | pending |
| M6 | Cache + Reload（fingerprint 快速路径） | pending |
| M7 | exec + doctor + 打包 | pending |
