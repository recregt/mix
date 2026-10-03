use std::process::ExitCode;

use crate::args::{Args, Command, EventsCommand};

pub fn run(args: &Args) -> ExitCode {
    match &args.command {
        Command::Explain { code, list } => mix_explain::command::run(code.as_deref(), *list),
        Command::Events { command } => match command {
            EventsCommand::Check { file } => check_events(file),
            EventsCommand::Show { file, node } => show_events(
                file,
                node.as_deref(),
                crate::output::level(args.quiet, args.verbose),
            ),
        },
        Command::Bootstrap { .. }
        | Command::Install { .. }
        | Command::Remove { .. }
        | Command::Clean { .. }
        | Command::Repair
        | Command::Doctor => unreachable!("only local commands reach here"),
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
            crate::output::replay::show(
                &captured,
                level,
                node,
                std::sync::Arc::new(mix_ui::Stdout),
            );
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
}
