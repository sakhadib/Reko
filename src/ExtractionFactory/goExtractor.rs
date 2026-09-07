use crate::ir::{IrFunction, Parameter, Position};
use anyhow::Result;
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Debug, Clone)]
struct GoFunctionRaw {
    name: String,
    return_type: Option<String>,
    type_params: Vec<String>,
    receiver: Option<String>,
    parameters: Vec<Parameter>,
    start_line: usize,
    start_col: usize,
    start_byte: usize,
    end_line: usize,
    end_col: usize,
    end_byte: usize,
    source_text: String,
}

/// Public entry: given file content and file path, extract IR for each Go function.
pub fn extract(content: &str, file_path: &Path) -> Result<Vec<IrFunction>> {
    let package = parse_package(content);
    let module = file_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    let file_str = file_path.display().to_string();

    let raws = parse_go_functions(content)?;

    let mut out = Vec::new();
    for r in raws {
        let hash = hash_source(&r.source_text);
        let pkg_name = package.clone().unwrap_or_else(|| module.clone());
        let qualified = match &r.receiver {
            Some(rcv) => format!("{}::{}::{}", pkg_name, rcv, r.name),
            None => format!("{}::{}", pkg_name, r.name),
        };
        let id = qualified.clone();

        let visibility = if r
            .name
            .chars()
            .next()
            .map(|c| c.is_uppercase())
            .unwrap_or(false)
        {
            "public".to_string()
        } else {
            "private".to_string()
        };

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
            visibility,
            Vec::new(),
            r.return_type.clone(),
            r.parameters.clone(),
            Vec::new(),
            package.clone(),
            r.receiver.clone(),
        );

        // Patch for Go specifics
        ir.identity.language = "go".to_string();
        ir.metadata.parser = Some("reko-goExtractor".to_string());
        ir.metadata.parser_version = Some("0.1.0".to_string());
        ir.context.module = Some(module.clone());
        ir.context.package = package.clone();
        ir.context.namespace = package.clone();
        // also store receiver as class/struct_
        ir.context.struct_ = r.receiver.clone();
        if ir.context.class.is_none() {
            ir.context.class = r.receiver.clone();
        }
        ir.source.module = module.clone();
        ir.signature.type_parameters = r.type_params.clone();
        // keep language specific empty
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

fn parse_package(content: &str) -> Option<String> {
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with("//") || t.starts_with("/*") || t.starts_with("*") {
            continue;
        }
        if t.starts_with("package ") {
            let after = t.trim_start_matches("package ").trim();
            // package name is first word before whitespace or comment
            let name = after
                .split_whitespace()
                .next()
                .unwrap_or("")
                .split("//")
                .next()
                .unwrap_or("")
                .trim();
            // remove trailing semicolon if any
            let name = name.trim_end_matches(';').trim();
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    None
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

fn find_open_brace_go(content: &str, start_byte: usize) -> Option<usize> {
    let bytes = content.as_bytes();
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let mut in_string = false;
    let mut in_raw_string = false;
    let mut in_char = false;
    let mut escape = false;
    let mut i = start_byte;
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
        if in_raw_string {
            if b == b'`' {
                in_raw_string = false;
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
        if b == b'"' {
            in_string = true;
            i += 1;
            continue;
        }
        if b == b'`' {
            in_raw_string = true;
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
        i += 1;
    }
    None
}

fn find_matching_brace(content: &str, open_pos: usize) -> Option<usize> {
    let bytes = content.as_bytes();
    let mut depth = 0i32;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let mut in_string = false;
    let mut in_raw_string = false;
    let mut in_char = false;
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
        if in_raw_string {
            if b == b'`' {
                in_raw_string = false;
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
        if b == b'"' {
            in_string = true;
            i += 1;
            continue;
        }
        if b == b'`' {
            in_raw_string = true;
            i += 1;
            continue;
        }
        if b == b'\'' {
            in_char = true;
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

fn parse_go_functions(content: &str) -> Result<Vec<GoFunctionRaw>> {
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
        if trimmed.is_empty()
            || trimmed.starts_with("//")
            || trimmed.starts_with("/*")
            || trimmed.starts_with("*")
        {
            i += 1;
            continue;
        }
        // Detect func declaration: trimmed must start with "func "
        if !trimmed.starts_with("func ") {
            i += 1;
            continue;
        }

        let start_line = i + 1;
        let start_col = first_non_space_col(line) + 1;
        let start_byte = line_starts[i] + first_non_space_col(line);

        // Find open brace
        let brace_byte = match find_open_brace_go(content, start_byte) {
            Some(v) => v,
            None => {
                i += 1;
                continue;
            }
        };

        // Determine end
        let end_byte = match find_matching_brace(content, brace_byte) {
            Some(v) => v,
            None => {
                i += 1;
                continue;
            }
        };
        let (end_line, end_col) = byte_to_line_col(content, &line_starts, end_byte);
        if end_byte < start_byte {
            i += 1;
            continue;
        }
        let source_text = content[start_byte..=end_byte].to_string();
        // signature is from start_byte to brace_byte exclusive
        let sig_text = content[start_byte..brace_byte].trim().to_string();

        // Parse signature
        if let Some((name, receiver, type_params, params, ret_type)) = parse_go_signature(&sig_text) {
            if name.is_empty() {
                i = end_line;
                continue;
            }
            result.push(GoFunctionRaw {
                name,
                return_type: ret_type,
                type_params,
                receiver,
                parameters: params,
                start_line,
                start_col,
                start_byte,
                end_line,
                end_col,
                end_byte,
                source_text,
            });
            i = end_line; // advance to line after function end (1-indexed to 0-indexed)
            continue;
        } else {
            i += 1;
            continue;
        }
    }
    Ok(result)
}

fn parse_go_signature(sig: &str) -> Option<(String, Option<String>, Vec<String>, Vec<Parameter>, Option<String>)> {
    // sig like "func Foo[T any](a int) (int, error)" or "func (r *Receiver) Method(a int) string"
    let mut s = sig.trim().to_string();
    if !s.starts_with("func") {
        return None;
    }
    s = s[4..].trim_start().to_string(); // after func
    if s.is_empty() {
        return None;
    }

    let mut receiver: Option<String> = None;

    // Check for receiver
    if s.starts_with('(') {
        // find matching ')'
        let (recv_inner, remainder) = split_receiver(&s)?;
        // recv_inner like "r *Receiver[T]" or "r Receiver"
        let rcv_type = extract_receiver_base(&recv_inner);
        receiver = rcv_type;
        s = remainder.trim_start().to_string();
        if s.is_empty() {
            return None;
        }
    }

    // Now s should be: Name [generics] (params) return
    // Extract name
    let mut name = String::new();
    for ch in s.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            name.push(ch);
        } else {
            break;
        }
    }
    if name.is_empty() {
        return None;
    }
    let mut rest = s[name.len()..].trim_start().to_string();

    // Generics after name
    let mut type_params: Vec<String> = Vec::new();
    if rest.starts_with('[') {
        if let Some((inner, remaining)) = split_bracket_content(&rest, '[', ']') {
            type_params = parse_type_params(&inner);
            rest = remaining.trim_start().to_string();
        } else {
            return None;
        }
    }

    // Now rest should start with '(' for params
    if !rest.starts_with('(') {
        return None;
    }
    let (params_inner, after_params) = split_paren_content(&rest)?;
    let parameters = parse_go_params(&params_inner);
    let after = after_params.trim().to_string();
    // After may be empty or contain generics? already handled
    // Return type is whatever remains (could be empty, or single type, or parenthesized list)
    let return_type = if after.is_empty() {
        None
    } else {
        // Remove trailing whitespace
        let rt = after.trim().to_string();
        if rt.is_empty() {
            None
        } else {
            Some(rt)
        }
    };

    Some((name, receiver, type_params, parameters, return_type))
}

fn split_receiver(s: &str) -> Option<(String, String)> {
    // s starts with '('
    let mut depth = 0i32;
    let mut end_idx: Option<usize> = None;
    for (idx, ch) in s.char_indices() {
        if ch == '(' {
            depth += 1;
        } else if ch == ')' {
            depth -= 1;
            if depth == 0 {
                end_idx = Some(idx);
                break;
            }
        }
    }
    let end = end_idx?;
    let inner = s[1..end].to_string();
    let remainder = s[end + 1..].to_string();
    Some((inner, remainder))
}

fn split_bracket_content(s: &str, open: char, close: char) -> Option<(String, String)> {
    let mut depth = 0i32;
    let mut start: Option<usize> = None;
    let mut end: Option<usize> = None;
    for (idx, ch) in s.char_indices() {
        if ch == open {
            if depth == 0 {
                start = Some(idx);
            }
            depth += 1;
        } else if ch == close {
            depth -= 1;
            if depth == 0 {
                end = Some(idx);
                break;
            }
            if depth < 0 {
                return None;
            }
        }
    }
    let st = start?;
    let en = end?;
    let inner = s[st + 1..en].to_string();
    let remainder = s[en + 1..].to_string();
    Some((inner, remainder))
}

fn split_paren_content(s: &str) -> Option<(String, String)> {
    // s starts with '(' ; find matching ')'
    let mut depth = 0i32;
    let mut end: Option<usize> = None;
    for (idx, ch) in s.char_indices() {
        if ch == '(' {
            depth += 1;
        } else if ch == ')' {
            depth -= 1;
            if depth == 0 {
                end = Some(idx);
                break;
            }
        }
    }
    let en = end?;
    // find byte index correctly: char_indices gives byte index
    let inner = s[1..en].to_string();
    let remainder = s[en + 1..].to_string();
    Some((inner, remainder))
}

fn extract_receiver_base(inner: &str) -> Option<String> {
    // inner like "r Receiver" or "r *Receiver[T]" or "r *pkg.Receiver"
    let tokens: Vec<&str> = inner.split_whitespace().collect();
    if tokens.is_empty() {
        return None;
    }
    // last token is type
    let mut typ = tokens.last().unwrap().trim().to_string();
    // Remove leading *
    typ = typ.trim_start_matches('*').to_string();
    // If contains '.' take after '.'
    if let Some(dot) = typ.rfind('.') {
        typ = typ[dot + 1..].to_string();
    }
    // Remove generics [ ... ]
    if let Some(br) = typ.find('[') {
        typ = typ[..br].to_string();
    }
    // Remove pointer or other noise
    typ = typ.trim().to_string();
    if typ.is_empty() {
        None
    } else {
        Some(typ)
    }
}

fn parse_type_params(s: &str) -> Vec<String> {
    let t = s.trim();
    if t.is_empty() {
        return Vec::new();
    }
    let parts = split_params_respecting(t);
    parts.into_iter().map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect()
}

fn parse_go_params(s: &str) -> Vec<Parameter> {
    let t = s.trim();
    if t.is_empty() {
        return Vec::new();
    }
    let parts = split_params_respecting(t);
    let mut out: Vec<Parameter> = Vec::new();
    let mut pending: Vec<String> = Vec::new();
    for part in parts {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        // Single token without space -> could be pending name or type-only
        if !p.contains(' ') && !p.contains('\t') {
            pending.push(p.to_string());
            continue;
        } else {
            // has space: split into tokens, last is type
            let mut tokens: Vec<&str> = p.split_whitespace().collect();
            if tokens.is_empty() {
                continue;
            }
            let typ = tokens.pop().unwrap().to_string();
            let mut all_names: Vec<String> = Vec::new();
            for n in pending.drain(..) {
                all_names.push(n);
            }
            for n in tokens {
                all_names.push(n.to_string());
            }
            if all_names.is_empty() {
                out.push(Parameter {
                    name: format!("arg{}", out.len()),
                    typ,
                });
            } else {
                for n in all_names {
                    // handle names that may have comma? already split
                    // remove trailing comma etc
                    let clean = n.trim().trim_matches(',').to_string();
                    if clean.is_empty() {
                        continue;
                    }
                    out.push(Parameter {
                        name: clean,
                        typ: typ.clone(),
                    });
                }
            }
        }
    }
    // remaining pending: treat as type-only params
    for pend in pending {
        out.push(Parameter {
            name: format!("arg{}", out.len()),
            typ: pend,
        });
    }
    out
}

fn split_params_respecting(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut depth_paren: i32 = 0;
    let mut depth_bracket: i32 = 0;
    let mut depth_brace: i32 = 0;
    let mut in_single = false;
    let mut in_double = false;
    let mut in_raw = false;
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
        if in_raw {
            if ch == '`' {
                in_raw = false;
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
            '`' => {
                in_raw = true;
                cur.push(ch);
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const SAMPLE_SIMPLE: &str = r#"package foo

func Hello(name string) string {
    return "hello " + name
}

func Add(a int, b int) int {
    return a + b
}

func NoReturn(a int) {
    println(a)
}
"#;

    const SAMPLE_METHODS: &str = r#"package foo

type Receiver struct {}

func (r Receiver) ValueMethod(x int) (int, error) {
    return x, nil
}

func (r *Receiver) PointerMethod(a string, b int) string {
    return a
}

func (r *Receiver[T]) GenericMethod(a T) T {
    return a
}
"#;

    const SAMPLE_GENERICS: &str = r#"package foo

func GenericFunc[T any](a T) T {
    return a
}

func MultiReturn(a int) (int, error) {
    return a, nil
}

func Variadic(a ...int) int {
    return 0
}

func WithRawString() string {
    s := `raw { } string with brace`
    return s
}
"#;

    const SAMPLE_WITH_COMMENTS: &str = r#"package foo

// line comment with { }
func Foo(a int) int {
    // inside comment {
    s := "string with { brace }"
    /* block comment { } */
    return a
}
"#;

    #[test]
    fn extracts_simple_functions() {
        let fns = extract(SAMPLE_SIMPLE, Path::new("foo.go")).unwrap();
        assert_eq!(fns.len(), 3);
        let hello = fns.iter().find(|f| f.identity.name == "Hello").unwrap();
        assert_eq!(hello.identity.qualified_name, "foo::Hello");
        assert_eq!(hello.signature.parameters.len(), 1);
        assert_eq!(hello.signature.return_type.as_deref(), Some("string"));
        assert_eq!(hello.identity.language, "go");
        assert_eq!(hello.metadata.parser.as_deref(), Some("reko-goExtractor"));
        assert!(hello.source.hash.starts_with("sha256:"));
        assert!(hello.source.location.start.byte < hello.source.location.end.byte);
        assert_eq!(hello.declaration.visibility, "public"); // Hello is exported

        let add = fns.iter().find(|f| f.identity.name == "Add").unwrap();
        assert_eq!(add.signature.parameters.len(), 2);
        assert_eq!(add.signature.return_type.as_deref(), Some("int"));

        let no_ret = fns.iter().find(|f| f.identity.name == "NoReturn").unwrap();
        assert_eq!(no_ret.signature.return_type, None);
        assert_eq!(no_ret.identity.qualified_name, "foo::NoReturn");
    }

    #[test]
    fn extracts_methods_and_generics() {
        let fns = extract(SAMPLE_METHODS, Path::new("foo.go")).unwrap();
        assert_eq!(fns.len(), 3);
        let vm = fns.iter().find(|f| f.identity.name == "ValueMethod").unwrap();
        assert_eq!(vm.identity.qualified_name, "foo::Receiver::ValueMethod");
        assert_eq!(vm.context.struct_.as_deref(), Some("Receiver"));
        // return type "(int, error)"
        assert_eq!(vm.signature.return_type.as_deref(), Some("(int, error)"));
        assert_eq!(vm.signature.parameters.len(), 1);

        let pm = fns.iter().find(|f| f.identity.name == "PointerMethod").unwrap();
        assert_eq!(pm.identity.qualified_name, "foo::Receiver::PointerMethod");
        assert_eq!(pm.signature.parameters.len(), 2);
        assert_eq!(pm.signature.return_type.as_deref(), Some("string"));

        let gm = fns.iter().find(|f| f.identity.name == "GenericMethod").unwrap();
        assert_eq!(gm.identity.qualified_name, "foo::Receiver::GenericMethod");
        // receiver base stripped generic
        assert_eq!(gm.context.struct_.as_deref(), Some("Receiver"));
    }

    #[test]
    fn extracts_generics_multiple_returns_and_raw_string() {
        let fns = extract(SAMPLE_GENERICS, Path::new("foo.go")).unwrap();
        assert_eq!(fns.len(), 4);
        let gf = fns.iter().find(|f| f.identity.name == "GenericFunc").unwrap();
        assert_eq!(gf.identity.qualified_name, "foo::GenericFunc");
        assert_eq!(gf.signature.type_parameters, vec!["T any"]);
        assert_eq!(gf.signature.return_type.as_deref(), Some("T"));
        assert_eq!(gf.signature.parameters.len(), 1);

        let mr = fns.iter().find(|f| f.identity.name == "MultiReturn").unwrap();
        assert_eq!(mr.signature.return_type.as_deref(), Some("(int, error)"));

        let vari = fns.iter().find(|f| f.identity.name == "Variadic").unwrap();
        assert_eq!(vari.signature.parameters[0].typ, "...int");

        let raw = fns.iter().find(|f| f.identity.name == "WithRawString").unwrap();
        assert!(raw.source.source_text.contains("raw { } string"));
        assert_eq!(raw.signature.return_type.as_deref(), Some("string"));
        assert!(raw.source.location.start.byte < raw.source.location.end.byte);
    }

    #[test]
    fn location_and_hash_and_comment_awareness() {
        let fns = extract(SAMPLE_WITH_COMMENTS, Path::new("foo.go")).unwrap();
        assert_eq!(fns.len(), 1);
        assert_eq!(fns[0].identity.name, "Foo");
        assert!(fns[0].source.hash.starts_with("sha256:"));
        assert_eq!(fns[0].source.location.start.line, 4);
        assert!(fns[0].source.location.end.line >= 8);
        // source_text should contain function body not truncated by braces in strings/comments
        assert!(fns[0].source.source_text.contains("return a"));
        assert!(fns[0].source.source_text.contains("string with { brace }"));
    }

    #[test]
    fn extract_to_json_valid() {
        let json = extract_to_json(SAMPLE_SIMPLE, Path::new("foo.go")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(v.is_array());
        assert_eq!(v.as_array().unwrap().len(), 3);
    }

    #[test]
    fn qualified_without_package_fallback_to_module() {
        let content = r#"func Foo(a int) int { return a }"#;
        let fns = extract(content, Path::new("mymod.go")).unwrap();
        assert_eq!(fns.len(), 1);
        assert_eq!(fns[0].identity.qualified_name, "mymod::Foo");
    }
}
