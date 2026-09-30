//! What `mix install` says when it cannot finish.

use mix_shell::profile::change::Error;

use super::{Diagnostic, change, failed, packages_action};

/// How the command is spelled when the reader is told to run it again.
const COMMAND: &str = "mix install";

pub fn explain(error: &anyhow::Error, packages: &[String]) -> Diagnostic {
    let action = packages_action("install", packages);
    match error.downcast_ref::<Error>() {
        Some(error) => change::describe(error, COMMAND, &action),
        None => failed(&action),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_reads_as_an_install_failure() {
        let error = anyhow::Error::from(Error::NotRoot);

        assert!(
            explain(&error, &["x".to_string()])
                .message()
                .contains("`mix install` can't be run as root")
        );
    }

    #[test]
    fn a_held_lock_tells_the_reader_to_run_install_again() {
        let error = anyhow::Error::from(Error::Core(mix_core::Error::Locked {
            path: "/var/lib/mix/lock".into(),
        }));

        let message = explain(&error, &["x".to_string()]).message();

        assert!(message.contains("another `mix` command is already running"));
        assert!(message.contains("run `mix install` again"));
    }

    #[test]
    fn an_error_from_elsewhere_says_what_could_not_be_done() {
        let error = anyhow::anyhow!("something else broke");

        assert_eq!(
            explain(&error, &["x".to_string()]).message(),
            "couldn't install x\nrun it again with `-v` to see what went wrong"
        );
    }
}
