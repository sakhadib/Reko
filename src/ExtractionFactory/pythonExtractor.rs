use crate::ir::{IrFunction, Parameter, Position};
use anyhow::Result;
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Debug, Clone)]
struct PythonFunctionRaw {
    name: String,
    return_type: Option<String>,
    visibility: String,
    modifiers: Vec<String>,
    parameters: Vec<Parameter>,
    annotations: Vec<String>,
    is_async: bool,
    start_line: usize,
    start_col: usize,
    start_byte: usize,
    end_line: usize,
    end_col: usize,
    end_byte: usize,
    source_text: String,
    class_context: Option<String>,
}

/// Public entry: given file content (exact, from reader) and file path, extract IR for each Python function.
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
            r.return_type.clone(),
            r.parameters.clone(),
            vec![],
            None,
            r.class_context.clone(),
        );
        // Patch for Python specifics (new_minimal hardcodes java)
        ir.identity.language = "python".to_string();
        ir.metadata.parser = Some("reko-pythonExtractor".to_string());
        ir.execution.is_async = r.is_async;
        ir.declaration.annotations = r.annotations.clone();
        // Ensure context module is correct (new_minimal maps package -> module)
        ir.context.module = Some(module.clone());
        ir.context.class = r.class_context.clone();
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

fn count_indent(s: &str) -> usize {
    s.chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .count()
}

fn first_non_space_col(s: &str) -> usize {
    s.chars()
        .position(|c| c != ' ' && c != '\t')
        .unwrap_or(0)
}

