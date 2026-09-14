mod cli;
mod commands;

use mix_ui as ui;

use std::process::ExitCode;

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

pub async fn run() -> ExitCode {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(level_filter(cli.verbose))
        .without_time()
        .with_target(false)
        .init();

    if !matches!(
        cli.command,
        Command::Doctor | Command::Repair | Command::Bootstrap { .. }
    ) && let Some(report) = mix_app::doctor::audit()
        .await
        .into_iter()
        .find(|r| !r.healthy)
    {
        let detail = report.detail.as_deref().unwrap_or("unhealthy");
        ui::fail(commands::doctor::check_failed_message(format!(
            "{}: {detail}",
            report.name
        )));
        return ExitCode::FAILURE;
    }

    let result = match cli.command {
        Command::Bootstrap { mirror } => commands::bootstrap::run(mirror).await,
        Command::Doctor => commands::doctor::run().await,
        Command::Repair => commands::repair::run().await,
    };

    match result {
        Ok(code) => code,
        Err(e) => {
            ui::fail(e);
            ExitCode::FAILURE
        }
    }
}
