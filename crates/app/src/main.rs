mod cli;
mod commands;
mod ui;

use clap::Parser;

use cli::{Cli, Command};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    if !matches!(cli.command, Command::Doctor { .. } | Command::Bootstrap)
        && let Err(e) = mix_bootstrap::Environment::open().await
    {
        ui::fail(commands::doctor::check_failed_message(e));
        std::process::exit(1);
    }

    let result = match cli.command {
        Command::Bootstrap => commands::bootstrap::run().await,
        Command::Doctor { fix } => commands::doctor::run(fix).await,
    };

    if let Err(e) = result {
        ui::fail(e);
        std::process::exit(1);
    }
}
