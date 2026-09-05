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

## MVP 里程碑状态

| 里程碑 | 内容 | 状态 |
|---|---|---|
| M0 | Scaffold（workspace / protocol schema / CLI 骨架 / PS module / ADR / CI） | ✅ |
| M1 | Discovery + Trust（向上发现、SHA-256 trust store、allow/deny/status） | ✅ |
| M2 | Evaluator（spawn pwsh、bootstrap、env capture、timeout） | pending |
| M3 | Diff + Export（大小写不敏感 diff、`pwdir export --format json`） | pending |
| M4 | PowerShell Module + Hook（Enable/Disable/hook、apply/restore） | pending |
| M5 | stdlib（PATH_add / dotenv / watch_file / source_env / source_up） | pending |
| M6 | Cache + Reload（fingerprint 快速路径） | pending |
| M7 | exec + doctor + 打包 | pending |
