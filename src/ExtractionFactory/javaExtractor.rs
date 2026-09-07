use crate::ir::{IrFunction, Parameter, Position};
use anyhow::Result;
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Debug, Clone)]
struct JavaMethodRaw {
    name: String,
    return_type: String,
    visibility: String,
    modifiers: Vec<String>,
    parameters: Vec<Parameter>,
    throws: Vec<String>,
    start_line: usize,
    start_col: usize,
    start_byte: usize,
    end_line: usize,
    end_col: usize,
    end_byte: usize,
    source_text: String,
}

/// Public entry: given file content (exact, from reader) and file path, extract IR for each Java method.
pub fn extract(content: &str, file_path: &Path) -> Result<Vec<IrFunction>> {
    let package = parse_package(content);
    let class = parse_class(content);
    let module = file_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    let file_str = file_path.display().to_string();

    let raws = parse_methods(content)?;

    let mut out = Vec::new();
    for r in raws {
        let hash = hash_source(&r.source_text);
        let qualified = match (&package, &class) {
            (Some(pkg), Some(cls)) => format!("{pkg}::{cls}::{}", r.name),
            (Some(pkg), None) => format!("{pkg}::{}", r.name),
            (None, Some(cls)) => format!("{cls}::{}", r.name),
            (None, None) => r.name.clone(),
        };
        let id = qualified.clone();

        let ir = IrFunction::new_minimal(
            id,
            r.name,
            qualified,
            file_str.clone(),
            module.clone(),
            r.source_text,
            hash,
            Position {
                line: r.start_line,
                column: r.start_col,
                byte: r.start_byte,
            },
            Position {
                line: r.end_line,
                column: r.end_col,
                byte: r.end_byte,
            },
            r.visibility,
            r.modifiers,
            Some(r.return_type),
            r.parameters,
            r.throws,
            package.clone(),
            class.clone(),
        );
        out.push(ir);
    }
    Ok(out)
}

/// JSON helper
#[allow(dead_code)]
pub fn extract_to_json(content: &str, file_path: &Path) -> Result<String> {
    let fns = extract(content, file_path)?;
    Ok(serde_json::to_string_pretty(&fns)?)
}

fn hash_source(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

fn parse_package(content: &str) -> Option<String> {
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with("package ") && t.ends_with(';') {
            let inner = t
                .trim_start_matches("package ")
                .trim_end_matches(';')
                .trim();
            return Some(inner.to_string());
        }
    }
    None
}

fn parse_class(content: &str) -> Option<String> {
    // naive: first occurrence of `class NAME`
    for line in content.lines() {
        let t = line.trim();
        // skip comments
        if t.starts_with("//") || t.starts_with("/*") || t.starts_with("*") {
            continue;
        }
        if let Some(idx) = t.find(" class ") {
            let after = &t[idx + 7..];
            let name: String = after
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
                .collect();
            if !name.is_empty() {
                return Some(name);
            }
        } else if t.starts_with("class ") {
            let after = &t[6..];
            let name: String = after
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
                .collect();
            if !name.is_empty() {
                return Some(name);
            }
        }
    }
    None
}

fn parse_methods(content: &str) -> Result<Vec<JavaMethodRaw>> {
    // Build line index: byte offset for each line start (1-indexed)
    let mut line_starts: Vec<usize> = vec![0];
    for (i, b) in content.bytes().enumerate() {
        if b == b'\n' {
            line_starts.push(i + 1);
        }
    }
    let mut result = Vec::new();
    let lines: Vec<&str> = content.lines().collect();

    // Precompute for quick skip: track if inside block comment
    let mut i = 0usize;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        // Skip empty, annotations, package/import, class decl, comments
        if trimmed.is_empty()
            || trimmed.starts_with("package ")
            || trimmed.starts_with("import ")
            || trimmed.starts_with("/*")
            || trimmed.starts_with("*")
            || trimmed.starts_with("//")
            || trimmed.starts_with("@")
            || (trimmed.contains(" class ") || trimmed.starts_with("class "))
            || trimmed == "{"
            || trimmed == "}"
        {
            i += 1;
            continue;
        }

        if is_method_signature(trimmed) {
            // Find open brace position: either on same line or next lines
            let (_brace_line, _brace_col_in_line, brace_byte) =
                find_open_brace(content, &line_starts, i)?;

            let start_line = i + 1;
            let start_col = first_non_space_col(line) + 1; // 1-indexed
            let start_byte = line_starts[i] + first_non_space_col(line);

            let end_byte_inclusive = find_matching_brace(content, brace_byte)
                .ok_or_else(|| anyhow::anyhow!("unmatched brace for method at line {}", start_line))?;
            // end line/col
            let (end_line, end_col) = byte_to_line_col(content, &line_starts, end_byte_inclusive);

            let source_text = content[start_byte..=end_byte_inclusive].to_string();

            let (vis, mods, ret, name, params, throws) = parse_signature(trimmed);

            result.push(JavaMethodRaw {
                name,
                return_type: ret,
                visibility: vis,
                modifiers: mods,
                parameters: params,
                throws,
                start_line,
                start_col,
                start_byte,
                end_line,
                end_col,
                end_byte: end_byte_inclusive,
                source_text,
            });

            // Advance i to end_line
            i = end_line; // next iteration will be end_line (1-indexed) -> need 0-indexed
            // convert: end_line is 1-indexed, next index = end_line
            // e.g., method ends at line 14 (1-indexed), lines[13] is that line, next is lines[14] -> i=14
            continue;
        }

        i += 1;
    }

    Ok(result)
}

