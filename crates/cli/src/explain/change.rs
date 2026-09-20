use mix_app::profile::change::Error;

use super::{Diagnostic, core_error};

pub(crate) fn describe(error: &Error, command: &str) -> Diagnostic {
    match error {
        Error::Core(e) => core_error(e, command),

        Error::Activation(e) => super::activation::describe(e, command),

        Error::InvalidPackage(e) => Diagnostic::hinting(
            format!("that is not a package `mix` can install: {e}"),
            "Packages are named as they are in nixpkgs, e.g. `ripgrep` or `python3`",
        ),

        Error::NotRoot => Diagnostic::hinting(
            format!("`{command}` cannot be run as root"),
            "Run it as the user whose profile it changes",
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
    fn running_as_root_names_the_command_and_says_who_should_run_it_instead() {
        for command in ["mix install", "mix remove"] {
            let message = describe(&Error::NotRoot, command).message();

            assert!(message.contains(&format!("`{command}` cannot be run as root")));
            assert!(message.contains("the user whose profile"));
        }
    }

    #[test]
    fn an_unbootstrapped_user_is_sent_to_bootstrap() {
        assert!(
            describe(&Error::NotBootstrapped, "mix install")
                .message()
                .contains("mix bootstrap")
        );
    }

    #[test]
    fn an_activation_failure_names_the_command_the_reader_ran() {
        for command in ["mix install", "mix remove"] {
            let message = describe(
                &Error::Core(mix_core::Error::Locked {
                    path: "/run/mix.lock".into(),
                }),
                command,
            )
            .message();

            assert!(message.contains(&format!("run `{command}` again")));
        }
    }
}
