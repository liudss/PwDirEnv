//! M6 cache / fingerprint 快速路径集成测试（真 pwsh evaluator + 临时 allow/cache 目录）。
//!
//! 核心断言手段：rc 每次被执行都以 `[DateTime]::UtcNow.Ticks` 重写一个 marker 文件。
//! marker 未被重写（ticks 不变）⇒ evaluator 未被 spawn；被重写 ⇒ 发生了重新评估。

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use pwdir_core::cache::CacheStore;
use pwdir_core::evaluator::EvaluatorConfig;
use pwdir_core::export::export_with_store;
use pwdir_core::trust::TrustStore;
use pwdir_protocol::{Action, RC_FILENAME};

static PWSH_AVAILABLE: OnceLock<bool> = OnceLock::new();

fn ensure_pwsh() {
    let ok = *PWSH_AVAILABLE.get_or_init(|| {
        Command::new("pwsh")
            .arg("-NoProfile")
            .arg("-Command")
            .arg("$PSVersionTable.PSVersion")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    });
    assert!(
        ok,
        "pwsh (PowerShell 7+) was not found on PATH; it is required for cache integration tests"
    );
}

/// 测试夹具：临时项目目录（含 rc + marker + 附加文件）+ 注入的 trust/cache store。
/// 每个测试独占一套 TempDir，环境变量名唯一，互不干扰。
struct Fixture {
    _root: tempfile::TempDir,
    project: PathBuf,
    rc: PathBuf,
    marker: PathBuf,
    store: TrustStore,
    cache: CacheStore,
}

impl Fixture {
    fn new(unique_env_suffix: &str) -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let project = root.path().join("proj");
        fs::create_dir_all(&project).expect("mkdir project");
        let rc = project.join(RC_FILENAME);
        let marker = root.path().join(format!("marker-{unique_env_suffix}.txt"));
        // rc 每次执行都重写 marker（UTC ticks 单调递增）→ 可判定 evaluator 是否运行
        let body = format!(
            "Set-Content -LiteralPath '{}' -Value ([DateTime]::UtcNow.Ticks)\n$env:PWDIR_IT_{unique_env_suffix} = 'on'\n",
            marker.display()
        );
        fs::write(&rc, &body).expect("write rc");

        let store = TrustStore::new(root.path().join("allow"));
        let cache = CacheStore::new(root.path().join("cache"));
        Self {
            _root: root,
            project,
            rc,
            marker,
            store,
            cache,
        }
    }

    fn export(&self, force: bool) -> pwdir_protocol::ExportOutput {
        export_with_store(
            &self.project,
            &EvaluatorConfig::default(),
            &self.store,
            &self.cache,
            force,
        )
        .expect("export ok")
    }

    fn marker_ticks(&self) -> String {
        fs::read_to_string(&self.marker)
            .expect("marker must exist after an evaluation")
            .trim()
            .to_string()
    }
}

/// 1. 快速路径稳态：指纹未变时第二次 export 不 spawn evaluator（marker 未重写）。
#[test]
fn fast_path_does_not_spawn_evaluator() {
    ensure_pwsh();
    let fx = Fixture::new("fast");
    fx.store.allow(&fx.rc).expect("allow");

    let first = fx.export(false);
    assert_eq!(first.action, Action::Apply, "first export must evaluate");
    let ticks_1 = fx.marker_ticks();

    let second = fx.export(false);
    assert_eq!(
        second.action,
        Action::Noop,
        "unchanged fingerprint must noop"
    );
    assert!(second.patch.is_empty(), "fast path must not carry a patch");
    assert!(second.fingerprint.is_some());
    assert!(
        second
            .diagnostics
            .iter()
            .any(|d| d.message == "fingerprint unchanged (fast path)"),
        "expected fast path diagnostic: {:?}",
        second.diagnostics
    );
    assert_eq!(
        fx.marker_ticks(),
        ticks_1,
        "evaluator must not have re-run on the fast path"
    );
}

