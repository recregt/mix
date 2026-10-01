use std::io::IsTerminal;
use std::sync::{Arc, Mutex, PoisonError};

use indicatif::ProgressBar;
use mix_core::BuildProgress;

use crate::Status;
use crate::progress::{ELAPSED_TICK, bar_style, bar_width, board, columns, live_style};

pub trait Display: Send + Sync {
    fn step(&self, status: Status, subject: &str) -> Arc<dyn StepLine>;
}

pub trait StepLine: Send + Sync {
    fn bytes(&self, done: u64, total: Option<u64>);
    fn builds(&self, progress: &BuildProgress);
    fn item(&self, name: &str);
    fn finish(&self);
}

pub struct Silent;

impl Display for Silent {
    fn step(&self, _status: Status, _subject: &str) -> Arc<dyn StepLine> {
        Arc::new(Silent)
    }
}

impl StepLine for Silent {
    fn bytes(&self, _done: u64, _total: Option<u64>) {}
    fn builds(&self, _progress: &BuildProgress) {}
    fn item(&self, _name: &str) {}
    fn finish(&self) {}
}

pub fn display() -> Arc<dyn Display> {
    if crate::progress_enabled() && std::io::stderr().is_terminal() {
        Arc::new(Terminal)
    } else {
        Arc::new(Silent)
    }
}

struct Terminal;

impl Display for Terminal {
    fn step(&self, status: Status, subject: &str) -> Arc<dyn StepLine> {
        let bar = board().add(
            ProgressBar::new_spinner()
                .with_style(live_style())
                .with_prefix(status.text())
                .with_message(subject.to_string()),
        );
        bar.enable_steady_tick(ELAPSED_TICK);
        Arc::new(Live {
            bar,
            status,
            subject: subject.to_string(),
            showing: Mutex::new(Showing::Working),
            item: Mutex::new(String::new()),
            frame: Mutex::new(String::new()),
            last: Mutex::new(None),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Showing {
    Working,
    Bar(usize),
}

struct Live {
    bar: ProgressBar,
    status: Status,
    subject: String,
    showing: Mutex<Showing>,
    item: Mutex<String>,
    frame: Mutex<String>,
    last: Mutex<Option<BuildProgress>>,
}

impl Live {
    fn draw(&self, prefix: &'static str, message: &str, at: Option<(u64, u64)>) {
        let wanted = match at.and(bar_width(columns())) {
            Some(width) => Showing::Bar(width),
            None => Showing::Working,
        };
        let set = || {
            self.bar.set_prefix(prefix);
            self.bar.set_message(message.to_string());
            if let Some((done, total)) = at {
                self.bar.set_length(total);
                self.bar.set_position(done);
            }
        };
        let mut showing = self.showing.lock().unwrap_or_else(PoisonError::into_inner);
        if *showing != wanted {
            *showing = wanted;
            self.bar.set_style(match wanted {
                Showing::Working => live_style(),
                Showing::Bar(width) => bar_style(width),
            });
        }
        set();
    }
}

impl StepLine for Live {
    fn bytes(&self, done: u64, total: Option<u64>) {
        if let Some(total) = total {
            let text = crate::activity::render_bytes(done, total);
            self.draw(self.status.text(), &text, Some((done, total)));
        }
    }

    fn builds(&self, progress: &BuildProgress) {
        *self.last.lock().unwrap_or_else(PoisonError::into_inner) = Some(*progress);
        let Some((phase, done, expected)) = crate::activity::phase(progress) else {
            self.draw(self.status.text(), &self.subject, None);
            return;
        };
        let mut frame = self.frame.lock().unwrap_or_else(PoisonError::into_inner);
        frame.clear();
        let item = self.item.lock().unwrap_or_else(PoisonError::into_inner);
        crate::activity::write_progress(&mut frame, progress, &item);
        let prefix = match phase {
            crate::activity::Phase::Fetching => Status::Fetching.text(),
            crate::activity::Phase::Building => Status::Building.text(),
        };
        self.draw(prefix, &frame, Some((done, expected)));
    }

    fn item(&self, name: &str) {
        {
            let mut item = self.item.lock().unwrap_or_else(PoisonError::into_inner);
            item.clear();
            item.push_str(name);
        }
        let last = *self.last.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(progress) = last {
            self.builds(&progress);
        }
    }

    fn finish(&self) {
        self.bar.finish_and_clear();
    }
}
