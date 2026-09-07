use crate::ir::{IrFunction, Parameter, Position};
use anyhow::Result;
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Debug, Clone)]
struct TsFunctionRaw {
    name: String,
    return_type: Option<String>,
    visibility: String,
    modifiers: Vec<String>,
    parameters: Vec<Parameter>,
    type_params: Vec<String>,
    is_async: bool,
    class_context: Option<String>,
    start_line: usize,
    start_col: usize,
    start_byte: usize,
    end_line: usize,
    end_col: usize,
    end_byte: usize,
    source_text: String,
}

#[derive(Debug, Clone)]
struct ClassRange {
    name: String,
    start: usize,
    end: usize,
}

#[derive(Debug, Clone)]
struct InterfaceRange {
    start: usize,
    end: usize,
}

/// Public entry: given file content and file path, extract IR for each TypeScript function.
pub fn extract(content: &str, file_path: &Path) -> Result<Vec<IrFunction>> {
    let module = file_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    let file_str = file_path.display().to_string();

    let raws = parse_functions(content)?;

    let mut out = Vec::new();
    for r in raws {
        let hash = hash_source(&r.source_text);
        let qualified = match &r.class_context {
            Some(cls) => format!("{}::{}::{}", module, cls, r.name),
            None => format!("{}::{}", module, r.name),
        };
        let id = qualified.clone();

        let mut ir = IrFunction::new_minimal(
            id,
            r.name.clone(),
            qualified,
            file_str.clone(),
            module.clone(),
            r.source_text.clone(),
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
            r.visibility.clone(),
            r.modifiers.clone(),
            r.return_type.clone(),
            r.parameters.clone(),
            vec![],
            None,
            r.class_context.clone(),
        );
        ir.identity.language = "typescript".to_string();
        ir.metadata.parser = Some("reko-tsExtractor".to_string());
        ir.metadata.parser_version = Some("0.1.0".to_string());
        ir.execution.is_async = r.is_async;
        ir.context.module = Some(module.clone());
        ir.context.class = r.class_context.clone();
        ir.source.module = module.clone();
        ir.identity.kind = "function".to_string();
        // store generics in types.generic and signature.type_parameters
        if !r.type_params.is_empty() {
            ir.signature.type_parameters = r.type_params.clone();
            ir.types.generic = r.type_params.clone();
        }

        out.push(ir);
    }
    Ok(out)
}

/// JSON helper
pub fn extract_to_json(content: &str, file_path: &Path) -> Result<String> {
    let fns = extract(content, file_path)?;
    Ok(serde_json::to_string_pretty(&fns)?)
}

