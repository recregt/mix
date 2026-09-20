//! What `mix install` says when it cannot finish.

use mix_app::profile::change::Error;

use super::{Diagnostic, change};

/// How the command is spelled when the reader is told to run it again.
const COMMAND: &str = "mix install";

pub fn explain(error: &anyhow::Error) -> Diagnostic {
    match error.downcast_ref::<Error>() {
        Some(error) => change::describe(error, COMMAND),
        None => Diagnostic::new(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_reads_as_an_install_failure() {
        let error = anyhow::Error::from(Error::NotRoot);

        assert!(
            explain(&error)
                .message()
                .contains("`mix install` cannot be run as root")
        );
    }

    #[test]
    fn a_held_lock_tells_the_reader_to_run_install_again() {
        let error = anyhow::Error::from(Error::Core(mix_core::Error::Locked {
            path: "/run/mix.lock".into(),
        }));

        let message = explain(&error).message();

        assert!(message.contains("another `mix` command is already running"));
        assert!(message.contains("run `mix install` again"));
    }

    #[test]
    fn an_error_from_elsewhere_is_left_as_it_was_written() {
        let error = anyhow::anyhow!("something else broke");

        assert_eq!(explain(&error).message(), "something else broke");
    }
}
