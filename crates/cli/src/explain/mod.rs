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
use std::fmt::{self, Display};

/// A failure in the words the reader should see: what happened, and what to do next.
#[derive(Debug, PartialEq, Eq)]
pub struct Diagnostic {
    text: Cow<'static, str>,
    summary_len: usize,
}

impl Diagnostic {
    /// A failure that speaks for itself.
    pub fn new(summary: impl Into<Cow<'static, str>>) -> Self {
        let text = summary.into();
        Self {
            summary_len: text.len(),
            text,
        }
    }

    /// A failure with the way out written under it.
    pub fn hinting(
        summary: impl Into<Cow<'static, str>>,
        hint: impl Into<Cow<'static, str>>,
    ) -> Self {
        let hint = hint.into();
        let mut text = match summary.into() {
            Cow::Owned(mut summary) => {
                summary.reserve_exact(1 + hint.len());
                summary
            }
            Cow::Borrowed(summary) => {
                let mut text = String::with_capacity(summary.len() + 1 + hint.len());
                text.push_str(summary);
                text
            }
        };
        let summary_len = text.len();
        text.push('\n');
        text.push_str(&hint);
        Self {
            text: text.into(),
            summary_len,
        }
    }

    /// The whole message, hint included, as one block for [`mix_ui`] to print.
    ///
    /// The hint is a line of its own: the printer indents continuation lines under the marker, so
    /// a failure reads as one block rather than as a line and an afterthought.
    pub(crate) fn written(text: String, summary_len: usize) -> Self {
        Self {
            text: text.into(),
            summary_len,
        }
    }

    pub fn message(self) -> String {
        self.text.into_owned()
    }

    pub(crate) fn summary(self) -> Cow<'static, str> {
        match self.text {
            Cow::Borrowed(text) => Cow::Borrowed(&text[..self.summary_len]),
            Cow::Owned(mut text) => {
                text.truncate(self.summary_len);
                Cow::Owned(text)
            }
        }
    }
}

const REPORT_BUG: &str =
    "This is a bug in `mix`; please report it at https://github.com/recregt/mix/issues";

/// What a raw `mix-core` failure means to somebody who ran `command`.
///
/// These are the errors every command can hit — the lock, a file, a process that would not
/// start. The fact is the library's; naming the command to run again is not something it could
/// have done.
pub(crate) fn core_error(
    error: &mix_core::Error,
    command: &str,
    action: &dyn Display,
) -> Diagnostic {
    use mix_core::Error;

    match error {
        Error::Locked { .. } => Diagnostic::hinting(
            "another `mix` command is already running",
            format!("Wait for it to finish, then run `{command}` again"),
        ),
        Error::LockMissing { .. } => {
            Diagnostic::hinting("`mix` isn't set up yet", "Run `mix bootstrap` first")
        }
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

pub(crate) fn failed(action: &dyn Display) -> Diagnostic {
    Diagnostic::hinting(
        format!("couldn't {action}"),
        "Run it again with `-v` to see what went wrong",
    )
}

pub(crate) fn bug() -> Diagnostic {
    Diagnostic::hinting("something went wrong inside `mix`", REPORT_BUG)
}

pub(crate) fn privileged(error: &mix_rpc::Error, action: &dyn Display) -> Diagnostic {
    use mix_rpc::Error;

    match error {
        Error::Spawn(_) | Error::Connect(_) | Error::Refused(_) => Diagnostic::hinting(
            format!("couldn't get administrator rights to {action}"),
            "Make sure your account can use sudo, then try again",
        ),
        Error::Ended => failed(action),
        Error::Malformed(_) | Error::NotAConnection(_) => bug(),
    }
}

pub(crate) struct PackagesAction<'a> {
    verb: &'static str,
    packages: &'a [String],
}

impl Display for PackagesAction<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.verb)?;
        match self.packages {
            [] => f.write_str(" the packages"),
            [first, rest @ ..] if rest.len() < 3 => {
                f.write_str(" ")?;
                f.write_str(first)?;
                rest.iter().try_for_each(|package| {
                    f.write_str(", ")?;
                    f.write_str(package)
                })
            }
            packages => write!(f, " {} packages", packages.len()),
        }
    }
}

pub(crate) fn packages_action<'a>(
    verb: &'static str,
    packages: &'a [String],
) -> PackagesAction<'a> {
    PackagesAction { verb, packages }
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
            path: "/var/lib/mix/lock".into(),
        };

        let install = core_error(&error, "mix install", &"install ripgrep").message();
        let repair = core_error(&error, "mix repair", &"finish the repair").message();

        assert!(!install.contains("/var/lib/mix/lock"));
        assert!(install.contains("run `mix install` again"));
        assert!(repair.contains("run `mix repair` again"));
    }

    #[test]
    fn a_refused_path_says_whose_permission_is_missing() {
        let error = mix_core::Error::Io {
            path: "/nix/store".into(),
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        };

        let message = core_error(&error, "mix doctor", &"finish the health check").message();

        assert!(message.starts_with("no permission to use /nix/store"));
        assert!(message.contains("Check who owns it"));
    }

    #[test]
    fn a_failed_step_says_what_could_not_be_done_and_keeps_the_internals_out() {
        let error = mix_core::Error::Command {
            command: "/nix/var/nix/profiles/default/bin/nix build path:/home/ada".to_string(),
            detail: "error: out of disk space".to_string(),
        };

        let message = core_error(&error, "mix install", &"install ripgrep").message();

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
            &"install ripgrep",
        )
        .message();

        assert!(message.contains("bug in `mix`"));
        assert!(message.contains("github.com/recregt/mix/issues"));
        assert!(!message.contains("oops"));
    }

    #[test]
    fn a_long_list_of_packages_is_counted() {
        let packages: Vec<String> = ["a", "b", "c", "d"].map(String::from).to_vec();

        assert_eq!(
            packages_action("install", &packages[..1]).to_string(),
            "install a"
        );
        assert_eq!(
            packages_action("install", &packages[..3]).to_string(),
            "install a, b, c"
        );
        assert_eq!(
            packages_action("install", &packages).to_string(),
            "install 4 packages"
        );
    }

    #[test]
    fn a_missing_lock_sends_the_reader_to_bootstrap() {
        let error = mix_core::Error::LockMissing {
            path: "/var/lib/mix/lock".into(),
        };

        assert_eq!(
            core_error(&error, "mix install", &"install ripgrep").message(),
            "`mix` isn't set up yet\nRun `mix bootstrap` first"
        );
    }

    #[test]
    fn a_refused_sudo_is_about_administrator_rights() {
        let message = privileged(
            &mix_rpc::Error::Refused("connection closed".into()),
            &"finish the repair",
        )
        .message();

        assert_eq!(
            message,
            "couldn't get administrator rights to finish the repair\n\
             Make sure your account can use sudo, then try again"
        );
    }

    #[test]
    fn a_worker_that_stopped_mid_way_points_at_the_details() {
        assert!(
            privileged(&mix_rpc::Error::Ended, &"finish the repair")
                .message()
                .contains("with `-v`")
        );
    }
}
