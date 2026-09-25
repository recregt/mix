//! Turning a failure into the words the person who ran the command should read.
//!
//! The crates underneath this one raise errors that state facts: a path, a command line, an
//! errno, a list of derivations. None of them know which command is running, so none of them can
//! know what a reader should be told to try — `already locked` asks a different question of
//! someone running `mix install` than of someone running `mix repair`, and `root privileges are
//! required` is advice to re-run with sudo in one command and a refusal in another.
//!
//! So the facts travel up untouched and the sentences are written here, one module per command.
//! The work more than one command shares — activating a profile, reconciling a declared target —
//! is written once in a module of its own and told which command to name.
//! A [`Diagnostic`] is what comes out: what happened, and — when there is one worth giving — what
//! to do about it on the line underneath.

pub mod bootstrap;
pub mod doctor;
pub mod install;
pub mod remove;
pub mod repair;

mod activation;
pub(crate) mod change;
pub mod target;

use std::borrow::Cow;

/// A failure in the words the reader should see: what happened, and what to do next.
#[derive(Debug, PartialEq, Eq)]
pub struct Diagnostic {
    summary: Cow<'static, str>,
    hint: Option<Cow<'static, str>>,
}

impl Diagnostic {
    /// A failure that speaks for itself.
    pub fn new(summary: impl Into<Cow<'static, str>>) -> Self {
        Self {
            summary: summary.into(),
            hint: None,
        }
    }

    /// A failure with the way out written under it.
    pub fn hinting(
        summary: impl Into<Cow<'static, str>>,
        hint: impl Into<Cow<'static, str>>,
    ) -> Self {
        Self {
            summary: summary.into(),
            hint: Some(hint.into()),
        }
    }

    /// The whole message, hint included, as one block for [`mix_ui`] to print.
    ///
    /// The hint is a line of its own: the printer indents continuation lines under the marker, so
    /// a failure reads as one block rather than as a line and an afterthought.
    pub fn message(&self) -> String {
        let Some(hint) = &self.hint else {
            return self.summary.as_ref().to_owned();
        };

        let mut out = String::with_capacity(self.summary.len() + hint.len() + 1);
        out.push_str(&self.summary);
        out.push('\n');
        out.push_str(hint);
        out
    }
}

const REPORT_BUG: &str =
    "This is a bug in `mix`; please report it at https://github.com/recregt/mix/issues";

/// What a raw `mix-core` failure means to somebody who ran `command`.
///
/// These are the errors every command can hit — the lock, a file, a process that would not
/// start. The fact is the library's; naming the command to run again is not something it could
/// have done.
pub(crate) fn core_error(error: &mix_core::Error, command: &str, action: &str) -> Diagnostic {
    use mix_core::Error;

    match error {
        Error::Locked { .. } => Diagnostic::hinting(
            "another `mix` command is already running",
            format!("Wait for it to finish, then run `{command}` again"),
        ),
        Error::Cancelled { .. } => Diagnostic::new("interrupted before it could finish"),
        Error::Io { path, source } if source.kind() == std::io::ErrorKind::PermissionDenied => {
            Diagnostic::hinting(
                format!("no permission to use {}", path.display()),
                format!("Check who owns it, then run `{command}` again"),
            )
        }
        Error::Io { .. } | Error::Command { .. } | Error::Exec { .. } => failed(action),
        Error::TaskPanicked(_) => bug(),
    }
}

pub(crate) fn failed(action: &str) -> Diagnostic {
    Diagnostic::hinting(
        format!("couldn't {action}"),
        "Run it again with `-v` to see what went wrong",
    )
}

pub(crate) fn bug() -> Diagnostic {
    Diagnostic::hinting("something went wrong inside `mix`", REPORT_BUG)
}

pub(crate) fn packages_action(verb: &str, packages: &[String]) -> String {
    match packages.len() {
        0 => format!("{verb} the packages"),
        1..=3 => format!("{verb} {}", packages.join(", ")),
        n => format!("{verb} {n} packages"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hint_is_printed_on_the_line_under_the_summary() {
        let diagnostic = Diagnostic::hinting("it failed", "try again");

        assert_eq!(diagnostic.message(), "it failed\ntry again");
    }

    #[test]
    fn a_failure_without_a_hint_is_left_as_it_was_written() {
        assert_eq!(Diagnostic::new("it failed").message(), "it failed");
    }

    /// The same raw error, two commands: the lock is the fact, the command to retry is not.
    #[test]
    fn a_held_lock_names_the_command_the_reader_ran() {
        let error = mix_core::Error::Locked {
            path: "/run/mix.lock".into(),
        };

        let install = core_error(&error, "mix install", "install ripgrep").message();
        let repair = core_error(&error, "mix repair", "finish the repair").message();

        assert!(!install.contains("/run/mix.lock"));
        assert!(install.contains("run `mix install` again"));
        assert!(repair.contains("run `mix repair` again"));
    }

    #[test]
    fn a_refused_path_says_whose_permission_is_missing() {
        let error = mix_core::Error::Io {
            path: "/nix/store".into(),
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        };

        let message = core_error(&error, "mix doctor", "finish the health check").message();

        assert!(message.starts_with("no permission to use /nix/store"));
        assert!(message.contains("Check who owns it"));
    }

    #[test]
    fn a_failed_step_says_what_could_not_be_done_and_keeps_the_internals_out() {
        let error = mix_core::Error::Command {
            command: "/nix/var/nix/profiles/default/bin/nix build path:/home/ada".to_string(),
            detail: "error: out of disk space".to_string(),
        };

        let message = core_error(&error, "mix install", "install ripgrep").message();

        assert_eq!(
            message,
            "couldn't install ripgrep\nRun it again with `-v` to see what went wrong"
        );
    }

    #[test]
    fn a_bug_is_called_a_bug_and_says_where_to_report_it() {
        let message = core_error(
            &mix_core::Error::TaskPanicked("oops".to_string()),
            "mix install",
            "install ripgrep",
        )
        .message();

        assert!(message.contains("bug in `mix`"));
        assert!(message.contains("github.com/recregt/mix/issues"));
        assert!(!message.contains("oops"));
    }

    #[test]
    fn a_long_list_of_packages_is_counted() {
        let packages: Vec<String> = ["a", "b", "c", "d"].map(String::from).to_vec();

        assert_eq!(packages_action("install", &packages[..1]), "install a");
        assert_eq!(
            packages_action("install", &packages[..3]),
            "install a, b, c"
        );
        assert_eq!(packages_action("install", &packages), "install 4 packages");
    }
}
