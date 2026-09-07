use crate::ir::{IrFunction, Parameter, Position};
use anyhow::Result;
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Debug, Clone)]
struct JsxFunctionRaw {
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

/// Public entry: given file content and file path, extract IR for each JSX function/component.
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
        ir.identity.language = "javascript".to_string();
        ir.metadata.parser = Some("reko-jsxExtractor".to_string());
        ir.metadata.parser_version = Some("0.1.0".to_string());
        ir.execution.is_async = r.is_async;
        ir.execution.generator = r.is_generator;
        ir.context.module = Some(module.clone());
        ir.context.class = r.class_context.clone();
        ir.source.module = module.clone();
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

// ---------- JSX detection ----------

fn contains_jsx(s: &str) -> bool {
    // Quick heuristic: must contain <Tag and > with either /> or </
    // Use simple scanning to avoid regex overhead per call, but also handle generics vs JSX
    // JSX tags start with < followed by letter, '/', '!', or maybe fragment <>.
    // Generics like <T> appear after identifier and typically not at return position.
    // We distinguish by checking for JSX pattern: <[A-Za-z][\w-]*(\s+[^>]*?)?> or </tag> or < />
    // Also self-closing /> and fragment <>/ </>
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            if i + 1 < bytes.len() {
                let nxt = bytes[i + 1];
                // fragment <> or </>
                if nxt == b'>' {
                    // check there is a closing later? likely JSX
                    return true;
                }
                if nxt == b'/' {
                    // closing tag </tag>
                    if i + 2 < bytes.len() && (bytes[i + 2].is_ascii_alphabetic() || bytes[i+2]==b'>') {
                        return true;
                    }
                } else if nxt.is_ascii_alphabetic() {
                    // opening tag <div or <MyComponent
                    // find closing '>' ahead on same logical JSX
                    // ensure we have '>' within reasonable distance (~500 chars)
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
                            if b == b'\'' { in_single = false; }
                        } else if in_double {
                            if b == b'"' { in_double = false; }
                        } else {
                            if b == b'\'' { in_single = true; }
                            else if b == b'"' { in_double = true; }
                            else if b == b'>' {
                                // found tag close -> JSX
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

// ---------- JSX-aware brace/parens matching ----------

fn skip_jsx_tag(content: &str, start: usize) -> Option<usize> {
    // start at '<', skip to matching '>' handling quotes inside tag
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
            if b == b'\'' { in_single = false; }
        } else if in_double {
            if b == b'"' { in_double = false; }
        } else {
            if b == b'\'' { in_single = true; }
            else if b == b'"' { in_double = true; }
            else if b == b'>' { return Some(i); }
            // handle '{' '}' inside attribute: e.g., attr={value}
            // We simply continue; braces will be skipped as part of tag
            // But need to handle nested braces inside {value} correctly: they may contain quotes/...
            // For simplicity, if we encounter '{', skip to '}' balancing inside attribute
            else if b == b'{' {
                // skip JavaScript expression inside JSX attribute
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
    // find matching '}' for '{' starting at open, handling strings/comments
    let bytes = content.as_bytes();
    if bytes[open] != b'{' { return None; }
    let mut depth = 0;
    let mut in_single = false;
    let mut in_double = false;
    let mut in_template = false;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let mut escape = false;
    for i in open..bytes.len() {
        let b = bytes[i];
        let next = if i + 1 < bytes.len() { Some(bytes[i+1]) } else { None };
        if in_line_comment {
            if b == b'\n' { in_line_comment = false; if depth==0 { } }
            continue;
        }
        if in_block_comment {
            if b == b'*' && next == Some(b'/') { in_block_comment = false; }
            continue;
        }
        if in_single {
            if escape { escape = false; } else if b == b'\\' { escape = true; } else if b == b'\'' { in_single = false; }
            continue;
        }
        if in_double {
            if escape { escape = false; } else if b == b'\\' { escape = true; } else if b == b'"' { in_double = false; }
            continue;
        }
        if in_template {
            if escape { escape = false; } else if b == b'\\' { escape = true; } else if b == b'`' { in_template = false; }
            // template can contain ${} which is another brace level, but we handle as nested braces
            if b == b'{' && !escape { depth += 1; } else if b == b'}' { depth -= 1; if depth==0 { return Some(i); } }
            continue;
        }
        if b == b'/' && next == Some(b'/') { in_line_comment = true; continue; }
        if b == b'/' && next == Some(b'*') { in_block_comment = true; continue; }
        if b == b'\'' { in_single = true; continue; }
        if b == b'"' { in_double = true; continue; }
        if b == b'`' { in_template = true; continue; }
        if b == b'{' { depth += 1; if depth==0 { } }
        else if b == b'}' { depth -= 1; if depth==0 { return Some(i); } }
        // If inside JSX expr we encounter '<', could be nested JSX; skipping tags not needed here
    }
    None
}

fn is_jsx_tag_start(content: &str, pos: usize) -> bool {
    let bytes = content.as_bytes();
    if pos >= bytes.len() || bytes[pos] != b'<' { return false; }
    if pos + 1 >= bytes.len() { return false; }
    let nxt = bytes[pos+1];
    // fragment <> , </>, <div, <MyComp, <! , <? - treat <! as comment/doctype (skip as well)
    if nxt == b'/' || nxt == b'>' || nxt.is_ascii_alphabetic() || nxt == b'!' {
        return true;
    }
    false
}

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
        let next = if i + 1 < bytes.len() { Some(bytes[i+1]) } else { None };
        if in_line_comment {
            if b == b'\n' { in_line_comment = false; }
            i += 1; continue;
        }
        if in_block_comment {
            if b == b'*' && next == Some(b'/') { in_block_comment = false; i += 2; continue; }
            i += 1; continue;
        }
        if in_single {
            if escape { escape = false; } else if b == b'\\' { escape = true; } else if b == b'\'' { in_single = false; }
            i += 1; continue;
        }
        if in_double {
            if escape { escape = false; } else if b == b'\\' { escape = true; } else if b == b'"' { in_double = false; }
            i += 1; continue;
        }
        if in_template {
            if escape { escape = false; } else if b == b'\\' { escape = true; } else if b == b'`' { in_template = false; }
            // handle ${ } inside template: we still count braces? For JSX purpose, inside template expression braces should be counted.
            // Simplify: if we see '{' inside template without escaping, treat as depth?
            // But template content between ` ` is stringified; braces there are not code braces unless ${}
            // We skip detailed handling by just continuing until closing `
            i += 1; continue;
        }
        if b == b'/' && next == Some(b'/') { in_line_comment = true; i += 2; continue; }
        if b == b'/' && next == Some(b'*') { in_block_comment = true; i += 2; continue; }
        if b == b'\'' { in_single = true; i += 1; continue; }
        if b == b'"' { in_double = true; i += 1; continue; }
        if b == b'`' { in_template = true; i += 1; continue; }
        // JSX tag awareness: skip entire tag <...>
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
            if depth == 0 { return Some(i); }
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
        let next = if i + 1 < bytes.len() { Some(bytes[i+1]) } else { None };
        if in_line_comment {
            if b == b'\n' { in_line_comment = false; }
            continue;
        }
        if in_block_comment {
            if b == b'*' && next == Some(b'/') { in_block_comment = false; }
            continue;
        }
        if in_single {
            if escape { escape = false; } else if b == b'\\' { escape = true; } else if b == b'\'' { in_single = false; }
            continue;
        }
        if in_double {
            if escape { escape = false; } else if b == b'\\' { escape = true; } else if b == b'"' { in_double = false; }
            continue;
        }
        if in_template {
            if escape { escape = false; } else if b == b'\\' { escape = true; } else if b == b'`' { in_template = false; }
            continue;
        }
        if b == b'/' && next == Some(b'/') { in_line_comment = true; continue; }
        if b == b'/' && next == Some(b'*') { in_block_comment = true; continue; }
        if b == b'\'' { in_single = true; continue; }
        if b == b'"' { in_double = true; continue; }
        if b == b'`' { in_template = true; continue; }
        // JSX tag not relevant inside parens except for arrow returning JSX: ( <div> ) - but angle brackets there should not be confused.
        // Skip JSX tags to avoid treating <div> as < generic?
        if b == b'<' && is_jsx_tag_start(content, i) {
            if let Some(end) = skip_jsx_tag(content, i) {
                // need to jump: but we are in for loop can't jump; emulate by continuing with index manipulation not easy.
                // Instead, we will handle by marking that '<' is not generic; but we don't need to skip braces inside because paren matching only cares about () not {}.
                // So we can just continue.
                let _ = end;
            }
        }
        if b == b'(' { depth += 1; } else if b == b')' { depth -= 1; if depth==0 { return Some(i); } }
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
        let next = if i + 1 < bytes.len() { Some(bytes[i+1]) } else { None };
        if in_line_comment {
            if b == b'\n' { in_line_comment = false; }
            i += 1; continue;
        }
        if in_block_comment {
            if b == b'*' && next == Some(b'/') { in_block_comment = false; i+=2; continue; }
            i+=1; continue;
        }
        if in_single {
            if escape { escape = false; } else if b == b'\\' { escape = true; } else if b == b'\'' { in_single=false; }
            i+=1; continue;
        }
        if in_double {
            if escape { escape = false; } else if b == b'\\' { escape = true; } else if b == b'"' { in_double=false; }
            i+=1; continue;
        }
        if in_template {
            if escape { escape=false; } else if b == b'\\' { escape=true; } else if b == b'`' { in_template=false; }
            i+=1; continue;
        }
        if b == b'/' && next == Some(b'/') { in_line_comment=true; i+=2; continue; }
        if b == b'/' && next == Some(b'*') { in_block_comment=true; i+=2; continue; }
        if b == b'\'' { in_single=true; i+=1; continue; }
        if b == b'"' { in_double=true; i+=1; continue; }
        if b == b'`' { in_template=true; i+=1; continue; }
        if b == b'<' && is_jsx_tag_start(content, i) {
            if let Some(end) = skip_jsx_tag(content, i) { i = end+1; continue; }
        }
        if b == b'{' { return Some(i); }
        i+=1;
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
        let next = if i + 1 < bytes.len() { Some(bytes[i+1]) } else { None };
        if in_line_comment {
            if b == b'\n' { in_line_comment=false; }
            i+=1; continue;
        }
        if in_block_comment {
            if b == b'*' && next==Some(b'/') { in_block_comment=false; i+=2; continue; }
            i+=1; continue;
        }
        if in_single {
            if escape { escape=false; } else if b==b'\\' { escape=true; } else if b==b'\'' { in_single=false; }
            i+=1; continue;
        }
        if in_double {
            if escape { escape=false; } else if b==b'\\' { escape=true; } else if b==b'"' { in_double=false; }
            i+=1; continue;
        }
        if in_template {
            if escape { escape=false; } else if b==b'\\' { escape=true; } else if b==b'`' { in_template=false; }
            i+=1; continue;
        }
        if b==b'/' && next==Some(b'/') { in_line_comment=true; i+=2; continue; }
        if b==b'/' && next==Some(b'*') { in_block_comment=true; i+=2; continue; }
        if b==b'\'' { in_single=true; i+=1; continue; }
        if b==b'"' { in_double=true; i+=1; continue; }
        if b==b'`' { in_template=true; i+=1; continue; }
        if b==b'<' && is_jsx_tag_start(content, i) {
            if let Some(end)=skip_jsx_tag(content,i){ i=end+1; continue; }
        }
        if b==target { return Some(i); }
        i+=1;
    }
    None
}

// ---------- helpers for position ----------

fn find_next_nonspace(content: &str, start: usize) -> Option<usize> {
    let bytes = content.as_bytes();
    let mut i = start;
    while i < bytes.len() {
        let b = bytes[i];
        if b != b' ' && b != b'\t' && b != b'\n' && b != b'\r' { return Some(i); }
        i+=1;
    }
    None
}

// ---------- class detection ----------

fn parse_classes(content: &str, line_starts: &[usize]) -> Vec<ClassRange> {
    let mut res = Vec::new();
    let lines: Vec<&str> = content.lines().collect();
    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with("*") { continue; }
        if let Some(pos) = line.find("class ") {
            if pos>0 { let prev=line.as_bytes()[pos-1]; if (prev as char).is_alphanumeric() || prev==b'_' || prev==b'$' { continue; } }
            let after_start = pos+6;
            let after = &line[after_start..];
            let after_trim = after.trim_start();
            let name: String = after_trim.chars().take_while(|c| c.is_alphanumeric()||*c=='_'||*c=='$').collect();
            if name.is_empty() { continue; }
            let start_byte = line_starts[idx]+pos;
            if let Some(open)=find_next_open_brace(content,start_byte){
                if let Some(close)=find_matching_brace(content,open){
                    res.push(ClassRange{name,start:open,end:close});
                }
            }
        }
    }
    res.sort_by_key(|c| c.start);
    res
}

fn find_enclosing_class(byte: usize, classes: &[ClassRange]) -> Option<String> {
    let mut best: Option<&ClassRange>=None;
    for c in classes {
        if byte > c.start && byte < c.end {
            if let Some(b)=best { if (c.end-c.start) < (b.end-b.start) { best=Some(c);} } else { best=Some(c);}
        }
    }
    best.map(|c| c.name.clone())
}

// ---------- params ----------

fn parse_params_js(s: &str) -> Vec<Parameter> {
    if s.trim().is_empty() { return vec![]; }
    let parts = split_params_js(s);
    let mut out=Vec::new();
    for part in parts {
        let p=part.trim();
        if p.is_empty(){continue;}
        let without_default = p.split('=').next().unwrap_or(p).trim();
        let name_raw = if without_default.starts_with('{') || without_default.starts_with('[') {
            without_default.to_string()
        } else {
            let before_colon = without_default.split(':').next().unwrap_or(without_default).trim();
            before_colon.to_string()
        };
        let mut name_clean = name_raw.trim().to_string();
        if name_clean.starts_with("..."){ name_clean=name_clean[3..].trim().to_string(); }
        if name_clean.is_empty(){continue;}
        if !name_clean.starts_with('{') && !name_clean.starts_with('[') && name_clean.contains(' ') {
            if let Some(first)=name_clean.split_whitespace().next(){ name_clean=first.to_string(); }
        }
        if name_clean.is_empty()||name_clean=="..." {continue;}
        out.push(Parameter{name:name_clean, typ:"Any".to_string()});
    }
    out
}

fn split_params_js(s: &str) -> Vec<String> {
    let mut parts: Vec<String>=Vec::new();
    let mut current=String::new();
    let mut depth_paren: i32=0;
    let mut depth_bracket: i32=0;
    let mut depth_brace: i32=0;
    let mut in_single=false;
    let mut in_double=false;
    let mut in_template=false;
    let mut escape=false;
    for ch in s.chars(){
        if escape{current.push(ch);escape=false;continue;}
        if ch=='\\'{escape=true;current.push(ch);continue;}
        if in_single{ if ch=='\''{in_single=false;} current.push(ch); continue; }
        if in_double{ if ch=='"'{in_double=false;} current.push(ch); continue; }
        if in_template{ if ch=='`'{in_template=false;} current.push(ch); continue; }
        match ch{
            '\''=>{in_single=true; current.push(ch);},
            '"' =>{in_double=true; current.push(ch);},
            '`' =>{in_template=true; current.push(ch);},
            '(' =>{depth_paren+=1; current.push(ch);},
            ')' =>{depth_paren-=1; current.push(ch);},
            '[' =>{depth_bracket+=1; current.push(ch);},
            ']' =>{depth_bracket-=1; current.push(ch);},
            '{' =>{depth_brace+=1; current.push(ch);},
            '}' =>{depth_brace-=1; current.push(ch);},
            ',' =>{ if depth_paren==0 && depth_bracket==0 && depth_brace==0 { parts.push(current.trim().to_string()); current.clear(); } else { current.push(ch); } },
            _=>current.push(ch),
        }
    }
    if !current.trim().is_empty(){ parts.push(current.trim().to_string()); }
    parts
}

// ---------- main parse ----------

fn parse_functions(content: &str) -> Result<Vec<JsxFunctionRaw>> {
    let mut line_starts: Vec<usize>=vec![0];
    for (i,b) in content.bytes().enumerate(){ if b==b'\n'{ line_starts.push(i+1); } }
    let lines: Vec<&str>=content.lines().collect();
    if content.is_empty(){ return Ok(Vec::new()); }
    let classes=parse_classes(content,&line_starts);
    let mut result=Vec::new();
    let mut visited: std::collections::HashSet<usize>=std::collections::HashSet::new();
    let mut i=0usize;
    while i < lines.len(){
        let line=lines[i];
        let trimmed=line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with("*") || trimmed.starts_with("import ") { i+=1; continue; }
        let start_byte_candidate = line_starts[i]+first_non_space_col(line);
        let enclosing = find_enclosing_class(start_byte_candidate, &classes);
        // const function expr
        if try_is_const_function_expr(trimmed){
            if let Some((raw, _end_idx)) = try_parse_const_function_expr(content,&line_starts,&lines,i,&classes){
                if !visited.contains(&raw.start_byte) && contains_jsx(&raw.source_text){
                    visited.insert(raw.start_byte);
                    let next_i = raw.end_line;
                    result.push(raw);
                    i = next_i;
                    continue;
                } else if visited.contains(&raw.start_byte) {
                    i = _end_idx+1; continue;
                } else {
                    // not JSX, skip but advance to end to avoid re-parsing inner
                    i = raw.end_line; continue;
                }
            }
        }
        // arrow
        if trimmed.contains("=>") && (trimmed.contains("const ") || trimmed.contains("let ") || trimmed.contains("var ") || trimmed.contains('=')){
            if let Some((raw,_)) = try_parse_arrow(content,&line_starts,&lines,i,&classes){
                if !visited.contains(&raw.start_byte) && contains_jsx(&raw.source_text){
                    visited.insert(raw.start_byte);
                    let next_i = raw.end_line;
                    result.push(raw);
                    i = next_i;
                    continue;
                } else if !visited.contains(&raw.start_byte) && !contains_jsx(&raw.source_text) {
                    // jsx check failed: still need to skip this arrow to avoid infinite loop
                    // But arrow without JSX should not be extracted; advance
                    let next_i = raw.end_line;
                    // avoid re-adding, just skip
                    i = next_i;
                    continue;
                }
            }
        }
        // function decl
        if trimmed.contains("function"){
            if let Some((raw,_)) = try_parse_function_decl(content,&line_starts,&lines,i,&classes){
                if !visited.contains(&raw.start_byte) && contains_jsx(&raw.source_text){
                    visited.insert(raw.start_byte);
                    let next_i = raw.end_line;
                    result.push(raw);
                    i = next_i;
                    continue;
                } else if !visited.contains(&raw.start_byte) && !contains_jsx(&raw.source_text){
                    // skip non-jsx function
                    let next_i = raw.end_line;
                    i = next_i;
                    continue;
                }
            }
        }
        // class method
        if enclosing.is_some(){
            if let Some((raw,_))=try_parse_class_method(content,&line_starts,&lines,i,&classes){
                if !visited.contains(&raw.start_byte) && contains_jsx(&raw.source_text){
                    visited.insert(raw.start_byte);
                    let next_i = raw.end_line;
                    result.push(raw);
                    i = next_i;
                    continue;
                } else if !visited.contains(&raw.start_byte) && !contains_jsx(&raw.source_text){
                    // still skip to avoid re-parse but not push
                    let next_i = raw.end_line;
                    i = next_i;
                    continue;
                }
            }
        }
        i+=1;
    }
    result.sort_by_key(|r| r.start_byte);
    Ok(result)
}

fn try_is_const_function_expr(trimmed: &str)->bool{
    if let Some(eq)=trimmed.find('='){
        if let Some(func)=trimmed.find("function"){ return eq<func; }
    }
    false
}

fn try_parse_const_function_expr(content:&str, line_starts:&[usize], lines:&[&str], i:usize, classes:&[ClassRange]) -> Option<(JsxFunctionRaw, usize)>{
    let line=lines[i];
    let eq_pos=line.find('=')?;
    let func_pos=line.find("function")?;
    if eq_pos>func_pos{ return None; }
    let before_eq=&line[..eq_pos];
    let name_candidate=before_eq.split_whitespace().last()?.trim_matches(|c: char| c==';'||c==',').to_string();
    let invalid=["const","let","var","export","default","="];
    if invalid.contains(&name_candidate.as_str()) || name_candidate.is_empty(){ return None; }
    if !name_candidate.chars().next().map(|c| c.is_alphabetic()||c=='_'||c=='$').unwrap_or(false){return None;}
    let name_ident: String = name_candidate.chars().take_while(|c| c.is_alphanumeric()||*c=='_'||*c=='$').collect();
    if name_ident.is_empty(){return None;}
    let mut is_async = line[..func_pos].contains("async");
    let after_func=&line[func_pos+8..];
    let after_trim=after_func.trim_start();
    let is_generator=after_trim.starts_with('*');
    let func_byte_start=line_starts[i]+func_pos;
    let open_paren=find_next_char_aware(content,func_byte_start,b'(')?;
    let close_paren=find_matching_paren(content,open_paren)?;
    let params_str=if close_paren>open_paren+1{ content[open_paren+1..close_paren].to_string()} else {String::new()};
    let params=parse_params_js(&params_str);
    let open_brace=find_next_open_brace(content,close_paren+1)?;
    let close_brace=find_matching_brace(content,open_brace)?;
    let start_byte=line_starts[i]+first_non_space_col(line);
    let start_line=i+1;
    let start_col=first_non_space_col(line)+1;
    let (end_line,end_col)=byte_to_line_col(content,line_starts,close_brace);
    let source_text=content[start_byte..=close_brace].to_string();
    let class_ctx=find_enclosing_class(start_byte,classes);
    let mut modifiers=Vec::new();
    if is_async{modifiers.push("async".to_string());}
    if is_generator{modifiers.push("generator".to_string());}
    if line.contains("export"){modifiers.push("export".to_string());}
    if line.contains("default"){modifiers.push("default".to_string());}
    if find_is_static(line){modifiers.push("static".to_string());}
    let raw=JsxFunctionRaw{name:name_ident,visibility:"public".to_string(),modifiers,parameters:params,is_async,is_generator,class_context:class_ctx,start_line,start_col,start_byte,end_line,end_col,end_byte:close_brace,source_text};
    let end_idx=end_line-1;
    Some((raw,end_idx))
}

fn try_parse_arrow(content:&str,line_starts:&[usize],lines:&[&str],i:usize,classes:&[ClassRange]) -> Option<(JsxFunctionRaw, usize)>{
    let line=lines[i];
    let eq_pos=line.find('=')?;
    let arrow_rel=line.find("=>")?;
    if eq_pos>arrow_rel{return None;}
    let before_eq=&line[..eq_pos];
    let name_candidate=before_eq.split_whitespace().last()?.to_string();
    let name_ident: String = name_candidate.chars().take_while(|c| c.is_alphanumeric()||*c=='_'||*c=='$').collect();
    if name_ident.is_empty(){return None;}
    if ["const","let","var","export","default"].contains(&name_ident.as_str()){return None;}
    let before_arrow=&line[..arrow_rel];
    let is_async=before_arrow.contains("async");
    let between=&line[eq_pos+1..arrow_rel];
    let between_trim=between.trim().trim_start_matches("async").trim();
    let params_str: String;
    let params: Vec<Parameter>;
    // Determine params
    if between_trim.starts_with('('){
        let search_start=line_starts[i]+eq_pos;
        if let Some(open)=find_next_char_aware(content,search_start,b'('){
            if open < line_starts[i]+arrow_rel {
                if let Some(close)=find_matching_paren(content,open){
                    let arrow_byte=line_starts[i]+arrow_rel;
                    if close < arrow_byte{
                        params_str=content[open+1..close].to_string();
                    } else { params_str=String::new(); }
                } else { params_str=String::new();}
            } else { params_str=String::new();}
        } else { params_str=String::new();}
        params=if params_str.trim().is_empty(){Vec::new()}else{parse_params_js(&params_str)};
    } else {
        let single=between_trim.trim();
        let clean=single.trim().to_string();
        if clean.is_empty()||clean.contains(' ')||clean.contains(','){
            params=Vec::new();
        } else {
            params=parse_params_js(&clean);
        }
    }

    let arrow_byte=line_starts[i]+arrow_rel;
    // After =>, check what follows
    let after_arrow_start = arrow_byte+2;
    let next_non = find_next_nonspace(content, after_arrow_start)?;
    let bytes = content.as_bytes();
    let start_byte = line_starts[i]+first_non_space_col(line);
    let start_line = i+1;
    let start_col = first_non_space_col(line)+1;
    let class_ctx = find_enclosing_class(start_byte, classes);
    let mut modifiers=Vec::new();
    if is_async{modifiers.push("async".to_string());}
    if line.contains("export"){modifiers.push("export".to_string());}
    if line.contains("default"){modifiers.push("default".to_string());}

    // case 1: block body { ... }
    if bytes[next_non]==b'{' {
        let open_brace = next_non;
        let close_brace = find_matching_brace(content, open_brace)?;
        let (end_line,end_col)=byte_to_line_col(content,line_starts,close_brace);
        let source_text=content[start_byte..=close_brace].to_string();
        let raw=JsxFunctionRaw{name:name_ident,visibility:"public".to_string(),modifiers,parameters:params,is_async,is_generator:false,class_context:class_ctx,start_line,start_col,start_byte,end_line,end_col,end_byte:close_brace,source_text};
        let end_idx=end_line-1;
        return Some((raw,end_idx));
    }
    // case 2: paren body ( <div> ... )
    if bytes[next_non]==b'(' {
        let open_paren = next_non;
        if let Some(close_paren)=find_matching_paren(content,open_paren){
            let (end_line,end_col)=byte_to_line_col(content,line_starts,close_paren);
            // Include trailing ; or whitespace? Source includes up to close_paren
            let mut end_byte = close_paren;
            // Optionally include semicolon if present immediately after
            if end_byte+1 < bytes.len() && bytes[end_byte+1]==b';'{
                end_byte+=1;
                // recompute end_line/col for that byte
                let (el, ec)=byte_to_line_col(content,line_starts,end_byte);
                let source_text=content[start_byte..=end_byte].to_string();
                let raw=JsxFunctionRaw{name:name_ident,visibility:"public".to_string(),modifiers,parameters:params,is_async,is_generator:false,class_context:class_ctx,start_line,start_col,start_byte,end_line:el,end_col:ec,end_byte,source_text};
                return Some((raw, el-1));
            }
            let source_text=content[start_byte..=close_paren].to_string();
            let raw=JsxFunctionRaw{name:name_ident,visibility:"public".to_string(),modifiers,parameters:params,is_async,is_generator:false,class_context:class_ctx,start_line,start_col,start_byte,end_line,end_col,end_byte:close_paren,source_text};
            return Some((raw,end_line-1));
        }
    }
    // case 3: direct JSX without braces/parens, e.g., () => <div>...</div>
    if bytes[next_non]==b'<' {
        // Find end of JSX expression: look for line end or ; or next newline beyond JSX
        // We scan until we find a line that ends JSX and not inside tag
        // Simplest: find next ';' or newline that is not inside JSX tag
        // We will expand to include the JSX tag(s) up to end of line or semicolon
        // Search for next '\n' after arrow and check if contains_jsx: take until next '\n' then expand for multiline JSX
        // For multiline, we need to find where JSX ends: find matching closing tag or self-close.
        // Simplify: find next line break after JSX and then scan for closing tag.
        // If we can't determine, take until next ';' or next line start with new declaration.
        let mut end = next_non;
        // heuristic: scan forward handling JSX tags and braces until we hit ';' or newline where next line is not part of JSX
        // We'll look for the outermost JSX element's closing tag.
        // Let's try to find balanced JSX: find first '>' after '<', then find matching closing tag.
        // Instead of full parser, we will just extend to include up to the line's semicolon or until we have balanced JSX tags.

        // Find semicolon after JSX start, awareness of strings/comments
        let mut scan = next_non;
        let mut in_single=false;
        let mut in_double=false;
        let mut in_template=false;
        let mut in_line_comment=false;
        let mut in_block_comment=false;
        let mut escape=false;
        let mut jsx_depth: i32=0;
        let mut seen_open=false;
        while scan < bytes.len(){
            let b=bytes[scan];
            let nxt = if scan+1 < bytes.len(){Some(bytes[scan+1])} else {None};
            if in_line_comment{
                if b==b'\n'{ in_line_comment=false; if jsx_depth==0 && seen_open { break; } }
                scan+=1; continue;
            }
            if in_block_comment{
                if b==b'*' && nxt==Some(b'/'){ in_block_comment=false; scan+=2; continue; }
                scan+=1; continue;
            }
            if in_single{
                if escape{escape=false;} else if b==b'\\'{escape=true;} else if b==b'\''{in_single=false;}
                scan+=1; continue;
            }
            if in_double{
                if escape{escape=false;} else if b==b'\\'{escape=true;} else if b==b'"'{in_double=false;}
                scan+=1; continue;
            }
            if in_template{
                if escape{escape=false;} else if b==b'\\'{escape=true;} else if b==b'`'{in_template=false;}
                scan+=1; continue;
            }
            if b==b'/' && nxt==Some(b'/'){ in_line_comment=true; scan+=2; continue; }
            if b==b'/' && nxt==Some(b'*'){ in_block_comment=true; scan+=2; continue; }
            if b==b'\''{ in_single=true; scan+=1; continue; }
            if b==b'"'{ in_double=true; scan+=1; continue; }
            if b==b'`'{ in_template=true; scan+=1; continue; }
            if b==b'{'{
                // JSX expr inside text: skip to matching }
                if let Some(e)=find_jsx_expr_end(content, scan){
                    scan=e+1; continue;
                }
            }
            if b==b'<'{
                if is_jsx_tag_start(content, scan){
                    // check if closing or opening
                    let is_closing = scan+1 < bytes.len() && bytes[scan+1]==b'/';
                    let is_self_close_check = false;
                    if let Some(tag_end)=skip_jsx_tag(content, scan){
                        // determine if self-closing: ends with "/>"
                        let tag_str = &content[scan..=tag_end];
                        let is_self_close = tag_str.trim_end().ends_with("/>");
                        if is_self_close{
                            if jsx_depth==0{ seen_open=true; }
                            // no depth change, but if at top level and self-close is the only element, we can finish
                            if jsx_depth==0{
                                scan=tag_end+1;
                                // check for trailing semicolon
                                end=scan-1;
                                // if after tag there's nothing else on line, we can stop
                                // look ahead for ';' then break
                                if scan < bytes.len() && bytes[scan]==b';'{ end=scan; scan+=1; }
                                break;
                            }
                        } else if is_closing{
                            jsx_depth-=1;
                            if jsx_depth<0{ jsx_depth=0; }
                            if jsx_depth==0 && seen_open{
                                scan=tag_end+1;
                                end=scan-1;
                                // include trailing ; if present
                                if scan < bytes.len() && bytes[scan]==b';'{ end=scan; scan+=1; }
                                break;
                            }
                        } else {
                            // opening tag
                            if jsx_depth==0{ seen_open=true; }
                            jsx_depth+=1;
                            // fragment <> counts as well
                        }
                        scan=tag_end+1;
                        continue;
                    }
                }
            }
            if b==b';' && jsx_depth==0 && seen_open{
                end=scan;
                break;
            }
            if b==b'\n' && jsx_depth==0 && seen_open{
                // Potential end of single-line JSX arrow without semicolon
                // Check if next line starts with new declaration (const, let, function, class, export, import)
                // We'll peek next line's trimmed content
                let remaining = &content[scan+1..];
                if let Some(nl) = remaining.lines().next(){
                    let t = nl.trim();
                    if t.starts_with("const ")||t.starts_with("let ")||t.starts_with("var ")||t.starts_with("function ")||t.starts_with("class ")||t.starts_with("export ")||t.starts_with("import ")||t.starts_with("}"){
                        end=scan-1;
                        // trim trailing newline not included
                        break;
                    }
                }
                // otherwise continue multiline JSX
            }
            scan+=1;
            end=scan;
            if scan > next_non + 800 && jsx_depth==0 && seen_open{ break; }
            if scan > next_non + 4000 { break; }
        }
        // ensure end is at least covering one JSX tag
        if end <= next_non { end = next_non; }
        // Move end to include up to end minus maybe newline
        while end>next_non && (bytes[end]==b'\n'||bytes[end]==b'\r'){ end-=1; }
        let actual_end = end;
        let (end_line,end_col)=byte_to_line_col(content,line_starts,actual_end);
        let source_text=content[start_byte..=actual_end].to_string();
        let raw=JsxFunctionRaw{name:name_ident,visibility:"public".to_string(),modifiers,parameters:params,is_async,is_generator:false,class_context:class_ctx,start_line,start_col,start_byte,end_line,end_col,end_byte:actual_end,source_text};
        return Some((raw,end_line-1));
    }
    // fallback: treat as block-less arrow with expression ending at ';' or newline
    let semi = find_next_char_aware(content, arrow_byte, b';');
    let end_byte = if let Some(s)=semi{ s } else {
        // take until end of line
        let line_end = line_starts[i] + lines[i].len();
        if line_end > 0 { line_end-1 } else { arrow_byte }
    };
    let (end_line,end_col)=byte_to_line_col(content,line_starts,end_byte);
    let source_text=content[start_byte..=end_byte].to_string();
    // Only return if it contains JSX (will be filtered by caller)
    let raw=JsxFunctionRaw{name:name_ident,visibility:"public".to_string(),modifiers,parameters:params,is_async,is_generator:false,class_context:class_ctx,start_line,start_col,start_byte,end_line,end_col,end_byte,source_text};
    Some((raw,end_line-1))
}

fn try_parse_function_decl(content:&str,line_starts:&[usize],lines:&[&str],i:usize,classes:&[ClassRange]) -> Option<(JsxFunctionRaw, usize)>{
    let line=lines[i];
    let func_pos=line.find("function")?;
    if let Some(eq)=line.find('='){ if eq < func_pos { return None; } }
    let before_func=&line[..func_pos];
    let after=&line[func_pos+8..];
    let after_trim=after.trim_start();
    let is_generator=after_trim.starts_with('*');
    let after_name_start=if is_generator{ after_trim[1..].trim_start()} else {after_trim};
    let name: String = after_name_start.chars().take_while(|c| c.is_alphanumeric()||*c=='_'||*c=='$').collect();
    if name.is_empty(){
        // handle export default function() anonymous
        if before_func.contains("default") || line.contains("export default"){
            let func_byte_start=line_starts[i]+func_pos;
            let open_paren=find_next_char_aware(content,func_byte_start,b'(')?;
            let close_paren=find_matching_paren(content,open_paren)?;
            let params_str=if close_paren>open_paren+1{content[open_paren+1..close_paren].to_string()}else{String::new()};
            let params=parse_params_js(&params_str);
            let open_brace=find_next_open_brace(content,close_paren+1)?;
            let close_brace=find_matching_brace(content,open_brace)?;
            let is_async=before_func.contains("async");
            let start_byte=line_starts[i]+first_non_space_col(line);
            let start_line=i+1;
            let start_col=first_non_space_col(line)+1;
            let (end_line,end_col)=byte_to_line_col(content,line_starts,close_brace);
            let source_text=content[start_byte..=close_brace].to_string();
            let class_ctx=find_enclosing_class(start_byte,classes);
            let mut modifiers=Vec::new();
            if is_async{modifiers.push("async".to_string());}
            if is_generator{modifiers.push("generator".to_string());}
            if before_func.contains("export")||line.contains("export"){modifiers.push("export".to_string());}
            if before_func.contains("default")||line.contains("default"){modifiers.push("default".to_string());}
            let raw=JsxFunctionRaw{name:"default".to_string(),visibility:"public".to_string(),modifiers,parameters:params,is_async,is_generator,class_context:class_ctx,start_line,start_col,start_byte,end_line,end_col,end_byte:close_brace,source_text};
            return Some((raw,end_line-1));
        }
        return None;
    }
    let is_async=before_func.contains("async");
    let func_byte_start=line_starts[i]+func_pos;
    let open_paren=find_next_char_aware(content,func_byte_start,b'(')?;
    let close_paren=find_matching_paren(content,open_paren)?;
    let params_str=if close_paren>open_paren+1{content[open_paren+1..close_paren].to_string()}else{String::new()};
    let params=parse_params_js(&params_str);
    let open_brace=find_next_open_brace(content,close_paren+1)?;
    let close_brace=find_matching_brace(content,open_brace)?;
    let start_byte=line_starts[i]+first_non_space_col(line);
    let start_line=i+1;
    let start_col=first_non_space_col(line)+1;
    let (end_line,end_col)=byte_to_line_col(content,line_starts,close_brace);
    let source_text=content[start_byte..=close_brace].to_string();
    let class_ctx=find_enclosing_class(start_byte,classes);
    let mut modifiers=Vec::new();
    if is_async{modifiers.push("async".to_string());}
    if is_generator{modifiers.push("generator".to_string());}
    if before_func.contains("export")||line.contains("export"){modifiers.push("export".to_string());}
    if before_func.contains("default")||line.contains("default"){modifiers.push("default".to_string());}
    let raw=JsxFunctionRaw{name,visibility:"public".to_string(),modifiers,parameters:params,is_async,is_generator,class_context:class_ctx,start_line,start_col,start_byte,end_line,end_col,end_byte:close_brace,source_text};
    Some((raw,end_line-1))
}

fn find_is_static(line:&str)->bool{ line.contains("static ") }

fn try_parse_class_method(content:&str,line_starts:&[usize],lines:&[&str],i:usize,classes:&[ClassRange]) -> Option<(JsxFunctionRaw, usize)>{
    let line=lines[i];
    let trimmed=line.trim();
    if trimmed.contains("function")||trimmed.contains("=>")||trimmed.contains("class "){return None;}
    if !trimmed.contains('(')||!trimmed.contains(')'){return None;}
    if trimmed.contains('='){return None;}
    let mut rest=trimmed;
    let mut is_static=false;
    let mut is_async=false;
    let mut is_generator=false;
    if rest.starts_with("static "){is_static=true; rest=rest[7..].trim_start();}
    if rest.starts_with("async "){is_async=true; rest=rest[6..].trim_start();}
    if !is_static && rest.starts_with("static "){is_static=true; rest=rest[7..].trim_start();}
    if !is_async && rest.starts_with("async "){is_async=true; rest=rest[6..].trim_start();}
    if rest.starts_with('*'){is_generator=true; rest=rest[1..].trim_start();}
    let mut is_getter=false;
    let mut is_setter=false;
    if rest.starts_with("get "){is_getter=true; rest=rest[4..].trim_start();}
    else if rest.starts_with("set "){is_setter=true; rest=rest[4..].trim_start();}
    let name: String = rest.chars().take_while(|c| c.is_alphanumeric()||*c=='_'||*c=='$').collect();
    if name.is_empty(){return None;}
    let keywords=["if","for","while","switch","catch","else","return","const","let","var","import","export","default","function","class"];
    if keywords.contains(&name.as_str()){return None;}
    let after_name=&rest[name.len()..];
    if !after_name.trim_start().starts_with('('){return None;}
    let name_idx=line.find(&name)?;
    let name_byte=line_starts[i]+name_idx;
    let open_paren=find_next_char_aware(content,name_byte,b'(')?;
    let close_paren=find_matching_paren(content,open_paren)?;
    let params_str=if close_paren>open_paren+1{content[open_paren+1..close_paren].to_string()}else{String::new()};
    let params=parse_params_js(&params_str);
    let open_brace=find_next_open_brace(content,close_paren+1)?;
    let close_brace=find_matching_brace(content,open_brace)?;
    let start_byte=line_starts[i]+first_non_space_col(line);
    let start_line=i+1;
    let start_col=first_non_space_col(line)+1;
    let (end_line,end_col)=byte_to_line_col(content,line_starts,close_brace);
    let source_text=content[start_byte..=close_brace].to_string();
    let class_ctx=find_enclosing_class(start_byte,classes);
    let mut modifiers=Vec::new();
    if is_static{modifiers.push("static".to_string());}
    if is_async{modifiers.push("async".to_string());}
    if is_generator{modifiers.push("generator".to_string());}
    if is_getter{modifiers.push("get".to_string());}
    if is_setter{modifiers.push("set".to_string());}
    let raw=JsxFunctionRaw{name,visibility:"public".to_string(),modifiers,parameters:params,is_async,is_generator,class_context:class_ctx,start_line,start_col,start_byte,end_line,end_col,end_byte:close_brace,source_text};
    Some((raw,end_line-1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const FUNCTION_COMPONENT: &str = r#"function Greeting(props) {
    return <div className="greeting">Hello {props.name}</div>;
}
export default function App() {
    return <Greeting name="World" />;
}
"#;

    const ARROW_COMPONENTS: &str = r#"const Card = (props) => {
    return <div attr={props.value}><span>{props.title}</span></div>;
};
const Inline = () => (<div>inline</div>);
const Direct = props => <div>{props.x}</div>;
"#;

    const CLASS_COMPONENT: &str = r#"class MyComponent extends React.Component {
    render() {
        return <div>{this.props.children}</div>;
    }
    customMethod(a, b) {
        return <span>{a + b}</span>;
    }
    helper() {
        return 42;
    }
}
"#;

    #[test]
    fn extracts_function_component() {
        let fns = extract(FUNCTION_COMPONENT, Path::new("comp.jsx")).unwrap();
        assert_eq!(fns.len(), 2);
        let greet = fns.iter().find(|f| f.identity.name == "Greeting").unwrap();
        assert_eq!(greet.identity.qualified_name, "comp::Greeting");
        assert_eq!(greet.identity.language, "javascript");
        assert_eq!(greet.metadata.parser.as_deref(), Some("reko-jsxExtractor"));
        assert!(greet.source.hash.starts_with("sha256:"));
        assert!(greet.source.source_text.contains("<div"));
        assert!(greet.source.source_text.contains("{props.name}"));
        // angle brackets not confused: should contain JSX but not generic
        assert_eq!(greet.signature.parameters.len(), 1);
        assert_eq!(greet.signature.parameters[0].name, "props");
        let app = fns.iter().find(|f| f.identity.name == "App").unwrap();
        assert!(app.declaration.modifiers.contains(&"export".to_string()));
        assert!(app.declaration.modifiers.contains(&"default".to_string()));
        assert!(app.source.source_text.contains("<Greeting"));
    }

    #[test]
    fn extracts_arrow_components() {
        let fns = extract(ARROW_COMPONENTS, Path::new("arrow.jsx")).unwrap();
        // Card, Inline, Direct => 3
        assert_eq!(fns.len(), 3);
        let card = fns.iter().find(|f| f.identity.name == "Card").unwrap();
        assert_eq!(card.identity.qualified_name, "arrow::Card");
        assert_eq!(card.signature.parameters.len(), 1);
        assert!(card.source.source_text.contains("<div attr={props.value}>"));
        // brace matching with JSX { } inside attribute vs block
        assert!(card.source.source_text.contains("<span>"));

        let inline = fns.iter().find(|f| f.identity.name == "Inline").unwrap();
        assert!(inline.source.source_text.contains("<div>inline</div>"));
        // arrow with paren () => (<div>) should be captured
        assert!(inline.source.source_text.contains("Inline"));

        let direct = fns.iter().find(|f| f.identity.name == "Direct").unwrap();
        assert!(direct.source.source_text.contains("<div>{props.x}</div>"));
    }

    #[test]
    fn extracts_class_methods_with_jsx() {
        let fns = extract(CLASS_COMPONENT, Path::new("my.jsx")).unwrap();
        // only render and customMethod have JSX, helper should be filtered out
        assert_eq!(fns.len(), 2);
        let render = fns.iter().find(|f| f.identity.name == "render").unwrap();
        assert_eq!(render.identity.qualified_name, "my::MyComponent::render");
        assert_eq!(render.context.class.as_deref(), Some("MyComponent"));
        assert!(render.source.source_text.contains("<div>{this.props.children}</div>"));
        assert!(render.source.source_text.contains("return"));

        let custom = fns.iter().find(|f| f.identity.name == "customMethod").unwrap();
        assert_eq!(custom.identity.qualified_name, "my::MyComponent::customMethod");
        assert_eq!(custom.signature.parameters.len(), 2);
        assert!(custom.source.source_text.contains("<span>{a + b}</span>"));
        // helper without JSX should not be present
        assert!(fns.iter().find(|f| f.identity.name == "helper").is_none());
    }

    #[test]
    fn jsx_aware_brace_matching() {
        let content = r#"function WithExpr(props) {
    // comment with <Tag> and }
    let tmpl = `template { not }`;
    let s = "} not a brace {";
    /* block } <div> */
    return <div attr={props.val} data-x="{">content {props.children}</div>;
}
"#;
        let fns = extract(content, Path::new("expr.jsx")).unwrap();
        assert_eq!(fns.len(), 1);
        assert_eq!(fns[0].identity.name, "WithExpr");
        assert!(fns[0].source.source_text.contains(r#"attr={props.val}"#));
        assert!(fns[0].source.source_text.contains("content {props.children}"));
        // ensure location correct and hash
        assert!(fns[0].source.location.start.line == 1);
        assert!(fns[0].source.location.end.line >= 6);
    }

    #[test]
    fn ignores_non_jsx_functions() {
        let content = r#"function add(a,b){ return a+b; }
const foo = (x)=>{ return x*2; }
"#;
        let fns = extract(content, Path::new("plain.jsx")).unwrap();
        assert_eq!(fns.len(), 0);
    }

    #[test]
    fn hash_and_location() {
        let content = "function Foo(){ return <div>hi</div>; }\n";
        let fns = extract(content, Path::new("a.jsx")).unwrap();
        assert_eq!(fns.len(), 1);
        let foo = &fns[0];
        assert!(foo.source.hash.starts_with("sha256:"));
        assert_eq!(foo.source.location.start.byte, 0);
        assert_eq!(foo.source.location.start.line, 1);
        assert!(foo.source.location.end.byte > foo.source.location.start.byte);
    }
}
