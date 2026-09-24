//! What a profile activation says when it will not finish.
//!
//! `mix bootstrap` stands a profile up and `mix install` changes one, through the same layer, so
//! the same raw failure reaches a reader who ran either. The words are written once and the
//! command they are told to re-run is passed in.

use mix_app::profile::Error;

use super::{Diagnostic, core_error};

const ONE_MISSING: &str = "package ";
const ONE_MISSING_END: &str = " is not available as a pre-built binary\ninstalling it ";
const SOME_MISSING: &str = "pre-built binaries are not available for: ";
const SOME_MISSING_END: &str = "\ninstalling them ";
const UNNAMED_MISSING: &str =
    "some packages are not available as pre-built binaries\ninstalling them ";
const SEPARATOR: &str = ", ";
const COMPILING: &str = "requires compiling from source, which may take a long time";
const RERUN: &str = "\n\nto proceed anyway, run: ";
const RERUN_FLAG: &str = "\n\nto proceed anyway, re-run with --build";

pub(crate) fn describe(error: &Error, command: &str, rerun: Option<&str>) -> Diagnostic {
    match error {
        Error::Core(e) => core_error(e, command),
        Error::SourceBuildRequired { packages } => source_build(packages.as_deref(), rerun),
    }
}

pub(crate) fn source_build(packages: Option<&[String]>, rerun: Option<&str>) -> Diagnostic {
    Diagnostic::new(render(packages.unwrap_or_default(), rerun))
}

fn render(packages: &[String], rerun: Option<&str>) -> String {
    let names: usize = packages.iter().map(String::len).sum();
    let opening = match packages {
        [] => UNNAMED_MISSING.len(),
        [_] => ONE_MISSING.len() + ONE_MISSING_END.len(),
        _ => SOME_MISSING.len() + SEPARATOR.len() * (packages.len() - 1) + SOME_MISSING_END.len(),
    };
    let closing = match rerun {
        Some(rerun) => RERUN.len() + rerun.len(),
        None => RERUN_FLAG.len(),
    };

    let mut message = String::with_capacity(opening + names + COMPILING.len() + closing);
    match packages {
        [] => message.push_str(UNNAMED_MISSING),
        [package] => {
            message.push_str(ONE_MISSING);
            message.push_str(package);
            message.push_str(ONE_MISSING_END);
        }
        [first, rest @ ..] => {
            message.push_str(SOME_MISSING);
            message.push_str(first);
            for package in rest {
                message.push_str(SEPARATOR);
                message.push_str(package);
            }
            message.push_str(SOME_MISSING_END);
        }
    }
    message.push_str(COMPILING);
    match rerun {
        Some(rerun) => {
            message.push_str(RERUN);
            message.push_str(rerun);
        }
        None => message.push_str(RERUN_FLAG),
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused(packages: Option<&[&str]>) -> Error {
        Error::SourceBuildRequired {
            packages: packages.map(|packages| packages.iter().map(|p| p.to_string()).collect()),
        }
    }

    const RERUN: Option<&str> = Some("mix install cowsay ripgrep --build");

    #[test]
    fn a_single_package_is_named_on_its_own() {
        let message = describe(&refused(Some(&["cowsay-3.8.4"])), "mix install", RERUN).message();

        assert_eq!(
            message,
            "package cowsay-3.8.4 is not available as a pre-built binary\n\
             installing it requires compiling from source, which may take a long time\n\
             \n\
             to proceed anyway, run: mix install cowsay ripgrep --build"
        );
    }

    #[test]
    fn several_packages_are_listed_together() {
        let message = describe(
            &refused(Some(&["cowsay-3.8.4", "ripgrep-14.1"])),
            "mix install",
            RERUN,
        )
        .message();

        assert_eq!(
            message,
            "pre-built binaries are not available for: cowsay-3.8.4, ripgrep-14.1\n\
             installing them requires compiling from source, which may take a long time\n\
             \n\
             to proceed anyway, run: mix install cowsay ripgrep --build"
        );
    }

    #[test]
    fn a_refusal_with_no_names_still_offers_the_way_out() {
        for packages in [None, Some(&[] as &[&str])] {
            let message = describe(&refused(packages), "mix install", RERUN).message();

            assert_eq!(
                message,
                "some packages are not available as pre-built binaries\n\
                 installing them requires compiling from source, which may take a long time\n\
                 \n\
                 to proceed anyway, run: mix install cowsay ripgrep --build"
            );
        }
    }

    #[test]
    fn with_no_command_to_repeat_the_flag_is_named_instead() {
        let message = describe(&refused(Some(&["cowsay-3.8.4"])), "mix install", None).message();

        assert!(message.ends_with("\n\nto proceed anyway, re-run with --build"));
    }

    #[test]
    fn nothing_from_nix_reaches_the_reader() {
        let message = describe(&refused(Some(&["cowsay-3.8.4"])), "mix install", RERUN).message();

        for word in ["derivation", "nix", "cache", "store"] {
            assert!(!message.contains(word), "{word:?} leaked into {message:?}");
        }
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
                None,
            )
            .message();

            assert!(message.contains(&format!("run `{command}` again")));
        }
    }

    #[test]
    fn the_message_is_written_into_exactly_the_space_it_needs() {
        let packages = ["cowsay-3.8.4".to_string(), "ripgrep-14.1".to_string()];
        for (packages, rerun) in [
            (&packages[..0], RERUN),
            (&packages[..1], RERUN),
            (&packages[..], RERUN),
            (&packages[..], None),
        ] {
            let message = render(packages, rerun);

            assert_eq!(message.capacity(), message.len(), "{message:?}");
        }
    }
}
