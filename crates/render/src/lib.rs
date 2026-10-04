#![cfg_attr(not(test), deny(clippy::wildcard_enum_match_arm))]

pub mod document;
pub mod human;
#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod replay;
mod results;
mod trace;
mod verbs;
pub mod words;

use std::sync::{Arc, Mutex, PoisonError};

use mix_events::result::v1::Result as Document;
use mix_events::v1::{Command, Envelope, envelope};
use mix_events::{Detail, Fault, ROOT, Render};

use human::Human;

pub use mix_ui::ColorChoice as Color;

pub fn start(color: Color, draws_progress: bool) {
    mix_ui::set_color(color);
    mix_ui::init(draws_progress);
}

pub fn restore() {
    mix_ui::restore_terminal();
}

/// Where the result document goes.
#[derive(Clone, Default)]
pub enum Json {
    #[default]
    Off,
    Stdout,
    Into(Arc<Mutex<Vec<Document>>>),
}

impl Json {
    fn on(&self) -> bool {
        !matches!(self, Json::Off)
    }

    pub fn write(&self, document: Document) {
        match self {
            Json::Off => {}
            Json::Stdout => document::print(&document),
            Json::Into(documents) => documents
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(document),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Seen {
    started: bool,
    exit: Option<u32>,
}

#[derive(Clone, Default)]
pub struct Exit(Arc<Mutex<Seen>>);

impl Exit {
    pub fn code(&self) -> Option<u32> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).exit
    }

    pub fn started(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .started
    }

    fn saw(&self, exit: Option<u32>) {
        let mut seen = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        seen.started = true;
        if exit.is_some() {
            seen.exit = exit;
        }
    }
}

pub struct View {
    pub json: Json,
    pub verbose: u8,
    pub quiet: bool,
    pub exit: Exit,
}

impl View {
    pub fn sinks(&self) -> Sinks {
        self.sinks_to(mix_ui::display())
    }

    fn sinks_to(&self, display: Arc<dyn mix_ui::Display>) -> Sinks {
        Sinks {
            human: Human::new(display).level(self.level()),
            document: self
                .json
                .on()
                .then(|| (document::Builder::default(), self.json.clone())),
            exit: self.exit.clone(),
        }
    }

    pub fn level(&self) -> Detail {
        level(self.quiet, self.verbose)
    }

    pub fn escalating(&self) {
        if self.level() >= Detail::Step {
            mix_ui::note(&words::escalating(), None);
        }
    }

    pub fn detaching(&self) {
        mix_ui::note(&words::detached(), None);
    }

    pub fn failed(
        &self,
        id: String,
        command: &Command,
        fault: &Fault,
        source: Option<&(dyn std::error::Error + 'static)>,
    ) {
        if self.exit.started() {
            return;
        }
        if self.json.on() {
            let mut sinks = self.sinks_to(Arc::new(mix_ui::Silent));
            mix_events::fail(id, command.clone(), fault.clone(), &mut sinks);
        }
        let Some(request) = &command.request else {
            return;
        };
        let words = words::outcome(request, fault);
        let code = (self.verbose > 0)
            .then(|| fault.code())
            .flatten()
            .map(mix_events::code::kebab);
        let mut causes = mix_explain::evidence(fault);
        for cause in mix_ui::causes_of(source, words.summary_text()) {
            if !causes.iter().any(|known| known.contains(&cause)) {
                causes.push(cause);
            }
        }
        mix_ui::report(
            mix_ui::Severity::Error,
            &words.report().code(code.as_deref()).causes(causes),
        );
    }
}

pub fn level(quiet: bool, verbose: u8) -> Detail {
    match (quiet, verbose) {
        (true, _) => Detail::Outcome,
        (false, 0) => Detail::Step,
        (false, 1) => Detail::Action,
        (false, _) => Detail::Trace,
    }
}

pub struct Sinks {
    human: Human,
    document: Option<(document::Builder, Json)>,
    exit: Exit,
}

impl Render for Sinks {
    fn detail(&self) -> Detail {
        let human = self.human.detail();
        match self.document {
            Some(_) => human.max(Detail::Action),
            None => human,
        }
    }

    fn envelope(&mut self, envelope: Envelope) {
        self.exit.saw(match &envelope.event {
            Some(envelope::Event::NodeFinished(finished)) if finished.id == ROOT => {
                Some(finished.exit_code)
            }
            Some(
                envelope::Event::NodeFinished(_)
                | envelope::Event::NodeStarted(_)
                | envelope::Event::NodeProgress(_)
                | envelope::Event::NotRun(_)
                | envelope::Event::Diagnostic(_),
            )
            | None => None,
        });
        if let Some((builder, json)) = &mut self.document
            && let Some(document) = builder.envelope(&envelope)
        {
            json.write(document);
        }
        self.human.envelope(envelope);
    }
}

#[cfg(test)]
mod tests {
    use mix_events::v1::command::Request;
    use mix_events::v1::{Code, InstallRequest, Status};

    use super::*;

    fn into() -> (View, Arc<Mutex<Vec<Document>>>) {
        let documents = Arc::new(Mutex::new(Vec::new()));
        let view = View {
            json: Json::Into(Arc::clone(&documents)),
            verbose: 0,
            quiet: true,
            exit: Exit::default(),
        };
        (view, documents)
    }

    fn install() -> Command {
        mix_events::command(Request::Install(InstallRequest {
            packages: vec!["ripgrep".to_string()],
        }))
    }

    fn refused() -> Fault {
        Fault::failed(Code::RootNotAllowed, "root", None)
    }

    #[test]
    fn a_failure_before_any_work_still_ends_in_a_document_with_its_code() {
        let (view, documents) = into();

        view.failed("request".to_string(), &install(), &refused(), None);

        let documents = documents.lock().unwrap();
        let [document] = documents.as_slice() else {
            panic!("one document: {documents:?}");
        };
        assert_eq!(document.command, "install");
        assert_eq!(document.status(), Status::Failed);
        assert_eq!(document.exit, mix_events::exit::FAILED);
        assert_eq!(document.problems[0].code(), Code::RootNotAllowed);
        assert_eq!(view.exit.code(), Some(mix_events::exit::FAILED));
    }

    #[test]
    fn a_stream_that_already_started_is_left_as_it_ended_rather_than_given_a_second_root() {
        let (view, documents) = into();
        let mut sinks = view.sinks_to(Arc::new(mix_ui::Silent));
        let outbox = Arc::new(mix_events::Outbox::new("request", || {}));
        let tree = mix_events::Tree::new(
            Arc::clone(&outbox),
            Arc::new(|| None),
            mix_events::Start::command("install", install()),
        );
        for envelope in outbox.drain() {
            sinks.envelope(envelope);
        }

        view.failed("other".to_string(), &install(), &refused(), None);

        drop(tree);
        assert_eq!(view.exit.code(), None);
        assert!(documents.lock().unwrap().is_empty());
    }
}
