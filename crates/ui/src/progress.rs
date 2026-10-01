use std::sync::OnceLock;
use std::time::Duration;

use indicatif::{MultiProgress, ProgressDrawTarget, ProgressStyle};

const WORKING: &str = "{prefix:>12.green.bright.bold} {msg} ({elapsed})";

const MAX_PRINT: usize = 50;

const HEADER: usize = 15;

const BAR: &str = "=> ";

const TERM_DRAW_HZ: u8 = 25;

pub(crate) const ELAPSED_TICK: Duration = Duration::from_secs(1);

static BOARD: OnceLock<MultiProgress> = OnceLock::new();

pub fn init(progress: bool) {
    crate::set_progress_enabled(progress);
    let _ = BOARD.set(MultiProgress::with_draw_target(if progress {
        ProgressDrawTarget::stderr_with_hz(TERM_DRAW_HZ)
    } else {
        ProgressDrawTarget::hidden()
    }));
}

pub(crate) fn board() -> &'static MultiProgress {
    BOARD.get_or_init(|| MultiProgress::with_draw_target(ProgressDrawTarget::hidden()))
}

fn style(template: &str) -> ProgressStyle {
    ProgressStyle::with_template(template)
        .expect("a live line template is valid")
        .progress_chars(BAR)
}

pub fn live_style() -> ProgressStyle {
    style(WORKING)
}

pub(crate) fn bar_width(columns: usize) -> Option<usize> {
    columns
        .min(MAX_PRINT)
        .checked_sub(HEADER + 2)
        .filter(|width| *width > 0)
}

pub(crate) fn bar_style(width: usize) -> ProgressStyle {
    style(&format!(
        "{{prefix:>12.green.bright.bold}} [{{bar:{width}}}] {{wide_msg}}"
    ))
}

pub(crate) fn columns() -> usize {
    console::Term::stderr()
        .size_checked()
        .map_or(0, |(_, columns)| usize::from(columns))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use indicatif::{ProgressBar, TermLike};

    use super::*;

    #[derive(Clone, Debug)]
    struct RecordingTerm(Arc<Mutex<Vec<String>>>);

    impl TermLike for RecordingTerm {
        fn width(&self) -> u16 {
            80
        }

        fn move_cursor_up(&self, _n: usize) -> std::io::Result<()> {
            Ok(())
        }

        fn move_cursor_down(&self, _n: usize) -> std::io::Result<()> {
            Ok(())
        }

        fn move_cursor_right(&self, _n: usize) -> std::io::Result<()> {
            Ok(())
        }

        fn move_cursor_left(&self, _n: usize) -> std::io::Result<()> {
            Ok(())
        }

        fn write_line(&self, s: &str) -> std::io::Result<()> {
            self.0.lock().unwrap().push(s.to_string());
            Ok(())
        }

        fn write_str(&self, s: &str) -> std::io::Result<()> {
            self.0.lock().unwrap().push(s.to_string());
            Ok(())
        }

        fn clear_line(&self) -> std::io::Result<()> {
            Ok(())
        }

        fn flush(&self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn drawn(bar: ProgressBar, recorded: &Arc<Mutex<Vec<String>>>) -> String {
        bar.tick();
        let lines = recorded.lock().unwrap();
        let last = lines
            .iter()
            .rev()
            .find(|line| !line.trim().is_empty())
            .cloned()
            .unwrap_or_default();
        console::strip_ansi_codes(&last).into_owned()
    }

    fn bar(style: ProgressStyle, length: Option<u64>) -> (ProgressBar, Arc<Mutex<Vec<String>>>) {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let bar = ProgressBar::with_draw_target(
            length,
            ProgressDrawTarget::term_like(Box::new(RecordingTerm(Arc::clone(&recorded)))),
        )
        .with_style(style);
        (bar, recorded)
    }

    #[test]
    fn a_running_step_shows_its_status_in_the_gutter() {
        let (bar, recorded) = bar(live_style(), None);
        let bar = bar.with_prefix("Activating").with_message("profile");
        let line = drawn(bar, &recorded);
        assert!(line.starts_with("  Activating profile ("), "{line:?}");
    }

    #[test]
    fn a_bar_keeps_its_width_whatever_text_follows_it() {
        let width = bar_width(80).unwrap();
        let mut bars = Vec::new();
        for message in ["0/3", "2/3, 60.9/354.1 KiB: hello-2.12.3"] {
            let (bar, recorded) = bar(bar_style(width), Some(3));
            let bar = bar.with_prefix("Fetching").with_message(message);
            bar.set_position(1);
            let line = drawn(bar, &recorded);
            assert!(line.starts_with("    Fetching ["), "{line:?}");
            let end = line.find(']').unwrap();
            assert!(message.starts_with(&line[end + 2..]), "{line:?}");
            assert!(line.is_ascii(), "{line:?}");
            assert!(console::measure_text_width(&line) <= 80, "{line:?}");
            bars.push(end);
        }
        assert_eq!(bars[0], bars[1]);
    }

    #[test]
    fn the_bar_takes_cargo_s_share_of_the_line_and_none_when_there_is_no_room() {
        assert_eq!(bar_width(200), bar_width(80));
        assert_eq!(bar_width(40), Some(23));
        assert_eq!(bar_width(17), None);
    }
}
