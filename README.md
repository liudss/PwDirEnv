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

## MVP 里程碑状态

| 里程碑 | 内容 | 状态 |
|---|---|---|
| M0 | Scaffold（workspace / protocol schema / CLI 骨架 / PS module / ADR / CI） | ✅ |
| M1 | Discovery + Trust（向上发现、SHA-256 trust store、allow/deny/status） | pending |
| M2 | Evaluator（spawn pwsh、bootstrap、env capture、timeout） | pending |
| M3 | Diff + Export（大小写不敏感 diff、`pwdir export --format json`） | pending |
| M4 | PowerShell Module + Hook（Enable/Disable/hook、apply/restore） | pending |
| M5 | stdlib（PATH_add / dotenv / watch_file / source_env / source_up） | pending |
| M6 | Cache + Reload（fingerprint 快速路径） | pending |
| M7 | exec + doctor + 打包 | pending |
