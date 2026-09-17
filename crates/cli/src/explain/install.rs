//! What `mix install` says when it cannot finish.

use mix_app::bootstrap::Error as ActivationError;
use mix_app::install::Error;

use super::{Diagnostic, core_error};

/// How the command is spelled when the reader is told to run it again.
const COMMAND: &str = "mix install";

/// How many derivations are worth naming before the list stops being readable.
const NAMED_SOURCE_BUILDS: usize = 5;

pub fn explain(error: &anyhow::Error) -> Diagnostic {
    match error.downcast_ref::<Error>() {
        Some(error) => describe(error),
        None => Diagnostic::new(error.to_string()),
    }
}

fn describe(error: &Error) -> Diagnostic {
    match error {
        Error::Core(e) => core_error(e, COMMAND),

        // Activation is bootstrap's machinery, so most of it reads the same; what it refused to
        // compile is the one failure `install` alone can offer a way out of.
        Error::Activation(ActivationError::SourceBuildRequired(derivations)) => {
            source_build(derivations)
        }
        Error::Activation(e) => super::bootstrap::describe(e, COMMAND),

        Error::InvalidPackage(e) => Diagnostic::hinting(
            format!("that is not a package `mix` can install: {e}"),
            "Packages are named as they are in nixpkgs, e.g. `ripgrep` or `python3`",
        ),

        Error::NotRoot => Diagnostic::hinting(
            "`mix install` cannot be run as root",
            "Run it as the user whose profile it installs into",
        ),

        Error::NotBootstrapped => Diagnostic::hinting(
            "this user has no managed environment yet",
            "Run `mix bootstrap` first",
        ),
    }
}

/// What the cache-only gate refused, and the flag that overrides it.
///
/// The plan can be long, and a plan longer than a few names stops being readable: the library
/// hands over every derivation it refused, and how many of them are worth printing is decided
/// here, where the printing happens.
pub(crate) fn source_build(derivations: &[String]) -> Diagnostic {
    let named: Vec<&str> = derivations
        .iter()
        .take(NAMED_SOURCE_BUILDS)
        .map(String::as_str)
        .collect();
    let mut list = named.join(", ");
    if let Some(rest) = derivations
        .len()
        .checked_sub(named.len())
        .filter(|n| *n > 0)
    {
        list.push_str(&format!(" and {rest} more"));
    }

    Diagnostic::hinting(
        format!("the binary cache has nothing to download for: {list}"),
        "Installing this would compile it from source, which can take hours.\n\
         To compile it anyway, re-run with `--build`:\n\
         \x20 mix install --build ...",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_as_root_says_who_should_run_it_instead() {
        let message = describe(&Error::NotRoot).message();

        assert!(message.contains("cannot be run as root"));
        assert!(message.contains("the user whose profile"));
    }

    #[test]
    fn an_unbootstrapped_user_is_sent_to_bootstrap() {
        assert!(
            describe(&Error::NotBootstrapped)
                .message()
                .contains("mix bootstrap")
        );
    }

    #[test]
    fn a_refused_source_build_names_it_and_offers_the_flag() {
        let message = describe(&Error::Activation(ActivationError::SourceBuildRequired(
            vec!["cowsay-3.8.4".to_string()],
        )))
        .message();

        assert!(message.contains("binary cache"));
        assert!(message.contains("cowsay-3.8.4"));
        assert!(message.contains("mix install --build"));
    }

    #[test]
    fn a_long_list_of_refusals_is_counted_rather_than_printed() {
        let derivations: Vec<String> = (0..8).map(|i| format!("package-{i}")).collect();

        let message = source_build(&derivations).message();

        assert!(message.contains("package-4"));
        assert!(!message.contains("package-5"));
        assert!(message.contains("and 3 more"));
    }

    /// Activation shares bootstrap's failures, and the reader is told to re-run what they ran.
    #[test]
    fn an_activation_failure_reads_as_an_install_failure() {
        let message = describe(&Error::Core(mix_core::Error::Locked {
            path: "/run/mix.lock".into(),
        }))
        .message();

        assert!(message.contains("run `mix install` again"));
    }
}
