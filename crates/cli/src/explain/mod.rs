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

/// What a raw `mix-core` failure means to somebody who ran `command`.
///
/// These are the errors every command can hit — the lock, a file, a process that would not
/// start. The fact is the library's; naming the command to run again is not something it could
/// have done.
pub(crate) fn core_error(error: &mix_core::Error, command: &str) -> Diagnostic {
    use mix_core::Error;

    match error {
        Error::Locked { path } => Diagnostic::hinting(
            "another `mix` command is already running".to_string(),
            format!(
                "It holds {}; wait for it to finish, then run `{command}` again",
                path.display()
            ),
        ),
        Error::Cancelled { .. } => Diagnostic::new("interrupted before it could finish"),
        Error::Io { path, source } if source.kind() == std::io::ErrorKind::PermissionDenied => {
            Diagnostic::hinting(
                format!("not allowed to use {}", path.display()),
                format!("Check who owns it, then run `{command}` again"),
            )
        }
        Error::Io { .. } | Error::Command { .. } | Error::Exec { .. } => {
            Diagnostic::new(error.to_string())
        }
        Error::TaskPanicked(_) => Diagnostic::hinting(
            error.to_string(),
            "This is a bug in `mix`; please report it with the output above",
        ),
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

        let install = core_error(&error, "mix install").message();
        let repair = core_error(&error, "mix repair").message();

        assert!(install.contains("/run/mix.lock"));
        assert!(install.contains("run `mix install` again"));
        assert!(repair.contains("run `mix repair` again"));
    }

    #[test]
    fn a_refused_path_says_whose_permission_is_missing() {
        let error = mix_core::Error::Io {
            path: "/nix/store".into(),
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        };

        let message = core_error(&error, "mix doctor").message();

        assert!(message.starts_with("not allowed to use /nix/store"));
        assert!(message.contains("Check who owns it"));
    }
}
