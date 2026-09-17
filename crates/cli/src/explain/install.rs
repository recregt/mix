//! What `mix install` says when it cannot finish.

use mix_app::install::Error;

use super::{Diagnostic, core_error};

/// How the command is spelled when the reader is told to run it again.
const COMMAND: &str = "mix install";

pub fn explain(error: &anyhow::Error) -> Diagnostic {
    match error.downcast_ref::<Error>() {
        Some(error) => describe(error),
        None => Diagnostic::new(error.to_string()),
    }
}

fn describe(error: &Error) -> Diagnostic {
    match error {
        Error::Core(e) => core_error(e, COMMAND),

        // Activating the profile is the layer both commands share, and its failures read the
        // same whichever of them asked for the activation.
        Error::Activation(e) => super::activation::describe(e, COMMAND),

        Error::InvalidPackage(e) => Diagnostic::hinting(
            format!("that is not a package `mix` can install: {e}"),
            "Packages are named as they are in nixpkgs, e.g. `ripgrep` or `python3`",
        ),

        Error::NotRoot => Diagnostic::hinting(
            "`mix install` cannot be run as root",
            "Run it as the user whose profile it installs into",
        ),

        Error::NotBootstrapped => Diagnostic::hinting(
            "this user has no managed environment yet",
            "Run `mix bootstrap` first",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_as_root_says_who_should_run_it_instead() {
        let message = describe(&Error::NotRoot).message();

        assert!(message.contains("cannot be run as root"));
        assert!(message.contains("the user whose profile"));
    }

    #[test]
    fn an_unbootstrapped_user_is_sent_to_bootstrap() {
        assert!(
            describe(&Error::NotBootstrapped)
                .message()
                .contains("mix bootstrap")
        );
    }

    /// Activation is shared machinery, and the reader is told to re-run what they ran.
    #[test]
    fn an_activation_failure_reads_as_an_install_failure() {
        let message = describe(&Error::Core(mix_core::Error::Locked {
            path: "/run/mix.lock".into(),
        }))
        .message();

        assert!(message.contains("run `mix install` again"));
    }
}
