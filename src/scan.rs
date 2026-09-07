use crate::ir::IrFunction;
use crate::orchestrator::Orchestrator;
use anyhow::Result;
use ignore::WalkBuilder;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// One line in the JSONL output: file -> methods hierarchy.
/// Folders are implicit via `file` path sorted lexicographically.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FileRecord {
    /// Relative file path from scan root (Unix-style)
    pub file: String,
    /// Absolute path (for debugging)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub absolute: Option<String>,
    /// Detected language (e.g. "python", "java")
    pub language: String,
    /// Number of functions in this file (redundant convenience)
    pub count: usize,
    /// IRs for each function in this file
    pub functions: Vec<IrFunction>,
}

#[derive(Default, Debug, Clone)]
pub struct ScanStats {
    pub total_walked: usize,
    pub supported_files: usize,
    pub files_with_functions: usize,
    pub total_functions: usize,
    pub unsupported_files: usize,
    pub ignored_package_dirs: usize,
    pub errors: usize,
    pub unsupported_ext_counts: HashMap<String, usize>,
}

/// Hard-coded package/build dirs to always ignore even if not gitignored.
/// User said "you know better than me. i dont want package files analyzed"
const DEFAULT_IGNORED_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".reko",
    "node_modules",
    "target",
    "dist",
    "build",
    "vendor",
    "__pycache__",
    ".venv",
    "venv",
    ".idea",
    ".vscode",
    ".next",
    "out",
    "coverage",
    ".pytest_cache",
    ".mypy_cache",
    ".gradle",
    ".parcel-cache",
    ".turbo",
    ".tmp",
    ".cache",
    ".tox",
    ".eggs",
    ".cocoapods",
    "Pods",
    ".bundle",
    ".dart_tool",
];

const SUPPORTED_EXTS: &[&str] = &[
    "java", "py", "python", "c", "h", "cpp", "cc", "cxx", "hpp", "hh", "php", "js", "mjs", "cjs",
    "jsx", "cs", "ts", "mts", "cts", "tsx", "go", "rs", "swift", "rb", "kt", "kts",
];

fn is_supported(ext: &str) -> bool {
    SUPPORTED_EXTS.contains(&ext.to_lowercase().as_str())
}

fn language_for(ext: &str) -> String {
    match ext.to_lowercase().as_str() {
        "java" => "java",
        "py" | "python" => "python",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" => "cpp",
        "php" => "php",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "jsx",
        "cs" => "csharp",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "go" => "go",
        "rs" => "rust",
        "swift" => "swift",
        "rb" => "ruby",
        "kt" | "kts" => "kotlin",
        other => other,
    }
    .to_string()
}

fn is_ignored_dir_component(path: &Path, ignored: &HashSet<String>) -> bool {
    for comp in path.components() {
        if let std::path::Component::Normal(os) = comp {
            if let Some(s) = os.to_str() {
                if ignored.contains(s) {
                    return true;
                }
            }
        }
    }
    false
}

/// Collect files respecting .gitignore and package ignores.
/// Returns sorted vec of absolute paths that are supported extension.
pub fn collect_files(root: &Path, ignored_dirs: &HashSet<String>) -> (Vec<PathBuf>, ScanStats) {
    let mut stats = ScanStats::default();
    let mut files = Vec::new();
    let mut unsupported_counts: HashMap<String, usize> = HashMap::new();

    let walker = WalkBuilder::new(root)
        .hidden(false) // let .gitignore handling decide; we filter package dirs ourselves
        .git_ignore(true)
        .parents(true)
        .git_global(true)
        .git_exclude(true)
        .require_git(false)
        .follow_links(false)
        .standard_filters(true)
        .build();

    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => {
                stats.errors += 1;
                continue;
            }
        };
        let path = entry.path();

        // Skip directories; we only want files
        if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
            // Even for dirs, if dir is ignored package, WalkBuilder may still descend unless matched by gitignore.
            // We rely on is_ignored_dir_component to skip files inside, but also we could skip walking into them.
            // WalkBuilder will still recurse; filtering files later suffices.
            continue;
        }

        // Skip symlinks (paranoia: file_type symlink already excluded via follow_links false, but check)
        if entry.file_type().map(|ft| ft.is_symlink()).unwrap_or(false) {
            continue;
        }

        // Only regular files
        if !path.is_file() {
            continue;
        }

        stats.total_walked += 1;

        // Check ignored dir components relative to root
        let rel = path.strip_prefix(root).unwrap_or(path);
        if is_ignored_dir_component(rel, ignored_dirs) {
            stats.ignored_package_dirs += 1;
            continue;
        }
        // Also check absolute just in case (e.g. /tmp/.../node_modules)
        if is_ignored_dir_component(path, ignored_dirs) {
            stats.ignored_package_dirs += 1;
            continue;
        }

        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        if !is_supported(&ext) {
            stats.unsupported_files += 1;
            *unsupported_counts.entry(ext.clone()).or_insert(0) += 1;
            continue;
        }

        files.push(path.to_path_buf());
    }
    stats.unsupported_ext_counts = unsupported_counts;
    stats.supported_files = files.len();
    files.sort();
    (files, stats)
}

