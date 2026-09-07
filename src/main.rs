use clap::{Parser, Subcommand};
use clap_verbosity_flag::Verbosity;
use std::path::PathBuf;

#[allow(non_snake_case)]
mod ir;
mod orchestrator;
mod reader;
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
    /// Extract functions from file via reader -> javaExtractor -> IR JSON
    Extract {
        /// Path to source file (currently .java only)
        path: PathBuf,

        /// Output file (default stdout)
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Compact JSON (default pretty)
        #[arg(long)]
        compact: bool,
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
            output,
            compact,
        }) => {
            // Upper layer orchestrator: reader -> javaExtractor
            let fns = orchestrator::Orchestrator::extract_file(&path)?;
            let json = if compact {
                serde_json::to_string(&fns)?
            } else {
                serde_json::to_string_pretty(&fns)?
            };
            if let Some(out_path) = output {
                std::fs::write(&out_path, &json)?;
                println!("Wrote {} functions to {}", fns.len(), out_path.display());
            } else {
                println!("{json}");
            }
        }
        None => {
            println!("Hello, world! Try `reko hello --help` or `reko --help`");
        }
    }

    Ok(())
}
