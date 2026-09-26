//! What a profile activation says when it will not finish.
//!
//! `mix bootstrap` stands a profile up and `mix install` changes one, through the same layer, so
//! the same raw failure reaches a reader who ran either. The words are written once and the
//! command they are told to re-run is passed in.

use mix_app::profile::Error;

use super::{Diagnostic, core_error};

const SOME_PACKAGES: &str = "some packages";
const SEPARATOR: &str = ", ";
const LAST_SEPARATOR: &str = " and ";
const FROM_SOURCE: &str = " must be built from source, which can take a long time";
const BUILD_IT: &str = "To build it anyway, run: ";
const BUILD_THEM: &str = "To build them anyway, run: ";
const BUILD_FLAG: &str = "To build anyway, run it again with `--build`";

pub(crate) fn describe(
    error: &Error,
    command: &str,
    action: &dyn std::fmt::Display,
    rerun: Option<&str>,
) -> Diagnostic {
    match error {
        Error::Core(e) => core_error(e, command, action),
        Error::SourceBuildRequired { packages } => {
            source_build(packages.as_deref().unwrap_or_default(), rerun)
        }
    }
}

pub(crate) fn source_build(packages: &[String], rerun: Option<&str>) -> Diagnostic {
    let lead = match (rerun, packages.len()) {
        (None, _) => BUILD_FLAG,
        (Some(_), 1) => BUILD_IT,
        (Some(_), _) => BUILD_THEM,
    };
    let rerun = rerun.unwrap_or_default();
    let names: usize = packages.iter().map(String::len).sum();
    let separators = match packages.len() {
        0 => SOME_PACKAGES.len(),
        1 => 0,
        n => SEPARATOR.len() * (n - 2) + LAST_SEPARATOR.len(),
    };

    let mut text = String::with_capacity(
        names + separators + FROM_SOURCE.len() + 1 + lead.len() + rerun.len(),
    );
    match packages {
        [] => text.push_str(SOME_PACKAGES),
        [only] => text.push_str(only),
        [init @ .., last] => {
            for (index, package) in init.iter().enumerate() {
                if index > 0 {
                    text.push_str(SEPARATOR);
                }
                text.push_str(package);
            }
            text.push_str(LAST_SEPARATOR);
            text.push_str(last);
        }
    }
    text.push_str(FROM_SOURCE);
    let summary_len = text.len();
    text.push('\n');
    text.push_str(lead);
    text.push_str(rerun);
    Diagnostic::written(text, summary_len)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused(packages: Option<&[&str]>) -> Error {
        Error::SourceBuildRequired {
            packages: packages.map(|packages| packages.iter().map(|p| p.to_string()).collect()),
        }
    }

    const RERUN: Option<&str> = Some("mix install cowsay --build");

    fn message(packages: Option<&[&str]>, rerun: Option<&str>) -> String {
        describe(&refused(packages), "mix install", &"install cowsay", rerun).message()
    }

    #[test]
    fn a_single_package_is_named_on_its_own() {
        assert_eq!(
            message(Some(&["cowsay-3.8.4"]), RERUN),
            "cowsay-3.8.4 must be built from source, which can take a long time\n\
             To build it anyway, run: mix install cowsay --build"
        );
    }

    #[test]
    fn two_packages_are_joined_with_and() {
        assert_eq!(
            message(Some(&["cowsay-3.8.4", "ripgrep-14.1"]), RERUN),
            "cowsay-3.8.4 and ripgrep-14.1 must be built from source, which can take a long \
             time\nTo build them anyway, run: mix install cowsay --build"
        );
    }

    #[test]
    fn a_longer_list_puts_and_before_the_last_one() {
        assert!(
            message(Some(&["a-1", "b-2", "c-3"]), RERUN)
                .starts_with("a-1, b-2 and c-3 must be built from source")
        );
    }

    #[test]
    fn a_refusal_with_no_names_still_offers_the_way_out() {
        for packages in [None, Some(&[] as &[&str])] {
            assert_eq!(
                message(packages, RERUN),
                "some packages must be built from source, which can take a long time\n\
                 To build them anyway, run: mix install cowsay --build"
            );
        }
    }

    #[test]
    fn with_no_command_to_repeat_the_flag_is_named_instead() {
        assert!(
            message(Some(&["cowsay-3.8.4"]), None)
                .ends_with("To build anyway, run it again with `--build`")
        );
    }

    #[test]
    fn nothing_from_nix_reaches_the_reader() {
        let message = message(Some(&["cowsay-3.8.4"]), RERUN);

        for word in ["derivation", "nix", "cache", "store", "binary"] {
            assert!(!message.contains(word), "{word:?} leaked into {message:?}");
        }
    }

    #[test]
    fn the_message_is_written_into_exactly_the_space_it_needs() {
        let packages: Vec<String> = ["cowsay-3.8.4", "ripgrep-14.1", "fd-10"]
            .map(String::from)
            .to_vec();
        for count in 0..=packages.len() {
            for rerun in [RERUN, None] {
                let message = source_build(&packages[..count], rerun).message();

                assert_eq!(message.capacity(), message.len(), "{message:?}");
            }
        }
    }

    #[test]
    fn a_shared_failure_names_the_command_that_hit_it() {
        for command in ["mix bootstrap", "mix install"] {
            let message = describe(
                &Error::Core(mix_core::Error::Locked {
                    path: "/var/lib/mix/lock".into(),
                }),
                command,
                &"finish",
                None,
            )
            .message();

            assert!(message.contains(&format!("run `{command}` again")));
        }
    }
}
