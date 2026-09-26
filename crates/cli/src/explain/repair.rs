//! What `mix repair` says when it cannot finish.

use mix_app::target::Error;

use super::{Diagnostic, failed};

/// How the command is spelled when the reader is told to run it again.
const COMMAND: &str = "mix repair";

const ACTION: &str = "finish the repair";

pub fn explain(error: &anyhow::Error) -> Diagnostic {
    if let Some(error) = error.downcast_ref::<mix_rpc::Error>() {
        return super::privileged(error, &ACTION);
    }
    match error.downcast_ref::<Error>() {
        Some(error) => super::target::describe(error, COMMAND, &ACTION),
        None => failed(&ACTION),
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
        assert!(message.contains("Remove it, then run `mix repair` again"));
    }

    #[test]
    fn a_failure_that_is_not_a_target_failure_says_what_could_not_be_done() {
        let error = anyhow::anyhow!("something else entirely");

        assert_eq!(
            explain(&error).message(),
            "couldn't finish the repair\nRun it again with `-v` to see what went wrong"
        );
    }
}