/// Parallel extract over files, returns FileRecords sorted hierarchically.
pub fn extract_files_parallel(
    root: &Path,
    files: Vec<PathBuf>,
) -> (Vec<FileRecord>, ScanStats) {
    let mut stats = ScanStats::default();
    // Use Arc Mutex for stats that need counting during parallel
    let unsupported: Arc<Mutex<HashMap<String, usize>>> = Arc::new(Mutex::new(HashMap::new()));
    let errors = Arc::new(Mutex::new(0usize));

    let records: Vec<Option<FileRecord>> = files
        .par_iter()
        .map(|path| {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase();
            let language = language_for(&ext);
            match Orchestrator::extract_file(path) {
                Ok(fns) => {
                    if fns.is_empty() {
                        // Still emit file? For hierarchy we can optionally skip empty files to reduce noise.
                        // We'll skip empty to keep JSONL concise.
                        None
                    } else {
                        Some(FileRecord {
                            file: rel,
                            absolute: Some(path.display().to_string()),
                            language,
                            count: fns.len(),
                            functions: fns,
                        })
                    }
                }
                Err(e) => {
                    eprintln!("warn: failed to extract {}: {}", path.display(), e);
                    if let Ok(mut m) = errors.lock() {
                        *m += 1
                    }
                    // Count unsupported-like errors separately? Already supported ext, so it's parse error.
                    None
                }
            }
        })
        .collect();

    let mut out: Vec<FileRecord> = records.into_iter().flatten().collect();
    out.sort_by(|a, b| a.file.cmp(&b.file));

    let total_functions: usize = out.iter().map(|r| r.count).sum();
    let files_with_functions = out.len();
    let err_cnt = errors.lock().map(|v| *v).unwrap_or(0);

    stats.supported_files = files.len();
    stats.files_with_functions = files_with_functions;
    stats.total_functions = total_functions;
    stats.errors = err_cnt;
    stats.unsupported_ext_counts = unsupported.lock().unwrap().clone();

    (out, stats)
}

/// High-level: walk + parallel extract, respecting ignores.
/// Show modern progress: spinner while exploring, bar while extracting (updated by workers).
pub fn scan_directory(root: &Path) -> Result<(Vec<FileRecord>, ScanStats)> {
    use indicatif::{ProgressBar, ProgressStyle};
    use std::time::Duration;

    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let ignored_dirs: HashSet<String> = DEFAULT_IGNORED_DIRS.iter().map(|s| s.to_string()).collect();

    // Phase 1: exploring repository (spinner)
    let explore_pb = ProgressBar::new_spinner();
    explore_pb.set_style(
        ProgressStyle::with_template("{spinner:.cyan} {msg} [{elapsed_precise}]")
            .unwrap()
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]),
    );
    explore_pb.set_message(format!("Exploring {}", root.display()));
    explore_pb.enable_steady_tick(Duration::from_millis(80));

    let (files, mut walk_stats) = collect_files(&root, &ignored_dirs);

    explore_pb.finish_with_message(format!(
        "Found {} source files ({} unsupported skipped, {} ignored dirs)",
        walk_stats.supported_files, walk_stats.unsupported_files, walk_stats.ignored_package_dirs
    ));

    // Phase 2: extracting (progress bar, updated by rayon workers)
    let (records, extract_stats) = if files.is_empty() {
        (Vec::new(), ScanStats::default())
    } else {
        let pb = ProgressBar::new(files.len() as u64);
        pb.set_style(
            ProgressStyle::with_template(
                "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({percent}%) {eta_precise} {msg}",
            )
            .unwrap()
            .progress_chars("█▉▊▋▌▍▎▏  "),
        );
        pb.set_message("Extracting...");
        // Share progress bar via Arc clone (ProgressBar is internally Arc, Clone is cheap)
        let pb_clone = pb.clone();
        let (recs, stats) = extract_files_parallel_with_progress(&root, files, pb_clone);
        pb.finish_with_message(format!(
            "Extracted {} files → {} functions",
            stats.files_with_functions, stats.total_functions
        ));
        (recs, stats)
    };

    // Merge stats
    walk_stats.files_with_functions = extract_stats.files_with_functions;
    walk_stats.total_functions = extract_stats.total_functions;
    walk_stats.errors = extract_stats.errors;
    // walk_stats already has unsupported and ignored counts

    Ok((records, walk_stats))
}

