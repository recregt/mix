use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "mix", version, about = "Reproducible systems, made effortless")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Initialize runtime and system dependencies
    Bootstrap,

    /// Inspect system health, or repair configuration drift
    Doctor {
        /// Restore the managed system state to a pristine condition (requires sudo)
        #[arg(long)]
        fix: bool,
    },
}
