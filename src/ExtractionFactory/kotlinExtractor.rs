use crate::ir::{IrFunction, Parameter, Position};
use anyhow::Result;
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Debug, Clone)]
struct KotlinFunctionRaw {
    name: String,
    return_type: Option<String>,
    visibility: String,
    modifiers: Vec<String>,
    attributes: Vec<String>,
    parameters: Vec<Parameter>,
    type_params: Vec<String>,
    is_suspend: bool,
    class_context: Option<String>,
    type_path: Option<String>,
    type_kind: Option<String>,
    start_line: usize,
    start_col: usize,
    start_byte: usize,
    end_line: usize,
    end_col: usize,
    end_byte: usize,
    source_text: String,
}

#[derive(Debug, Clone)]
struct TypeInfo {
    kind: String,
    name: String,
    open: usize,
    close: usize,
}

pub fn extract(content: &str, file_path: &Path) -> Result<Vec<IrFunction>> {
    let package = parse_package(content);
    let module = file_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    let file_str = file_path.display().to_string();

    let raws = parse_kotlin_functions(content)?;

    let mut out = Vec::new();
    for r in raws {
        let hash = hash_source(&r.source_text);
        let qualified = match (&package, &r.type_path) {
            (Some(pkg), Some(tp)) => format!("{pkg}::{tp}::{}", r.name),
            (Some(pkg), None) => {
                if let Some(cls) = &r.class_context {
                    format!("{pkg}::{cls}::{}", r.name)
                } else {
                    format!("{pkg}::{}", r.name)
                }
            }
            (None, Some(tp)) => format!("{tp}::{}", r.name),
            (None, None) => {
                if let Some(cls) = &r.class_context {
                    format!("{cls}::{}", r.name)
                } else {
                    r.name.clone()
                }
            }
        };
        let id = qualified.clone();

        let mut ir = IrFunction::new_minimal(
            id,
            r.name.clone(),
            qualified.clone(),
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
            package.clone(),
            r.class_context.clone(),
        );
        ir.identity.language = "kotlin".to_string();
        ir.metadata.parser = Some("reko-kotlinExtractor".to_string());
        ir.metadata.parser_version = Some("0.1.0".to_string());
        ir.metadata.confidence = 1.0;
        // context adjustments
        ir.context.package = package.clone();
        ir.context.module = package.clone().or(Some(module.clone()));
        ir.source.module = package.clone().unwrap_or(module.clone());
        if let Some(tp) = &r.type_path {
            // if type_path contains ::, use innermost for class/interface
            // keep class_context already set; also handle struct/interface distinction
            if r.type_kind.as_deref() == Some("interface") {
                ir.context.interface = r.class_context.clone();
                ir.context.class = None;
            } else if r.type_kind.as_deref() == Some("object") {
                ir.context.class = r.class_context.clone();
            } else {
                // class / enum etc keep as class for compatibility
                // also keep struct_ mirroring if needed
            }
            let _ = tp;
        } else if r.type_kind.as_deref() == Some("interface") {
            ir.context.interface = r.class_context.clone();
            ir.context.class = None;
        }
        ir.declaration.attributes = r.attributes.clone();
        ir.declaration.annotations = r.attributes.clone();
        ir.signature.type_parameters = r.type_params.clone();
        ir.execution.is_async = r.is_suspend;
        if r.is_suspend {
            ir.execution.coroutine = true;
        }
        out.push(ir);
    }
    Ok(out)
}

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

fn parse_package(content: &str) -> Option<String> {
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with("package ") {
            let inner = t.trim_start_matches("package ").trim();
            // remove trailing ; if present (kotlin usually no ;)
            let inner = inner.trim_end_matches(';').trim();
            // package may have trailing comment? strip
            let inner = inner.split_whitespace().next().unwrap_or(inner);
            if !inner.is_empty() {
                return Some(inner.to_string());
            }
        }
    }
    None
}

// ---------- brace / string aware helpers -----------

