//! What a declared target says when it drifted, or could not be put back.
//!
//! Three commands read the same measurement: `mix doctor` prints it, `mix repair` reconciles
//! from it, and `mix bootstrap` declares the per-user configuration through it. So what a
//! finding and a reason read like is written once, here.

use mix_shell::target::Error;

pub(crate) use super::render::unfixable;
use super::{Context, Diagnostic};

pub(crate) fn describe(error: &Error, command: &str, action: &dyn std::fmt::Display) -> Diagnostic {
    super::render::render_error(error, &Context { command, action })
}

pub fn report(error: &Error) -> Diagnostic {
    match error {
        Error::Unrepairable { artifact, reason } => super::render::unrepairable(artifact, *reason),
        e => Diagnostic::new(mix_ui::phrase!("{e}")),
    }
}

#[cfg(test)]
mod tests {
    use mix_shell::target::Unfixable;

    use super::*;

    #[test]
    fn a_missing_runtime_is_sent_to_bootstrap() {
        let error = Error::Unrepairable {
            artifact: "default profile".to_string(),
            reason: Unfixable::MissingRuntime,
        };

        let line = report(&error).message();

        assert!(line.contains("`mix repair` can't restore it"));
        assert!(line.contains("mix bootstrap"));
    }

    #[test]
    fn something_in_the_way_is_left_to_the_reader_to_remove() {
        let message = describe(
            &Error::Unrepairable {
                artifact: "/nix".to_string(),
                reason: Unfixable::NotADirectory,
            },
            "mix repair",
            &"finish the repair",
        )
        .message();

        assert!(message.contains("/nix: exists but is not a directory"));
        assert!(message.contains("remove it, then run `mix repair` again"));
    }

    #[test]
    fn a_raw_failure_is_reported_as_the_line_it_is() {
        let error = Error::Core(mix_core::Error::Command {
            command: "gpasswd --add ciuser mix-users".to_string(),
            detail: "exit 1".to_string(),
        });

        assert_eq!(
            report(&error).message(),
            "command `gpasswd --add ciuser mix-users` failed: exit 1"
        );
    }
}
