use crate::ir::IrFunction;
use crate::reader;
use anyhow::Result;
use std::path::Path;

/// Upper-layer orchestrator: reader -> javaExtractor -> JSON/IR
///
/// 1. Uses `reader::read_exact` to get exact file content preserving tabs/linebreaks
/// 2. Dispatches to `ExtractionFactory::javaExtractor` based on file extension (currently Java only)
/// 3. Returns Vec<IrFunction> or JSON string
pub struct Orchestrator;

impl Orchestrator {
    /// Extract IR from a file path. Auto-detects language via extension.
    pub fn extract_file(path: &Path) -> Result<Vec<IrFunction>> {
        let content = reader::read_exact(path)?;
        Self::extract_content(&content, path)
    }

    /// Extract IR from already-read content (good for testing, avoids double I/O)
    pub fn extract_content(content: &str, file_path: &Path) -> Result<Vec<IrFunction>> {
        let ext = file_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        match ext.as_str() {
            "java" => crate::ExtractionFactory::javaExtractor::extract(content, file_path),
            "py" | "python" => crate::ExtractionFactory::pythonExtractor::extract(content, file_path),
            "c" | "h" => crate::ExtractionFactory::cExtractor::extract(content, file_path),
            "cpp" | "cc" | "cxx" | "hpp" | "hh" => {
                crate::ExtractionFactory::cppExtractor::extract(content, file_path)
            }
            "php" => crate::ExtractionFactory::phpExtractor::extract(content, file_path),
            "js" | "mjs" | "cjs" => crate::ExtractionFactory::jsExtractor::extract(content, file_path),
            "jsx" => crate::ExtractionFactory::jsxExtractor::extract(content, file_path),
            "cs" => crate::ExtractionFactory::csharpExtractor::extract(content, file_path),
            "ts" | "mts" | "cts" => crate::ExtractionFactory::tsExtractor::extract(content, file_path),
            "tsx" => crate::ExtractionFactory::tsxExtractor::extract(content, file_path),
            "go" => crate::ExtractionFactory::goExtractor::extract(content, file_path),
            "rs" => crate::ExtractionFactory::rustExtractor::extract(content, file_path),
            "swift" => crate::ExtractionFactory::swiftExtractor::extract(content, file_path),
            "rb" => crate::ExtractionFactory::rubyExtractor::extract(content, file_path),
            "kt" | "kts" => crate::ExtractionFactory::kotlinExtractor::extract(content, file_path),
            other => anyhow::bail!("unsupported file type: .{other} (supported: .java .py .c .h .cpp .cc .cxx .hpp .php .js .mjs .cjs .jsx .cs .ts .mts .cts .tsx .go .rs .swift .rb .kt .kts)"),
        }
    }

    /// Returns pretty JSON string for CLI / serialization.
    #[allow(dead_code)]
    pub fn extract_file_to_json(path: &Path) -> Result<String> {
        let fns = Self::extract_file(path)?;
        Ok(serde_json::to_string_pretty(&fns)?)
    }

    #[allow(dead_code)]
    pub fn extract_content_to_json(content: &str, file_path: &Path) -> Result<String> {
        let fns = Self::extract_content(content, file_path)?;
        Ok(serde_json::to_string_pretty(&fns)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn orchestrator_via_reader_on_manual_test() {
        let path = Path::new("ManualTest/TestFunctions.java");
        if !path.exists() {
            // skip if not present in CI
            return;
        }
        let fns = Orchestrator::extract_file(path).unwrap();
        assert_eq!(fns.len(), 5);
        let names: Vec<_> = fns.iter().map(|f| f.identity.name.as_str()).collect();
        assert!(names.contains(&"add"));
        assert!(names.contains(&"factorial"));
        assert!(names.contains(&"isPrime"));
        assert!(names.contains(&"reverseString"));
        assert!(names.contains(&"findMax"));
        // Check location and hash populated
        for f in &fns {
            assert!(f.source.hash.starts_with("sha256:"));
            assert!(f.source.location.start.line >= 1);
            assert_eq!(f.identity.language, "java");
        }
    }

    #[test]
    fn orchestrator_in_memory() {
        let content = r#"package demo;
public class Foo {
    public int bar(int x) { return x; }
}"#;
        let fns =
            Orchestrator::extract_content(content, Path::new("Foo.java")).unwrap();
        assert_eq!(fns.len(), 1);
        assert_eq!(fns[0].identity.name, "bar");
    }
}
