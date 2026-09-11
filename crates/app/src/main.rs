mod cli;
mod commands;
mod ui;

use clap::Parser;
use tracing_subscriber::filter::LevelFilter;

use cli::{Cli, Command};

fn level_filter(verbosity: u8) -> LevelFilter {
    match verbosity {
        0 => LevelFilter::WARN,
        1 => LevelFilter::INFO,
        2 => LevelFilter::DEBUG,
        _ => LevelFilter::TRACE,
    }
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(level_filter(cli.verbose))
        .without_time()
        .with_target(false)
        .init();

    if !matches!(
        cli.command,
        Command::Doctor { .. } | Command::Bootstrap { .. }
    ) && let Err(e) = mix_bootstrap::Environment::open().await
    {
        ui::fail(commands::doctor::check_failed_message(e));
        std::process::exit(1);
    }

    let result = match cli.command {
        Command::Bootstrap { mirror } => commands::bootstrap::run(mirror).await,
        Command::Doctor { fix, mirror } => commands::doctor::run(fix, mirror).await,
    };

    if let Err(e) = result {
        ui::fail(e);
        std::process::exit(1);
    }
}
