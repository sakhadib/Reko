use crate::ir::{IrFunction, Parameter, Position};
use anyhow::Result;
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Debug, Clone)]
struct CFunctionRaw {
    name: String,
    return_type: String,
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
}

/// Public entry: given file content (exact, from reader) and file path, extract IR for each C function.
pub fn extract(content: &str, file_path: &Path) -> Result<Vec<IrFunction>> {
    let module = file_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    let file_str = file_path.display().to_string();

    let raws = parse_c_functions(content)?;

    let mut out = Vec::new();
    for r in raws {
        let hash = hash_source(&r.source_text);
        let qualified = format!("{}::{}", module, r.name);
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
            Some(r.return_type.clone()),
            r.parameters.clone(),
            vec![],
            None,
            None,
        );
        // Patch language from java default to c
        ir.identity.language = "c".to_string();
        ir.metadata.parser = Some("reko-cExtractor".to_string());
        ir.metadata.parser_version = Some("0.1.0".to_string());
        ir.context.module = Some(module.clone());
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
        || trimmed.starts_with("do ")
        || trimmed.starts_with("do{")
        || trimmed.starts_with("else")
        || trimmed.starts_with("catch ")
        || trimmed.starts_with("try ")
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
        if idx > start_idx + 3 {
            break;
        }
    }
    anyhow::bail!("no open brace found for function starting at line {}", start_idx + 1)
}

fn find_paren_end_idx(lines: &[&str], start_idx: usize) -> Option<usize> {
    let mut depth: i32 = 0;
    let mut started = false;
    let max = std::cmp::min(start_idx + 10, lines.len());
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
        // if started && depth <0 => unbalanced
        if started && depth < 0 {
            return None;
        }
    }
    None
}

fn parse_c_functions(content: &str) -> Result<Vec<CFunctionRaw>> {
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
    let mut result = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        // Skip obvious non-function lines
        if trimmed.is_empty()
            || trimmed.starts_with('#')
            || trimmed.starts_with("//")
            || trimmed.starts_with("/*")
            || trimmed.starts_with('*')
            || trimmed.starts_with("typedef")
            || trimmed.starts_with("struct ")
            || trimmed.starts_with("enum ")
            || trimmed.starts_with("union ")
            || trimmed == "{"
            || trimmed == "}"
        {
            i += 1;
            continue;
        }
        if is_control_flow(trimmed) {
            i += 1;
            continue;
        }

        // Need '(' to be candidate
        if !trimmed.contains('(') {
            i += 1;
            continue;
        }

        // Try to find paren end
        let paren_end_idx = match find_paren_end_idx(&lines, i) {
            Some(v) => v,
            None => {
                i += 1;
                continue;
            }
        };

        // Check after paren for prototype vs definition
        // Also need to ensure not detecting function pointer variable or similar
        // Heuristic: before '(' should contain an identifier (function name) and not contain '=','*(' etc
        // Quick check: if trimmed contains '=' before '(' maybe assignment
        if trimmed.contains('=') {
            // could be function pointer assignment, skip
            // But ensure '=' is before '('
            if let Some(eq) = trimmed.find('=') {
                if let Some(par) = trimmed.find('(') {
                    if eq < par {
                        i += 1;
                        continue;
                    }
                }
            }
        }

        // prototype detection: if ';' before '{' => skip
        let after = {
            // collect text after ')' up to next brace
            let mut s = String::new();
            let close_line = lines[paren_end_idx];
            if let Some(pos) = close_line.rfind(')') {
                s.push_str(&close_line[pos + 1..]);
            }
            for idx in paren_end_idx + 1..std::cmp::min(paren_end_idx + 3, lines.len()) {
                s.push(' ');
                s.push_str(lines[idx]);
            }
            s
        };
        let after_trim = after.trim();
        // If the collected after contains ';' before '{', it's prototype
        // Find first occurrences
        let semi_pos = after_trim.find(';');
        let brace_pos = after_trim.find('{');
        let is_proto = match (semi_pos, brace_pos) {
            (Some(s), Some(b)) => s < b,
            (Some(_), None) => true,
            _ => false,
        };
        if is_proto {
            // skip to after paren_end
            i = paren_end_idx + 1;
            continue;
        }

        // Also if no brace at all in near window, not a function definition
        let has_brace_near = brace_pos.is_some() || lines[paren_end_idx].contains('{');
        if !has_brace_near {
            // Check next 3 lines explicitly for '{' else skip
            let mut found = false;
            for idx in paren_end_idx..std::cmp::min(paren_end_idx + 4, lines.len()) {
                if lines[idx].contains('{') {
                    found = true;
                    break;
                }
                if lines[idx].contains(';') {
                    break;
                }
            }
            if !found {
                i += 1;
                continue;
            }
        }

        // Build signature string: lines[i..=paren_end_idx] joined
        let sig_lines: Vec<String> = (i..=paren_end_idx).map(|idx| lines[idx].to_string()).collect();
        let sig_joined = sig_lines.join(" ").trim().to_string();
        // Additional guard: sig must contain an identifier before '(' that is not a control keyword
        let (visibility, modifiers, return_type, name, params) = parse_c_signature(&sig_joined);
        if name.is_empty()
            || name == "if"
            || name == "for"
            || name == "while"
            || name == "switch"
            || name == "return"
        {
            i += 1;
            continue;
        }
        // Ensure return_type not empty and name is valid identifier
        if !is_valid_c_identifier(&name) {
            i += 1;
            continue;
        }

        // Find open brace position
        let (_brace_line, _brace_col, brace_byte) = match find_open_brace(content, &line_starts, i) {
            Ok(v) => v,
            Err(_) => {
                i = paren_end_idx + 1;
                continue;
            }
        };

        let start_line = i + 1;
        let start_col = first_non_space_col(line) + 1;
        let start_byte = line_starts[i] + first_non_space_col(line);

        let end_byte_inclusive = match find_matching_brace(content, brace_byte) {
            Some(v) => v,
            None => {
                i = paren_end_idx + 1;
                continue;
            }
        };
        let (end_line, end_col) = byte_to_line_col(content, &line_starts, end_byte_inclusive);
        let source_text = content[start_byte..=end_byte_inclusive].to_string();

        result.push(CFunctionRaw {
            name,
            return_type,
            visibility,
            modifiers,
            parameters: params,
            start_line,
            start_col,
            start_byte,
            end_line,
            end_col,
            end_byte: end_byte_inclusive,
            source_text,
        });

        i = end_line;
        continue;
    }
    Ok(result)
}

