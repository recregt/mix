use annotate_snippets::renderer::DecorStyle;
use annotate_snippets::{AnnotationKind, Group, Level, Patch, Renderer, Snippet};
use mix_events::v1::Hunk;

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

fn fitted(text: &str, start: usize, width: Option<usize>) -> String {
    let Some(width) = width else {
        return text.to_string();
    };
    let gutter = start.max(1).ilog10() as usize + 1 + " - ".len();
    let room = width.saturating_sub(gutter);
    text.lines()
        .map(|line| console::truncate_str(line, room, "…").into_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn problem_text(
    severity: Severity,
    problem: &Problem<'_>,
    colours: bool,
    width: Option<usize>,
) -> String {
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
            let shown = fitted(found, start, width);
            suggestion = suggestion.element(
                Snippet::source(shown.clone())
                    .path(lines.path)
                    .line_start(start)
                    .patch(Patch::new(0..shown.len(), fitted(expected, start, width))),
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
    let renderer = match width {
        Some(width) => renderer.term_width(width),
        None => renderer,
    };
    let report: Vec<Group<'_>> = std::iter::once(title).chain(fix).collect();
    renderer.render(&report)
}

pub fn problem_to(out: &dyn Out, severity: Severity, problem: &Problem<'_>) {
    out.line(&problem_text(severity, problem, out.colours(), out.width()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels() -> Labels {
        Labels {
            added: crate::phrase!("not written by `mix`"),
            changed: crate::phrase!("differs from what `mix` wrote"),
            missing: crate::note_around!("lines `mix` wrote are missing at line ", ""),
            fix: crate::phrase!("`mix repair` puts back what `mix` wrote"),
        }
    }

    fn drift(found: &str, width: Option<usize>) -> String {
        let labels = labels();
        let hunks = [Hunk {
            found_line: 19,
            found: vec![found.to_string()],
            expected: vec!["  home.stateVersion = \"25.05\";".into()],
        }];
        let summary =
            crate::phrase!("/home/alice/.local/state/mix/home.nix was changed outside `mix`");
        let problem = Problem::new(&summary).lines(Some(Lines {
            path: "/home/alice/.local/state/mix/home.nix",
            hunks: &hunks,
            labels: &labels,
        }));
        problem_text(Severity::Warning, &problem, false, width)
    }

    fn long_line() -> String {
        format!("  home.stateVersion = \"25.05\";{}", "A".repeat(10_000))
    }

    fn content(shown: &str) -> Vec<&str> {
        shown
            .lines()
            .filter(|line| line.starts_with("19 "))
            .collect()
    }

    #[test]
    fn on_a_terminal_every_changed_line_of_a_drift_fits_its_width() {
        let shown = drift(&long_line(), Some(80));

        let lines = content(&shown);
        assert_eq!(lines.len(), 3, "{shown}");
        for line in lines {
            assert!(console::measure_text_width(line) <= 80, "{line}");
        }
        assert!(
            shown.contains("19 -   home.stateVersion = \"25.05\";AAA"),
            "{shown}"
        );
        assert!(
            shown.contains("19 +   home.stateVersion = \"25.05\";"),
            "{shown}"
        );
    }

    #[test]
    fn away_from_a_terminal_the_repair_suggestion_keeps_the_whole_line() {
        let line = long_line();

        let shown = drift(&line, None);

        assert!(shown.contains(&format!("19 - {line}")));
    }

    #[test]
    fn a_drift_that_fits_reads_the_same_on_a_terminal() {
        let line = "  home.stateVersion = \"24.11\";";

        assert_eq!(drift(line, Some(80)), drift(line, None));
    }
}
