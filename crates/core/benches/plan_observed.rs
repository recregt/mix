// Separate binary on purpose: installing a subscriber raises tracing's global max level for the
// rest of the process, which would push every other plan benchmark onto the recording path too.
use std::sync::Arc;

use async_trait::async_trait;
use mix_core::{CancellationToken, Plan, Result, Step, StepObserver};

fn main() {
    divan::main();
}

struct NoopStep;

#[async_trait]
impl Step for NoopStep {
    type Error = mix_core::Error;

    fn name(&self) -> &'static str {
        "noop"
    }

    async fn check(&self) -> Result<bool> {
        Ok(false)
    }

    async fn execute(&mut self, _token: &CancellationToken) -> Result<()> {
        Ok(())
    }
}

struct NoopObserver;

impl StepObserver for NoopObserver {
    fn on_step_span(&self, _span: &tracing::Span) {}
}

struct EnablingSubscriber;

impl tracing::Subscriber for EnablingSubscriber {
    fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _attributes: &tracing::span::Attributes<'_>) -> tracing::Id {
        tracing::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::Id, _values: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _span: &tracing::Id, _follows: &tracing::Id) {}
    fn event(&self, _event: &tracing::Event<'_>) {}
    fn enter(&self, _span: &tracing::Id) {}
    fn exit(&self, _span: &tracing::Id) {}
}

fn plan(steps: usize) -> Plan<mix_core::Error> {
    Plan::new(
        (0..steps)
            .map(|_| Box::new(NoopStep) as Box<dyn Step<Error = mix_core::Error>>)
            .collect(),
    )
    .with_step_observer(Arc::new(NoopObserver))
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("building a current-thread runtime")
}

#[divan::bench(args = [1, 8, 64])]
fn run_observed_steps(bencher: divan::Bencher, steps: usize) {
    let runtime = runtime();
    let _guard = tracing::subscriber::set_default(EnablingSubscriber);
    bencher
        .with_inputs(|| plan(steps))
        .bench_local_values(|mut plan| runtime.block_on(plan.run()).unwrap());
}
