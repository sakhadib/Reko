use crate::ir::{IrFunction, Parameter, Position};
use anyhow::Result;
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Debug, Clone)]
struct PhpFunctionRaw {
    name: String,
    visibility: String,
    modifiers: Vec<String>,
    parameters: Vec<Parameter>,
    return_type: Option<String>,
    start_line: usize,
    start_col: usize,
    start_byte: usize,
    end_line: usize,
    end_col: usize,
    end_byte: usize,
    source_text: String,
    namespace: Option<String>,
    class: Option<String>,
}

/// Public entry: given file content and file path, extract IR for each PHP function/method.
pub fn extract(content: &str, file_path: &Path) -> Result<Vec<IrFunction>> {
    let module = file_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    let file_str = file_path.display().to_string();

    let raws = parse_php_functions(content)?;

    let mut out = Vec::new();
    for r in raws {
        let hash = hash_source(&r.source_text);
        // qualified_name like "Namespace\\Class::func" or "Namespace::Class::func" or "Class::func" or "Namespace\\func" or "func"
        let qualified = match (&r.namespace, &r.class) {
            (Some(ns), Some(cls)) => format!("{}\\{}::{}", ns, cls, r.name),
            (Some(ns), None) => format!("{}\\{}", ns, r.name),
            (None, Some(cls)) => format!("{}::{}", cls, r.name),
            (None, None) => r.name.clone(),
        };
        let id = qualified.clone();

        // new_minimal hardcodes language="java", we patch after
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
            r.namespace.clone(),
            r.class.clone(),
        );
        ir.identity.language = "php".to_string();
        ir.metadata.parser = Some("reko-phpExtractor".to_string());
        ir.metadata.parser_version = Some("0.1.0".to_string());
        // ensure context.module reflects file module if needed, but keep namespace as package/namespace
        // new_minimal sets context.module = package; for php keep package as namespace, but also ensure source.module is file module
        ir.source.module = module.clone();
        // keep context.namespace as namespace
        ir.context.namespace = r.namespace.clone();
        ir.context.module = r.namespace.clone().or(Some(module.clone()));
        // language_specific remains empty

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

fn is_identifier_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

fn is_boundary_before(content: &str, pos: usize) -> bool {
    if pos == 0 {
        return true;
    }
    let prev = content.as_bytes()[pos - 1];
    !is_identifier_char(prev) && prev != b'\\'
}

fn is_boundary_after(content: &str, pos: usize) -> bool {
    if pos >= content.len() {
        return true;
    }
    let b = content.as_bytes()[pos];
    // after keyword should be whitespace or not ident char
    !(is_identifier_char(b))
}

fn find_matching_brace(content: &str, open_pos: usize) -> Option<usize> {
    let bytes = content.as_bytes();
    let mut depth = 0i32;
    let mut in_line_comment_slash = false;
    let mut in_line_comment_hash = false;
    let mut in_block_comment = false;
    let mut in_single = false;
    let mut in_double = false;
    let mut escape = false;

    for i in open_pos..bytes.len() {
        let b = bytes[i];
        let next = if i + 1 < bytes.len() {
            Some(bytes[i + 1])
        } else {
            None
        };

        if in_line_comment_slash || in_line_comment_hash {
            if b == b'\n' {
                in_line_comment_slash = false;
                in_line_comment_hash = false;
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

        // not in any
        if b == b'/' && next == Some(b'/') {
            in_line_comment_slash = true;
            continue;
        }
        if b == b'#' {
            // # starts comment if not in string; in PHP # is line comment
            in_line_comment_hash = true;
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
    let mut depth = 0i32;
    let mut in_line_comment_slash = false;
    let mut in_line_comment_hash = false;
    let mut in_block_comment = false;
    let mut in_single = false;
    let mut in_double = false;
    let mut escape = false;

    for i in open_pos..bytes.len() {
        let b = bytes[i];
        let next = if i + 1 < bytes.len() {
            Some(bytes[i + 1])
        } else {
            None
        };

        if in_line_comment_slash || in_line_comment_hash {
            if b == b'\n' {
                in_line_comment_slash = false;
                in_line_comment_hash = false;
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

        if b == b'/' && next == Some(b'/') {
            in_line_comment_slash = true;
            continue;
        }
        if b == b'#' {
            in_line_comment_hash = true;
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

fn find_open_brace_aware(content: &str, start: usize) -> Option<usize> {
    let bytes = content.as_bytes();
    let mut in_line_comment_slash = false;
    let mut in_line_comment_hash = false;
    let mut in_block_comment = false;
    let mut in_single = false;
    let mut in_double = false;
    let mut escape = false;

    let mut i = start;
    while i < bytes.len() {
        let b = bytes[i];
        let next = if i + 1 < bytes.len() {
            Some(bytes[i + 1])
        } else {
            None
        };

        if in_line_comment_slash || in_line_comment_hash {
            if b == b'\n' {
                in_line_comment_slash = false;
                in_line_comment_hash = false;
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

        if b == b'/' && next == Some(b'/') {
            in_line_comment_slash = true;
            i += 2;
            continue;
        }
        if b == b'#' {
            in_line_comment_hash = true;
            i += 1;
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
        if b == b'{' {
            return Some(i);
        }
        if b == b';' {
            // semicolon before brace -> abstract/interface method without body
            return None;
        }
        i += 1;
        // limit search window to avoid scanning whole file for far braces? but php functions brace is near
        if i > start + 5000 {
            // still continue but we want to find brace relatively soon; if not found within 5000 bytes, probably no brace
            // break to avoid long scan for abstract methods that have ; soon but we already returned None on ;
            // For normal functions, brace is close.
        }
    }
    None
}

fn parse_php_params(s: &str) -> Vec<Parameter> {
    let t = s.trim();
    if t.is_empty() {
        return vec![];
    }
    let parts = split_params_php(t);
    let mut out = Vec::new();
    for part in parts {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        // cut default value
        let left = p.split('=').next().unwrap_or(p).trim();
        // left may contain type and $var and ... 
        // find $
        if let Some(dollar) = left.find('$') {
            let name_part = &left[dollar..];
            // name is $ + identifier
            let mut name = String::new();
            for ch in name_part.chars() {
                if ch == '$' || ch.is_alphanumeric() || ch == '_' || ch as u8 >= 0x80 {
                    name.push(ch);
                } else {
                    break;
                }
            }
            if name.is_empty() || name == "$" {
                continue;
            }
            let before = left[..dollar].trim();
            // remove trailing ... and spaces
            let mut typ = before.replace("...", "").trim().to_string();
            // also remove leading & if passed by reference like "&$var"
            typ = typ.trim_start_matches('&').trim().to_string();
            if typ.is_empty() {
                typ = "mixed".to_string();
            }
            // handle variadic without type but with ... before $
            // already handled. If original part contains "...$" then typ may be empty, keep mixed
            // Check if part contains "..." before $ => variadic, but type already captured
            out.push(Parameter { name, typ });
        } else {
            // no $, could be variadic without $?? unlikely, skip
            // also could be `self`? but php params always have $
            continue;
        }
    }
    out
}

fn split_params_php(s: &str) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut depth_paren: i32 = 0;
    let mut depth_bracket: i32 = 0;
    let mut depth_brace: i32 = 0;
    let mut in_single = false;
    let mut in_double = false;
    let mut escape = false;
    for ch in s.chars() {
        if escape {
            cur.push(ch);
            escape = false;
            continue;
        }
        if ch == '\\' {
            if in_single || in_double {
                escape = true;
            }
            cur.push(ch);
            continue;
        }
        if in_single {
            if ch == '\'' {
                in_single = false;
            }
            cur.push(ch);
            continue;
        }
        if in_double {
            if ch == '"' {
                in_double = false;
            }
            cur.push(ch);
            continue;
        }
        match ch {
            '\'' => {
                in_single = true;
                cur.push(ch);
            }
            '"' => {
                in_double = true;
                cur.push(ch);
            }
            '(' => {
                depth_paren += 1;
                cur.push(ch);
            }
            ')' => {
                depth_paren -= 1;
                cur.push(ch);
            }
            '[' => {
                depth_bracket += 1;
                cur.push(ch);
            }
            ']' => {
                depth_bracket -= 1;
                cur.push(ch);
            }
            '{' => {
                depth_brace += 1;
                cur.push(ch);
            }
            '}' => {
                depth_brace -= 1;
                cur.push(ch);
            }
            ',' => {
                if depth_paren == 0 && depth_bracket == 0 && depth_brace == 0 {
                    parts.push(cur.trim().to_string());
                    cur.clear();
                } else {
                    cur.push(ch);
                }
            }
            _ => cur.push(ch),
        }
    }
    if !cur.trim().is_empty() {
        parts.push(cur.trim().to_string());
    }
    parts
}

fn parse_php_functions(content: &str) -> Result<Vec<PhpFunctionRaw>> {
    // Build line_starts for byte -> line/col
    let mut line_starts: Vec<usize> = vec![0];
    for (i, b) in content.bytes().enumerate() {
        if b == b'\n' {
            line_starts.push(i + 1);
        }
    }
    // lines for first_non_space
    let lines: Vec<&str> = content.lines().collect();

    let bytes = content.as_bytes();
    let mut pos: usize = 0;
    let mut current_namespace: Option<String> = None;
    let mut class_stack: Vec<(String, usize)> = Vec::new(); // (name, end_byte)

    let mut result = Vec::new();

    // state for scanning with comment/string awareness
    let mut in_single = false;
    let mut in_double = false;
    let mut in_line_slash = false;
    let mut in_line_hash = false;
    let mut in_block = false;
    let mut escape = false;

    while pos < bytes.len() {
        let b = bytes[pos];
        let next = if pos + 1 < bytes.len() {
            Some(bytes[pos + 1])
        } else {
            None
        };

        // handle state transitions when inside
        if in_line_slash || in_line_hash {
            if b == b'\n' {
                in_line_slash = false;
                in_line_hash = false;
            }
            pos += 1;
            continue;
        }
        if in_block {
            if b == b'*' && next == Some(b'/') {
                in_block = false;
                pos += 2;
                continue;
            }
            pos += 1;
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
            pos += 1;
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
            pos += 1;
            continue;
        }

        // not in any -> check for comment/string start
        if b == b'/' && next == Some(b'/') {
            in_line_slash = true;
            pos += 2;
            continue;
        }
        if b == b'#' {
            in_line_hash = true;
            pos += 1;
            continue;
        }
        if b == b'/' && next == Some(b'*') {
            in_block = true;
            pos += 2;
            continue;
        }
        if b == b'\'' {
            in_single = true;
            pos += 1;
            continue;
        }
        if b == b'"' {
            in_double = true;
            pos += 1;
            continue;
        }

        // pop expired classes before checking keywords
        // clean stack where pos > end
        while let Some((_, end)) = class_stack.last() {
            if pos > *end {
                class_stack.pop();
            } else {
                break;
            }
        }

        // check for namespace
        if pos + 9 <= bytes.len()
            && &content[pos..pos + 9] == "namespace"
            && is_boundary_before(content, pos)
            && is_boundary_after(content, pos + 9)
        {
            let mut p = pos + 9;
            while p < bytes.len() && bytes[p].is_ascii_whitespace() {
                p += 1;
            }
            let start = p;
            while p < bytes.len()
                && (bytes[p].is_ascii_alphanumeric() || bytes[p] == b'_' || bytes[p] == b'\\')
            {
                p += 1;
            }
            let ns = content[start..p].trim().to_string();
            if !ns.is_empty() {
                current_namespace = Some(ns);
            }
            // Move pos to p and continue; we will handle comment skipping naturally
            pos = p;
            continue;
        }

        // check for class / interface / trait
        let mut is_class_keyword = false;
        let mut keyword_len = 0usize;
        if pos + 5 <= bytes.len()
            && &content[pos..pos + 5] == "class"
            && is_boundary_before(content, pos)
            && is_boundary_after(content, pos + 5)
        {
            is_class_keyword = true;
            keyword_len = 5;
        } else if pos + 9 <= bytes.len()
            && &content[pos..pos + 9] == "interface"
            && is_boundary_before(content, pos)
            && is_boundary_after(content, pos + 9)
        {
            is_class_keyword = true;
            keyword_len = 9;
        } else if pos + 5 <= bytes.len()
            && &content[pos..pos + 5] == "trait"
            && is_boundary_before(content, pos)
            && is_boundary_after(content, pos + 5)
        {
            is_class_keyword = true;
            keyword_len = 5;
        }

        if is_class_keyword {
            let mut p = pos + keyword_len;
            while p < bytes.len() && bytes[p].is_ascii_whitespace() {
                p += 1;
            }
            let start = p;
            while p < bytes.len() && (bytes[p].is_ascii_alphanumeric() || bytes[p] == b'_' ) {
                p += 1;
            }
            let name = content[start..p].trim().to_string();
            if !name.is_empty() {
                // find brace for this class
                if let Some(brace) = find_open_brace_aware(content, p) {
                    if let Some(end) = find_matching_brace(content, brace) {
                        class_stack.push((name, end));
                    }
                }
            }
            pos = p;
            continue;
        }

        // check for function
        if pos + 8 <= bytes.len()
            && &content[pos..pos + 8] == "function"
            && is_boundary_before(content, pos)
            && is_boundary_after(content, pos + 8)
        {
            let func_kw_pos = pos;
            let mut p = pos + 8;
            while p < bytes.len() && bytes[p].is_ascii_whitespace() {
                p += 1;
            }
            // handle reference &
            if p < bytes.len() && bytes[p] == b'&' {
                p += 1;
                while p < bytes.len() && bytes[p].is_ascii_whitespace() {
                    p += 1;
                }
            }
            // check anonymous: next char '('
            if p < bytes.len() && bytes[p] == b'(' {
                pos = p + 1;
                continue;
            }
            let name_start = p;
            while p < bytes.len()
                && (bytes[p].is_ascii_alphanumeric() || bytes[p] == b'_' || bytes[p] >= 0x80)
            {
                p += 1;
            }
            let name = content[name_start..p].trim().to_string();
            if name.is_empty() {
                pos = p + 1;
                continue;
            }
            // skip whitespace
            while p < bytes.len() && bytes[p].is_ascii_whitespace() {
                p += 1;
            }
            if p >= bytes.len() || bytes[p] != b'(' {
                pos = p + 1;
                continue;
            }
            let paren_open = p;
            let paren_close = match find_matching_paren(content, paren_open) {
                Some(v) => v,
                None => {
                    pos = p + 1;
                    continue;
                }
            };
            let params_str = if paren_close > paren_open + 1 {
                content[paren_open + 1..paren_close].to_string()
            } else {
                String::new()
            };

            // determine visibility/modifiers from line containing function
            // find line index of func_kw_pos
            let line_idx = {
                // find line containing func_kw_pos
                let mut idx = 0usize;
                for (i, &start) in line_starts.iter().enumerate() {
                    if start <= func_kw_pos {
                        idx = i;
                    } else {
                        break;
                    }
                }
                idx
            };
            let line_str = if line_idx < lines.len() {
                lines[line_idx]
            } else {
                ""
            };
            // column offset of func keyword within line
            let line_start_byte = line_starts[line_idx];
            let col_offset = func_kw_pos - line_start_byte;
            let before = if col_offset <= line_str.len() {
                &line_str[..col_offset]
            } else {
                line_str
            };
            let mut visibility = "public".to_string();
            let mut modifiers: Vec<String> = Vec::new();
            for tok in before.split_whitespace() {
                match tok {
                    "public" | "private" | "protected" => visibility = tok.to_string(),
                    "static" | "final" | "abstract" => {
                        if !modifiers.contains(&tok.to_string()) {
                            modifiers.push(tok.to_string());
                        }
                    }
                    _ => {}
                }
            }
            // If modifiers contain abstract and no body expected, we still try to find brace; if none, skip
            // Find open brace aware from after paren_close
            let brace_open = match find_open_brace_aware(content, paren_close + 1) {
                Some(v) => v,
                None => {
                    // abstract method without body: skip (no source_text via brace)
                    // Could create source up to ';' but spec says brace matching, so skip
                    pos = paren_close + 1;
                    continue;
                }
            };
            // check if brace is outside class range? still include

            // return type: look between paren_close+1 and brace_open for ':'
            let mut return_type: Option<String> = None;
            let between = &content[paren_close + 1..brace_open];
            if let Some(colon_idx) = between.find(':') {
                let after_colon = &between[colon_idx + 1..];
                // type is trimmed, may include `?`, `\`, `|`, alphanumeric, etc
                // Remove any trailing `use`? not needed
                // also need to handle that after_colon may contain `//` comment? but we already excluded comments via aware? Between slice raw may contain comments but ok
                let rt = after_colon.trim().to_string();
                // rt may contain whitespace; clean
                // if rt contains '{' shouldn't, but between ends before '{' so not
                // Take first token? Actually between before brace is just type
                // there may be trailing whitespace; we need to ensure rt doesn't contain `//` etc
                // Split by whitespace and take first contiguous?
                // For type like `?string` or `\Foo\Bar` or `int|null` it's single token
                // So we can take rt split whitespace first part? But type could be `? \Foo\Bar` unlikely
                // Simplify: take first line of rt trimmed, remove any comment starter
                let first = rt
                    .split("//")
                    .next()
                    .unwrap_or(&rt)
                    .split('#')
                    .next()
                    .unwrap_or(&rt)
                    .trim()
                    .to_string();
                // also remove any trailing whitespace + possible `/*`
                let cleaned = first.split("/*").next().unwrap_or(&first).trim().to_string();
                if !cleaned.is_empty() {
                    return_type = Some(cleaned);
                }
            }

            let brace_end = match find_matching_brace(content, brace_open) {
                Some(v) => v,
                None => {
                    pos = brace_open + 1;
                    continue;
                }
            };

            // compute start byte: first non-space of line containing function
            let start_col0 = first_non_space_col(line_str);
            let start_byte = line_start_byte + start_col0;
            // ensure start_byte <= func_kw_pos
            let (start_line, start_col) = byte_to_line_col(content, &line_starts, start_byte);
            let (end_line, end_col) = byte_to_line_col(content, &line_starts, brace_end);

            // pop expired classes already done before, but also for this function, current class is top of stack if within range
            let current_class = class_stack.last().map(|(n, _)| n.clone());

            let source_text = content[start_byte..=brace_end].to_string();
            let parameters = parse_php_params(&params_str);

            result.push(PhpFunctionRaw {
                name,
                visibility,
                modifiers,
                parameters,
                return_type,
                start_line,
                start_col,
                start_byte,
                end_line,
                end_col,
                end_byte: brace_end,
                source_text,
                namespace: current_namespace.clone(),
                class: current_class,
            });

            // advance pos beyond this function to avoid re-scanning inside body
            // reset state after jumping: we need to clear string/comment state because we jumped over content that may contain strings
            // simplest: reset flags and set pos = brace_end+1
            pos = brace_end + 1;
            // reset scanning state
            in_single = false;
            in_double = false;
            in_line_slash = false;
            in_line_hash = false;
            in_block = false;
            escape = false;
            continue;
        }

        pos += 1;
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const SAMPLE_BASIC: &str = r#"<?php
namespace App\Controller;

class UserController {
    public function index(): string {
        return "hello";
    }

    private static function helper(int $a, string $b = "default"): void {
        // do something
        $x = "{ not a brace }";
        return;
    }
}

function globalFunc($x, $y) {
    return $x + $y;
}
"#;

    const SAMPLE_VISIBILITY: &str = r#"<?php
namespace Foo\Bar;

final class MyClass {
    protected function withReturn(int $a, ?string $b = null): ?int {
        return $a;
    }

    public static final function complex(array $arr, ...$args): array {
        return $arr;
    }

    abstract protected function abstractMethod($x);
}

function withNamespaceFunc(): void {
}
"#;

    const SAMPLE_BRACE_COMMENT: &str = r#"<?php
function braceInString() {
    $s = "} { // comment {";
    // comment with { }
    /* block } { */
    $a = '# }';
    return $s;
}
"#;

    #[test]
    fn extracts_basic_functions() {
        let fns = extract(SAMPLE_BASIC, Path::new("UserController.php")).unwrap();
        assert_eq!(fns.len(), 3, "found {:?}", fns.iter().map(|f| f.identity.name.clone()).collect::<Vec<_>>());
        let index = fns.iter().find(|f| f.identity.name == "index").unwrap();
        assert_eq!(index.identity.qualified_name, "App\\Controller\\UserController::index");
        assert_eq!(index.declaration.visibility, "public");
        assert_eq!(index.signature.return_type.as_deref(), Some("string"));
        assert_eq!(index.context.namespace.as_deref(), Some("App\\Controller"));
        assert_eq!(index.context.class.as_deref(), Some("UserController"));
        assert_eq!(index.identity.language, "php");
        assert!(index.source.hash.starts_with("sha256:"));
        assert!(index.source.source_text.contains("function index"));

        let helper = fns.iter().find(|f| f.identity.name == "helper").unwrap();
        assert_eq!(helper.declaration.visibility, "private");
        assert!(helper.declaration.modifiers.contains(&"static".to_string()));
        assert_eq!(helper.signature.parameters.len(), 2);
        assert_eq!(helper.signature.parameters[0].name, "$a");
        assert_eq!(helper.signature.parameters[0].typ, "int");
        assert_eq!(helper.signature.parameters[1].typ, "string");
        assert_eq!(helper.signature.return_type.as_deref(), Some("void"));
        assert_eq!(helper.identity.qualified_name, "App\\Controller\\UserController::helper");

        let global = fns.iter().find(|f| f.identity.name == "globalFunc").unwrap();
        assert_eq!(global.identity.qualified_name, "App\\Controller\\globalFunc");
        assert_eq!(global.declaration.visibility, "public");
        assert_eq!(global.context.class, None);
        // location
        assert!(global.source.location.start.byte < global.source.location.end.byte);
        assert!(global.source.location.start.line < global.source.location.end.line);
    }

    #[test]
    fn visibility_modifiers_and_namespace() {
        let fns = extract(SAMPLE_VISIBILITY, Path::new("MyClass.php")).unwrap();
        // should skip abstractMethod (no body) => 3 functions (withReturn, complex, withNamespaceFunc)
        assert_eq!(fns.len(), 3, "found {:?}", fns.iter().map(|f| f.identity.name.clone()).collect::<Vec<_>>());
        let wr = fns.iter().find(|f| f.identity.name == "withReturn").unwrap();
        assert_eq!(wr.declaration.visibility, "protected");
        assert_eq!(wr.signature.return_type.as_deref(), Some("?int"));
        assert_eq!(wr.signature.parameters.len(), 2);
        assert_eq!(wr.signature.parameters[0].typ, "int");
        assert_eq!(wr.signature.parameters[1].typ, "?string");
        assert_eq!(wr.identity.qualified_name, "Foo\\Bar\\MyClass::withReturn");

        let complex = fns.iter().find(|f| f.identity.name == "complex").unwrap();
        assert!(complex.declaration.modifiers.contains(&"static".to_string()));
        assert!(complex.declaration.modifiers.contains(&"final".to_string()));
        assert_eq!(complex.signature.parameters.len(), 2);
        assert_eq!(complex.signature.parameters[0].typ, "array");
        // ...$args -> name $args, type mixed? we map to mixed
        assert_eq!(complex.signature.parameters[1].name, "$args");
        assert_eq!(complex.identity.qualified_name, "Foo\\Bar\\MyClass::complex");

        let nsf = fns.iter().find(|f| f.identity.name == "withNamespaceFunc").unwrap();
        assert_eq!(nsf.identity.qualified_name, "Foo\\Bar\\withNamespaceFunc");
        assert!(nsf.source.hash.starts_with("sha256:"));
    }

    #[test]
    fn brace_matching_with_strings_and_comments() {
        let fns = extract(SAMPLE_BRACE_COMMENT, Path::new("test.php")).unwrap();
        assert_eq!(fns.len(), 1);
        let f = &fns[0];
        assert_eq!(f.identity.name, "braceInString");
        // ensure source_text includes whole function and not truncated at string brace
        assert!(f.source.source_text.contains("\"} { // comment {\""));
        assert!(f.source.source_text.ends_with('}'));
        assert!(f.source.location.start.byte < f.source.location.end.byte);
        // hash
        assert!(f.source.hash.starts_with("sha256:"));
    }

    #[test]
    fn extract_to_json_works() {
        let json = extract_to_json(SAMPLE_BASIC, Path::new("a.php")).unwrap();
        assert!(json.contains("UserController"));
        assert!(json.contains("globalFunc"));
        // parse back
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(v.is_array());
        assert_eq!(v.as_array().unwrap().len(), 3);
    }

    #[test]
    fn skips_anonymous_closure() {
        let content = r#"<?php
function named() {
    $fn = function ($x) { return $x; };
    $arrow = fn($y) => $y + 1;
}
"#;
        let fns = extract(content, Path::new("c.php")).unwrap();
        assert_eq!(fns.len(), 1);
        assert_eq!(fns[0].identity.name, "named");
        // ensure anonymous not extracted
        assert!(!fns.iter().any(|f| f.identity.name.is_empty()));
    }
}