/// Variant used by spinner-bar flow to allow progress updates.
fn extract_files_parallel_with_progress(
    root: &Path,
    files: Vec<PathBuf>,
    pb: indicatif::ProgressBar,
) -> (Vec<FileRecord>, ScanStats) {
    let mut stats = ScanStats::default();
    let errors = Arc::new(Mutex::new(0usize));

    let records: Vec<Option<FileRecord>> = files
        .par_iter()
        .map(|path| {
            // Catch panics from extractors so one bad file doesn't crash all workers
            let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .replace('\\', "/");
                // Update progress message (truncate long paths)
                let display = if rel.len() > 48 {
                    format!("…{}", &rel[rel.len() - 47..])
                } else {
                    rel.clone()
                };
                pb.set_message(display);

                let ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                let language = language_for(&ext);
                match Orchestrator::extract_file(path) {
                    Ok(fns) => {
                        if fns.is_empty() {
                            None
                        } else {
                            Some(FileRecord {
                                file: rel,
                                absolute: Some(path.display().to_string()),
                                language,
                                count: fns.len(),
                                functions: fns,
                            })
                        }
                    }
                    Err(e) => {
                        pb.println(format!("warn: {}: {}", path.display(), e));
                        if let Ok(mut m) = errors.lock() {
                            *m += 1
                        }
                        None
                    }
                }
            }));
            let result = match res {
                Ok(v) => v,
                Err(_) => {
                    pb.println(format!(
                        "warn: panic while extracting {} (skipped)",
                        path.display()
                    ));
                    if let Ok(mut m) = errors.lock() {
                        *m += 1
                    }
                    None
                }
            };
            pb.inc(1);
            result
        })
        .collect();

    let mut out: Vec<FileRecord> = records.into_iter().flatten().collect();
    out.sort_by(|a, b| a.file.cmp(&b.file));

    let total_functions: usize = out.iter().map(|r| r.count).sum();
    let files_with_functions = out.len();
    let err_cnt = errors.lock().map(|v| *v).unwrap_or(0);

    stats.supported_files = files.len();
    stats.files_with_functions = files_with_functions;
    stats.total_functions = total_functions;
    stats.errors = err_cnt;

    (out, stats)
}

/// Resolve output path:
/// - if `output` is None => <root>/reko.jsonl
/// - if `output` exists as dir => <output>/reko.jsonl
/// - if `output` has no extension and not existing but parent exists and name has no dot => treat as dir
/// - otherwise treat as file.
pub fn resolve_output_path(root: &Path, output: Option<&Path>) -> PathBuf {
    match output {
        None => root.join("reko.jsonl"),
        Some(p) => {
            // If p exists and is dir
            if p.exists() && p.is_dir() {
                return p.join("reko.jsonl");
            }
            // If p ends with / or has no file extension and looks like dir intent
            // Heuristic: if extension is empty and path does not contain '.' in file name, and parent exists
            let has_ext = p.extension().is_some();
            if !has_ext {
                // If parent is existing dir, or p ends with separator, treat as dir
                // Also if user explicitly passed folder path like "out/" or "results"
                // We'll treat extension-less as dir if not ending with .jsonl/.json
                // Simpler: if p is like "output" (no dot) and not existing, still consider dir if last component has no dot
                // But to avoid ambiguity, if user passes "my.jsonl" it has ext, handled above.
                // So no ext => dir
                return p.join("reko.jsonl");
            }
            // Has extension => file
            // If parent doesn't exist, we will create it later
            p.to_path_buf()
        }
    }
}

/// Write JSONL: one line per FileRecord (folders->files->methods hierarchy)
pub fn write_jsonl(path: &Path, records: &[FileRecord]) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    use std::io::{BufWriter, Write};
    let file = std::fs::File::create(path)?;
    let mut w = BufWriter::new(file);
    for rec in records {
        let line = serde_json::to_string(rec)?;
        writeln!(w, "{}", line)?;
    }
    w.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn ignores_package_dirs_and_gitignore() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // Create ignored dirs
        fs::create_dir_all(root.join("node_modules/foo")).unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join("node_modules/foo/bar.js"), "function foo(){return <div/>}").unwrap();
        fs::write(root.join("src/app.py"), "def hello():\n    pass\n").unwrap();
        fs::write(root.join("src/skip.pyc"), "binary").unwrap();
        fs::write(root.join(".gitignore"), "src/skip.pyc\n").unwrap();
        fs::write(root.join("README.md"), "# hi").unwrap();

        let (records, stats) = scan_directory(root).unwrap();
        // src/app.py should be found (python), node_modules ignored, README unsupported, skip.pyc ignored via .gitignore
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].file, "src/app.py");
        assert!(stats.ignored_package_dirs >= 1);
        assert!(stats.unsupported_files >= 1);
    }

    #[test]
    fn hierarchical_sort() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("b")).unwrap();
        fs::create_dir_all(root.join("a")).unwrap();
        fs::write(root.join("b/file.py"), "def foo():\n    pass\n").unwrap();
        fs::write(root.join("a/file.py"), "def bar():\n    pass\n").unwrap();
        let (records, _) = scan_directory(root).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].file, "a/file.py");
        assert_eq!(records[1].file, "b/file.py");
    }

    #[test]
    fn resolve_output_file_vs_dir() {
        let root = Path::new("/tmp/repo");
        assert_eq!(resolve_output_path(root, None), Path::new("/tmp/repo/reko.jsonl"));
        assert_eq!(
            resolve_output_path(root, Some(Path::new("/tmp/out"))),
            Path::new("/tmp/out/reko.jsonl")
        );
        assert_eq!(
            resolve_output_path(root, Some(Path::new("/tmp/out/reko.jsonl"))),
            Path::new("/tmp/out/reko.jsonl")
        );
    }
}
