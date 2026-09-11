//! Idempotent, marker-based PowerShell profile installation. Preserve surrounding user content.

use std::fs;
use std::io;
use std::path::Path;

pub const HOOK_BEGIN_MARKER: &str = "# >>> PwDirEnv (begin) >>>";
pub const HOOK_END_MARKER: &str = "# <<< PwDirEnv (end) <<<";

pub fn hook_block_lines(module_psd1: &str) -> Vec<String> {
    let escaped = module_psd1.replace('\'', "''");
    vec![
        HOOK_BEGIN_MARKER.to_string(),
        format!("Import-Module '{escaped}'"),
        "Enable-PwDirEnv".to_string(),
        HOOK_END_MARKER.to_string(),
    ]
}

pub fn contains_hook_block(content: &str) -> bool {
    content.contains(HOOK_BEGIN_MARKER) && content.contains(HOOK_END_MARKER)
}

fn detect_newline(content: &str) -> &'static str {
    if content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

pub fn append_block(content: &str, block: &[String]) -> Option<String> {
    if contains_hook_block(content) {
        return None;
    }
    let nl = detect_newline(content);
    let mut out = String::with_capacity(content.len() + 128);
    out.push_str(content);
    if !out.is_empty() && !out.ends_with('\n') {
        out.push_str(nl);
    }
    for line in block {
        out.push_str(line);
        out.push_str(nl);
    }
    Some(out)
}

pub fn remove_block(content: &str) -> Option<String> {
    if !contains_hook_block(content) {
        return None;
    }
    let nl = detect_newline(content);
    let mut out = String::with_capacity(content.len());
    let mut inside = false;
    let mut removed = false;
    for line in content.lines() {
        if inside {
            if line.trim() == HOOK_END_MARKER {
                inside = false;
                removed = true;
            }
            continue;
        }
        if line.trim() == HOOK_BEGIN_MARKER {
            inside = true;
            continue;
        }
        out.push_str(line);
        out.push_str(nl);
    }
    if removed {
        Some(out)
    } else {
        None
    }
}

pub fn install_to_file(target: &Path, block: &[String]) -> io::Result<bool> {
    let content = match fs::read_to_string(target) {
        Ok(content) => content,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let Some(new_content) = append_block(&content, block) else {
        return Ok(false);
    };
    if let Some(parent) = target.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    fs::write(target, new_content)?;
    Ok(true)
}

pub fn uninstall_from_file(target: &Path) -> io::Result<bool> {
    let content = match fs::read_to_string(target) {
        Ok(content) => content,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    let Some(new_content) = remove_block(&content) else {
        return Ok(false);
    };
    fs::write(target, new_content)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_creates_file_with_marker_block() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("profile.ps1");
        let block = hook_block_lines(r"C:\tools\PwDirEnv\modules\PwDirEnv\PwDirEnv.psd1");

        assert!(install_to_file(&target, &block).expect("install"));

        let content = fs::read_to_string(&target).expect("read profile");
        assert!(content.contains(HOOK_BEGIN_MARKER));
        assert!(content.contains(HOOK_END_MARKER));
        assert!(
            content.contains(r"Import-Module 'C:\tools\PwDirEnv\modules\PwDirEnv\PwDirEnv.psd1'"),
            "Import-Module line with resolved path expected: {content}"
        );
        assert!(content.contains("Enable-PwDirEnv"));
        assert!(content.starts_with(HOOK_BEGIN_MARKER));
    }

    #[test]
    fn install_is_idempotent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("profile.ps1");
        let block = hook_block_lines("C:\\m\\PwDirEnv.psd1");

        assert!(install_to_file(&target, &block).expect("install 1"));
        let first = fs::read_to_string(&target).expect("read 1");

        assert!(!install_to_file(&target, &block).expect("install 2"));
        let second = fs::read_to_string(&target).expect("read 2");
        assert_eq!(
            first, second,
            "repeated install must not duplicate the block"
        );
        assert_eq!(second.matches(HOOK_BEGIN_MARKER).count(), 1);
    }

    #[test]
    fn install_appends_after_existing_content() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("profile.ps1");
        fs::write(&target, "Set-PSReadLineOption -PredictionSource None\r\n")
            .expect("seed profile");
        let block = hook_block_lines("C:\\m\\PwDirEnv.psd1");

        assert!(install_to_file(&target, &block).expect("install"));
        let content = fs::read_to_string(&target).expect("read");
        assert!(content.starts_with("Set-PSReadLineOption"));
        assert!(content.contains(HOOK_BEGIN_MARKER));
        assert!(content.contains("\r\n"));
    }

    #[test]
    fn uninstall_removes_block_and_keeps_user_content() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("profile.ps1");
        let original = "Write-Host my own setup\r\n";
        fs::write(&target, format!("{}{}", original, "extra\r\n")).expect("seed");
        let block = hook_block_lines("C:\\m\\PwDirEnv.psd1");
        assert!(install_to_file(&target, &block).expect("install"));

        assert!(uninstall_from_file(&target).expect("uninstall"));
        let content = fs::read_to_string(&target).expect("read");
        assert!(!content.contains(HOOK_BEGIN_MARKER));
        assert!(!content.contains(HOOK_END_MARKER));
        assert!(!content.contains("Enable-PwDirEnv"));
        assert!(
            content.starts_with(original),
            "user content kept: {content}"
        );

        assert!(!uninstall_from_file(&target).expect("uninstall 2"));
    }

    #[test]
    fn uninstall_on_missing_file_is_noop() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("ghost.ps1");
        assert!(!uninstall_from_file(&target).expect("uninstall missing"));
    }

    #[test]
    fn remove_block_leaves_orphan_begin_marker_untouched() {
        let malformed = "# >>> PwDirEnv (begin) >>>\nImport-Module 'x'\nuser stuff\n";
        assert!(remove_block(malformed).is_none());
    }

    #[test]
    fn module_path_with_apostrophe_is_escaped_for_powershell() {
        let block = hook_block_lines(r"C:\repo's\modules\PwDirEnv\PwDirEnv.psd1");
        let import = &block[1];
        assert_eq!(
            *import,
            r"Import-Module 'C:\repo''s\modules\PwDirEnv\PwDirEnv.psd1'"
        );
    }
}
