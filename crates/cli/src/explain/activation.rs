//! What a profile activation says when it will not finish.
//!
//! `mix bootstrap` stands a profile up and `mix install` changes one, through the same layer, so
//! the same raw failure reaches a reader who ran either. The words are written once and the
//! command they are told to re-run is passed in.

use mix_app::profile::Error;

use super::{Diagnostic, core_error};

/// How many derivations are worth naming before the list stops being readable.
const NAMED_SOURCE_BUILDS: usize = 5;

pub(crate) fn describe(error: &Error, command: &str) -> Diagnostic {
    match error {
        Error::Core(e) => core_error(e, command),
        Error::SourceBuildRequired(derivations) => source_build(derivations),
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
    fn a_refused_source_build_names_it_and_offers_the_flag() {
        let message = describe(
            &Error::SourceBuildRequired(vec!["cowsay-3.8.4".to_string()]),
            "mix install",
        )
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

    /// The same failure, and the command the reader should try again, is the one they ran.
    #[test]
    fn a_shared_failure_names_the_command_that_hit_it() {
        for command in ["mix bootstrap", "mix install"] {
            let message = describe(
                &Error::Core(mix_core::Error::Locked {
                    path: "/run/mix.lock".into(),
                }),
                command,
            )
            .message();

            assert!(message.contains(&format!("run `{command}` again")));
        }
    }
}
