//! Lines that are redrawn in place while a step runs, handed out one per step.

use std::io::IsTerminal;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use indicatif::ProgressBar;
use mix_core::BuildProgress;

use crate::activity::Activity;
use crate::live::LiveLine;
use crate::progress::{board, download_style, failed_step_style, spinner_tick, step_style};

/// Where a command's steps are drawn.
pub trait Display: Send + Sync {
    fn step(&self, title: &str) -> Arc<dyn StepLine>;
}

/// One step's line: what it is doing now, and how it ended.
pub trait StepLine: Send + Sync {
    fn bytes(&self, done: u64, total: Option<u64>);
    fn line(&self, text: &str);
    fn builds(&self, progress: &BuildProgress);
    fn clear(&self);
    fn finish(&self, failed: bool);
}

/// Draws nothing, for a pipe, a CI log or `--no-progress`.
pub struct Silent;

impl Display for Silent {
    fn step(&self, _title: &str) -> Arc<dyn StepLine> {
        Arc::new(Silent)
    }
}

impl StepLine for Silent {
    fn bytes(&self, _done: u64, _total: Option<u64>) {}
    fn line(&self, _text: &str) {}
    fn builds(&self, _progress: &BuildProgress) {}
    fn clear(&self) {}
    fn finish(&self, _failed: bool) {}
}

/// The display for this process: drawn on the terminal when drawing is on and stderr is one,
/// silent otherwise. A step that worked keeps its line only when `keep_passing` is set.
pub fn display(keep_passing: bool) -> Arc<dyn Display> {
    if crate::progress_enabled() && std::io::stderr().is_terminal() {
        Arc::new(Terminal {
            live: LiveLine::start(),
            keep_passing,
            next: AtomicU64::new(0),
        })
    } else {
        Arc::new(Silent)
    }
}

struct Terminal {
    live: Arc<LiveLine>,
    keep_passing: bool,
    next: AtomicU64,
}

impl Display for Terminal {
    fn step(&self, title: &str) -> Arc<dyn StepLine> {
        let bar = board().add(
            ProgressBar::new_spinner()
                .with_style(step_style())
                .with_prefix(title.to_string()),
        );
        bar.enable_steady_tick(spinner_tick());
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.live.opened(id, &bar);
        Arc::new(Bar {
            id,
            activity: Activity::new(Arc::clone(&self.live), bar.clone()),
            bar,
            keep: self.keep_passing,
            live: Arc::clone(&self.live),
            sized: AtomicBool::new(false),
        })
    }
}

struct Bar {
    id: u64,
    bar: ProgressBar,
    keep: bool,
    live: Arc<LiveLine>,
    activity: Activity,
    sized: AtomicBool,
}

impl StepLine for Bar {
    fn bytes(&self, done: u64, total: Option<u64>) {
        if let Some(total) = total
            && !self.sized.swap(true, Ordering::Relaxed)
        {
            self.bar.set_style(download_style());
            self.bar.set_length(total);
        }
        self.bar.set_position(done);
        self.live.received();
    }

    fn line(&self, text: &str) {
        self.activity.line(text);
    }

    fn builds(&self, progress: &BuildProgress) {
        self.activity.progress(progress);
    }

    fn clear(&self) {
        self.activity.clear();
    }

    fn finish(&self, failed: bool) {
        if failed {
            self.bar.set_style(failed_step_style());
        }
        self.live.closed(self.id);
        if self.keep {
            self.bar.finish_with_message("");
        } else {
            self.bar.finish_and_clear();
        }
    }
}
