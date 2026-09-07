use clap::{Parser, Subcommand};
use clap_verbosity_flag::Verbosity;

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
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Initialize logging level from verbosity flag (optional: add env_logger/log later)
    // e.g. log::debug!("verbosity: {:?}", cli.verbose);

    match cli.command {
        Some(Commands::Hello { name }) => {
            println!("Hello, {name}!");
        }
        None => {
            println!("Hello, world! Try `reko hello --help` or `reko --help`");
        }
    }

    Ok(())
}
