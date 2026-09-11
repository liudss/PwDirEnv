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

#[test]
fn watch_change_triggers_reevaluation() {
    ensure_pwsh();
    let fx = Fixture::new("watch");
    let watch_target = fx.project.join("package.json");
    fs::write(&watch_target, "{\"version\": 1}").expect("write watch target");
    let mut body = fs::read_to_string(&fx.rc).expect("read rc");
    body.push_str("watch_file 'package.json'\n");
    fs::write(&fx.rc, &body).expect("append watch_file");
    fx.store.allow(&fx.rc).expect("allow");

    let first = fx.export(false);
    assert_eq!(first.action, Action::Apply);
    let ticks_1 = fx.marker_ticks();
    let entry = fx.cache.load(&fx.rc).expect("load").expect("cache entry");
    assert_eq!(entry.fingerprint.watches.len(), 1);
    assert_eq!(entry.fingerprint.watch_states.len(), 1);

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

#[test]
fn rc_change_triggers_reevaluation() {
    ensure_pwsh();
    let fx = Fixture::new("rcchange");
    fx.store.allow(&fx.rc).expect("allow");

    let first = fx.export(false);
    assert_eq!(first.action, Action::Apply);
    let ticks_1 = fx.marker_ticks();

    let body = fs::read_to_string(&fx.rc).expect("read rc");
    let changed = body.replacen("'on'", "'oo'", 1);
    assert_ne!(body, changed, "fixture must actually change one byte");
    fs::write(&fx.rc, &changed).expect("rewrite rc");

    let blocked = fx.export(false);
    assert_eq!(blocked.action, Action::Blocked);

    fx.store.allow(&fx.rc).expect("re-allow");
    let second = fx.export(false);
    assert_eq!(second.action, Action::Apply, "rc change must re-evaluate");
    assert_ne!(fx.marker_ticks(), ticks_1, "rc must have been re-executed");
}

#[test]
fn force_reevaluates_even_when_unchanged() {
    ensure_pwsh();
    let fx = Fixture::new("force");
    fx.store.allow(&fx.rc).expect("allow");

    let first = fx.export(false);
    assert_eq!(first.action, Action::Apply);
    let ticks_1 = fx.marker_ticks();

    let steady = fx.export(false);
    assert_eq!(steady.action, Action::Noop);
    assert_eq!(fx.marker_ticks(), ticks_1);

    let forced = fx.export(true);
    assert_eq!(forced.action, Action::Apply, "force must re-evaluate");
    assert_ne!(
        fx.marker_ticks(),
        ticks_1,
        "force must re-execute the rc even when the fingerprint is unchanged"
    );
}

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
