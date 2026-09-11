//! Benchmarks for the step orchestration engine: `Plan::run` drives every
//! bootstrap and doctor invocation.

use async_trait::async_trait;
use mix_core::{Plan, Result, Step};

fn main() {
    divan::main();
}

/// A step with no side effects, so the measurement covers the engine only.
struct NoopStep {
    satisfied: bool,
}

#[async_trait]
impl Step for NoopStep {
    fn name(&self) -> &'static str {
        "noop"
    }

    async fn check(&self) -> Result<bool> {
        Ok(self.satisfied)
    }

    async fn execute(&mut self) -> Result<()> {
        Ok(())
    }
}

fn plan(steps: usize, satisfied: bool) -> Plan {
    Plan::new(
        (0..steps)
            .map(|_| Box::new(NoopStep { satisfied }) as Box<dyn Step>)
            .collect(),
    )
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("building a current-thread runtime")
}

/// The doctor path: every step reports that it is already satisfied.
#[divan::bench(args = [1, 8, 64])]
fn run_satisfied_steps(bencher: divan::Bencher, steps: usize) {
    let runtime = runtime();
    bencher
        .with_inputs(|| plan(steps, true))
        .bench_local_values(|mut plan| runtime.block_on(plan.run()).unwrap());
}

/// The bootstrap path: every step needs to be checked and then executed.
#[divan::bench(args = [1, 8, 64])]
fn run_executed_steps(bencher: divan::Bencher, steps: usize) {
    let runtime = runtime();
    bencher
        .with_inputs(|| plan(steps, false))
        .bench_local_values(|mut plan| runtime.block_on(plan.run()).unwrap());
}
