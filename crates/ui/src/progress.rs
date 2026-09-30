use std::sync::OnceLock;
use std::time::Duration;

use indicatif::{MultiProgress, ProgressDrawTarget, ProgressStyle};

const WORKING: &str = "{prefix:>12.green.bold} {msg} ({elapsed})";

const COUNTING: &str = "{prefix:>12.green.bold} [{wide_bar}] {msg}";

const LOADING: &str = "{prefix:>12.green.bold} [{wide_bar}] {bytes}/{total_bytes}";

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

pub(crate) fn counting_style() -> ProgressStyle {
    style(COUNTING)
}

pub(crate) fn loading_style() -> ProgressStyle {
    style(LOADING)
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
    fn a_count_is_a_bar_of_plain_ascii_that_fits_the_terminal() {
        let (bar, recorded) = bar(counting_style(), Some(17));
        let bar = bar
            .with_prefix("Activating")
            .with_message("building 3/17, downloading 12/37");
        bar.set_position(3);
        let line = drawn(bar, &recorded);
        assert!(line.starts_with("  Activating ["), "{line:?}");
        assert!(
            line.ends_with("] building 3/17, downloading 12/37"),
            "{line:?}"
        );
        assert!(line.is_ascii(), "{line:?}");
        assert!(console::measure_text_width(&line) <= 80, "{line:?}");
    }

    #[test]
    fn a_download_counts_bytes() {
        let (bar, recorded) = bar(loading_style(), Some(91 * 1024 * 1024));
        let bar = bar.with_prefix("Installing");
        bar.set_position(48 * 1024 * 1024);
        let line = drawn(bar, &recorded);
        assert!(line.contains("48.00 MiB/91.00 MiB"), "{line:?}");
        assert!(console::measure_text_width(&line) <= 80, "{line:?}");
    }
}