fn hash_source(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

fn first_non_space_col(s: &str) -> usize {
    s.chars()
        .position(|c| c != ' ' && c != '\t')
        .unwrap_or(0)
}

fn byte_to_line_col(_content: &str, line_starts: &[usize], byte: usize) -> (usize, usize) {
    let mut line = 1;
    for (idx, &start) in line_starts.iter().enumerate() {
        if start <= byte {
            line = idx + 1;
        } else {
            break;
        }
    }
    let col = byte - line_starts[line - 1] + 1;
    (line, col)
}

// ---------- brace / paren / angle matching with string/comment awareness ----------

fn find_matching_brace(content: &str, open_pos: usize) -> Option<usize> {
    let bytes = content.as_bytes();
    if open_pos >= bytes.len() || bytes[open_pos] != b'{' {
        return None;
    }
    let mut depth: i32 = 0;
    let mut in_single = false;
    let mut in_double = false;
    let mut in_template = false;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
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
        if in_single {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'\'' {
                in_single = false;
            }
            continue;
        }
        if in_double {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_double = false;
            }
            continue;
        }
        if in_template {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'`' {
                in_template = false;
            }
            continue;
        }
        if b == b'/' && next == Some(b'/') {
            in_line_comment = true;
            continue;
        }
        if b == b'/' && next == Some(b'*') {
            in_block_comment = true;
            continue;
        }
        if b == b'\'' {
            in_single = true;
            continue;
        }
        if b == b'"' {
            in_double = true;
            continue;
        }
        if b == b'`' {
            in_template = true;
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

fn find_matching_paren(content: &str, open_pos: usize) -> Option<usize> {
    let bytes = content.as_bytes();
    if open_pos >= bytes.len() || bytes[open_pos] != b'(' {
        return None;
    }
    let mut depth: i32 = 0;
    let mut in_single = false;
    let mut in_double = false;
    let mut in_template = false;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
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
        if in_single {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'\'' {
                in_single = false;
            }
            continue;
        }
        if in_double {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_double = false;
            }
            continue;
        }
        if in_template {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'`' {
                in_template = false;
            }
            continue;
        }
        if b == b'/' && next == Some(b'/') {
            in_line_comment = true;
            continue;
        }
        if b == b'/' && next == Some(b'*') {
            in_block_comment = true;
            continue;
        }
        if b == b'\'' {
            in_single = true;
            continue;
        }
        if b == b'"' {
            in_double = true;
            continue;
        }
        if b == b'`' {
            in_template = true;
            continue;
        }
        if b == b'(' {
            depth += 1;
        } else if b == b')' {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

fn find_matching_angle(content: &str, open_pos: usize) -> Option<usize> {
    let bytes = content.as_bytes();
    if open_pos >= bytes.len() || bytes[open_pos] != b'<' {
        return None;
    }
    let mut depth: i32 = 0;
    let mut in_single = false;
    let mut in_double = false;
    let mut in_template = false;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
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
        if in_single {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'\'' {
                in_single = false;
            }
            continue;
        }
        if in_double {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_double = false;
            }
            continue;
        }
        if in_template {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'`' {
                in_template = false;
            }
            continue;
        }
        if b == b'/' && next == Some(b'/') {
            in_line_comment = true;
            continue;
        }
        if b == b'/' && next == Some(b'*') {
            in_block_comment = true;
            continue;
        }
        if b == b'\'' {
            in_single = true;
            continue;
        }
        if b == b'"' {
            in_double = true;
            continue;
        }
        if b == b'`' {
            in_template = true;
            continue;
        }
        if b == b'<' {
            depth += 1;
        } else if b == b'>' {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

fn find_next_open_brace(content: &str, start: usize) -> Option<usize> {
    let bytes = content.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    let mut in_template = false;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let mut escape = false;
    let mut i = start;
    while i < bytes.len() {
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
            i += 1;
            continue;
        }
        if in_block_comment {
            if b == b'*' && next == Some(b'/') {
                in_block_comment = false;
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if in_single {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'\'' {
                in_single = false;
            }
            i += 1;
            continue;
        }
        if in_double {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_double = false;
            }
            i += 1;
            continue;
        }
        if in_template {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'`' {
                in_template = false;
            }
            i += 1;
            continue;
        }
        if b == b'/' && next == Some(b'/') {
            in_line_comment = true;
            i += 2;
            continue;
        }
        if b == b'/' && next == Some(b'*') {
            in_block_comment = true;
            i += 2;
            continue;
        }
        if b == b'\'' {
            in_single = true;
            i += 1;
            continue;
        }
        if b == b'"' {
            in_double = true;
            i += 1;
            continue;
        }
        if b == b'`' {
            in_template = true;
            i += 1;
            continue;
        }
        if b == b'{' {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn find_next_char_aware(content: &str, start: usize, target: u8) -> Option<usize> {
    let bytes = content.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    let mut in_template = false;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let mut escape = false;
    let mut i = start;
    while i < bytes.len() {
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
            i += 1;
            continue;
        }
        if in_block_comment {
            if b == b'*' && next == Some(b'/') {
                in_block_comment = false;
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if in_single {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'\'' {
                in_single = false;
            }
            i += 1;
            continue;
        }
        if in_double {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_double = false;
            }
            i += 1;
            continue;
        }
        if in_template {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'`' {
                in_template = false;
            }
            i += 1;
            continue;
        }
        if b == b'/' && next == Some(b'/') {
            in_line_comment = true;
            i += 2;
            continue;
        }
        if b == b'/' && next == Some(b'*') {
            in_block_comment = true;
            i += 2;
            continue;
        }
        if b == b'\'' {
            in_single = true;
            i += 1;
            continue;
        }
        if b == b'"' {
            in_double = true;
            i += 1;
            continue;
        }
        if b == b'`' {
            in_template = true;
            i += 1;
            continue;
        }
        if b == target {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn find_arrow(content: &str, start: usize) -> Option<usize> {
    let bytes = content.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    let mut in_template = false;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let mut escape = false;
    let mut i = start;
    while i + 1 < bytes.len() {
        let b = bytes[i];
        let next = bytes[i + 1];
        if in_line_comment {
            if b == b'\n' {
                in_line_comment = false;
            }
            i += 1;
            continue;
        }
        if in_block_comment {
            if b == b'*' && next == b'/' {
                in_block_comment = false;
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if in_single {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'\'' {
                in_single = false;
            }
            i += 1;
            continue;
        }
        if in_double {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_double = false;
            }
            i += 1;
            continue;
        }
        if in_template {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'`' {
                in_template = false;
            }
            i += 1;
            continue;
        }
        if b == b'/' && next == b'/' {
            in_line_comment = true;
            i += 2;
            continue;
        }
        if b == b'/' && next == b'*' {
            in_block_comment = true;
            i += 2;
            continue;
        }
        if b == b'\'' {
            in_single = true;
            i += 1;
            continue;
        }
        if b == b'"' {
            in_double = true;
            i += 1;
            continue;
        }
        if b == b'`' {
            in_template = true;
            i += 1;
            continue;
        }
        if b == b'=' && next == b'>' {
            return Some(i);
        }
        i += 1;
    }
    None
}

// ---------- class / interface detection ----------

fn parse_classes(content: &str, line_starts: &[usize]) -> Vec<ClassRange> {
    let mut res = Vec::new();
    let lines: Vec<&str> = content.lines().collect();
    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with("//")
            || trimmed.starts_with("/*")
            || trimmed.starts_with("*")
        {
            continue;
        }
        // find "class " as word
        if let Some(pos) = line.find("class ") {
            // word boundary check
            if pos > 0 {
                let prev = line.as_bytes()[pos - 1];
                if (prev as char).is_alphanumeric() || prev == b'_' || prev == b'$' {
                    continue;
                }
            }
            let after_start = pos + 6;
            let after = &line[after_start..];
            let after_trim = after.trim_start();
            let name: String = after_trim
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
                .collect();
            if name.is_empty() {
                continue;
            }
            let start_byte = line_starts[idx] + pos;
            if let Some(open) = find_next_open_brace(content, start_byte) {
                if let Some(close) = find_matching_brace(content, open) {
                    res.push(ClassRange {
                        name,
                        start: open,
                        end: close,
                    });
                }
            }
        }
    }
    res.sort_by_key(|c| c.start);
    res
}

fn parse_interfaces(content: &str, line_starts: &[usize]) -> Vec<InterfaceRange> {
    let mut res = Vec::new();
    let lines: Vec<&str> = content.lines().collect();
    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with("//")
            || trimmed.starts_with("/*")
            || trimmed.starts_with("*")
        {
            continue;
        }
        if let Some(pos) = line.find("interface ") {
            if pos > 0 {
                let prev = line.as_bytes()[pos - 1];
                if (prev as char).is_alphanumeric() || prev == b'_' || prev == b'$' {
                    continue;
                }
            }
            let start_byte = line_starts[idx] + pos;
            if let Some(open) = find_next_open_brace(content, start_byte) {
                if let Some(close) = find_matching_brace(content, open) {
                    res.push(InterfaceRange {
                        start: open,
                        end: close,
                    });
                }
            }
        }
    }
    res
}

fn find_enclosing_class(byte: usize, classes: &[ClassRange]) -> Option<String> {
    let mut best: Option<&ClassRange> = None;
    for c in classes {
        if byte > c.start && byte < c.end {
            if let Some(b) = best {
                if (c.end - c.start) < (b.end - b.start) {
                    best = Some(c);
                }
            } else {
                best = Some(c);
            }
        }
    }
    best.map(|c| c.name.clone())
}

fn is_inside_interface(byte: usize, interfaces: &[InterfaceRange]) -> bool {
    for it in interfaces {
        if byte > it.start && byte < it.end {
            return true;
        }
    }
    false
}

// ---------- params ----------

fn parse_params_ts(s: &str) -> Vec<Parameter> {
    if s.trim().is_empty() {
        return vec![];
    }
    let parts = split_params_ts(s);
    let mut out = Vec::new();
    for part in parts {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        // handle rest params and optional etc.
        // Remove default value: split at '=' outside brackets (simple)
        let without_default = p.split('=').next().unwrap_or(p).trim();
        // For TS, param may be like "a: string", "b?: number", "...rest: string[]"
        // Extract before colon as name part
        // If destructuring {a,b}: Type, keep whole destructuring as name? Use raw before colon first char
        let name_and_type: (String, String) = if without_default.starts_with('{') || without_default.starts_with('[') {
            // destructuring: try to find colon after closing brace/bracket
            // Find last colon that is not inside destructuring? Simple: find colon after last } or ]
            // Use rfind(':')
            if let Some(colon) = without_default.rfind(':') {
                let before = without_default[..colon].trim().to_string();
                let typ = without_default[colon + 1..].trim().to_string();
                (before, if typ.is_empty() { "any".to_string() } else { typ })
            } else {
                (without_default.to_string(), "any".to_string())
            }
        } else {
            if let Some(colon) = without_default.find(':') {
                let n = without_default[..colon].trim();
                let t = without_default[colon + 1..].trim();
                // n may contain "?" for optional: "b?"
                let n_clean = n.trim_end_matches('?').trim().to_string();
                let mut name_clean = n_clean;
                if name_clean.starts_with("...") {
                    name_clean = name_clean[3..].trim().to_string();
                }
                // also handle modifiers like "public a: string" inside constructor params
                // take last token as name
                if name_clean.contains(' ') {
                    if let Some(last) = name_clean.split_whitespace().last() {
                        name_clean = last.to_string();
                    }
                }
                (name_clean, if t.is_empty() { "any".to_string() } else { t.to_string() })
            } else {
                // no type annotation, name may have ? or ...
                let mut name_clean = without_default.trim_end_matches('?').trim().to_string();
                if name_clean.starts_with("...") {
                    name_clean = name_clean[3..].trim().to_string();
                }
                if name_clean.contains(' ') {
                    if let Some(last) = name_clean.split_whitespace().last() {
                        name_clean = last.to_string();
                    }
                }
                (name_clean, "any".to_string())
            }
        };
        let (name_clean, typ) = name_and_type;
        if name_clean.is_empty() {
            continue;
        }
        // skip if name is 'this' weird?
        out.push(Parameter { name: name_clean, typ });
    }
    out
}

fn split_params_ts(s: &str) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut depth_paren: i32 = 0;
    let mut depth_bracket: i32 = 0;
    let mut depth_brace: i32 = 0;
    let mut depth_angle: i32 = 0;
    let mut in_single = false;
    let mut in_double = false;
    let mut in_template = false;
    let mut escape = false;

    for ch in s.chars() {
        if escape {
            current.push(ch);
            escape = false;
            continue;
        }
        if ch == '\\' && (in_single || in_double || in_template) {
            escape = true;
            current.push(ch);
            continue;
        }
        if in_single {
            if ch == '\'' {
                in_single = false;
            }
            current.push(ch);
            continue;
        }
        if in_double {
            if ch == '"' {
                in_double = false;
            }
            current.push(ch);
            continue;
        }
        if in_template {
            if ch == '`' {
                in_template = false;
            }
            current.push(ch);
            continue;
        }
        match ch {
            '\'' => {
                in_single = true;
                current.push(ch);
            }
            '"' => {
                in_double = true;
                current.push(ch);
            }
            '`' => {
                in_template = true;
                current.push(ch);
            }
            '(' => {
                depth_paren += 1;
                current.push(ch);
            }
            ')' => {
                depth_paren -= 1;
                current.push(ch);
            }
            '[' => {
                depth_bracket += 1;
                current.push(ch);
            }
            ']' => {
                depth_bracket -= 1;
                current.push(ch);
            }
            '{' => {
                depth_brace += 1;
                current.push(ch);
            }
            '}' => {
                depth_brace -= 1;
                current.push(ch);
            }
            '<' => {
                depth_angle += 1;
                current.push(ch);
            }
            '>' => {
                if depth_angle > 0 {
                    depth_angle -= 1;
                }
                current.push(ch);
            }
            ',' => {
                if depth_paren == 0 && depth_bracket == 0 && depth_brace == 0 && depth_angle == 0 {
                    parts.push(current.trim().to_string());
                    current.clear();
                } else {
                    current.push(ch);
                }
            }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        parts.push(current.trim().to_string());
    }
    parts
}

fn extract_return_type_after_paren(content: &str, close_paren: usize, open_brace_hint: usize) -> Option<String> {
    if close_paren + 1 >= content.len() {
        return None;
    }
    let end = if open_brace_hint < content.len() {
        open_brace_hint
    } else {
        content.len()
    };
    if close_paren + 1 >= end {
        return None;
    }
    let between = content[close_paren + 1..end].trim();
    // return type is after ':' before '{' or '=>' ; between may contain ": Type"
    // Remove leading whitespace, handle case where between starts with ":"
    // Also need to handle generic after colon with nested.
    // Simple: if contains ':', take substring after first ':' up to end, trim trailing ';' etc.
    if let Some(colon_pos) = between.find(':') {
        let after = between[colon_pos + 1..].trim();
        // after may still contain trailing characters like " {" or " =>"
        // Strip: if contains "=>", cut before? For function decl, between ends at "{" so no =>. For arrow, we pass open_brace? Actually for arrow we call with arrow as hint? For arrow we use find_arrow, but this helper used for decl/method where hint is brace.
        // So just clean.
        let clean = after.trim().trim_end_matches('{').trim().trim_end_matches(';').trim();
        // If clean still contains "{" due to not trimming properly, take before '{'
        let clean = clean.split('{').next().unwrap_or(clean).trim();
        // Also handle "=>" remains
        let clean = clean.split("=>").next().unwrap_or(clean).trim();
        if clean.is_empty() {
            None
        } else {
            Some(clean.to_string())
        }
    } else {
        None
    }
}

fn parse_generics(segment: &str) -> Vec<String> {
    let s = segment.trim();
    if s.is_empty() {
        return vec![];
    }
    // split by ',' respecting nested brackets < > , [] etc? Generics may contain constraints like "T extends string"
    // Keep each param name as first token before space/extends
    let parts = split_params_ts(s);
    let mut out = Vec::new();
    for p in parts {
        let t = p.trim();
        if t.is_empty() {
            continue;
        }
        // Take identifier before space or extends
        let first = t.split_whitespace().next().unwrap_or(t).trim_matches(|c| c == ',' || c == '<' || c == '>').to_string();
        // Also split at "extends"
        let name = first.split("extends").next().unwrap_or(&first).trim().to_string();
        if !name.is_empty() {
            out.push(name);
        }
    }
    out
}

// ---------- main parse ----------

fn parse_functions(content: &str) -> Result<Vec<TsFunctionRaw>> {
    let mut line_starts: Vec<usize> = vec![0];
    for (i, b) in content.bytes().enumerate() {
        if b == b'\n' {
            line_starts.push(i + 1);
        }
    }
    let lines: Vec<&str> = content.lines().collect();
    if content.is_empty() {
        return Ok(Vec::new());
    }
    let classes = parse_classes(content, &line_starts);
    let interfaces = parse_interfaces(content, &line_starts);
    let mut result = Vec::new();
    let mut visited: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut i = 0usize;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with("//")
            || trimmed.starts_with("/*")
            || trimmed.starts_with("*")
            || trimmed.starts_with("import ")
        {
            i += 1;
            continue;
        }
        let start_byte_candidate = line_starts[i] + first_non_space_col(line);
        // skip if inside interface
        if is_inside_interface(start_byte_candidate, &interfaces) {
            i += 1;
            continue;
        }
        // Try arrow: const/let/var with => (TypeScript variant)
        if trimmed.contains("=>") && (trimmed.contains("const ") || trimmed.contains("let ") || trimmed.contains("var ") || trimmed.contains('=')) {
            if let Some((raw, _)) = try_parse_arrow(content, &line_starts, &lines, i, &classes, &interfaces) {
                if !visited.contains(&raw.start_byte) && !is_inside_interface(raw.start_byte, &interfaces) {
                    visited.insert(raw.start_byte);
                    let next_i = raw.end_line;
                    result.push(raw);
                    i = next_i;
                    continue;
                }
            }
        }
        // Try function declaration
        if trimmed.contains("function") {
            if let Some((raw, _)) = try_parse_function_decl(content, &line_starts, &lines, i, &classes, &interfaces) {
                if !visited.contains(&raw.start_byte) && !is_inside_interface(raw.start_byte, &interfaces) {
                    visited.insert(raw.start_byte);
                    let next_i = raw.end_line;
                    result.push(raw);
                    i = next_i;
                    continue;
                }
            }
        }
        // Try class method (only if inside class)
        let enclosing = find_enclosing_class(start_byte_candidate, &classes);
        if enclosing.is_some() {
            // avoid interface already handled
            if let Some((raw, _)) = try_parse_class_method(content, &line_starts, &lines, i, &classes, &interfaces) {
                if !visited.contains(&raw.start_byte) && !is_inside_interface(raw.start_byte, &interfaces) {
                    visited.insert(raw.start_byte);
                    let next_i = raw.end_line;
                    result.push(raw);
                    i = next_i;
                    continue;
                }
            }
        }
        i += 1;
    }
    result.sort_by_key(|r| r.start_byte);
    Ok(result)
}

fn try_parse_function_decl(
    content: &str,
    line_starts: &[usize],
    lines: &[&str],
    i: usize,
    classes: &[ClassRange],
    _interfaces: &[InterfaceRange],
) -> Option<(TsFunctionRaw, usize)> {
    let line = lines[i];
    let func_pos = line.find("function")?;
    // Ensure '=' not immediately before function with invalid? For const assignment, skip (already handled via arrow/const)
    // If line contains '=' before function and pattern is const expr, we skip decl to avoid double detection for `const foo = function`
    if let Some(eq) = line.find('=') {
        if eq < func_pos {
            // Check if it's assignment form `const x = function` not declaration; we treat assignment variant separately?
            // But task spec mentions handling `function name<T>(...)` primarily; we can still allow export function etc.
            // To avoid double counting, if name before eq is const/let/var, then skip.
            let before = &line[..eq];
            if before.contains("const") || before.contains("let") || before.contains("var") {
                return None;
            }
        }
    }
    let before_func = &line[..func_pos];
    // Validate that "function" is word: preceding char not alphanum
    if func_pos > 0 {
        let prev = line.as_bytes()[func_pos - 1];
        if (prev as char).is_alphanumeric() || prev == b'_' || prev == b'$' {
            return None;
        }
    }
    let after = &line[func_pos + 8..];
    let after_trim = after.trim_start();
    let is_gen = after_trim.starts_with('*');
    let after_name_start = if is_gen {
        after_trim[1..].trim_start()
    } else {
        after_trim
    };
    let name: String = after_name_start
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
        .collect();
    if name.is_empty() {
        return None;
    }
    // generics
    let mut type_params = Vec::new();
    // Use line-based position to find "<" after name if exists
    let name_byte_in_line = line.find(&name).unwrap_or(func_pos);
    let name_end_byte = line_starts[i] + name_byte_in_line + name.len();
    let mut cursor = name_end_byte;
    // skip whitespace
    while cursor < content.len() && (content.as_bytes()[cursor] == b' ' || content.as_bytes()[cursor] == b'\t') {
        cursor += 1;
    }
    if cursor < content.len() && content.as_bytes()[cursor] == b'<' {
        if let Some(close_angle) = find_matching_angle(content, cursor) {
            let inner = content[cursor + 1..close_angle].to_string();
            type_params = parse_generics(&inner);
            cursor = close_angle + 1;
        }
    }
    // find '(' from cursor
    let open_paren = find_next_char_aware(content, cursor, b'(')?;
    let close_paren = find_matching_paren(content, open_paren)?;
    let params_str = if close_paren > open_paren + 1 {
        content[open_paren + 1..close_paren].to_string()
    } else {
        String::new()
    };
    let params = parse_params_ts(&params_str);
    let open_brace = find_next_open_brace(content, close_paren + 1)?;
    let close_brace = find_matching_brace(content, open_brace)?;
    // Ensure not semicolon before brace (overload)
    let between = content[close_paren + 1..open_brace].to_string();
    if between.contains(';') && !between.contains(':') {
        // semicolon before brace likely overload; skip
        // But if we found brace, it's impl; the ';' could be from next line? We strictly found brace, so it's impl; keep.
    }
    let return_type = extract_return_type_after_paren(content, close_paren, open_brace);

    let is_async = before_func.contains("async");
    let is_export = before_func.contains("export") || line.trim_start().starts_with("export");
    let is_generator = is_gen;

    let start_byte = line_starts[i] + first_non_space_col(line);
    let start_line = i + 1;
    let start_col = first_non_space_col(line) + 1;
    let (end_line, end_col) = byte_to_line_col(content, line_starts, close_brace);
    let source_text = content[start_byte..=close_brace].to_string();
    let class_ctx = find_enclosing_class(start_byte, classes);

    let mut modifiers = Vec::new();
    if is_async {
        modifiers.push("async".to_string());
    }
    if is_export {
        modifiers.push("export".to_string());
    }
    if is_generator {
        modifiers.push("generator".to_string());
    }
    if line.contains("default") && before_func.contains("default") {
        modifiers.push("default".to_string());
    }

    let visibility = "public".to_string();

    let raw = TsFunctionRaw {
        name,
        return_type,
        visibility,
        modifiers,
        parameters: params,
        type_params,
        is_async,
        class_context: class_ctx,
        start_line,
        start_col,
        start_byte,
        end_line,
        end_col,
        end_byte: close_brace,
        source_text,
    };
    Some((raw, end_line - 1))
}

fn try_parse_arrow(
    content: &str,
    line_starts: &[usize],
    lines: &[&str],
    i: usize,
    classes: &[ClassRange],
    _interfaces: &[InterfaceRange],
) -> Option<(TsFunctionRaw, usize)> {
    let line = lines[i];
    let eq_pos = line.find('=')?;
    let arrow_rel = line.find("=>")?;
    if eq_pos > arrow_rel {
        return None;
    }
    let before_eq = &line[..eq_pos];
    // Find name: last identifier before '='
    // Handle export: "export const foo ="
    let tokens: Vec<&str> = before_eq.split_whitespace().collect();
    let mut name_candidate: Option<String> = None;
    // iterate reverse, find first identifier not in keywords
    let kw = ["const", "let", "var", "export", "default", "async"];
    for tok in tokens.iter().rev() {
        let clean = tok.trim_matches(|c: char| c == ';' || c == ',' || c == ':').trim();
        // may contain type annotation after name like "foo: string"? For arrow const with type: "const foo: MyType = ..."
        let before_colon = clean.split(':').next().unwrap_or(clean).trim();
        let ident: String = before_colon
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
            .collect();
        // Actually need last word token might be "foo:" etc.
        // Better split token by ':' to get identifier
        // Alternative: take whole last token and extract identifier chars from start
        let ident2: String = before_colon
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
            .collect();
        if ident2.is_empty() {
            continue;
        }
        if kw.contains(&ident2.as_str()) {
            continue;
        }
        // identifier must be alphanum start
        if ident2.chars().next().map(|c| c.is_alphabetic() || c == '_' || c == '$').unwrap_or(false) {
            name_candidate = Some(ident2);
            break;
        }
        // Also handle case where token is identifier with extra chars: e.g., "foo"
        if !ident.is_empty() && !kw.contains(&ident.as_str()) {
            name_candidate = Some(ident);
            break;
        }
    }
    let name_ident = name_candidate?;
    // Invalid keywords check
    if kw.contains(&name_ident.as_str()) {
        return None;
    }

    let before_arrow = &line[..arrow_rel];
    let is_async = before_arrow.contains("async");
    let is_export = line.contains("export");

    // Find generics and params between eq and arrow
    let eq_byte = line_starts[i] + eq_pos;
    let arrow_byte = line_starts[i] + arrow_rel;
    // Search for '(' between eq and arrow
    let open_opt = find_next_char_aware(content, eq_byte + 1, b'(');
    let mut params = Vec::new();
    let mut type_params = Vec::new();
    let mut return_type: Option<String> = None;

    if let Some(open) = open_opt {
        if open < arrow_byte {
            if let Some(close) = find_matching_paren(content, open) {
                if close < arrow_byte {
                    let params_str = if close > open + 1 {
                        content[open + 1..close].to_string()
                    } else {
                        String::new()
                    };
                    params = parse_params_ts(&params_str);
                    // Check for generic before '(' : e.g., "<T>"
                    // Look between eq and open for "<"
                    // Find '<' between eq_byte and open
                    let mut cursor = eq_byte + 1;
                    while cursor < open {
                        if content.as_bytes()[cursor] == b'<' {
                            if let Some(ca) = find_matching_angle(content, cursor) {
                                if ca < open {
                                    let inner = content[cursor + 1..ca].to_string();
                                    type_params = parse_generics(&inner);
                                    break;
                                }
                            }
                        }
                        cursor += 1;
                    }
                    // return type between close and arrow
                    if close + 1 < arrow_byte {
                        let between = content[close + 1..arrow_byte].to_string();
                        if let Some(colon) = between.find(':') {
                            let after = between[colon + 1..].trim();
                            // cut at possible generic? keep whole
                            let rt = after.trim().trim_end_matches(';').trim().to_string();
                            if !rt.is_empty() {
                                return_type = Some(rt);
                            }
                        }
                    }
                } else {
                    return None;
                }
            } else {
                return None;
            }
        }
    } else {
        // single param without parens: e.g., x => { ; handle type?  x: number => 
        let between = content[eq_byte + 1..arrow_byte].trim().to_string();
        let between_no_async = between.trim().trim_start_matches("async").trim().to_string();
        // Remove possible return type? single param arrow typically no return type before =>
        // But TS may have "x: number =>" where colon is part of param type
        // We'll treat as single param.
        let ptrim = between_no_async.trim();
        // Remove generics? skip
        let param_clean = ptrim.split(':').next().unwrap_or(ptrim).trim().trim_end_matches('?').to_string();
        let name_clean = param_clean.trim_start_matches("...").trim().to_string();
        // Handle generics single? ignore
        if !name_clean.is_empty() && !name_clean.contains(' ') && !name_clean.contains(',') {
            let typ = if let Some(colon) = ptrim.find(':') {
                ptrim[colon + 1..].trim().to_string()
            } else {
                "any".to_string()
            };
            let typ = if typ.is_empty() { "any".to_string() } else { typ };
            params.push(Parameter { name: name_clean, typ });
        } else if !ptrim.is_empty() {
            // fallback parse via parse_params_ts
            params = parse_params_ts(&ptrim);
        }
    }

    let open_brace = find_next_open_brace(content, arrow_byte + 2)?;
    let close_brace = find_matching_brace(content, open_brace)?;

    let start_byte = line_starts[i] + first_non_space_col(line);
    let start_line = i + 1;
    let start_col = first_non_space_col(line) + 1;
    let (end_line, end_col) = byte_to_line_col(content, line_starts, close_brace);
    let source_text = content[start_byte..=close_brace].to_string();
    let class_ctx = find_enclosing_class(start_byte, classes);

    let mut modifiers = Vec::new();
    if is_async {
        modifiers.push("async".to_string());
    }
    if is_export {
        modifiers.push("export".to_string());
    }

    let raw = TsFunctionRaw {
        name: name_ident,
        return_type,
        visibility: "public".to_string(),
        modifiers,
        parameters: params,
        type_params,
        is_async,
        class_context: class_ctx,
        start_line,
        start_col,
        start_byte,
        end_line,
        end_col,
        end_byte: close_brace,
        source_text,
    };
    Some((raw, end_line - 1))
}

fn try_parse_class_method(
    content: &str,
    line_starts: &[usize],
    lines: &[&str],
    i: usize,
    classes: &[ClassRange],
    interfaces: &[InterfaceRange],
) -> Option<(TsFunctionRaw, usize)> {
    let line = lines[i];
    let trimmed = line.trim();
    // Quick rejects
    if trimmed.contains("function") || trimmed.contains("=>") || trimmed.contains("class ") || trimmed.contains("interface ") {
        return None;
    }
    if !trimmed.contains('(') || !trimmed.contains(')') {
        return None;
    }
    if trimmed.contains('=') {
        return None;
    }
    // reject if start is decorator or comment
    if trimmed.starts_with("@") {
        return None;
    }
    // Must have '{' eventually but overload without body should be skipped (ends with ';')
    // Check if trimmed ends with ';' and no '{' on same line before => overload
    // We'll search for brace anyway, but if semicolon before brace without brace on same line, it's overload -> skip
    // Detect semicolon between ')' and '{'?
    let mut rest = trimmed;
    let mut is_static = false;
    let mut is_async = false;
    let mut is_abstract = false;
    let mut is_override = false;
    let mut visibility = "public".to_string();
    let mut is_getter = false;
    let mut is_setter = false;

    // Parse modifiers in order - consume leading keywords
    loop {
        let mut consumed = false;
        if rest.starts_with("public ") {
            visibility = "public".to_string();
            rest = rest[7..].trim_start();
            consumed = true;
        } else if rest.starts_with("private ") {
            visibility = "private".to_string();
            rest = rest[8..].trim_start();
            consumed = true;
        } else if rest.starts_with("protected ") {
            visibility = "protected".to_string();
            rest = rest[10..].trim_start();
            consumed = true;
        } else if rest.starts_with("static ") {
            is_static = true;
            rest = rest[7..].trim_start();
            consumed = true;
        } else if rest.starts_with("async ") {
            is_async = true;
            rest = rest[6..].trim_start();
            consumed = true;
        } else if rest.starts_with("abstract ") {
            is_abstract = true;
            rest = rest[9..].trim_start();
            consumed = true;
        } else if rest.starts_with("override ") {
            is_override = true;
            rest = rest[9..].trim_start();
            consumed = true;
        } else if rest.starts_with("readonly ") {
            rest = rest[9..].trim_start();
            consumed = true;
        } else if rest.starts_with("get ") {
            is_getter = true;
            rest = rest[4..].trim_start();
            consumed = true;
        } else if rest.starts_with("set ") {
            is_setter = true;
            rest = rest[4..].trim_start();
            consumed = true;
        }
        if !consumed {
            break;
        }
    }
    if rest.starts_with('*') {
        rest = rest[1..].trim_start();
    }
    // Now name
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
        .collect();
    if name.is_empty() {
        return None;
    }
    let keywords = [
        "if", "for", "while", "switch", "catch", "else", "return", "const", "let", "var", "import", "export", "default", "function", "class", "interface", "type", "enum",
    ];
    if keywords.contains(&name.as_str()) {
        return None;
    }
    let after_name = &rest[name.len()..];
    if !after_name.trim_start().starts_with('<') && !after_name.trim_start().starts_with('(') {
        return None;
    }
    // Find name byte
    let name_idx = line.find(&name)?;
    let name_byte = line_starts[i] + name_idx;
    if is_inside_interface(name_byte, interfaces) {
        return None;
    }
    // generics handling
    let mut type_params = Vec::new();
    let mut cursor = name_byte + name.len();
    while cursor < content.len() && (content.as_bytes()[cursor] == b' ' || content.as_bytes()[cursor] == b'\t') {
        cursor += 1;
    }
    if cursor < content.len() && content.as_bytes()[cursor] == b'<' {
        if let Some(close_angle) = find_matching_angle(content, cursor) {
            let inner = content[cursor + 1..close_angle].to_string();
            type_params = parse_generics(&inner);
            cursor = close_angle + 1;
        }
    }
    let open_paren = find_next_char_aware(content, cursor, b'(')?;
    let close_paren = find_matching_paren(content, open_paren)?;
    let params_str = if close_paren > open_paren + 1 {
        content[open_paren + 1..close_paren].to_string()
    } else {
        String::new()
    };
    let params = parse_params_ts(&params_str);
    // Check for semicolon overload before brace
    let open_brace = find_next_open_brace(content, close_paren + 1)?;
    // If between close_paren and open_brace there's a ';' without colon handling? For TS overload, it ends with ';' and no brace on that range; but we found brace beyond, need to ensure immediate? 
    // Take content between close_paren and open_brace, if it contains ';' and before ';' there is no ':', it's overload but we found later brace belonging to next overload impl? Actually overloads have ';' and then next line is impl with brace. Our current method would incorrectly treat first overload as impl with next brace. Heuristic: if between contains ';', then this is overload signature, skip; the impl will be later line with body.
    let between = content[close_paren + 1..open_brace].to_string();
    if between.contains(';') {
        // Check if between trimmed is just ";": e.g., " : void;"
        // If semicolon appears before brace, likely overload, skip this detection
        // But for impl, between is like ": void {" -> no ';'
        // So skip
        // Use heuristic: if ';' index < '{' index (we already at open brace, so ';' before brace means overload)
        return None;
    }
    let close_brace = find_matching_brace(content, open_brace)?;
    let return_type = extract_return_type_after_paren(content, close_paren, open_brace);

    let start_byte = line_starts[i] + first_non_space_col(line);
    let start_line = i + 1;
    let start_col = first_non_space_col(line) + 1;
    let (end_line, end_col) = byte_to_line_col(content, line_starts, close_brace);
    let source_text = content[start_byte..=close_brace].to_string();
    let class_ctx = find_enclosing_class(start_byte, classes);

    let mut modifiers = Vec::new();
    if is_static {
        modifiers.push("static".to_string());
    }
    if is_async {
        modifiers.push("async".to_string());
    }
    if is_abstract {
        modifiers.push("abstract".to_string());
    }
    if is_override {
        modifiers.push("override".to_string());
    }
    if is_getter {
        modifiers.push("get".to_string());
    }
    if is_setter {
        modifiers.push("set".to_string());
    }
    if visibility != "public" {
        modifiers.push(visibility.clone());
    }

    let raw = TsFunctionRaw {
        name,
        return_type,
        visibility,
        modifiers,
        parameters: params,
        type_params,
        is_async,
        class_context: class_ctx,
        start_line,
        start_col,
        start_byte,
        end_line,
        end_col,
        end_byte: close_brace,
        source_text,
    };
    Some((raw, end_line - 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const SAMPLE_FUNCTION_GENERIC: &str = r#"export async function fetchData<T>(url: string): Promise<T> {
    const res = await fetch(url);
    return res.json();
}
function add(a: number, b: number): number {
    return a + b;
}
"#;

    const SAMPLE_ARROW: &str = r#"const sum = (a: number, b: number): number => {
    return a + b;
}
export const identity = <T>(x: T): T => {
    return x;
}
const inc = async (x: number): Promise<number> => {
    return x + 1;
}
"#;

    const SAMPLE_CLASS: &str = r#"class Calculator {
    constructor(private val: number) {}
    public add(b: number): number {
        return this.val + b;
    }
    private static create<T>(val: T): Calculator {
        return new Calculator(val as unknown as number);
    }
    async fetchValue(): Promise<number> {
        return this.val;
    }
}
interface Skipped {
    foo(): void;
    bar(a: string): number;
}
"#;

    #[test]
    fn extracts_function_with_generics() {
        let fns = extract(SAMPLE_FUNCTION_GENERIC, Path::new("sample.ts")).unwrap();
        assert_eq!(fns.len(), 2);
        let fetch = fns.iter().find(|f| f.identity.name == "fetchData").unwrap();
        assert_eq!(fetch.identity.qualified_name, "sample::fetchData");
        assert_eq!(fetch.signature.return_type.as_deref(), Some("Promise<T>"));
        assert!(fetch.execution.is_async);
        assert!(fetch.declaration.modifiers.contains(&"async".to_string()));
        assert!(fetch.declaration.modifiers.contains(&"export".to_string()));
        assert_eq!(fetch.signature.parameters.len(), 1);
        assert_eq!(fetch.signature.parameters[0].name, "url");
        assert_eq!(fetch.signature.parameters[0].typ, "string");
        assert_eq!(fetch.signature.type_parameters, vec!["T"]);
        assert!(fetch.source.hash.starts_with("sha256:"));
        assert_eq!(fetch.identity.language, "typescript");
        assert_eq!(fetch.metadata.parser.as_deref(), Some("reko-tsExtractor"));

        let add = fns.iter().find(|f| f.identity.name == "add").unwrap();
        assert_eq!(add.signature.return_type.as_deref(), Some("number"));
        assert_eq!(add.signature.parameters.len(), 2);
    }

    #[test]
    fn extracts_arrow_with_types() {
        let fns = extract(SAMPLE_ARROW, Path::new("arrow.ts")).unwrap();
        assert_eq!(fns.len(), 3);
        let sum = fns.iter().find(|f| f.identity.name == "sum").unwrap();
        assert_eq!(sum.identity.qualified_name, "arrow::sum");
        assert_eq!(sum.signature.parameters.len(), 2);
        assert_eq!(sum.signature.parameters[0].typ, "number");
        assert_eq!(sum.signature.return_type.as_deref(), Some("number"));

        let id = fns.iter().find(|f| f.identity.name == "identity").unwrap();
        assert_eq!(id.signature.type_parameters, vec!["T"]);
        assert_eq!(id.signature.return_type.as_deref(), Some("T"));
        assert!(id.declaration.modifiers.contains(&"export".to_string()));

        let inc = fns.iter().find(|f| f.identity.name == "inc").unwrap();
        assert!(inc.execution.is_async);
        assert_eq!(inc.signature.return_type.as_deref(), Some("Promise<number>"));
    }

    #[test]
    fn extracts_class_and_skips_interface() {
        let fns = extract(SAMPLE_CLASS, Path::new("calc.ts")).unwrap();
        // constructor is considered method? Our parser treats constructor as method; but constructor has no return type and uses private param modifier
        // It should be extracted as "constructor"
        // plus add, create, fetchValue => total 4 if constructor counted
        // Check that interface methods foo/bar are NOT extracted
        assert!(fns.iter().any(|f| f.identity.name == "add"));
        assert!(fns.iter().any(|f| f.identity.name == "create"));
        assert!(fns.iter().any(|f| f.identity.name == "fetchValue"));
        assert!(!fns.iter().any(|f| f.identity.name == "foo"));
        assert!(!fns.iter().any(|f| f.identity.name == "bar"));
        let add = fns.iter().find(|f| f.identity.name == "add").unwrap();
        assert_eq!(add.identity.qualified_name, "calc::Calculator::add");
        assert_eq!(add.context.class.as_deref(), Some("Calculator"));
        assert_eq!(add.declaration.visibility, "public");
        assert_eq!(add.signature.return_type.as_deref(), Some("number"));

        let create = fns.iter().find(|f| f.identity.name == "create").unwrap();
        assert!(create.declaration.modifiers.contains(&"static".to_string()));
        assert_eq!(create.declaration.visibility, "private");
        assert_eq!(create.signature.type_parameters, vec!["T"]);

        let fetch = fns.iter().find(|f| f.identity.name == "fetchValue").unwrap();
        assert!(fetch.execution.is_async);
        assert_eq!(fetch.signature.return_type.as_deref(), Some("Promise<number>"));
    }

    #[test]
    fn brace_matching_with_strings_and_comments() {
        let content = r#"function tricky(a: string): void {
    let s = "} not a brace {";
    let t = '} still {';
    // comment with }
    /* block } */
    return;
}
"#;
        let fns = extract(content, Path::new("tricky.ts")).unwrap();
        assert_eq!(fns.len(), 1);
        assert_eq!(fns[0].identity.name, "tricky");
        assert!(fns[0].source.source_text.contains("return;"));
        assert!(fns[0].source.location.end.line >= 5);
        assert!(fns[0].source.hash.starts_with("sha256:"));
    }

    #[test]
    fn location_and_hash_and_qualified() {
        let content = "export function greet(name: string): string {\n    return `hello ${name}`;\n}\n";
        let fns = extract(content, Path::new("hello.ts")).unwrap();
        assert_eq!(fns.len(), 1);
        let g = &fns[0];
        assert_eq!(g.identity.qualified_name, "hello::greet");
        assert!(g.source.hash.starts_with("sha256:"));
        assert_eq!(g.source.location.start.line, 1);
        assert!(g.source.location.start.byte < g.source.location.end.byte);
        // extract_to_json works
        let json = extract_to_json(content, Path::new("hello.ts")).unwrap();
        assert!(json.contains("greet"));
        assert!(json.contains("typescript"));
    }
}
