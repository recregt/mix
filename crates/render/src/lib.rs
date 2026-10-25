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
use mix_events::{Detail, Fault, ROOT};

use human::Human;

pub use mix_ui::ColorChoice as Color;
pub use mix_ui::{confirm, interactive};

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

    pub fn write(&self, document: Document) -> std::io::Result<()> {
        match self {
            Json::Off => Ok(()),
            Json::Stdout => document::print(&document),
            Json::Into(documents) => {
                documents
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(document);
                Ok(())
            }
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
        self.sinks_to(mix_ui::display(), Box::new(mix_ui::Stdout))
    }

    fn sinks_to(
        &self,
        display: Arc<dyn mix_ui::Display>,
        data: Box<dyn std::io::Write + Send>,
    ) -> Sinks {
        Sinks {
            human: Human::new(display)
                .level(self.level())
                .data((!self.json.on()).then_some(data))
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
    ) -> std::io::Result<()> {
        if self.exit.code().is_some() {
            return Ok(());
        }
        let documented = match self.json.on() {
            true => self.documented(id, command, fault),
            false => Ok(()),
        };
        let Some(request) = &command.request else {
            return documented;
        };
        let words = words::outcome(request, &[], fault);
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
        documented
    }

    fn documented(&self, id: String, command: &Command, fault: &Fault) -> std::io::Result<()> {
        let mut builder = document::Builder::default();
        for envelope in mix_events::failure(id, command.clone(), fault.clone()) {
            self.exit.saw(root_exit(&envelope));
            if let Some(document) = builder.envelope(&envelope) {
                self.json.write(document)?;
            }
        }
        Ok(())
    }
}

pub fn report_unwritten(error: &std::io::Error) {
    mix_ui::report(
        mix_ui::Severity::Error,
        &mix_ui::Report::new(&mix_ui::phrase!("`mix` couldn't write its output"))
            .causes(vec![format!("standard output: {}", error.kind())]),
    );
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

impl Sinks {
    pub fn envelope(&mut self, envelope: Envelope) -> std::io::Result<()> {
        self.exit.saw(root_exit(&envelope));
        self.departure.saw(&envelope);
        let documented = match &mut self.document {
            Some((builder, json)) => builder
                .envelope(&envelope)
                .map_or(Ok(()), |document| json.write(document)),
            None => Ok(()),
        };
        let shown = self.human.envelope(envelope);
        documented.and(shown)
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
        assert!(
            !json
                .sinks_to(Arc::new(mix_ui::Silent), Box::new(std::io::sink()))
                .human
                .prints_data()
        );
        assert!(
            human
                .sinks_to(Arc::new(mix_ui::Silent), Box::new(std::io::sink()))
                .human
                .prints_data()
        );
    }

    #[test]
    fn a_failure_before_any_work_still_ends_in_a_document_with_its_code() {
        let (view, documents) = into();

        view.failed("request".to_string(), &install(), &refused(), None)
            .unwrap();

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
        let mut sinks = view.sinks_to(Arc::new(mix_ui::Silent), Box::new(std::io::sink()));
        let outbox = Arc::new(mix_events::Outbox::new("request", || {}));
        let tree = mix_events::Tree::new(
            Arc::clone(&outbox),
            Arc::new(|| None),
            mix_events::Start::command("install", install()),
        );
        for envelope in outbox.drain() {
            sinks.envelope(envelope).unwrap();
        }
        std::mem::forget(tree);

        view.failed("other".to_string(), &install(), &refused(), None)
            .unwrap();

        let documents = documents.lock().unwrap();
        assert_eq!(documents.len(), 1);
        assert_eq!(documents[0].problems[0].code(), refused().code().unwrap());
        assert!(view.exit.code().is_some());
    }

    #[test]
    fn a_stream_that_reached_its_ending_is_not_reported_again() {
        let (view, documents) = into();
        let mut sinks = view.sinks_to(Arc::new(mix_ui::Silent), Box::new(std::io::sink()));
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
            sinks.envelope(envelope).unwrap();
        }
        let ended = view.exit.code();

        view.failed("other".to_string(), &install(), &refused(), None)
            .unwrap();

        assert!(ended.is_some());
        assert_eq!(documents.lock().unwrap().len(), 1);
        assert_eq!(view.exit.code(), ended);
    }

    struct Full;

    impl std::io::Write for Full {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::StorageFull.into())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_list_that_could_not_be_written_says_why_and_the_commands_own_exit_is_kept() {
        let (view, _) = into();
        let view = View {
            json: Json::Off,
            ..view
        };
        let mut sinks = view.sinks_to(Arc::new(mix_ui::Silent), Box::new(Full));
        let outbox = Arc::new(mix_events::Outbox::new("request", || {}));
        let mut tree = mix_events::Tree::new(
            Arc::clone(&outbox),
            Arc::new(|| None),
            mix_events::Start::command(
                "list",
                mix_events::command(Request::List(mix_events::v1::ListRequest::default())),
            ),
        );
        let listed = mix_events::v1::ListResult {
            packages: vec![mix_events::v1::InstalledPackage {
                name: "curl".into(),
                ..mix_events::v1::InstalledPackage::default()
            }],
            ..mix_events::v1::ListResult::default()
        };
        tree.finish(
            mix_events::ROOT,
            mix_events::Ending::succeeded()
                .with_result(node_finished::Result::List(listed))
                .for_root(false),
        )
        .unwrap();
        drop(tree);

        let written = outbox
            .drain()
            .into_iter()
            .try_for_each(|envelope| sinks.envelope(envelope));

        assert_eq!(
            written.map_err(|error| error.kind()),
            Err(std::io::ErrorKind::StorageFull)
        );
        assert_eq!(view.exit.code(), Some(mix_events::exit::SUCCEEDED));
    }
}