fn first_non_space_col(s: &str) -> usize {
    s.chars()
        .position(|c| c != ' ' && c != '\t')
        .unwrap_or(0)
}

fn is_method_signature(trimmed: &str) -> bool {
    // Heuristic: contains '(' and ')' and '{' or next line is '{', and not control flow
    if trimmed.starts_with("if ")
        || trimmed.starts_with("if(")
        || trimmed.starts_with("for ")
        || trimmed.starts_with("for(")
        || trimmed.starts_with("while ")
        || trimmed.starts_with("while(")
        || trimmed.starts_with("switch ")
        || trimmed.starts_with("catch ")
        || trimmed.starts_with("try ")
        || trimmed.starts_with("else")
    {
        return false;
    }
    // must have parens
    if !trimmed.contains('(') || !trimmed.contains(')') {
        return false;
    }
    // must look like `type name(` or `name(` with maybe modifiers
    // Check that ')' is followed by optional `throws ...` and then `{` or empty (brace on next line)
    // For simplicity, check that after ')' we have `{` or `throws` or nothing, but not `;`
    if trimmed.ends_with(';') {
        return false;
    }
    // Contains at least one space before '(' for return type + name separation, or is constructor (ClassName())
    // Accept both.
    true
}

fn find_open_brace(
    content: &str,
    line_starts: &[usize],
    start_idx: usize,
) -> Result<(usize, usize, usize)> {
    // Search from start_idx line forward for first '{'
    let lines: Vec<&str> = content.lines().collect();
    for idx in start_idx..lines.len() {
        if let Some(col) = lines[idx].find('{') {
            let byte = line_starts[idx] + col;
            // ensure it's not inside comment? simplified
            return Ok((idx + 1, col + 1, byte));
        }
        // If line contains `)` but no `{`, continue to next line expecting `{`
        // But stop if we hit `;`
        if lines[idx].contains(';') {
            anyhow::bail!("no open brace found")
        }
        if idx > start_idx + 2 {
            break;
        }
    }
    anyhow::bail!("no open brace found for method starting at line {}", start_idx + 1)
}

