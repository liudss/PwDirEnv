//! Lexical Windows path comparison without filesystem resolution; nonexistent paths are valid inputs.

pub fn normalize_path_str(s: &str) -> String {
    let replaced = s.replace('/', "\\");
    if !replaced.is_empty() && replaced.trim_end_matches('\\').is_empty() {
        return replaced;
    }
    let trimmed = replaced.trim_end_matches('\\');
    let is_drive_root = matches!(trimmed.as_bytes(), [c, b':'] if c.is_ascii_alphabetic());
    if is_drive_root {
        format!("{trimmed}\\")
    } else {
        trimmed.to_string()
    }
}

pub fn paths_equal_ci(a: &str, b: &str) -> bool {
    normalize_path_str(a).eq_ignore_ascii_case(&normalize_path_str(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_forward_slashes_and_trailing_separator() {
        assert_eq!(normalize_path_str("C:/repo/sub"), r"C:\repo\sub");
        assert_eq!(normalize_path_str(r"C:\repo\"), r"C:\repo");
        assert_eq!(normalize_path_str(r"C:\repo\\"), r"C:\repo");
        assert_eq!(normalize_path_str("C:/repo/"), r"C:\repo");
    }

    #[test]
    fn preserves_drive_root() {
        assert_eq!(normalize_path_str(r"C:\"), r"C:\");
        assert_eq!(normalize_path_str("C:/"), r"C:\");
        assert_eq!(normalize_path_str(r"D:\"), r"D:\");
    }

    #[test]
    fn handles_paths_with_spaces() {
        assert_eq!(
            normalize_path_str(r"C:\repo with spaces\sub dir\"),
            r"C:\repo with spaces\sub dir"
        );
        assert!(paths_equal_ci(
            r"C:\repo with spaces\sub dir\",
            r"C:\repo with spaces\sub dir"
        ));
    }

    #[test]
    fn trailing_separator_is_irrelevant_for_equality() {
        assert!(paths_equal_ci(r"C:\repo\", r"C:\repo"));
        assert!(paths_equal_ci(r"C:\repo\sub\", r"C:\repo\sub"));
        assert!(!paths_equal_ci(r"C:\repo\", r"C:\repo\sub"));
    }

    #[test]
    fn comparison_is_case_insensitive() {
        assert!(paths_equal_ci(r"C:\Foo\Bin", r"c:\foo\bin"));
        assert!(paths_equal_ci(r"C:\Foo\Bin", r"C:\foo\BIN"));
        assert!(!paths_equal_ci(r"C:\Foo\Bin", r"C:\Foo\Baz"));
    }

    #[test]
    fn handles_unc_paths() {
        assert_eq!(
            normalize_path_str(r"\\server\share\dir\"),
            r"\\server\share\dir"
        );
        assert_eq!(
            normalize_path_str("//server/share/dir"),
            r"\\server\share\dir"
        );
        assert!(paths_equal_ci(r"\\SERVER\Share\Dir", r"\\server\share\dir"));
    }

    #[test]
    fn empty_and_separator_only_inputs() {
        assert_eq!(normalize_path_str(""), "");
        assert_eq!(normalize_path_str(r"\"), r"\");
        assert_eq!(normalize_path_str(r"\\"), r"\\");
    }
}
