use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "mix", version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    Install,
    Doctor,
    Status,
}