fn is_valid_c_identifier(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_alphabetic() || c == '_' => {}
        _ => return false,
    }
    for c in chars {
        if !(c.is_alphanumeric() || c == '_') {
            return false;
        }
    }
    true
}

fn parse_c_signature(sig: &str) -> (String, Vec<String>, String, String, Vec<Parameter>) {
    // sig like "static inline int foo(int a, const char *b) {"
    // Remove trailing '{' and trim
    let s = sig.trim().trim_end_matches('{').trim().to_string();

    // Remove possible trailing attribute like "__attribute__((...))" after )
    // We already truncated at paren end, so not needed

    // Extract inside parens (safe slicing)
    let paren_start = s.find('(').unwrap_or(s.len());
    let before_paren = if paren_start <= s.len() {
        s[..paren_start].trim().to_string()
    } else {
        String::new()
    };
    let inside_paren = if paren_start < s.len() {
        if let Some(paren_end) = s.rfind(')') {
            if paren_end > paren_start && paren_end <= s.len() {
                s[paren_start + 1..paren_end].trim().to_string()
            } else {
                String::new()
            }
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    // tokens before '('
    let tokens: Vec<&str> = before_paren.split_whitespace().collect();
    if tokens.is_empty() {
        return (
            "public".to_string(),
            vec![],
            "int".to_string(),
            "unknown".to_string(),
            vec![],
        );
    }
    let raw_name_token = tokens.last().unwrap().to_string();
    // handle stars attached to name: "*foo" or "foo" with leading stars
    let mut stars_in_name = 0usize;
    for c in raw_name_token.chars() {
        if c == '*' {
            stars_in_name += 1;
        } else {
            break;
        }
    }
    // Extract clean name: remove leading '*' and any trailing array/ptr marks
    let mut clean = raw_name_token.trim_start_matches('*').trim().to_string();
    // Remove trailing brackets like "[10]" if present inside name token
    if let Some(br) = clean.find('[') {
        clean = clean[..br].to_string();
    }
    // Also handle "(*name)" style not expected as function name but skip
    clean = clean.trim_matches(|c| c == '(' || c == ')' || c == '*').to_string();
    let name = clean;

    let ret_and_mods = if tokens.len() >= 2 {
        &tokens[..tokens.len() - 1]
    } else {
        &[]
    };

    let mut modifiers = Vec::new();
    let mut type_parts: Vec<String> = Vec::new();
    const MODS: &[&str] = &["static", "inline", "extern", "__inline__", "__inline", "register"];

    for tok in ret_and_mods {
        // Handle tok like "char*" -> contains star
        // Split handling: if tok == "*" standalone
        let t = tok.trim();
        if t.is_empty() {
            continue;
        }
        // Check if it's a pure modifier
        // For tokens like "static", "inline" etc.
        // But also tokens like "static" could be combined? no
        let lower = t.to_lowercase();
        // Use exact match for modifier (c is case-sensitive but we lower for safety)
        if MODS.contains(&t) || MODS.contains(&lower.as_str()) {
            modifiers.push(t.to_string());
        } else {
            // Might be like "const" or "unsigned" -> part of type
            // Keep as is
            type_parts.push(t.to_string());
        }
    }
    // Add stars that were attached to name as part of return type
    for _ in 0..stars_in_name {
        type_parts.push("*".to_string());
    }

    let return_type = if type_parts.is_empty() {
        "int".to_string()
    } else {
        // Join with space and normalize spacing around *
        let joined = type_parts.join(" ");
        // Normalize: ensure "* " handling remains
        joined
            .replace("  ", " ")
            .replace("* *", "**")
            .trim()
            .to_string()
    };

    let visibility = if modifiers.contains(&"static".to_string()) {
        "private".to_string()
    } else {
        "public".to_string()
    };

    let parameters = parse_c_params(&inside_paren);

    (visibility, modifiers, return_type, name, parameters)
}

fn parse_c_params(s: &str) -> Vec<Parameter> {
    let t = s.trim();
    if t.is_empty() || t == "void" {
        return vec![];
    }
    // split by ',' respecting nested parentheses/brackets (for function pointers)
    let parts = split_params_respecting(t);
    let mut out = Vec::new();
    for part in parts {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        if p == "void" {
            continue;
        }
        if p == "..." {
            out.push(Parameter {
                name: "...".to_string(),
                typ: "...".to_string(),
            });
            continue;
        }
        // Handle function pointer param like "int (*callback)(int, int)" -> complex
        // Simplify: if contains "(*" treat whole as type with name extracted inside
        if p.contains("(*") {
            // extract name between (* and )
            if let Some(start) = p.find("(*") {
                let after = &p[start + 2..];
                if let Some(end) = after.find(')') {
                    let fn_name = after[..end].trim().to_string();
                    let clean_name = fn_name
                        .trim_matches(|c| c == '*' || c == ' ' || c == '(' || c == ')')
                        .to_string();
                    let typ = p.to_string();
                    out.push(Parameter {
                        name: if clean_name.is_empty() {
                            "callback".to_string()
                        } else {
                            clean_name
                        },
                        typ,
                    });
                    continue;
                }
            }
            out.push(Parameter {
                name: "arg".to_string(),
                typ: p.to_string(),
            });
            continue;
        }

        let tokens: Vec<&str> = p.split_whitespace().collect();
        if tokens.is_empty() {
            continue;
        }
        if tokens.len() == 1 {
            // e.g., "int" without name (e.g., prototype style) -> treat as type with generated name
            // Or "..." already handled
            // Single token could be "int" -> we need name placeholder
            // If token contains '*', maybe it's "char*" ?
            // We'll generate arg name
            let typ = tokens[0].to_string();
            // If it's just a type without name, assign name like argN
            out.push(Parameter {
                name: format!("arg{}", out.len()),
                typ,
            });
            continue;
        }
        let raw_name = tokens.last().unwrap().to_string();
        // stars prefix
        let star_count = raw_name.chars().take_while(|c| *c == '*').count();
        let mut name = raw_name.trim_start_matches('*').to_string();
        // handle array notation
        if let Some(br) = name.find('[') {
            name = name[..br].to_string();
        }
        name = name.trim_matches(|c| c == '(' || c == ')' || c == '*').to_string();
        // Extract identifier safely: keep only leading alnum_ part
        // Name may be like "buffer[256]" already trimmed
        // Ensure valid
        let mut typ_str = tokens[..tokens.len() - 1].join(" ");
        if star_count > 0 {
            typ_str = format!("{} {}", typ_str, "*".repeat(star_count));
            typ_str = typ_str.trim().to_string();
        }
        // Also handle case where typ_str may have array brackets from earlier? ignore
        if name.is_empty() || !is_valid_c_identifier(&name) {
            name = format!("arg{}", out.len());
        }
        let typ_clean = if typ_str.is_empty() {
            "int".to_string()
        } else {
            typ_str
        };
        out.push(Parameter { name, typ: typ_clean });
    }
    out
}

fn split_params_respecting(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut depth_paren: i32 = 0;
    let mut depth_bracket: i32 = 0;
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
            '[' => {
                depth_bracket += 1;
                cur.push(ch);
            }
            ']' => {
                depth_bracket -= 1;
                cur.push(ch);
            }
            ',' => {
                if depth_paren == 0 && depth_bracket == 0 {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const SAMPLE_C: &str = r#"#include <stdio.h>
#define MAX 10

struct Point {
    int x;
    int y;
};

int add(int a, int b) {
    return a + b;
}

static void helper(void) {
    printf("help\n");
}

const char *get_name(const char *prefix, int id) {
    return prefix;
}

int proto(int x, int y);

void *alloc_mem(size_t size) {
    return 0;
}
"#;

    const SAMPLE_C2: &str = r#"static inline int compute(int x) {
    if (x > 0) { return x; }
    return 0;
}

extern int external_func(const char *s, char *buf);
int external_func(const char *s, char *buf) {
    return 0;
}

"#;

    #[test]
    fn extracts_functions_and_skips_non_defs() {
        let fns = extract(SAMPLE_C, Path::new("sample.c")).unwrap();
        // add, helper, get_name, alloc_mem => 4
        assert_eq!(fns.len(), 4, "found: {:?}", fns.iter().map(|f| f.identity.name.clone()).collect::<Vec<_>>());
        let names: Vec<_> = fns.iter().map(|f| f.identity.name.as_str()).collect();
        assert!(names.contains(&"add"));
        assert!(names.contains(&"helper"));
        assert!(names.contains(&"get_name"));
        assert!(names.contains(&"alloc_mem"));
        // prototype should be ignored
        assert!(!names.contains(&"proto"));

        let add = fns.iter().find(|f| f.identity.name == "add").unwrap();
        assert_eq!(add.signature.parameters.len(), 2);
        assert_eq!(add.signature.return_type.as_deref(), Some("int"));
        assert_eq!(add.identity.qualified_name, "sample::add");
        assert_eq!(add.identity.language, "c");
        assert!(add.source.hash.starts_with("sha256:"));
        assert!(add.source.location.start.byte < add.source.location.end.byte);

        let helper = fns.iter().find(|f| f.identity.name == "helper").unwrap();
        assert_eq!(helper.declaration.visibility, "private");
        assert!(helper.declaration.modifiers.contains(&"static".to_string()));

        let get_name = fns.iter().find(|f| f.identity.name == "get_name").unwrap();
        // return type contains char and *
        assert!(get_name.signature.return_type.as_deref().unwrap().contains("char"));
        assert_eq!(get_name.signature.parameters.len(), 2);

        let alloc = fns.iter().find(|f| f.identity.name == "alloc_mem").unwrap();
        assert!(alloc.signature.return_type.as_deref().unwrap().contains("void"));
        assert_eq!(alloc.signature.parameters[0].name, "size");
    }

    #[test]
    fn modifiers_and_prototype_ignored() {
        let fns = extract(SAMPLE_C2, Path::new("mod.c")).unwrap();
        assert_eq!(fns.len(), 2);
        let comp = fns.iter().find(|f| f.identity.name == "compute").unwrap();
        assert!(comp.declaration.modifiers.contains(&"static".to_string()));
        assert!(comp.declaration.modifiers.contains(&"inline".to_string()));
        assert_eq!(comp.declaration.visibility, "private");
        assert_eq!(comp.identity.qualified_name, "mod::compute");

        let ext = fns.iter().find(|f| f.identity.name == "external_func").unwrap();
        assert!(ext.signature.parameters.len() == 2);
        assert!(ext.source.source_text.contains("external_func"));
    }

    #[test]
    fn location_and_hash() {
        let content = "int foo(int a) {\n return a;\n}\n";
        let fns = extract(content, Path::new("a.c")).unwrap();
        assert_eq!(fns.len(), 1);
        assert_eq!(fns[0].identity.name, "foo");
        assert!(fns[0].source.hash.starts_with("sha256:"));
        assert_eq!(fns[0].source.location.start.line, 1);
        assert!(fns[0].source.location.end.line >= 3);
        assert!(fns[0].source.location.start.byte < fns[0].source.location.end.byte);
        assert_eq!(fns[0].identity.qualified_name, "a::foo");
    }

    #[test]
    fn void_param_means_no_params() {
        let content = "void bar(void) {\n}\n";
        let fns = extract(content, Path::new("b.c")).unwrap();
        assert_eq!(fns.len(), 1);
        assert_eq!(fns[0].signature.parameters.len(), 0);
    }
}
