//! Turning a failure into the words the person who ran the command should read.
//!
//! The crates underneath this one raise errors that state facts: a path, a command line, an
//! errno, a list of derivations. None of them know which command is running, so none of them can
//! know what a reader should be told to try. `already locked` asks a different question of
//! someone running `mix install` than of someone running `mix repair`, and `root privileges are
//! required` is advice to re-run with sudo in one command and a refusal in another.
//!
//! So the facts travel up untouched and the words are written here, one module per command.
//! The work more than one command shares, such as activating a profile or reconciling a declared
//! target, is written once in a module of its own and told which command to name.
//! A [`Diagnostic`] is what comes out: a summary of what happened, a note with a fact the reader
//! needs, and a help with what to do about it. Each is checked when `mix` is built.

pub mod bootstrap;
pub mod clean;
pub mod codes;
pub mod doctor;
pub mod install;
pub mod remove;
pub mod repair;

pub(crate) mod change;
pub(crate) mod render;
pub mod target;

pub(crate) use render::{Context, render, rpc_fault};

use std::fmt::{self, Display};

use std::borrow::Cow;

use mix_ui::text::HelpAround;
use mix_ui::{Help, Note, Phrase, help, phrase};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    text: Cow<'static, str>,
    summary_end: usize,
    note_end: usize,
}

impl Diagnostic {
    #[inline]
    pub fn new(summary: Phrase) -> Self {
        let text = summary.into_cow();
        let end = text.len();
        Self {
            text,
            summary_end: end,
            note_end: end,
        }
    }

    #[inline]
    fn owned(&mut self, extra: usize) -> &mut String {
        if let Cow::Borrowed(text) = self.text {
            let mut owned = String::with_capacity(text.len() + extra);
            owned.push_str(text);
            self.text = Cow::Owned(owned);
        }
        self.text.to_mut()
    }

    pub fn note(mut self, note: Note) -> Self {
        let at = self.note_end;
        let note = note.as_str();
        let text = self.owned(note.len() + 1);
        text.insert(at, '\n');
        text.insert_str(at + 1, note);
        self.note_end = at + 1 + note.len();
        self
    }

    #[inline]
    pub fn help(mut self, help: Help) -> Self {
        let help = help.as_str();
        let text = self.owned(help.len() + 1);
        text.push('\n');
        text.push_str(help);
        self
    }

    pub fn hinting(summary: Phrase, help: HelpAround, value: &str) -> Self {
        Self::new(summary).help_around(help, value)
    }

    pub fn help_around(mut self, help: HelpAround, value: &str) -> Self {
        let (before, after) = help.parts();
        let parts = ["\n", before, value, after];
        let extra: usize = parts.iter().map(|part| part.len()).sum();
        let mut text = match std::mem::take(&mut self.text) {
            Cow::Borrowed(summary) => {
                let mut text = String::with_capacity(summary.len() + extra);
                text.push_str(summary);
                text
            }
            Cow::Owned(mut text) => {
                text.reserve_exact(extra);
                text
            }
        };
        for part in parts {
            text.push_str(part);
        }
        self.text = Cow::Owned(text);
        self
    }

    #[inline]
    pub fn summary_text(&self) -> &str {
        &self.text[..self.summary_end]
    }

    #[inline]
    fn note_text(&self) -> Option<&str> {
        (self.note_end > self.summary_end).then(|| &self.text[self.summary_end + 1..self.note_end])
    }

    #[inline]
    fn help_text(&self) -> Option<&str> {
        (self.text.len() > self.note_end).then(|| &self.text[self.note_end + 1..])
    }

    #[inline]
    pub fn report(&self) -> mix_ui::Report<'_> {
        mix_ui::Report::checked(self.summary_text(), self.note_text(), self.help_text())
    }

    pub fn problem<'a>(&'a self, lines: Option<mix_ui::Lines<'a>>) -> mix_ui::Problem<'a> {
        mix_ui::Problem::checked(self.summary_text(), self.note_text(), self.help_text())
            .lines(lines)
    }

    pub fn message(&self) -> String {
        self.text.to_string()
    }

    pub(crate) fn summary(self) -> Phrase {
        match self.text {
            Cow::Borrowed(text) => Phrase::checked_static(&text[..self.summary_end]),
            Cow::Owned(mut text) => {
                text.truncate(self.summary_end);
                Phrase::checked_owned(text)
            }
        }
    }
}

pub(crate) fn core_error(
    error: &mix_core::Error,
    command: &str,
    action: &dyn Display,
) -> Diagnostic {
    render::render_error(error, &Context { command, action })
}

pub(crate) fn fault_of(error: &anyhow::Error) -> mix_events::Fault {
    use mix_events::Diagnose;

    if let Some(error) = error.downcast_ref::<mix_rpc::Error>() {
        return rpc_fault(error);
    }
    if let Some(failed) = error.downcast_ref::<crate::remote::client::Failed>() {
        return failed.fault.clone();
    }
    if let Some(error) = error.downcast_ref::<mix_shell::ops::bootstrap::Error>() {
        return error.fault();
    }
    if let Some(error) = error.downcast_ref::<mix_shell::ops::remove::Error>() {
        return error.fault();
    }
    if let Some(error) = error.downcast_ref::<mix_shell::profile::change::Error>() {
        return error.fault();
    }
    if let Some(error) = error.downcast_ref::<mix_shell::target::Error>() {
        return error.fault();
    }
    if let Some(error) = error.downcast_ref::<mix_core::Error>() {
        return error.fault();
    }
    mix_core::diagnose::failed(mix_events::v1::Code::Internal, error.to_string(), None)
}

