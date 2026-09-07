use crate::ir::{IrFunction, Parameter, Position};
use anyhow::Result;
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Debug, Clone)]
struct RustFunctionRaw {
    name: String,
    return_type: Option<String>,
    visibility: String,
    modifiers: Vec<String>,
    parameters: Vec<Parameter>,
    type_params: Vec<String>,
    constraints: Vec<String>,
    is_async: bool,
    struct_context: Option<String>,
    mod_path: Option<String>,
    start_line: usize,
    start_col: usize,
    start_byte: usize,
    end_line: usize,
    end_col: usize,
    end_byte: usize,
    source_text: String,
}

/// Public entry: given file content (exact, from reader) and file path, extract IR for each Rust function.
pub fn extract(content: &str, file_path: &Path) -> Result<Vec<IrFunction>> {
    let module = file_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    let file_str = file_path.display().to_string();

    let raws = parse_rust_functions(content)?;

    let mut out = Vec::new();
    for r in raws {
        let hash = hash_source(&r.source_text);
        // qualified_name like crate::Struct::method or crate::mod::func or crate::func
        let qualified = build_qualified(&r.mod_path, &r.struct_context, &r.name);

        let id = qualified.clone();

        // visibility string already "public"/"private"
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
            r.struct_context.clone(),
        );
        ir.identity.language = "rust".to_string();
        ir.metadata.parser = Some("reko-rustExtractor".to_string());
        ir.metadata.parser_version = Some("0.1.0".to_string());
        ir.context.module = Some(module.clone());
        if let Some(mp) = &r.mod_path {
            // namespace/module reflects mod path if present, else module file stem
            ir.context.namespace = Some(format!("crate::{}", mp));
            // keep original module for source
        } else {
            ir.context.namespace = Some("crate".to_string());
        }
        // package stays None for Rust
        ir.context.package = None;
        ir.context.struct_ = r.struct_context.clone();
        // class alias also set via new_minimal's class param, but ensure
        ir.context.class = r.struct_context.clone();
        ir.source.module = module.clone();
        ir.signature.type_parameters = r.type_params.clone();
        ir.signature.constraints = r.constraints.clone();
        ir.execution.is_async = r.is_async;

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

fn build_qualified(mod_path: &Option<String>, struct_ctx: &Option<String>, name: &str) -> String {
    let mut parts = vec!["crate".to_string()];
    if let Some(m) = mod_path {
        if !m.is_empty() {
            parts.push(m.clone());
        }
    }
    if let Some(s) = struct_ctx {
        if !s.is_empty() {
            parts.push(s.clone());
        }
    }
    parts.push(name.to_string());
    parts.join("::")
}

// ---------- Rust-aware brace handling ----------

fn is_char_literal(bytes: &[u8], pos: usize) -> bool {
    // Check if ' at pos is a char literal '\'' or '\n' style with closing ' within next 4 bytes
    // Pattern: 'X' length 3, or '\' + escaped (1-2 chars) + '\''
    if bytes[pos] != b'\'' {
        return false;
    }
    if pos + 2 >= bytes.len() {
        return false;
    }
    // Check next char after opening '
    // If second char is '\' then need to find closing ' within 3-4
    if bytes[pos + 1] == b'\\' {
        // escaped: '\'' , '\\', '\n', '\x..' etc - look for ' within pos+2..pos+4
        for k in pos + 2..std::cmp::min(pos + 5, bytes.len()) {
            if bytes[k] == b'\'' {
                return true;
            }
        }
        return false;
    } else {
        // simple 'a' or '{' etc
        if pos + 2 < bytes.len() && bytes[pos + 2] == b'\'' {
            return true;
        }
        // empty? not
        return false;
    }
}

fn raw_string_hashes(bytes: &[u8], pos: usize) -> Option<usize> {
    // Check if at pos we have r + #* + "
    // pos points to 'r'
    if bytes[pos] != b'r' {
        return None;
    }
    let mut j = pos + 1;
    let mut hashes = 0usize;
    while j < bytes.len() && bytes[j] == b'#' {
        hashes += 1;
        j += 1;
    }
    if j < bytes.len() && bytes[j] == b'"' {
        Some(hashes)
    } else {
        None
    }
}

fn find_open_brace_rust(content: &str, start_byte: usize) -> Option<usize> {
    let bytes = content.as_bytes();
    let mut i = start_byte;
    let mut in_line_comment = false;
    let mut in_block_comment: i32 = 0;
    let mut in_string = false;
    let mut in_char = false;
    let mut in_backtick = false;
    let mut in_raw: Option<usize> = None; // hashes
    let mut escape = false;

    while i < bytes.len() {
        let b = bytes[i];
        let next = if i + 1 < bytes.len() { Some(bytes[i + 1]) } else { None };

        if let Some(_hashes) = in_raw {
            // inside raw string, look for closing
            if b == b'"' {
                // check if followed by same number of #
                let mut ok = true;
                for h in 0.._hashes {
                    if i + 1 + h >= bytes.len() || bytes[i + 1 + h] != b'#' {
                        ok = false;
                        break;
                    }
                }
                if ok {
                    // close raw
                    // advance past " + hashes
                    i += 1 + _hashes;
                    in_raw = None;
                    continue;
                }
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
        if in_block_comment > 0 {
            if b == b'/' && next == Some(b'*') {
                in_block_comment += 1;
                i += 2;
                continue;
            }
            if b == b'*' && next == Some(b'/') {
                in_block_comment -= 1;
                i += 2;
                continue;
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
        if in_backtick {
            if b == b'`' {
                in_backtick = false;
            }
            i += 1;
            continue;
        }

        // not in any, check for raw start
        if b == b'r' {
            if let Some(h) = raw_string_hashes(bytes, i) {
                // raw string starts at r, but actual string content starts at " after hashes
                // mark in_raw and jump to after opening "
                // opening pattern is r + #* + "
                let opening_len = 1 + h + 1;
                i += opening_len;
                in_raw = Some(h);
                continue;
            }
        }
        if b == b'/' && next == Some(b'/') {
            in_line_comment = true;
            i += 2;
            continue;
        }
        if b == b'/' && next == Some(b'*') {
            in_block_comment = 1;
            i += 2;
            continue;
        }
        if b == b'"' {
            in_string = true;
            i += 1;
            continue;
        }
        if b == b'\'' {
            if is_char_literal(bytes, i) {
                in_char = true;
                i += 1;
                continue;
            } else {
                // lifetime, just skip '
                i += 1;
                continue;
            }
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
            // prototype terminator before brace -> no open brace for this decl
            // But we shouldn't return None immediately if searching for fn -> caller will detect ;
            // For generic open search we still treat ';' as not found? We'll let matching logic handle.
            // Just continue; the caller will check for ';' before '{' separately when needed.
        }
        i += 1;
    }
    None
}

fn find_matching_brace_rust(content: &str, open_pos: usize) -> Option<usize> {
    let bytes = content.as_bytes();
    let mut depth: i32 = 0;
    let mut i = open_pos;
    let mut in_line_comment = false;
    let mut in_block_comment: i32 = 0;
    let mut in_string = false;
    let mut in_char = false;
    let mut in_backtick = false;
    let mut in_raw: Option<usize> = None;
    let mut escape = false;

    while i < bytes.len() {
        let b = bytes[i];
        let next = if i + 1 < bytes.len() { Some(bytes[i + 1]) } else { None };

        if let Some(_hashes) = in_raw {
            if b == b'"' {
                let mut ok = true;
                for h in 0.._hashes {
                    if i + 1 + h >= bytes.len() || bytes[i + 1 + h] != b'#' {
                        ok = false;
                        break;
                    }
                }
                if ok {
                    i += 1 + _hashes;
                    in_raw = None;
                    continue;
                }
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
        if in_block_comment > 0 {
            if b == b'/' && next == Some(b'*') {
                in_block_comment += 1;
                i += 2;
                continue;
            }
            if b == b'*' && next == Some(b'/') {
                in_block_comment -= 1;
                i += 2;
                continue;
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
        if in_backtick {
            if b == b'`' {
                in_backtick = false;
            }
            i += 1;
            continue;
        }

        if b == b'r' {
            if let Some(h) = raw_string_hashes(bytes, i) {
                let opening_len = 1 + h + 1;
                i += opening_len;
                in_raw = Some(h);
                continue;
            }
        }
        if b == b'/' && next == Some(b'/') {
            in_line_comment = true;
            i += 2;
            continue;
        }
        if b == b'/' && next == Some(b'*') {
            in_block_comment = 1;
            i += 2;
            continue;
        }
        if b == b'"' {
            in_string = true;
            i += 1;
            continue;
        }
        if b == b'\'' {
            if is_char_literal(bytes, i) {
                in_char = true;
                i += 1;
                continue;
            } else {
                i += 1;
                continue;
            }
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

// ---------- Helpers for signature parsing ----------

fn parse_visibility_and_modifiers(prefix: &str) -> (String, Vec<String>) {
    let mut visibility = "private".to_string();
    let mut modifiers: Vec<String> = Vec::new();
    let mut raw_vis: Option<String> = None;

    // prefix is trimmed string before fn, may contain e.g. "pub(crate) async const unsafe"
    // We split but need to keep pub(...) together
    // Approach: iterate tokens with awareness of parentheses
    let tokens = split_vis_tokens(prefix);
    for tok in tokens {
        let t = tok.trim();
        if t.is_empty() {
            continue;
        }
        if t.starts_with("pub") {
            raw_vis = Some(t.to_string());
            visibility = "public".to_string();
        } else if t == "async" || t == "const" || t == "unsafe" || t == "extern" {
            modifiers.push(t.to_string());
        } else if t.starts_with("extern") {
            modifiers.push("extern".to_string());
        } else if t == "pub" {
            raw_vis = Some("pub".to_string());
            visibility = "public".to_string();
        }
    }
    // Keep raw_vis also in modifiers for fidelity if needed? Put pub variant as modifier too
    if let Some(rv) = raw_vis.clone() {
        if rv != "pub" {
            // If visibility was pub(crate) etc, add that as modifier distinction
            // But visibility field already public; modifiers can include the raw
            // To avoid duplication, push raw if not already in modifiers
            if !modifiers.contains(&rv) {
                // Place at front
                modifiers.insert(0, rv);
            }
        } else if !modifiers.contains(&"pub".to_string()) {
            // optionally include pub
            // We will not include plain pub to keep modifiers minimal, but could.
        }
    }

    (visibility, modifiers)
}

fn split_vis_tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    for ch in s.chars() {
        match ch {
            '(' => {
                depth += 1;
                cur.push(ch);
            }
            ')' => {
                depth -= 1;
                cur.push(ch);
            }
            ' ' | '\t' | '\n' if depth == 0 => {
                if !cur.trim().is_empty() {
                    out.push(cur.trim().to_string());
                    cur.clear();
                }
            }
            _ => cur.push(ch),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

fn extract_generics_after_name(s: &str, name_end: usize) -> (Option<String>, usize) {
    // s is string starting at name pos? Actually we pass trimmed after fn.
    // Find '<' after name
    let after = &s[name_end..];
    let after_trim = after.trim_start();
    if after_trim.starts_with('<') {
        // find matching '>'
        let start_rel = s.len() - after_trim.len(); // absolute offset in s where '<' begins
        let mut depth = 0i32;
        let mut end_opt: Option<usize> = None;
        for (idx, ch) in s[start_rel..].char_indices() {
            if ch == '<' {
                depth += 1;
            } else if ch == '>' {
                depth -= 1;
                if depth == 0 {
                    end_opt = Some(start_rel + idx);
                    break;
                }
            }
        }
        if let Some(end) = end_opt {
            let inner = s[start_rel + 1..end].to_string();
            return (Some(inner), end + 1);
        }
    }
    (None, name_end)
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

fn parse_rust_signature(sig: &str) -> (String, Vec<String>, String, Vec<String>, Vec<Parameter>, Option<String>, Vec<String>) {
    // Returns (visibility, modifiers, name, type_params, params, ret, constraints)
    // sig is trimmed text from start line to open brace exclusive, e.g. "pub(crate) unsafe fn foo<T>(x: i32) -> i32 where T: Clone"
    // Find fn keyword
    let fn_pos = if let Some(p) = sig.find("fn ") { p } else if let Some(p) = sig.find("fn\t") { p } else { return ("private".to_string(), vec![], "unknown".to_string(), vec![], vec![], None, vec![]) };
    let prefix = sig[..fn_pos].trim().to_string();
    let (visibility, mut modifiers) = parse_visibility_and_modifiers(&prefix);

    let after_fn = sig[fn_pos + 2..].trim_start().to_string(); // after "fn"
    // after_fn starts with name
    // Extract name
    let mut name_end = 0usize;
    for (idx, ch) in after_fn.char_indices() {
        if ch.is_alphanumeric() || ch == '_' {
            name_end = idx + ch.len_utf8();
        } else {
            break;
        }
    }
    let name = if name_end > 0 { after_fn[..name_end].to_string() } else { "unknown".to_string() };
    let remainder_after_name = if name_end < after_fn.len() { &after_fn[name_end..] } else { "" };

    // Generics
    let mut generics_inner_opt: Option<String> = None;
    let mut after_generics_offset = 0usize; // offset in remainder_after_name where generics end, or 0 if none
    if remainder_after_name.trim_start().starts_with('<') {
        // find matching >
        let trimmed = remainder_after_name.trim_start();
        let start_in_remainder = remainder_after_name.len() - trimmed.len();
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
            generics_inner_opt = Some(inner);
            after_generics_offset = end + 1;
        }
    }

    // Now need to locate params '(' after generics or name
    let params_search_slice = if generics_inner_opt.is_some() {
        &remainder_after_name[after_generics_offset..]
    } else {
        remainder_after_name
    };
    let paren_start_rel = params_search_slice.find('(');
    let (params_str, after_params_str) = if let Some(rel) = paren_start_rel {
        let absolute_start = if generics_inner_opt.is_some() {
            after_generics_offset + rel
        } else {
            // need to map rel relative to remainder_after_name
            rel
        };
        // But our slice basis differs: if no generics, params_search_slice == remainder_after_name so rel is absolute in remainder_after_name
        // If generics, params_search_slice is remainder_after_name[after_generics_offset..] so absolute = after_generics_offset + rel
        let start_in_remainder = absolute_start;
        // Now find matching ')'
        let search_in = &remainder_after_name[start_in_remainder..];
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

    let parameters = parse_rust_params(&params_str);

    // Return type: look for "->" in after_params_str
    let mut return_type: Option<String> = None;
    let mut where_clause: Option<String> = None;
    let mut effects_placeholder: Vec<String> = Vec::new();

    let after_trim = after_params_str.trim();
    let mut ret_search = after_trim.to_string();
    // Where clause detection
    // If contains "where", split
    let where_pos = ret_search.find("where");
    let (ret_part, where_part) = if let Some(wp) = where_pos {
        let before = ret_search[..wp].trim().to_string();
        let after = ret_search[wp + 5..].trim().to_string(); // 5 = len "where"
        (before, Some(after))
    } else {
        (ret_search.clone(), None)
    };

    if ret_part.trim_start().starts_with("->") {
        let after_arrow = ret_part.trim_start()[2..].trim().to_string();
        if !after_arrow.is_empty() {
            return_type = Some(after_arrow);
        }
    }

    if let Some(wp) = where_part {
        where_clause = Some(wp);
    }

    let constraints = if let Some(wc) = where_clause {
        // split by ',' respecting nested < >? simple split
        split_where_constraints(&wc)
    } else {
        Vec::new()
    };

    let type_params = if let Some(inner) = generics_inner_opt {
        if inner.trim().is_empty() {
            Vec::new()
        } else {
            split_params_respecting_angle(&inner)
        }
    } else {
        Vec::new()
    };

    // adjust modifiers for async detection already done; ensure is_async captured
    // Note: caller will also check modifiers contains async

    (visibility, modifiers, name, type_params, parameters, return_type, constraints)
}

fn split_where_constraints(s: &str) -> Vec<String> {
    // split by ',' respecting brackets
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth_angle: i32 = 0;
    let mut depth_paren: i32 = 0;
    let mut in_string = false;
    let mut escape = false;
    for ch in s.chars() {
        if escape {
            cur.push(ch);
            escape = false;
            continue;
        }
        if ch == '\\' && in_string {
            escape = true;
            cur.push(ch);
            continue;
        }
        if in_string {
            if ch == '"' {
                in_string = false;
            }
            cur.push(ch);
            continue;
        }
        if ch == '"' {
            in_string = true;
            cur.push(ch);
            continue;
        }
        match ch {
            '<' => { depth_angle += 1; cur.push(ch); }
            '>' => { if depth_angle>0 {depth_angle-=1;} cur.push(ch); }
            '(' => { depth_paren+=1; cur.push(ch); }
            ')' => { if depth_paren>0 {depth_paren-=1;} cur.push(ch); }
            ',' if depth_angle==0 && depth_paren==0 => {
                out.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(ch),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out.into_iter().filter(|x| !x.is_empty()).collect()
}

fn split_params_respecting_angle(s: &str) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut depth_angle: i32 = 0;
    let mut depth_paren: i32 = 0;
    let mut depth_bracket: i32 = 0;
    let mut in_string = false;
    let mut escape = false;
    for ch in s.chars() {
        if escape {
            cur.push(ch);
            escape=false;
            continue;
        }
        if ch == '\\' && in_string {
            escape=true;
            cur.push(ch);
            continue;
        }
        if in_string {
            if ch == '"' { in_string=false; }
            cur.push(ch);
            continue;
        }
        if ch == '"' {
            in_string=true;
            cur.push(ch);
            continue;
        }
        match ch {
            '<' => {depth_angle+=1; cur.push(ch);}
            '>' => {depth_angle-=1; cur.push(ch);}
            '(' => {depth_paren+=1; cur.push(ch);}
            ')' => {depth_paren-=1; cur.push(ch);}
            '[' => {depth_bracket+=1; cur.push(ch);}
            ']' => {depth_bracket-=1; cur.push(ch);}
            ',' if depth_angle==0 && depth_paren==0 && depth_bracket==0 => {
                parts.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(ch),
        }
    }
    if !cur.trim().is_empty() {
        parts.push(cur.trim().to_string());
    }
    parts.into_iter().filter(|p| !p.is_empty()).collect()
}

fn parse_rust_params(s: &str) -> Vec<Parameter> {
    let t = s.trim();
    if t.is_empty() {
        return vec![];
    }
    let parts = split_rust_params_respecting(t);
    let mut out = Vec::new();
    for part in parts {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        // self variants
        if p == "self" || p == "mut self" || p == "&self" || p == "&mut self" || p == "Box<Self>" || p == "Arc<Self>" {
            // treat as self
            out.push(Parameter { name: "self".to_string(), typ: p.to_string() });
            continue;
        }
        if p == "&self" || p.contains("self") && !p.contains(':') {
            // self with lifetime like "'a self" or "&'a self" etc
            // Determine name self
            out.push(Parameter { name: "self".to_string(), typ: p.to_string() });
            continue;
        }
        // handle param like "mut x: i32" or "x: Vec<T>" or "_: i32"
        if let Some(colon_idx) = find_colon_outside_angle(p) {
            let before = p[..colon_idx].trim();
            let typ = p[colon_idx+1..].trim().to_string();
            // before may be "mut x" or "ref x" or "x"
            let mut name_tokens: Vec<&str> = before.split_whitespace().collect();
            let name = if let Some(last) = name_tokens.last() {
                let mut n = last.to_string();
                // remove leading '&' or '*' if present
                n = n.trim_start_matches(|c| c=='&' || c=='*').to_string();
                // handle mut prefix already removed via last token
                // If name is "_" keep
                if n.is_empty() {
                    "_".to_string()
                } else {
                    n
                }
            } else {
                "_".to_string()
            };
            // If name is still like "mut", fallback
            let final_name = if name == "mut" || name == "ref" { "_".to_string() } else { name };
            let typ_clean = if typ.is_empty() { "Any".to_string() } else { typ };
            out.push(Parameter { name: final_name, typ: typ_clean });
        } else {
            // No colon, might be "self" already handled, or type-only? For closures not relevant.
            // Treat as name with unknown type
            let mut typ = "Any".to_string();
            let name = p.trim().to_string();
            // If p contains spaces like "mut x" without colon (should not), try to take last token
            if p.contains(' ') {
                let tokens: Vec<&str> = p.split_whitespace().collect();
                if let Some(last) = tokens.last() {
                    out.push(Parameter { name: last.to_string(), typ: tokens[..tokens.len()-1].join(" ") });
                    continue;
                }
            }
            out.push(Parameter { name, typ });
        }
    }
    out
}

fn find_colon_outside_angle(s: &str) -> Option<usize> {
    let mut depth_angle: i32 = 0;
    let mut in_string = false;
    let mut escape = false;
    for (idx, ch) in s.char_indices() {
        if escape {
            escape = false;
            continue;
        }
        if ch == '\\' && in_string {
            escape = true;
            continue;
        }
        if in_string {
            if ch == '"' {
                in_string = false;
            }
            continue;
        }
        if ch == '"' {
            in_string = true;
            continue;
        }
        match ch {
            '<' => depth_angle += 1,
            '>' => if depth_angle>0 { depth_angle-=1; },
            ':' if depth_angle==0 => return Some(idx),
            _ => {}
        }
    }
    None
}

fn split_rust_params_respecting(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut depth_paren: i32 = 0;
    let mut depth_angle: i32 = 0;
    let mut depth_bracket: i32 = 0;
    let mut depth_brace: i32 = 0;
    let mut in_string = false;
    let mut in_char = false;
    let mut escape = false;
    let mut in_raw: Option<usize> = None;

    // For simplicity inside params we don't expect raw strings, but handle generically
    let chars: Vec<char> = s.chars().collect();
    let mut idx = 0usize;
    while idx < chars.len() {
        let ch = chars[idx];
        if escape {
            cur.push(ch);
            escape = false;
            idx += 1;
            continue;
        }
        if ch == '\\' && (in_string || in_char) {
            escape = true;
            cur.push(ch);
            idx += 1;
            continue;
        }
        if in_string {
            if ch == '"' {
                in_string = false;
            }
            cur.push(ch);
            idx += 1;
            continue;
        }
        if in_char {
            if ch == '\'' {
                in_char = false;
            }
            cur.push(ch);
            idx += 1;
            continue;
        }
        match ch {
            '"' => { in_string=true; cur.push(ch); }
            '\'' => { in_char=true; cur.push(ch); }
            '(' => { depth_paren+=1; cur.push(ch); }
            ')' => { depth_paren-=1; cur.push(ch); }
            '<' => { depth_angle+=1; cur.push(ch); }
            '>' => { if depth_angle>0 {depth_angle-=1;} cur.push(ch); }
            '[' => { depth_bracket+=1; cur.push(ch); }
            ']' => { depth_bracket-=1; cur.push(ch); }
            '{' => { depth_brace+=1; cur.push(ch); }
            '}' => { depth_brace-=1; cur.push(ch); }
            ',' if depth_paren==0 && depth_angle==0 && depth_bracket==0 && depth_brace==0 => {
                parts.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(ch),
        }
        idx += 1;
    }
    if !cur.trim().is_empty() {
        parts.push(cur.trim().to_string());
    }
    parts
}

// ---------- Main parsing orchestration ----------

#[derive(Debug, Clone)]
struct ModInfo {
    name: String,
    open: usize,
    close: usize,
}
#[derive(Debug, Clone)]
struct ImplInfo {
    name: String,
    open: usize,
    close: usize,
}

fn collect_mods_and_impls(content: &str) -> (Vec<ModInfo>, Vec<ImplInfo>) {
    let mut mods = Vec::new();
    let mut impls = Vec::new();

    // Build masked lines for keyword detection? Use direct scanning with raw awareness to find keywords
    // Simple approach: iterate through content bytes with state and detect `mod`/`impl` at word boundaries
    // But easier: use line iteration with masked checks

    let lines: Vec<&str> = content.lines().collect();
    let mut line_starts: Vec<usize> = vec![0];
    for (i, b) in content.bytes().enumerate() {
        if b == b'\n' {
            line_starts.push(i + 1);
        }
    }

    // Helper to check if keyword at position is outside string/comments using find_open scanning trick?
    // Instead we will mask content first to find keyword positions safely by reusing find logic that respects strings/comments
    // Simpler: we will scan bytes sequential to find keywords outside contexts

    let bytes = content.as_bytes();
    let mut i = 0usize;
    let mut in_line_comment = false;
    let mut in_block_comment: i32 = 0;
    let mut in_string = false;
    let mut in_char = false;
    let mut in_backtick = false;
    let mut in_raw: Option<usize> = None;
    let mut escape = false;

    while i < bytes.len() {
        let b = bytes[i];
        let next = if i+1 < bytes.len() { Some(bytes[i+1]) } else { None };

        if let Some(h) = in_raw {
            if b == b'"' {
                let mut ok = true;
                for hh in 0..h {
                    if i+1+hh >= bytes.len() || bytes[i+1+hh] != b'#' { ok = false; break; }
                }
                if ok {
                    i += 1 + h;
                    in_raw = None;
                    continue;
                }
            }
            i+=1;
            continue;
        }
        if in_line_comment {
            if b == b'\n' { in_line_comment=false; }
            i+=1; continue;
        }
        if in_block_comment>0 {
            if b==b'/' && next==Some(b'*') { in_block_comment+=1; i+=2; continue; }
            if b==b'*' && next==Some(b'/') { in_block_comment-=1; i+=2; continue; }
            i+=1; continue;
        }
        if in_string {
            if escape { escape=false; } else if b==b'\\' { escape=true; } else if b==b'"' { in_string=false; }
            i+=1; continue;
        }
        if in_char {
            if escape { escape=false; } else if b==b'\\' { escape=true; } else if b==b'\'' { in_char=false; }
            i+=1; continue;
        }
        if in_backtick {
            if b==b'`' { in_backtick=false; }
            i+=1; continue;
        }

        if b==b'r' {
            if let Some(h) = raw_string_hashes(bytes, i) {
                i += 1 + h + 1;
                in_raw = Some(h);
                continue;
            }
        }
        if b==b'/' && next==Some(b'/') { in_line_comment=true; i+=2; continue; }
        if b==b'/' && next==Some(b'*') { in_block_comment=1; i+=2; continue; }
        if b==b'"' { in_string=true; i+=1; continue; }
        if b==b'\'' && is_char_literal(bytes, i) { in_char=true; i+=1; continue; }
        if b==b'`' { in_backtick=true; i+=1; continue; }

        // Check for keyword `mod` or `impl` at this position with word boundaries
        // Ensure previous char is not alphanumeric/_
        let prev_is_ident = if i>0 { (bytes[i-1].is_ascii_alphanumeric() || bytes[i-1]==b'_') } else { false };
        if !prev_is_ident {
            // check mod
            if i+3 <= bytes.len() && &bytes[i..i+3] == b"mod" {
                let after = if i+3 < bytes.len() { bytes[i+3] } else { b' ' };
                if !(after.is_ascii_alphanumeric() || after==b'_') {
                    // found mod keyword, extract name
                    let mut j = i+3;
                    while j < bytes.len() && (bytes[j]==b' ' || bytes[j]==b'\t' || bytes[j]==b'\n' || bytes[j]==b'\r') { j+=1; }
                    let name_start = j;
                    while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j]==b'_') { j+=1; }
                    if j > name_start {
                        let name = String::from_utf8_lossy(&bytes[name_start..j]).to_string();
                        // Look for opening brace after name, skipping whitespace/comments? For now scan for '{' via find_open_brace
                        if let Some(open_pos) = find_open_brace_rust(content, j) {
                            // check that before open there's no ';' (mod foo;)
                            let between = &content[j..open_pos];
                            if !between.contains(';') {
                                if let Some(close_pos) = find_matching_brace_rust(content, open_pos) {
                                    mods.push(ModInfo { name, open: open_pos, close: close_pos });
                                }
                            }
                        }
                    }
                }
            } else if i+4 <= bytes.len() && &bytes[i..i+4] == b"impl" {
                let after = if i+4 < bytes.len() { bytes[i+4] } else { b' ' };
                if !(after.is_ascii_alphanumeric() || after==b'_') {
                    // found impl
                    // Need to parse impl name: from i+4 to open brace
                    if let Some(open_pos) = find_open_brace_rust(content, i+4) {
                        let sig_slice = &content[i..open_pos];
                        if let Some(impl_name) = parse_impl_name(sig_slice) {
                            if let Some(close_pos) = find_matching_brace_rust(content, open_pos) {
                                impls.push(ImplInfo { name: impl_name, open: open_pos, close: close_pos });
                            }
                        }
                    }
                }
            }
        }

        i+=1;
    }

    // Deduplicate/sort? Keep as found
    mods.sort_by_key(|m| m.open);
    impls.sort_by_key(|m| m.open);
    (mods, impls)
}

fn parse_impl_name(sig: &str) -> Option<String> {
    // sig is like "impl<T> Foo<T> where T: Clone " or "impl Foo for Bar"
    let mut s = sig.trim();
    // remove leading impl
    if let Some(pos) = s.find("impl") {
        s = s[pos+4..].trim();
    } else {
        return None;
    }
    // handle generics after impl: skip <...>
    if s.starts_with('<') {
        let mut depth = 0i32;
        let mut end: Option<usize> = None;
        for (idx, ch) in s.char_indices() {
            if ch == '<' { depth+=1; }
            else if ch == '>' { depth-=1; if depth==0 { end=Some(idx); break; } }
        }
        if let Some(e) = end {
            s = s[e+1..].trim();
        }
    }
    if s.is_empty() { return None; }
    // If contains " for ", take after last " for "
    if let Some(for_pos) = s.rfind(" for ") {
        let after = s[for_pos+5..].trim();
        // after may be "MyStruct<T> where ..."
        // take first identifier
        let mut name = String::new();
        for ch in after.chars() {
            if ch.is_alphanumeric() || ch == '_' {
                name.push(ch);
            } else {
                break;
            }
        }
        if !name.is_empty() { return Some(name); }
        return None;
    } else {
        // first type name
        let mut name = String::new();
        for ch in s.chars() {
            if ch.is_alphanumeric() || ch == '_' {
                name.push(ch);
            } else {
                break;
            }
        }
        if !name.is_empty() { return Some(name); }
        return None;
    }
}

fn parse_rust_functions(content: &str) -> Result<Vec<RustFunctionRaw>> {
    let mut line_starts: Vec<usize> = vec![0];
    for (i, b) in content.bytes().enumerate() {
        if b == b'\n' {
            line_starts.push(i + 1);
        }
    }
    let (mods, impls) = collect_mods_and_impls(content);

    let mut result = Vec::new();
    let bytes = content.as_bytes();
    let mut idx = 0usize;

    // State scanning for fn keyword similar to previous but we use line iteration for simplicity
    // Instead reuse byte scanning approach to find `fn` outside comments/strings

    let mut i: usize = 0;
    let mut in_line_comment = false;
    let mut in_block_comment: i32 = 0;
    let mut in_string = false;
    let mut in_char = false;
    let mut in_backtick = false;
    let mut in_raw: Option<usize> = None;
    let mut escape = false;

    while i < bytes.len() {
        let b = bytes[i];
        let next = if i+1 < bytes.len() { Some(bytes[i+1]) } else { None };

        if let Some(h) = in_raw {
            if b == b'"' {
                let mut ok = true;
                for hh in 0..h {
                    if i+1+hh >= bytes.len() || bytes[i+1+hh] != b'#' { ok = false; break; }
                }
                if ok { i += 1 + h; in_raw = None; continue; }
            }
            i+=1; continue;
        }
        if in_line_comment { if b==b'\n' { in_line_comment=false; } i+=1; continue; }
        if in_block_comment>0 {
            if b==b'/' && next==Some(b'*') { in_block_comment+=1; i+=2; continue; }
            if b==b'*' && next==Some(b'/') { in_block_comment-=1; i+=2; continue; }
            i+=1; continue;
        }
        if in_string { if escape {escape=false;} else if b==b'\\' {escape=true;} else if b==b'"' {in_string=false;} i+=1; continue; }
        if in_char { if escape {escape=false;} else if b==b'\\' {escape=true;} else if b==b'\'' {in_char=false;} i+=1; continue; }
        if in_backtick { if b==b'`' {in_backtick=false;} i+=1; continue; }

        if b==b'r' {
            if let Some(h) = raw_string_hashes(bytes, i) { i+=1+h+1; in_raw=Some(h); continue; }
        }
        if b==b'/' && next==Some(b'/') { in_line_comment=true; i+=2; continue; }
        if b==b'/' && next==Some(b'*') { in_block_comment=1; i+=2; continue; }
        if b==b'"' { in_string=true; i+=1; continue; }
        if b==b'\'' && is_char_literal(bytes, i) { in_char=true; i+=1; continue; }
        if b==b'`' { in_backtick=true; i+=1; continue; }

        // check for fn
        let prev_is_ident = if i>0 { bytes[i-1].is_ascii_alphanumeric() || bytes[i-1]==b'_' } else { false };
        if !prev_is_ident && i+2 <= bytes.len() && &bytes[i..i+2]==b"fn" {
            let after = if i+2 < bytes.len() { bytes[i+2] } else { b' ' };
            if !(after.is_ascii_alphanumeric() || after==b'_') {
                // candidate fn, need to verify it's a def: find prefix line start
                // Find start of declaration: look backwards to line start or to previous ';'/'{'/'}' ?
                // For start_byte we want the first non-space of the line containing fn, plus handle attributes? Simplify: line start
                // Find line start for i
                let mut line_idx = 0usize;
                for (idx, &start) in line_starts.iter().enumerate() {
                    if start <= i { line_idx = idx; } else { break; }
                }
                let line_start_byte = line_starts[line_idx];
                let line_text = &content[line_start_byte..i+2]; // up to fn
                // Determine start_byte: first non-space of this line
                let line_full = if line_idx < line_starts.len()-1 {
                    // get whole line without newline for first_non_space
                    let end = if line_idx+1 < line_starts.len() { line_starts[line_idx+1]-1 } else { content.len() };
                    &content[line_start_byte..end.min(content.len())]
                } else {
                    &content[line_start_byte..]
                };
                let col_offset = first_non_space_col(line_full);
                let start_byte = line_start_byte + col_offset;
                // Find open brace after i
                if let Some(open_pos) = find_open_brace_rust(content, i) {
                    // Ensure no ';' before open: if there's a ';' between i and open, it's a trait decl, skip
                    let between = &content[i..open_pos];
                    if between.contains(';') {
                        i += 2;
                        continue;
                    }
                    if let Some(close_pos) = find_matching_brace_rust(content, open_pos) {
                        let (start_line, start_col) = byte_to_line_col(content, &line_starts, start_byte);
                        let (end_line, end_col) = byte_to_line_col(content, &line_starts, close_pos);
                        let source_text = content[start_byte..=close_pos].to_string();
                        // Signature is from start_byte to open_pos exclusive
                        let sig_text = content[start_byte..open_pos].trim().to_string();
                        let (visibility, modifiers_raw, name, type_params, params, ret, constraints) = parse_rust_signature(&sig_text);
                        if name == "unknown" || name.is_empty() {
                            i = close_pos + 1;
                            continue;
                        }
                        // Determine if async
                        let is_async = modifiers_raw.contains(&"async".to_string());
                        // Build modifiers vector includes async/const/unsafe etc already, but ensure visibility not duplicated
                        let mut modifiers = modifiers_raw.clone();
                        // Enclosing context
                        let mut enclosing_mod: Option<String> = None;
                        let mut innermost_mod_open = 0usize;
                        for m in &mods {
                            if m.open < start_byte && close_pos < m.close {
                                // nested mods: pick innermost that encloses, but also need path of all enclosing mods
                                // For path we need to collect all that enclose in order
                            }
                        }
                        // Build mod path as :: joined of all enclosing mods sorted
                        let mut enclosing_mods: Vec<&ModInfo> = mods.iter().filter(|m| m.open < start_byte && close_pos < m.close).collect();
                        enclosing_mods.sort_by_key(|m| m.open);
                        let mod_path = if enclosing_mods.is_empty() { None } else { Some(enclosing_mods.iter().map(|m| m.name.clone()).collect::<Vec<_>>().join("::")) };

                        let mut struct_ctx: Option<String> = None;
                        let mut innermost_impl: Option<&ImplInfo> = None;
                        for imp in &impls {
                            if imp.open < start_byte && close_pos < imp.close {
                                if innermost_impl.map(|prev| imp.open > prev.open).unwrap_or(true) {
                                    innermost_impl = Some(imp);
                                }
                            }
                        }
                        if let Some(imp) = innermost_impl {
                            struct_ctx = Some(imp.name.clone());
                        }

                        result.push(RustFunctionRaw {
                            name,
                            return_type: ret,
                            visibility,
                            modifiers,
                            parameters: params,
                            type_params,
                            constraints,
                            is_async,
                            struct_context: struct_ctx,
                            mod_path,
                            start_line,
                            start_col,
                            start_byte,
                            end_line,
                            end_col,
                            end_byte: close_pos,
                            source_text,
                        });
                        // Move i after close to avoid nested detection inside function body
                        // But need to resume scanning after close, keeping comment/string state? Our state currently at i before open, we jump.
                        // Reset state after jump: we need to recompute state from close+1 as raw scanning would have consumed string/comments inside body correctly via find_matching.
                        // So just set i = close_pos+1 and reset flags (they should be neutral after balanced braces)
                        // Need to reinitialize scanning state after jump: since find_matching already consumed strings correctly, we can reset to neutral.
                        in_line_comment = false;
                        in_block_comment = 0;
                        in_string = false;
                        in_char = false;
                        in_backtick = false;
                        in_raw = None;
                        escape = false;
                        i = close_pos + 1;
                        continue;
                    }
                }
            }
        }

        i+=1;
    }

    // Deduplicate? Ensure stable order by start_byte
    result.sort_by_key(|r| r.start_byte);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const SAMPLE_SIMPLE: &str = r#"pub fn hello(name: String) -> String {
    format!("hello {}", name)
}

fn private_add(a: i32, b: i32) -> i32 {
    a + b
}

pub(crate) async fn fetch(url: &str) -> Result<String, Error> {
    // comment with { }
    let s = "string with { brace }";
    Ok(s.to_string())
}
"#;

    const SAMPLE_IMPL_MOD: &str = r#"mod inner {
    pub struct MyStruct;

    impl MyStruct {
        pub fn method(&self, x: i32) -> i32 {
            x + 1
        }

        async fn async_method(&mut self) {
        }
    }

    pub mod nested {
        pub fn nested_fn() {}
    }
}

const fn const_fn() -> i32 { 42 }

unsafe fn unsafe_fn() {}

fn generic_fn<T: Clone, U>(a: T, b: U) -> T where T: Clone {
    a
}
"#;

    const SAMPLE_COMPLEX: &str = r###"// line comment with fn fake() { }
    /* block comment with fn fake2() { } */
    fn real() {
        let raw = r#"raw string with { and } and fn fake() "#;
        let raw2 = r##"raw ## with " inside"##;
        let s = "string with { } and // comment";
        let c = '{';
        // comment brace {
    }

    pub fn with_where<T>(x: T) -> T where T: Debug + Clone {
        x
    }

    mod mymod {
        pub fn mod_fn() {}
    }
"###;

    #[test]
    fn extracts_simple_functions() {
        let fns = extract(SAMPLE_SIMPLE, Path::new("sample.rs")).unwrap();
        assert_eq!(fns.len(), 3, "found {:?}", fns.iter().map(|f| &f.identity.name).collect::<Vec<_>>());
        let hello = fns.iter().find(|f| f.identity.name == "hello").unwrap();
        assert_eq!(hello.identity.qualified_name, "crate::hello");
        assert_eq!(hello.signature.parameters.len(), 1);
        assert_eq!(hello.signature.return_type.as_deref(), Some("String"));
        assert_eq!(hello.declaration.visibility, "public");
        assert_eq!(hello.identity.language, "rust");
        assert_eq!(hello.metadata.parser.as_deref(), Some("reko-rustExtractor"));
        assert!(hello.source.hash.starts_with("sha256:"));
        assert!(hello.source.location.start.byte < hello.source.location.end.byte);
        assert_eq!(hello.source.location.start.line, 1);

        let priv_add = fns.iter().find(|f| f.identity.name == "private_add").unwrap();
        assert_eq!(priv_add.identity.qualified_name, "crate::private_add");
        assert_eq!(priv_add.declaration.visibility, "private");
        assert_eq!(priv_add.signature.return_type.as_deref(), Some("i32"));

        let fetch = fns.iter().find(|f| f.identity.name == "fetch").unwrap();
        assert!(fetch.execution.is_async);
        assert!(fetch.declaration.modifiers.contains(&"async".to_string()));
        assert_eq!(fetch.identity.qualified_name, "crate::fetch");
        assert!(fetch.source.source_text.contains("string with { brace }"));
    }

    #[test]
    fn extracts_impl_and_mod_context() {
        let fns = extract(SAMPLE_IMPL_MOD, Path::new("lib.rs")).unwrap();
        // expected: method, async_method, nested_fn, const_fn, unsafe_fn, generic_fn => 6
        assert_eq!(fns.len(), 6, "found {:?}", fns.iter().map(|f| format!("{} {}", f.identity.qualified_name, f.identity.name)).collect::<Vec<_>>());

        let method = fns.iter().find(|f| f.identity.name == "method").unwrap();
        assert_eq!(method.identity.qualified_name, "crate::inner::MyStruct::method");
        assert_eq!(method.context.struct_.as_deref(), Some("MyStruct"));
        assert_eq!(method.signature.parameters.len(), 2); // &self, x
        assert_eq!(method.signature.return_type.as_deref(), Some("i32"));

        let async_m = fns.iter().find(|f| f.identity.name == "async_method").unwrap();
        assert!(async_m.execution.is_async);
        assert_eq!(async_m.identity.qualified_name, "crate::inner::MyStruct::async_method");
        assert_eq!(async_m.context.struct_.as_deref(), Some("MyStruct"));

        let nested = fns.iter().find(|f| f.identity.name == "nested_fn").unwrap();
        assert_eq!(nested.identity.qualified_name, "crate::inner::nested::nested_fn");

        let constf = fns.iter().find(|f| f.identity.name == "const_fn").unwrap();
        assert!(constf.declaration.modifiers.contains(&"const".to_string()));
        assert_eq!(constf.identity.qualified_name, "crate::const_fn");

        let unsafe_f = fns.iter().find(|f| f.identity.name == "unsafe_fn").unwrap();
        assert!(unsafe_f.declaration.modifiers.contains(&"unsafe".to_string()));

        let gen = fns.iter().find(|f| f.identity.name == "generic_fn").unwrap();
        assert_eq!(gen.signature.type_parameters.len(), 2);
        assert!(gen.signature.type_parameters.contains(&"T: Clone".to_string()));
        assert!(gen.signature.constraints.contains(&"T: Clone".to_string()));
        assert_eq!(gen.signature.return_type.as_deref(), Some("T"));
    }

    #[test]
    fn handles_comments_strings_raw_and_where() {
        let fns = extract(SAMPLE_COMPLEX, Path::new("complex.rs")).unwrap();
        assert_eq!(fns.len(), 3, "found {:?}", fns.iter().map(|f| &f.identity.name).collect::<Vec<_>>());
        let real = fns.iter().find(|f| f.identity.name == "real").unwrap();
        assert_eq!(real.identity.qualified_name, "crate::real");
        assert!(real.source.source_text.contains(r#"raw string with { and } "#));
        assert!(real.source.source_text.contains("string with { }"));
        // ensure braces in comments/strings didn't truncate
        assert!(real.source.location.start.byte < real.source.location.end.byte);

        let with_where = fns.iter().find(|f| f.identity.name == "with_where").unwrap();
        assert_eq!(with_where.signature.type_parameters, vec!["T"]);
        assert!(!with_where.signature.constraints.is_empty());
        assert!(with_where.signature.constraints[0].contains("Debug"));

        let modfn = fns.iter().find(|f| f.identity.name == "mod_fn").unwrap();
        assert_eq!(modfn.identity.qualified_name, "crate::mymod::mod_fn");
        assert_eq!(modfn.context.namespace.as_deref(), Some("crate::mymod"));
    }

    #[test]
    fn extract_to_json_valid() {
        let json = extract_to_json(SAMPLE_SIMPLE, Path::new("sample.rs")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(v.is_array());
        assert_eq!(v.as_array().unwrap().len(), 3);
        let first = &v.as_array().unwrap()[0];
        assert_eq!(first["identity"]["language"], "rust");
        assert_eq!(first["metadata"]["parser"], "reko-rustExtractor");
    }

    #[test]
    fn qualified_without_mod_or_impl() {
        let content = r#"fn foo() {}"#;
        let fns = extract(content, Path::new("mymod.rs")).unwrap();
        assert_eq!(fns.len(), 1);
        assert_eq!(fns[0].identity.qualified_name, "crate::foo");
        assert_eq!(fns[0].source.module, "mymod");
    }
}
