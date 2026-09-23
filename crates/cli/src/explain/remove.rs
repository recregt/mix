//! What `mix remove` says when it cannot finish.

use mix_app::remove::Error;

use super::{Diagnostic, change};

/// How the command is spelled when the reader is told to run it again.
const COMMAND: &str = "mix remove";

pub fn explain(error: &anyhow::Error) -> Diagnostic {
    match error.downcast_ref::<Error>() {
        Some(Error::Change(error)) => change::describe(error, COMMAND),
        Some(Error::Protected(packages)) => protected(packages),
        None => Diagnostic::new(error.to_string()),
    }
}

fn protected(packages: &[String]) -> Diagnostic {
    Diagnostic::hinting(
        format!("`{}` cannot be removed", packages.join("`, `")),
        "Packages that `mix` relies on always stay in the profile",
    )
}

#[cfg(test)]
mod tests {
    use mix_app::profile::change;

    use super::*;

    #[test]
    fn a_protected_package_is_named_and_explained() {
        let error = anyhow::Error::from(Error::Protected(vec!["git".to_string()]));

        let message = explain(&error).message();

        assert!(message.contains("`git` cannot be removed"));
        assert!(message.contains("always stay in the profile"));
    }

    #[test]
    fn several_protected_packages_are_each_named() {
        let error = anyhow::Error::from(Error::Protected(vec![
            "git".to_string(),
            "curl".to_string(),
        ]));

        assert!(
            explain(&error)
                .message()
                .contains("`git`, `curl` cannot be removed")
        );
    }

    #[test]
    fn running_as_root_reads_as_a_remove_failure() {
        let error = anyhow::Error::from(Error::Change(change::Error::NotRoot));

        assert!(
            explain(&error)
                .message()
                .contains("`mix remove` cannot be run as root")
        );
    }

    #[test]
    fn a_held_lock_tells_the_reader_to_run_remove_again() {
        let error = anyhow::Error::from(Error::Change(change::Error::Core(
            mix_core::Error::Locked {
                path: "/run/mix.lock".into(),
            },
        )));

        assert!(explain(&error).message().contains("run `mix remove` again"));
    }

    #[test]
    fn an_error_from_elsewhere_is_left_as_it_was_written() {
        let error = anyhow::anyhow!("something else broke");

        assert_eq!(explain(&error).message(), "something else broke");
    }
}