fn unworded(code: mix_events::v1::Code) -> bool {
    matches!(
        code,
        mix_events::v1::Code::Internal | mix_events::v1::Code::Unspecified
    )
}

pub fn evidence(fault: &mix_events::Fault) -> Vec<String> {
    fn collect(diagnostic: &mix_events::v1::Diagnostic, cause: bool, out: &mut Vec<String>) {
        let words = match &diagnostic.detail {
            Some(mix_events::v1::diagnostic::Detail::Command(command)) => {
                command.output_tail.trim()
            }
            _ if cause || unworded(diagnostic.code()) => diagnostic.message.trim(),
            _ => "",
        };
        if !words.is_empty() && !out.iter().any(|known| known == words) {
            out.push(words.to_string());
        }
        for inner in &diagnostic.causes {
            collect(inner, true, out);
        }
    }

    let mut out = Vec::new();
    if let mix_events::Fault::Failed(diagnostic) = fault {
        collect(diagnostic, false, &mut out);
    }
    out
}

#[cfg(test)]
mod evidence_tests {
    use mix_events::v1::{CommandDetail, Diagnostic as Wire, diagnostic::Detail};

    use super::evidence;

    #[test]
    fn evidence_is_the_programs_own_words_and_each_causes_message() {
        let fault = mix_events::Fault::Failed(Wire {
            code: mix_events::v1::Code::BuildFailed as i32,
            message: "the summary is worded elsewhere".into(),
            causes: vec![
                Wire {
                    message: "`nix build` failed".into(),
                    detail: Some(Detail::Command(CommandDetail {
                        output_tail: "error: Cannot build 'hello'.\n".into(),
                        ..CommandDetail::default()
                    })),
                    ..Wire::default()
                },
                Wire {
                    message: "/var/lib/mix/journal/r1: PermissionDenied".into(),
                    ..Wire::default()
                },
            ],
            ..Wire::default()
        });

        assert_eq!(
            evidence(&fault),
            [
                "error: Cannot build 'hello'.",
                "/var/lib/mix/journal/r1: PermissionDenied"
            ]
        );
    }

    #[test]
    fn a_failure_mix_has_no_words_for_keeps_its_producers_words() {
        for code in [
            mix_events::v1::Code::Internal,
            mix_events::v1::Code::Unspecified,
        ] {
            let fault = mix_events::Fault::Failed(Wire {
                code: code as i32,
                message: "state file ended early".into(),
                ..Wire::default()
            });

            assert_eq!(evidence(&fault), ["state file ended early"], "{code:?}");
        }
    }
}

pub fn command_of(request: Option<&mix_events::v1::command::Request>) -> &'static str {
    use mix_events::v1::command::Request;

    match request {
        Some(Request::Install(_)) => install::COMMAND,
        Some(Request::Remove(_)) => remove::COMMAND,
        Some(Request::Bootstrap(_)) => bootstrap::COMMAND,
        Some(Request::Repair(_)) => repair::COMMAND,
        Some(Request::Doctor(_)) => doctor::COMMAND,
        Some(Request::Clean(_)) => clean::COMMAND,
        None => "mix",
    }
}

pub fn outcome(
    request: Option<&mix_events::v1::command::Request>,
    fault: &mix_events::Fault,
) -> Diagnostic {
    use mix_events::v1::command::Request;

    let action: Box<dyn Display + '_> = match request {
        Some(Request::Install(install)) => Box::new(packages_action("install", &install.packages)),
        Some(Request::Remove(remove)) => Box::new(packages_action("remove", &remove.packages)),
        Some(Request::Bootstrap(_)) => Box::new(bootstrap::ACTION),
        Some(Request::Repair(_)) => Box::new(repair::ACTION),
        Some(Request::Doctor(_)) => Box::new(doctor::ACTION),
        Some(Request::Clean(_)) => Box::new(clean::ACTION),
        None => Box::new("finish"),
    };
    render(
        fault,
        &Context {
            command: command_of(request),
            action: &*action,
        },
    )
}

pub(crate) fn failed(action: &dyn Display) -> Diagnostic {
    Diagnostic::new(phrase!("couldn't {action}"))
        .help(help!("run it again with `-v` to see what went wrong"))
}

pub(crate) fn bug() -> Diagnostic {
    Diagnostic::new(phrase!("something went wrong inside `mix`")).help(help!(
        "report this bug at https://github.com/recregt/mix/issues"
    ))
}

pub(crate) fn privileged(error: &mix_rpc::Error, action: &dyn Display) -> Diagnostic {
    render(
        &rpc_fault(error),
        &Context {
            command: "",
            action,
        },
    )
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
    fn a_message_lists_the_summary_then_the_note_then_the_help() {
        let diagnostic = Diagnostic::new(phrase!("it failed"))
            .help(help!("run it again"))
            .note(mix_ui::note!("it was busy"));

        assert_eq!(diagnostic.message(), "it failed\nit was busy\nrun it again");
    }

    #[test]
    fn a_failure_without_a_note_or_help_is_its_summary() {
        assert_eq!(Diagnostic::new(phrase!("it failed")).message(), "it failed");
    }

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
        assert!(message.contains("check who owns it"));
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
            "couldn't install ripgrep\nrun it again with `-v` to see what went wrong"
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

        assert!(message.contains("report this bug"));
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
            "`mix` isn't set up yet\nrun `mix bootstrap` first"
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
             make sure your account can use sudo, then try again"
        );
    }

    #[test]
    fn a_worker_that_stopped_mid_way_says_to_run_the_command_again() {
        let message = privileged(&mix_rpc::Error::Ended, &"finish the repair").message();

        assert!(message.contains("stopped before it could finish the repair"));
        assert!(message.contains("run the same command again"));
    }
}
