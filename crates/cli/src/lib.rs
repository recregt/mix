mod cli;
mod commands;
mod controls;
pub mod explain;
mod remote;
pub mod render;

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
    if let Command::Events {
        command: cli::EventsCommand::Check { file },
    } = &cli.command
    {
        return check_events(file);
    }

    let view = render::sinks::View {
        output: cli.output,
        events_file: cli.events_file.clone(),
        verbose: cli.verbose,
        quiet: cli.quiet,
        exit: render::sinks::Exit::default(),
    };
    mix_ui::init(cli.draws_progress());

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
        Command::Repair
        | Command::Worker
        | Command::HomeFiles { .. }
        | Command::Explain { .. }
        | Command::Events { .. } => Box::new(explain::repair::explain),
    };

    let result = match &cli.command {
        Command::Bootstrap {
            mirror,
            mirror_key,
            force,
        } => {
            let mirror =
                commands::mirror_setting(mirror.as_deref(), mirror_key.as_deref(), |name| {
                    std::env::var(name).ok()
                });
            commands::bootstrap::run(mirror.url.as_deref(), mirror.key.as_deref(), *force, &view)
                .await
        }
        Command::Install { packages } => commands::install::run(packages, &view).await,
        Command::Remove { packages } => commands::remove::run(packages, &view).await,
        Command::Doctor => commands::doctor::run(&view).await,
        Command::Repair
        | Command::Worker
        | Command::HomeFiles { .. }
        | Command::Explain { .. }
        | Command::Events { .. } => commands::repair::run(&view).await,
    };

    if let Err(error) = &result
        && unstreamed(&view)
    {
        stream_the_failure(&cli.command, error, &view);
    }
    let from_root = view
        .exit
        .code()
        .map(|code| ExitCode::from(u8::try_from(code).unwrap_or(u8::MAX)));
    match result {
        Ok(code) => from_root.unwrap_or(code),
        Err(e) => {
            if cli.output == cli::Output::Human {
                let words = explain(&e);
                let (summary, hint) = words.parts();
                let fault = explain::fault_of(&e);
                let code = (cli.verbose > 0)
                    .then(|| fault.code())
                    .flatten()
                    .map(explain::codes::kebab);
                let mut causes = explain::evidence(&fault);
                for cause in mix_ui::causes_of(e.chain().nth(1), summary) {
                    if !causes.iter().any(|known| known.contains(&cause)) {
                        causes.push(cause);
                    }
                }
                mix_ui::report(
                    mix_ui::Severity::Error,
                    &mix_ui::Report {
                        code: code.as_deref(),
                        summary,
                        causes,
                        helps: hint.into_iter().collect(),
                    },
                );
            }
            from_root.unwrap_or(ExitCode::FAILURE)
        }
    }
}

fn unstreamed(view: &render::sinks::View) -> bool {
    view.streams() && !view.exit.started()
}

