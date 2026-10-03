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

use mix_events::Render;
use mix_events::capture::Capture;
use mix_events::capture::v1::Header;
use mix_events::v1::{Envelope, envelope};
use mix_events::{Detail, ROOT};

use human::Human;

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
    pub fn sinks(&self, display: Arc<dyn mix_ui::Display>) -> std::io::Result<Sinks> {
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
