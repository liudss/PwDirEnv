//! hook profile 安装/卸载（M7，SPEC §3.1/§14.6）。
//!
//! 设计要点：
//! - **marker 幂等**：hook 块由 begin/end marker 包裹。install 先查 marker，
//!   已存在直接幂等返回；uninstall 按行移除完整 marker 块。重复 install/uninstall
//!   不会堆积或残留片段；
//! - **all-hosts profile**：默认目标是 `$PROFILE.CurrentUserAllHosts`（`profile.ps1`），
//!   对 PowerShell 7 的全部 host 应用生效，且不与 Windows PowerShell 5.1 的
//!   Documents\WindowsPowerShell 目录混用（5.1 本就不受支持）；
//! - 纯文本操作（marker 常量 + 按行读写）与文件 IO 分离，便于 temp 文件注入测试；
//! - 写入 UTF-8 无 BOM（`fs::write` 原始字节，不追加 BOM）；
//!   行结束符探测沿用文件现有风格（CRLF 优先，Windows PowerShell 友好）。

use std::fs;
use std::io;
use std::path::Path;

/// hook 块起始 marker（含 PwDirEnv 字样，doctor 的 profile-hook 检查据此识别）。
pub const HOOK_BEGIN_MARKER: &str = "# >>> PwDirEnv (begin) >>>";
/// hook 块结束 marker。
pub const HOOK_END_MARKER: &str = "# <<< PwDirEnv (end) <<<";

/// 生成 hook 块的行序列（marker + 两行 bootstrap，SPEC §14.6 的极小 bootstrap）。
///
/// `module_psd1` 为解析到的 psd1 绝对路径；找不到时由调用方传入占位符文本。
/// 路径按 PowerShell 单引号字符串转义（`'` → `''`），路径含单引号（如 `C:\repo's`）也合法。
pub fn hook_block_lines(module_psd1: &str) -> Vec<String> {
    let escaped = module_psd1.replace('\'', "''");
    vec![
        HOOK_BEGIN_MARKER.to_string(),
        format!("Import-Module '{escaped}'"),
        "Enable-PwDirEnv".to_string(),
        HOOK_END_MARKER.to_string(),
    ]
}

/// 内容里是否已有完整 marker 块（要求 begin 与 end 同时存在）。
pub fn contains_hook_block(content: &str) -> bool {
    content.contains(HOOK_BEGIN_MARKER) && content.contains(HOOK_END_MARKER)
}

/// 探测文件的行结束符风格：已有 CRLF 则保持 CRLF，否则用 LF。
fn detect_newline(content: &str) -> &'static str {
    if content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

/// 把 hook 块追加到 profile 内容：已含 marker → None（幂等不重复写）；
/// 否则返回追加后的新内容。追加前保证既有内容以换行结尾，marker 块独立成行。
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

/// 从 profile 内容按行移除完整 marker 块：无块或块不完整 → None；
/// 否则返回移除后的新内容（块外的行原样保留）。
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
            // 块内行全部跳过，直到 end marker
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

/// 把 hook 块写入目标文件（幂等）。返回 `Ok(true)` 表示已写入，`Ok(false)`
/// 表示已有块（already installed）。文件不存在时创建（含父目录）。
pub fn install_to_file(target: &Path, block: &[String]) -> io::Result<bool> {
    let content = match fs::read_to_string(target) {
        Ok(content) => content,
        // 文件不存在 → 视为空内容，稍后创建（SPEC §3.1：幂等安装）
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

/// 从目标文件移除 hook 块。返回 `Ok(true)` 表示已移除，`Ok(false)` 表示无块
/// （nothing to uninstall）。目标文件不存在时同样返回 `Ok(false)`。
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
        // 无 BOM：文件以 marker 的 '#' 开头
        assert!(content.starts_with(HOOK_BEGIN_MARKER));
    }

    #[test]
    fn install_is_idempotent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("profile.ps1");
        let block = hook_block_lines("C:\\m\\PwDirEnv.psd1");

        assert!(install_to_file(&target, &block).expect("install 1"));
        let first = fs::read_to_string(&target).expect("read 1");

        // 第二次 install：already installed，内容不变
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
        // 用户既有内容在前、hook 块在后，且 CRLF 风格被保持
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

        // 再次 uninstall：nothing to uninstall
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
        // 只有 begin 没有 end 的畸形内容：remove_block 必须拒绝（返回 None），
        // 不能把后续用户内容一起吞掉
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
