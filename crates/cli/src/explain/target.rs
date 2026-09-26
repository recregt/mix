//! What a declared target says when it drifted, or could not be put back.
//!
//! Three commands read the same measurement — `mix doctor` prints it, `mix repair` reconciles
//! from it, and `mix bootstrap` declares the per-user configuration through it — so what a
//! finding and a reason read like is written once, here.

use mix_app::target::{Error, Unfixable};

use super::{Diagnostic, core_error};

pub(crate) fn describe(error: &Error, command: &str, action: &dyn std::fmt::Display) -> Diagnostic {
    match error {
        Error::Core(e) => core_error(e, command, action),
        Error::Unrepairable { artifact, reason } => Diagnostic::hinting(
            format!("{artifact}: {reason}"),
            unfixable(*reason).to_string(),
        ),
    }
}

/// What went wrong with one artifact, for printing under its own name.
///
/// An artifact that could not be repaired says why, and what would put it back; anything else is
/// the raw failure as it was raised. The name is the caller's to print, so nothing here has to
/// guess whether it is a word or a path.
pub fn report(error: &Error) -> String {
    match error {
        Error::Unrepairable { reason, .. } => format!("{reason}\n{}", unfixable(*reason)),
        e => e.to_string(),
    }
}

/// What to do about an artifact repair will not touch.
///
/// Written once and read by both commands: `mix doctor` says the same thing about a finding
/// repair cannot reconcile as `mix repair` says when it meets it.
pub(crate) fn unfixable(reason: Unfixable) -> &'static str {
    match reason {
        Unfixable::NotADirectory => "Remove it, then run `mix repair` again",
        Unfixable::MissingUser => "Recreate the user, or ignore this if it was removed on purpose",
        Unfixable::MissingRuntime => "Run `mix bootstrap` to reinstall it",
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

        let line = report(&error);

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
        assert!(message.contains("Remove it, then run `mix repair` again"));
    }

    #[test]
    fn a_raw_failure_is_reported_as_the_line_it_is() {
        let error = Error::Core(mix_core::Error::Command {
            command: "gpasswd --add ciuser mix-users".to_string(),
            detail: "exit 1".to_string(),
        });

        assert_eq!(
            report(&error),
            "command `gpasswd --add ciuser mix-users` failed: exit 1"
        );
    }
}
