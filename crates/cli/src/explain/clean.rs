use mix_shell::profile::change::Error;

use super::{Diagnostic, change, failed};

pub(crate) const COMMAND: &str = "mix clean";

pub(crate) const ACTION: &str = "clean up your profile";

pub fn explain(error: &anyhow::Error) -> Diagnostic {
    match error.downcast_ref::<Error>() {
        Some(error) => change::describe(error, COMMAND, &ACTION),
        None => failed(&ACTION),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_reads_as_a_clean_failure() {
        let error = anyhow::Error::from(Error::NotRoot);

        assert!(
            explain(&error)
                .message()
                .contains("`mix clean` can't be run as root")
        );
    }

    #[test]
    fn an_error_from_elsewhere_says_what_could_not_be_done() {
        let error = anyhow::anyhow!("something else broke");

        assert_eq!(
            explain(&error).message(),
            "couldn't clean up your profile\nrun it again with `-v` to see what went wrong"
        );
    }
}
