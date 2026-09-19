mod cli;
mod commands;
pub mod explain;

use std::process::ExitCode;

use clap::Parser;

use cli::{Cli, Command};
use explain::Diagnostic;

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
            .find(|report| !report.healthy())
        {
            mix_ui::fail(explain::doctor::blocked(&report).message());
            return ExitCode::FAILURE;
        }
    }

    // Which command was run is what decides how a failure should read, so the words are picked
    // before it runs: every crate below this one raises facts, and this is where they are put
    // into a sentence.
    let explain: fn(&anyhow::Error) -> Diagnostic = match &cli.command {
        Command::Bootstrap { .. } => explain::bootstrap::explain,
        Command::Install { .. } => explain::install::explain,
        Command::Doctor => explain::doctor::explain,
        Command::Repair => explain::repair::explain,
    };

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
            // The words this command chose, and the chain behind them: a library error explains
            // itself in its sources, and the sentence above it does not repeat them.
            mix_ui::fail_explained(&explain(&e).message(), &*e);
            ExitCode::FAILURE
        }
    }
}