/// 2. watch 变化触发重评估：rc `watch_file 'package.json'`，改其内容后必须重新 evaluate。
#[test]
fn watch_change_triggers_reevaluation() {
    ensure_pwsh();
    let fx = Fixture::new("watch");
    let watch_target = fx.project.join("package.json");
    fs::write(&watch_target, "{\"version\": 1}").expect("write watch target");
    // 在 rc 末尾追加 watch 注册（保持 marker 行为不变）
    let mut body = fs::read_to_string(&fx.rc).expect("read rc");
    body.push_str("watch_file 'package.json'\n");
    fs::write(&fx.rc, &body).expect("append watch_file");
    fx.store.allow(&fx.rc).expect("allow");

    let first = fx.export(false);
    assert_eq!(first.action, Action::Apply);
    let ticks_1 = fx.marker_ticks();
    // 确认 cache 已落盘且 watch 状态被记录
    let entry = fx.cache.load(&fx.rc).expect("load").expect("cache entry");
    assert_eq!(entry.fingerprint.watches.len(), 1);
    assert_eq!(entry.fingerprint.watch_states.len(), 1);

    // Windows mtime 有文件系统粒度：sleep 后改写内容（size+mtime 双变化，确定性判变）
    std::thread::sleep(Duration::from_millis(60));
    fs::write(&watch_target, "{\"version\": 2}").expect("modify watch target");

    let second = fx.export(false);
    assert_eq!(
        second.action,
        Action::Apply,
        "watch change must re-evaluate"
    );
    assert_ne!(
        fx.marker_ticks(),
        ticks_1,
        "rc must have been re-executed after the watch changed"
    );
}

/// 3. rc 内容变化触发重评估：改一个 byte → trust 失效（blocked）→ 重新 allow → 重评估。
///    （trust 是 cache 的前置关卡：内容变化必须先重新授权才能到达 cache 判变层，
///    与 e2e 的 blocked → re-allow → reload 流程一致。）
#[test]
fn rc_change_triggers_reevaluation() {
    ensure_pwsh();
    let fx = Fixture::new("rcchange");
    fx.store.allow(&fx.rc).expect("allow");

    let first = fx.export(false);
    assert_eq!(first.action, Action::Apply);
    let ticks_1 = fx.marker_ticks();

    // 改一个 byte：'on' → 'oo'
    let body = fs::read_to_string(&fx.rc).expect("read rc");
    let changed = body.replacen("'on'", "'oo'", 1);
    assert_ne!(body, changed, "fixture must actually change one byte");
    fs::write(&fx.rc, &changed).expect("rewrite rc");

    // trust 先失效：blocked，绝不执行（安全边界）
    let blocked = fx.export(false);
    assert_eq!(blocked.action, Action::Blocked);

    // 重新 allow 后：cache 指纹的 rc_sha256 失配 → 强制重评估
    fx.store.allow(&fx.rc).expect("re-allow");
    let second = fx.export(false);
    assert_eq!(second.action, Action::Apply, "rc change must re-evaluate");
    assert_ne!(fx.marker_ticks(), ticks_1, "rc must have been re-executed");
}

/// 4. force 语义：cache 有效（指纹未变）时 force=true 仍强制重评估。
#[test]
fn force_reevaluates_even_when_unchanged() {
    ensure_pwsh();
    let fx = Fixture::new("force");
    fx.store.allow(&fx.rc).expect("allow");

    let first = fx.export(false);
    assert_eq!(first.action, Action::Apply);
    let ticks_1 = fx.marker_ticks();

    // 不 force：快速路径 noop（对照）
    let steady = fx.export(false);
    assert_eq!(steady.action, Action::Noop);
    assert_eq!(fx.marker_ticks(), ticks_1);

    // force：跳过快速路径，marker 被重写
    let forced = fx.export(true);
    assert_eq!(forced.action, Action::Apply, "force must re-evaluate");
    assert_ne!(
        fx.marker_ticks(),
        ticks_1,
        "force must re-execute the rc even when the fingerprint is unchanged"
    );
}

/// 5. reload 语义：cache 失效（invalidate）后，下一次不 force 的 export 强制重评估。
#[test]
fn reload_invalidates_and_reevaluates() {
    ensure_pwsh();
    let fx = Fixture::new("reload");
    fx.store.allow(&fx.rc).expect("allow");

    let first = fx.export(false);
    assert_eq!(first.action, Action::Apply);
    let ticks_1 = fx.marker_ticks();
    assert!(
        fx.cache.load(&fx.rc).expect("load").is_some(),
        "cache saved"
    );

    // `pwdir reload` 的 core 语义：使当前 rc 的 cache 失效
    fx.cache.invalidate(&fx.rc).expect("invalidate");
    assert!(fx.cache.load(&fx.rc).expect("load").is_none());

    let second = fx.export(false);
    assert_eq!(
        second.action,
        Action::Apply,
        "reload must force re-evaluation"
    );
    assert_ne!(fx.marker_ticks(), ticks_1, "rc must have been re-executed");
    assert!(
        fx.cache.load(&fx.rc).expect("load").is_some(),
        "cache must be rebuilt after re-evaluation"
    );
}
