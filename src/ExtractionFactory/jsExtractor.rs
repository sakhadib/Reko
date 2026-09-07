use crate::ir::{IrFunction, Parameter, Position};
use anyhow::Result;
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Debug, Clone)]
struct JsFunctionRaw {
    name: String,
    visibility: String,
    modifiers: Vec<String>,
    parameters: Vec<Parameter>,
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

/// Public entry: given file content and file path, extract IR for each JS function.
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
            None,
            r.parameters.clone(),
            vec![],
            None,
            r.class_context.clone(),
        );
        // patch language/patch parser (new_minimal hardcodes java)
        ir.identity.language = "javascript".to_string();
        ir.metadata.parser = Some("reko-jsExtractor".to_string());
        ir.metadata.parser_version = Some("0.1.0".to_string());
        ir.execution.is_async = r.is_async;
        ir.execution.generator = r.is_generator;
        ir.context.module = Some(module.clone());
        ir.context.class = r.class_context.clone();
        ir.source.module = module.clone();
        // ensure kind remains function
        ir.identity.kind = "function".to_string();

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

fn byte_to_line_col(content: &str, line_starts: &[usize], byte: usize) -> (usize, usize) {
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

// ---------- brace/parens matching with string/comment awareness ----------

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
                // skip '/' next iteration? we handle by continuing; next char '/' will be processed but in_block_comment false then?
                // Need to skip next char
                // To avoid double counting, we will manually handle skip via flag: we are still at '*', next is '/', we set flag and continue; the '/' will be consumed on next loop as normal but we have already exited? Actually we need to advance i by 1 extra. Simplify: just continue and next iteration will see '/' but not special.
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

        // not in any
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
        if b == b'}' {
            // encountered closing before opening, skip
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
        let next_next = if i + 2 < bytes.len() {
            Some(bytes[i + 2])
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
            // ensure not part of >= or ==>
            // In JS, => is distinct. Check that previous char not = or > already
            return Some(i);
        }
        let _ = next_next;
        i += 1;
    }
    None
}

