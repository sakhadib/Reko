use crate::ir::{IrFunction, Parameter, Position};
use anyhow::Result;
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Debug, Clone)]
struct TsxFunctionRaw {
    name: String,
    return_type: Option<String>,
    visibility: String,
    modifiers: Vec<String>,
    parameters: Vec<Parameter>,
    type_params: Vec<String>,
    is_async: bool,
    is_generator: bool,
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

/// Public entry: given file content and file path, extract IR for each TSX React function/component.
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
        ir.metadata.parser = Some("reko-tsxExtractor".to_string());
        ir.metadata.parser_version = Some("0.1.0".to_string());
        ir.execution.is_async = r.is_async;
        ir.execution.generator = r.is_generator;
        ir.context.module = Some(module.clone());
        ir.context.class = r.class_context.clone();
        ir.source.module = module.clone();
        ir.identity.kind = "function".to_string();
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

// ---------- JSX detection (TSX-aware) ----------

fn contains_jsx(s: &str) -> bool {
    // JSX tags start with < followed by letter, '/', '>', '!' .
    // This is TSX-aware: generics like <T> appear before '(' and are excluded by caller
    // slicing body from '{' onward; still we keep heuristic robust.
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            if i + 1 < bytes.len() {
                let nxt = bytes[i + 1];
                if nxt == b'>' {
                    return true;
                }
                if nxt == b'/' {
                    if i + 2 < bytes.len() && (bytes[i + 2].is_ascii_alphabetic() || bytes[i + 2] == b'>') {
                        return true;
                    }
                } else if nxt.is_ascii_alphabetic() {
                    // opening tag <div or <MyComponent
                    let mut j = i + 1;
                    let mut in_single = false;
                    let mut in_double = false;
                    let mut escape = false;
                    while j < bytes.len() && j < i + 800 {
                        let b = bytes[j];
                        if escape {
                            escape = false;
                        } else if b == b'\\' && (in_single || in_double) {
                            escape = true;
                        } else if in_single {
                            if b == b'\'' {
                                in_single = false;
                            }
                        } else if in_double {
                            if b == b'"' {
                                in_double = false;
                            }
                        } else {
                            if b == b'\'' {
                                in_single = true;
                            } else if b == b'"' {
                                in_double = true;
                            } else if b == b'>' {
                                return true;
                            } else if b == b'<' {
                                break;
                            }
                        }
                        j += 1;
                    }
                }
            }
        }
        i += 1;
    }
    false
}

fn body_contains_jsx(source_text: &str) -> bool {
    // For TSX, distinguish generics <T> in header vs JSX <Tag> in body.
    // Slice from first '{' or "=>" to exclude type/generic header.
    let slice = if let Some(pos) = source_text.find('{') {
        &source_text[pos..]
    } else if let Some(pos) = source_text.find("=>") {
        &source_text[pos..]
    } else {
        source_text
    };
    contains_jsx(slice)
}

fn is_jsx_tag_start(content: &str, pos: usize) -> bool {
    let bytes = content.as_bytes();
    if pos >= bytes.len() || bytes[pos] != b'<' {
        return false;
    }
    if pos + 1 >= bytes.len() {
        return false;
    }
    let nxt = bytes[pos + 1];
    if nxt == b'/' || nxt == b'>' || nxt.is_ascii_alphabetic() || nxt == b'!' {
        return true;
    }
    false
}

fn skip_jsx_tag(content: &str, start: usize) -> Option<usize> {
    let bytes = content.as_bytes();
    if start >= bytes.len() || bytes[start] != b'<' {
        return None;
    }
    let mut i = start + 1;
    let mut in_single = false;
    let mut in_double = false;
    let mut escape = false;
    while i < bytes.len() {
        let b = bytes[i];
        if escape {
            escape = false;
        } else if b == b'\\' && (in_single || in_double) {
            escape = true;
        } else if in_single {
            if b == b'\'' {
                in_single = false;
            }
        } else if in_double {
            if b == b'"' {
                in_double = false;
            }
        } else {
            if b == b'\'' {
                in_single = true;
            } else if b == b'"' {
                in_double = true;
            } else if b == b'>' {
                return Some(i);
            } else if b == b'{' {
                if let Some(end) = find_jsx_expr_end(content, i) {
                    i = end;
                }
            }
        }
        i += 1;
    }
    None
}

