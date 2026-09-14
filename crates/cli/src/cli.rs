use clap::{ArgAction, Parser, Subcommand};

#[derive(Parser)]
#[command(name = "mix", version, about = "Reproducible systems, made effortless")]
pub struct Cli {
    /// Increase log verbosity (-v steps, -vv commands, -vvv command output)
    #[arg(short, long, action = ArgAction::Count, global = true)]
    pub verbose: u8,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Initialize runtime and system dependencies
    Bootstrap {
        /// Alternate base URL to fetch the pinned Nix archive from (e.g. an internal mirror)
        #[arg(long, env = "MIX_NIX_MIRROR")]
        mirror: Option<String>,
    },

    /// Inspect system health
    Doctor,

    /// Repair configuration drift
    Repair,
}
