//! Environment differences using ASCII case-insensitive names. Preserve the distinction between absent and empty values.

use std::collections::BTreeMap;
use std::ffi::OsString;

use pwdir_protocol::{EnvPatch, EnvironmentSnapshot};

fn index_by_canonical(snapshot: &EnvironmentSnapshot) -> BTreeMap<String, (String, OsString)> {
    let mut index = BTreeMap::new();
    for (key, value) in &snapshot.vars {
        // Rust exposes hidden drive entries such as =C:, but .NET filters them.
        // These are not legal names for host SetEnvironmentVariable calls.
        if key.display.contains('=') {
            continue;
        }
        index.insert(key.canonical.clone(), (key.display.clone(), value.clone()));
    }
    index
}

pub fn diff_env(baseline: &EnvironmentSnapshot, desired: &EnvironmentSnapshot) -> EnvPatch {
    let base = index_by_canonical(baseline);
    let want = index_by_canonical(desired);

    let mut patch = EnvPatch::default();

    for (canonical, (display, value)) in &want {
        match base.get(canonical) {
            None => {
                patch
                    .set
                    .insert(display.clone(), value.to_string_lossy().into_owned());
            }
            Some((_, old)) if old != value => {
                patch
                    .set
                    .insert(display.clone(), value.to_string_lossy().into_owned());
            }
            Some(_) => {}
        }
    }

    for (canonical, (display, _)) in &base {
        if !want.contains_key(canonical) {
            patch.unset.insert(display.clone());
        }
    }

    patch
}

#[cfg(test)]
mod tests {
    use super::*;
    use pwdir_protocol::EnvKey;

    fn snap(pairs: &[(&str, &str)]) -> EnvironmentSnapshot {
        EnvironmentSnapshot {
            vars: pairs
                .iter()
                .map(|(k, v)| (EnvKey::from_name(k), OsString::from(*v)))
                .collect(),
        }
    }

    #[test]
    fn new_var_in_desired_becomes_set() {
        let baseline = snap(&[("A", "1")]);
        let desired = snap(&[("A", "1"), ("NEW_VAR", "fresh")]);

        let patch = diff_env(&baseline, &desired);
        assert_eq!(patch.set.get("NEW_VAR").map(String::as_str), Some("fresh"));
        assert!(patch.unset.is_empty());
    }

    #[test]
    fn changed_value_becomes_set_with_new_value() {
        let baseline = snap(&[("MODE", "debug")]);
        let desired = snap(&[("MODE", "release")]);

        let patch = diff_env(&baseline, &desired);
        assert_eq!(patch.set.get("MODE").map(String::as_str), Some("release"));
        assert!(patch.unset.is_empty());
    }

    #[test]
    fn removed_var_becomes_unset() {
        let baseline = snap(&[("OLD", "1"), ("KEEP", "x")]);
        let desired = snap(&[("KEEP", "x")]);

        let patch = diff_env(&baseline, &desired);
        assert!(patch.set.is_empty());
        assert_eq!(patch.unset.iter().collect::<Vec<_>>(), ["OLD"]);
    }

    #[test]
    fn no_changes_yield_empty_patch() {
        let baseline = snap(&[("A", "1"), ("B", ""), ("PATH", "C:\\bin")]);
        let desired = snap(&[("A", "1"), ("B", ""), ("PATH", "C:\\bin")]);

        let patch = diff_env(&baseline, &desired);
        assert!(patch.is_empty(), "identical snapshots must diff to empty");
    }

    #[test]
    fn same_value_different_casing_is_same_key() {
        let baseline = snap(&[("PATH", "C:\\bin")]);
        let desired = snap(&[("Path", "C:\\bin")]);

        let patch = diff_env(&baseline, &desired);
        assert!(patch.is_empty());
    }

    #[test]
    fn value_change_across_casing_uses_desired_display() {
        let baseline = snap(&[("PATH", "C:\\old")]);
        let desired = snap(&[("Path", "C:\\new")]);

        let patch = diff_env(&baseline, &desired);
        assert!(patch.unset.is_empty(), "same canonical must not be unset");
        assert_eq!(
            patch.set.keys().collect::<Vec<_>>(),
            ["Path"],
            "set key must use desired display casing"
        );
        assert_eq!(patch.set.get("Path").map(String::as_str), Some("C:\\new"));
    }

    #[test]
    fn path_three_states() {
        let patch = diff_env(&snap(&[]), &snap(&[("PATH", "")]));
        assert_eq!(patch.set.get("PATH").map(String::as_str), Some(""));

        let patch = diff_env(&snap(&[("PATH", "")]), &snap(&[("PATH", "abc")]));
        assert_eq!(patch.set.get("PATH").map(String::as_str), Some("abc"));

        let patch = diff_env(&snap(&[("PATH", "abc")]), &snap(&[]));
        assert!(patch.set.is_empty());
        assert_eq!(patch.unset.iter().collect::<Vec<_>>(), ["PATH"]);

        let patch = diff_env(&snap(&[("PATH", "")]), &snap(&[]));
        assert!(patch.set.is_empty());
        assert_eq!(patch.unset.iter().collect::<Vec<_>>(), ["PATH"]);
    }

    #[test]
    fn unicode_values_round_trip() {
        let baseline = snap(&[("GREETING", "你好")]);
        let desired = snap(&[("GREETING", "こんにちは 🎉")]);

        let patch = diff_env(&baseline, &desired);
        assert_eq!(
            patch.set.get("GREETING").map(String::as_str),
            Some("こんにちは 🎉")
        );

        let baseline = snap(&[("EMOJI", "🦀")]);
        let desired = snap(&[("EMOJI", "🚀")]);
        let patch = diff_env(&baseline, &desired);
        assert_eq!(patch.set.get("EMOJI").map(String::as_str), Some("🚀"));
    }

    #[test]
    fn empty_value_vs_missing_are_different_states() {
        let patch = diff_env(&snap(&[]), &snap(&[("X", "")]));
        assert_eq!(patch.set.get("X").map(String::as_str), Some(""));

        let patch = diff_env(&snap(&[("X", "")]), &snap(&[("X", "")]));
        assert!(patch.is_empty());
    }

    #[test]
    fn hidden_drive_vars_with_equal_sign_are_ignored() {
        let baseline = snap(&[("=D:", "D:\\project\\hip_bench"), ("OLD", "1")]);
        let desired = snap(&[]);

        let patch = diff_env(&baseline, &desired);
        assert!(patch.set.is_empty());
        assert_eq!(
            patch.unset.iter().collect::<Vec<_>>(),
            vec!["OLD"],
            "hidden =X: pseudo-vars must never surface in the patch"
        );
        assert!(
            !patch.unset.iter().any(|k| k.contains('=')),
            "patch unset must only contain valid env var names: {:?}",
            patch.unset
        );

        let baseline = snap(&[("OLD", "1")]);
        let desired = snap(&[("=D:", "x"), ("OLD", "1")]);
        let patch = diff_env(&baseline, &desired);
        assert!(patch.set.is_empty(), "set must not carry =X: keys");
    }
}
