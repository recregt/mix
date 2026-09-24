//! What a profile activation says when it will not finish.
//!
//! `mix bootstrap` stands a profile up and `mix install` changes one, through the same layer, so
//! the same raw failure reaches a reader who ran either. The words are written once and the
//! command they are told to re-run is passed in.

use mix_app::profile::Error;

use super::{Diagnostic, core_error};

const COMPILING: &str = "requires compiling from source, which may take a long time";

pub(crate) fn describe(error: &Error, command: &str, rerun: Option<&str>) -> Diagnostic {
    match error {
        Error::Core(e) => core_error(e, command),
        Error::SourceBuildRequired { packages } => source_build(packages.as_deref(), rerun),
    }
}

pub(crate) fn source_build(packages: Option<&[String]>, rerun: Option<&str>) -> Diagnostic {
    let summary = match packages {
        Some([package]) => format!(
            "package {package} is not available as a pre-built binary\ninstalling it {COMPILING}"
        ),
        Some(packages) if !packages.is_empty() => format!(
            "pre-built binaries are not available for: {}\ninstalling them {COMPILING}",
            packages.join(", ")
        ),
        _ => format!(
            "some packages are not available as pre-built binaries\ninstalling them {COMPILING}"
        ),
    };
    let hint = match rerun {
        Some(rerun) => format!("\nto proceed anyway, run: {rerun}"),
        None => "\nto proceed anyway, re-run with --build".to_string(),
    };

    Diagnostic::hinting(summary, hint)
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
}
