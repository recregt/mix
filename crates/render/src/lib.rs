#![cfg_attr(not(test), deny(clippy::wildcard_enum_match_arm))]

pub mod human;
#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod replay;
mod results;
mod trace;
mod verbs;
pub mod words;

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use mix_events::capture::Capture;
use mix_events::capture::v1::Header;
use mix_events::v1::command::Request;
use mix_events::v1::{Envelope, envelope};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Human,
    Json,
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
    pub format: Format,
    pub events_file: Option<PathBuf>,
    pub verbose: u8,
    pub quiet: bool,
    pub exit: Exit,
}

impl View {
    pub fn sinks(&self) -> std::io::Result<Sinks> {
        self.sinks_to(mix_ui::display())
    }

    fn sinks_to(&self, display: Arc<dyn mix_ui::Display>) -> std::io::Result<Sinks> {
        Ok(Sinks {
            human: (self.format == Format::Human).then(|| Human::new(display).level(self.level())),
            json: self.format == Format::Json,
            file: self
                .events_file
                .as_deref()
                .map(Recorder::create)
                .transpose()?,
            exit: self.exit.clone(),
        })
    }

    pub fn streams(&self) -> bool {
        self.format == Format::Json || self.events_file.is_some()
    }

    pub fn level(&self) -> Detail {
        level(self.quiet, self.verbose)
    }

    pub fn escalating(&self) {
        if self.format == Format::Human && self.level() >= Detail::Step {
            mix_ui::note(&words::escalating(), None);
        }
    }

    pub fn detaching(&self) {
        if self.format == Format::Human {
            mix_ui::note(&words::detached(), None);
        }
    }

    pub fn failed(
        &self,
        id: String,
        request: &Request,
        fault: &Fault,
        source: Option<&(dyn std::error::Error + 'static)>,
    ) {
        if self.streams()
            && !self.exit.started()
            && let Ok(mut sinks) = self.sinks_to(Arc::new(mix_ui::Silent))
        {
            mix_events::fail(
                id,
                mix_events::command(request.clone()),
                fault.clone(),
                &mut sinks,
            );
        }
        if self.format == Format::Human && self.exit.code().is_none() {
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
    human: Option<Human>,
    json: bool,
    file: Option<Recorder>,
    exit: Exit,
}

impl Render for Sinks {
    fn detail(&self) -> Detail {
        if self.json || self.file.is_some() {
            return Detail::Trace;
        }
        self.human.as_ref().map_or(Detail::Outcome, Render::detail)
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
        if self.json {
            json_line(&envelope);
        }
        if let Some(file) = &mut self.file {
            file.record(&envelope);
        }
        if let Some(human) = &mut self.human {
            human.envelope(envelope);
        }
    }
}

fn json_line(envelope: &Envelope) {
    if let Ok(line) = serde_json::to_string(envelope) {
        mix_ui::data(&line);
    }
}

struct Recorder {
    path: PathBuf,
    waiting: Option<File>,
    capture: Option<Capture<File>>,
    started: SystemTime,
    clock: Instant,
    failed: bool,
}

impl Recorder {
    #[allow(clippy::disallowed_methods)]
    fn create(path: &Path) -> std::io::Result<Self> {
        Ok(Self {
            path: path.to_path_buf(),
            waiting: Some(File::create(path)?),
            capture: None,
            started: SystemTime::now(),
            clock: Instant::now(),
            failed: false,
        })
    }

    fn record(&mut self, envelope: &Envelope) {
        let offset = self.clock.elapsed();
        if let Some(file) = self.waiting.take() {
            let since = self.started.duration_since(UNIX_EPOCH).unwrap_or_default();
            let header = Header {
                format: String::new(),
                mix_version: env!("CARGO_PKG_VERSION").to_string(),
                request: envelope.request.clone(),
                started: Some(pbjson_types::Timestamp {
                    seconds: i64::try_from(since.as_secs()).unwrap_or(i64::MAX),
                    nanos: i32::try_from(since.subsec_nanos()).unwrap_or(0),
                }),
            };
            match Capture::start(file, header) {
                Ok(capture) => self.capture = Some(capture),
                Err(error) => self.lost(&error),
            }
        }
        if let Some(capture) = &mut self.capture
            && let Err(error) = capture.record(offset, envelope)
        {
            self.lost(&error);
        }
    }

    fn lost(&mut self, error: &std::io::Error) {
        if !self.failed {
            self.failed = true;
            mix_ui::report(
                mix_ui::Severity::Warning,
                &mix_ui::Report::new(&mix_ui::phrase!(
                    "couldn't record events to {}",
                    self.path.display()
                ))
                .causes(vec![error.to_string()]),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use mix_events::v1::envelope::Event;
    use mix_events::v1::{Code, InstallRequest};

    use super::*;

    fn recording(file: PathBuf) -> View {
        View {
            format: Format::Human,
            events_file: Some(file),
            verbose: 0,
            quiet: false,
            exit: Exit::default(),
        }
    }

    fn install() -> Request {
        Request::Install(InstallRequest {
            packages: vec!["ripgrep".to_string()],
        })
    }

    fn refused() -> Fault {
        Fault::failed(Code::RootNotAllowed, "root", None)
    }

    fn roots(file: &Path) -> Vec<mix_events::v1::NodeFinished> {
        let captured =
            mix_events::capture::read(std::io::BufReader::new(File::open(file).unwrap())).unwrap();
        mix_events::validate(captured.envelopes.iter()).unwrap();
        captured
            .envelopes
            .into_iter()
            .filter_map(|envelope| match envelope.event {
                Some(Event::NodeFinished(finished)) if finished.id == ROOT => Some(finished),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_failure_before_any_work_still_records_a_valid_stream_with_its_code() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("events.ndjson");
        let view = recording(file.clone());

        view.failed("request".to_string(), &install(), &refused(), None);

        let roots = roots(&file);
        assert_eq!(roots.len(), 1);
        assert_eq!(
            roots[0].diagnostic.as_ref().unwrap().code(),
            Code::RootNotAllowed
        );
        assert_eq!(roots[0].exit_code, mix_events::exit::FAILED);
        assert_eq!(view.exit.code(), Some(mix_events::exit::FAILED));
    }

    #[test]
    fn a_stream_that_already_started_is_left_as_it_ended_rather_than_given_a_second_root() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("events.ndjson");
        let view = recording(file.clone());
        let mut sinks = view.sinks_to(Arc::new(mix_ui::Silent)).unwrap();
        let outbox = Arc::new(mix_events::Outbox::new("request", || {}));
        let tree = mix_events::Tree::new(
            Arc::clone(&outbox),
            Arc::new(|| None),
            mix_events::Start::command("install", mix_events::command(install())),
        );
        for envelope in outbox.drain() {
            sinks.envelope(envelope);
        }

        view.failed("other".to_string(), &install(), &refused(), None);

        drop(tree);
        assert_eq!(view.exit.code(), None);
        let started = std::fs::read_to_string(&file).unwrap();
        assert!(!started.contains("\"other\""), "{started}");
    }
}
