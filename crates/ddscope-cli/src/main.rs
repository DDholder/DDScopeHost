//! Headless benchmark, replay verification, and diagnostics CLI for `ddscope-core`.

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "ddscope-cli", version, about = "DDScopeHost headless engine & benchmark CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Print core engine build metadata.
    Info,
    /// Run headless acquisition & M4 throughput benchmarks (implemented in T04).
    Bench {
        /// Number of simulated signal sources.
        #[arg(long, default_value_t = 1)]
        sources: usize,
        /// Target aggregate sample rate in samples/sec.
        #[arg(long, default_value_t = 5_000_000)]
        rate: u64,
    },
    /// Verify offline raw file replay integrity (implemented in Track B / T04).
    ReplayVerify {
        /// Path to the raw capture file.
        #[arg(long)]
        input: String,
    },
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Info => {
            let info = ddscope_core::build_info();
            println!(
                "{} v{} (plugin-api v{})",
                info.name, info.version, info.plugin_api_version
            );
        }
        Commands::Bench { sources, rate } => {
            eprintln!(
                "ddscope-cli bench (sources={sources}, rate={rate}) is scheduled for T04."
            );
            std::process::exit(2);
        }
        Commands::ReplayVerify { input } => {
            eprintln!(
                "ddscope-cli replay-verify (input={input}) is scheduled for Track B / T04."
            );
            std::process::exit(2);
        }
    }
}