fn parse_class_name(trimmed: &str) -> Option<String> {
    // trimmed starts with "class "
    if !trimmed.starts_with("class ") {
        return None;
    }
    let after = trimmed[6..].trim_start();
    // name until ':', '(', whitespace
    let mut name = String::new();
    for c in after.chars() {
        if c.is_alphanumeric() || c == '_' {
            name.push(c);
        } else {
            break;
        }
    }
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

fn parse_functions(content: &str) -> Result<Vec<PythonFunctionRaw>> {
    // Build line_starts: byte offset for each line start (1-indexed lines)
    let mut line_starts: Vec<usize> = vec![0];
    for (i, b) in content.bytes().enumerate() {
        if b == b'\n' {
            line_starts.push(i + 1);
        }
    }
    let lines: Vec<&str> = content.lines().collect();
    // If content is empty, return empty
    if lines.is_empty() {
        return Ok(Vec::new());
    }

    let mut result = Vec::new();
    let mut class_stack: Vec<(String, usize)> = Vec::new();

    let mut i = 0usize;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();
        let indent = count_indent(line);

        // maintain class stack: pop when dedented
        if !trimmed.is_empty() && !trimmed.starts_with('#') {
            while let Some((_, class_indent)) = class_stack.last() {
                if indent <= *class_indent {
                    class_stack.pop();
                } else {
                    break;
                }
            }
        }

        // class definition ?
        if trimmed.starts_with("class ") {
            if let Some(name) = parse_class_name(trimmed) {
                class_stack.push((name, indent));
            }
            i += 1;
            continue;
        }

        // skip pure decorator lines - they will be handled together with def
        if trimmed.starts_with('@') {
            // peek ahead if next non-empty is def; if so, don't consume now, let def handling backtrack
            // but to avoid double counting, just advance. The def handler will backtrack to include decorators.
            // So we can skip decorators as standalone.
            // However we need to ensure class_stack not popped incorrectly for decorators inside class.
            i += 1;
            continue;
        }

        // check for def
        let is_async = trimmed.starts_with("async def ");
        let is_def = trimmed.starts_with("def ") || is_async;
        if is_def {
            // find decorator start (contiguous @ lines immediately above, no blank)
            let mut decor_start = i;
            if i > 0 {
                let mut j = i as isize - 1;
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

            let def_indent = count_indent(lines[i]);

            let start_line = decor_start + 1;
            let start_col = first_non_space_col(lines[decor_start]) + 1;
            let start_byte = line_starts[decor_start] + first_non_space_col(lines[decor_start]);

            // Find signature end (line containing ':' after balanced parens)
            let mut sig_end_idx = i;
            let mut found = false;
            for k in i..lines.len() {
                // compute paren balance from i to k
                let mut balance: i32 = 0;
                for idx in i..=k {
                    for ch in lines[idx].chars() {
                        if ch == '(' {
                            balance += 1;
                        } else if ch == ')' {
                            balance -= 1;
                        }
                    }
                }
                let cur_trim = lines[k].trim();
                // colon must exist
                if lines[k].contains(':') && balance <= 0 {
                    // heuristic: colon after ')', else maybe no params?
                    // check that ':' is present and typically at end (maybe with comment)
                    // We'll accept if trimmed ends with ':' or contains ':'.
                    // To avoid matching dict ':' inside, ensure colon is not inside parens/brackets? balance <=0 ensures outside.
                    // Also ensure line after ':' maybe not part of type inside string.
                    // Accept this k as end.
                    sig_end_idx = k;
                    found = true;
                    break;
                }
                // Limit search window for performance; multi-line sigs rarely > 20 lines
                if k >= i + 30 {
                    break;
                }
                // if we encounter a new dedented def/class without colon, break
                if k > i {
                    let cur_indent = count_indent(lines[k]);
                    let cur_t = lines[k].trim();
                    if !cur_t.is_empty() && cur_indent <= def_indent && (cur_t.starts_with("def ") || cur_t.starts_with("async def ") || cur_t.starts_with("class ") || cur_t.starts_with('@')) {
                        break;
                    }
                }
            }
            if !found {
                i += 1;
                continue;
            }

            // Check inline body: text after ':' on same line
            let sig_line = lines[sig_end_idx];
            let colon_pos = sig_line.rfind(':').unwrap_or(sig_line.len());
            let after_colon = sig_line[colon_pos + 1..].trim();
            let has_inline_body = !after_colon.is_empty() && !after_colon.starts_with('#');

            let end_idx: usize;
            if has_inline_body {
                end_idx = sig_end_idx;
            } else {
                // find first non-blank body line after sig_end_idx
                let mut k = sig_end_idx + 1;
                while k < lines.len() && lines[k].trim().is_empty() {
                    k += 1;
                }
                if k >= lines.len() {
                    // no body
                    end_idx = sig_end_idx;
                } else {
                    let first_indent = count_indent(lines[k]);
                    if first_indent <= def_indent {
                        // empty body (maybe `pass` expected but absent)
                        end_idx = sig_end_idx;
                    } else {
                        let mut last_body_idx = k;
                        let mut j = k + 1;
                        while j < lines.len() {
                            let cur = lines[j];
                            let cur_trim = cur.trim();
                            if cur_trim.is_empty() {
                                j += 1;
                                continue;
                            }
                            // If line is decorator at same indent as def inside class? Actually decorator inside class has indent > class but equal to method indent, but since we're inside method body, no.
                            // For body detection, any line with indent <= def_indent terminates.
                            let cur_indent = count_indent(cur);
                            if cur_indent <= def_indent {
                                break;
                            }
                            last_body_idx = j;
                            j += 1;
                        }
                        // Include trailing blank lines? Keep last_body_idx as is (last non-blank)
                        // However, Python often has blank lines inside function that we already skipped without updating, but they are between body lines, still part of slice.
                        // Our end_idx points to last non-blank, which will include blanks in slice because slice is byte-range spanning all lines including blanks.
                        end_idx = last_body_idx;
                    }
                }
            }

            let end_line = end_idx + 1;
            let end_col = if lines[end_idx].is_empty() {
                1
            } else {
                lines[end_idx].len()
            };
            let end_byte = if lines[end_idx].is_empty() {
                line_starts[end_idx]
            } else {
                // inclusive byte of last char
                let len = lines[end_idx].len();
                // Need to adjust for original bytes: lines from content.lines() may have stripped '\r' if CRLF.
                // Use line_starts to compute inclusive. len is without newline, so last char inclusive = start + len -1
                line_starts[end_idx] + len - 1
            };

            // sanity: if end_byte < start_byte, fallback
            let source_text = if end_byte >= start_byte && end_byte < content.len() {
                content[start_byte..=end_byte].to_string()
            } else if start_byte < content.len() {
                // fallback slice exclusive
                let exclusive = line_starts[end_idx] + lines[end_idx].len();
                let ex = exclusive.min(content.len());
                if start_byte < ex {
                    content[start_byte..ex].to_string()
                } else {
                    String::new()
                }
            } else {
                String::new()
            };

            // Parse signature
            let mut sig_parts: Vec<String> = Vec::new();
            for idx in i..=sig_end_idx {
                sig_parts.push(lines[idx].trim().to_string());
            }
            let sig_joined = sig_parts.join(" ");
            let (name, params, ret_type) = parse_python_signature(&sig_joined, is_async);

            if name.is_empty() || name == "unknown" {
                i = end_idx + 1;
                continue;
            }

            // visibility
            let visibility = if name.starts_with("__") && name.ends_with("__") {
                "public".to_string()
            } else if name.starts_with('_') {
                "private".to_string()
            } else {
                "public".to_string()
            };

            let mut modifiers: Vec<String> = Vec::new();
            if is_async {
                modifiers.push("async".to_string());
            }
            // Decorators -> modifiers
            for idx in decor_start..i {
                let d = lines[idx].trim();
                if d.starts_with('@') {
                    let dec_name = d[1..].split('(').next().unwrap_or("").trim();
                    // map decorators to modifiers
                    match dec_name {
                        "staticmethod" => modifiers.push("static".to_string()),
                        "classmethod" => modifiers.push("classmethod".to_string()),
                        "property" => modifiers.push("property".to_string()),
                        "abstractmethod" => modifiers.push("abstract".to_string()),
                        _ => {
                            // keep decorator name as modifier for visibility? e.g., decorators may be kept but not duplicate async
                            if !dec_name.is_empty() && dec_name != "async" {
                                // optional: add raw decorator
                            }
                        }
                    }
                }
            }

            let annotations: Vec<String> = (decor_start..i)
                .filter_map(|idx| {
                    let t = lines[idx].trim();
                    if t.starts_with('@') {
                        Some(t.to_string())
                    } else {
                        None
                    }
                })
                .collect();

            let class_ctx = class_stack.last().map(|(n, _)| n.clone());

            result.push(PythonFunctionRaw {
                name,
                return_type: ret_type,
                visibility,
                modifiers,
                parameters: params,
                annotations,
                is_async,
                start_line,
                start_col,
                start_byte,
                end_line,
                end_col,
                end_byte,
                source_text,
                class_context: class_ctx,
            });

            i = end_idx + 1;
            continue;
        }

        i += 1;
    }

    Ok(result)
}

fn parse_python_signature(sig: &str, _is_async: bool) -> (String, Vec<Parameter>, Option<String>) {
    // sig like "def foo(a, b: int = 2) -> str:" or "async def foo(...):"
    let def_pos = sig.find("def ").unwrap_or(0);
    let after_def = &sig[def_pos + 4..];
    // after_def: "foo(a, ...) -> str:" etc
    let paren_start = after_def.find('(');
    let name = if let Some(p) = paren_start {
        after_def[..p].trim().to_string()
    } else {
        // no paren? fallback
        after_def
            .split(':')
            .next()
            .unwrap_or("")
            .split_whitespace()
            .next()
            .unwrap_or("unknown")
            .to_string()
    };
    let name = name.trim().to_string();

    // extract inside parens
    let params_str = if let Some(start) = paren_start {
        let after = &after_def[start..];
        // find matching closing ')'
        // since sig_joined collapsed spaces, simple rfind
        let paren_end = after.rfind(')').unwrap_or(after.len());
        if paren_end > 0 {
            after[1..paren_end].trim().to_string()
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    let parameters = parse_params_python(&params_str);

    // return type: look for "->" before ":"
    let ret_type = if let Some(arrow) = sig.find("->") {
        let after_arrow = &sig[arrow + 2..];
        // trim up to ':'
        let colon = after_arrow.find(':').unwrap_or(after_arrow.len());
        let t = after_arrow[..colon].trim().trim_end_matches(':').trim().to_string();
        if t.is_empty() {
            None
        } else {
            Some(t)
        }
    } else {
        None
    };

    (name, parameters, ret_type)
}

fn parse_params_python(s: &str) -> Vec<Parameter> {
    if s.trim().is_empty() {
        return Vec::new();
    }
    let parts = split_params_respecting_brackets(s);
    let mut out = Vec::new();
    for part in parts {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        // remove default value
        let without_default = p.split('=').next().unwrap_or(p).trim();
        // split type hint
        let (name_part, typ) = if let Some(colon_idx) = without_default.find(':') {
            let n = without_default[..colon_idx].trim();
            let t = without_default[colon_idx + 1..].trim();
            (n, if t.is_empty() { "Any".to_string() } else { t.to_string() })
        } else {
            (without_default, "Any".to_string())
        };
        // strip leading * or **
        let mut name_clean = name_part.trim().trim_start_matches('*').trim().to_string();
        // edge: if name is "/" or "*" (positional marker), skip
        if name_clean == "/" || name_clean == "*" || name_clean.is_empty() {
            continue;
        }
        // also handle "self: Self" etc; keep name
        // skip if name contains spaces (shouldn't)
        if name_clean.contains(' ') {
            // take last token
            if let Some(last) = name_clean.split_whitespace().last() {
                name_clean = last.to_string();
            }
        }
        out.push(Parameter {
            name: name_clean,
            typ,
        });
    }
    out
}

fn split_params_respecting_brackets(s: &str) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut depth_paren: i32 = 0;
    let mut depth_bracket: i32 = 0;
    let mut depth_brace: i32 = 0;
    let mut in_single = false;
    let mut in_double = false;
    let mut escape = false;

    for ch in s.chars() {
        if escape {
            current.push(ch);
            escape = false;
            continue;
        }
        if ch == '\\' {
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
        match ch {
            '\'' => {
                in_single = true;
                current.push(ch);
            }
            '"' => {
                in_double = true;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const SAMPLE_CLASS: &str = r#"class MyClass:
    def method_one(self, x: int) -> int:
        return x + 1

    @staticmethod
    def static_method(a, b=2):
        return a + b

    async def async_method(self):
        await something()
"#;

    const SAMPLE_FUNCS: &str = r#"def hello(name: str = "world") -> str:
    return f"hello {name}"

@decorator
def _private_func(a, b: int, *args, **kwargs):
    x = 1
    return x

def outer():
    def inner():
        pass
    return inner
"#;

    const SAMPLE_DECORATED_ARGS: &str = r#"@decorator1
@decorator2(param=1)
async def complex(a: List[int], b: Dict[str, int] = {}, *args: int, **kwargs: str) -> Optional[int]:
    """docstring"""
    pass
"#;

    #[test]
    fn extracts_methods_and_functions() {
        let fns = extract(SAMPLE_CLASS, Path::new("mymod.py")).unwrap();
        assert_eq!(fns.len(), 3);
        // method_one is inside MyClass
        let m1 = fns.iter().find(|f| f.identity.name == "method_one").unwrap();
        assert_eq!(m1.identity.qualified_name, "mymod::MyClass::method_one");
        assert_eq!(m1.signature.return_type.as_deref(), Some("int"));
        assert_eq!(m1.signature.parameters.len(), 2); // self, x
        assert_eq!(m1.context.class.as_deref(), Some("MyClass"));
        // static_method with decorator
        let sm = fns.iter().find(|f| f.identity.name == "static_method").unwrap();
        assert!(sm.declaration.modifiers.contains(&"static".to_string()));
        assert_eq!(sm.signature.parameters.len(), 2);
        // async
        let am = fns.iter().find(|f| f.identity.name == "async_method").unwrap();
        assert!(am.execution.is_async);
        assert!(am.declaration.modifiers.contains(&"async".to_string()));
        for f in &fns {
            assert!(f.source.hash.starts_with("sha256:"));
            assert!(f.source.location.start.byte <= f.source.location.end.byte);
        }
    }

    #[test]
    fn visibility_and_return_type() {
        let fns = extract(SAMPLE_FUNCS, Path::new("test_mod.py")).unwrap();
        assert!(fns.len() >= 3);
        let hello = fns.iter().find(|f| f.identity.name == "hello").unwrap();
        assert_eq!(hello.identity.qualified_name, "test_mod::hello");
        assert_eq!(hello.signature.return_type.as_deref(), Some("str"));
        assert_eq!(hello.declaration.visibility, "public");
        assert_eq!(hello.signature.parameters[0].name, "name");
        // private
        let privf = fns.iter().find(|f| f.identity.name == "_private_func").unwrap();
        assert_eq!(privf.declaration.visibility, "private");
        assert_eq!(privf.signature.parameters.len(), 4); // a, b, args, kwargs
        assert_eq!(privf.source.location.start.line, 4); // decorator line is start
        assert!(privf.declaration.annotations.contains(&"@decorator".to_string()));
        // outer contains inner as separate? nested functions both extracted
        let outer = fns.iter().find(|f| f.identity.name == "outer").unwrap();
        assert_eq!(outer.signature.parameters.len(), 0);
    }

    #[test]
    fn decorators_and_complex_params() {
        let fns = extract(SAMPLE_DECORATED_ARGS, Path::new("mod.py")).unwrap();
        assert_eq!(fns.len(), 1);
        let c = &fns[0];
        assert_eq!(c.identity.name, "complex");
        assert_eq!(c.identity.qualified_name, "mod::complex");
        assert!(c.execution.is_async);
        assert_eq!(c.declaration.annotations.len(), 2);
        assert!(c.declaration.annotations.contains(&"@decorator1".to_string()));
        assert_eq!(c.signature.return_type.as_deref(), Some("Optional[int]"));
        // params: a, b, args, kwargs = 4
        assert_eq!(c.signature.parameters.len(), 4);
        assert_eq!(c.signature.parameters[0].typ, "List[int]");
        assert_eq!(c.signature.parameters[1].typ, "Dict[str, int]");
        assert!(c.source.source_text.contains("@decorator1"));
        assert!(c.source.hash.starts_with("sha256:"));
        assert_eq!(c.source.location.start.line, 1);
    }

    #[test]
    fn location_bytes_consistency() {
        let content = "def foo():\n    pass\n";
        let fns = extract(content, Path::new("a.py")).unwrap();
        assert_eq!(fns.len(), 1);
        assert!(fns[0].source.location.start.byte < fns[0].source.location.end.byte);
        assert_eq!(fns[0].source.location.start.line, 1);
        assert_eq!(fns[0].source.location.end.line, 2);
    }
}
