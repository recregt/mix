mod cli;
mod commands;

use clap::Parser;

use cli::{Cli, Command};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    if !matches!(
        cli.command,
        Command::Doctor | Command::Bootstrap | Command::Status
    ) && let Err(e) = mix_bootstrap::Environment::open().await
    {
        eprintln!("{e}\n\nrun `mix doctor` to reset mix's managed state.");
        std::process::exit(1);
    }

    match cli.command {
        Command::Bootstrap => commands::bootstrap::run().await,
        Command::Doctor => commands::doctor::run().await,
        Command::Status => commands::status::run().await,
    }
}
