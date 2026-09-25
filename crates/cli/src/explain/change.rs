use mix_app::profile::change::Error;
use mix_app::profile::state::{Invalid, Source};

use super::{Diagnostic, core_error};

pub(crate) fn describe(error: &Error, command: &str, rerun: Option<&str>) -> Diagnostic {
    match error {
        Error::Core(e) => core_error(e, command),

        Error::Activation(e) => super::activation::describe(e, command, rerun),

        Error::InvalidPackage(e) => Diagnostic::hinting(
            format!("that is not a package `mix` can install: {e}"),
            "Packages are named as they are in nixpkgs, e.g. `ripgrep` or `python3`",
        ),

        Error::InvalidState(Invalid::Package(name)) => Diagnostic::hinting(
            format!("`{name}` is not a package `mix` can install"),
            "Packages are named as they are in nixpkgs, e.g. `ripgrep` or `python3`",
        ),

        Error::InvalidState(_) => Diagnostic::new("that change could not be made"),

        Error::NewerState(_) => Diagnostic::hinting(
            "this version of `mix` is older than the one that set up your packages",
            "Update `mix`",
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

pub fn restored(source: Source) -> Option<&'static str> {
    match source {
        Source::File | Source::Generation => None,
        Source::Fresh => Some(
            "your installed packages could not be recovered\nInstall them again with `mix install`",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_as_root_names_the_command_and_says_who_should_run_it_instead() {
        for command in ["mix install", "mix remove"] {
            let message = describe(&Error::NotRoot, command, None).message();

            assert!(message.contains(&format!("`{command}` cannot be run as root")));
            assert!(message.contains("the user whose profile"));
        }
    }

    #[test]
    fn an_older_mix_is_told_to_update() {
        let message = describe(&Error::NewerState(2), "mix install", None).message();

        assert!(message.contains("older than the one that set up your packages"));
        assert!(message.contains("Update `mix`"));
    }

    #[test]
    fn a_bad_package_name_reads_like_any_other_bad_name() {
        let message = describe(
            &Error::InvalidState(Invalid::Package("rm -rf".to_string())),
            "mix install",
            None,
        )
        .message();

        assert!(message.contains("`rm -rf` is not a package `mix` can install"));
    }

    #[test]
    fn only_packages_that_are_gone_are_worth_telling() {
        assert_eq!(restored(Source::File), None);
        assert_eq!(restored(Source::Generation), None);
        assert!(restored(Source::Fresh).unwrap().contains("mix install"));
    }

    #[test]
    fn an_unbootstrapped_user_is_sent_to_bootstrap() {
        assert!(
            describe(&Error::NotBootstrapped, "mix install", None)
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
                None,
            )
            .message();

            assert!(message.contains(&format!("run `{command}` again")));
        }
    }
}
