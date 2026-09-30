#![cfg_attr(not(test), deny(clippy::wildcard_enum_match_arm))]

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use mix_events::capture::Capture;
use mix_events::capture::v1::Header;
use mix_events::v1::{Envelope, envelope};
use mix_events::{Detail, ROOT};
use mix_shell::render::Render;

use super::human::Human;
use crate::cli::Output;
use crate::controls::Stopping;

#[derive(Clone, Default)]
pub struct Exit(Arc<Mutex<Option<u32>>>);

impl Exit {
    pub fn code(&self) -> Option<u32> {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn record(&self, code: u32) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(code);
    }
}

pub struct View {
    pub output: Output,
    pub events_file: Option<PathBuf>,
    pub verbose: u8,
    pub quiet: bool,
    pub exit: Exit,
}

impl View {
    pub fn sinks(&self, display: Arc<dyn mix_ui::Display>) -> std::io::Result<Sinks> {
        Ok(Sinks {
            human: (self.output == Output::Human).then(|| Human::new(display).level(self.level())),
            json: self.output == Output::Json,
            file: self
                .events_file
                .as_deref()
                .map(Recorder::create)
                .transpose()?,
            exit: self.exit.clone(),
        })
    }

    pub fn streams(&self) -> bool {
        self.output == Output::Json || self.events_file.is_some()
    }

    pub fn level(&self) -> Detail {
        match (self.quiet, self.verbose) {
            (true, _) => Detail::Outcome,
            (false, 0) => Detail::Step,
            (false, 1) => Detail::Action,
            (false, _) => Detail::Trace,
        }
    }

    pub fn notices(&self, stopping: Stopping) -> Option<Stopping> {
        (self.output == Output::Human).then_some(stopping)
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
        if let Some(envelope::Event::NodeFinished(finished)) = &envelope.event
            && finished.id == ROOT
        {
            self.exit.record(finished.exit_code);
        }
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
    let Ok(mut line) = serde_json::to_vec(envelope) else {
        return;
    };
    line.push(b'\n');
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(&line).and_then(|()| stdout.flush());
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
                &mix_ui::Report {
                    summary: &format!("couldn't record events to {}", self.path.display()),
                    causes: vec![error.to_string()],
                    ..mix_ui::Report::default()
                },
            );
        }
    }
}
