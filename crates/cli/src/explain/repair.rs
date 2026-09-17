//! What `mix repair` says when it cannot finish, and what it says about a single artifact it
//! could not put back.

use mix_app::repair::{Error, Unfixable};

use super::{Diagnostic, core_error};

/// How the command is spelled when the reader is told to run it again.
const COMMAND: &str = "mix repair";

pub fn explain(error: &anyhow::Error) -> Diagnostic {
    match error.downcast_ref::<Error>() {
        Some(error) => describe(error, COMMAND),
        None => Diagnostic::new(error.to_string()),
    }
}

pub(crate) fn describe(error: &Error, command: &str) -> Diagnostic {
    match error {
        Error::Core(e) => core_error(e, command),
        Error::Unrepairable { artifact, reason } => Diagnostic::hinting(
            format!("{artifact}: {reason}"),
            unfixable(*reason).to_string(),
        ),
    }
}

/// One line per report, for the list `mix repair` prints as it goes.
///
/// A repaired artifact says so; one that could not be repaired says why, and what would put it
/// back. Both are one line: the list is read as a list.
pub fn report(name: &str, error: &Error) -> String {
    match error {
        Error::Unrepairable { reason, .. } => format!("{name}: {reason}\n{}", unfixable(*reason)),
        e => format!("{name}: {e}"),
    }
}

/// What to do about an artifact repair will not touch.
fn unfixable(reason: Unfixable) -> &'static str {
    match reason {
        Unfixable::NotADirectory => "Remove it by hand, then run `mix repair` again",
        Unfixable::MissingUser => "Nothing is left to enrol; the user has to exist first",
        Unfixable::MissingRuntime => "Run `mix bootstrap` to restore the nix installation",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_runtime_is_sent_to_bootstrap() {
        let error = Error::Unrepairable {
            artifact: "default profile".to_string(),
            reason: Unfixable::MissingRuntime,
        };

        let line = report("default profile", &error);

        assert!(line.contains("default profile"));
        assert!(line.contains("mix bootstrap"));
    }

    #[test]
    fn something_in_the_way_is_left_to_the_reader_to_remove() {
        let message = describe(
            &Error::Unrepairable {
                artifact: "/nix".to_string(),
                reason: Unfixable::NotADirectory,
            },
            COMMAND,
        )
        .message();

        assert!(message.contains("/nix: exists but is not a directory"));
        assert!(message.contains("Remove it by hand"));
    }

    #[test]
    fn a_raw_failure_is_reported_as_the_line_it_is() {
        let error = Error::Core(mix_core::Error::Command {
            command: "gpasswd --add ciuser mix-users".to_string(),
            detail: "exit 1".to_string(),
        });

        assert_eq!(
            report("mix-users", &error),
            "mix-users: command `gpasswd --add ciuser mix-users` failed: exit 1"
        );
    }
}
