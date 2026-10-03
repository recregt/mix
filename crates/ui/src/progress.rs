use std::sync::OnceLock;
use std::time::Duration;

use indicatif::{MultiProgress, ProgressDrawTarget, ProgressStyle, TermLike};

const WORKING: &str = "{prefix:>12.green.bright.bold} {msg} ({elapsed})";

const MAX_PRINT: usize = 50;

const HEADER: usize = 15;

const BAR: &str = "=> ";

const TERM_DRAW_HZ: u8 = 25;

pub(crate) const ELAPSED_TICK: Duration = Duration::from_secs(1);

static BOARD: OnceLock<MultiProgress> = OnceLock::new();

const CONTROL_ECHO: &str = "^C";

#[derive(Debug)]
struct EchoRoom<T>(T);

impl<T: TermLike> TermLike for EchoRoom<T> {
    fn width(&self) -> u16 {
        self.0.width().saturating_sub(CONTROL_ECHO.len() as u16)
    }

    fn height(&self) -> u16 {
        self.0.height()
    }

    fn move_cursor_up(&self, n: usize) -> std::io::Result<()> {
        self.0.move_cursor_up(n)
    }

    fn move_cursor_down(&self, n: usize) -> std::io::Result<()> {
        self.0.move_cursor_down(n)
    }

    fn move_cursor_right(&self, n: usize) -> std::io::Result<()> {
        self.0.move_cursor_right(n)
    }

    fn move_cursor_left(&self, n: usize) -> std::io::Result<()> {
        self.0.move_cursor_left(n)
    }

    fn write_line(&self, s: &str) -> std::io::Result<()> {
        self.0.write_line(s)
    }

    fn write_str(&self, s: &str) -> std::io::Result<()> {
        self.0.write_str(s)
    }

    fn clear_line(&self) -> std::io::Result<()> {
        self.0.clear_line()
    }

    fn flush(&self) -> std::io::Result<()> {
        self.0.flush()
    }
}

pub fn init(progress: bool) {
    let stderr = console::Term::buffered_stderr();
    let progress = progress && stderr.is_term();
    crate::set_progress_enabled(progress);
    let _ = BOARD.set(MultiProgress::with_draw_target(if progress {
        ProgressDrawTarget::term_like_with_hz(Box::new(EchoRoom(stderr)), TERM_DRAW_HZ)
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
    fn a_drawn_line_leaves_room_for_an_echoed_control_character() {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let term = RecordingTerm(Arc::clone(&recorded));
        let columns = usize::from(term.width());
        let bar = ProgressBar::with_draw_target(
            None,
            ProgressDrawTarget::term_like(Box::new(EchoRoom(term))),
        )
        .with_style(live_style())
        .with_prefix("Activating")
        .with_message("profile");

        bar.tick();

        let written: String = recorded.lock().unwrap().concat();
        let line = console::strip_ansi_codes(written.trim_start_matches('\r'));
        assert!(line.starts_with("  Activating profile ("), "{line:?}");
        assert_eq!(
            console::measure_text_width(&line) + CONTROL_ECHO.len(),
            columns,
            "{line:?}"
        );
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
