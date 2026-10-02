//! What `mix remove` says when it cannot finish.

use mix_shell::ops::remove::Error;

use super::{Context, Diagnostic, failed, packages_action};

/// How the command is spelled when the reader is told to run it again.
pub(crate) const COMMAND: &str = "mix remove";

pub fn explain(error: &anyhow::Error, packages: &[String]) -> Diagnostic {
    let action = packages_action("remove", packages);
    match error.downcast_ref::<Error>() {
        Some(error) => super::render::render_error(
            error,
            &Context {
                command: COMMAND,
                action: &action,
            },
        ),
        None => failed(&action),
    }
}

#[cfg(test)]
mod tests {
    use mix_shell::profile::change;

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
    fn a_root_caller_is_told_which_command_refused() {
        let error = anyhow::Error::from(Error::Change(change::Error::NotRoot));

        assert!(
            explain(&error, &["git".to_string()])
                .message()
                .contains("`mix remove` can't be run as root")
        );
    }

    #[test]
    fn an_error_from_elsewhere_says_what_could_not_be_done() {
        let error = anyhow::anyhow!("something else broke");

        assert_eq!(
            explain(&error, &["git".to_string()]).message(),
            "couldn't remove git\nrun it again with `-v` to see what went wrong"
        );
    }
}
