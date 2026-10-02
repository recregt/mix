mod cli;
mod client;
mod commands;
mod controls;
pub mod explain;
pub mod render;

use std::process::ExitCode;

use cli::{Cli, Command};

pub async fn run() -> ExitCode {
    let cli = Cli::parse_with_color();
    mix_ui::set_color(match cli.color {
        cli::Color::Auto => mix_ui::ColorChoice::Auto,
        cli::Color::Always => mix_ui::ColorChoice::Always,
        cli::Color::Never => mix_ui::ColorChoice::Never,
    });
    if let Command::Explain { code, list } = &cli.command {
        if *list {
            mix_ui::data(&explain::codes::list_text());
            return ExitCode::SUCCESS;
        }
        return explain_code(code.as_deref().unwrap_or_default());
    }
    if let Command::Events { command } = &cli.command {
        return match command {
            cli::EventsCommand::Check { file } => check_events(file),
            cli::EventsCommand::Show { file, node } => show_events(
                file,
                node.as_deref(),
                render::sinks::level(cli.quiet, cli.verbose),
            ),
        };
    }

    let view = render::sinks::View {
        output: cli.output,
        events_file: cli.events_file.clone(),
        verbose: cli.verbose,
        quiet: cli.quiet,
        exit: render::sinks::Exit::default(),
    };
    mix_ui::init(cli.draws_progress());

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
        Command::Clean { all } => commands::clean::run(*all, &view).await,
        Command::Doctor => commands::doctor::run(&view).await,
        Command::Repair | Command::Explain { .. } | Command::Events { .. } => {
            commands::repair::run(&view).await
        }
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
            if cli.output == cli::Output::Human && view.exit.code().is_none() {
                let words = explain::words(&e, &request_of(&cli.command));
                let fault = explain::fault_of(&e);
                let code = (cli.verbose > 0)
                    .then(|| fault.code())
                    .flatten()
                    .map(explain::codes::kebab);
                let mut causes = explain::evidence(&fault);
                for cause in mix_ui::causes_of(e.chain().nth(1), words.summary_text()) {
                    if !causes.iter().any(|known| known.contains(&cause)) {
                        causes.push(cause);
                    }
                }
                mix_ui::report(
                    mix_ui::Severity::Error,
                    &words.report().code(code.as_deref()).causes(causes),
                );
            }
            from_root.unwrap_or(ExitCode::FAILURE)
        }
    }
}

fn unstreamed(view: &render::sinks::View) -> bool {
    view.streams() && !view.exit.started()
}

fn request_of(command: &Command) -> mix_events::v1::command::Request {
    use mix_events::v1::{
        BootstrapRequest, CleanRequest, DoctorRequest, InstallRequest, RemoveRequest,
        RepairRequest, command::Request,
    };

    match command {
        Command::Bootstrap {
            mirror,
            mirror_key,
            force,
        } => Request::Bootstrap(Box::new(BootstrapRequest {
            force: *force,
            mirror: mirror.clone(),
            mirror_key: mirror_key.clone(),
        })),
        Command::Install { packages } => Request::Install(InstallRequest {
            packages: packages.clone(),
        }),
        Command::Remove { packages } => Request::Remove(RemoveRequest {
            packages: packages.clone(),
        }),
        Command::Clean { all } => Request::Clean(CleanRequest { all: *all }),
        Command::Doctor => Request::Doctor(DoctorRequest {}),
        Command::Repair | Command::Explain { .. } | Command::Events { .. } => {
            Request::Repair(RepairRequest {})
        }
    }
}

fn stream_the_failure(command: &Command, error: &anyhow::Error, view: &render::sinks::View) {
    let Ok(mut sinks) = view.sinks(std::sync::Arc::new(mix_ui::Silent)) else {
        return;
    };
    mix_events::fail(
        mix_events::command(request_of(command)),
        explain::fault_of(error),
        &mut sinks,
    );
}

fn explain_code(name: &str) -> ExitCode {
    match explain::codes::parse(name) {
        Some(code) => {
            mix_ui::data(&explain::codes::explanation_text(code));
            ExitCode::SUCCESS
        }
        None => {
            mix_ui::report(
                mix_ui::Severity::Error,
                &mix_ui::Report::new(&mix_ui::phrase!("`{name}` isn't a code `mix` uses"))
                    .note(&mix_ui::note!("codes look like `locked` or `network`")),
            );
            ExitCode::from(u8::try_from(mix_events::exit::USAGE).unwrap_or(u8::MAX))
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
            unreadable(path, reason);
            return ExitCode::FAILURE;
        }
    };
    match mix_events::validate(captured.envelopes.iter()) {
        Ok(validated) => {
            for entry in &validated.entries {
                mix_ui::data(&format!("{} {}", entry.path, outcome_words(entry.outcome)));
            }
            ExitCode::SUCCESS
        }
        Err(violation) => {
            unreadable(path, violation.to_string());
            ExitCode::FAILURE
        }
    }
}

fn show_events(path: &std::path::Path, node: Option<&str>, level: mix_events::Detail) -> ExitCode {
    let captured = std::fs::File::open(path)
        .map_err(|error| error.to_string())
        .and_then(|file| {
            mix_events::capture::read(std::io::BufReader::new(file))
                .map_err(|broken| broken.to_string())
        });
    match captured {
        Ok(captured) => {
            render::replay::show(&captured, level, node, std::sync::Arc::new(mix_ui::Stdout));
            ExitCode::SUCCESS
        }
        Err(reason) => {
            unreadable(path, reason);
            ExitCode::FAILURE
        }
    }
}

fn named(name: &str, prefix: &str) -> String {
    name.trim_start_matches(prefix)
        .to_ascii_lowercase()
        .replace('_', "-")
}

fn outcome_words(outcome: mix_events::Outcome) -> String {
    match outcome {
        mix_events::Outcome::Running => "running".to_string(),
        mix_events::Outcome::Finished(status) => named(status.as_str_name(), "STATUS_"),
        mix_events::Outcome::NotRun(reason) => {
            format!(
                "not run: {}",
                named(reason.as_str_name(), "NOT_RUN_REASON_")
            )
        }
    }
}

fn unreadable(path: &std::path::Path, reason: String) {
    mix_ui::report(
        mix_ui::Severity::Error,
        &mix_ui::Report::new(&mix_ui::phrase!(
            "{} isn't a valid events file",
            path.display()
        ))
        .causes(vec![reason]),
    );
}

#[cfg(test)]
mod tests {
    use mix_events::v1::Code;
    use mix_events::v1::envelope::Event;

    use super::*;

    #[test]
    fn a_checked_node_reads_its_outcome_in_words() {
        use mix_events::v1::{NotRunReason, Status};

        assert_eq!(
            outcome_words(mix_events::Outcome::Finished(Status::AlreadySatisfied)),
            "already-satisfied"
        );
        assert_eq!(
            outcome_words(mix_events::Outcome::NotRun(NotRunReason::NotReached)),
            "not run: not-reached"
        );
    }

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
            mix_events::Render::envelope(&mut sinks, envelope);
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
        let refused = anyhow::Error::from(client::Failed {
            request: request_of(&Command::Install {
                packages: vec!["ripgrep".into()],
            }),
            fault: mix_events::Diagnose::fault(&mix_shell::profile::change::Error::NotRoot),
        });

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