fn find_matching_brace(content: &str, open_pos: usize) -> Option<usize> {
    let bytes = content.as_bytes();
    let mut depth = 0i32;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let mut in_string = false;
    let mut in_char = false;
    let mut escape = false;

    for i in open_pos..bytes.len() {
        let b = bytes[i];
        let next = if i + 1 < bytes.len() {
            Some(bytes[i + 1])
        } else {
            None
        };

        if in_line_comment {
            if b == b'\n' {
                in_line_comment = false;
            }
            continue;
        }
        if in_block_comment {
            if b == b'*' && next == Some(b'/') {
                in_block_comment = false;
            }
            continue;
        }
        if in_string {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        if in_char {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'\'' {
                in_char = false;
            }
            continue;
        }

        // not in any
        if b == b'/' && next == Some(b'/') {
            in_line_comment = true;
            continue;
        }
        if b == b'/' && next == Some(b'*') {
            in_block_comment = true;
            continue;
        }
        if b == b'"' {
            in_string = true;
            continue;
        }
        if b == b'\'' {
            in_char = true;
            continue;
        }
        if b == b'{' {
            depth += 1;
        } else if b == b'}' {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

fn byte_to_line_col(_content: &str, line_starts: &[usize], byte: usize) -> (usize, usize) {
    // find greatest line_start <= byte
    let mut line = 1;
    for (idx, &start) in line_starts.iter().enumerate() {
        if start <= byte {
            line = idx + 1;
        } else {
            break;
        }
    }
    let col = byte - line_starts[line - 1] + 1;
    // also need to clamp col within line content: last char may be '}' -> col is position
    (line, col)
}

fn parse_signature(trimmed: &str) -> (String, Vec<String>, String, String, Vec<Parameter>, Vec<String>) {
    // Extract up to '('
    // Example: "public int add(int a, int b) {"
    // or "public boolean isPrime(int n) {"
    // or "public int findMax(int... numbers) {"
    // Visibility: public/private/protected or default
    // Modifiers: static, final, synchronized, etc
    let mut visibility = "package-private".to_string();
    let mut modifiers = Vec::new();

    // Remove trailing '{' and trim
    let mut sig = trimmed.trim().trim_end_matches('{').trim().to_string();
    // Remove throws part for now
    let mut throws = Vec::new();
    if let Some(throws_idx) = sig.find(" throws ") {
        let after = sig[throws_idx + 8..].to_string();
        sig = sig[..throws_idx].to_string();
        throws = after
            .split(',')
            .map(|s| s.trim().trim_end_matches('{').trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }

    // Now sig is like "public int add(int a, int b)"
    let paren_start = sig.find('(').unwrap_or(sig.len());
    let before_paren = sig[..paren_start].trim();
    let inside_paren = if paren_start < sig.len() {
        let paren_end = sig.rfind(')').unwrap_or(sig.len() - 1);
        sig[paren_start + 1..paren_end].trim().to_string()
    } else {
        String::new()
    };

    // before_paren tokens
    let tokens: Vec<&str> = before_paren.split_whitespace().collect();
    let name = tokens.last().unwrap_or(&"unknown").to_string();
    let ret_and_mods = if tokens.len() >= 2 {
        &tokens[..tokens.len() - 1]
    } else {
        &[]
    };

    let mut return_type = "void".to_string();
    for tok in ret_and_mods {
        match *tok {
            "public" | "private" | "protected" => visibility = tok.to_string(),
            "static" | "final" | "synchronized" | "abstract" | "native" | "strictfp" => {
                modifiers.push(tok.to_string())
            }
            _ => {
                // first non-modifier after visibility is return type (may include generics)
                // Could be multiple tokens like "Map<String, Integer>"
                // Join remaining as return type
                return_type = ret_and_mods
                    .iter()
                    .skip_while(|t| {
                        **t == "public"
                            || **t == "private"
                            || **t == "protected"
                            || **t == "static"
                            || **t == "final"
                            || **t == "synchronized"
                            || **t == "abstract"
                    })
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" ");
                break;
            }
        }
    }
    if return_type.is_empty() && !ret_and_mods.is_empty() {
        return_type = ret_and_mods.last().unwrap().to_string();
    }

    // Parse parameters: split by ',' but not inside <>
    let parameters = parse_params(&inside_paren);

    (visibility, modifiers, return_type, name, parameters, throws)
}

fn parse_params(s: &str) -> Vec<Parameter> {
    if s.trim().is_empty() {
        return vec![];
    }
    let mut params = Vec::new();
    // simple split by ','
    for part in s.split(',') {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        // p like "int a" or "int... numbers" or "final String input"
        // Remove annotations/modifiers like final
        let tokens: Vec<&str> = p.split_whitespace().collect();
        if tokens.is_empty() {
            continue;
        }
        let name = tokens.last().unwrap().to_string();
        let typ = if tokens.len() >= 2 {
            tokens[..tokens.len() - 1].join(" ").replace("final ", "").trim().to_string()
        } else {
            "Object".to_string()
        };
        params.push(Parameter { name, typ });
    }
    params
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const SAMPLE: &str = r#"package com.example.test;

public class TestFunctions {
	public int add(int a, int b) {
		return a + b;
	}
    public boolean isPrime(int n) {
        if (n <= 1) return false;
        return true;
    }
	public String reverseString(String input) {
		if (input == null) return null;
		return input;
	}
}
"#;

    #[test]
    fn extracts_three() {
        let fns = extract(SAMPLE, Path::new("ManualTest/TestFunctions.java")).unwrap();
        assert_eq!(fns.len(), 3);
        assert_eq!(fns[0].identity.name, "add");
        assert_eq!(fns[0].signature.return_type.as_deref(), Some("int"));
        assert_eq!(fns[0].signature.parameters.len(), 2);
        assert_eq!(fns[1].identity.name, "isPrime");
        assert_eq!(fns[2].identity.name, "reverseString");
        assert_eq!(
            fns[0].identity.qualified_name,
            "com.example.test::TestFunctions::add"
        );
    }

    #[test]
    fn location_bytes() {
        let fns = extract(SAMPLE, Path::new("a.java")).unwrap();
        assert!(fns[0].source.location.start.byte < fns[0].source.location.end.byte);
        assert!(fns[0].source.hash.starts_with("sha256:"));
    }
}
