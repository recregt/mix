//! What `mix remove` says when it cannot finish.

use mix_app::remove::Error;

use super::{Diagnostic, change, failed, packages_action};

/// How the command is spelled when the reader is told to run it again.
const COMMAND: &str = "mix remove";

pub fn explain(error: &anyhow::Error, packages: &[String]) -> Diagnostic {
    let action = packages_action("remove", packages);
    match error.downcast_ref::<Error>() {
        Some(Error::Change(error)) => change::describe(error, COMMAND, &action, None),
        Some(Error::Protected(packages)) => protected(packages),
        None => failed(&action),
    }
}

fn protected(packages: &[String]) -> Diagnostic {
    let hint = if packages.len() == 1 {
        "`mix` needs it to work"
    } else {
        "`mix` needs them to work"
    };
    Diagnostic::hinting(
        format!("`{}` can't be removed", packages.join("`, `")),
        hint,
    )
}

#[cfg(test)]
mod tests {
    use mix_app::profile::change;

    use super::*;

    #[test]
    fn a_protected_package_is_named_and_explained() {
        let error = anyhow::Error::from(Error::Protected(vec!["git".to_string()]));

        let message = explain(&error, &["git".to_string()]).message();

        assert!(message.contains("`git` can't be removed"));
        assert!(message.contains("`mix` needs it to work"));
    }

    #[test]
    fn several_protected_packages_are_each_named() {
        let error = anyhow::Error::from(Error::Protected(vec![
            "git".to_string(),
            "curl".to_string(),
        ]));

        assert!(
            explain(&error, &["git".to_string()])
                .message()
                .contains("`git`, `curl` can't be removed")
        );
    }

    #[test]
    fn running_as_root_reads_as_a_remove_failure() {
        let error = anyhow::Error::from(Error::Change(change::Error::NotRoot));

        assert!(
            explain(&error, &["git".to_string()])
                .message()
                .contains("`mix remove` can't be run as root")
        );
    }

    #[test]
    fn a_held_lock_tells_the_reader_to_run_remove_again() {
        let error = anyhow::Error::from(Error::Change(change::Error::Core(
            mix_core::Error::Locked {
                path: "/run/mix.lock".into(),
            },
        )));

        assert!(
            explain(&error, &["git".to_string()])
                .message()
                .contains("run `mix remove` again")
        );
    }

    #[test]
    fn an_error_from_elsewhere_says_what_could_not_be_done() {
        let error = anyhow::anyhow!("something else broke");

        assert_eq!(
            explain(&error, &["git".to_string()]).message(),
            "couldn't remove git\nRun it again with `-v` to see what went wrong"
        );
    }
}
