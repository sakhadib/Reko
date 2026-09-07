use anyhow::{Context, Result};
use std::path::Path;

/// Read file exactly as-is, preserving tabs, spaces, line breaks, etc.
///
/// Returns the raw `String` content (UTF-8). For non-UTF8 files use
/// `read_bytes_exact`.
pub fn read_exact(path: &Path) -> Result<String> {
    std::fs::read_to_string(path)
        .with_context(|| format!("failed to read file: {}", path.display()))
}

/// Read file as raw bytes, fully preserving semantics (binary-safe).
pub fn read_bytes_exact(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).with_context(|| format!("failed to read file: {}", path.display()))
}

#[allow(dead_code)]
/// Return lines with numbers, fully preserving original content.
///
/// Each entry is `(line_number, line_content_with_terminator)` where
/// `line_content_with_terminator` includes the original line ending (`\n`, `\r\n`, or `\r`)
/// if present, and preserves tabs/spaces verbatim. The last line without terminator
/// is returned as-is. Empty file returns empty vec.
///
/// Uses byte-level scan so we don't normalize line endings.
pub fn read_with_line_numbers(path: &Path) -> Result<Vec<(usize, String)>> {
    let content = read_exact(path)?;
    Ok(with_line_numbers_from_content(&content))
}

/// Pure helper: split `content` into `(line_num, line)` preserving exact bytes.
///
/// Visible logic for testing without filesystem.
pub fn with_line_numbers_from_content(content: &str) -> Vec<(usize, String)> {
    if content.is_empty() {
        return Vec::new();
    }

    let mut result = Vec::new();
    let mut line_num = 1usize;
    let mut start = 0usize;
    let bytes = content.as_bytes();

    for i in 0..bytes.len() {
        if bytes[i] == b'\n' {
            // include up to and including '\n' (so \r\n is preserved as \r + \n together)
            let line = &content[start..=i];
            result.push((line_num, line.to_string()));
            line_num += 1;
            start = i + 1;
        }
    }

    // trailing content without \n (or after last \n)
    if start < content.len() {
        let line = &content[start..];
        result.push((line_num, line.to_string()));
    } else if content.ends_with('\n') {
        // content ended with newline -> no extra push, already handled
    }

    // Handle lone \r line breaks (old Mac) that weren't covered by \n scan:
    // If file uses only \r as separator, above loop yields one entry.
    // We post-process if needed: detect presence of \r without \n and split.
    // To keep exact semantics we only do this if content contains \r and no \n
    if !content.contains('\n') && content.contains('\r') {
        result.clear();
        let mut r_start = 0usize;
        // work on bytes_indices of content
        let mut idx = 0usize;
        while idx < bytes.len() {
            if bytes[idx] == b'\r' {
                let line = &content[r_start..=idx];
                result.push((result.len() + 1, line.to_string()));
                r_start = idx + 1;
            }
            idx += 1;
        }
        if r_start < content.len() {
            result.push((result.len() + 1, content[r_start..].to_string()));
        }
    }

    result
}

/// Format content with line numbers as `"{num:>6}: {line}"`.
///
/// - Line numbers are right-aligned width 6 (like `cat -n` / `nl`).
/// - Original line content is appended verbatim (including its terminator).
/// - If original line already had a terminator, no extra newline is added.
/// - If original last line lacked terminator, the formatted output ends without extra newline
///   (preserving exact semantics).
///
/// For display you may want to always end with newline; use `format_with_line_numbers_always_newline`.
pub fn format_with_line_numbers(content: &str) -> String {
    let lines = with_line_numbers_from_content(content);
    let mut out = String::with_capacity(content.len() + lines.len() * 8);
    for (num, line) in lines {
        // Use width 6 + ": " prefix, then exact line
        // line already contains its terminator if present
        out.push_str(&format!("{num:>6}: {line}"));
        // If line lacked terminator we don't add one - preserves semantics
    }
    out
}

/// Variant that always ensures formatted output ends with newline (friendlier for terminal).
pub fn format_with_line_numbers_always_newline(content: &str) -> String {
    let mut s = format_with_line_numbers(content);
    if !s.is_empty() && !s.ends_with('\n') {
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty() {
        assert!(with_line_numbers_from_content("").is_empty());
        assert_eq!(format_with_line_numbers(""), "");
    }

    #[test]
    fn single_line_no_newline() {
        let v = with_line_numbers_from_content("hello\tworld");
        assert_eq!(v, vec![(1, "hello\tworld".to_string())]);
        assert_eq!(format_with_line_numbers("hello\tworld"), "     1: hello\tworld");
    }

    #[test]
    fn two_lines_unix() {
        let content = "a\tb\nc d\n";
        let v = with_line_numbers_from_content(content);
        assert_eq!(v, vec![(1, "a\tb\n".to_string()), (2, "c d\n".to_string())]);
        assert_eq!(
            format_with_line_numbers(content),
            "     1: a\tb\n     2: c d\n"
        );
    }

    #[test]
    fn preserves_crlf() {
        let content = "line1\r\nline2\r\n";
        let v = with_line_numbers_from_content(content);
        assert_eq!(
            v,
            vec![(1, "line1\r\n".to_string()), (2, "line2\r\n".to_string())]
        );
        // format preserves \r\n inside
        assert_eq!(
            format_with_line_numbers(content),
            "     1: line1\r\n     2: line2\r\n"
        );
    }

    #[test]
    fn mixed_trailing_no_newline() {
        let content = "x\n y\tz";
        let v = with_line_numbers_from_content(content);
        assert_eq!(
            v,
            vec![(1, "x\n".to_string()), (2, " y\tz".to_string())]
        );
        assert_eq!(format_with_line_numbers(content), "     1: x\n     2:  y\tz");
    }

    #[test]
    fn spaces_and_tabs_preserved() {
        let content = "  \t  hello  \t \n\t\n";
        let v = with_line_numbers_from_content(content);
        assert_eq!(
            v,
            vec![
                (1, "  \t  hello  \t \n".to_string()),
                (2, "\t\n".to_string())
            ]
        );
    }

    #[test]
    fn lone_cr_old_mac() {
        let content = "a\rb\rc";
        let v = with_line_numbers_from_content(content);
        assert_eq!(
            v,
            vec![
                (1, "a\r".to_string()),
                (2, "b\r".to_string()),
                (3, "c".to_string())
            ]
        );
    }

    #[test]
    fn bytes_exact_roundtrip() {
        use std::io::Write;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        let data = "a\tb\r\nc d\n  \t\n";
        tmp.write_all(data.as_bytes()).unwrap();
        let v = read_with_line_numbers(tmp.path()).unwrap();
        assert_eq!(v[0].1, "a\tb\r\n");
        assert_eq!(v[1].1, "c d\n");
        assert_eq!(v[2].1, "  \t\n");
    }
}
