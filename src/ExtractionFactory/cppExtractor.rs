use crate::ir::{IrFunction, Parameter, Position};
use anyhow::Result;
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Debug, Clone)]
struct CppFunctionRaw {
    name: String,
    short_name: String,
    return_type: Option<String>,
    visibility: String,
    modifiers: Vec<String>,
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

/// Public entry: given file content (exact, from reader) and file path, extract IR for each C++ function/method.
pub fn extract(content: &str, file_path: &Path) -> Result<Vec<IrFunction>> {
    let module = file_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    let file_str = file_path.display().to_string();

    let raws = parse_cpp_functions(content)?;

    let mut out = Vec::new();
    for r in raws {
        let hash = hash_source(&r.source_text);
        // qualified_name like "ns::Class::func" or "module::func"
        let qualified = match (&r.namespace, &r.class) {
            (Some(ns), Some(cls)) => format!("{}::{}::{}", ns, cls, r.short_name),
            (Some(ns), None) => format!("{}::{}", ns, r.short_name),
            (None, Some(cls)) => format!("{}::{}", cls, r.short_name),
            (None, None) => format!("{}::{}", module, r.short_name),
        };
        // If raw name already contains :: and namespace/class empty, use full name as qualified fallback
        // but keep above logic for context-aware qualified; if r.name contains :: and qualified doesn't contain it, prefer r.name
        let qualified_final = if r.name.contains("::") && !qualified.contains(&r.name) {
            // If class/namespace context already covers, keep qualified; otherwise use name with module prefix if needed
            // For definitions like "int ns::Foo::bar()" where class is Foo but namespace ns::Foo scope, the name captures full.
            // Use r.name as qualified if it already looks fully qualified and namespace is None
            if r.namespace.is_none() && r.class.is_none() {
                // ensure module prefix if not present
                if r.name.contains("::") {
                    r.name.clone()
                } else {
                    qualified.clone()
                }
            } else {
                qualified.clone()
            }
        } else {
            qualified.clone()
        };
        let id = qualified_final.clone();

        let mut ir = IrFunction::new_minimal(
            id,
            r.short_name.clone(),
            qualified_final.clone(),
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
            r.namespace.clone().or_else(|| Some(r.namespace.clone().unwrap_or_default())).and_then(|s| if s.is_empty() { None } else { Some(s) }),
            r.class.clone(),
        );
        // Patch language from java default to cpp
        ir.identity.language = "cpp".to_string();
        ir.metadata.parser = Some("reko-cppExtractor".to_string());
        ir.metadata.parser_version = Some("0.1.0".to_string());
        // context: namespace, package, module
        ir.context.namespace = r.namespace.clone();
        ir.context.package = r.namespace.clone();
        ir.context.module = Some(module.clone());
        ir.context.class = r.class.clone();
        ir.source.module = module.clone();

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

fn is_control_flow(trimmed: &str) -> bool {
    trimmed.starts_with("if ")
        || trimmed.starts_with("if(")
        || trimmed.starts_with("for ")
        || trimmed.starts_with("for(")
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

fn find_open_brace(content: &str, line_starts: &[usize], start_idx: usize) -> Result<(usize, usize, usize)> {
    let lines: Vec<&str> = content.lines().collect();
    for idx in start_idx..lines.len() {
        if let Some(col) = lines[idx].find('{') {
            let byte = line_starts[idx] + col;
            return Ok((idx + 1, col + 1, byte));
        }
        if lines[idx].contains(';') {
            anyhow::bail!("prototype terminates with ;")
        }
        if idx > start_idx + 4 {
            break;
        }
    }
    anyhow::bail!("no open brace found for function starting at line {}", start_idx + 1)
}

fn find_paren_end_idx(lines: &[&str], start_idx: usize) -> Option<usize> {
    let mut depth: i32 = 0;
    let mut started = false;
    let max = std::cmp::min(start_idx + 12, lines.len());
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

fn parse_namespace_name(trimmed: &str) -> Option<String> {
    if !trimmed.starts_with("namespace") {
        return None;
    }
    let after = trimmed["namespace".len()..].trim_start();
    if after.is_empty() || after.starts_with('{') || after.starts_with('=') {
        return None;
    }
    // capture until '{' or ';' or '='
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
    // candidate may have alias like "ns = other::ns", take first token
    let name = candidate.split_whitespace().next().unwrap_or("").trim().trim_end_matches('{').trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

fn parse_class_name(trimmed: &str) -> Option<(String, String)> {
    // returns (kind, name)
    // handle "class Foo" and "struct Foo"
    let mut kind = "";
    let mut idx_opt: Option<usize> = None;
    if let Some(pos) = trimmed.find("class ") {
        // ensure it's a keyword: preceding char space or start
        idx_opt = Some(pos);
        kind = "class";
    } else if trimmed.starts_with("class ") {
        idx_opt = Some(0);
        kind = "class";
    }
    if idx_opt.is_none() {
        if let Some(pos) = trimmed.find("struct ") {
            idx_opt = Some(pos);
            kind = "struct";
        } else if trimmed.starts_with("struct ") {
            idx_opt = Some(0);
            kind = "struct";
        }
    }
    let idx = idx_opt?;
    let kw_len = if kind == "class" { 6 } else { 7 };
    let after = &trimmed[idx + kw_len..];
    let after_trim = after.trim_start();
    if after_trim.is_empty() {
        return None;
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
        None
    } else {
        Some((kind.to_string(), name))
    }
}

fn is_valid_cpp_identifier(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    // handle qualified names: split by ::
    let mut parts: Vec<&str> = s.split("::").collect();
    // For destructor, first part may be empty? e.g., "~Foo"
    // So check each part
    for part in &mut parts {
        let mut p = part.to_string();
        // strip leading ~ for destructor
        if p.starts_with('~') {
            p = p[1..].to_string();
        }
        // strip operator prefix? e.g., operator+
        if p.starts_with("operator") {
            // consider valid
            continue;
        }
        if p.is_empty() {
            return false;
        }
        let mut chars = p.chars();
        match chars.next() {
            Some(c) if c.is_alphabetic() || c == '_' => {}
            _ => return false,
        }
        for c in chars {
            if !(c.is_alphanumeric() || c == '_' ) {
                // allow template params like Foo<int> not here
                // if contains '<' or '>' treat as invalid for simple check
                return false;
            }
        }
    }
    true
}

fn parse_cpp_signature(sig: &str, class_hint: Option<&str>) -> (String, Vec<String>, Option<String>, String, String, Vec<Parameter>) {
    // sig may contain template prefix
    let mut core = sig.trim().trim_end_matches('{').trim().to_string();
    // strip template<...> prefix
    if core.starts_with("template") {
        if let Some(par) = core.find('(') {
            if let Some(gt) = core[..par].rfind('>') {
                core = core[gt + 1..].trim().to_string();
            }
        } else if let Some(gt) = core.rfind('>') {
            // template class? not function
            // but leave as is
            if gt + 1 < core.len() {
                // keep after
            }
        }
    }
    // Extract trailing qualifiers after ')': const, noexcept, override, final, &, &&
    let paren_start = core.find('(').unwrap_or(core.len());
    let before_paren = core[..paren_start].trim().to_string();
    let inside_paren = if paren_start < core.len() {
        let paren_end = core.rfind(')').unwrap_or(core.len() - 1);
        core[paren_start + 1..paren_end].trim().to_string()
    } else {
        String::new()
    };
    let after_paren_raw = if paren_start < core.len() {
        if let Some(end) = core.rfind(')') {
            core[end + 1..].trim().to_string()
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    let mut modifiers: Vec<String> = Vec::new();
    // detect trailing modifiers: const, noexcept, noexcept(...), override, final, volatile, &, &&
    for tok in after_paren_raw.split_whitespace() {
        let t = tok.trim_matches(|c| c == '{' || c == ';' || c == ':' || c == ',');
        match t {
            "const" | "volatile" | "override" | "final" => modifiers.push(t.to_string()),
            _ if t.starts_with("noexcept") => modifiers.push("noexcept".to_string()),
            "&" | "&&" => modifiers.push(t.to_string()),
            _ => {}
        }
    }
    // tokens before '('
    let tokens: Vec<&str> = before_paren.split_whitespace().collect();
    if tokens.is_empty() {
        return (
            "public".to_string(),
            modifiers,
            None,
            "unknown".to_string(),
            "unknown".to_string(),
            vec![],
        );
    }
    let raw_name_token = tokens.last().unwrap().to_string();
    // handle leading *& attached to name token like "*foo" or "&foo"
    let mut prefix = String::new();
    for c in raw_name_token.chars() {
        if c == '*' || c == '&' {
            prefix.push(c);
        } else {
            break;
        }
    }
    let mut clean = raw_name_token.trim_start_matches(|c| c == '*' || c == '&').trim().to_string();
    // Remove array brackets
    if let Some(br) = clean.find('[') {
        clean = clean[..br].to_string();
    }
    // For operator overloads, keep full like "operator+"
    // clean may be "operator+" etc. We'll keep.
    let name = clean.clone();
    // short name: last component after ::
    let short_name = if name.contains("::") {
        name.split("::").last().unwrap_or(&name).to_string()
    } else {
        name.clone()
    };

    let ret_and_mods = if tokens.len() >= 2 {
        &tokens[..tokens.len() - 1]
    } else {
        &[]
    };

    const MODS: &[&str] = &["static", "inline", "virtual", "explicit", "friend", "constexpr", "consteval", "constinit", "extern", "register", "mutable", "volatile"];
    let mut type_parts: Vec<String> = Vec::new();
    for tok in ret_and_mods {
        let t = tok.trim();
        if t.is_empty() {
            continue;
        }
        let lower = t.to_lowercase();
        if MODS.contains(&t) || MODS.contains(&lower.as_str()) {
            modifiers.push(t.to_string());
        } else {
            type_parts.push(t.to_string());
        }
    }
    // Also push prefix stars/amps as part of return type? Actually they belong to return type if attached to name
    // But if we stripped them, they were part of return type pointer
    if !prefix.is_empty() && !type_parts.is_empty() {
        // append to last type part?
        let last = type_parts.last_mut().unwrap();
        *last = format!("{}{}", last, prefix);
    } else if !prefix.is_empty() && type_parts.is_empty() {
        type_parts.push(prefix);
    }

    // Determine return_type: if type_parts empty => likely constructor/destructor
    let return_type = if type_parts.is_empty() {
        // Check if short_name == class_hint or ~class_hint => constructor/destructor
        if let Some(cls) = class_hint {
            if short_name == cls || short_name == format!("~{}", cls) {
                None
            } else {
                // maybe no return type but not constructor? e.g., macro? return None
                None
            }
        } else {
            // No class hint, but maybe constructor heuristics: if name without return may still be function with implicit int? Keep None
            None
        }
    } else {
        let joined = type_parts.join(" ").replace("  ", " ").trim().to_string();
        if joined.is_empty() { None } else { Some(joined) }
    };

    // visibility is determined externally; we return placeholder "public"
    // modifiers already collected
    let visibility = "public".to_string();

    let parameters = parse_cpp_params(&inside_paren);

    (visibility, modifiers, return_type, name, short_name, parameters)
}

fn parse_cpp_params(s: &str) -> Vec<Parameter> {
    let t = s.trim();
    if t.is_empty() || t == "void" {
        return vec![];
    }
    let parts = split_params_respecting(t);
    let mut out = Vec::new();
    for part in parts {
        let p = part.trim();
        if p.is_empty() || p == "void" {
            continue;
        }
        if p == "..." {
            out.push(Parameter { name: "...".to_string(), typ: "...".to_string() });
            continue;
        }
        if p.contains("(*") {
            // function pointer param simplified
            if let Some(start) = p.find("(*") {
                let after = &p[start + 2..];
                if let Some(end) = after.find(')') {
                    let fn_name = after[..end].trim().trim_start_matches('*').trim().to_string();
                    let clean = fn_name.split_whitespace().next().unwrap_or("callback").to_string();
                    out.push(Parameter { name: if clean.is_empty() { "callback".to_string() } else { clean }, typ: p.to_string() });
                    continue;
                }
            }
            out.push(Parameter { name: format!("arg{}", out.len()), typ: p.to_string() });
            continue;
        }
        // Remove default value
        let before_eq = p.split('=').next().unwrap_or(p).trim();
        let tokens: Vec<&str> = before_eq.split_whitespace().collect();
        if tokens.is_empty() {
            continue;
        }
        if tokens.len() == 1 {
            let typ = tokens[0].to_string();
            out.push(Parameter { name: format!("arg{}", out.len()), typ });
            continue;
        }
        let raw_name = tokens.last().unwrap().to_string();
        let prefix_count = raw_name.chars().take_while(|c| *c == '*' || *c == '&').count();
        let mut name = raw_name.trim_start_matches(|c| c == '*' || c == '&').to_string();
        if let Some(br) = name.find('[') {
            name = name[..br].to_string();
        }
        name = name.trim_matches(|c| c == '(' || c == ')' || c == '*' || c == '&').to_string();
        let mut typ_str = tokens[..tokens.len() - 1].join(" ");
        if prefix_count > 0 {
            let prefix: String = raw_name.chars().take(prefix_count).collect();
            typ_str = format!("{} {}", typ_str, prefix).trim().to_string();
        }
        // handle reference markers attached to type like "std::string&"
        // already in typ_str
        if name.is_empty() || name.contains('<') || name.contains('>') || name.contains("::") && name.contains(' ') {
            name = format!("arg{}", out.len());
        }
        // validate name
        let valid = {
            if name.is_empty() { false } else {
                let mut cs = name.chars();
                match cs.next() {
                    Some(c) if c.is_alphabetic() || c == '_' => true,
                    _ => false,
                }
            }
        };
        if !valid {
            // maybe name is like "&&" etc
            name = format!("arg{}", out.len());
        }
        let typ_clean = if typ_str.is_empty() { "int".to_string() } else { typ_str };
        out.push(Parameter { name, typ: typ_clean });
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
            '"' => { in_string = true; cur.push(ch); }
            '\'' => { in_char = true; cur.push(ch); }
            '(' => { depth_paren += 1; cur.push(ch); }
            ')' => { depth_paren -= 1; cur.push(ch); }
            '<' => { depth_angle += 1; cur.push(ch); }
            '>' => { if depth_angle > 0 { depth_angle -= 1; } cur.push(ch); }
            '[' => { depth_bracket += 1; cur.push(ch); }
            ']' => { depth_bracket -= 1; cur.push(ch); }
            '{' => { depth_brace += 1; cur.push(ch); }
            '}' => { depth_brace -= 1; cur.push(ch); }
            ',' => {
                if depth_paren==0 && depth_angle==0 && depth_bracket==0 && depth_brace==0 {
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
enum ScopeKind { Namespace, Class }
#[derive(Debug, Clone)]
struct Scope {
    kind: ScopeKind,
    name: String,
    open_depth: usize,
    visibility: String, // only for class
}

fn count_braces(s: &str) -> (usize, usize) {
    // naive count, ignoring strings/comments for scope tracking is okay for tests
    let open = s.matches('{').count();
    let close = s.matches('}').count();
    (open, close)
}

fn parse_cpp_functions(content: &str) -> Result<Vec<CppFunctionRaw>> {
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
    let mut result: Vec<CppFunctionRaw> = Vec::new();
    let mut scope_stack: Vec<Scope> = Vec::new();
    let mut brace_depth: usize = 0;
    // track template pending: when we see template line, next function should be considered template
    let mut i = 0usize;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        // derive current namespace/class for detection (before updating with this line's scope)
        let current_namespace = {
            let ns_parts: Vec<String> = scope_stack.iter().filter_map(|s| match s.kind { ScopeKind::Namespace => Some(s.name.clone()), _ => None }).collect();
            if ns_parts.is_empty() { None } else { Some(ns_parts.join("::")) }
        };
        let current_class = scope_stack.iter().rev().find_map(|s| match s.kind { ScopeKind::Class => Some(s.name.clone()), _ => None });
        let current_visibility = scope_stack.iter().rev().find_map(|s| match s.kind { ScopeKind::Class => Some(s.visibility.clone()), _ => None }).unwrap_or_else(|| "public".to_string());

        // handle visibility labels inside class
        if trimmed == "public:" || trimmed == "private:" || trimmed == "protected:" {
            if let Some(last) = scope_stack.iter_mut().rev().find(|s| matches!(s.kind, ScopeKind::Class)) {
                let vis = trimmed.trim_end_matches(':').to_string();
                last.visibility = vis;
            }
            // update depth then continue
            let (o,c) = count_braces(line);
            brace_depth = brace_depth + o;
            if c > brace_depth { brace_depth = 0; } else { brace_depth -= c; }
            // pop closed scopes
            while let Some(top) = scope_stack.last() {
                if brace_depth <= top.open_depth {
                    scope_stack.pop();
                } else { break; }
            }
            i += 1;
            continue;
        }

        // Skip obvious non-function lines for detection but still need scope handling below
        let skip_detection = trimmed.is_empty()
            || trimmed.starts_with('#')
            || trimmed.starts_with("//")
            || trimmed.starts_with("/*")
            || trimmed.starts_with('*')
            || trimmed.starts_with("typedef")
            || trimmed.starts_with("using ")
            || trimmed.starts_with("extern \"")
            || trimmed == "{"
            || trimmed == "}";

        let mut tried_detection = false;
        if !skip_detection && !is_control_flow(trimmed) && trimmed.contains('(') {
            // handle template prefix: if this line starts with template, look ahead
            let mut sig_start_idx = i;
            let mut template_prefix = String::new();
            if trimmed.starts_with("template") {
                // include this line as part of signature; start is here
                template_prefix = line.to_string();
                // find function start on next non-empty line
                let mut j = i + 1;
                while j < lines.len() && lines[j].trim().is_empty() { j += 1; }
                if j < lines.len() && lines[j].trim().contains('(') {
                    sig_start_idx = j;
                    // We'll treat sig as template line + function lines
                    // For detection we will use j as effective i but keep start for source_text
                } else {
                    // template without function? skip
                    sig_start_idx = i; // will fail later
                }
            }

            // For signature joining we need to find paren end
            let effective_idx = if trimmed.starts_with("template") {
                // find next line with '('
                let mut j = i + 1;
                while j < lines.len() && !lines[j].contains('(') { j += 1; }
                if j < lines.len() { j } else { i }
            } else { i };

            if let Some(paren_end_idx) = find_paren_end_idx(&lines, effective_idx) {
                // check after paren for prototype vs definition
                let after = {
                    let mut s = String::new();
                    let close_line = lines[paren_end_idx];
                    if let Some(pos) = close_line.rfind(')') {
                        s.push_str(&close_line[pos+1..]);
                    }
                    for idx in paren_end_idx+1..std::cmp::min(paren_end_idx+4, lines.len()) {
                        s.push(' ');
                        s.push_str(lines[idx]);
                    }
                    s
                };
                let after_trim = after.trim();
                let semi_pos = after_trim.find(';');
                let brace_pos = after_trim.find('{');
                let colon_pos = after_trim.find(':'); // initializer list
                // Determine if prototype: ';' before '{' and no initializer colon before brace?
                // Cases: pure virtual "= 0;" has ';' and maybe no '{'
                // Constructor initializer ": member(x) {" has ':' before '{'
                let is_proto = match (semi_pos, brace_pos) {
                    (Some(s), Some(b)) => s < b,
                    (Some(_), None) => {
                        // check if after contains "= 0" or "= default" or "= delete" => prototype-like
                        true
                    },
                    _ => false,
                };
                // also handle "= 0" , "= default", "= delete" without brace - only check segment before brace
                let after_before_brace = if let Some(b) = brace_pos { &after_trim[..b] } else { after_trim };
                let after_before_nospace = after_before_brace.replace(' ', "").replace('\t',"");
                let is_pure = after_before_nospace.contains("=0") || after_before_nospace.contains("=default") || after_before_nospace.contains("=delete");
                if is_pure {
                    // skip, it's declaration
                } else if !is_proto {
                    // Check has brace near
                    let mut has_brace_near = brace_pos.is_some() || lines[paren_end_idx].contains('{');
                    // Also initializer colon + brace on next lines
                    if !has_brace_near {
                        for idx in paren_end_idx..std::cmp::min(paren_end_idx+4, lines.len()) {
                            if lines[idx].contains('{') { has_brace_near = true; break; }
                            if lines[idx].contains(';') { break; }
                        }
                    }
                    if has_brace_near || colon_pos.is_some() {
                        // Need to ensure not detecting function pointer or variable
                        // Check '=' before '(' in effective line
                        let eff_trim = lines[effective_idx].trim();
                        let mut skip_due_eq = false;
                        if eff_trim.contains('=') {
                            if let Some(eq) = eff_trim.find('=') {
                                if let Some(par) = eff_trim.find('(') {
                                    if eq < par { skip_due_eq = true; }
                                }
                            }
                        }
                        if !skip_due_eq {
                            // Build signature string: from sig_start_idx (or effective) to paren_end_idx joined
                            let sig_lines: Vec<String> = (effective_idx..=paren_end_idx).map(|idx| lines[idx].to_string()).collect();
                            let mut sig_joined = sig_lines.join(" ").trim().to_string();
                            if !template_prefix.is_empty() {
                                sig_joined = format!("{} {}", template_prefix, sig_joined);
                            }
                            // also handle qualified name detection after paren may include const etc.
                            // Parse signature with class hint for constructor detection
                            let (_vis, mods, ret, name, short_name, params) = parse_cpp_signature(&sig_joined, current_class.as_deref());

                            if name.is_empty() || name == "if" || name == "for" || name == "while" || name == "switch" || name == "return" {
                                // not valid
                            } else if !is_valid_cpp_identifier(&name) && !name.starts_with("operator") {
                                // invalid
                            } else {
                                // Find open brace
                                let brace_search_start = if trimmed.starts_with("template") { effective_idx } else { i };
                                if let Ok((_bl,_bc, brace_byte)) = find_open_brace(content, &line_starts, brace_search_start) {
                                    let start_line: usize;
                                    let start_col: usize;
                                    let start_byte: usize;
                                    if !template_prefix.is_empty() {
                                        // source starts at template line
                                        start_line = sig_start_idx + 1;
                                        start_col = first_non_space_col(lines[sig_start_idx]) + 1;
                                        start_byte = line_starts[sig_start_idx] + first_non_space_col(lines[sig_start_idx]);
                                    } else {
                                        start_line = brace_search_start + 1;
                                        start_col = first_non_space_col(lines[brace_search_start]) + 1;
                                        start_byte = line_starts[brace_search_start] + first_non_space_col(lines[brace_search_start]);
                                    }
                                    if let Some(end_byte_inclusive) = find_matching_brace(content, brace_byte) {
                                        let (end_line, end_col) = byte_to_line_col(content, &line_starts, end_byte_inclusive);
                                        let source_text = content[start_byte..=end_byte_inclusive].to_string();
                                        // Determine visibility: if inside class use current_visibility else public
                                        let vis = if current_class.is_some() { current_visibility.clone() } else { "public".to_string() };
                                        // Merge visibility into modifiers? keep separate
                                        result.push(CppFunctionRaw {
                                            name: name.clone(),
                                            short_name: short_name.clone(),
                                            return_type: ret.clone(),
                                            visibility: vis,
                                            modifiers: mods.clone(),
                                            parameters: params.clone(),
                                            start_line,
                                            start_col,
                                            start_byte,
                                            end_line,
                                            end_col,
                                            end_byte: end_byte_inclusive,
                                            source_text,
                                            namespace: current_namespace.clone(),
                                            class: current_class.clone(),
                                        });
                                        // Advance i to end_line
                                        tried_detection = true;
                                        // Update brace_depth to after function? We'll skip to end_line and also need to keep scope_stack correct for inner.
                                        // For now set i to end_line and update brace_depth approximately by counting braces inside function? Instead we will reset depth by recomputing?
                                        // Simpler: we will manually update brace_depth by counting braces from brace_search_start to end_line inclusive.
                                        // But we already have scope_stack that may be inside class; function braces should not affect pop of class (class remains).
                                        // So we need to ensure depth stays consistent.
                                        // We will advance i and update brace_depth by counting braces of skipped lines? Next iteration's depth handling will do counting per line, but we are jumping over lines, so we need to simulate.
                                        // Instead we jump to end_line and continue loop without extra per-line updates for skipped lines except we need to account for brace counts inside function.
                                        // Count braces inside function to keep depth correct: net braces inside function should be 0 (matched), so depth before function == depth after function.
                                        // So we can keep depth unchanged.
                                        i = end_line;
                                        // Need to ensure we don't process scope pushes inside function body (like nested namespace inside function not typical). We'll just continue.
                                        continue;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        if !tried_detection {
            // Normal scope handling for this line
            // Detect namespace / class declarations to push
            let mut pushed = false;
            if let Some(ns_name) = parse_namespace_name(trimmed) {
                if trimmed.contains('{') {
                    scope_stack.push(Scope { kind: ScopeKind::Namespace, name: ns_name, open_depth: brace_depth, visibility: String::new() });
                    pushed = true;
                } else {
                    // namespace without '{' on same line, check next line for '{'
                    if i + 1 < lines.len() && lines[i+1].trim().starts_with('{') {
                        scope_stack.push(Scope { kind: ScopeKind::Namespace, name: ns_name, open_depth: brace_depth, visibility: String::new() });
                        pushed = true;
                    }
                }
            }
            if !pushed {
                if let Some((kind, cls_name)) = parse_class_name(trimmed) {
                    if trimmed.contains('{') || trimmed.contains(':') || trimmed.ends_with('{') || trimmed.contains("class") || trimmed.contains("struct") {
                        // Heuristic: push if contains '{' or ':' (inheritance) or even if just class declaration line (forward decl ends with ';' should not push)
                        if !trimmed.ends_with(';') {
                            // Determine if there's a '{' in this or next line
                            let has_brace = trimmed.contains('{') || (i+1 < lines.len() && lines[i+1].trim().starts_with('{'));
                            if has_brace {
                                let vis = if kind == "class" { "private".to_string() } else { "public".to_string() };
                                scope_stack.push(Scope { kind: ScopeKind::Class, name: cls_name, open_depth: brace_depth, visibility: vis });
                            }
                        }
                    }
                }
            }

            // Update brace depth for this line
            let (o,c) = count_braces(line);
            brace_depth = brace_depth + o;
            if c > brace_depth { brace_depth = 0; } else { brace_depth -= c; }
            while let Some(top) = scope_stack.last() {
                if brace_depth <= top.open_depth {
                    scope_stack.pop();
                } else { break; }
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

    const SAMPLE_CPP: &str = r#"#include <string>
namespace math {
    template<typename T>
    T add(T a, T b) {
        return a + b;
    }

    class Calculator {
    public:
        int multiply(int a, int b) const {
            return a * b;
        }
        Calculator() {
            // constructor
        }
        ~Calculator() {
        }
        static inline void helper() noexcept {
        }
    private:
        void secret() const noexcept override {
        }
    };

    int nsfunc(double x) {
        if (x > 0) { return 1; }
        return 0;
    }
}

int globalFunc(int x, const std::string& s) {
    return x;
}

virtual void proto(int a);
"#;

    const SAMPLE_QUALIFIED: &str = r#"namespace ns {
class Foo {
public:
    void bar();
};
}

int ns::Foo::bar() {
    return 0;
}

template<typename T>
T templFunc(T v) {
    return v;
}
"#;

    #[test]
    fn extracts_cpp_functions() {
        let fns = extract(SAMPLE_CPP, Path::new("sample.cpp")).unwrap();
        // expected: add, multiply, Calculator (ctor), ~Calculator, helper, secret, nsfunc, globalFunc = 8
        assert_eq!(fns.len(), 8, "found: {:?}", fns.iter().map(|f| format!("{} {}", f.identity.qualified_name, f.identity.name)).collect::<Vec<_>>());
        let names: Vec<_> = fns.iter().map(|f| f.identity.name.as_str()).collect();
        assert!(names.contains(&"add"));
        assert!(names.contains(&"multiply"));
        assert!(names.contains(&"Calculator"));
        assert!(names.contains(&"~Calculator"));
        assert!(names.contains(&"helper"));
        assert!(names.contains(&"secret"));
        assert!(names.contains(&"nsfunc"));
        assert!(names.contains(&"globalFunc"));

        let add = fns.iter().find(|f| f.identity.name == "add").unwrap();
        assert_eq!(add.identity.qualified_name, "math::add");
        assert_eq!(add.context.namespace.as_deref(), Some("math"));
        assert!(add.identity.language == "cpp");
        assert!(add.source.hash.starts_with("sha256:"));
        assert!(add.source.location.start.byte < add.source.location.end.byte);
        // template function should have return type T
        assert!(add.signature.return_type.is_some());

        let mul = fns.iter().find(|f| f.identity.name == "multiply").unwrap();
        assert_eq!(mul.identity.qualified_name, "math::Calculator::multiply");
        assert_eq!(mul.context.class.as_deref(), Some("Calculator"));
        assert_eq!(mul.context.namespace.as_deref(), Some("math"));
        assert!(mul.declaration.modifiers.contains(&"const".to_string()) || mul.declaration.modifiers.iter().any(|m| m=="const"));
        assert_eq!(mul.declaration.visibility, "public");

        let helper = fns.iter().find(|f| f.identity.name == "helper").unwrap();
        assert!(helper.declaration.modifiers.contains(&"static".to_string()));
        assert!(helper.declaration.modifiers.contains(&"inline".to_string()));
        assert!(helper.declaration.modifiers.contains(&"noexcept".to_string()));

        let secret = fns.iter().find(|f| f.identity.name == "secret").unwrap();
        assert_eq!(secret.declaration.visibility, "private");
        assert!(secret.declaration.modifiers.contains(&"const".to_string()));
        assert!(secret.declaration.modifiers.contains(&"noexcept".to_string()));
        assert!(secret.declaration.modifiers.contains(&"override".to_string()));

        let ctor = fns.iter().find(|f| f.identity.name == "Calculator" && f.signature.return_type.is_none()).unwrap();
        assert_eq!(ctor.identity.qualified_name, "math::Calculator::Calculator");

        let global = fns.iter().find(|f| f.identity.name == "globalFunc").unwrap();
        assert_eq!(global.identity.qualified_name, "sample::globalFunc");
        assert_eq!(global.signature.parameters.len(), 2);
        assert!(global.source.source_text.contains("globalFunc"));
        assert!(!names.contains(&"proto"));
    }

    #[test]
    fn qualified_and_template() {
        let fns = extract(SAMPLE_QUALIFIED, Path::new("qual.cpp")).unwrap();
        // bar defined as ns::Foo::bar plus templFunc = 2
        assert_eq!(fns.len(), 2, "found {:?}", fns.iter().map(|f| &f.identity.name).collect::<Vec<_>>());
        let bar = fns.iter().find(|f| f.identity.name == "bar").unwrap();
        // bar's qualified may be ns::Foo::bar or qual::...
        // Since namespace stack at definition line is empty (outside namespace), but name contains ns::Foo::bar, qualified should retain that
        assert!(bar.identity.qualified_name.contains("bar"));
        assert!(bar.identity.qualified_name.contains("Foo") || bar.identity.qualified_name.contains("ns"));
        let templ = fns.iter().find(|f| f.identity.name == "templFunc").unwrap();
        assert_eq!(templ.identity.qualified_name, "qual::templFunc");
        assert!(templ.signature.parameters.len()==1);
        assert!(templ.source.location.start.byte < templ.source.location.end.byte);
    }

    #[test]
    fn control_flow_skipped() {
        let content = r#"int foo(int x) {
    if (x>0) { return x; }
    for(int i=0;i<x;i++) {}
    return 0;
}
if (true) { }
"#;
        let fns = extract(content, Path::new("a.cpp")).unwrap();
        assert_eq!(fns.len(), 1);
        assert_eq!(fns[0].identity.name, "foo");
    }
}
