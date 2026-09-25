//! What `mix install` says when it cannot finish.

use mix_app::profile::change::Error;

use super::{Diagnostic, change, failed, packages_action};

/// How the command is spelled when the reader is told to run it again.
const COMMAND: &str = "mix install";

const PROGRAM: &str = "mix";
const BUILD_FLAG: &str = "--build";

pub fn explain(error: &anyhow::Error, packages: &[String], rerun: &str) -> Diagnostic {
    let action = packages_action("install", packages);
    match error.downcast_ref::<Error>() {
        Some(error) => change::describe(error, COMMAND, &action, Some(rerun)),
        None => failed(&action),
    }
}

pub fn rerun_with_build(args: impl IntoIterator<Item = String>) -> String {
    let mut rerun = String::from(PROGRAM);
    for arg in args.into_iter().skip(1) {
        rerun.push(' ');
        push_quoted(&mut rerun, &arg);
    }
    rerun.push(' ');
    rerun.push_str(BUILD_FLAG);
    rerun
}

fn push_quoted(out: &mut String, arg: &str) {
    let plain = !arg.is_empty()
        && arg
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"@%+=:,./_-".contains(&b));
    if plain {
        out.push_str(arg);
        return;
    }
    out.push('\'');
    for (index, part) in arg.split('\'').enumerate() {
        if index > 0 {
            out.push_str(r"'\''");
        }
        out.push_str(part);
    }
    out.push('\'');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_reads_as_an_install_failure() {
        let error = anyhow::Error::from(Error::NotRoot);

        assert!(
            explain(&error, &["x".to_string()], "mix install x --build")
                .message()
                .contains("`mix install` can't be run as root")
        );
    }

    #[test]
    fn a_held_lock_tells_the_reader_to_run_install_again() {
        let error = anyhow::Error::from(Error::Core(mix_core::Error::Locked {
            path: "/var/lib/mix/lock".into(),
        }));

        let message = explain(&error, &["x".to_string()], "mix install x --build").message();

        assert!(message.contains("another `mix` command is already running"));
        assert!(message.contains("run `mix install` again"));
    }

    #[test]
    fn an_error_from_elsewhere_says_what_could_not_be_done() {
        let error = anyhow::anyhow!("something else broke");

        assert_eq!(
            explain(&error, &["x".to_string()], "mix install x --build").message(),
            "couldn't install x\nRun it again with `-v` to see what went wrong"
        );
    }

    fn args(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| arg.to_string()).collect()
    }

    #[test]
    fn the_command_to_repeat_is_the_one_that_was_run_with_the_flag_added() {
        assert_eq!(
            rerun_with_build(args(&[
                "/usr/local/bin/mix",
                "install",
                "cowsay",
                "ripgrep"
            ])),
            "mix install cowsay ripgrep --build"
        );
    }

    #[test]
    fn the_options_that_were_given_are_kept() {
        assert_eq!(
            rerun_with_build(args(&[
                "mix",
                "--no-progress",
                "install",
                "cowsay",
                "--mirror",
                "http://mirror.internal:8080",
                "--mirror-key",
                "mix-mirror-1:AAAA+/=",
            ])),
            "mix --no-progress install cowsay --mirror http://mirror.internal:8080 \
             --mirror-key mix-mirror-1:AAAA+/= --build"
        );
    }

    #[test]
    fn an_argument_a_shell_would_split_is_quoted() {
        assert_eq!(
            rerun_with_build(args(&["mix", "install", "a b", "", "it's"])),
            r"mix install 'a b' '' 'it'\''s' --build"
        );
    }

    #[test]
    fn a_refusal_ends_with_the_command_to_repeat() {
        let error = anyhow::Error::from(Error::Activation(
            mix_app::profile::Error::SourceBuildRequired {
                packages: Some(vec!["cowsay-3.8.4".to_string()]),
            },
        ));

        let message = explain(
            &error,
            &["cowsay".to_string()],
            "mix install cowsay --build",
        )
        .message();

        assert!(message.starts_with("cowsay-3.8.4 must be built from source"));
        assert!(message.ends_with("To build it anyway, run: mix install cowsay --build"));
    }
}
