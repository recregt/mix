mod cli;
mod commands;

use std::process::ExitCode;

use clap::Parser;

use cli::{Cli, Command};

pub async fn run() -> ExitCode {
    let cli = Cli::parse();

    mix_ui::init_tracing(cli.verbose, cli.draws_progress());

    if !matches!(
        cli.command,
        Command::Doctor | Command::Repair | Command::Bootstrap { .. } | Command::Install { .. }
    ) {
        let user_config = mix_app::resolve_existing_user_config();
        if let Some(report) = mix_app::doctor::audit(user_config.as_ref())
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
    }

    let result = match cli.command {
        Command::Bootstrap {
            mirror,
            mirror_key,
            force,
        } => commands::bootstrap::run(mirror, mirror_key, force).await,
        Command::Install {
            packages,
            mirror,
            mirror_key,
            json,
            build,
        } => commands::install::run(packages, mirror, mirror_key, json, build).await,
        Command::Doctor => commands::doctor::run(cli.verbose).await,
        Command::Repair => commands::repair::run().await,
    };

    match result {
        Ok(code) => code,
        Err(e) => {
            // The chain, not just the top of it: a library error explains itself in its sources.
            mix_ui::fail_error(&*e);
            ExitCode::FAILURE
        }
    }
}
