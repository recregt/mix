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
use mix_events::v1::{Command, Envelope, envelope, node_finished};
use mix_events::{Detail, Fault, ROOT, Render};

use human::Human;

pub use mix_ui::ColorChoice as Color;
pub use mix_ui::{confirm, interactive, report_unwritten, unwritten};

pub fn start(color: Color, draws_progress: bool) {
    mix_ui::set_color(color);
    mix_ui::init(draws_progress);
}

pub fn restore() {
    mix_ui::restore_terminal();
}

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

#[derive(Debug, Clone, Default)]
struct Seen {
    exit: Option<u32>,
}

#[derive(Clone, Default)]
pub struct Exit(Arc<Mutex<Seen>>);

impl Exit {
    pub fn code(&self) -> Option<u32> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).exit
    }

    fn saw(&self, exit: Option<u32>) {
        let mut seen = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if exit.is_some() {
            seen.exit = exit;
        }
    }
}

#[derive(Clone, Default)]
pub struct Departure(Arc<Mutex<Option<Vec<String>>>>);

impl Departure {
    pub fn others(&self) -> Option<Vec<String>> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn saw(&self, envelope: &Envelope) {
        if let Some(others) = root_departure(envelope) {
            *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(others);
        }
    }
}

pub struct View {
    pub json: Json,
    pub verbose: u8,
    pub quiet: bool,
    pub exit: Exit,
    pub departure: Departure,
    pub preview: bool,
}

impl View {
    pub fn sinks(&self) -> Sinks {
        self.sinks_to(mix_ui::display())
    }

    fn sinks_to(&self, display: Arc<dyn mix_ui::Display>) -> Sinks {
        Sinks {
            human: Human::new(display)
                .level(self.level())
                .data(!self.json.on())
                .preview(self.preview),
            document: self
                .json
                .on()
                .then(|| (document::Builder::default(), self.json.clone())),
            exit: self.exit.clone(),
            departure: self.departure.clone(),
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
        if self.exit.code().is_some() {
            return;
        }
        if self.json.on() {
            let mut documented = Documented {
                builder: document::Builder::default(),
                json: self.json.clone(),
                exit: self.exit.clone(),
            };
            mix_events::fail(id, command.clone(), fault.clone(), &mut documented);
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
    departure: Departure,
}

fn root_exit(envelope: &Envelope) -> Option<u32> {
    match &envelope.event {
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
    }
}

fn root_departure(envelope: &Envelope) -> Option<Vec<String>> {
    if let Some(envelope::Event::NodeFinished(finished)) = &envelope.event
        && finished.id == ROOT
        && let Some(node_finished::Result::UserRemove(found)) = &finished.result
    {
        return Some(found.others.clone());
    }
    None
}

struct Documented {
    builder: document::Builder,
    json: Json,
    exit: Exit,
}

impl Render for Documented {
    fn detail(&self) -> Detail {
        Detail::Action
    }

    fn envelope(&mut self, envelope: Envelope) {
        self.exit.saw(root_exit(&envelope));
        if let Some(document) = self.builder.envelope(&envelope) {
            self.json.write(document);
        }
    }
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
        self.exit.saw(root_exit(&envelope));
        self.departure.saw(&envelope);
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
            departure: Departure::default(),
            preview: false,
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
    fn stdout_carries_data_only_when_it_is_not_carrying_the_document() {
        let (json, _) = into();
        let human = View {
            json: Json::Off,
            ..json
        };
        let (json, _) = into();
        assert!(!json.sinks_to(Arc::new(mix_ui::Silent)).human.prints_data());
        assert!(human.sinks_to(Arc::new(mix_ui::Silent)).human.prints_data());
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
    fn a_stream_that_stopped_before_its_ending_is_reported_once_by_the_client() {
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
        std::mem::forget(tree);

        view.failed("other".to_string(), &install(), &refused(), None);

        let documents = documents.lock().unwrap();
        assert_eq!(documents.len(), 1);
        assert_eq!(documents[0].problems[0].code(), refused().code().unwrap());
        assert!(view.exit.code().is_some());
    }

    #[test]
    fn a_stream_that_reached_its_ending_is_not_reported_again() {
        let (view, documents) = into();
        let mut sinks = view.sinks_to(Arc::new(mix_ui::Silent));
        let outbox = Arc::new(mix_events::Outbox::new("request", || {}));
        let mut tree = mix_events::Tree::new(
            Arc::clone(&outbox),
            Arc::new(|| None),
            mix_events::Start::command("install", install()),
        );
        tree.finish(mix_events::ROOT, mix_events::Ending::succeeded())
            .unwrap();
        drop(tree);
        for envelope in outbox.drain() {
            sinks.envelope(envelope);
        }
        let ended = view.exit.code();

        view.failed("other".to_string(), &install(), &refused(), None);

        assert!(ended.is_some());
        assert_eq!(documents.lock().unwrap().len(), 1);
        assert_eq!(view.exit.code(), ended);
    }
}
