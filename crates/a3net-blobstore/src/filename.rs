//! Filename-sanitising helpers used when staging blobs to the local
//! filesystem.
//!
//! Mirrors the filename sanitisation in
//! `Exodus@src-backup/.../p2p_cdn/download.rs` (`p2p_blobs_service`
//! uses the same family of helpers). The function is intentionally
//! ultra-conservative — anything that could be confusing to a shell,
//! filesystem, or user interface is replaced with `_`.
//!
//! The result is **not** guaranteed to be safe to use as a path
//! component; callers must still wrap with `std::path::Path::new(...)`
//! and ensure the absolute path stays inside an allowed directory.

/// Maximum allowed length of a sanitised filename. Longer names are
/// truncated, preserving the extension when present.
pub const MAX_FILENAME_LEN: usize = 200;

/// Replace every character that is not `[A-Za-z0-9._-]` with `_` and
/// collapse runs of `_`.
///
/// - Leading dots are stripped (avoid creating hidden files).
/// - The string is truncated to [`MAX_FILENAME_LEN`] characters.
/// - An empty / fully-stripped result is replaced with the literal
///   `"file"` so the caller always gets a non-empty filename.
pub fn safe_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();

    // Strip leading dots to avoid hidden files.
    let trimmed = cleaned.trim_start_matches('.').to_string();

    // Collapse runs of `_`.
    let mut collapsed = String::with_capacity(trimmed.len());
    let mut prev_underscore = false;
    for c in trimmed.chars() {
        if c == '_' {
            if !prev_underscore {
                collapsed.push('_');
            }
            prev_underscore = true;
        } else {
            collapsed.push(c);
            prev_underscore = false;
        }
    }

    let collapsed = collapsed.trim_matches('_').to_string();

    let final_str = if collapsed.is_empty() {
        "file".to_string()
    } else {
        collapsed
    };

    truncate_with_ext(&final_str, MAX_FILENAME_LEN)
}

/// Truncate to `max_len` while preserving the extension when possible.
fn truncate_with_ext(name: &str, max_len: usize) -> String {
    if name.len() <= max_len {
        return name.to_string();
    }
    if let Some((stem, ext)) = name.rsplit_once('.')
        && !stem.is_empty()
        && !ext.is_empty()
    {
        let ext_with_dot = format!(".{ext}");
        let keep = max_len.saturating_sub(ext_with_dot.len());
        if keep > 0 {
            let stem: String = stem.chars().take(keep).collect();
            return format!("{stem}{ext_with_dot}");
        }
    }
    name.chars().take(max_len).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_alphanumeric_and_basic_punct() {
        assert_eq!(safe_filename("hello.txt"), "hello.txt");
        assert_eq!(safe_filename("report-2026-03.pdf"), "report-2026-03.pdf");
    }

    #[test]
    fn strips_path_separators() {
        // Slashes and backslashes become underscores.
        assert_eq!(safe_filename("a/b/c.txt"), "a_b_c.txt");
        assert_eq!(safe_filename("a\\b\\c.txt"), "a_b_c.txt");
        // Leading dots are stripped to avoid hidden files.
        assert_eq!(safe_filename(".bashrc"), "bashrc");
        // Path traversal: leading dots stripped, separators → underscores,
        // then trimmed on both ends.
        assert_eq!(safe_filename("../etc/passwd"), "etc_passwd");
        assert_eq!(safe_filename("..\\windows\\sys"), "windows_sys");
    }

    #[test]
    fn replaces_unsafe_chars_with_underscore() {
        assert_eq!(safe_filename("hello world.txt"), "hello_world.txt");
        // `quote"and<space>` → `quote_and_space_` → trim trailing `_` → `quote_and_space`.
        assert_eq!(safe_filename("quote\"and<space>"), "quote_and_space");
    }

    #[test]
    fn collapses_underscore_runs() {
        assert_eq!(safe_filename("a___b.txt"), "a_b.txt");
        assert_eq!(safe_filename("a   b.txt"), "a_b.txt");
    }

    #[test]
    fn falls_back_to_file_when_empty() {
        assert_eq!(safe_filename(""), "file");
        assert_eq!(safe_filename("..."), "file");
        assert_eq!(safe_filename("///"), "file");
    }

    #[test]
    fn truncates_long_names_preserving_extension() {
        let long = "a".repeat(500);
        let s = safe_filename(&format!("{long}.txt"));
        assert!(s.len() <= MAX_FILENAME_LEN);
        assert!(s.ends_with(".txt"));
    }

    #[test]
    fn truncates_long_names_without_extension() {
        let long = "a".repeat(500);
        let s = safe_filename(&long);
        assert!(s.len() <= MAX_FILENAME_LEN);
        assert!(s.starts_with('a'));
    }

    #[test]
    fn trims_trailing_underscores() {
        assert_eq!(safe_filename("foo___"), "foo");
        // `foo<bar>` → `foo_bar_` → trim trailing `_` → `foo_bar`.
        assert_eq!(safe_filename("foo<bar>"), "foo_bar");
    }
}
