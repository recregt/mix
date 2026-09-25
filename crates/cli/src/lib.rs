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
        Command::Doctor
            | Command::Repair
            | Command::Bootstrap { .. }
            | Command::Install { .. }
            | Command::Remove { .. }
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
    let explain: Box<dyn Fn(&anyhow::Error) -> Diagnostic + '_> = match &cli.command {
        Command::Bootstrap { .. } => Box::new(explain::bootstrap::explain),
        Command::Install { packages, .. } => Box::new(|error| {
            explain::install::explain(
                error,
                packages,
                &explain::install::rerun_with_build(std::env::args()),
            )
        }),
        Command::Remove { packages, .. } => {
            Box::new(|error| explain::remove::explain(error, packages))
        }
        Command::Doctor => Box::new(explain::doctor::explain),
        Command::Repair => Box::new(explain::repair::explain),
    };

    let result = match &cli.command {
        Command::Bootstrap {
            mirror,
            mirror_key,
            force,
        } => commands::bootstrap::run(mirror.as_deref(), mirror_key.as_deref(), *force).await,
        Command::Install {
            packages,
            mirror,
            mirror_key,
            json,
            build,
        } => {
            commands::install::run(
                packages,
                mirror.as_deref(),
                mirror_key.as_deref(),
                *json,
                *build,
            )
            .await
        }
        Command::Remove {
            packages,
            mirror,
            mirror_key,
            json,
        } => commands::remove::run(packages, mirror.as_deref(), mirror_key.as_deref(), *json).await,
        Command::Doctor => commands::doctor::run(cli.verbose).await,
        Command::Repair => commands::repair::run().await,
    };

    match result {
        Ok(code) => code,
        Err(e) => {
            let message = explain(&e).message();
            if cli.verbose > 0 {
                mix_ui::fail_in_detail(&message, &*e);
            } else {
                mix_ui::fail(message);
            }
            ExitCode::FAILURE
        }
    }
}