fn find_open_brace_kotlin(content: &str, start_byte: usize) -> Option<usize> {
    let bytes = content.as_bytes();
    let mut i = start_byte;
    let mut in_line_comment = false;
    let mut block_depth: i32 = 0;
    let mut in_string = false;
    let mut in_triple = false;
    let mut in_char = false;
    let mut escape = false;

    while i < bytes.len() {
        let b = bytes[i];
        let next = if i + 1 < bytes.len() { Some(bytes[i + 1]) } else { None };
        let next2 = if i + 2 < bytes.len() { Some(bytes[i + 2]) } else { None };

        if in_triple {
            if b == b'"' && next == Some(b'"') && next2 == Some(b'"') {
                in_triple = false;
                i += 3;
                continue;
            }
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else {
                escape = false;
            }
            i += 1;
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
            i += 1;
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
            i += 1;
            continue;
        }
        if in_line_comment {
            if b == b'\n' {
                in_line_comment = false;
            }
            i += 1;
            continue;
        }
        if block_depth > 0 {
            if b == b'/' && next == Some(b'*') {
                block_depth += 1;
                i += 2;
                continue;
            }
            if b == b'*' && next == Some(b'/') {
                block_depth -= 1;
                i += 2;
                continue;
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
            block_depth = 1;
            i += 2;
            continue;
        }
        if b == b'"' && next == Some(b'"') && next2 == Some(b'"') {
            in_triple = true;
            i += 3;
            continue;
        }
        if b == b'"' {
            in_string = true;
            escape = false;
            i += 1;
            continue;
        }
        if b == b'\'' {
            in_char = true;
            escape = false;
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

fn find_matching_brace_kotlin(content: &str, open_pos: usize) -> Option<usize> {
    let bytes = content.as_bytes();
    let mut depth: i32 = 0;
    let mut i = open_pos;
    let mut in_line_comment = false;
    let mut block_depth: i32 = 0;
    let mut in_string = false;
    let mut in_triple = false;
    let mut in_char = false;
    let mut escape = false;

    while i < bytes.len() {
        let b = bytes[i];
        let next = if i + 1 < bytes.len() { Some(bytes[i + 1]) } else { None };
        let next2 = if i + 2 < bytes.len() { Some(bytes[i + 2]) } else { None };

        if in_triple {
            if b == b'"' && next == Some(b'"') && next2 == Some(b'"') {
                in_triple = false;
                i += 3;
                continue;
            }
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else {
                escape = false;
            }
            i += 1;
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
            i += 1;
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
            i += 1;
            continue;
        }
        if in_line_comment {
            if b == b'\n' {
                in_line_comment = false;
            }
            i += 1;
            continue;
        }
        if block_depth > 0 {
            if b == b'/' && next == Some(b'*') {
                block_depth += 1;
                i += 2;
                continue;
            }
            if b == b'*' && next == Some(b'/') {
                block_depth -= 1;
                i += 2;
                continue;
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
            block_depth = 1;
            i += 2;
            continue;
        }
        if b == b'"' && next == Some(b'"') && next2 == Some(b'"') {
            in_triple = true;
            i += 3;
            continue;
        }
        if b == b'"' {
            in_string = true;
            escape = false;
            i += 1;
            continue;
        }
        if b == b'\'' {
            in_char = true;
            escape = false;
            i += 1;
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
        i += 1;
    }
    None
}

fn find_matching_paren_kotlin(content: &str, open_pos: usize) -> Option<usize> {
    let bytes = content.as_bytes();
    let mut depth: i32 = 0;
    let mut i = open_pos;
    let mut in_line_comment = false;
    let mut block_depth: i32 = 0;
    let mut in_string = false;
    let mut in_triple = false;
    let mut in_char = false;
    let mut escape = false;
    while i < bytes.len() {
        let b = bytes[i];
        let next = if i + 1 < bytes.len() { Some(bytes[i + 1]) } else { None };
        let next2 = if i + 2 < bytes.len() { Some(bytes[i + 2]) } else { None };
        if in_triple {
            if b == b'"' && next == Some(b'"') && next2 == Some(b'"') {
                in_triple = false;
                i += 3;
                continue;
            }
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else {
                escape = false;
            }
            i += 1;
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
            i += 1;
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
            i += 1;
            continue;
        }
        if in_line_comment {
            if b == b'\n' {
                in_line_comment = false;
            }
            i += 1;
            continue;
        }
        if block_depth > 0 {
            if b == b'/' && next == Some(b'*') {
                block_depth += 1;
                i += 2;
                continue;
            }
            if b == b'*' && next == Some(b'/') {
                block_depth -= 1;
                i += 2;
                continue;
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
            block_depth = 1;
            i += 2;
            continue;
        }
        if b == b'"' && next == Some(b'"') && next2 == Some(b'"') {
            in_triple = true;
            i += 3;
            continue;
        }
        if b == b'"' {
            in_string = true;
            escape = false;
            i += 1;
            continue;
        }
        if b == b'\'' {
            in_char = true;
            escape = false;
            i += 1;
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
        i += 1;
    }
    None
}

// ---------- param helpers ----------

fn split_params_respecting(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut depth_paren: i32 = 0;
    let mut depth_angle: i32 = 0;
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
        if ch == '\\' && (in_double || in_single) {
            escape = true;
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
            '<' => {
                depth_angle += 1;
                cur.push(ch);
            }
            '>' => {
                if depth_angle > 0 {
                    depth_angle -= 1;
                }
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
                if depth_paren == 0 && depth_angle == 0 && depth_bracket == 0 && depth_brace == 0 {
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

fn parse_kotlin_params(s: &str) -> Vec<Parameter> {
    let t = s.trim();
    if t.is_empty() {
        return vec![];
    }
    let parts = split_params_respecting(t);
    let mut out = Vec::new();
    for part in parts {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        // remove default value after = but respecting nested? simple split at first = at depth 0 (we already split respecting)
        let without_default = p.split('=').next().unwrap_or(p).trim();
        // without_default may contain annotations like @Inject or vararg, crossinline etc
        // find colon for name:type separation
        if let Some(colon_idx) = without_default.find(':') {
            let before = without_default[..colon_idx].trim();
            let typ = without_default[colon_idx + 1..].trim().to_string();
            // before may be "vararg name", "crossinline name", "noinline name", "private val name" etc
            // take last token as name
            let tokens: Vec<&str> = before.split_whitespace().collect();
            let mut name_raw = tokens.last().unwrap_or(&"_").to_string();
            // Kotlin may have `name` in backticks? handle
            name_raw = name_raw.trim_matches('`').to_string();
            // remove leading `var` etc? Already picking last
            let name = if name_raw.is_empty() { format!("arg{}", out.len()) } else { name_raw };
            let typ = if typ.is_empty() { "Any".to_string() } else { typ };
            out.push(Parameter { name, typ });
        } else {
            // No colon: might be like "name" without type (should not happen), treat as name with Any
            let tokens: Vec<&str> = without_default.split_whitespace().collect();
            let name = tokens.last().unwrap_or(&without_default).trim_matches('`').to_string();
            if name.is_empty() {
                continue;
            }
            out.push(Parameter { name, typ: "Any".to_string() });
        }
    }
    out
}

fn parse_kotlin_signature(sig: &str) -> (String, Vec<String>, Vec<String>, String, Vec<String>, Vec<Parameter>, Option<String>, bool) {
    // returns (visibility, modifiers, attributes, name, type_params, params, ret, is_suspend)
    let mut visibility = "public".to_string();
    let mut modifiers: Vec<String> = Vec::new();
    let mut attributes: Vec<String> = Vec::new();
    let mut is_suspend = false;

    // extract attributes starting with @
    // scan for @... respecting parens
    {
        let chars: Vec<char> = sig.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == '@' {
                let mut attr = String::new();
                attr.push('@');
                i += 1;
                let mut depth = 0;
                while i < chars.len() {
                    let c = chars[i];
                    if c == '(' {
                        depth += 1;
                        attr.push(c);
                    } else if c == ')' {
                        if depth > 0 {
                            depth -= 1;
                            attr.push(c);
                            if depth == 0 {
                                i += 1;
                                break;
                            }
                        } else {
                            break;
                        }
                    } else if c.is_whitespace() && depth == 0 {
                        break;
                    } else {
                        attr.push(c);
                    }
                    i += 1;
                }
                if !attr.is_empty() {
                    attributes.push(attr);
                }
            } else {
                i += 1;
            }
        }
    }

    let func_pos_opt = sig.find("fun ");
    let func_pos = if let Some(p) = func_pos_opt {
        p
    } else if let Some(p) = sig.find("fun\t") {
        p
    } else {
        return (visibility, modifiers, attributes, "unknown".to_string(), vec![], vec![], None, is_suspend);
    };
    let prefix = sig[..func_pos].trim().to_string();
    let prefix_low = prefix.to_lowercase();
    if prefix_low.contains("private") {
        visibility = "private".to_string();
    } else if prefix_low.contains("protected") {
        visibility = "protected".to_string();
    } else if prefix_low.contains("internal") {
        visibility = "internal".to_string();
    } else if prefix_low.contains("public") {
        visibility = "public".to_string();
    } else {
        visibility = "public".to_string();
    }
    // modifiers detection
    for kw in ["suspend", "open", "override", "inline", "operator", "infix", "tailrec", "external", "actual", "expect", "abstract", "final", "sealed", "data", "value", "annotation"] {
        if prefix.contains(kw) {
            // ensure word boundary
            let pat = format!(" {kw} ");
            if prefix == kw || prefix.contains(&pat) || prefix.starts_with(&format!("{kw} ")) || prefix.ends_with(&format!(" {kw}")) || prefix.contains(kw) {
                // also handle annotations? fine
                if kw == "suspend" {
                    is_suspend = true;
                }
                if !modifiers.contains(&kw.to_string()) {
                    modifiers.push(kw.to_string());
                }
            }
        }
    }
    // ensure suspend in modifiers if is_suspend
    if is_suspend && !modifiers.contains(&"suspend".to_string()) {
        modifiers.push("suspend".to_string());
    }
    if prefix.contains("override") && !modifiers.contains(&"override".to_string()) {
        modifiers.push("override".to_string());
    }
    if prefix.contains("open") && !modifiers.contains(&"open".to_string()) {
        modifiers.push("open".to_string());
    }

    let after_func = sig[func_pos + 3..].trim_start().to_string(); // after "fun" (3 chars) plus space trimmed
    // Handle generics after fun: <T, R>
    let mut idx = 0usize;
    let after_chars: Vec<char> = after_func.chars().collect();
    while idx < after_chars.len() && after_chars[idx].is_whitespace() {
        idx += 1;
    }
    let mut type_params: Vec<String> = Vec::new();
    if idx < after_chars.len() && after_chars[idx] == '<' {
        let mut depth = 0i32;
        let mut start = idx;
        let mut end_opt: Option<usize> = None;
        for j in idx..after_chars.len() {
            if after_chars[j] == '<' {
                depth += 1;
            } else if after_chars[j] == '>' {
                depth -= 1;
                if depth == 0 {
                    end_opt = Some(j);
                    break;
                }
            }
        }
        if let Some(end) = end_opt {
            let inner: String = after_chars[start + 1..end].iter().collect();
            if !inner.trim().is_empty() {
                type_params = split_params_respecting(&inner)
                    .into_iter()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            idx = end + 1;
            while idx < after_chars.len() && after_chars[idx].is_whitespace() {
                idx += 1;
            }
        }
    }

    let remaining: String = after_chars[idx..].iter().collect();
    // remaining starts with receiver? e.g., "String.foo()" or "List<String>.bar()" or "foo()"
    // Find '(' position to extract name part
    let paren_pos = remaining.find('(');
    let name_part = if let Some(p) = paren_pos {
        remaining[..p].trim().to_string()
    } else {
        remaining.trim().to_string()
    };
    // name_part may contain receiver: "String.foo" or "String . foo" (unlikely)
    // Split by '.' last component is function name
    let name = if name_part.contains('.') {
        let parts: Vec<&str> = name_part.split('.').collect();
        let last = parts.last().unwrap_or(&"unknown").trim();
        // last may contain generics after name like "foo<T>"? Actually generics after name also possible but we already handled before receiver; there is also `foo<T>` form where T after name before '('
        // Handle trailing generics: "foo<T>"
        let name_clean = if let Some(lt) = last.find('<') {
            last[..lt].trim().to_string()
        } else {
            last.to_string()
        };
        name_clean.trim_matches('`').to_string()
    } else {
        let n = name_part.trim();
        let name_clean = if let Some(lt) = n.find('<') {
            n[..lt].trim().to_string()
        } else {
            n.to_string()
        };
        name_clean.trim_matches('`').to_string()
    };
    let name = if name.is_empty() { "unknown".to_string() } else { name };

    // If name_part had generic after name like foo<T>, capture type_params from there if not already
    if type_params.is_empty() && name_part.contains('<') {
        if let Some(lt) = name_part.find('<') {
            if let Some(gt) = name_part.rfind('>') {
                if gt > lt {
                    let inner = name_part[lt + 1..gt].to_string();
                    type_params = split_params_respecting(&inner)
                        .into_iter()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                }
            }
        }
    }

    // Params and return type
    let params_str;
    let after_params;
    if let Some(ppos) = paren_pos {
        let slice = &remaining[ppos..];
        if let Some(end_rel) = find_matching_paren_simple(slice, 0) {
            params_str = slice[1..end_rel].to_string();
            after_params = slice[end_rel + 1..].to_string();
        } else {
            params_str = String::new();
            after_params = String::new();
        }
    } else {
        params_str = String::new();
        after_params = String::new();
    }
    let parameters = parse_kotlin_params(&params_str);
    let after_trim = after_params.trim().to_string();
    let mut return_type: Option<String> = None;
    if let Some(colon_pos) = after_trim.find(':') {
        // colon separates return type from params; but need to ensure colon is for return type not inside where?
        let after_colon = after_trim[colon_pos + 1..].trim().to_string();
        // return type ends at '{' or '=' or 'where' or end
        let mut ret_end = after_colon.len();
        for kw in [" where ", " {", " =", "\n"] {
            if let Some(pos) = after_colon.find(kw) {
                if pos < ret_end {
                    ret_end = pos;
                }
            }
        }
        // also handle trailing '{' without space?
        let mut ret_part = after_colon[..ret_end].trim().to_string();
        // remove trailing { if present
        ret_part = ret_part.trim_end_matches('{').trim().to_string();
        // handle suspend after? already
        if !ret_part.is_empty() {
            return_type = Some(ret_part);
        }
    }
    // check for suspend in after? not needed

    (visibility, modifiers, attributes, name, type_params, parameters, return_type, is_suspend)
}

fn find_matching_paren_simple(s: &str, open_idx: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (idx, ch) in s[open_idx..].char_indices() {
        if ch == '(' {
            depth += 1;
        } else if ch == ')' {
            depth -= 1;
            if depth == 0 {
                return Some(open_idx + idx);
            }
        }
    }
    None
}

fn collect_type_contexts(content: &str) -> Vec<TypeInfo> {
    let mut types = Vec::new();
    let bytes = content.as_bytes();
    let mut i = 0usize;
    let mut in_line_comment = false;
    let mut block_depth: i32 = 0;
    let mut in_string = false;
    let mut in_triple = false;
    let mut in_char = false;
    let mut escape = false;

    while i < bytes.len() {
        let b = bytes[i];
        let next = if i + 1 < bytes.len() { Some(bytes[i + 1]) } else { None };
        let next2 = if i + 2 < bytes.len() { Some(bytes[i + 2]) } else { None };

        if in_triple {
            if b == b'"' && next == Some(b'"') && next2 == Some(b'"') {
                in_triple = false;
                i += 3;
                continue;
            }
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else {
                escape = false;
            }
            i += 1;
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
            i += 1;
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
            i += 1;
            continue;
        }
        if in_line_comment {
            if b == b'\n' {
                in_line_comment = false;
            }
            i += 1;
            continue;
        }
        if block_depth > 0 {
            if b == b'/' && next == Some(b'*') {
                block_depth += 1;
                i += 2;
                continue;
            }
            if b == b'*' && next == Some(b'/') {
                block_depth -= 1;
                i += 2;
                continue;
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
            block_depth = 1;
            i += 2;
            continue;
        }
        if b == b'"' && next == Some(b'"') && next2 == Some(b'"') {
            in_triple = true;
            i += 3;
            continue;
        }
        if b == b'"' {
            in_string = true;
            escape = false;
            i += 1;
            continue;
        }
        if b == b'\'' {
            in_char = true;
            escape = false;
            i += 1;
            continue;
        }

        let prev_is_ident = if i > 0 { bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_' } else { false };
        if !prev_is_ident {
            let mut kind_opt: Option<&str> = None;
            let mut kw_len = 0usize;
            if i + 5 <= bytes.len() && &bytes[i..i + 5] == b"class" {
                let after = if i + 5 < bytes.len() { bytes[i + 5] } else { b' ' };
                if !(after.is_ascii_alphanumeric() || after == b'_') {
                    kind_opt = Some("class");
                    kw_len = 5;
                }
            } else if i + 9 <= bytes.len() && &bytes[i..i + 9] == b"interface" {
                let after = if i + 9 < bytes.len() { bytes[i + 9] } else { b' ' };
                if !(after.is_ascii_alphanumeric() || after == b'_') {
                    kind_opt = Some("interface");
                    kw_len = 9;
                }
            } else if i + 6 <= bytes.len() && &bytes[i..i + 6] == b"object" {
                let after = if i + 6 < bytes.len() { bytes[i + 6] } else { b' ' };
                if !(after.is_ascii_alphanumeric() || after == b'_') {
                    kind_opt = Some("object");
                    kw_len = 6;
                }
            } else if i + 4 <= bytes.len() && &bytes[i..i + 4] == b"enum" {
                // enum class -> treat as class enum
                let after = if i + 4 < bytes.len() { bytes[i + 4] } else { b' ' };
                if !(after.is_ascii_alphanumeric() || after == b'_') {
                    // check if followed by " class"
                    let mut j = i + 4;
                    while j < bytes.len() && bytes[j] == b' ' { j += 1; }
                    if j + 5 <= bytes.len() && &bytes[j..j + 5] == b"class" {
                        kind_opt = Some("class");
                        kw_len = j + 5 - i;
                    } else {
                        kind_opt = Some("class");
                        kw_len = 4;
                    }
                }
            }

            if let Some(kind) = kind_opt {
                let mut j = i + kw_len;
                while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t' || bytes[j] == b'\n' || bytes[j] == b'\r') {
                    j += 1;
                }
                let name_start = j;
                while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_' ) {
                    j += 1;
                }
                if j > name_start {
                    let raw_name = String::from_utf8_lossy(&bytes[name_start..j]).to_string();
                    // handle companion object case where name may be missing: already filtered j>name_start so companion without name skipped
                    if !raw_name.is_empty() {
                        if let Some(open_pos) = find_open_brace_kotlin(content, j) {
                            let between = &content[j..open_pos];
                            // ensure not abstract without brace? check that between doesn't contain ';' - but kotlin doesn't use ;
                            if !between.contains(';') {
                                if let Some(close_pos) = find_matching_brace_kotlin(content, open_pos) {
                                    types.push(TypeInfo {
                                        kind: kind.to_string(),
                                        name: raw_name,
                                        open: open_pos,
                                        close: close_pos,
                                    });
                                }
                            }
                        }
                    }
                } else {
                    // nameless companion object: skip but we could still track its braces to provide context? For simplicity skip
                    // However we still need to find its braces to know its range for nesting? Skip
                }
            }
        }

        i += 1;
    }
    types.sort_by_key(|t| t.open);
    types
}

fn parse_kotlin_functions(content: &str) -> Result<Vec<KotlinFunctionRaw>> {
    let mut line_starts: Vec<usize> = vec![0];
    for (idx, b) in content.bytes().enumerate() {
        if b == b'\n' {
            line_starts.push(idx + 1);
        }
    }
    let lines: Vec<&str> = content.lines().collect();
    let type_infos = collect_type_contexts(content);
    let bytes = content.as_bytes();
    let mut i = 0usize;
    let mut in_line_comment = false;
    let mut block_depth: i32 = 0;
    let mut in_string = false;
    let mut in_triple = false;
    let mut in_char = false;
    let mut escape = false;
    let mut result = Vec::new();

    while i < bytes.len() {
        let b = bytes[i];
        let next = if i + 1 < bytes.len() { Some(bytes[i + 1]) } else { None };
        let next2 = if i + 2 < bytes.len() { Some(bytes[i + 2]) } else { None };

        if in_triple {
            if b == b'"' && next == Some(b'"') && next2 == Some(b'"') {
                in_triple = false;
                i += 3;
                continue;
            }
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else {
                escape = false;
            }
            i += 1;
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
            i += 1;
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
            i += 1;
            continue;
        }
        if in_line_comment {
            if b == b'\n' {
                in_line_comment = false;
            }
            i += 1;
            continue;
        }
        if block_depth > 0 {
            if b == b'/' && next == Some(b'*') {
                block_depth += 1;
                i += 2;
                continue;
            }
            if b == b'*' && next == Some(b'/') {
                block_depth -= 1;
                i += 2;
                continue;
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
            block_depth = 1;
            i += 2;
            continue;
        }
        if b == b'"' && next == Some(b'"') && next2 == Some(b'"') {
            in_triple = true;
            i += 3;
            continue;
        }
        if b == b'"' {
            in_string = true;
            escape = false;
            i += 1;
            continue;
        }
        if b == b'\'' {
            in_char = true;
            escape = false;
            i += 1;
            continue;
        }

        let prev_is_ident = if i > 0 { bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_' } else { false };
        if !prev_is_ident && i + 3 <= bytes.len() && &bytes[i..i + 3] == b"fun" {
            let after = if i + 3 < bytes.len() { bytes[i + 3] } else { b' ' };
            if after == b' ' || after == b'\t' || after == b'\n' || after == b'\r' || after == b'<' {
                // candidate fun
                // find line idx for i
                let mut line_idx = 0usize;
                for (idx, &start) in line_starts.iter().enumerate() {
                    if start <= i {
                        line_idx = idx;
                    } else {
                        break;
                    }
                }
                // decor_start: include preceding annotation lines
                let mut decor_start = line_idx;
                if line_idx > 0 {
                    let mut j = line_idx as isize - 1;
                    while j >= 0 {
                        let prev = lines[j as usize];
                        let prev_trim = prev.trim();
                        if prev_trim.starts_with('@') {
                            decor_start = j as usize;
                            j -= 1;
                        } else if prev_trim.is_empty() {
                            break;
                        } else {
                            break;
                        }
                    }
                }
                let start_byte = line_starts[decor_start] + first_non_space_col(lines[decor_start]);

                // locate '(' for params to find closing ')'
                let mut paren_open: Option<usize> = None;
                // scan forward from i to find '(' respecting same states? but we are not in string etc, so simple search avoiding comments? Use a lightweight scan with string awareness from i
                // For simplicity, find next '(' via scanning with awareness (reuse find logic)
                {
                    let mut k = i + 3;
                    let mut k_in_line = false;
                    let mut k_block: i32 = 0;
                    let mut k_str = false;
                    let mut k_triple = false;
                    let mut k_char = false;
                    let mut k_esc = false;
                    while k < bytes.len() {
                        let kb = bytes[k];
                        let kn = if k + 1 < bytes.len() { Some(bytes[k + 1]) } else { None };
                        let kn2 = if k + 2 < bytes.len() { Some(bytes[k + 2]) } else { None };
                        if k_triple {
                            if kb == b'"' && kn == Some(b'"') && kn2 == Some(b'"') { k_triple = false; k += 3; continue; }
                            if k_esc { k_esc = false; } else if kb == b'\\' { k_esc = true; } else { k_esc = false; }
                            k += 1; continue;
                        }
                        if k_str {
                            if k_esc { k_esc = false; } else if kb == b'\\' { k_esc = true; } else if kb == b'"' { k_str = false; }
                            k += 1; continue;
                        }
                        if k_char {
                            if k_esc { k_esc = false; } else if kb == b'\\' { k_esc = true; } else if kb == b'\'' { k_char = false; }
                            k += 1; continue;
                        }
                        if k_in_line {
                            if kb == b'\n' { k_in_line = false; }
                            k += 1; continue;
                        }
                        if k_block > 0 {
                            if kb == b'/' && kn == Some(b'*') { k_block += 1; k += 2; continue; }
                            if kb == b'*' && kn == Some(b'/') { k_block -= 1; k += 2; continue; }
                            k += 1; continue;
                        }
                        if kb == b'/' && kn == Some(b'/') { k_in_line = true; k += 2; continue; }
                        if kb == b'/' && kn == Some(b'*') { k_block = 1; k += 2; continue; }
                        if kb == b'"' && kn == Some(b'"') && kn2 == Some(b'"') { k_triple = true; k += 3; continue; }
                        if kb == b'"' { k_str = true; k_esc = false; k += 1; continue; }
                        if kb == b'\'' { k_char = true; k_esc = false; k += 1; continue; }
                        if kb == b'(' {
                            paren_open = Some(k);
                            break;
                        }
                        if kb == b'\n' && k > i + 2000 { break; } // avoid infinite
                        if kb == b'{' || kb == b'=' { break; }
                        k += 1;
                    }
                }

                if let Some(open_paren) = paren_open {
                    if let Some(close_paren) = find_matching_paren_kotlin(content, open_paren) {
                        // after close_paren, search for brace or = at top level before next fun? Use find_open_brace from close_paren
                        let mut search_start = close_paren + 1;
                        // we need to skip strings/comments to find first '{' or '=' before ';'
                        let mut found_brace: Option<usize> = None;
                        let mut found_equals: Option<usize> = None;
                        {
                            let mut k = search_start;
                            let mut k_in_line = false;
                            let mut k_block: i32 = 0;
                            let mut k_str = false;
                            let mut k_triple = false;
                            let mut k_char = false;
                            let mut k_esc = false;
                            while k < bytes.len() {
                                let kb = bytes[k];
                                let kn = if k + 1 < bytes.len() { Some(bytes[k + 1]) } else { None };
                                let kn2 = if k + 2 < bytes.len() { Some(bytes[k + 2]) } else { None };
                                if k_triple {
                                    if kb == b'"' && kn == Some(b'"') && kn2 == Some(b'"') { k_triple = false; k += 3; continue; }
                                    if k_esc { k_esc = false; } else if kb == b'\\' { k_esc = true; } else { k_esc = false; }
                                    k += 1; continue;
                                }
                                if k_str {
                                    if k_esc { k_esc = false; } else if kb == b'\\' { k_esc = true; } else if kb == b'"' { k_str = false; }
                                    k += 1; continue;
                                }
                                if k_char {
                                    if k_esc { k_esc = false; } else if kb == b'\\' { k_esc = true; } else if kb == b'\'' { k_char = false; }
                                    k += 1; continue;
                                }
                                if k_in_line {
                                    if kb == b'\n' { k_in_line = false; }
                                    k += 1; continue;
                                }
                                if k_block > 0 {
                                    if kb == b'/' && kn == Some(b'*') { k_block += 1; k += 2; continue; }
                                    if kb == b'*' && kn == Some(b'/') { k_block -= 1; k += 2; continue; }
                                    k += 1; continue;
                                }
                                if kb == b'/' && kn == Some(b'/') { k_in_line = true; k += 2; continue; }
                                if kb == b'/' && kn == Some(b'*') { k_block = 1; k += 2; continue; }
                                if kb == b'"' && kn == Some(b'"') && kn2 == Some(b'"') { k_triple = true; k += 3; continue; }
                                if kb == b'"' { k_str = true; k_esc = false; k += 1; continue; }
                                if kb == b'\'' { k_char = true; k_esc = false; k += 1; continue; }
                                if kb == b'{' {
                                    found_brace = Some(k);
                                    break;
                                }
                                if kb == b'=' {
                                    // check not == or =>
                                    let nxt = if k + 1 < bytes.len() { bytes[k + 1] } else { b' ' };
                                    if nxt == b'=' || nxt == b'>' {
                                        k += 1;
                                        continue;
                                    }
                                    found_equals = Some(k);
                                    break;
                                }
                                if kb == b'\n' {
                                    // if we hit newline and next non-space is not '{' nor '=', maybe function without body -> check if next line contains '{' at start?
                                    // Look ahead for brace on next line: we already handle brace after newline via loop, so continue
                                    // But if we encounter ';' before brace, treat as abstract
                                    // For now continue scanning; but limit scanning to maybe 500 chars to avoid sweeping far
                                    if k - search_start > 800 {
                                        break;
                                    }
                                }
                                if kb == b';' {
                                    break;
                                }
                                k += 1;
                                if k - search_start > 1000 {
                                    break;
                                }
                            }
                        }

                        if let Some(open_pos) = found_brace {
                            // ensure brace is before equals if both found (brace first)
                            if let Some(_eq) = found_equals {
                                // if equals was found first, we already break at equals; so this branch is brace first
                            }
                            // check that between close_paren and open_pos there's no ';'
                            let between = &content[close_paren..open_pos];
                            if between.contains(';') {
                                i += 3;
                                continue;
                            }
                            if let Some(close_pos) = find_matching_brace_kotlin(content, open_pos) {
                                let (start_line, start_col) = byte_to_line_col(content, &line_starts, start_byte);
                                let (end_line, end_col) = byte_to_line_col(content, &line_starts, close_pos);
                                let source_text = content[start_byte..=close_pos].to_string();
                                let sig_text = content[start_byte..open_pos].trim().to_string();
                                let (visibility, modifiers, attributes, name, type_params, params, ret, is_suspend) =
                                    parse_kotlin_signature(&sig_text);
                                if name == "unknown" || name.is_empty() {
                                    i = close_pos + 1;
                                    in_line_comment = false;
                                    block_depth = 0;
                                    in_string = false;
                                    in_triple = false;
                                    in_char = false;
                                    escape = false;
                                    continue;
                                }
                                let mut enclosing: Vec<&TypeInfo> = type_infos
                                    .iter()
                                    .filter(|t| t.open < start_byte && close_pos < t.close)
                                    .collect();
                                enclosing.sort_by_key(|t| t.open);
                                let type_path = if enclosing.is_empty() {
                                    None
                                } else {
                                    Some(
                                        enclosing
                                            .iter()
                                            .map(|t| t.name.clone())
                                            .collect::<Vec<_>>()
                                            .join("::"),
                                    )
                                };
                                let innermost = enclosing.last();
                                let class_context = innermost.map(|t| t.name.clone());
                                let type_kind = innermost.map(|t| t.kind.clone());

                                result.push(KotlinFunctionRaw {
                                    name,
                                    return_type: ret,
                                    visibility,
                                    modifiers,
                                    attributes,
                                    parameters: params,
                                    type_params,
                                    is_suspend,
                                    class_context: class_context.clone(),
                                    type_path,
                                    type_kind,
                                    start_line,
                                    start_col,
                                    start_byte,
                                    end_line,
                                    end_col,
                                    end_byte: close_pos,
                                    source_text,
                                });
                                i = close_pos + 1;
                                in_line_comment = false;
                                block_depth = 0;
                                in_string = false;
                                in_triple = false;
                                in_char = false;
                                escape = false;
                                continue;
                            }
                        } else if let Some(eq_pos) = found_equals {
                            // expression body: capture single line or up to next newline depth 0, or brace block after =
                            // For simplicity, capture until end of line, but include following block if present
                            let mut expr_end = eq_pos;
                            // scan forward to find end of expression: next newline at depth 0 not in string, or ';' at depth 0
                            let mut k = eq_pos + 1;
                            let mut k_in_line = false;
                            let mut k_block: i32 = 0;
                            let mut k_str = false;
                            let mut k_triple = false;
                            let mut k_char = false;
                            let mut k_esc = false;
                            let mut depth_paren: i32 = 0;
                            let mut depth_bracket: i32 = 0;
                            let mut depth_brace: i32 = 0;
                            let mut last_non_space = eq_pos;
                            while k < bytes.len() {
                                let kb = bytes[k];
                                let kn = if k + 1 < bytes.len() { Some(bytes[k + 1]) } else { None };
                                let kn2 = if k + 2 < bytes.len() { Some(bytes[k + 2]) } else { None };
                                if k_triple {
                                    if kb == b'"' && kn == Some(b'"') && kn2 == Some(b'"') { k_triple = false; k += 3; last_non_space = k; continue; }
                                    if k_esc { k_esc = false; } else if kb == b'\\' { k_esc = true; } else { k_esc = false; }
                                    k += 1; continue;
                                }
                                if k_str {
                                    if k_esc { k_esc = false; } else if kb == b'\\' { k_esc = true; } else if kb == b'"' { k_str = false; }
                                    k += 1; continue;
                                }
                                if k_char {
                                    if k_esc { k_esc = false; } else if kb == b'\\' { k_esc = true; } else if kb == b'\'' { k_char = false; }
                                    k += 1; continue;
                                }
                                if k_in_line {
                                    if kb == b'\n' { k_in_line = false; break; }
                                    k += 1; continue;
                                }
                                if k_block > 0 {
                                    if kb == b'/' && kn == Some(b'*') { k_block += 1; k += 2; continue; }
                                    if kb == b'*' && kn == Some(b'/') { k_block -= 1; k += 2; continue; }
                                    k += 1; continue;
                                }
                                if kb == b'/' && kn == Some(b'/') { k_in_line = true; k += 2; continue; }
                                if kb == b'/' && kn == Some(b'*') { k_block = 1; k += 2; continue; }
                                if kb == b'"' && kn == Some(b'"') && kn2 == Some(b'"') { k_triple = true; k += 3; continue; }
                                if kb == b'"' { k_str = true; k_esc = false; k += 1; continue; }
                                if kb == b'\'' { k_char = true; k_esc = false; k += 1; continue; }
                                if kb == b'(' { depth_paren += 1; }
                                else if kb == b')' { if depth_paren > 0 { depth_paren -= 1; } }
                                else if kb == b'[' { depth_bracket += 1; }
                                else if kb == b']' { if depth_bracket > 0 { depth_bracket -= 1; } }
                                else if kb == b'{' { depth_brace += 1; }
                                else if kb == b'}' { if depth_brace > 0 { depth_brace -= 1; } else { // closing without opening? end
                                        // if we are at top level after '=', a '}' likely ends expression block, but we include it?
                                    }
                                }
                                if kb == b'\n' && depth_paren == 0 && depth_bracket == 0 && depth_brace == 0 {
                                    break;
                                }
                                if kb != b' ' && kb != b'\t' && kb != b'\r' && kb != b'\n' {
                                    last_non_space = k;
                                }
                                if kb == b';' && depth_paren == 0 && depth_bracket == 0 && depth_brace == 0 {
                                    expr_end = k - 1;
                                    break;
                                }
                                k += 1;
                                if k - eq_pos > 2000 { break; }
                            }
                            if expr_end == eq_pos {
                                expr_end = last_non_space;
                                if k < bytes.len() && bytes[k] == b'\n' {
                                    expr_end = k - 1;
                                    // trim trailing spaces already via last_non_space
                                    expr_end = last_non_space;
                                }
                            }
                            // ensure expr_end >= start_byte
                            if expr_end < start_byte {
                                expr_end = eq_pos;
                            }
                            let close_pos = expr_end;
                            let (start_line, start_col) = byte_to_line_col(content, &line_starts, start_byte);
                            let (end_line, end_col) = byte_to_line_col(content, &line_starts, close_pos);
                            let source_text = content[start_byte..=close_pos].to_string();
                            // sig up to equals
                            let sig_text = content[start_byte..eq_pos].trim().to_string();
                            let (visibility, modifiers, attributes, name, type_params, params, ret, is_suspend) =
                                parse_kotlin_signature(&sig_text);
                            if name == "unknown" || name.is_empty() {
                                i = close_pos + 1;
                                continue;
                            }
                            // For expression bodies, if ret is None, try to infer from sig_text after colon (parse already)
                            let mut enclosing: Vec<&TypeInfo> = type_infos
                                .iter()
                                .filter(|t| t.open < start_byte && close_pos < t.close)
                                .collect();
                            enclosing.sort_by_key(|t| t.open);
                            let type_path = if enclosing.is_empty() { None } else { Some(enclosing.iter().map(|t| t.name.clone()).collect::<Vec<_>>().join("::")) };
                            let innermost = enclosing.last();
                            let class_context = innermost.map(|t| t.name.clone());
                            let type_kind = innermost.map(|t| t.kind.clone());
                            result.push(KotlinFunctionRaw {
                                name,
                                return_type: ret,
                                visibility,
                                modifiers,
                                attributes,
                                parameters: params,
                                type_params,
                                is_suspend,
                                class_context: class_context.clone(),
                                type_path,
                                type_kind,
                                start_line,
                                start_col,
                                start_byte,
                                end_line,
                                end_col,
                                end_byte: close_pos,
                                source_text,
                            });
                            i = close_pos + 1;
                            in_line_comment = false;
                            block_depth = 0;
                            in_string = false;
                            in_triple = false;
                            in_char = false;
                            escape = false;
                            continue;
                        }
                    }
                }
            }
        }

        i += 1;
    }

    result.sort_by_key(|r| r.start_byte);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const SAMPLE_PACKAGE_CLASS: &str = r#"package com.example.test

class Foo {
    fun bar(x: Int): String {
        return x.toString()
    }

    suspend fun loadData(url: String): String {
        return url
    }
}
"#;

    const SAMPLE_EXTENSION_GENERICS: &str = r#"package com.example.ext

fun <T> List<T>.customFilter(predicate: (T) -> Boolean): List<T> {
    return this.filter(predicate)
}

fun String.foo(name: String = "default", age: Int = 42): Int {
    return name.length + age
}

suspend fun <R> fetchData(param: String): R {
    return param as R
}
"#;

    const SAMPLE_BRACES_COMMENTS: &str = r#"package com.example.brace

// line comment with { fun fake() { }
    /* block comment with fun fake2() { } and { } */
    /* nested /* comment with { } */ still comment */
    fun real() {
        val s = "string with { brace } and // comment"
        val t = """triple string with { and } brace
        still inside"""
        // comment brace {
        /* block brace { */
    }

    data class User(val name: String) {
        fun getName(): String {
            return name
        }
    }

    object MyObject {
        fun objectFun() {
        }
    }

    interface MyInterface {
        fun interfaceFun(): String {
            return "hi"
        }
    }

    companion object {
        fun companionFun() {
        }
    }
 "#;

    #[test]
    fn extracts_package_class() {
        let fns = extract(SAMPLE_PACKAGE_CLASS, Path::new("Foo.kt")).unwrap();
        assert_eq!(fns.len(), 2, "found {:?}", fns.iter().map(|f| &f.identity.name).collect::<Vec<_>>());
        let bar = fns.iter().find(|f| f.identity.name == "bar").unwrap();
        assert_eq!(bar.identity.qualified_name, "com.example.test::Foo::bar");
        assert_eq!(bar.signature.return_type.as_deref(), Some("String"));
        assert_eq!(bar.signature.parameters.len(), 1);
        assert_eq!(bar.signature.parameters[0].typ, "Int");
        assert_eq!(bar.context.package.as_deref(), Some("com.example.test"));
        assert_eq!(bar.context.class.as_deref(), Some("Foo"));
        assert_eq!(bar.identity.language, "kotlin");
        assert_eq!(bar.metadata.parser.as_deref(), Some("reko-kotlinExtractor"));
        assert!(bar.source.hash.starts_with("sha256:"));
        assert!(bar.source.location.start.byte < bar.source.location.end.byte);

        let load = fns.iter().find(|f| f.identity.name == "loadData").unwrap();
        assert_eq!(load.identity.qualified_name, "com.example.test::Foo::loadData");
        assert!(load.execution.is_async);
        assert!(load.declaration.modifiers.contains(&"suspend".to_string()));
    }

    #[test]
    fn extension_generics_defaults() {
        let fns = extract(SAMPLE_EXTENSION_GENERICS, Path::new("Ext.kt")).unwrap();
        assert_eq!(fns.len(), 3, "found {:?}", fns.iter().map(|f| &f.identity.name).collect::<Vec<_>>());
        let cf = fns.iter().find(|f| f.identity.name == "customFilter").unwrap();
        assert_eq!(cf.identity.qualified_name, "com.example.ext::customFilter");
        assert_eq!(cf.signature.type_parameters, vec!["T".to_string()]);
        assert_eq!(cf.signature.parameters.len(), 1);
        assert_eq!(cf.signature.return_type.as_deref(), Some("List<T>"));

        let foo = fns.iter().find(|f| f.identity.name == "foo").unwrap();
        assert_eq!(foo.signature.parameters.len(), 2);
        // default values should be stripped
        assert_eq!(foo.signature.parameters[0].name, "name");
        assert_eq!(foo.signature.parameters[0].typ, "String");
        assert_eq!(foo.signature.parameters[1].name, "age");
        assert_eq!(foo.signature.parameters[1].typ, "Int");
        assert_eq!(foo.signature.return_type.as_deref(), Some("Int"));

        let fetch = fns.iter().find(|f| f.identity.name == "fetchData").unwrap();
        assert!(fetch.execution.is_async);
        assert_eq!(fetch.signature.type_parameters, vec!["R".to_string()]);
        assert_eq!(fetch.signature.return_type.as_deref(), Some("R"));
    }

    #[test]
    fn brace_matching_and_types() {
        let fns = extract(SAMPLE_BRACES_COMMENTS, Path::new("Brace.kt")).unwrap();
        // real, getName, objectFun, interfaceFun, companionFun = 5 (companion nameless still inside? our companion object without name is ignored, so companionFun is top-level inside package but not inside class -> still should be found as top-level)
        // Actually companionFun is inside nameless companion object inside file scope, not inside class: it will be considered top-level
        assert!(fns.len() >= 4, "found {:?}", fns.iter().map(|f| format!("{} -> {}", f.identity.name, f.identity.qualified_name)).collect::<Vec<_>>());
        let real = fns.iter().find(|f| f.identity.name == "real").unwrap();
        assert!(real.source.source_text.contains("string with { brace }"));
        assert!(real.source.source_text.contains("triple string with { and }"));
        assert!(real.source.hash.starts_with("sha256:"));
        assert!(real.source.location.start.byte < real.source.location.end.byte);

        let get_name = fns.iter().find(|f| f.identity.name == "getName").unwrap();
        assert_eq!(get_name.identity.qualified_name, "com.example.brace::User::getName");
        assert_eq!(get_name.context.class.as_deref(), Some("User"));

        let obj_fun = fns.iter().find(|f| f.identity.name == "objectFun").unwrap();
        assert_eq!(obj_fun.identity.qualified_name, "com.example.brace::MyObject::objectFun");

        let iface = fns.iter().find(|f| f.identity.name == "interfaceFun").unwrap();
        assert_eq!(iface.context.interface.as_deref(), Some("MyInterface"));
    }

    #[test]
    fn extract_to_json_valid() {
        let json = extract_to_json(SAMPLE_PACKAGE_CLASS, Path::new("Foo.kt")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(v.is_array());
        assert_eq!(v.as_array().unwrap().len(), 2);
        assert_eq!(v[0]["identity"]["language"], "kotlin");
        assert_eq!(v[0]["metadata"]["parser"], "reko-kotlinExtractor");
    }
}
