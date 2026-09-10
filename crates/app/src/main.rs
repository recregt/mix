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

    if !matches!(cli.command, Command::Doctor { .. } | Command::Bootstrap) {
        commands::doctor::report_health().await?;
    }

    match cli.command {
        Command::Bootstrap => commands::bootstrap::run().await,
        Command::Doctor { fix } => commands::doctor::run(fix).await,
    }
}
