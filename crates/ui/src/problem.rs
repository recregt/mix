use annotate_snippets::renderer::DecorStyle;
use annotate_snippets::{AnnotationKind, Group, Level, Patch, Renderer, Snippet};
use mix_core::health::Hunk;

use crate::text::{Help, Note, NoteAround, Phrase};
use crate::{Out, Severity};

pub struct Labels {
    pub added: Phrase,
    pub changed: Phrase,
    pub missing: NoteAround,
    pub fix: Phrase,
}

pub struct Lines<'a> {
    pub path: &'a str,
    pub hunks: &'a [Hunk],
    pub labels: &'a Labels,
}

pub struct Problem<'a> {
    summary: &'a str,
    lines: Option<Lines<'a>>,
    note: Option<&'a str>,
    help: Option<&'a str>,
}

impl<'a> Problem<'a> {
    pub fn new(summary: &'a Phrase) -> Self {
        Self::checked(summary.as_str(), None, None)
    }

    #[doc(hidden)]
    pub fn checked(summary: &'a str, note: Option<&'a str>, help: Option<&'a str>) -> Self {
        Self {
            summary,
            lines: None,
            note,
            help,
        }
    }

    pub fn lines(mut self, lines: Option<Lines<'a>>) -> Self {
        if lines.is_some() {
            self.note = None;
        }
        self.lines = lines;
        self
    }

    pub fn note(mut self, note: &'a Note) -> Self {
        self.note = Some(note.as_str());
        self
    }

    pub fn help(mut self, help: &'a Help) -> Self {
        self.help = Some(help.as_str());
        self
    }
}

fn joined(lines: &[String]) -> String {
    lines.join("\n")
}

pub fn problem_text(severity: Severity, problem: &Problem<'_>, colours: bool) -> String {
    let level = match severity {
        Severity::Error => Level::ERROR,
        Severity::Warning => Level::WARNING,
    };
    let sources: Vec<(String, String)> = problem
        .lines
        .iter()
        .flat_map(|lines| lines.hunks.iter())
        .map(|hunk| (joined(&hunk.found), joined(&hunk.expected)))
        .collect();
    let mut missing = Vec::new();
    let mut title = Group::with_title(level.primary_title(problem.summary));
    let mut fix = None;
    if let Some(lines) = &problem.lines {
        let mut suggestion =
            Group::with_title(Level::HELP.secondary_title(lines.labels.fix.as_str()));
        for (hunk, (found, expected)) in lines.hunks.iter().zip(&sources) {
            let start = usize::try_from(hunk.found_line).unwrap_or(usize::MAX);
            if hunk.found.is_empty() {
                missing.push(hunk.found_line);
                suggestion = suggestion.element(
                    Snippet::source("")
                        .path(lines.path)
                        .line_start(start)
                        .patch(Patch::new(0..0, format!("{expected}\n"))),
                );
                continue;
            }
            let label = if hunk.expected.is_empty() {
                lines.labels.added.as_str()
            } else {
                lines.labels.changed.as_str()
            };
            let mut snippet = Snippet::source(found.as_str())
                .path(lines.path)
                .line_start(start);
            let mut at = 0;
            for line in &hunk.found {
                snippet = snippet.annotation(
                    AnnotationKind::Primary
                        .span(at..at + line.len())
                        .label(label),
                );
                at += line.len() + 1;
            }
            title = title.element(snippet);
            suggestion = suggestion.element(
                Snippet::source(found.as_str())
                    .path(lines.path)
                    .line_start(start)
                    .patch(Patch::new(0..found.len(), expected.as_str())),
            );
        }
        fix = Some(suggestion);
    }
    let mut digits = [0u8; 22];
    let missing_notes: Vec<Note> = match &problem.lines {
        Some(lines) => missing
            .iter()
            .map(|line| {
                lines
                    .labels
                    .missing
                    .note(crate::text::decimal(&mut digits, u64::from(*line)))
            })
            .collect(),
        None => Vec::new(),
    };
    for note in missing_notes.iter().map(Note::as_str).chain(problem.note) {
        title = title.element(Level::NOTE.message(note));
    }
    if let Some(help) = problem.help {
        title = title.element(Level::HELP.message(help));
    }
    let renderer = if colours {
        Renderer::styled()
    } else {
        Renderer::plain()
    }
    .decor_style(DecorStyle::Ascii);
    let report: Vec<Group<'_>> = std::iter::once(title).chain(fix).collect();
    renderer.render(&report)
}

pub fn problem_to(out: &dyn Out, severity: Severity, problem: &Problem<'_>) {
    out.line(&problem_text(severity, problem, out.colours()));
}