fn find_jsx_expr_end(content: &str, open: usize) -> Option<usize> {
    let bytes = content.as_bytes();
    if bytes[open] != b'{' {
        return None;
    }
    let mut depth = 0;
    let mut in_single = false;
    let mut in_double = false;
    let mut in_template = false;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let mut escape = false;
    for i in open..bytes.len() {
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
            if b == b'{' && !escape {
                depth += 1;
            } else if b == b'}' {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
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

// ---------- JSX-aware brace/parens matching ----------

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
    let mut i = open_pos;
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
        if b == b'<' && is_jsx_tag_start(content, i) {
            if let Some(end) = skip_jsx_tag(content, i) {
                i = end + 1;
                continue;
            }
        }
        if b == b'{' {
            depth += 1;
        } else if b == b'}' {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
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
        if b == b'<' && is_jsx_tag_start(content, i) {
            if let Some(end) = skip_jsx_tag(content, i) {
                let _ = end;
            }
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
        if b == b'<' && is_jsx_tag_start(content, i) {
            if let Some(end) = skip_jsx_tag(content, i) {
                i = end + 1;
                continue;
            }
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
        if b == b'<' && is_jsx_tag_start(content, i) {
            if let Some(end) = skip_jsx_tag(content, i) {
                i = end + 1;
                continue;
            }
        }
        if b == target {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn find_next_nonspace(content: &str, start: usize) -> Option<usize> {
    let bytes = content.as_bytes();
    let mut i = start;
    while i < bytes.len() {
        let b = bytes[i];
        if b != b' ' && b != b'\t' && b != b'\n' && b != b'\r' {
            return Some(i);
        }
        i += 1;
    }
    None
}

// ---------- class / interface detection ----------

fn find_class_open_brace(content: &str, start: usize) -> Option<usize> {
    // TSX-aware: skip { inside <...> generics like React.Component<{ children: ... }>
    let bytes = content.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    let mut in_template = false;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let mut escape = false;
    let mut angle_depth: i32 = 0;
    let mut i = start;
    while i < bytes.len() {
        let b = bytes[i];
        let next = if i + 1 < bytes.len() { Some(bytes[i+1]) } else { None };
        if in_line_comment {
            if b == b'\n' { in_line_comment = false; }
            i+=1; continue;
        }
        if in_block_comment {
            if b == b'*' && next == Some(b'/') { in_block_comment=false; i+=2; continue; }
            i+=1; continue;
        }
        if in_single {
            if escape { escape=false; } else if b==b'\\' {escape=true;} else if b==b'\'' {in_single=false;}
            i+=1; continue;
        }
        if in_double {
            if escape { escape=false; } else if b==b'\\' {escape=true;} else if b==b'"' {in_double=false;}
            i+=1; continue;
        }
        if in_template {
            if escape { escape=false; } else if b==b'\\' {escape=true;} else if b==b'`' {in_template=false;}
            i+=1; continue;
        }
        if b==b'/' && next==Some(b'/') { in_line_comment=true; i+=2; continue; }
        if b==b'/' && next==Some(b'*') { in_block_comment=true; i+=2; continue; }
        if b==b'\'' { in_single=true; i+=1; continue; }
        if b==b'"' { in_double=true; i+=1; continue; }
        if b==b'`' { in_template=true; i+=1; continue; }
        if b==b'<' && angle_depth>=0 {
            // heuristic: if '<' looks like generic start, increase depth.
            // Avoid treating '<<' shift or comparison as generic? For class header it's safe.
            // Only increase if not inside JSX tag? But class header has no JSX.
            // Increase depth for any '<' that is not part of '<<' and not inside string.
            // We'll track depth for any '<' and decrement on '>'.
            angle_depth+=1;
            i+=1; continue;
        }
        if b==b'>' && angle_depth>0 {
            angle_depth-=1;
            i+=1; continue;
        }
        if b==b'<' && is_jsx_tag_start(content, i) {
            if let Some(end)=skip_jsx_tag(content,i){ i=end+1; continue; }
        }
        if b==b'{' && angle_depth==0 {
            return Some(i);
        }
        i+=1;
    }
    None
}

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
        if let Some(pos) = line.find("class ") {
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
            if let Some(open) = find_class_open_brace(content, start_byte) {
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
            if let Some(open) = find_class_open_brace(content, start_byte) {
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
        let without_default = p.split('=').next().unwrap_or(p).trim();
        let name_and_type: (String, String) = if without_default.starts_with('{') || without_default.starts_with('[') {
            if let Some(colon) = without_default.rfind(':') {
                let before = without_default[..colon].trim().to_string();
                let typ = without_default[colon + 1..].trim().to_string();
                (before, if typ.is_empty() { "any".to_string() } else { typ })
            } else {
                (without_default.to_string(), "any".to_string())
            }
        } else if let Some(colon) = without_default.find(':') {
            let n = without_default[..colon].trim();
            let t = without_default[colon + 1..].trim();
            let n_clean = n.trim_end_matches('?').trim().to_string();
            let mut name_clean = n_clean;
            if name_clean.starts_with("...") {
                name_clean = name_clean[3..].trim().to_string();
            }
            if name_clean.contains(' ') {
                if let Some(last) = name_clean.split_whitespace().last() {
                    name_clean = last.to_string();
                }
            }
            (name_clean, if t.is_empty() { "any".to_string() } else { t.to_string() })
        } else {
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
        };
        let (name_clean, typ) = name_and_type;
        if name_clean.is_empty() {
            continue;
        }
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
    if let Some(colon_pos) = between.find(':') {
        let after = between[colon_pos + 1..].trim();
        let clean = after.trim().trim_end_matches('{').trim().trim_end_matches(';').trim();
        let clean = clean.split('{').next().unwrap_or(clean).trim();
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
    let parts = split_params_ts(s);
    let mut out = Vec::new();
    for p in parts {
        let t = p.trim();
        if t.is_empty() {
            continue;
        }
        let first = t.split_whitespace().next().unwrap_or(t).trim_matches(|c| c == ',' || c == '<' || c == '>').to_string();
        let name = first.split("extends").next().unwrap_or(&first).trim().to_string();
        if !name.is_empty() {
            out.push(name);
        }
    }
    out
}

fn extract_variable_name(before_eq: &str) -> Option<String> {
    // before_eq like "export const Card: React.FC<Props>" or "const Name"
    // Strategy: find declaration keyword const/let/var and take next identifier
    let decl_kw = ["const", "let", "var"];
    for kw in decl_kw {
        if let Some(pos) = before_eq.find(kw) {
            // ensure word boundary
            let before_ok = if pos == 0 { true } else {
                let prev = before_eq.as_bytes()[pos - 1];
                !(prev.is_ascii_alphanumeric() || prev == b'_' || prev == b'$')
            };
            let after_ok = {
                let after = pos + kw.len();
                if after >= before_eq.len() { true } else {
                    let c = before_eq.as_bytes()[after];
                    c == b' ' || c == b'\t' || c == b'\n'
                }
            };
            if !before_ok || !after_ok {
                continue;
            }
            let after = &before_eq[pos + kw.len()..];
            let trimmed = after.trim_start();
            let ident: String = trimmed.chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$').collect();
            if !ident.is_empty() && !["const","let","var","export","default","async"].contains(&ident.as_str()) {
                return Some(ident);
            }
        }
    }
    // fallback: last identifier before colon or before type
    let kw = ["const", "let", "var", "export", "default", "async"];
    let tokens: Vec<&str> = before_eq.split_whitespace().collect();
    // prefer token that is before a colon (variable name with type)
    for tok in tokens.iter() {
        if tok.contains(':') {
            let before_colon = tok.split(':').next().unwrap_or(tok).trim();
            let ident: String = before_colon.chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$').collect();
            if !ident.is_empty() && !kw.contains(&ident.as_str()) {
                return Some(ident);
            }
        }
    }
    for tok in tokens.iter().rev() {
        let before_colon = tok.split(':').next().unwrap_or(tok).trim();
        let clean = before_colon.trim_matches(|c: char| c == ';' || c == ',' || c == '=');
        // skip tokens that look like types (contain '.' or '<' or '>')
        if clean.contains('.') || clean.contains('<') || clean.contains('>') {
            continue;
        }
        let ident: String = clean
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
            .collect();
        if ident.is_empty() {
            continue;
        }
        if kw.contains(&ident.as_str()) {
            continue;
        }
        if ident.chars().next().map(|c| c.is_alphabetic() || c == '_' || c == '$').unwrap_or(false) {
            return Some(ident);
        }
    }
    None
}

// ---------- main parse ----------

fn parse_functions(content: &str) -> Result<Vec<TsxFunctionRaw>> {
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
        if is_inside_interface(start_byte_candidate, &interfaces) {
            i += 1;
            continue;
        }

        // Arrow with typed variable: const Name: React.FC<Props> = (props) => { return <div> }
        if trimmed.contains("=>") && (trimmed.contains("const ") || trimmed.contains("let ") || trimmed.contains("var ") || trimmed.contains('=')) {
            if let Some((raw, _)) = try_parse_arrow(content, &line_starts, &lines, i, &classes, &interfaces) {
                if !visited.contains(&raw.start_byte) {
                    if body_contains_jsx(&raw.source_text) {
                        visited.insert(raw.start_byte);
                        let next_i = raw.end_line;
                        result.push(raw);
                        i = next_i;
                        continue;
                    } else {
                        // not JSX: skip but advance
                        visited.insert(raw.start_byte);
                        let next_i = raw.end_line;
                        i = next_i;
                        continue;
                    }
                }
            }
        }

        // function declaration: function Greeting(props: Props): JSX.Element { return <div> }
        if trimmed.contains("function") {
            if let Some((raw, _)) = try_parse_function_decl(content, &line_starts, &lines, i, &classes, &interfaces) {
                if !visited.contains(&raw.start_byte) {
                    if body_contains_jsx(&raw.source_text) {
                        visited.insert(raw.start_byte);
                        let next_i = raw.end_line;
                        result.push(raw);
                        i = next_i;
                        continue;
                    } else {
                        // generic function without JSX should be ignored, but avoid infinite loop
                        let next_i = raw.end_line;
                        // mark visited to not re-parse
                        visited.insert(raw.start_byte);
                        i = next_i;
                        continue;
                    }
                }
            }
        }

        // class method
        let enclosing = find_enclosing_class(start_byte_candidate, &classes);
        if enclosing.is_some() {
            if let Some((raw, _)) = try_parse_class_method(content, &line_starts, &lines, i, &classes, &interfaces) {
                if !visited.contains(&raw.start_byte) {
                    if body_contains_jsx(&raw.source_text) {
                        visited.insert(raw.start_byte);
                        let next_i = raw.end_line;
                        result.push(raw);
                        i = next_i;
                        continue;
                    } else {
                        visited.insert(raw.start_byte);
                        let next_i = raw.end_line;
                        i = next_i;
                        continue;
                    }
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
) -> Option<(TsxFunctionRaw, usize)> {
    let line = lines[i];
    let func_pos = line.find("function")?;
    if let Some(eq) = line.find('=') {
        if eq < func_pos {
            let before = &line[..eq];
            if before.contains("const") || before.contains("let") || before.contains("var") {
                return None;
            }
        }
    }
    let before_func = &line[..func_pos];
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
    let mut type_params = Vec::new();
    let name_byte_in_line = line.find(&name).unwrap_or(func_pos);
    let name_end_byte = line_starts[i] + name_byte_in_line + name.len();
    let mut cursor = name_end_byte;
    while cursor < content.len() && (content.as_bytes()[cursor] == b' ' || content.as_bytes()[cursor] == b'\t') {
        cursor += 1;
    }
    // TSX: generic <T> vs JSX <Tag> - here '<' immediately after function name is generic
    if cursor < content.len() && content.as_bytes()[cursor] == b'<' {
        if let Some(close_angle) = find_matching_angle(content, cursor) {
            // Ensure this is generic (followed by '(' after optional whitespace)
            // To distinguish from JSX, check that after '>' the next non-space is '('
            let after_angle = close_angle + 1;
            let mut probe = after_angle;
            while probe < content.len() && (content.as_bytes()[probe] == b' ' || content.as_bytes()[probe] == b'\t' || content.as_bytes()[probe] == b'\n' || content.as_bytes()[probe] == b'\r') {
                probe += 1;
            }
            if probe < content.len() && content.as_bytes()[probe] == b'(' {
                let inner = content[cursor + 1..close_angle].to_string();
                type_params = parse_generics(&inner);
                cursor = close_angle + 1;
            } else {
                // Check if it's still generic context: could be like <T extends string> followed by '(' anyway.
                // If not followed by '(', it might be JSX - but function name followed by JSX is impossible; so treat as generic anyway if before '('
                // We will search for '(' ahead and see if generic encloses correctly
                if let Some(open_paren_probe) = find_next_char_aware(content, cursor, b'(') {
                    if open_paren_probe > cursor && open_paren_probe < close_angle + 50 {
                        // likely generic with complex constraint, still parse
                        let inner = content[cursor + 1..close_angle].to_string();
                        type_params = parse_generics(&inner);
                        cursor = close_angle + 1;
                    }
                }
            }
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
    let open_brace = find_next_open_brace(content, close_paren + 1)?;
    let between = content[close_paren + 1..open_brace].to_string();
    if between.contains(';') {
        return None;
    }
    let close_brace = find_matching_brace(content, open_brace)?;
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

    let raw = TsxFunctionRaw {
        name,
        return_type,
        visibility,
        modifiers,
        parameters: params,
        type_params,
        is_async,
        is_generator,
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
) -> Option<(TsxFunctionRaw, usize)> {
    let line = lines[i];
    let eq_pos = line.find('=')?;
    let arrow_rel = line.find("=>")?;
    if eq_pos > arrow_rel {
        return None;
    }
    let before_eq = &line[..eq_pos];
    let name_ident = extract_variable_name(before_eq)?;
    let kw = ["const", "let", "var", "export", "default", "async"];
    if kw.contains(&name_ident.as_str()) {
        return None;
    }

    let before_arrow = &line[..arrow_rel];
    let is_async = before_arrow.contains("async");
    let is_export = line.contains("export");

    let eq_byte = line_starts[i] + eq_pos;
    let arrow_byte = line_starts[i] + arrow_rel;
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
                    // generic before '(' : <T>
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
                    if close + 1 < arrow_byte {
                        let between = content[close + 1..arrow_byte].to_string();
                        if let Some(colon) = between.find(':') {
                            let after = between[colon + 1..].trim();
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
        // single param without parens
        let between = content[eq_byte + 1..arrow_byte].trim().to_string();
        let between_no_async = between.trim().trim_start_matches("async").trim().to_string();
        let ptrim = between_no_async.trim();
        // handle generic single param like <T>(x: T) already handled above; this branch for `x =>`
        let param_clean = ptrim.split(':').next().unwrap_or(ptrim).trim().trim_end_matches('?').to_string();
        let name_clean = param_clean.trim_start_matches("...").trim().to_string();
        if !name_clean.is_empty() && !name_clean.contains(' ') && !name_clean.contains(',') {
            let typ = if let Some(colon) = ptrim.find(':') {
                ptrim[colon + 1..].trim().to_string()
            } else {
                "any".to_string()
            };
            let typ = if typ.is_empty() { "any".to_string() } else { typ };
            // strip generic prefix if any: e.g., "<T> x" ?
            let mut n = name_clean.clone();
            if n.contains('<') {
                // shouldn't happen for single param, skip
                n = n.split('<').next().unwrap_or(&n).trim().to_string();
            }
            if !n.is_empty() {
                params.push(Parameter { name: n, typ });
            }
        } else if !ptrim.is_empty() {
            params = parse_params_ts(&ptrim);
        }
    }

    // After =>, handle body variants (JSX-aware)
    let arrow_end = arrow_byte + 2;
    let next_non = find_next_nonspace(content, arrow_end)?;
    let bytes = content.as_bytes();
    let start_byte = line_starts[i] + first_non_space_col(line);
    let start_line = i + 1;
    let start_col = first_non_space_col(line) + 1;
    let class_ctx = find_enclosing_class(start_byte, classes);

    let mut modifiers = Vec::new();
    if is_async {
        modifiers.push("async".to_string());
    }
    if is_export {
        modifiers.push("export".to_string());
    }

    // case block body { ... }
    if bytes[next_non] == b'{' {
        let open_brace = next_non;
        let close_brace = find_matching_brace(content, open_brace)?;
        let (end_line, end_col) = byte_to_line_col(content, line_starts, close_brace);
        let source_text = content[start_byte..=close_brace].to_string();
        let raw = TsxFunctionRaw {
            name: name_ident,
            return_type,
            visibility: "public".to_string(),
            modifiers,
            parameters: params,
            type_params,
            is_async,
            is_generator: false,
            class_context: class_ctx,
            start_line,
            start_col,
            start_byte,
            end_line,
            end_col,
            end_byte: close_brace,
            source_text,
        };
        return Some((raw, end_line - 1));
    }
    // case paren body ( <div> ... )
    if bytes[next_non] == b'(' {
        let open_paren = next_non;
        if let Some(close_paren) = find_matching_paren(content, open_paren) {
            let (end_line, end_col) = byte_to_line_col(content, line_starts, close_paren);
            let mut end_byte = close_paren;
            if end_byte + 1 < bytes.len() && bytes[end_byte + 1] == b';' {
                end_byte += 1;
                let (el, ec) = byte_to_line_col(content, line_starts, end_byte);
                let source_text = content[start_byte..=end_byte].to_string();
                let raw = TsxFunctionRaw {
                    name: name_ident,
                    return_type,
                    visibility: "public".to_string(),
                    modifiers,
                    parameters: params,
                    type_params,
                    is_async,
                    is_generator: false,
                    class_context: class_ctx,
                    start_line,
                    start_col,
                    start_byte,
                    end_line: el,
                    end_col: ec,
                    end_byte,
                    source_text,
                };
                return Some((raw, el - 1));
            }
            let source_text = content[start_byte..=close_paren].to_string();
            let raw = TsxFunctionRaw {
                name: name_ident,
                return_type,
                visibility: "public".to_string(),
                modifiers,
                parameters: params,
                type_params,
                is_async,
                is_generator: false,
                class_context: class_ctx,
                start_line,
                start_col,
                start_byte,
                end_line,
                end_col,
                end_byte: close_paren,
                source_text,
            };
            return Some((raw, end_line - 1));
        }
    }
    // case direct JSX without braces/parens e.g., () => <div>...</div>
    if bytes[next_non] == b'<' {
        let mut scan = next_non;
        let mut in_single = false;
        let mut in_double = false;
        let mut in_template = false;
        let mut in_line_comment = false;
        let mut in_block_comment = false;
        let mut escape = false;
        let mut jsx_depth: i32 = 0;
        let mut seen_open = false;
        let mut end = next_non;
        while scan < bytes.len() {
            let b = bytes[scan];
            let nxt = if scan + 1 < bytes.len() { Some(bytes[scan + 1]) } else { None };
            if in_line_comment {
                if b == b'\n' {
                    in_line_comment = false;
                    if jsx_depth == 0 && seen_open {
                        break;
                    }
                }
                scan += 1;
                continue;
            }
            if in_block_comment {
                if b == b'*' && nxt == Some(b'/') {
                    in_block_comment = false;
                    scan += 2;
                    continue;
                }
                scan += 1;
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
                scan += 1;
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
                scan += 1;
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
                scan += 1;
                continue;
            }
            if b == b'/' && nxt == Some(b'/') {
                in_line_comment = true;
                scan += 2;
                continue;
            }
            if b == b'/' && nxt == Some(b'*') {
                in_block_comment = true;
                scan += 2;
                continue;
            }
            if b == b'\'' {
                in_single = true;
                scan += 1;
                continue;
            }
            if b == b'"' {
                in_double = true;
                scan += 1;
                continue;
            }
            if b == b'`' {
                in_template = true;
                scan += 1;
                continue;
            }
            if b == b'{' {
                if let Some(e) = find_jsx_expr_end(content, scan) {
                    scan = e + 1;
                    continue;
                }
            }
            if b == b'<' {
                if is_jsx_tag_start(content, scan) {
                    let is_closing = scan + 1 < bytes.len() && bytes[scan + 1] == b'/';
                    if let Some(tag_end) = skip_jsx_tag(content, scan) {
                        let tag_str = &content[scan..=tag_end];
                        let is_self_close = tag_str.trim_end().ends_with("/>");
                        if is_self_close {
                            if jsx_depth == 0 {
                                seen_open = true;
                            }
                            if jsx_depth == 0 {
                                scan = tag_end + 1;
                                end = scan - 1;
                                if scan < bytes.len() && bytes[scan] == b';' {
                                    end = scan;
                                    scan += 1;
                                }
                                break;
                            }
                        } else if is_closing {
                            jsx_depth -= 1;
                            if jsx_depth < 0 {
                                jsx_depth = 0;
                            }
                            if jsx_depth == 0 && seen_open {
                                scan = tag_end + 1;
                                end = scan - 1;
                                if scan < bytes.len() && bytes[scan] == b';' {
                                    end = scan;
                                    scan += 1;
                                }
                                break;
                            }
                        } else {
                            if jsx_depth == 0 {
                                seen_open = true;
                            }
                            jsx_depth += 1;
                        }
                        scan = tag_end + 1;
                        continue;
                    }
                }
            }
            if b == b';' && jsx_depth == 0 && seen_open {
                end = scan;
                break;
            }
            if b == b'\n' && jsx_depth == 0 && seen_open {
                let remaining = &content[scan + 1..];
                if let Some(nl) = remaining.lines().next() {
                    let t = nl.trim();
                    if t.starts_with("const ")
                        || t.starts_with("let ")
                        || t.starts_with("var ")
                        || t.starts_with("function ")
                        || t.starts_with("class ")
                        || t.starts_with("export ")
                        || t.starts_with("import ")
                        || t.starts_with("}")
                    {
                        end = scan - 1;
                        break;
                    }
                }
            }
            scan += 1;
            end = scan;
            if scan > next_non + 800 && jsx_depth == 0 && seen_open {
                break;
            }
            if scan > next_non + 4000 {
                break;
            }
        }
        if end <= next_non {
            end = next_non;
        }
        while end > next_non && (bytes[end] == b'\n' || bytes[end] == b'\r') {
            end -= 1;
        }
        let actual_end = end;
        let (end_line, end_col) = byte_to_line_col(content, line_starts, actual_end);
        let source_text = content[start_byte..=actual_end].to_string();
        let raw = TsxFunctionRaw {
            name: name_ident,
            return_type,
            visibility: "public".to_string(),
            modifiers,
            parameters: params,
            type_params,
            is_async,
            is_generator: false,
            class_context: class_ctx,
            start_line,
            start_col,
            start_byte,
            end_line,
            end_col,
            end_byte: actual_end,
            source_text,
        };
        return Some((raw, end_line - 1));
    }
    // fallback semicolon / line
    let semi = find_next_char_aware(content, arrow_byte, b';');
    let end_byte = if let Some(s) = semi {
        s
    } else {
        let line_end = line_starts[i] + lines[i].len();
        if line_end > 0 { line_end - 1 } else { arrow_byte }
    };
    let (end_line, end_col) = byte_to_line_col(content, line_starts, end_byte);
    let source_text = content[start_byte..=end_byte].to_string();
    let raw = TsxFunctionRaw {
        name: name_ident,
        return_type,
        visibility: "public".to_string(),
        modifiers,
        parameters: params,
        type_params,
        is_async,
        is_generator: false,
        class_context: class_ctx,
        start_line,
        start_col,
        start_byte,
        end_line,
        end_col,
        end_byte,
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
) -> Option<(TsxFunctionRaw, usize)> {
    let line = lines[i];
    let trimmed = line.trim();
    if trimmed.contains("function") || trimmed.contains("=>") || trimmed.contains("class ") || trimmed.contains("interface ") {
        return None;
    }
    if !trimmed.contains('(') || !trimmed.contains(')') {
        return None;
    }
    if trimmed.contains('=') {
        return None;
    }
    if trimmed.starts_with("@") {
        return None;
    }
    let mut rest = trimmed;
    let mut is_static = false;
    let mut is_async = false;
    let mut is_abstract = false;
    let mut is_override = false;
    let mut visibility = "public".to_string();
    let mut is_getter = false;
    let mut is_setter = false;

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
    let name_idx = line.find(&name)?;
    let name_byte = line_starts[i] + name_idx;
    if is_inside_interface(name_byte, interfaces) {
        return None;
    }
    let mut type_params = Vec::new();
    let mut cursor = name_byte + name.len();
    while cursor < content.len() && (content.as_bytes()[cursor] == b' ' || content.as_bytes()[cursor] == b'\t') {
        cursor += 1;
    }
    if cursor < content.len() && content.as_bytes()[cursor] == b'<' {
        if let Some(close_angle) = find_matching_angle(content, cursor) {
            // Only treat as generic if before '('
            let probe = close_angle + 1;
            let mut p = probe;
            while p < content.len() && (content.as_bytes()[p] == b' ' || content.as_bytes()[p] == b'\t') {
                p += 1;
            }
            if p < content.len() && content.as_bytes()[p] == b'(' {
                let inner = content[cursor + 1..close_angle].to_string();
                type_params = parse_generics(&inner);
                cursor = close_angle + 1;
            }
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
    let open_brace = find_next_open_brace(content, close_paren + 1)?;
    let between = content[close_paren + 1..open_brace].to_string();
    if between.contains(';') {
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

    let raw = TsxFunctionRaw {
        name,
        return_type,
        visibility,
        modifiers,
        parameters: params,
        type_params,
        is_async,
        is_generator: false,
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

    const TYPED_FUNCTION_COMPONENT: &str = r#"type Props = { name: string };
function Greeting(props: Props): JSX.Element {
    return <div className="greeting">Hello {props.name}</div>;
}
export function GenericCard<T>(props: T): JSX.Element {
    return <div>{JSON.stringify(props)}</div>;
}
"#;

    const TYPED_ARROW_COMPONENT: &str = r#"type Props = { title: string; value: number };
const Card: React.FC<Props> = (props: Props): JSX.Element => {
    return <div attr={props.value}><span>{props.title}</span></div>;
};
const Inline = (): JSX.Element => (<div>inline</div>);
const Simple = (props: Props) => <div>{props.title}</div>;
"#;

    const CLASS_COMPONENT: &str = r#"import React from 'react';
class MyComponent extends React.Component<{ children: React.ReactNode }> {
    render(): JSX.Element {
        return <div>{this.props.children}</div>;
    }
    customMethod(a: string, b: number): JSX.Element {
        return <span>{a + b}</span>;
    }
    helper(): number {
        return 42;
    }
}
"#;

    #[test]
    fn extracts_typed_function_component() {
        let fns = extract(TYPED_FUNCTION_COMPONENT, Path::new("comp.tsx")).unwrap();
        // Greeting and GenericCard => 2
        assert_eq!(fns.len(), 2);
        let greet = fns.iter().find(|f| f.identity.name == "Greeting").unwrap();
        assert_eq!(greet.identity.qualified_name, "comp::Greeting");
        assert_eq!(greet.identity.language, "typescript");
        assert_eq!(greet.metadata.parser.as_deref(), Some("reko-tsxExtractor"));
        assert!(greet.source.hash.starts_with("sha256:"));
        assert!(greet.source.source_text.contains("<div"));
        assert!(greet.source.source_text.contains("{props.name}"));
        assert_eq!(greet.signature.parameters.len(), 1);
        assert_eq!(greet.signature.parameters[0].name, "props");
        assert_eq!(greet.signature.parameters[0].typ, "Props");
        assert_eq!(greet.signature.return_type.as_deref(), Some("JSX.Element"));
        assert_eq!(greet.signature.type_parameters.len(), 0);

        let generic = fns.iter().find(|f| f.identity.name == "GenericCard").unwrap();
        assert_eq!(generic.identity.qualified_name, "comp::GenericCard");
        assert!(generic.source.source_text.contains("<div>"));
        assert_eq!(generic.signature.type_parameters, vec!["T"]);
        assert!(generic.declaration.modifiers.contains(&"export".to_string()));
        // ensure generic <T> not confused with JSX <div>
        assert_eq!(generic.signature.parameters.len(), 1);
        assert_eq!(generic.signature.parameters[0].typ, "T");
    }

    #[test]
    fn extracts_typed_arrow_component() {
        let fns = extract(TYPED_ARROW_COMPONENT, Path::new("arrow.tsx")).unwrap();
        // Card, Inline, Simple => 3
        assert_eq!(fns.len(), 3);
        let card = fns.iter().find(|f| f.identity.name == "Card").unwrap();
        assert_eq!(card.identity.qualified_name, "arrow::Card");
        assert_eq!(card.identity.language, "typescript");
        assert_eq!(card.metadata.parser.as_deref(), Some("reko-tsxExtractor"));
        assert_eq!(card.signature.parameters.len(), 1);
        assert_eq!(card.signature.parameters[0].name, "props");
        assert_eq!(card.signature.parameters[0].typ, "Props");
        assert_eq!(card.signature.return_type.as_deref(), Some("JSX.Element"));
        assert!(card.source.source_text.contains("<div attr={props.value}>"));
        assert!(card.source.source_text.contains("<span>"));
        assert!(card.source.hash.starts_with("sha256:"));
        assert!(body_contains_jsx(&card.source.source_text));

        let inline = fns.iter().find(|f| f.identity.name == "Inline").unwrap();
        assert!(inline.source.source_text.contains("<div>inline</div>"));
        assert_eq!(inline.signature.return_type.as_deref(), Some("JSX.Element"));

        let simple = fns.iter().find(|f| f.identity.name == "Simple").unwrap();
        assert!(simple.source.source_text.contains("<div>{props.title}</div>"));
        assert_eq!(simple.signature.parameters[0].typ, "Props");
    }

    #[test]
    fn extracts_class_component() {
        let fns = extract(CLASS_COMPONENT, Path::new("my.tsx")).unwrap();
        // only render and customMethod have JSX, helper should be filtered out
        assert_eq!(fns.len(), 2);
        let render = fns.iter().find(|f| f.identity.name == "render").unwrap();
        assert_eq!(render.identity.qualified_name, "my::MyComponent::render");
        assert_eq!(render.context.class.as_deref(), Some("MyComponent"));
        assert!(render.source.source_text.contains("<div>{this.props.children}</div>"));
        assert_eq!(render.signature.return_type.as_deref(), Some("JSX.Element"));

        let custom = fns.iter().find(|f| f.identity.name == "customMethod").unwrap();
        assert_eq!(custom.identity.qualified_name, "my::MyComponent::customMethod");
        assert_eq!(custom.signature.parameters.len(), 2);
        assert!(custom.source.source_text.contains("<span>{a + b}</span>"));
        assert!(fns.iter().find(|f| f.identity.name == "helper").is_none());
        assert_eq!(custom.identity.language, "typescript");
        assert_eq!(custom.metadata.parser.as_deref(), Some("reko-tsxExtractor"));
    }

    #[test]
    fn generic_vs_jsx_distinction() {
        // Generic function without JSX should NOT be extracted, even though it has <T>
        let content = r#"function Identity<T>(x: T): T {
    return x;
}
function WithJsx<T>(props: T): JSX.Element {
    return <div>{JSON.stringify(props)}</div>;
}
"#;
        let fns = extract(content, Path::new("gen.tsx")).unwrap();
        assert_eq!(fns.len(), 1);
        assert_eq!(fns[0].identity.name, "WithJsx");
        assert_eq!(fns[0].signature.type_parameters, vec!["T"]);
        // Identity without JSX should be ignored
        assert!(fns.iter().find(|f| f.identity.name == "Identity").is_none());
    }

    #[test]
    fn hash_and_location_and_json() {
        let content = r#"function Foo(props: Props): JSX.Element { return <div>hi</div>; }
"#;
        let fns = extract(content, Path::new("a.tsx")).unwrap();
        assert_eq!(fns.len(), 1);
        let foo = &fns[0];
        assert!(foo.source.hash.starts_with("sha256:"));
        assert_eq!(foo.source.location.start.byte, 0);
        assert_eq!(foo.source.location.start.line, 1);
        assert!(foo.source.location.end.byte > foo.source.location.start.byte);
        assert_eq!(foo.identity.language, "typescript");
        assert_eq!(foo.metadata.parser.as_deref(), Some("reko-tsxExtractor"));
        let json = extract_to_json(content, Path::new("a.tsx")).unwrap();
        assert!(json.contains("Foo"));
        assert!(json.contains("typescript"));
        assert!(json.contains("reko-tsxExtractor"));
    }

    #[test]
    fn jsx_aware_brace_matching_with_types() {
        let content = r#"function WithExpr(props: Props): JSX.Element {
    // comment with <Tag> and }
    let tmpl = `template { not }`;
    let s: string = "} not a brace {";
    /* block } <div> */
    return <div attr={props.val} data-x="{">content {props.children}</div>;
}
"#;
        let fns = extract(content, Path::new("expr.tsx")).unwrap();
        assert_eq!(fns.len(), 1);
        assert_eq!(fns[0].identity.name, "WithExpr");
        assert!(fns[0].source.source_text.contains(r#"attr={props.val}"#));
        assert!(fns[0].source.location.start.line == 1);
        assert!(fns[0].source.location.end.line >= 6);
    }
}
