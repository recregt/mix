use async_trait::async_trait;
use mix_core::{CancellationToken, Plan, Result, Step};

fn main() {
    divan::main();
}

struct NoopStep {
    satisfied: bool,
    fail: bool,
}

#[async_trait]
impl Step for NoopStep {
    type Error = mix_core::Error;

    fn name(&self) -> &'static str {
        "noop"
    }

    async fn check(&self) -> Result<bool> {
        Ok(self.satisfied)
    }

    async fn execute(&mut self, _token: &CancellationToken) -> Result<()> {
        if self.fail {
            Err(mix_core::Error::TaskPanicked(String::new()))
        } else {
            Ok(())
        }
    }
}

fn plan(steps: usize, satisfied: bool) -> Plan<mix_core::Error> {
    Plan::new(
        (0..steps)
            .map(|_| {
                Box::new(NoopStep {
                    satisfied,
                    fail: false,
                }) as Box<dyn Step<Error = mix_core::Error>>
            })
            .collect(),
    )
}

fn failing_plan(steps: usize) -> Plan<mix_core::Error> {
    Plan::new(
        (0..steps)
            .map(|idx| {
                Box::new(NoopStep {
                    satisfied: false,
                    fail: idx + 1 == steps,
                }) as Box<dyn Step<Error = mix_core::Error>>
            })
            .collect(),
    )
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("building a current-thread runtime")
}

#[divan::bench(args = [1, 8, 64])]
fn run_satisfied_steps(bencher: divan::Bencher, steps: usize) {
    let runtime = runtime();
    bencher
        .with_inputs(|| plan(steps, true))
        .bench_local_values(|mut plan| runtime.block_on(plan.run()).unwrap());
}

#[divan::bench(args = [1, 8, 64])]
fn run_executed_steps(bencher: divan::Bencher, steps: usize) {
    let runtime = runtime();
    bencher
        .with_inputs(|| plan(steps, false))
        .bench_local_values(|mut plan| runtime.block_on(plan.run()).unwrap());
}

#[divan::bench(args = [8, 64, 128])]
fn rollback_after_a_failed_step(bencher: divan::Bencher, steps: usize) {
    let runtime = runtime();
    bencher
        .with_inputs(|| failing_plan(steps))
        .bench_local_values(|mut plan| runtime.block_on(plan.run()).unwrap_err());
}
