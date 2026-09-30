mod cli;
mod commands;
mod controls;
pub mod explain;
mod remote;

use std::io::Write as _;
use std::process::ExitCode;

use clap::Parser;

use cli::{Cli, Command};
use explain::Diagnostic;

pub async fn run() -> ExitCode {
    let cli = Cli::parse();
    if matches!(cli.command, Command::Worker) {
        return remote::worker::run().await;
    }
    if let Command::HomeFiles { request } = &cli.command {
        return home_files(request);
    }
    if let Command::Explain { code } = &cli.command {
        return explain_code(code);
    }

    mix_ui::init_tracing(cli.verbose, cli.draws_progress());

    if !matches!(
        cli.command,
        Command::Doctor
            | Command::Repair
            | Command::Bootstrap { .. }
            | Command::Install { .. }
            | Command::Remove { .. }
    ) {
        let ctx =
            mix_shell::Context::new(mix_exec::Scope::root()).with_user(commands::enrolled_user());
        if let Some(report) = mix_shell::ops::doctor::audit(&ctx)
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
        Command::Install { packages, .. } => {
            Box::new(|error| explain::install::explain(error, packages))
        }
        Command::Remove { packages, .. } => {
            Box::new(|error| explain::remove::explain(error, packages))
        }
        Command::Doctor => Box::new(explain::doctor::explain),
        Command::Repair | Command::Worker | Command::HomeFiles { .. } | Command::Explain { .. } => {
            Box::new(explain::repair::explain)
        }
    };

    let result = match &cli.command {
        Command::Bootstrap {
            mirror,
            mirror_key,
            force,
        } => {
            commands::bootstrap::run(
                mirror.as_deref(),
                mirror_key.as_deref(),
                *force,
                cli.verbose,
            )
            .await
        }
        Command::Install { packages, json } => commands::install::run(packages, *json).await,
        Command::Remove { packages, json } => commands::remove::run(packages, *json).await,
        Command::Doctor => commands::doctor::run(cli.verbose).await,
        Command::Repair | Command::Worker | Command::HomeFiles { .. } | Command::Explain { .. } => {
            commands::repair::run(cli.verbose).await
        }
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

fn home_files(request: &str) -> ExitCode {
    let input = std::io::BufReader::new(std::io::stdin());
    let output = std::io::BufWriter::new(std::io::stdout());
    match mix_shell::effect::home::serve(request, input, output) {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            let _ = writeln!(std::io::stderr(), "{failure:?}");
            ExitCode::FAILURE
        }
    }
}

fn explain_code(name: &str) -> ExitCode {
    match explain::codes::parse(name) {
        Some(code) => {
            println!(
                "{}\n\n{}",
                explain::codes::name(code),
                explain::codes::long(code)
            );
            ExitCode::SUCCESS
        }
        None => {
            eprintln!("`{name}` is not a code `mix` uses. Codes look like LOCKED or NETWORK.");
            ExitCode::from(2)
        }
    }
}
