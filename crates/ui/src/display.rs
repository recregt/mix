use std::io::IsTerminal;
use std::sync::{Arc, Mutex, PoisonError};

use indicatif::ProgressBar;
use mix_core::BuildProgress;

use crate::Status;
use crate::progress::{ELAPSED_TICK, board, counting_style, live_style, loading_style};

pub trait Display: Send + Sync {
    fn step(&self, status: Status, subject: &str) -> Arc<dyn StepLine>;
}

pub trait StepLine: Send + Sync {
    fn bytes(&self, done: u64, total: Option<u64>);
    fn builds(&self, progress: &BuildProgress);
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
            subject: subject.to_string(),
            showing: Mutex::new(Showing::Working),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Showing {
    Working,
    Counting,
    Loading,
}

struct Live {
    bar: ProgressBar,
    subject: String,
    showing: Mutex<Showing>,
}

impl Live {
    fn show(&self, wanted: Showing) {
        let mut showing = self.showing.lock().unwrap_or_else(PoisonError::into_inner);
        if *showing == wanted {
            return;
        }
        *showing = wanted;
        self.bar.set_style(match wanted {
            Showing::Working => live_style(),
            Showing::Counting => counting_style(),
            Showing::Loading => loading_style(),
        });
    }
}

impl StepLine for Live {
    fn bytes(&self, done: u64, total: Option<u64>) {
        if let Some(total) = total {
            self.show(Showing::Loading);
            self.bar.set_length(total);
            self.bar.set_position(done);
        }
    }

    fn builds(&self, progress: &BuildProgress) {
        let (done, expected) = if progress.builds_expected > 0 {
            (progress.builds_done, progress.builds_expected)
        } else {
            (progress.downloads_done, progress.downloads_expected)
        };
        if expected == 0 {
            self.show(Showing::Working);
            self.bar.set_message(self.subject.clone());
            return;
        }
        self.show(Showing::Counting);
        self.bar
            .set_message(crate::activity::render_progress(progress));
        self.bar.set_length(expected);
        self.bar.set_position(done);
    }

    fn finish(&self) {
        self.bar.finish_and_clear();
    }
}
