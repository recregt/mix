mod cli;
mod commands;

use std::process::ExitCode;

use clap::Parser;

use cli::{Cli, Command};

pub async fn run() -> ExitCode {
    let cli = Cli::parse();

    mix_ui::init_tracing(cli.verbose);

    if !matches!(
        cli.command,
        Command::Doctor | Command::Repair | Command::Bootstrap { .. }
    ) && let Some(report) = mix_app::doctor::audit()
        .await
        .into_iter()
        .find(|r| !r.healthy)
    {
        let detail = report.detail.as_deref().unwrap_or("unhealthy");
        mix_ui::fail(commands::doctor::check_failed_message(format!(
            "{}: {detail}",
            report.name
        )));
        return ExitCode::FAILURE;
    }

    let result = match cli.command {
        Command::Bootstrap { mirror, force } => commands::bootstrap::run(mirror, force).await,
        Command::Doctor => commands::doctor::run(cli.verbose).await,
        Command::Repair => commands::repair::run().await,
    };

    match result {
        Ok(code) => code,
        Err(e) => {
            mix_ui::fail(e);
            ExitCode::FAILURE
        }
    }
}
