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
}

pub struct NoopStepObserver;

impl StepObserver for NoopStepObserver {
    fn on_step_span(&self, _span: &tracing::Span) {}
}
