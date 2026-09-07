use crate::ir::{IrFunction, Parameter, Position};
use anyhow::Result;
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Debug, Clone)]
struct SwiftFunctionRaw {
    name: String,
    return_type: Option<String>,
    visibility: String,
    modifiers: Vec<String>,
    attributes: Vec<String>,
    parameters: Vec<Parameter>,
    type_params: Vec<String>,
    throws: Vec<String>,
    is_async: bool,
    struct_context: Option<String>, // innermost type name for context.class/struct_
    type_context_kind: Option<String>,
    type_path: Option<String>, // joined path of all enclosing types
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
    kind: String, // class, struct, enum, actor, extension, protocol
    name: String,
    open: usize,
    close: usize,
}

/// Public entry: given file content and file path, extract IR for each Swift function.
pub fn extract(content: &str, file_path: &Path) -> Result<Vec<IrFunction>> {
    let module = file_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    let file_str = file_path.display().to_string();

    let raws = parse_swift_functions(content)?;

    let mut out = Vec::new();
    for r in raws {
        let hash = hash_source(&r.source_text);
        let qualified = if let Some(tp) = &r.type_path {
            format!("{}::{}::{}", module, tp, r.name)
        } else if let Some(ctx) = &r.struct_context {
            format!("{}::{}::{}", module, ctx, r.name)
        } else {
            format!("{}::{}", module, r.name)
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
            r.throws.clone(),
            None,
            r.struct_context.clone(),
        );
        ir.identity.language = "swift".to_string();
        ir.metadata.parser = Some("reko-swiftExtractor".to_string());
        ir.metadata.parser_version = Some("0.1.0".to_string());
        ir.metadata.confidence = 1.0;
        ir.context.module = Some(module.clone());
        ir.context.namespace = Some(module.clone());
        ir.source.module = module.clone();
        // Distribute type context correctly
        match r.type_context_kind.as_deref() {
            Some("struct") => {
                ir.context.struct_ = r.struct_context.clone();
                // keep class as original (from new_minimal) for compatibility, but also set
                // if needed, retain class as None to avoid confusion
                // We'll keep both: class mirrors struct for queries that look at class
            }
            Some("class") | Some("actor") | Some("extension") => {
                ir.context.class = r.struct_context.clone();
                // struct_ should be None for class contexts, but keep as is if extension of struct
                if r.type_context_kind.as_deref() == Some("struct") {
                    // already handled
                } else {
                    // ensure struct_ is None unless explicitly struct
                    // check if type_path indicates struct? Keep simple
                    if ir.context.struct_.is_some() && r.type_context_kind.as_deref() != Some("struct") {
                        // if innermost was class/extension, struct_ should be None; but if type_path includes struct,
                        // we already set innermost. So clear struct_ if not struct
                        let innermost_is_struct = r.type_context_kind.as_deref() == Some("struct");
                        if !innermost_is_struct {
                            ir.context.struct_ = None;
                        }
                    }
                }
            }
            Some("enum") => {
                // use struct_ to store enum name as well for backward compatibility, plus keep class None
                ir.context.struct_ = r.struct_context.clone();
            }
            Some("protocol") => {
                ir.context.interface = r.struct_context.clone();
            }
            _ => {
                ir.context.class = r.struct_context.clone();
            }
        }
        // Handle attributes/annotations
        ir.declaration.attributes = r.attributes.clone();
        ir.declaration.annotations = r.attributes.clone();
        ir.signature.type_parameters = r.type_params.clone();
        ir.execution.is_async = r.is_async;
        // effects could store async/throws but we already use throws field
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

// ---------- Swift-aware brace handling ----------
fn find_open_brace_swift(content: &str, start_byte: usize) -> Option<usize> {
    let bytes = content.as_bytes();
    let mut i = start_byte;
    let mut in_line_comment = false;
    let mut block_depth: i32 = 0;
    let mut in_string = false;
    let mut in_triple = false;
    let mut in_backtick = false;
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
            // triple strings handle escapes? treat \" as escaped but still inside
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
        if in_backtick {
            if b == b'`' {
                in_backtick = false;
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

        // not in any
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
        if b == b'`' {
            in_backtick = true;
            i += 1;
            continue;
        }
        if b == b'{' {
            return Some(i);
        }
        if b == b';' {
            // For protocol func declarations without body, caller will check ';' before '{'
            // but we continue searching for brace; we don't return None here.
        }
        i += 1;
    }
    None
}

fn find_matching_brace_swift(content: &str, open_pos: usize) -> Option<usize> {
    let bytes = content.as_bytes();
    let mut depth: i32 = 0;
    let mut i = open_pos;
    let mut in_line_comment = false;
    let mut block_depth: i32 = 0;
    let mut in_string = false;
    let mut in_triple = false;
    let mut in_backtick = false;
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
        if in_backtick {
            if b == b'`' {
                in_backtick = false;
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
        if b == b'`' {
            in_backtick = true;
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

// ---------- helpers for signature parsing ----------
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

fn parse_swift_params(s: &str) -> Vec<Parameter> {
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
        // p examples: "_ name: Type", "label name: Type = default", "name: Type...", "name: @escaping (Int) -> Void"
        // Remove default value after '='
        let without_default = p.split('=').next().unwrap_or(p).trim();
        // Find colon
        if let Some(colon_idx) = without_default.find(':') {
            let before = without_default[..colon_idx].trim();
            let typ = without_default[colon_idx + 1..].trim().to_string();
            // before may be "label name" or "_ name" or "name"
            let tokens: Vec<&str> = before.split_whitespace().collect();
            let name_raw = tokens.last().unwrap_or(&"_").to_string();
            let name = name_raw.trim_matches('`').to_string();
            let name = if name.is_empty() { format!("arg{}", out.len()) } else { name };
            let typ = if typ.is_empty() { "Any".to_string() } else { typ };
            out.push(Parameter { name, typ });
        } else {
            // No colon: could be like "Int" type only? fallback to indexed name
            // Check if part contains whitespace -> last token maybe name? But Swift params always have colon.
            // We'll treat as name with Any type
            let name = p.trim().trim_matches('`').to_string();
            if name.is_empty() {
                continue;
            }
            // If it looks like a type without name, still create param
            out.push(Parameter { name, typ: "Any".to_string() });
        }
    }
    out
}

fn parse_swift_signature(sig: &str) -> (String, Vec<String>, Vec<String>, String, Vec<String>, Vec<Parameter>, Option<String>, Vec<String>, bool) {
    // Returns (visibility, modifiers, attributes, name, type_params, params, ret, throws, is_async)
    let mut visibility = "internal".to_string();
    let mut modifiers: Vec<String> = Vec::new();
    let mut attributes: Vec<String> = Vec::new();
    let mut throws_vec: Vec<String> = Vec::new();
    let mut is_async = false;

    // Find func keyword
    let func_pos_opt = sig.find("func ");
    let func_pos = if let Some(p) = func_pos_opt {
        p
    } else if let Some(p) = sig.find("func\t") {
        p
    } else {
        return (visibility, modifiers, attributes, "unknown".to_string(), vec![], vec![], None, throws_vec, is_async);
    };
    let prefix = sig[..func_pos].trim().to_string();
    // collect attributes starting with @
    for tok in prefix.split_whitespace() {
        if tok.starts_with('@') {
            // attribute may be like @discardableResult or @available(...)
            // keep up to '(' balanced? For simplicity take token up to whitespace; but @available(iOS 13, *) spans spaces? Actually prefix split loses it.
            // So better extract attributes via scanning prefix for '@' to whitespace respecting parens.
            // For now push token
            attributes.push(tok.to_string());
        }
    }
    // Also extract attributes with parentheses correctly: scan prefix for '@'
    // Re-extract more accurately
    attributes.clear();
    {
        let mut i = 0;
        let chars: Vec<char> = prefix.chars().collect();
        while i < chars.len() {
            if chars[i] == '@' {
                let mut attr = String::new();
                attr.push('@');
                i += 1;
                let mut depth_paren = 0;
                while i < chars.len() {
                    let c = chars[i];
                    if c == '(' {
                        depth_paren += 1;
                        attr.push(c);
                    } else if c == ')' {
                        if depth_paren > 0 {
                            depth_paren -= 1;
                            attr.push(c);
                            if depth_paren == 0 {
                                i += 1;
                                break;
                            }
                        } else {
                            break;
                        }
                    } else if c.is_whitespace() && depth_paren == 0 {
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

    // Visibility detection
    let prefix_low = prefix.to_lowercase();
    if prefix_low.contains("public") || prefix_low.contains("open") {
        visibility = "public".to_string();
    } else if prefix_low.contains("private") || prefix_low.contains("fileprivate") {
        visibility = "private".to_string();
    } else if prefix_low.contains("internal") {
        visibility = "internal".to_string();
    } else {
        visibility = "internal".to_string();
    }
    // Modifiers
    if prefix.contains("static") {
        modifiers.push("static".to_string());
    }
    if prefix.contains("class ") || prefix.split_whitespace().any(|t| t == "class") {
        // avoid duplicating static if already, but class func is a modifier
        if !modifiers.contains(&"class".to_string()) && prefix.contains("class") {
            // Ensure we don't capture 'class' from 'class MyClass' contexts; but prefix is before func so class there means class func
            modifiers.push("class".to_string());
        }
    }
    if prefix.contains("final") {
        modifiers.push("final".to_string());
    }
    if prefix.contains("override") {
        modifiers.push("override".to_string());
    }
    if prefix.contains("mutating") {
        modifiers.push("mutating".to_string());
    }
    if prefix.contains("nonmutating") {
        modifiers.push("nonmutating".to_string());
    }
    if prefix.contains("required") {
        modifiers.push("required".to_string());
    }
    if prefix.contains("convenience") {
        modifiers.push("convenience".to_string());
    }
    // async/throws may appear before func in some contexts? Check prefix
    if prefix.contains("async") {
        is_async = true;
        if !modifiers.contains(&"async".to_string()) {
            modifiers.push("async".to_string());
        }
    }
    if prefix.contains("throws") || prefix.contains("rethrows") {
        if prefix.contains("rethrows") {
            throws_vec.push("rethrows".to_string());
        } else {
            throws_vec.push("throws".to_string());
        }
        if !modifiers.contains(&"throws".to_string()) && prefix.contains("throws") {
            // do not duplicate if already in throws_vec, but modifiers may include throws for visibility
        }
    }

    let after_func = sig[func_pos + 4..].trim_start().to_string(); // after "func"
    // Extract name: up to '(' or '<' or whitespace
    let mut name_end = 0usize;
    for (idx, ch) in after_func.char_indices() {
        if ch.is_alphanumeric() || ch == '_' || ch == '`' {
            name_end = idx + ch.len_utf8();
        } else {
            break;
        }
    }
    let mut name = if name_end > 0 {
        after_func[..name_end].trim_matches('`').to_string()
    } else {
        "unknown".to_string()
    };
    // Handle operator func names like "==" ? Swift allows func == ... For simplicity if name is operator chars, fallback
    if name.is_empty() || name == "unknown" {
        // Check for operator: extract until '('
        if let Some(paren) = after_func.find('(') {
            let candidate = after_func[..paren].trim().trim_matches('`').to_string();
            if !candidate.is_empty() {
                name = candidate;
            }
        }
    }

    let remainder_after_name = if name_end < after_func.len() {
        &after_func[name_end..]
    } else {
        ""
    };

    // Generics handling
    let mut type_params: Vec<String> = Vec::new();
    let mut after_generics_offset = 0usize;
    let remainder_trim = remainder_after_name.trim_start();
    if remainder_trim.starts_with('<') {
        let start_in_remainder = remainder_after_name.len() - remainder_trim.len();
        let mut depth = 0i32;
        let mut end_idx: Option<usize> = None;
        for (idx, ch) in remainder_after_name[start_in_remainder..].char_indices() {
            if ch == '<' {
                depth += 1;
            } else if ch == '>' {
                depth -= 1;
                if depth == 0 {
                    end_idx = Some(start_in_remainder + idx);
                    break;
                }
            }
        }
        if let Some(end) = end_idx {
            let inner = remainder_after_name[start_in_remainder + 1..end].to_string();
            if !inner.trim().is_empty() {
                type_params = split_params_respecting(&inner)
                    .into_iter()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            after_generics_offset = end + 1;
        }
    }

    // Now find params
    let params_search_slice = if type_params.is_empty() {
        remainder_after_name
    } else {
        &remainder_after_name[after_generics_offset..]
    };
    let paren_start_rel = params_search_slice.find('(');
    let (params_str, after_params_str) = if let Some(rel) = paren_start_rel {
        let absolute_start = if type_params.is_empty() {
            rel
        } else {
            after_generics_offset + rel
        };
        let search_in = &remainder_after_name[absolute_start..];
        if let Some(end_rel) = find_matching_paren(search_in, 0) {
            let inner = search_in[1..end_rel].to_string();
            let after = search_in[end_rel + 1..].to_string();
            (inner, after)
        } else {
            (String::new(), String::new())
        }
    } else {
        (String::new(), String::new())
    };

    let parameters = parse_swift_params(&params_str);

    let after_trim = after_params_str.trim().to_string();
    // Check async/throws after params
    if after_trim.contains("async") {
        is_async = true;
        if !modifiers.contains(&"async".to_string()) {
            modifiers.push("async".to_string());
        }
    }
    if after_trim.contains("rethrows") {
        if !throws_vec.contains(&"rethrows".to_string()) {
            throws_vec.push("rethrows".to_string());
        }
    } else if after_trim.contains("throws") {
        if !throws_vec.contains(&"throws".to_string()) {
            // Check for typed throws like throws(MyError) -> still push throws
            throws_vec.push("throws".to_string());
        }
    }
    // Return type
    let mut return_type: Option<String> = None;
    if let Some(arrow_pos) = after_trim.find("->") {
        let after_arrow = after_trim[arrow_pos + 2..].trim().to_string();
        // Remove where clause tail and trailing '{' remnants (already excluded)
        let where_pos = after_arrow.find(" where ");
        let ret_part = if let Some(wp) = where_pos {
            after_arrow[..wp].trim().to_string()
        } else {
            after_arrow
        };
        // Also handle where without spaces? e.g., "->T where..."
        let ret_part = ret_part.trim().to_string();
        if !ret_part.is_empty() {
            // Remove any trailing async/throws that might have been after return? Not needed
            return_type = Some(ret_part);
        }
    }

    (visibility, modifiers, attributes, name, type_params, parameters, return_type, throws_vec, is_async)
}

fn find_matching_paren(s: &str, open_idx: usize) -> Option<usize> {
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
    let mut in_backtick = false;
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
        if in_backtick {
            if b == b'`' {
                in_backtick = false;
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
        if b == b'`' {
            in_backtick = true;
            i += 1;
            continue;
        }

        // Check for type keywords
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
            } else if i + 6 <= bytes.len() && &bytes[i..i + 6] == b"struct" {
                let after = if i + 6 < bytes.len() { bytes[i + 6] } else { b' ' };
                if !(after.is_ascii_alphanumeric() || after == b'_') {
                    kind_opt = Some("struct");
                    kw_len = 6;
                }
            } else if i + 4 <= bytes.len() && &bytes[i..i + 4] == b"enum" {
                let after = if i + 4 < bytes.len() { bytes[i + 4] } else { b' ' };
                if !(after.is_ascii_alphanumeric() || after == b'_') {
                    kind_opt = Some("enum");
                    kw_len = 4;
                }
            } else if i + 5 <= bytes.len() && &bytes[i..i + 5] == b"actor" {
                let after = if i + 5 < bytes.len() { bytes[i + 5] } else { b' ' };
                if !(after.is_ascii_alphanumeric() || after == b'_') {
                    kind_opt = Some("actor");
                    kw_len = 5;
                }
            } else if i + 9 <= bytes.len() && &bytes[i..i + 9] == b"extension" {
                let after = if i + 9 < bytes.len() { bytes[i + 9] } else { b' ' };
                if !(after.is_ascii_alphanumeric() || after == b'_') {
                    kind_opt = Some("extension");
                    kw_len = 9;
                }
            } else if i + 8 <= bytes.len() && &bytes[i..i + 8] == b"protocol" {
                let after = if i + 8 < bytes.len() { bytes[i + 8] } else { b' ' };
                if !(after.is_ascii_alphanumeric() || after == b'_') {
                    kind_opt = Some("protocol");
                    kw_len = 8;
                }
            }

            if let Some(kind) = kind_opt {
                let mut j = i + kw_len;
                while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t' || bytes[j] == b'\n' || bytes[j] == b'\r') {
                    j += 1;
                }
                let name_start = j;
                while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_' || bytes[j] == b'.') {
                    j += 1;
                }
                if j > name_start {
                    let raw_name = String::from_utf8_lossy(&bytes[name_start..j]).to_string();
                    // For extension with generics or protocol conformance, take first component before ':' or '<' or '.'
                    let name = raw_name
                        .split(|c| c == ':' || c == '<' || c == '.' || c == ' ')
                        .next()
                        .unwrap_or(&raw_name)
                        .to_string();
                    if !name.is_empty() {
                        if let Some(open_pos) = find_open_brace_swift(content, j) {
                            let between = &content[j..open_pos];
                            if !between.contains(';') {
                                if let Some(close_pos) = find_matching_brace_swift(content, open_pos) {
                                    types.push(TypeInfo {
                                        kind: kind.to_string(),
                                        name,
                                        open: open_pos,
                                        close: close_pos,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }

        i += 1;
    }

    types.sort_by_key(|t| t.open);
    types
}

fn parse_swift_functions(content: &str) -> Result<Vec<SwiftFunctionRaw>> {
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
    let mut in_backtick = false;
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
        if in_backtick {
            if b == b'`' {
                in_backtick = false;
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
        if b == b'`' {
            in_backtick = true;
            i += 1;
            continue;
        }

        // check for func keyword
        let prev_is_ident = if i > 0 { bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_' } else { false };
        if !prev_is_ident && i + 4 <= bytes.len() && &bytes[i..i + 4] == b"func" {
            let after = if i + 4 < bytes.len() { bytes[i + 4] } else { b' ' };
            if !(after.is_ascii_alphanumeric() || after == b'_' || after == b'`') {
                // candidate func - ensure word boundary before
                // Find line idx for i
                let mut line_idx = 0usize;
                for (idx, &start) in line_starts.iter().enumerate() {
                    if start <= i {
                        line_idx = idx;
                    } else {
                        break;
                    }
                }
                // For start_byte, include preceding attribute lines
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

                // Find open brace
                if let Some(open_pos) = find_open_brace_swift(content, i) {
                    let between = &content[i..open_pos];
                    if between.contains(';') {
                        i += 4;
                        continue;
                    }
                    if let Some(close_pos) = find_matching_brace_swift(content, open_pos) {
                        let (start_line, start_col) = byte_to_line_col(content, &line_starts, start_byte);
                        let (end_line, end_col) = byte_to_line_col(content, &line_starts, close_pos);
                        let source_text = content[start_byte..=close_pos].to_string();
                        let sig_text = content[start_byte..open_pos].trim().to_string();
                        let (visibility, modifiers, attributes, name, type_params, params, ret, throws, is_async) =
                            parse_swift_signature(&sig_text);
                        if name == "unknown" || name.is_empty() {
                            i = close_pos + 1;
                            // reset state after jump
                            in_line_comment = false;
                            block_depth = 0;
                            in_string = false;
                            in_triple = false;
                            in_backtick = false;
                            escape = false;
                            continue;
                        }

                        // Determine enclosing type path
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
                        let struct_context = innermost.map(|t| t.name.clone());
                        let type_kind = innermost.map(|t| t.kind.clone());

                        result.push(SwiftFunctionRaw {
                            name,
                            return_type: ret,
                            visibility,
                            modifiers,
                            attributes,
                            parameters: params,
                            type_params,
                            throws,
                            is_async,
                            struct_context: struct_context.clone(),
                            type_context_kind: type_kind,
                            type_path,
                            start_line,
                            start_col,
                            start_byte,
                            end_line,
                            end_col,
                            end_byte: close_pos,
                            source_text,
                        });

                        // jump after close and reset state
                        i = close_pos + 1;
                        in_line_comment = false;
                        block_depth = 0;
                        in_string = false;
                        in_triple = false;
                        in_backtick = false;
                        escape = false;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const SAMPLE_SIMPLE: &str = r#"func hello(name: String) -> String {
    return "hello \(name)"
}

private func add(a: Int, b: Int) -> Int {
    return a + b
}

public static func staticMethod() {
}
"#;

    const SAMPLE_CLASS_STRUCT: &str = r#"class MyClass {
    func methodOne(x: Int) -> Int {
        return x + 1
    }

    private static func helper() throws -> String {
        return "hi"
    }
}

struct MyStruct {
    func structMethod() async -> Int {
        return 42
    }
}

enum MyEnum {
    func enumMethod() {}
}

extension MyClass {
    func extMethod(a: String) -> Bool {
        return true
    }
}
"#;

    const SAMPLE_BRACES_COMMENTS: &str = r#"// line comment with { func fake() { }
    /* block comment with func fake2() { } and { } */
    /* nested /* comment with { } */ still comment */
    func real() {
        let s = "string with { brace } and // comment"
        let t = """triple string with { and } brace
        still inside"""
        let u = `backtick with { }`
        // comment brace {
        /* block brace { */
    }

    @discardableResult
    public func withAttributes<T: Equatable>(x: T) async throws -> T where T: Equatable {
        return x
    }

    func noReturn() {
    }
"#;

    #[test]
    fn extracts_simple_and_modifiers() {
        let fns = extract(SAMPLE_SIMPLE, Path::new("Simple.swift")).unwrap();
        assert_eq!(fns.len(), 3, "found {:?}", fns.iter().map(|f| &f.identity.name).collect::<Vec<_>>());
        let hello = fns.iter().find(|f| f.identity.name == "hello").unwrap();
        assert_eq!(hello.identity.qualified_name, "Simple::hello");
        assert_eq!(hello.signature.parameters.len(), 1);
        assert_eq!(hello.signature.return_type.as_deref(), Some("String"));
        assert_eq!(hello.identity.language, "swift");
        assert_eq!(hello.metadata.parser.as_deref(), Some("reko-swiftExtractor"));
        assert!(hello.source.hash.starts_with("sha256:"));
        assert!(hello.source.location.start.byte < hello.source.location.end.byte);
        assert_eq!(hello.declaration.visibility, "internal");

        let add = fns.iter().find(|f| f.identity.name == "add").unwrap();
        assert_eq!(add.declaration.visibility, "private");
        assert_eq!(add.identity.qualified_name, "Simple::add");
        assert_eq!(add.signature.return_type.as_deref(), Some("Int"));

        let sm = fns.iter().find(|f| f.identity.name == "staticMethod").unwrap();
        assert!(sm.declaration.modifiers.contains(&"static".to_string()));
        assert_eq!(sm.declaration.visibility, "public");
    }

    #[test]
    fn class_struct_enum_extension_context() {
        let fns = extract(SAMPLE_CLASS_STRUCT, Path::new("Types.swift")).unwrap();
        assert_eq!(fns.len(), 5, "found {:?}", fns.iter().map(|f| format!("{} -> {}", f.identity.name, f.identity.qualified_name)).collect::<Vec<_>>());
        let m1 = fns.iter().find(|f| f.identity.name == "methodOne").unwrap();
        assert_eq!(m1.identity.qualified_name, "Types::MyClass::methodOne");
        assert_eq!(m1.context.class.as_deref(), Some("MyClass"));

        let helper = fns.iter().find(|f| f.identity.name == "helper").unwrap();
        assert_eq!(helper.identity.qualified_name, "Types::MyClass::helper");
        assert_eq!(helper.declaration.visibility, "private");
        assert!(helper.declaration.modifiers.contains(&"static".to_string()));
        assert_eq!(helper.signature.throws, vec!["throws".to_string()]);

        let sm = fns.iter().find(|f| f.identity.name == "structMethod").unwrap();
        assert_eq!(sm.identity.qualified_name, "Types::MyStruct::structMethod");
        assert_eq!(sm.context.struct_.as_deref(), Some("MyStruct"));
        assert!(sm.execution.is_async);

        let em = fns.iter().find(|f| f.identity.name == "enumMethod").unwrap();
        assert_eq!(em.identity.qualified_name, "Types::MyEnum::enumMethod");

        let ext = fns.iter().find(|f| f.identity.name == "extMethod").unwrap();
        assert_eq!(ext.identity.qualified_name, "Types::MyClass::extMethod");
        assert_eq!(ext.context.class.as_deref(), Some("MyClass"));
        assert_eq!(ext.signature.return_type.as_deref(), Some("Bool"));
    }

    #[test]
    fn brace_matching_async_throws_generics_and_attributes() {
        let fns = extract(SAMPLE_BRACES_COMMENTS, Path::new("Brace.swift")).unwrap();
        assert_eq!(fns.len(), 3, "found {:?}", fns.iter().map(|f| &f.identity.name).collect::<Vec<_>>());
        let real = fns.iter().find(|f| f.identity.name == "real").unwrap();
        assert!(real.source.source_text.contains("string with { brace }"));
        assert!(real.source.source_text.contains("triple string with { and }"));
        assert!(real.source.source_text.contains("backtick with { }"));
        assert!(real.source.location.start.byte < real.source.location.end.byte);
        assert!(real.source.hash.starts_with("sha256:"));

        let wa = fns.iter().find(|f| f.identity.name == "withAttributes").unwrap();
        assert_eq!(wa.declaration.visibility, "public");
        assert!(wa.execution.is_async);
        assert_eq!(wa.signature.throws, vec!["throws".to_string()]);
        assert_eq!(wa.signature.return_type.as_deref(), Some("T"));
        assert!(wa.signature.type_parameters.contains(&"T: Equatable".to_string()));
        assert!(wa.declaration.attributes.iter().any(|a| a.contains("discardableResult")));
        assert_eq!(wa.identity.qualified_name, "Brace::withAttributes");

        let nr = fns.iter().find(|f| f.identity.name == "noReturn").unwrap();
        assert_eq!(nr.signature.return_type, None);
    }

    #[test]
    fn extract_to_json_valid() {
        let json = extract_to_json(SAMPLE_SIMPLE, Path::new("Simple.swift")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(v.is_array());
        assert_eq!(v.as_array().unwrap().len(), 3);
        // ensure language field is swift in json
        assert_eq!(v[0]["identity"]["language"], "swift");
        assert_eq!(v[0]["metadata"]["parser"], "reko-swiftExtractor");
    }
}
