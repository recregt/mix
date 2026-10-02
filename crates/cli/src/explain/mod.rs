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

pub mod codes;
pub mod doctor;
pub(crate) mod render;
#[cfg(test)]
mod tests;

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

pub fn words(error: &anyhow::Error, request: &mix_events::v1::command::Request) -> Diagnostic {
    if let Some(failed) = error.downcast_ref::<crate::remote::client::Failed>() {
        return outcome(Some(&failed.request), &failed.fault);
    }
    if let Some(error) = error.downcast_ref::<mix_rpc::Error>() {
        return outcome(Some(request), &rpc_fault(error));
    }
    failed(&*action_of(Some(request)))
}

pub(crate) fn fault_of(error: &anyhow::Error) -> mix_events::Fault {
    if let Some(failed) = error.downcast_ref::<crate::remote::client::Failed>() {
        return failed.fault.clone();
    }
    if let Some(error) = error.downcast_ref::<mix_rpc::Error>() {
        return rpc_fault(error);
    }
    mix_core::diagnose::failed(mix_events::v1::Code::Internal, error.to_string(), None)
}

pub fn restored(source: mix_core::change::Source) -> Option<Diagnostic> {
    match source {
        mix_core::change::Source::File | mix_core::change::Source::Generation => None,
        mix_core::change::Source::Fresh => Some(
            Diagnostic::new(phrase!(
                "your package list was damaged and couldn't be recovered, so it was reset"
            ))
            .help(help!("reinstall your packages with `mix install`")),
        ),
    }
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

pub fn command_of(request: Option<&mix_events::v1::command::Request>) -> &'static str {
    use mix_events::v1::command::Request;

    match request {
        Some(Request::Install(_)) => "mix install",
        Some(Request::Remove(_)) => "mix remove",
        Some(Request::Bootstrap(_)) => "mix bootstrap",
        Some(Request::Repair(_)) => "mix repair",
        Some(Request::Doctor(_)) => "mix doctor",
        Some(Request::Clean(_)) => "mix clean",
        None => "mix",
    }
}

fn action_of(request: Option<&mix_events::v1::command::Request>) -> Box<dyn Display + '_> {
    use mix_events::v1::command::Request;

    match request {
        Some(Request::Install(install)) => Box::new(packages_action("install", &install.packages)),
        Some(Request::Remove(remove)) => Box::new(packages_action("remove", &remove.packages)),
        Some(Request::Bootstrap(_)) => Box::new("finish setting up `mix`"),
        Some(Request::Repair(_)) => Box::new("finish the repair"),
        Some(Request::Doctor(_)) => Box::new("finish the health check"),
        Some(Request::Clean(_)) => Box::new("clean up your profile"),
        None => Box::new("finish"),
    }
}

pub fn outcome(
    request: Option<&mix_events::v1::command::Request>,
    fault: &mix_events::Fault,
) -> Diagnostic {
    render(
        fault,
        &Context {
            command: command_of(request),
            action: &*action_of(request),
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
