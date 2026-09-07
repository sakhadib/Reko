use crate::ir::{IrFunction, Parameter, Position};
use anyhow::Result;
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Debug, Clone)]
struct CsharpMethodRaw {
    name: String,
    return_type: Option<String>,
    visibility: String,
    modifiers: Vec<String>,
    type_params: Vec<String>,
    parameters: Vec<Parameter>,
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

/// Public entry: given file content and file path, extract IR for each C# method.
pub fn extract(content: &str, file_path: &Path) -> Result<Vec<IrFunction>> {
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
        let qualified = match (&r.namespace, &r.class) {
            (Some(ns), Some(cls)) => format!("{}::{}::{}", ns, cls, r.name),
            (Some(ns), None) => format!("{}::{}", ns, r.name),
            (None, Some(cls)) => format!("{}::{}", cls, r.name),
            (None, None) => r.name.clone(),
        };
        let id = qualified.clone();
        // return_type already Option
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
            r.namespace.clone(),
            r.class.clone(),
        );
        // patch language/parser from java defaults
        ir.identity.language = "csharp".to_string();
        ir.metadata.parser = Some("reko-csharpExtractor".to_string());
        ir.metadata.parser_version = Some("0.1.0".to_string());
        // signature type_params
        ir.signature.type_parameters = r.type_params.clone();
        ir.types.generic = r.type_params.clone();
        // context patch
        ir.context.namespace = r.namespace.clone();
        ir.context.module = Some(module.clone());
        ir.context.package = r.namespace.clone();
        ir.context.class = r.class.clone();
        ir.source.module = module.clone();
        // execution async
        if r.modifiers.contains(&"async".to_string()) {
            ir.execution.is_async = true;
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

fn find_matching_brace(content: &str, open_pos: usize) -> Option<usize> {
    let bytes = content.as_bytes();
    let mut depth = 0i32;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let mut in_string = false;
    let mut in_char = false;
    let mut in_verbatim = false;
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
        if in_verbatim {
            if b == b'"' {
                if next == Some(b'"') {
                    // escaped "" inside verbatim, skip next
                    // Need to advance extra one char; emulate by ignoring next quote
                    // We handle by checking next char and skipping
                    continue;
                } else {
                    in_verbatim = false;
                }
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

        if b == b'/' && next == Some(b'/') {
            in_line_comment = true;
            continue;
        }
        if b == b'/' && next == Some(b'*') {
            in_block_comment = true;
            continue;
        }
        if b == b'@' && next == Some(b'"') {
            in_verbatim = true;
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

fn find_open_brace(
    content: &str,
    line_starts: &[usize],
    start_idx: usize,
) -> Result<(usize, usize, usize)> {
    let lines: Vec<&str> = content.lines().collect();
    for idx in start_idx..lines.len() {
        if let Some(col) = lines[idx].find('{') {
            let byte = line_starts[idx] + col;
            return Ok((idx + 1, col + 1, byte));
        }
        if lines[idx].contains(';') {
            // check if ';' appears before any '{' in this scan window -> prototype
            // We should stop only if ';' appears and no '{' on same line
            // But also expression-bodied => ; is expected, but we treat as not brace
            anyhow::bail!("no open brace found")
        }
        if idx > start_idx + 4 {
            break;
        }
    }
    anyhow::bail!("no open brace found for method starting at line {}", start_idx + 1)
}

fn find_open_brace_after(content: &str, from: usize) -> Option<usize> {
    let bytes = content.as_bytes();
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let mut in_string = false;
    let mut in_char = false;
    let mut in_verbatim = false;
    let mut escape = false;
    let mut i = from;
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
        if in_verbatim {
            if b == b'"' {
                if next == Some(b'"') {
                    i += 2;
                    continue;
                } else {
                    in_verbatim = false;
                }
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
        if b == b'@' && next == Some(b'"') {
            in_verbatim = true;
            i += 2;
            continue;
        }
        if b == b'"' {
            in_string = true;
            i += 1;
            continue;
        }
        if b == b'\'' {
            in_char = true;
            i += 1;
            continue;
        }
        if b == b'{' {
            return Some(i);
        }
        if b == b';' {
            // prototype terminator before brace -> no brace
            return None;
        }
        i += 1;
        // limit scan window to avoid scanning whole file: stop if we passed too far without brace and encountered a line with just '}'? keep scanning up to 2000 chars
        if i > from + 2000 {
            break;
        }
    }
    None
}

fn find_paren_end_idx(lines: &[&str], start_idx: usize) -> Option<usize> {
    let mut depth: i32 = 0;
    let mut started = false;
    let max = std::cmp::min(start_idx + 16, lines.len());
    for k in start_idx..max {
        for ch in lines[k].chars() {
            if ch == '(' {
                depth += 1;
                started = true;
            } else if ch == ')' {
                depth -= 1;
                if started && depth == 0 {
                    return Some(k);
                }
            }
        }
        if started && depth == 0 {
            return Some(k);
        }
        if started && depth < 0 {
            return None;
        }
    }
    None
}

fn count_braces(s: &str) -> (usize, usize) {
    let open = s.matches('{').count();
    let close = s.matches('}').count();
    (open, close)
}

fn parse_namespace_name(trimmed: &str) -> Option<String> {
    if !trimmed.starts_with("namespace") {
        return None;
    }
    let after = trimmed["namespace".len()..].trim_start();
    if after.is_empty() || after.starts_with('{') || after.starts_with('=') {
        return None;
    }
    let mut end = after.len();
    for (i, c) in after.char_indices() {
        if c == '{' || c == ';' || c == '=' {
            end = i;
            break;
        }
    }
    let candidate = after[..end].trim();
    if candidate.is_empty() {
        return None;
    }
    let name = candidate
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim()
        .trim_end_matches('{')
        .trim()
        .to_string();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

fn parse_file_scoped_namespace(content: &str) -> Option<String> {
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with("namespace ") && t.ends_with(';') {
            let after = t["namespace".len()..].trim();
            let cand = after.trim_end_matches(';').trim();
            // take up to ';' or whitespace comment?
            let name = cand.split_whitespace().next().unwrap_or("").trim().to_string();
            if !name.is_empty() {
                return Some(name);
            }
        }
    }
    None
}

fn parse_class_name(trimmed: &str) -> Option<(String, String)> {
    // handle class, interface, struct, record, record struct, enum
    // simplify: search for keywords
    let keywords = ["class ", "interface ", "struct ", "record ", "enum "];
    for kw in keywords {
        if let Some(pos) = trimmed.find(kw) {
            // ensure keyword at start or preceded by modifiers
            let after = &trimmed[pos + kw.len()..];
            let after_trim = after.trim_start();
            if after_trim.is_empty() {
                continue;
            }
            let mut name = String::new();
            for c in after_trim.chars() {
                if c.is_alphanumeric() || c == '_' {
                    name.push(c);
                } else {
                    break;
                }
            }
            if name.is_empty() {
                continue;
            }
            let kind = kw.trim().to_string();
            return Some((kind, name));
        }
        if trimmed.starts_with(kw) {
            let after = &trimmed[kw.len()..];
            let after_trim = after.trim_start();
            let mut name = String::new();
            for c in after_trim.chars() {
                if c.is_alphanumeric() || c == '_' {
                    name.push(c);
                } else {
                    break;
                }
            }
            if !name.is_empty() {
                let kind = kw.trim().to_string();
                return Some((kind, name));
            }
        }
    }
    None
}

fn is_control_flow(trimmed: &str) -> bool {
    trimmed.starts_with("if ")
        || trimmed.starts_with("if(")
        || trimmed.starts_with("for ")
        || trimmed.starts_with("for(")
        || trimmed.starts_with("foreach ")
        || trimmed.starts_with("foreach(")
        || trimmed.starts_with("while ")
        || trimmed.starts_with("while(")
        || trimmed.starts_with("switch ")
        || trimmed.starts_with("switch(")
        || trimmed.starts_with("catch ")
        || trimmed.starts_with("catch(")
        || trimmed.starts_with("try ")
        || trimmed.starts_with("try{")
        || trimmed.starts_with("else")
        || trimmed.starts_with("do ")
        || trimmed.starts_with("do{")
        || trimmed.starts_with("return ")
        || trimmed.starts_with("return(")
        || trimmed.starts_with("using(")
        || trimmed.starts_with("using ")
        || trimmed.starts_with("lock ")
        || trimmed.starts_with("lock(")
}

fn parse_csharp_signature(
    sig: &str,
) -> (String, Vec<String>, Option<String>, String, Vec<String>, Vec<Parameter>) {
    // Remove trailing '{' and trim
    let mut core = sig.trim().trim_end_matches('{').trim().to_string();
    // Remove attribute prefix like "[...]" if present on same line
    // Attributes are typically separate lines, but handle inline
    while core.trim_start().starts_with('[') {
        if let Some(end) = core.find(']') {
            core = core[end + 1..].trim().to_string();
        } else {
            break;
        }
    }
    // Find paren start
    let paren_start = core.find('(').unwrap_or(core.len());
    let before_paren = core[..paren_start].trim().to_string();
    let inside_paren = if paren_start < core.len() {
        let paren_end = core.rfind(')').unwrap_or(core.len() - 1);
        if paren_end > paren_start {
            core[paren_start + 1..paren_end].trim().to_string()
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    let tokens: Vec<&str> = before_paren.split_whitespace().collect();
    if tokens.is_empty() {
        return (
            "private".to_string(),
            vec![],
            None,
            "unknown".to_string(),
            vec![],
            vec![],
        );
    }
    // name token is last
    let raw_name_token = tokens.last().unwrap().to_string();
    let mut name = raw_name_token.clone();
    let mut type_params: Vec<String> = Vec::new();
    // Extract generics from name: Foo<T> or Foo<T,U>
    if let Some(lt) = raw_name_token.find('<') {
        if let Some(gt) = raw_name_token.rfind('>') {
            if gt > lt {
                name = raw_name_token[..lt].trim().to_string();
                let inside = &raw_name_token[lt + 1..gt];
                type_params = inside
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
        }
    }
    // Also handle case where generics split across whitespace? e.g., "Foo < T >" unlikely

    let ret_and_mods = if tokens.len() >= 2 {
        &tokens[..tokens.len() - 1]
    } else {
        &[]
    };

    // Visibility handling: may be "protected internal" etc.
    // Collect leading visibility tokens
    let visibility_keywords = ["public", "private", "protected", "internal"];
    let mut vis_parts: Vec<String> = Vec::new();
    let mut idx = 0;
    while idx < ret_and_mods.len() {
        let t = ret_and_mods[idx].to_lowercase();
        if visibility_keywords.contains(&t.as_str()) {
            vis_parts.push(ret_and_mods[idx].to_string());
            idx += 1;
        } else {
            break;
        }
    }
    let visibility = if vis_parts.is_empty() {
        "private".to_string()
    } else {
        vis_parts.join(" ")
    };

    let modifier_keywords = [
        "static", "virtual", "override", "async", "abstract", "sealed", "extern", "new",
        "unsafe", "partial", "readonly", "volatile", "ref", "in", "out",
    ];
    let mut modifiers: Vec<String> = Vec::new();
    let mut type_parts: Vec<String> = Vec::new();
    for tok in &ret_and_mods[idx..] {
        let low = tok.to_lowercase();
        if modifier_keywords.contains(&low.as_str()) {
            modifiers.push(tok.to_string());
        } else {
            type_parts.push(tok.to_string());
        }
    }

    let return_type = if type_parts.is_empty() {
        None
    } else {
        let joined = type_parts.join(" ").trim().to_string();
        if joined.is_empty() {
            None
        } else {
            Some(joined)
        }
    };

    // If return_type is None and method name equals class? Could be constructor but we treat as None (like c++)
    let parameters = parse_csharp_params(&inside_paren);

    // Deduplicate modifiers keep order but lower? Keep as is
    (visibility, modifiers, return_type, name, type_params, parameters)
}

fn parse_csharp_params(s: &str) -> Vec<Parameter> {
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
        // Remove default value
        let before_eq = p.split('=').next().unwrap_or(p).trim();
        // Remove attributes like [FromBody]
        let mut cleaned = before_eq.trim().to_string();
        while cleaned.starts_with('[') {
            if let Some(end) = cleaned.find(']') {
                cleaned = cleaned[end + 1..].trim().to_string();
            } else {
                break;
            }
        }
        let tokens: Vec<&str> = cleaned.split_whitespace().collect();
        if tokens.is_empty() {
            continue;
        }
        if tokens.len() == 1 {
            // single type? Could be like "int" without name? but assume name missing
            out.push(Parameter {
                name: format!("arg{}", out.len()),
                typ: tokens[0].to_string(),
            });
            continue;
        }
        // Last token is name, rest is type (+ modifiers like ref/out/in/params)
        let raw_name = tokens.last().unwrap().to_string();
        let mut name = raw_name.trim_start_matches(|c| c == '@').to_string();
        // Handle params like "params int[] arr" where name is last, type includes "params"
        // Filter leading param modifiers from type part? Keep them in typ? Better keep only type.
        // Determine typ_str from tokens[..len-1]
        let mut typ_tokens = tokens[..tokens.len() - 1].to_vec();
        // Remove leading modifiers like ref, out, in, params, this
        let param_mods = ["ref", "out", "in", "params", "this"];
        // Keep them as part of modifier? For now strip them from typ but could keep
        // If first token is param modifier, we can keep type as remaining
        // For simplicity, if typ_tokens[0] is modifier, we still join all but record? We'll just join remaining without modifier unless it's the only
        // Actually we should keep them out of typ? But keep type accurate: e.g., "ref int" -> typ "int" with modifier? We'll just treat typ as joined without ref/out for simplicity? But include for fidelity.
        // Let's keep typ_tokens as is for now, but if typ_tokens contains param modifier we can keep as part of type? We'll filter to keep type clean: remove leading param modifiers and put into typ as prefix? Simpler keep as is.
        // We'll remove leading param mods from typ_tokens if they are exactly one of them and more than one token left
        while typ_tokens.len() > 1 && param_mods.contains(&typ_tokens[0].to_lowercase().as_str()) {
            // Keep the modifier as part of type? We'll remove to avoid confusing but keep typ as remaining
            typ_tokens.remove(0);
        }
        // Handle ref/out attached to name like "&name" or "*name" unlikely in C#
        let mut typ_str = typ_tokens.join(" ");
        // Handle nullable ? attached to type or name
        // name may include "?"? No
        // If typ_str is empty -> use "object"
        if typ_str.is_empty() {
            typ_str = "object".to_string();
        }
        // Validate name
        if name.is_empty() || name.contains('<') || name.contains('>') {
            name = format!("arg{}", out.len());
        }
        // Validate identifier
        let valid = {
            let mut cs = name.chars();
            match cs.next() {
                Some(c) if c.is_alphabetic() || c == '_' || c == '@' => true,
                _ => false,
            }
        };
        if !valid {
            name = format!("arg{}", out.len());
        }
        // Strip @ prefix
        if name.starts_with('@') {
            name = name[1..].to_string();
        }
        out.push(Parameter {
            name,
            typ: typ_str,
        });
    }
    out
}

fn split_params_respecting(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut depth_paren: i32 = 0;
    let mut depth_angle: i32 = 0;
    let mut depth_bracket: i32 = 0;
    let mut depth_brace: i32 = 0;
    let mut in_string = false;
    let mut in_char = false;
    let mut escape = false;
    for ch in s.chars() {
        if escape {
            cur.push(ch);
            escape = false;
            continue;
        }
        if ch == '\\' {
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
        if in_char {
            if ch == '\'' {
                in_char = false;
            }
            cur.push(ch);
            continue;
        }
        match ch {
            '"' => {
                in_string = true;
                cur.push(ch);
            }
            '\'' => {
                in_char = true;
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

#[derive(Debug, Clone)]
enum ScopeKind {
    Namespace,
    Class,
}
#[derive(Debug, Clone)]
struct Scope {
    kind: ScopeKind,
    name: String,
    open_depth: usize,
}

fn parse_methods(content: &str) -> Result<Vec<CsharpMethodRaw>> {
    let mut line_starts: Vec<usize> = vec![0];
    for (i, b) in content.bytes().enumerate() {
        if b == b'\n' {
            line_starts.push(i + 1);
        }
    }
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return Ok(Vec::new());
    }

    let file_scoped_ns = parse_file_scoped_namespace(content);
    let mut scope_stack: Vec<Scope> = Vec::new();
    let mut brace_depth: usize = 0;
    let mut result: Vec<CsharpMethodRaw> = Vec::new();

    let mut i = 0usize;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        // compute current namespace/class for detection (before updating with this line)
        let current_namespace = {
            let mut parts: Vec<String> = Vec::new();
            if let Some(ref fns) = file_scoped_ns {
                parts.push(fns.clone());
            }
            for s in &scope_stack {
                if let ScopeKind::Namespace = s.kind {
                    parts.push(s.name.clone());
                }
            }
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("."))
            }
        };
        let current_class = scope_stack
            .iter()
            .rev()
            .find_map(|s| match s.kind {
                ScopeKind::Class => Some(s.name.clone()),
                _ => None,
            });
        // For qualified we may want innermost class, but also handle nested classes joined? Use innermost for now; alternative join all class scopes with '.'
        let _all_classes = {
            let cs: Vec<String> = scope_stack
                .iter()
                .filter_map(|s| match s.kind {
                    ScopeKind::Class => Some(s.name.clone()),
                    _ => None,
                })
                .collect();
            if cs.is_empty() {
                None
            } else {
                Some(cs.join("."))
            }
        };

        // skip detection for obvious non-method lines
        let skip_detection = trimmed.is_empty()
            || trimmed.starts_with("//")
            || trimmed.starts_with("/*")
            || trimmed.starts_with("*")
            || trimmed.starts_with("[") && !trimmed.contains('(') // attribute line alone
            || trimmed.starts_with("using ")
            || trimmed.starts_with("namespace ")
            || trimmed == "{"
            || trimmed == "}";

        // Property skip: properties have no '(' so they will be naturally skipped

        let mut tried_detection = false;
        if !skip_detection && !is_control_flow(trimmed) && trimmed.contains('(') {
            // Heuristic: must have ')' and then '{' or "=>" nearby
            if let Some(paren_end_idx) = find_paren_end_idx(&lines, i) {
                // Build after string from paren_end line onwards up to 4 lines
                let mut after = String::new();
                let close_line = lines[paren_end_idx];
                if let Some(pos) = close_line.rfind(')') {
                    after.push_str(&close_line[pos + 1..]);
                }
                for idx in paren_end_idx + 1..std::cmp::min(paren_end_idx + 5, lines.len()) {
                    after.push(' ');
                    after.push_str(lines[idx]);
                }
                let after_trim = after.trim();
                // Skip if prototype like abstract without body: ends with ';' and no '{' nearby
                // Also skip property accessor like "{ get; set; }" without parens already filtered
                // Check for semicolon before brace => declaration without body
                let semi_pos = after_trim.find(';');
                let brace_pos = after_trim.find('{');
                let arrow_pos = after_trim.find("=>");
                // If has "=>" and not brace, it's expression-bodied method -> allow but need brace matching alternative? We require brace, so skip expression-bodied for now? However we could handle => as body.
                // Task says Use brace matching, so we focus on brace. We'll allow expression-bodied as alternative with semi but not required for tests.
                // For now, if has brace before semi, it's definition.
                let is_proto = match (semi_pos, brace_pos) {
                    (Some(s), Some(b)) => s < b,
                    (Some(_), None) => {
                        if arrow_pos.is_some() {
                            false // expression bodied counts as definition
                        } else {
                            true
                        }
                    }
                    _ => false,
                };
                // Also skip if line contains "=>" accessor for property? But property no parens so already out
                // Check that trimmed not containing '=' before '(' (variable assignment with function call)
                let mut skip_due_eq = false;
                let trimmed_before_paren = if let Some(p) = lines[i].find('(') {
                    &lines[i][..p]
                } else {
                    lines[i]
                };
                if trimmed_before_paren.contains('=') && !trimmed.contains("=>") {
                    // heuristic: if '=' appears before '(' it's likely variable assignment not method
                    // but allow "=>" lambda? not here
                    // Also allow generics with "where" constraints containing "="
                    // Simple check: if trimmed contains " = " before '('
                    if let Some(eq) = lines[i].find('=') {
                        if let Some(par) = lines[i].find('(') {
                            if eq < par {
                                skip_due_eq = true;
                            }
                        }
                    }
                }
                if is_proto {
                    // skip prototype/abstract/interface method without body
                } else if skip_due_eq {
                    // skip
                } else {
                    // Need brace or arrow
                    let mut has_brace_near = brace_pos.is_some() || lines[paren_end_idx].contains('{');
                    let has_arrow_near = arrow_pos.is_some() || lines[paren_end_idx].contains("=>");
                    if !has_brace_near && !has_arrow_near {
                        for idx in paren_end_idx..std::cmp::min(paren_end_idx + 5, lines.len()) {
                            if lines[idx].contains('{') {
                                has_brace_near = true;
                                break;
                            }
                            if lines[idx].contains("=>") {
                                has_brace_near = true;
                                break;
                            }
                            if lines[idx].contains(';') {
                                break;
                            }
                        }
                    }
                    if has_brace_near {
                        // Build signature join from i to paren_end_idx
                        let sig_lines: Vec<String> = (i..=paren_end_idx)
                            .map(|idx| lines[idx].to_string())
                            .collect();
                        let sig_joined = sig_lines.join(" ").trim().to_string();
                        // Quick validation: signature must look like method not property
                        // Check that before_paren contains at least one space or generic? else maybe constructor without return type
                        // Allow.

                        let (vis, mods, ret, name, type_params, params) =
                            parse_csharp_signature(&sig_joined);

                        if name.is_empty() || name == "unknown" || name == "if" || name == "for" || name == "while" || name == "switch" {
                            // invalid
                        } else {
                            // Additional skip for property: if sig contains " get;" or " set;" ?
                            let low = sig_joined.to_lowercase();
                            if low.contains("{ get") || low.contains("{ set") {
                                // property skip
                            } else {
                                // Find open brace after closing paren for accurate single-line cases
                                let paren_col = lines[paren_end_idx].rfind(')').unwrap_or(0);
                                let paren_byte = line_starts[paren_end_idx] + paren_col;
                                if let Some(brace_byte) =
                                    find_open_brace_after(content, paren_byte + 1)
                                {
                                    if let Some(end_byte_inclusive) =
                                        find_matching_brace(content, brace_byte)
                                    {
                                        let (end_line, end_col) =
                                            byte_to_line_col(content, &line_starts, end_byte_inclusive);
                                        let start_line = i + 1;
                                        let start_col = first_non_space_col(lines[i]) + 1;
                                        let start_byte =
                                            line_starts[i] + first_non_space_col(lines[i]);
                                        let source_text =
                                            content[start_byte..=end_byte_inclusive].to_string();

                                        // Determine class for qualified: use all_classes if nested else current_class
                                        let class_for_q = _all_classes.clone().or(current_class.clone());

                                        result.push(CsharpMethodRaw {
                                            name: name.clone(),
                                            return_type: ret.clone(),
                                            visibility: vis.clone(),
                                            modifiers: mods.clone(),
                                            type_params: type_params.clone(),
                                            parameters: params.clone(),
                                            start_line,
                                            start_col,
                                            start_byte,
                                            end_line,
                                            end_col,
                                            end_byte: end_byte_inclusive,
                                            source_text,
                                            namespace: current_namespace.clone(),
                                            class: class_for_q.clone(),
                                        });
                                        tried_detection = true;
                                        i = end_line;
                                        continue;
                                    }
                                }
                            }
                        }
                    } else if has_arrow_near {
                        // Expression-bodied method: treat as method with end at ';'
                        let sig_lines: Vec<String> = (i..=paren_end_idx)
                            .map(|idx| lines[idx].to_string())
                            .collect();
                        let sig_joined = sig_lines.join(" ").trim().to_string();
                        let (vis, mods, ret, name, type_params, params) =
                            parse_csharp_signature(&sig_joined);
                        if !name.is_empty() && name != "unknown" {
                            // Find semicolon end
                            let mut end_idx = paren_end_idx;
                            for idx in paren_end_idx..lines.len() {
                                if lines[idx].contains(';') {
                                    end_idx = idx;
                                    break;
                                }
                                if lines[idx].contains('{') {
                                    break;
                                }
                                if idx > paren_end_idx + 3 {
                                    break;
                                }
                            }
                            let start_line = i + 1;
                            let start_col = first_non_space_col(lines[i]) + 1;
                            let start_byte = line_starts[i] + first_non_space_col(lines[i]);
                            let end_line = end_idx + 1;
                            let end_col = lines[end_idx].len();
                            let end_byte = line_starts[end_idx] + lines[end_idx].len();
                            let end_byte_inc = if end_byte > 0 && end_byte <= content.len() {
                                end_byte - 1
                            } else {
                                end_byte
                            };
                            let source_text = if start_byte <= end_byte_inc && end_byte_inc < content.len() {
                                content[start_byte..=end_byte_inc].to_string()
                            } else {
                                sig_joined.clone()
                            };
                            let class_for_q = _all_classes.clone().or(current_class.clone());
                            result.push(CsharpMethodRaw {
                                name: name.clone(),
                                return_type: ret.clone(),
                                visibility: vis.clone(),
                                modifiers: mods.clone(),
                                type_params: type_params.clone(),
                                parameters: params.clone(),
                                start_line,
                                start_col,
                                start_byte,
                                end_line,
                                end_col,
                                end_byte: end_byte_inc,
                                source_text,
                                namespace: current_namespace.clone(),
                                class: class_for_q.clone(),
                            });
                            tried_detection = true;
                            i = end_line;
                            continue;
                        }
                    }
                }
            }
        }

        if !tried_detection {
            // Normal scope handling
            let mut pushed = false;
            if let Some(ns_name) = parse_namespace_name(trimmed) {
                if trimmed.contains('{') {
                    scope_stack.push(Scope {
                        kind: ScopeKind::Namespace,
                        name: ns_name,
                        open_depth: brace_depth,
                    });
                    pushed = true;
                } else if i + 1 < lines.len() && lines[i + 1].trim().starts_with('{') {
                    scope_stack.push(Scope {
                        kind: ScopeKind::Namespace,
                        name: ns_name,
                        open_depth: brace_depth,
                    });
                    pushed = true;
                }
            }
            if !pushed {
                if let Some((_kind, cls_name)) = parse_class_name(trimmed) {
                    if !trimmed.ends_with(';') {
                        let has_brace = trimmed.contains('{')
                            || (i + 1 < lines.len() && lines[i + 1].trim().starts_with('{'));
                        if has_brace {
                            scope_stack.push(Scope {
                                kind: ScopeKind::Class,
                                name: cls_name,
                                open_depth: brace_depth,
                            });
                        }
                    }
                }
            }

            let (o, c) = count_braces(line);
            brace_depth = brace_depth + o;
            if c > brace_depth {
                brace_depth = 0;
            } else {
                brace_depth -= c;
            }
            while let Some(top) = scope_stack.last() {
                if brace_depth <= top.open_depth {
                    scope_stack.pop();
                } else {
                    break;
                }
            }
            i += 1;
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const SAMPLE_NS_CLASS: &str = r#"namespace Ns {
    class Cls {
        public static int Foo(int a) {
            return a;
        }
        private async Task<string> Bar<T>(List<T> items) {
            return "";
        }
        public int Prop { get; set; }
        protected virtual void Baz() {
        }
    }
}
"#;

    const SAMPLE_GENERICS: &str = r#"namespace MyApp.Models {
    public class Repo {
        public sealed override string Get<T>(T arg) where T : class {
            return arg.ToString();
        }
        internal static async Task<int> ComputeAsync(int x, int y) {
            return x + y;
        }
    }
}
"#;

    const SAMPLE_FILE_SCOPED: &str = r#"namespace FileNs;
public class Foo {
    public void MethodA() {
        var s = "{ not a brace }";
        // comment { }
        /* block { } */
    }
    private int MethodB(string s) => s.Length;
}
"#;

    #[test]
    fn extracts_ns_class_methods() {
        let fns = extract(SAMPLE_NS_CLASS, Path::new("Cls.cs")).unwrap();
        // Should find Foo, Bar, Baz (3) skip Prop
        assert_eq!(fns.len(), 3, "found {:?}", fns.iter().map(|f| &f.identity.name).collect::<Vec<_>>());
        let foo = fns.iter().find(|f| f.identity.name == "Foo").unwrap();
        assert_eq!(foo.identity.qualified_name, "Ns::Cls::Foo");
        assert_eq!(foo.declaration.visibility, "public");
        assert!(foo.declaration.modifiers.contains(&"static".to_string()));
        assert_eq!(foo.signature.return_type.as_deref(), Some("int"));
        assert_eq!(foo.signature.parameters.len(), 1);
        assert!(foo.source.hash.starts_with("sha256:"));
        assert_eq!(foo.identity.language, "csharp");
        assert_eq!(foo.metadata.parser.as_deref(), Some("reko-csharpExtractor"));

        let bar = fns.iter().find(|f| f.identity.name == "Bar").unwrap();
        assert_eq!(bar.declaration.visibility, "private");
        assert!(bar.declaration.modifiers.contains(&"async".to_string()));
        assert_eq!(bar.signature.type_parameters, vec!["T"]);
        assert!(bar.execution.is_async);
        // namespace/class context
        assert_eq!(bar.context.namespace.as_deref(), Some("Ns"));
        assert_eq!(bar.context.class.as_deref(), Some("Cls"));

        let baz = fns.iter().find(|f| f.identity.name == "Baz").unwrap();
        assert_eq!(baz.declaration.visibility, "protected");
        assert!(baz.declaration.modifiers.contains(&"virtual".to_string()));
        assert_eq!(baz.signature.return_type.as_deref(), Some("void"));
    }

    #[test]
    fn extracts_generics_and_modifiers() {
        let fns = extract(SAMPLE_GENERICS, Path::new("Repo.cs")).unwrap();
        assert_eq!(fns.len(), 2);
        let get = fns.iter().find(|f| f.identity.name == "Get").unwrap();
        assert_eq!(get.identity.qualified_name, "MyApp.Models::Repo::Get");
        assert!(get.declaration.modifiers.contains(&"sealed".to_string()));
        assert!(get.declaration.modifiers.contains(&"override".to_string()));
        // generics
        assert_eq!(get.signature.type_parameters, vec!["T"]);
        assert_eq!(get.signature.return_type.as_deref(), Some("string"));
        assert_eq!(get.signature.parameters.len(), 1);

        let comp = fns.iter().find(|f| f.identity.name == "ComputeAsync").unwrap();
        assert_eq!(comp.declaration.visibility, "internal");
        assert!(comp.declaration.modifiers.contains(&"static".to_string()));
        assert!(comp.declaration.modifiers.contains(&"async".to_string()));
        assert_eq!(comp.signature.return_type.as_deref(), Some("Task<int>"));
        assert_eq!(comp.signature.parameters.len(), 2);
    }

    #[test]
    fn brace_matching_and_property_skip() {
        let fns = extract(SAMPLE_FILE_SCOPED, Path::new("Foo.cs")).unwrap();
        // MethodA and MethodB (expression-bodied) = 2, no property
        assert_eq!(fns.len(), 2, "found {:?}", fns.iter().map(|f| &f.identity.name).collect::<Vec<_>>());
        let ma = fns.iter().find(|f| f.identity.name == "MethodA").unwrap();
        assert_eq!(ma.identity.qualified_name, "FileNs::Foo::MethodA");
        // brace matching should include string with braces but not confuse
        assert!(ma.source.source_text.contains("{ not a brace }"));
        assert!(ma.source.location.start.byte < ma.source.location.end.byte);
        assert!(ma.source.hash.starts_with("sha256:"));
        let mb = fns.iter().find(|f| f.identity.name == "MethodB").unwrap();
        assert_eq!(mb.signature.return_type.as_deref(), Some("int"));
        assert_eq!(mb.declaration.visibility, "private");
    }

    #[test]
    fn location_and_hash() {
        let content = "namespace N {\n class C {\n public void X() { int x=1; }\n }\n}";
        let fns = extract(content, Path::new("a.cs")).unwrap();
        assert_eq!(fns.len(), 1);
        let f = &fns[0];
        assert!(f.source.location.start.line >= 1);
        assert!(f.source.location.end.byte >= f.source.location.start.byte);
        assert!(f.source.hash.starts_with("sha256:"));
        assert_eq!(f.identity.language, "csharp");
    }
}
