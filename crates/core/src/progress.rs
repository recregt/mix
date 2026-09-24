use crate::nix_log::BuildProgress;

pub trait DownloadProgress: Send + Sync {
    fn set_total(&self, total: u64);
    fn add(&self, delta: u64);
}

pub struct NoopProgress;

impl DownloadProgress for NoopProgress {
    fn set_total(&self, _total: u64) {}
    fn add(&self, _delta: u64) {}
}

/// Lets a presentation layer configure a step's tracing span the moment it's created, e.g. to
/// keep a fast-finishing step's indicator visible instead of letting it flash and disappear.
pub trait StepObserver: Send + Sync {
    fn on_step_span(&self, span: &tracing::Span);

    /// Called before the span of a step that failed is closed, so a presentation layer can mark
    /// the line it is about to keep on screen as a failure rather than as a success.
    fn on_step_failed(&self, _span: &tracing::Span) {}
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
}

pub struct NoopActivity;

impl ActivityReporter for NoopActivity {
    fn line(&self, _line: &str) {}
    fn progress(&self, _progress: &BuildProgress) {}
    fn clear(&self) {}
}
