//! `mix-explain` turns a failure into the words the person who ran the command reads. The code
//! underneath reports facts as a coded diagnostic, but only the command knows what the reader
//! should try next, so the caller passes the command's name and action, and the words are
//! chosen here from the code and its details. The longer text `mix explain` shows for each code
//! is here too.

pub mod codes;
pub mod doctor;
pub(crate) mod render;
#[cfg(test)]
mod tests;

pub use render::warning;
pub(crate) use render::{Context, render};

use std::fmt::Display;

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
}

pub fn reset() -> Diagnostic {
    Diagnostic::new(phrase!(
        "your package list was damaged and couldn't be recovered, so it was reset"
    ))
    .help(help!("reinstall your packages with `mix install`"))
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
            Some(mix_events::v1::diagnostic::Detail::Io(_)) => diagnostic.message.trim(),
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

pub fn outcome(command: &str, action: &dyn Display, fault: &mix_events::Fault) -> Diagnostic {
    render(fault, &Context { command, action })
}

pub fn failed(action: &dyn Display) -> Diagnostic {
    Diagnostic::new(phrase!("couldn't {action}"))
        .help(help!("run it again with `-v` to see what went wrong"))
}

pub(crate) fn bug() -> Diagnostic {
    Diagnostic::new(phrase!("something went wrong inside `mix`")).help(report_a_bug())
}

pub(crate) fn report_a_bug() -> Help {
    help!("report this bug at https://github.com/recregt/mix/issues")
}
