//! What `mix repair` says when it cannot finish.

use mix_app::target::Error;

use super::Diagnostic;

/// How the command is spelled when the reader is told to run it again.
const COMMAND: &str = "mix repair";

pub fn explain(error: &anyhow::Error) -> Diagnostic {
    match error.downcast_ref::<Error>() {
        Some(error) => super::target::describe(error, COMMAND),
        None => Diagnostic::new(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use mix_app::target::Unfixable;

    use super::*;

    #[test]
    fn a_target_repair_will_not_touch_is_explained_in_its_own_words() {
        let error = anyhow::Error::from(Error::Unrepairable {
            artifact: "/nix".to_string(),
            reason: Unfixable::NotADirectory,
        });

        let message = explain(&error).message();

        assert!(message.contains("/nix: exists but is not a directory"));
        assert!(message.contains("Remove it by hand"));
    }

    #[test]
    fn a_failure_that_is_not_a_target_failure_is_reported_as_it_is() {
        let error = anyhow::anyhow!("something else entirely");

        assert_eq!(explain(&error).message(), "something else entirely");
    }
}
