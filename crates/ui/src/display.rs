use std::io::IsTerminal;
use std::sync::{Arc, Mutex, PoisonError};

use indicatif::ProgressBar;
use mix_events::v1::Builds;

use crate::Status;
use crate::progress::{ELAPSED_TICK, bar_style, bar_width, board, columns, live_style};

pub trait Display: Send + Sync {
    fn step(&self, status: Status, subject: &str) -> Arc<dyn StepLine>;
    fn live(&self) -> bool;
}

pub trait StepLine: Send + Sync {
    fn bytes(&self, done: u64, total: Option<u64>);
    fn builds(&self, progress: &Builds);
    fn item(&self, name: &str);
    fn commit(&self);
    fn finish(&self);
}

pub struct Silent;

impl Display for Silent {
    fn step(&self, _status: Status, _subject: &str) -> Arc<dyn StepLine> {
        Arc::new(Silent)
    }

    fn live(&self) -> bool {
        false
    }
}

impl StepLine for Silent {
    fn bytes(&self, _done: u64, _total: Option<u64>) {}
    fn builds(&self, _progress: &Builds) {}
    fn item(&self, _name: &str) {}
    fn commit(&self) {}
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
            status,
            subject: subject.to_string(),
            drawn: Mutex::new(Drawn {
                bar: Some(bar),
                showing: Showing::Working,
                committed: false,
            }),
            item: Mutex::new(String::new()),
            frame: Mutex::new(String::new()),
            last: Mutex::new(None),
        })
    }

    fn live(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Showing {
    Working,
    Bar(usize),
}

struct Drawn {
    bar: Option<ProgressBar>,
    showing: Showing,
    committed: bool,
}

struct Live {
    status: Status,
    subject: String,
    drawn: Mutex<Drawn>,
    item: Mutex<String>,
    frame: Mutex<String>,
    last: Mutex<Option<Builds>>,
}

impl Live {
    fn committed(drawn: &mut Drawn) {
        drawn.committed = true;
        if drawn.showing == Showing::Working
            && let Some(bar) = drawn.bar.take()
        {
            bar.finish_and_clear();
        }
    }

    fn draw(&self, prefix: &'static str, message: &str, at: Option<(u64, u64)>) {
        let wanted = match at.and(bar_width(columns())) {
            Some(width) => Showing::Bar(width),
            None => Showing::Working,
        };
        let mut drawn = self.drawn.lock().unwrap_or_else(PoisonError::into_inner);
        if wanted == Showing::Working && drawn.committed {
            return;
        }
        let fresh = drawn.bar.is_none();
        let bar = drawn
            .bar
            .get_or_insert_with(|| board().add(ProgressBar::new_spinner()))
            .clone();
        if fresh || drawn.showing != wanted {
            drawn.showing = wanted;
            bar.set_style(match wanted {
                Showing::Working => live_style(),
                Showing::Bar(width) => bar_style(width),
            });
        }
        bar.set_prefix(prefix);
        bar.set_message(message.to_string());
        if let Some((done, total)) = at {
            bar.set_length(total);
            bar.set_position(done);
        }
    }
}

impl StepLine for Live {
    fn bytes(&self, done: u64, total: Option<u64>) {
        if let Some(total) = total {
            let text = crate::activity::render_bytes(done, total);
            self.draw(self.status.text(), &text, Some((done, total)));
        }
    }

    fn builds(&self, progress: &Builds) {
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

    fn commit(&self) {
        Self::committed(&mut self.drawn.lock().unwrap_or_else(PoisonError::into_inner));
    }

    fn finish(&self) {
        let mut drawn = self.drawn.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(bar) = drawn.bar.take() {
            bar.finish_and_clear();
        }
    }
}