fn stream_the_failure(command: &Command, error: &anyhow::Error, view: &render::sinks::View) {
    use mix_events::v1::{
        BootstrapRequest, DoctorRequest, InstallRequest, RemoveRequest, RepairRequest,
        command::Request,
    };

    let Ok(sinks) = view.sinks(std::sync::Arc::new(mix_ui::Silent)) else {
        return;
    };
    let (key, request) = match command {
        Command::Bootstrap { mirror, force, .. } => (
            "bootstrap",
            Request::Bootstrap(BootstrapRequest {
                force: *force,
                mirror: mirror.clone(),
            }),
        ),
        Command::Install { packages } => (
            "install",
            Request::Install(InstallRequest {
                packages: packages.clone(),
            }),
        ),
        Command::Remove { packages } => (
            "remove",
            Request::Remove(RemoveRequest {
                packages: packages.clone(),
            }),
        ),
        Command::Doctor => ("doctor", Request::Doctor(DoctorRequest {})),
        Command::Repair
        | Command::Worker
        | Command::HomeFiles { .. }
        | Command::Explain { .. }
        | Command::Events { .. } => ("repair", Request::Repair(RepairRequest {})),
    };
    let outbox = std::sync::Arc::new(mix_events::Outbox::new(mix_shell::request_id(), || {}));
    let mut tree = mix_events::Tree::new(
        std::sync::Arc::clone(&outbox),
        std::sync::Arc::new(|| None),
        mix_events::Start::command(
            key,
            mix_events::v1::Command {
                mix_version: env!("CARGO_PKG_VERSION").to_string(),
                schema_minor: mix_events::SCHEMA_MINOR,
                request: Some(request),
            },
        ),
    );
    let ending: mix_events::Ending = explain::fault_of(error).into();
    let _ = tree.finish(mix_events::ROOT, ending.for_root(false));
    drop(tree);
    let mut sinks = sinks;
    for envelope in outbox.drain() {
        mix_shell::render::Render::envelope(&mut sinks, envelope);
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

fn check_events(path: &std::path::Path) -> ExitCode {
    let captured = std::fs::File::open(path)
        .map_err(|error| error.to_string())
        .and_then(|file| {
            mix_events::capture::read(std::io::BufReader::new(file))
                .map_err(|broken| broken.to_string())
        });
    let captured = match captured {
        Ok(captured) => captured,
        Err(reason) => {
            eprintln!("{}: {reason}", path.display());
            return ExitCode::FAILURE;
        }
    };
    match mix_events::validate(captured.envelopes.iter()) {
        Ok(validated) => {
            for entry in &validated.entries {
                println!("{} {:?}", entry.path, entry.outcome);
            }
            ExitCode::SUCCESS
        }
        Err(violation) => {
            eprintln!("{}: {violation:?}", path.display());
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use mix_events::v1::Code;
    use mix_events::v1::envelope::Event;

    use super::*;

    #[test]
    fn a_stream_that_already_started_is_left_as_it_ended_rather_than_given_a_second_root() {
        let directory = tempfile::tempdir().unwrap();
        let view = render::sinks::View {
            output: cli::Output::Human,
            events_file: Some(directory.path().join("events.ndjson")),
            verbose: 0,
            quiet: false,
            exit: render::sinks::Exit::default(),
        };
        assert!(unstreamed(&view));

        let mut sinks = view.sinks(std::sync::Arc::new(mix_ui::Silent)).unwrap();
        let outbox = std::sync::Arc::new(mix_events::Outbox::new("request", || {}));
        let tree = mix_events::Tree::new(
            std::sync::Arc::clone(&outbox),
            std::sync::Arc::new(|| None),
            mix_events::Start::command("repair", mix_events::v1::Command::default()),
        );
        for envelope in outbox.drain() {
            mix_shell::render::Render::envelope(&mut sinks, envelope);
        }

        assert!(!unstreamed(&view));
        assert_eq!(view.exit.code(), None);
        drop(tree);
    }

    #[test]
    fn a_failure_before_any_work_still_records_a_valid_stream_with_its_code() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("events.ndjson");
        let view = render::sinks::View {
            output: cli::Output::Human,
            events_file: Some(file.clone()),
            verbose: 0,
            quiet: false,
            exit: render::sinks::Exit::default(),
        };
        let refused = anyhow::Error::from(mix_shell::profile::change::Error::NotRoot);

        stream_the_failure(
            &Command::Install {
                packages: vec!["ripgrep".into()],
            },
            &refused,
            &view,
        );

        let captured =
            mix_events::capture::read(std::io::BufReader::new(std::fs::File::open(&file).unwrap()))
                .unwrap();
        mix_events::validate(captured.envelopes.iter()).unwrap();
        let root = captured
            .envelopes
            .iter()
            .find_map(|envelope| match &envelope.event {
                Some(Event::NodeFinished(finished)) if finished.id == mix_events::ROOT => {
                    Some(finished.clone())
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(root.diagnostic.unwrap().code(), Code::RootNotAllowed);
        assert_eq!(root.exit_code, mix_events::exit::FAILED);
        assert_eq!(view.exit.code(), Some(mix_events::exit::FAILED));
    }
}
