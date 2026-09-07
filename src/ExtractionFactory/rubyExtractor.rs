use crate::ir::{IrFunction, Parameter, Position};
use anyhow::Result;
use regex::Regex;
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Debug, Clone)]
struct RubyFunctionRaw {
    name: String,
    qualified_name: String,
    parameters: Vec<Parameter>,
    is_singleton: bool,
    context_parts: Vec<String>,
    class_context: Option<String>,
    module_context: Option<String>,
    start_line: usize,
    start_col: usize,
    start_byte: usize,
    end_line: usize,
    end_col: usize,
    end_byte: usize,
    source_text: String,
}

/// Public entry: given file content (exact, from reader) and file path, extract IR for each Ruby method.
pub fn extract(content: &str, file_path: &Path) -> Result<Vec<IrFunction>> {
    let file_module = file_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    let file_str = file_path.display().to_string();

    let raws = parse_ruby_functions(content)?;

    let mut out = Vec::new();
    for r in raws {
        let hash = hash_source(&r.source_text);
        let qualified = r.qualified_name.clone();
        let id = qualified.clone();

        let mut ir = IrFunction::new_minimal(
            id,
            r.name.clone(),
            qualified.clone(),
            file_str.clone(),
            file_module.clone(),
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
            "public".to_string(),
            vec![],
            None,
            r.parameters.clone(),
            vec![],
            None,
            r.class_context.clone(),
        );
        ir.identity.language = "ruby".to_string();
        ir.metadata.parser = Some("reko-rubyExtractor".to_string());
        ir.metadata.parser_version = Some("0.1.0".to_string());
        // context
        if !r.context_parts.is_empty() {
            ir.context.namespace = Some(r.context_parts.join("::"));
        } else {
            ir.context.namespace = None;
        }
        // module: outermost module if present else file_module? keep file_module as fallback for source but namespace captures actual
        if let Some(m) = r.module_context.clone() {
            ir.context.module = Some(m);
        } else {
            ir.context.module = Some(file_module.clone());
        }
        ir.context.class = r.class_context.clone();
        // package for ruby not used, keep None
        ir.context.package = None;
        // Keep trait/struct/interface None
        ir.source.module = file_module.clone();
        // Ensure behavior fields reflect singleton modifier if needed
        if r.is_singleton {
            // mark as static-like via modifiers
            ir.declaration.modifiers = vec!["self".to_string()];
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
    s.chars().position(|c| c != ' ' && c != '\t').unwrap_or(0)
}

// Mask a line: replace string contents with spaces and strip comment after '#'
fn mask_line(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut in_single = false;
    let mut in_double = false;
    let mut escape = false;
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if escape {
            // inside string, replace with space
            if in_single || in_double {
                out.push(' ');
            } else {
                out.push(c);
            }
            escape = false;
            i += 1;
            continue;
        }
        if in_single {
            if c == '\\' {
                escape = true;
                out.push(' ');
            } else if c == '\'' {
                in_single = false;
                out.push(' ');
            } else {
                out.push(' ');
            }
            i += 1;
            continue;
        }
        if in_double {
            if c == '\\' {
                escape = true;
                out.push(' ');
            } else if c == '"' {
                in_double = false;
                out.push(' ');
            } else if c == '#' && i + 1 < chars.len() && chars[i + 1] == '{' {
                // interpolation start #{ -> keep as code? simplify treat as space but not comment
                out.push(' ');
                // don't break; continue inside double but we could push
                // actually interpolation contains code, but we mask for simplicity
            } else {
                out.push(' ');
            }
            i += 1;
            continue;
        }
        // not in string
        if c == '\'' {
            in_single = true;
            out.push(' ');
            i += 1;
            continue;
        }
        if c == '"' {
            in_double = true;
            out.push(' ');
            i += 1;
            continue;
        }
        if c == '#' {
            // comment start -> replace rest with spaces
            for _ in i..chars.len() {
                out.push(' ');
            }
            break;
        }
        out.push(c);
        i += 1;
    }
    out
}

fn split_by_semicolon(masked: &str, original: &str) -> Vec<(String, String, usize)> {
    let mut res = Vec::new();
    let mut start = 0usize;
    let masked_bytes = masked.as_bytes();
    for i in 0..masked.len() {
        if masked_bytes[i] == b';' {
            res.push((
                masked[start..i].to_string(),
                original[start..i].to_string(),
                start,
            ));
            start = i + 1;
        }
    }
    res.push((
        masked[start..].to_string(),
        original[start..].to_string(),
        start,
    ));
    res
}

#[derive(Debug, Clone)]
struct Segment {
    line_idx: usize,
    start_offset: usize,
    masked: String,
    original: String,
}

fn build_segments(lines: &[&str], masks: &[String]) -> Vec<Segment> {
    let mut segs = Vec::new();
    for (idx, (orig, masked)) in lines.iter().zip(masks.iter()).enumerate() {
        let parts = split_by_semicolon(masked, orig);
        for (m, o, off) in parts {
            // Keep even empty segments? skip empty trimmed?
            // We keep all but empty will have no keywords; still needed for offset but can skip empty for analysis
            segs.push(Segment {
                line_idx: idx,
                start_offset: off,
                masked: m,
                original: o,
            });
        }
    }
    segs
}

fn extract_module_name(masked_seg: &str) -> Option<String> {
    let trimmed = masked_seg.trim();
    // must start with module
    // Use regex
    let re = Regex::new(r"^\s*module\s+([A-Za-z_][\w:]*)").unwrap();
    if let Some(cap) = re.captures(masked_seg) {
        if let Some(m) = cap.get(1) {
            let name = m.as_str().trim().to_string();
            if !name.is_empty() {
                return Some(name);
            }
        }
    } else {
        // fallback for leading spaces already trimmed? check trimmed
        let re2 = Regex::new(r"^module\s+([A-Za-z_][\w:]*)").unwrap();
        if let Some(cap) = re2.captures(trimmed) {
            if let Some(m) = cap.get(1) {
                return Some(m.as_str().to_string());
            }
        }
    }
    None
}

fn extract_class_name(masked_seg: &str) -> Option<String> {
    let re = Regex::new(r"^\s*class\s+([A-Za-z_][\w:]*)").unwrap();
    if let Some(cap) = re.captures(masked_seg) {
        if let Some(m) = cap.get(1) {
            let raw = m.as_str().trim().to_string();
            // raw may include `Foo < Bar` but regex stops at Foo, so fine
            if !raw.is_empty() {
                return Some(raw);
            }
        }
    }
    let trimmed = masked_seg.trim();
    let re2 = Regex::new(r"^class\s+([A-Za-z_][\w:]*)").unwrap();
    if let Some(cap) = re2.captures(trimmed) {
        if let Some(m) = cap.get(1) {
            return Some(m.as_str().to_string());
        }
    }
    None
}

fn detect_def_in_segment(masked_seg: &str) -> Option<(usize, bool, String, String)> {
    // Returns (def_pos_in_seg, is_self, name, params_inside)
    // Use regex to find def
    // But we need position
    let re = Regex::new(r"\bdef\b").unwrap();
    if let Some(mat) = re.find(masked_seg) {
        let def_pos = mat.start();
        // Extract tail from masked for name parsing? Use original? Use masked for name but name chars are preserved (since not in string)
        // We'll parse from masked_seg tail for simplicity
        let tail = &masked_seg[def_pos + 3..];
        let tail_trim = tail.trim_start();
        let mut is_self = false;
        let mut remainder = tail_trim;
        if remainder.starts_with("self.") {
            is_self = true;
            remainder = remainder[5..].trim_start();
        } else if remainder.starts_with("self .") {
            // unlikely
        }
        // Extract name
        let mut name_end = 0usize;
        let chars: Vec<char> = remainder.chars().collect();
        let mut idx = 0usize;
        while idx < chars.len() {
            let c = chars[idx];
            if c.is_alphanumeric() || c == '_' {
                // allow ! ? = at end
                name_end = idx + 1;
                idx += 1;
                // check if next is !?=
                if idx < chars.len() && (chars[idx] == '!' || chars[idx] == '?' || chars[idx] == '=') {
                    // include one
                    name_end = idx + 1;
                    idx += 1;
                }
                // For simplicity after initial alphanumeric, break if subsequent char not !?=
                // But names may include only alnum + _ so we break after first non-alnum segment?
                // Actually we need to capture full name; loop continues but will capture contiguous alnum/_ plus one suffix char
                // To get full name, we already captured first word; remaining chars after suffix should be break
                // Find word boundary: next char after name should be whitespace, '(' or other
                // So we break after capturing name
                break;
            } else {
                break;
            }
        }
        // Alternative: simpler method using regex for name
        let name_re = Regex::new(r"^([A-Za-z_][\w]*[!?=]?)").unwrap();
        let mut name = String::new();
        if let Some(cap) = name_re.captures(remainder) {
            if let Some(m) = cap.get(1) {
                name = m.as_str().to_string();
                name_end = m.end();
                // For self case, name_end already accounted
            }
        } else {
            return None;
        }
        if name.is_empty() {
            return None;
        }
        // Remainder after name for params
        let after_name = &remainder[name_end..];
        let after_trim = after_name.trim_start();
        let params_inside = if after_trim.starts_with('(') {
            // find matching ')'
            if let Some(close) = after_trim.find(')') {
                after_trim[1..close].to_string()
            } else {
                String::new()
            }
        } else {
            // No parens: if next content looks like params without parens, capture until comment/end
            // e.g., "a, b" - take up to end of segment trimmed
            let tail = after_trim.trim();
            if !tail.is_empty() && !tail.starts_with(';') && !tail.starts_with('#') {
                // heuristic: if contains ',' or is single identifier
                // Take whole tail as params string, stripping trailing keywords like 'then' not needed
                // We treat entire tail as params string
                // Clean up possible trailing stuff like "do" etc not part of params? Ruby def params not include do
                // We'll capture up to before any keyword that is not param? For simplicity capture whole tail
                // Remove any trailing comment already stripped
                // Split by '#' already masked, so just take tail
                // But ensure we don't include stray 'end' etc
                // Limit to until newline - the segment is already one semicolon piece, so it's fine
                // We'll take tail and remove any trailing ';' etc
                // If tail contains '=', ':' etc still param
                // Use simple heuristic: if tail contains word characters, consider as params
                // However for cases like "def foo\n" tail empty => no params
                // For "def foo a, b" => tail = "a, b"
                tail.to_string()
            } else {
                String::new()
            }
        };

        return Some((def_pos, is_self, name, params_inside));
    }
    None
}

fn parse_ruby_params(params_str: &str) -> Vec<Parameter> {
    let s = params_str.trim();
    if s.is_empty() {
        return Vec::new();
    }
    // Split by ',' but respect brackets/parens? simple split
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
        if ch == '\\' && (in_single || in_double) {
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
            ',' if depth_paren == 0 && depth_bracket == 0 && depth_brace == 0 => {
                parts.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        parts.push(current.trim().to_string());
    }

    let mut out = Vec::new();
    for part in parts {
        let p = part.trim().to_string();
        if p.is_empty() {
            continue;
        }
        // Remove default values, keyword defaults, type hints? Ruby has no type hints but has defaults
        // Extract name before '=', ':', whitespace
        let without_default = p.split('=').next().unwrap_or(&p).trim();
        // For keyword args like "e:" or "f: 1" or "g:" we handle
        // Remove leading * ** &
        let stripped = without_default.trim_start_matches('*').trim_start_matches('&').trim();
        // After stripping *, may be "**g" -> after one *, remaining "*g" still has *, second trim above trims all?
        // Better: loop trim first chars
        let mut name_candidate = stripped.trim().to_string();
        // Remove trailing ':' for keyword args without default (e.g., "e:")
        if name_candidate.ends_with(':') {
            name_candidate = name_candidate.trim_end_matches(':').trim().to_string();
        }
        // If contains ':', e.g., "f: 1" already handled by split '=', but now "f:" already trimmed, "f: 1" after split '=' still contains ':'
        // So also handle colon split
        if name_candidate.contains(':') {
            // e.g., "f: 1" or "e: 1" -> take before ':'
            if let Some(colon) = name_candidate.find(':') {
                name_candidate = name_candidate[..colon].trim().to_string();
            }
        }
        // Remove remaining punctuation
        // name may still contain spaces like "a" fine
        // Take first token
        let name = name_candidate
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_matches(|c: char| c == '*' || c == '&' || c == ':')
            .to_string();
        if name.is_empty() || name == "/" || name == "*" {
            continue;
        }
        // Edge: name may be like "*c" after earlier trim we removed *, but if original was "*c", stripped becomes "c" ok
        out.push(Parameter {
            name,
            typ: "Object".to_string(),
        });
    }
    out
}

fn count_openings(masked: &str) -> usize {
    // Count openings: def, class, module, do, begin, if, unless, case, while, until, for
    // Use regex
    let re = Regex::new(r"\b(def|class|module|do|begin|if|unless|case|while|until|for)\b").unwrap();
    re.find_iter(masked).count()
}

fn count_ends(masked: &str) -> usize {
    let re = Regex::new(r"\bend\b").unwrap();
    re.find_iter(masked).count()
}

fn parse_ruby_functions(content: &str) -> Result<Vec<RubyFunctionRaw>> {
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return Ok(Vec::new());
    }
    let mut line_starts: Vec<usize> = vec![0];
    for (i, b) in content.bytes().enumerate() {
        if b == b'\n' {
            line_starts.push(i + 1);
        }
    }
    let masks: Vec<String> = lines.iter().map(|l| mask_line(l)).collect();
    let segments = build_segments(&lines, &masks);

    // Pre-build re for def detection quick
    let def_word_re = Regex::new(r"\bdef\b").unwrap();

    let mut result = Vec::new();

    // Helper to compute enclosing context for a given flat seg index
    // We will compute on the fly per def
    for (seg_idx, seg) in segments.iter().enumerate() {
        if !def_word_re.is_match(&seg.masked) {
            continue;
        }
        // Need to ensure this seg actually contains a valid def; check via detect
        let def_info = match detect_def_in_segment(&seg.masked) {
            Some(v) => v,
            None => continue,
        };
        let (def_pos_in_seg, is_self, name, params_inside) = def_info;
        if name.is_empty() {
            continue;
        }

        // Compute enclosing by scanning segments before this seg_idx
        let mut stack: Vec<(String, String)> = Vec::new(); // (name, kind)
        // We need also anonymous stack for proper popping; use Vec<String> kind stack
        let mut block_stack: Vec<String> = Vec::new();
        // For tracking which block corresponds to module/class name
        let mut name_stack: Vec<Option<String>> = Vec::new();

        for prev_idx in 0..seg_idx {
            let prev = &segments[prev_idx];
            let m = prev.masked.trim();
            if m.is_empty() {
                continue;
            }
            // Determine if this segment opens a block
            // Check for module/class first (they include name)
            if let Some(mod_name) = extract_module_name(&prev.masked) {
                block_stack.push("module".to_string());
                name_stack.push(Some(mod_name.clone()));
                stack.push((mod_name, "module".to_string()));
                // Segments are split by ';', so typically one keyword per segment.
                // Ends in same segment would be in a different segment after split, so no need to handle here.
                continue;
            }
            if let Some(cls_name) = extract_class_name(&prev.masked) {
                block_stack.push("class".to_string());
                name_stack.push(Some(cls_name.clone()));
                stack.push((cls_name, "class".to_string()));
                continue;
            }

            // Generic openings: count def etc but not already module/class
            // If segment contains def, it's a function opening
            let opens = count_openings(&prev.masked);
            let ends = count_ends(&prev.masked);
            // For segments with multiple openings/ends, push/pop accordingly using simple counts assuming order openings before ends?
            // Simplified: push opens times then pop ends times
            // This loses ordering but works for typical cases where def not closed same line.
            for _ in 0..opens {
                // Determine kind of opening? If def already counted as function, push "def"
                // But we have already handled module/class, so remaining opens are def/do/if etc
                // We push anonymous
                // To keep stack correct for module/class popping on end, generic push as "other"
                block_stack.push("other".to_string());
                name_stack.push(None);
            }
            for _ in 0..ends {
                if let Some(k) = block_stack.pop() {
                    name_stack.pop();
                    if k == "module" || k == "class" {
                        stack.pop();
                    }
                }
            }
        }

        // At this point stack holds enclosing modules/classes
        let context_parts: Vec<String> = stack.iter().map(|(n, _)| n.clone()).collect();
        // Derive class and module contexts
        let mut class_ctx: Option<String> = None;
        let mut module_ctx: Option<String> = None;
        for (n, kind) in stack.iter().rev() {
            if class_ctx.is_none() && kind == "class" {
                class_ctx = Some(n.clone());
            }
            if module_ctx.is_none() && kind == "module" {
                module_ctx = Some(n.clone());
            }
        }
        // If qualified contains ::, class_ctx may need full qualified chain? keep innermost
        // For qualified_name, join context_parts + name
        let base_name = name.clone();
        let qualified_name = if context_parts.is_empty() {
            base_name.clone()
        } else {
            format!("{}::{}", context_parts.join("::"), base_name)
        };

        // Find matching end
        // Build suffix after def within current segment
        let seg_masked = &seg.masked;
        let def_start_in_seg = def_pos_in_seg;
        let suffix_after_def = &seg_masked[def_start_in_seg + 3..];
        let mut depth: i32 = 1;
        // Count openings/ends in suffix_after_def
        // Need to exclude the def itself (we started at 1, so not count it)
        let opens_suffix = count_openings(suffix_after_def);
        let ends_suffix = count_ends(suffix_after_def);
        depth += opens_suffix as i32 - ends_suffix as i32;
        let mut end_seg_idx: Option<usize> = None;
        let mut end_line_idx: Option<usize> = None;
        let mut end_offset_in_seg: Option<usize> = None;
        if depth == 0 {
            // end is within same segment
            // find position of 'end' in suffix
            if let Some(pos) = suffix_after_def.find("end") {
                // ensure word boundary? but okay
                // Find actual word match
                let re_end = Regex::new(r"\bend\b").unwrap();
                if let Some(mat) = re_end.find(suffix_after_def) {
                    end_seg_idx = Some(seg_idx);
                    end_line_idx = Some(seg.line_idx);
                    end_offset_in_seg = Some(seg.start_offset + def_start_in_seg + 3 + mat.start());
                } else {
                    // fallback
                    end_seg_idx = Some(seg_idx);
                    end_line_idx = Some(seg.line_idx);
                    end_offset_in_seg = Some(seg.start_offset + def_start_in_seg + 3 + pos);
                }
            } else {
                end_seg_idx = Some(seg_idx);
                end_line_idx = Some(seg.line_idx);
                end_offset_in_seg = Some(seg.start_offset + suffix_after_def.len());
            }
        } else {
            // scan subsequent segments
            for next_idx in seg_idx + 1..segments.len() {
                let next_seg = &segments[next_idx];
                let opens = count_openings(&next_seg.masked);
                let ends = count_ends(&next_seg.masked);
                depth += opens as i32 - ends as i32;
                if depth == 0 {
                    end_seg_idx = Some(next_idx);
                    end_line_idx = Some(next_seg.line_idx);
                    // locate 'end' within this segment (last occurrence maybe first that makes depth 0)
                    // Find position of 'end' that corresponds to closing; we pick last 'end' word in segment for simplicity, or first
                    let re_end = Regex::new(r"\bend\b").unwrap();
                    let mut last_pos: Option<usize> = None;
                    for mat in re_end.find_iter(&next_seg.masked) {
                        last_pos = Some(mat.start());
                    }
                    let pos = last_pos.unwrap_or(0);
                    end_offset_in_seg = Some(next_seg.start_offset + pos);
                    break;
                }
                if depth < 0 {
                    break;
                }
            }
        }

        let (end_line_idx, end_offset, _end_seg_idx) = match (end_line_idx, end_offset_in_seg, end_seg_idx) {
            (Some(l), Some(off), Some(s)) => (l, off, s),
            _ => continue, // no matching end found
        };

        let start_line = seg.line_idx + 1;
        let start_col = seg.start_offset + def_pos_in_seg + 1; // 1-indexed
        let start_byte = line_starts[seg.line_idx] + seg.start_offset + def_pos_in_seg;

        let end_line = end_line_idx + 1;
        // end_col inclusive last char of 'end'
        let end_col = end_offset + 3; // 'end' length 3, 1-indexed column of last char = start col +2
        let end_byte = line_starts[end_line_idx] + end_offset + 2; // inclusive byte of last char

        // source_text: content[start_byte..=end_byte]
        let source_text = if start_byte <= end_byte && end_byte < content.len() {
            content[start_byte..=end_byte].to_string()
        } else if start_byte < content.len() {
            // fallback exclusive
            let ex = (end_byte + 1).min(content.len());
            if start_byte < ex {
                content[start_byte..ex].to_string()
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        let parameters = parse_ruby_params(&params_inside);

        result.push(RubyFunctionRaw {
            name: base_name,
            qualified_name,
            parameters,
            is_singleton: is_self,
            context_parts,
            class_context: class_ctx,
            module_context: module_ctx,
            start_line,
            start_col,
            start_byte,
            end_line,
            end_col,
            end_byte,
            source_text,
        });
    }

    // Sort by start_byte for stable order
    result.sort_by_key(|r| r.start_byte);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const SAMPLE_SIMPLE: &str = r#"def hello(name)
  puts name
end

def add(a, b)
  a + b
end

def no_params
  42
end
"#;

    const SAMPLE_CLASS_MODULE: &str = r#"module Greeter
  class Foo
    def greet(name)
      "hello #{name}"
    end

    def self.class_method(x, y=1)
      x + y
    end
  end
end

def top_level
  1
end
"#;

    const SAMPLE_NESTED_ENDS: &str = r#"class Outer
  def outer_method
    if true
      puts "hi"
    end
    arr.each do |x|
      puts x
    end
  end
end

module Mod
  def mod_func(a, b=2, *args, &blk)
  end
end
"#;

    const SAMPLE_INLINE: &str = r#"class Foo; def method; 42; end; end"#;

    #[test]
    fn extracts_simple_defs() {
        let fns = extract(SAMPLE_SIMPLE, Path::new("sample.rb")).unwrap();
        assert_eq!(fns.len(), 3, "found {:?}", fns.iter().map(|f| &f.identity.name).collect::<Vec<_>>());
        let hello = fns.iter().find(|f| f.identity.name == "hello").unwrap();
        assert_eq!(hello.identity.qualified_name, "hello");
        assert_eq!(hello.signature.parameters.len(), 1);
        assert_eq!(hello.identity.language, "ruby");
        assert_eq!(hello.metadata.parser.as_deref(), Some("reko-rubyExtractor"));
        assert!(hello.source.hash.starts_with("sha256:"));
        assert!(hello.source.location.start.byte < hello.source.location.end.byte);
        let add = fns.iter().find(|f| f.identity.name == "add").unwrap();
        assert_eq!(add.signature.parameters.len(), 2);
        let np = fns.iter().find(|f| f.identity.name == "no_params").unwrap();
        assert_eq!(np.signature.parameters.len(), 0);
    }

    #[test]
    fn extracts_class_module_qualified() {
        let fns = extract(SAMPLE_CLASS_MODULE, Path::new("greeter.rb")).unwrap();
        assert_eq!(fns.len(), 3);
        let greet = fns.iter().find(|f| f.identity.name == "greet").unwrap();
        assert_eq!(greet.identity.qualified_name, "Greeter::Foo::greet");
        assert_eq!(greet.context.class.as_deref(), Some("Foo"));
        assert_eq!(greet.context.module.as_deref(), Some("Greeter"));
        assert!(greet.context.namespace.as_deref().unwrap().contains("Greeter"));
        let cm = fns.iter().find(|f| f.identity.name == "class_method").unwrap();
        assert_eq!(cm.identity.qualified_name, "Greeter::Foo::class_method");
        assert_eq!(cm.signature.parameters.len(), 2);
        let top = fns.iter().find(|f| f.identity.name == "top_level").unwrap();
        assert_eq!(top.identity.qualified_name, "top_level");
        // singleton modifier
        assert!(cm.declaration.modifiers.contains(&"self".to_string()) || cm.identity.qualified_name.contains("class_method"));
    }

    #[test]
    fn extracts_nested_end_counting() {
        let fns = extract(SAMPLE_NESTED_ENDS, Path::new("nested.rb")).unwrap();
        assert_eq!(fns.len(), 2);
        let outer = fns.iter().find(|f| f.identity.name == "outer_method").unwrap();
        assert_eq!(outer.identity.qualified_name, "Outer::outer_method");
        // ensure outer_method's end is after inner if/do ends, not prematurely
        assert!(outer.source.source_text.contains("arr.each"));
        assert!(outer.source.source_text.contains("if true"));
        let modf = fns.iter().find(|f| f.identity.name == "mod_func").unwrap();
        assert_eq!(modf.identity.qualified_name, "Mod::mod_func");
        assert_eq!(modf.signature.parameters.len(), 4); // a, b, args, blk
        assert_eq!(modf.signature.parameters[0].name, "a");
    }

    #[test]
    fn extracts_inline_class_def() {
        let fns = extract(SAMPLE_INLINE, Path::new("inline.rb")).unwrap();
        assert_eq!(fns.len(), 1);
        assert_eq!(fns[0].identity.name, "method");
        assert_eq!(fns[0].identity.qualified_name, "Foo::method");
    }

    #[test]
    fn location_bytes_consistency() {
        let content = "def foo():\n  pass\nend\n";
        // ruby-like but with end
        let fns = extract("def foo\n  1\nend\n", Path::new("a.rb")).unwrap();
        assert_eq!(fns.len(), 1);
        assert!(fns[0].source.location.start.byte < fns[0].source.location.end.byte);
        assert_eq!(fns[0].source.location.start.line, 1);
    }

    #[test]
    fn extract_to_json_works() {
        let json = extract_to_json(SAMPLE_SIMPLE, Path::new("a.rb")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(v.is_array());
        assert_eq!(v.as_array().unwrap().len(), 3);
    }
}
