use mix_nixlog::BuildProgress;

pub trait DownloadProgress: Send + Sync {
    fn fetching(&self, url: &str);
    fn set_total(&self, total: u64);
    fn add(&self, delta: u64);
}

pub struct NoopProgress;

impl DownloadProgress for NoopProgress {
    fn fetching(&self, _url: &str) {}
    fn set_total(&self, _total: u64) {}
    fn add(&self, _delta: u64) {}
}

/// Receives a long-running child process's output as it is produced, one display line at a time.
///
/// A build can emit thousands of lines a second, so `line` is on a hot path: implementations are
/// expected to drop what they cannot draw rather than queue it, and to do no work at all when
/// nothing is watching.
pub trait ActivityReporter: Send + Sync {
    fn line(&self, line: &str);

    /// Reports what the process is doing as counters rather than as text. Called as often as
    /// `line`, and for the same reason expected to drop frames rather than queue them.
    fn progress(&self, progress: &BuildProgress);

    /// Called once the process has exited, to drop whatever the last line left on screen.
    fn clear(&self);

    fn build_started(&self, _derivation: &str) {}

    fn fetch_started(&self, _path: &str) {}

    /// Called when the process waits for `lock`, which `holder` running `command` holds.
    fn waiting(&self, _lock: &str, _holder: Option<&str>, _command: Option<&str>) {}
}

pub struct NoopActivity;

impl ActivityReporter for NoopActivity {
    fn line(&self, _line: &str) {}
    fn progress(&self, _progress: &BuildProgress) {}
    fn clear(&self) {}
}
