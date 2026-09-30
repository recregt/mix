use std::fmt::Write as _;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use indicatif::ProgressBar;

use crate::activity::FrameBuffer;

const SECOND: Duration = Duration::from_secs(1);

pub(crate) struct LiveLine {
    line: Mutex<Line>,
    changed: Condvar,
}

pub(crate) struct Line {
    open: Vec<(u64, ProgressBar)>,
    pub(crate) frame: FrameBuffer,
    since: Instant,
    shown: u64,
    message: String,
}

impl LiveLine {
    pub(crate) fn start() -> Arc<Self> {
        let live = Arc::new(Self::new());
        let refresher = Arc::clone(&live);
        let _ = std::thread::Builder::new()
            .name("mix-quiet".to_string())
            .spawn(move || refresher.refresh_forever());
        live
    }

    pub(crate) fn new() -> Self {
        Self {
            line: Mutex::new(Line {
                open: Vec::new(),
                frame: FrameBuffer::new(),
                since: Instant::now(),
                shown: 0,
                message: String::new(),
            }),
            changed: Condvar::new(),
        }
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, Line> {
        self.line.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn refresh_forever(&self) {
        let mut line = self.lock();
        loop {
            line = match line.refresh(Instant::now()) {
                Some(wait) => {
                    self.changed
                        .wait_timeout(line, wait)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
                None => self
                    .changed
                    .wait(line)
                    .unwrap_or_else(PoisonError::into_inner),
            };
        }
    }

    pub(crate) fn opened(&self, id: u64, bar: &ProgressBar) {
        let mut line = self.lock();
        line.open.push((id, bar.clone()));
        line.frame.forget();
        line.restart(Instant::now());
        drop(line);
        self.changed.notify_one();
    }

    pub(crate) fn closed(&self, id: u64) {
        let mut line = self.lock();
        line.open.retain(|(open, _)| *open != id);
        line.frame.forget();
        line.restart(Instant::now());
        drop(line);
        self.changed.notify_one();
    }

    pub(crate) fn received(&self) {
        let mut line = self.lock();
        if line.shown > 0 {
            let Line { open, frame, .. } = &*line;
            if let Some((_, bar)) = open.last() {
                bar.set_message(frame.drawn().to_string());
            }
        }
        line.restart(Instant::now());
    }
}

impl Line {
    pub(crate) fn restart(&mut self, now: Instant) {
        self.since = now;
        self.shown = 0;
    }

    fn refresh(&mut self, now: Instant) -> Option<Duration> {
        let (_, bar) = self.open.last()?;
        let quiet = now.saturating_duration_since(self.since);
        let seconds = quiet.as_secs();
        if seconds > self.shown {
            self.shown = seconds;
            self.message.clear();
            self.message.push_str(self.frame.drawn());
            write_quiet(&mut self.message, seconds);
            bar.set_message(self.message.clone());
        }
        Some((SECOND * (self.shown as u32 + 1)).saturating_sub(quiet))
    }
}

fn write_quiet(out: &mut String, seconds: u64) {
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    out.push_str(" \u{1b}[2m");
    let _ = if hours > 0 {
        write!(out, "(no output for {hours}h {minutes:02}m)")
    } else if minutes > 0 {
        write!(out, "(no output for {minutes}m {seconds:02}s)")
    } else {
        write!(out, "(no output for {seconds}s)")
    };
    out.push_str("\u{1b}[0m");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quiet(seconds: u64) -> String {
        let mut out = String::new();
        write_quiet(&mut out, seconds);
        out
    }

    #[test]
    fn a_quiet_time_reads_in_the_largest_units_that_matter() {
        assert_eq!(quiet(2), " \u{1b}[2m(no output for 2s)\u{1b}[0m");
        assert!(quiet(65).contains("(no output for 1m 05s)"));
        assert!(quiet(3 * 3600 + 7 * 60 + 9).contains("(no output for 3h 07m)"));
    }

    #[test]
    fn nothing_is_refreshed_while_no_step_is_open() {
        let live = LiveLine::new();
        assert_eq!(live.lock().refresh(Instant::now()), None);
    }

    #[test]
    fn a_step_is_refreshed_at_each_whole_second_of_quiet() {
        let live = LiveLine::new();
        live.opened(0, &ProgressBar::hidden());
        let started = live.lock().since;

        let mut line = live.lock();
        assert_eq!(
            line.refresh(started + Duration::from_millis(400)),
            Some(Duration::from_millis(600))
        );
        assert_eq!(line.shown, 0);
        assert_eq!(
            line.refresh(started + Duration::from_millis(2300)),
            Some(Duration::from_millis(700))
        );
        assert_eq!(line.shown, 2);
        assert!(line.message.contains("(no output for 2s)"));
    }

    #[test]
    fn output_starts_the_quiet_time_again() {
        let live = LiveLine::new();
        live.opened(0, &ProgressBar::hidden());
        let started = live.lock().since;
        live.lock().refresh(started + Duration::from_secs(3));

        live.received();

        let mut line = live.lock();
        assert_eq!(line.shown, 0);
        let since = line.since;
        assert_eq!(
            line.refresh(since + Duration::from_millis(100)),
            Some(Duration::from_millis(900))
        );
    }

    #[test]
    fn closing_the_last_step_stops_the_refresh() {
        let live = LiveLine::new();
        live.opened(0, &ProgressBar::hidden());
        live.closed(0);
        assert_eq!(live.lock().refresh(Instant::now()), None);
    }
}