// ---------- class detection ----------

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
        // look for "class " substring
        if let Some(pos) = line.find("class ") {
            // ensure word boundary before
            // Check that before pos is not alphanumeric
            // Extract name after "class "
            let after_start = pos + 6; // len "class "
            let after = &line[after_start..];
            let after_trim = after.trim_start();
            let name: String = after_trim
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
                .collect();
            if name.is_empty() {
                continue;
            }
            // avoid matching `className` inside variable
            // Ensure "class" is separate word: preceding char should be whitespace or start or ';' etc.
            if pos > 0 {
                let prev = line.as_bytes()[pos - 1];
                if (prev as char).is_alphanumeric() || prev == b'_' || prev == b'$' {
                    continue;
                }
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
    // sort by start
    res.sort_by_key(|c| c.start);
    res
}

fn find_enclosing_class(byte: usize, classes: &[ClassRange]) -> Option<String> {
    let mut best: Option<&ClassRange> = None;
    for c in classes {
        if byte > c.start && byte < c.end {
            // pick innermost (smallest range containing byte)
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

// ---------- params ----------

fn parse_params_js(s: &str) -> Vec<Parameter> {
    if s.trim().is_empty() {
        return vec![];
    }
    let parts = split_params_js(s);
    let mut out = Vec::new();
    for part in parts {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        // remove default value part after '='
        let without_default = p.split('=').next().unwrap_or(p).trim();
        // strip rest/spread for TypeScript colon etc.
        // Handle destructuring: if starts with { or [, keep whole token without default as name
        let name_raw = if without_default.starts_with('{') || without_default.starts_with('[') {
            without_default.to_string()
        } else {
            // split by ':' for TypeScript type annotation (e.g., a: number)
            let before_colon = without_default.split(':').next().unwrap_or(without_default).trim();
            before_colon.to_string()
        };
        let mut name_clean = name_raw.trim().to_string();
        // strip rest operator
        if name_clean.starts_with("...") {
            name_clean = name_clean[3..].trim().to_string();
        }
        // strip leading *? not needed
        if name_clean.is_empty() {
            continue;
        }
        // handle comma leftover? already split
        // skip if contains spaces (should take first token? but destructuring contains spaces)
        // For simple case, if contains space before, take first? but better keep whole destructuring as name? For JS, param like "{a, b}" whole is name.
        // If name_clean contains spaces and not destructuring, take first word? e.g., "a /* comment */" not needed.
        if !name_clean.starts_with('{')
            && !name_clean.starts_with('[')
            && name_clean.contains(' ')
        {
            // take first token
            if let Some(first) = name_clean.split_whitespace().next() {
                name_clean = first.to_string();
            }
        }
        if name_clean.is_empty() || name_clean == "..." {
            continue;
        }
        out.push(Parameter {
            name: name_clean,
            typ: "Any".to_string(),
        });
    }
    out
}

fn split_params_js(s: &str) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut depth_paren: i32 = 0;
    let mut depth_bracket: i32 = 0;
    let mut depth_brace: i32 = 0;
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
        if ch == '\\' {
            // keep escape handling inside strings only? but fine
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
            ',' => {
                if depth_paren == 0 && depth_bracket == 0 && depth_brace == 0 {
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

// ---------- main parse ----------

fn parse_functions(content: &str) -> Result<Vec<JsFunctionRaw>> {
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
    let mut result = Vec::new();
    let mut i = 0usize;
    // To avoid duplicate detection of same function detected via overlapping patterns, track visited start bytes
    let mut visited_starts: std::collections::HashSet<usize> = std::collections::HashSet::new();

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
        let enclosing = find_enclosing_class(start_byte_candidate, &classes);

        // Try const function expression:  const|let|var  name = [async] function
        if try_is_const_function_expr(trimmed) {
            if let Some((raw, end_line_idx)) =
                try_parse_const_function_expr(content, &line_starts, &lines, i, &classes)
            {
                if !visited_starts.contains(&raw.start_byte) {
                    visited_starts.insert(raw.start_byte);
                    let next_i = raw.end_line; // 1-indexed
                    result.push(raw);
                    i = next_i;
                    continue;
                } else {
                    // already visited, skip
                    i = end_line_idx + 1;
                    continue;
                }
            }
        }

        // Try arrow function:  const|let|var name = [async] (params) => {
        if trimmed.contains("=>") && (trimmed.contains("const ") || trimmed.contains("let ") || trimmed.contains("var ") || trimmed.contains('=')) {
            if let Some((raw, _end_idx)) =
                try_parse_arrow(content, &line_starts, &lines, i, &classes)
            {
                if !visited_starts.contains(&raw.start_byte) {
                    visited_starts.insert(raw.start_byte);
                    let next_i = raw.end_line;
                    result.push(raw);
                    i = next_i;
                    continue;
                }
            }
        }

        // Try traditional function declaration (including export/async/generator)
        if trimmed.contains("function") {
            if let Some((raw, _)) =
                try_parse_function_decl(content, &line_starts, &lines, i, &classes)
            {
                if !visited_starts.contains(&raw.start_byte) {
                    visited_starts.insert(raw.start_byte);
                    let next_i = raw.end_line;
                    result.push(raw);
                    i = next_i;
                    continue;
                }
            }
        }

        // Try class method (only if inside class)
        if enclosing.is_some() {
            if let Some((raw, _)) =
                try_parse_class_method(content, &line_starts, &lines, i, &classes)
            {
                if !visited_starts.contains(&raw.start_byte) {
                    visited_starts.insert(raw.start_byte);
                    let next_i = raw.end_line;
                    result.push(raw);
                    i = next_i;
                    continue;
                }
            }
        }

        i += 1;
    }

    // Sort by start byte to maintain order
    result.sort_by_key(|r| r.start_byte);
    Ok(result)
}

fn try_is_const_function_expr(trimmed: &str) -> bool {
    // pattern: contains "=" and "function"
    if let Some(eq) = trimmed.find('=') {
        if let Some(func) = trimmed.find("function") {
            return eq < func;
        }
    }
    false
}

fn try_parse_const_function_expr(
    content: &str,
    line_starts: &[usize],
    lines: &[&str],
    i: usize,
    classes: &[ClassRange],
) -> Option<(JsFunctionRaw, usize)> {
    let line = lines[i];
    let trimmed = line.trim();
    // find '=' and 'function'
    let eq_pos = line.find('=')?;
    let func_pos = line.find("function")?;
    if eq_pos > func_pos {
        return None;
    }
    // extract name before '='
    let before_eq = &line[..eq_pos];
    // take last word before eq (skip const/let/var/export)
    let name = before_eq
        .split_whitespace()
        .last()?
        .trim_matches(|c: char| c == ';' || c == ',' )
        .to_string();
    let name_clean = name
        .trim()
        .trim_end_matches(';')
        .to_string();
    // need to filter name if it's const/let/var/export etc.
    let invalid = ["const", "let", "var", "export", "default", "="];
    if invalid.contains(&name_clean.as_str()) || name_clean.is_empty() {
        return None;
    }
    // ensure name is valid identifier
    if !name_clean.chars().next().map(|c| c.is_alphabetic() || c == '_' || c == '$').unwrap_or(false) {
        return None;
    }
    // Clean name: take identifier chars
    let name_ident: String = name_clean
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
        .collect();
    if name_ident.is_empty() {
        return None;
    }

    // modifiers
    let mut is_async = line[..func_pos].contains("async");
    let after_func = &line[func_pos + 8..];
    let after_trim = after_func.trim_start();
    let is_generator = after_trim.starts_with('*');

    // find '(' after function
    let func_byte_start = line_starts[i] + func_pos;
    let open_paren = find_next_char_aware(content, func_byte_start, b'(')?;
    let close_paren = find_matching_paren(content, open_paren)?;
    let params_str = if close_paren > open_paren + 1 {
        content[open_paren + 1..close_paren].to_string()
    } else {
        String::new()
    };
    let params = parse_params_js(&params_str);
    let open_brace = find_next_open_brace(content, close_paren + 1)?;
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
    if is_generator {
        modifiers.push("generator".to_string());
    }
    if line.contains("export") {
        modifiers.push("export".to_string());
    }
    if find_is_static(line) {
        modifiers.push("static".to_string());
    }

    let raw = JsFunctionRaw {
        name: name_ident,
        visibility: "public".to_string(),
        modifiers,
        parameters: params,
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
    let end_idx = end_line - 1;
    Some((raw, end_idx))
}

fn try_parse_arrow(
    content: &str,
    line_starts: &[usize],
    lines: &[&str],
    i: usize,
    classes: &[ClassRange],
) -> Option<(JsFunctionRaw, usize)> {
    let line = lines[i];
    // need '=' and '=>'
    let eq_pos = line.find('=')?;
    let arrow_rel = line.find("=>")?;
    if eq_pos > arrow_rel {
        return None;
    }
    // extract name before '='
    let before_eq = &line[..eq_pos];
    let name_candidate = before_eq.split_whitespace().last()?.to_string();
    let name_ident: String = name_candidate
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
        .collect();
    if name_ident.is_empty() {
        return None;
    }
    // invalid name filter
    if ["const", "let", "var", "export", "default"].contains(&name_ident.as_str()) {
        // In case line is "export const foo = ...", before_eq last word is foo, not const, so okay
        // But if our split got const, it means missing name; try to extract more accurately
        // Try to find name after const/let/var
        // Fallback: search for const/let/var keyword and take next word
        return None;
    }

    // Determine async
    let before_arrow = &line[..arrow_rel];
    let is_async = before_arrow.contains("async");

    // Extract params substring between '=' and '=>'
    let between = &line[eq_pos + 1..arrow_rel];
    let between_trim = between.trim().trim_start_matches("async").trim();

    let params_str: String;
    if between_trim.starts_with('(') {
        // find '(' and matching ')'
        // Use content-level aware scanning for better accuracy
        let search_start = line_starts[i] + eq_pos;
        if let Some(open) = find_next_char_aware(content, search_start, b'(') {
            if open < line_starts[i] + arrow_rel {
                if let Some(close) = find_matching_paren(content, open) {
                    // ensure close < arrow byte
                    let arrow_byte = line_starts[i] + arrow_rel;
                    if close < arrow_byte {
                        params_str = content[open + 1..close].to_string();
                    } else {
                        params_str = String::new();
                    }
                } else {
                    params_str = String::new();
                }
            } else {
                params_str = String::new();
            }
        } else {
            params_str = String::new();
        }
    } else {
        // single param without parens: e.g., x => {
        let single = between_trim.trim();
        // Remove async etc.
        let clean = single.trim().to_string();
        if clean.is_empty() || clean.contains(' ') || clean.contains(',') {
            params_str = String::new();
        } else {
            params_str = clean;
        }
    }

    let params = if params_str.trim().is_empty() {
        Vec::new()
    } else if !between_trim.trim().starts_with('(') && !params_str.contains(',') {
        // single param case
        parse_params_js(&params_str)
    } else {
        parse_params_js(&params_str)
    };

    // find open brace after =>
    let arrow_byte = line_starts[i] + arrow_rel;
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
    if line.contains("export") {
        modifiers.push("export".to_string());
    }

    let raw = JsFunctionRaw {
        name: name_ident,
        visibility: "public".to_string(),
        modifiers,
        parameters: params,
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
    let end_idx = end_line - 1;
    Some((raw, end_idx))
}

fn try_parse_function_decl(
    content: &str,
    line_starts: &[usize],
    lines: &[&str],
    i: usize,
    classes: &[ClassRange],
) -> Option<(JsFunctionRaw, usize)> {
    let line = lines[i];
    let trimmed = line.trim();
    // must contain "function"
    let func_pos = line.find("function")?;
    // Ensure '=' not before function with valid name? If eq before, it's const expr, already handled; but decl could still have export default etc., so we allow.
    // However if line is "const foo = function" we would have already returned via const expr; for decl we skip if eq before func and name before eq is identifier (to avoid double)
    if let Some(eq) = line.find('=') {
        if eq < func_pos {
            // Check if before eq has identifier that looks like const assignment; then this is not decl, it's anonymous function expr
            // But decl with `export default function` has no '=', so keep.
            // For safety, if eq < func and trimmed contains "function" after "=", skip decl
            // We already handled const expr; for anonymous function assigned, decl would have no name -> will fail name extraction anyway.
            // To avoid misclassifying, if eq < func and name after function is empty (anonymous), we should not treat as decl.
            // Let it fall through but name extraction will fail.
        }
    }

    let before_func = &line[..func_pos];
    // Check that before_func does not contain '(' that would indicate something else? Not needed.

    let after = &line[func_pos + 8..];
    let after_trim = after.trim_start();
    let is_generator = after_trim.starts_with('*');
    let after_name_start = if is_generator {
        after_trim[1..].trim_start()
    } else {
        after_trim
    };
    let name: String = after_name_start
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
        .collect();
    if name.is_empty() {
        // Could be anonymous export default function() { -> generate name "default" or skip
        // For spec, export default function may be anonymous; we will treat as "default" if contains default
        if before_func.contains("default") || trimmed.contains("export default") {
            // Use "default" as name? Or skip anonymous
            // Spec says handle export default function, etc. Might be named like `export default function foo()`
            // Anonymous case: we can use "default"
            // Check if we should synthesize name
            // Only if param parens exist and brace exists, we can return with name "default"
            // Let's do that.
            // Verify parens exist
            let func_byte_start = line_starts[i] + func_pos;
            let open_paren = find_next_char_aware(content, func_byte_start, b'(')?;
            let close_paren = find_matching_paren(content, open_paren)?;
            let params_str = if close_paren > open_paren + 1 {
                content[open_paren + 1..close_paren].to_string()
            } else {
                String::new()
            };
            let params = parse_params_js(&params_str);
            let open_brace = find_next_open_brace(content, close_paren + 1)?;
            let close_brace = find_matching_brace(content, open_brace)?;
            let is_async = before_func.contains("async");
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
            if is_generator {
                modifiers.push("generator".to_string());
            }
            if before_func.contains("export") || trimmed.contains("export") {
                modifiers.push("export".to_string());
            }
            if before_func.contains("default") || trimmed.contains("default") {
                modifiers.push("default".to_string());
            }
            let raw = JsFunctionRaw {
                name: "default".to_string(),
                visibility: "public".to_string(),
                modifiers,
                parameters: params,
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
            return Some((raw, end_line - 1));
        }
        return None;
    }

    let is_async = before_func.contains("async");
    // Check export/default in before_func or trimmed prefix
    let func_byte_start = line_starts[i] + func_pos;
    let open_paren = find_next_char_aware(content, func_byte_start, b'(')?;
    let close_paren = find_matching_paren(content, open_paren)?;
    let params_str = if close_paren > open_paren + 1 {
        content[open_paren + 1..close_paren].to_string()
    } else {
        String::new()
    };
    let params = parse_params_js(&params_str);
    let open_brace = find_next_open_brace(content, close_paren + 1)?;
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
    if is_generator {
        modifiers.push("generator".to_string());
    }
    if before_func.contains("export") || trimmed.contains("export") {
        modifiers.push("export".to_string());
    }
    if before_func.contains("default") || trimmed.contains("default") {
        modifiers.push("default".to_string());
    }

    let raw = JsFunctionRaw {
        name,
        visibility: "public".to_string(),
        modifiers,
        parameters: params,
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

fn find_is_static(line: &str) -> bool {
    line.contains("static ")
}

fn try_parse_class_method(
    content: &str,
    line_starts: &[usize],
    lines: &[&str],
    i: usize,
    classes: &[ClassRange],
) -> Option<(JsFunctionRaw, usize)> {
    let line = lines[i];
    let trimmed = line.trim();
    // Quick rejects
    if trimmed.contains("function") || trimmed.contains("=>") || trimmed.contains("class ") {
        return None;
    }
    if !trimmed.contains('(') || !trimmed.contains(')') {
        return None;
    }
    // reject if line contains '=' (assignment) – method shouldn't have '='
    if trimmed.contains('=') {
        // Could be `foo = () => {}` inside class fields, but we handle via arrow; skip method for '='
        return None;
    }
    // Must end with '{' or have '{' after ')', but we search for brace anyway
    // Parse prefix tokens
    let mut rest = trimmed;
    let mut is_static = false;
    let mut is_async = false;
    let mut is_generator = false;

    // handle static
    if rest.starts_with("static ") {
        is_static = true;
        rest = rest[7..].trim_start();
    }
    if rest.starts_with("async ") {
        is_async = true;
        rest = rest[6..].trim_start();
    }
    // Could be "static async " order swapped? check again
    if !is_static && rest.starts_with("static ") {
        is_static = true;
        rest = rest[7..].trim_start();
    }
    if !is_async && rest.starts_with("async ") {
        is_async = true;
        rest = rest[6..].trim_start();
    }
    if rest.starts_with('*') {
        is_generator = true;
        rest = rest[1..].trim_start();
    }
    // Handle get/set? e.g., "get foo() {" -> rest starts with "get "
    // For simplicity, if starts with get/set and next token before '(' is identifier, treat as method with name after get/set
    // Detect get/set prefix
    let mut is_getter = false;
    let mut is_setter = false;
    if rest.starts_with("get ") {
        is_getter = true;
        rest = rest[4..].trim_start();
    } else if rest.starts_with("set ") {
        is_setter = true;
        rest = rest[4..].trim_start();
    }

    // Now name
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
        .collect();
    if name.is_empty() {
        return None;
    }
    // Filter keywords that shouldn't be method names (except constructor)
    let keywords = [
        "if", "for", "while", "switch", "catch", "else", "return", "const", "let", "var",
        "import", "export", "default", "function", "class",
    ];
    if keywords.contains(&name.as_str()) {
        return None;
    }
    // After name should be '(' (maybe with spaces)
    let after_name = &rest[name.len()..];
    if !after_name.trim_start().starts_with('(') {
        return None;
    }

    // Find open paren byte
    // Need to locate name position in line to get accurate byte
    // Find name index in line
    let name_idx = line.find(&name)?;
    let name_byte = line_starts[i] + name_idx;
    let open_paren = find_next_char_aware(content, name_byte, b'(')?;
    let close_paren = find_matching_paren(content, open_paren)?;
    let params_str = if close_paren > open_paren + 1 {
        content[open_paren + 1..close_paren].to_string()
    } else {
        String::new()
    };
    let params = parse_params_js(&params_str);
    let open_brace = find_next_open_brace(content, close_paren + 1)?;
    let close_brace = find_matching_brace(content, open_brace)?;

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
    if is_generator {
        modifiers.push("generator".to_string());
    }
    if is_getter {
        modifiers.push("get".to_string());
    }
    if is_setter {
        modifiers.push("set".to_string());
    }

    let raw = JsFunctionRaw {
        name,
        visibility: "public".to_string(),
        modifiers,
        parameters: params,
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

// ---------- tests ----------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const SAMPLE_FUNCS: &str = r#"function add(a, b) {
    return a + b;
}
export async function fetchData(url) {
    const res = await fetch(url);
    return res;
}
const multiply = function(x, y) {
    return x * y;
}
"#;

    const SAMPLE_ARROW: &str = r#"const sum = (a, b) => {
    return a + b;
}
const inc = x => {
    return x + 1;
}
export const asyncArrow = async (a, b) => {
    return a + b;
}
"#;

    const SAMPLE_CLASS: &str = r#"class Calculator {
    constructor(a) {
        this.a = a;
    }
    add(b) {
        return this.a + b;
    }
    static create(a) {
        return new Calculator(a);
    }
    async fetchValue() {
        return this.a;
    }
}
"#;

    #[test]
    fn extracts_traditional_and_const_function() {
        let fns = extract(SAMPLE_FUNCS, Path::new("math.js")).unwrap();
        assert_eq!(fns.len(), 3);
        let add = fns.iter().find(|f| f.identity.name == "add").unwrap();
        assert_eq!(add.identity.qualified_name, "math::add");
        assert_eq!(add.signature.parameters.len(), 2);
        assert_eq!(add.identity.language, "javascript");
        assert_eq!(add.metadata.parser.as_deref(), Some("reko-jsExtractor"));
        assert!(add.source.hash.starts_with("sha256:"));
        assert_eq!(add.source.location.start.line, 1);

        let fetch = fns.iter().find(|f| f.identity.name == "fetchData").unwrap();
        assert!(fetch.execution.is_async);
        assert!(fetch.declaration.modifiers.contains(&"async".to_string()));
        assert!(fetch.declaration.modifiers.contains(&"export".to_string()));

        let mul = fns.iter().find(|f| f.identity.name == "multiply").unwrap();
        assert_eq!(mul.signature.parameters.len(), 2);
        assert_eq!(mul.source.location.start.line, 8);
    }

    #[test]
    fn extracts_arrow_functions() {
        let fns = extract(SAMPLE_ARROW, Path::new("arrow.js")).unwrap();
        assert_eq!(fns.len(), 3);
        let sum = fns.iter().find(|f| f.identity.name == "sum").unwrap();
        assert_eq!(sum.identity.qualified_name, "arrow::sum");
        assert_eq!(sum.signature.parameters.len(), 2);
        assert_eq!(sum.signature.parameters[0].name, "a");

        let inc = fns.iter().find(|f| f.identity.name == "inc").unwrap();
        assert_eq!(inc.signature.parameters.len(), 1);
        assert_eq!(inc.signature.parameters[0].name, "x");

        let async_arr = fns.iter().find(|f| f.identity.name == "asyncArrow").unwrap();
        assert!(async_arr.execution.is_async);
        assert!(async_arr.declaration.modifiers.contains(&"async".to_string()));
    }

    #[test]
    fn extracts_class_methods() {
        let fns = extract(SAMPLE_CLASS, Path::new("calc.js")).unwrap();
        // constructor, add, create, fetchValue => 4
        assert_eq!(fns.len(), 4);
        let cons = fns.iter().find(|f| f.identity.name == "constructor").unwrap();
        assert_eq!(cons.identity.qualified_name, "calc::Calculator::constructor");
        assert_eq!(cons.context.class.as_deref(), Some("Calculator"));

        let add = fns.iter().find(|f| f.identity.name == "add").unwrap();
        assert_eq!(add.identity.qualified_name, "calc::Calculator::add");

        let create = fns.iter().find(|f| f.identity.name == "create").unwrap();
        assert!(create.declaration.modifiers.contains(&"static".to_string()));

        let fetch = fns.iter().find(|f| f.identity.name == "fetchValue").unwrap();
        assert!(fetch.execution.is_async);
        assert!(fetch.declaration.modifiers.contains(&"async".to_string()));
    }

    #[test]
    fn brace_matching_with_strings() {
        let content = r#"function tricky(a) {
    let s = "} not a brace {";
    let t = '} still {';
    // comment with }
    /* block } */
    return a;
}
"#;
        let fns = extract(content, Path::new("tricky.js")).unwrap();
        assert_eq!(fns.len(), 1);
        assert_eq!(fns[0].identity.name, "tricky");
        assert!(fns[0].source.source_text.contains("return a;"));
        // Ensure end byte correctly encloses whole function
        assert!(fns[0].source.location.end.line >= 6);
    }

    #[test]
    fn location_and_hash() {
        let content = "function foo() {\n    return 1;\n}\n";
        let fns = extract(content, Path::new("a.js")).unwrap();
        assert_eq!(fns.len(), 1);
        assert!(fns[0].source.location.start.byte < fns[0].source.location.end.byte);
        assert!(fns[0].source.hash.starts_with("sha256:"));
        assert_eq!(fns[0].source.location.start.line, 1);
    }

    #[test]
    fn export_default_function() {
        let content = r#"export default function greet(name) {
    return "hi " + name;
}
"#;
        let fns = extract(content, Path::new("greet.js")).unwrap();
        assert_eq!(fns.len(), 1);
        assert_eq!(fns[0].identity.name, "greet");
        assert!(fns[0].declaration.modifiers.contains(&"export".to_string()));
        assert!(fns[0].declaration.modifiers.contains(&"default".to_string()));
    }
}
