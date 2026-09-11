use clap::{Parser, Subcommand};
use clap_verbosity_flag::Verbosity;
use std::path::{Path, PathBuf};

#[allow(non_snake_case)]
mod ir;
mod orchestrator;
mod reader;
mod scan;
mod embed;
mod index;
mod model;
mod search;
mod update;
#[allow(non_snake_case)]
mod ExtractionFactory;

#[derive(Parser, Debug)]
#[command(name = "reko", version, about = "Cross-platform CLI for Windows/Linux/macOS", long_about = None)]
struct Cli {
    #[command(flatten)]
    verbose: Verbosity,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Example subcommand
    Hello {
        /// Name to greet
        #[arg(default_value = "world")]
        name: String,
    },
    /// Read a file and print exact content with line numbers (tabs/spaces/linebreaks preserved)
    Read {
        /// Path to file
        path: PathBuf,

        /// Show raw bytes mode (hex dump) for non-UTF8 files
        #[arg(long)]
        raw: bool,

        /// Always end output with newline (default preserves exact semantics)
        #[arg(long)]
        ensure_newline: bool,
    },
    /// Alias for `read`
    #[command(name = "cat")]
    Cat {
        /// Path to file
        path: PathBuf,
    },
    /// Extract functions from file/dir via reader -> extractors -> IR JSON
    /// When path is a directory: walks recursively (respects .gitignore, skips package dirs), parallel extraction, writes single JSONL hierarchically (folders->files->methods).
    Extract {
        /// Path to source file or directory (default "."). Also supports --path alias.
        #[arg(default_value = ".", value_name = "PATH")]
        path: PathBuf,

        /// Alias for `path` so you can do `reko extract --path <dir>` from anywhere
        #[arg(long = "path", hide = true)]
        path_alias: Option<PathBuf>,

        /// Output file or folder (default: <root>/.reko/reko.jsonl for dirs, stdout for single file). If folder, writes reko.jsonl inside it.
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Compact JSON (single line per function/file; default pretty for single-file, JSONL for dirs)
        #[arg(long)]
        compact: bool,
    },
    /// Index .reko/reko.jsonl into .reko/reko.db with sqlite-vec + EmbeddingGemma
    /// Checks if .reko/reko.jsonl exists, else runs extract pipeline first.
    Index {
        /// Path to repository root (default "."). Also supports --path alias.
        #[arg(default_value = ".", value_name = "PATH")]
        path: PathBuf,

        #[arg(long = "path", hide = true)]
        path_alias: Option<PathBuf>,

        /// Model directory (default: auto-resolve ./model then ~/.cache/reko/model with auto-download)
        #[arg(long)]
        model: Option<PathBuf>,

        /// Force re-index even if DB exists
        #[arg(long)]
        force: bool,
    },
    /// Semantic find over indexed code (sqlite-vec + EmbeddingGemma)
    Find {
        /// Query text (natural language or code)
        query: String,

        /// Show top N results (default 5)
        #[arg(long, short = 'n', default_value = "5")]
        top: usize,

        /// Repository path (default ".")
        #[arg(long, short, default_value = ".")]
        path: PathBuf,

        /// Model directory (default auto)
        #[arg(long)]
        model: Option<PathBuf>,
    },
    /// Update .reko/reko.jsonl and .reko/reko.db incrementally (copy old, re-extract, diff by hash, re-embed modified)
    Update {
        /// Repository path (default ".")
        #[arg(default_value = ".", value_name = "PATH")]
        path: PathBuf,

        #[arg(long = "path", hide = true)]
        path_alias: Option<PathBuf>,

        /// Model directory (default auto)
        #[arg(long)]
        model: Option<PathBuf>,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Initialize logging level from verbosity flag (optional: add env_logger/log later)
    // e.g. log::debug!("verbosity: {:?}", cli.verbose);

    match cli.command {
        Some(Commands::Hello { name }) => {
            println!("Hello, {name}!");
        }
        Some(Commands::Read {
            path,
            raw,
            ensure_newline,
        }) => {
            if raw {
                let bytes = reader::read_bytes_exact(&path)?;
                // hex + ascii like `xxd` but exact bytes preserved
                for (i, chunk) in bytes.chunks(16).enumerate() {
                    print!("{i:08x}: ");
                    for b in chunk {
                        print!("{b:02x} ");
                    }
                    println!();
                }
            } else {
                let content = reader::read_exact(&path)?;
                let out = if ensure_newline {
                    reader::format_with_line_numbers_always_newline(&content)
                } else {
                    reader::format_with_line_numbers(&content)
                };
                print!("{out}");
                // Ensure flush for exact semantics - no extra newline unless content had it
                use std::io::Write;
                std::io::stdout().flush().ok();
            }
        }
        Some(Commands::Cat { path }) => {
            let content = reader::read_exact(&path)?;
            print!("{}", reader::format_with_line_numbers(&content));
        }
        Some(Commands::Extract {
            path,
            path_alias,
            output,
            compact,
        }) => {
            let target = path_alias.as_ref().unwrap_or(&path).clone();
            let target = if target.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                target
            };

            if target.is_dir() {
                // Directory mode: parallel walk + JSONL export hierarchically
                // Progress: explore spinner + extracting bar handled inside scan_directory (modern, worker-updated)
                let root = target.canonicalize().unwrap_or(target.clone());

                let (records, stats) = scan::scan_directory(&root)?;

                // Resolve output
                let out_path = scan::resolve_output_path(&root, output.as_deref());

                // Write JSONL (one line per file: {file,language,count,functions})
                scan::write_jsonl(&out_path, &records)?;

                // Reports
                eprintln!(
                    "Done: {} file(s) with functions, {} total functions, {} unsupported file(s) skipped, {} ignored package entries, {} errors.",
                    stats.files_with_functions,
                    stats.total_functions,
                    stats.unsupported_files,
                    stats.ignored_package_dirs,
                    stats.errors
                );
                if !stats.unsupported_ext_counts.is_empty() {
                    let mut v: Vec<_> = stats.unsupported_ext_counts.iter().collect();
                    v.sort_by_key(|(k, _)| *k);
                    let detail: Vec<String> = v
                        .into_iter()
                        .map(|(ext, cnt)| {
                            let label = if ext.is_empty() { "(no ext)" } else { ext };
                            format!("{}:{} ", label, cnt)
                        })
                        .collect();
                    eprintln!("Unsupported breakdown: {}", detail.join(""));
                }
                eprintln!("Wrote JSONL ({} lines) to {}", records.len(), out_path.display());
                // Also print path to stdout for scripting?
                if output.is_none() {
                    // hint
                    eprintln!("Hint: use --output <file> to customize location");
                }
            } else if target.is_file() {
                // Single-file mode (legacy)
                let fns = orchestrator::Orchestrator::extract_file(&target)?;
                let json = if compact {
                    serde_json::to_string(&fns)?
                } else {
                    serde_json::to_string_pretty(&fns)?
                };
                if let Some(out_path) = output {
                    // If output path is a directory, write inside it; otherwise treat as file
                    let out_path_ref: &Path = out_path.as_path();
                    let final_path = if out_path_ref.exists() && out_path_ref.is_dir() {
                        out_path_ref.join("reko.json")
                    } else if out_path_ref.extension().is_none() && !out_path_ref.to_string_lossy().contains('.') {
                        // Heuristic: no extension => folder intent, but fallback to file write directly if not dir
                        // Keep as is if user passed explicit file without ext? We'll treat as file.
                        out_path.clone()
                    } else {
                        out_path.clone()
                    };
                    if let Some(parent) = final_path.parent() {
                        if !parent.as_os_str().is_empty() {
                            std::fs::create_dir_all(parent)?;
                        }
                    }
                    std::fs::write(&final_path, &json)?;
                    println!("Wrote {} functions to {}", fns.len(), final_path.display());
                } else {
                    println!("{json}");
                }
            } else {
                anyhow::bail!("path does not exist: {}", target.display());
            }
        }
        Some(Commands::Index {
            path,
            path_alias,
            model,
            force,
        }) => {
            let target = path_alias.as_ref().unwrap_or(&path).clone();
            let target = if target.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                target
            };
            let root = if target.is_file() {
                target.parent().unwrap_or_else(|| Path::new(".")).to_path_buf()
            } else {
                target.clone()
            };
            let root = root.canonicalize().unwrap_or(root);
            let db_path = root.join(".reko").join("reko.db");
            if db_path.exists() && !force {
                eprintln!("Found existing DB at {} (use --force to re-index)", db_path.display());
                // Quick check count
                if let Ok(conn) = index::open_db(&db_path) {
                    let cnt: i64 = conn
                        .query_row("SELECT COUNT(*) FROM vec_index", [], |r| r.get(0))
                        .unwrap_or(0);
                    eprintln!(" vec_index contains {cnt} vectors");
                }
            } else {
                let model_display = model.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "auto (global ~/.cache/reko/model)".to_string());
                eprintln!("Indexing {} → {} (model: {})", root.display(), db_path.display(), model_display);
                let out = index::index_directory_with_model(&root, model.as_deref())?;
                eprintln!("Indexed → {}", out.display());
            }
        }
        Some(Commands::Find {
            query,
            top,
            path,
            model,
        }) => {
            let root = path.canonicalize().unwrap_or(path);
            search::search_and_print(&root, &query, top, model.as_deref())?;
        }
        Some(Commands::Update {
            path,
            path_alias,
            model,
        }) => {
            let target = path_alias.as_ref().unwrap_or(&path).clone();
            let root = target.canonicalize().unwrap_or(target);
            update::update_directory(&root, model.as_deref())?;
        }
        None => {
            println!("Hello, world! Try `reko hello --help` or `reko --help`");
        }
    }

    Ok(())
}
