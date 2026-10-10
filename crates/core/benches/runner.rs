use std::borrow::Cow;
use std::sync::{Arc, LazyLock};

use mix_core::effect::{Action, Fact, Failure, Performed, Query};
use mix_core::run::{Closed, Input, Next, Runner, Session, StepSpec, Verdict, make_guard};
use mix_events::v1::Command;
use mix_events::{Outbox, ROOT, Start, Tree};

fn main() {
    divan::main();
}

static KEYS: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    (0..128)
        .map(|index| &*Box::leak(format!("step-{index}").into_boxed_str()))
        .collect()
});

struct Noop {
    key: &'static str,
    acts: bool,
}

impl StepSpec for Noop {
    fn key(&self) -> Cow<'static, str> {
        self.key.into()
    }

    fn title(&self) -> mix_core::run::Title {
        mix_core::run::Title::new(mix_events::v1::Verb::Creating, "do nothing")
    }

    fn queries(&self) -> Vec<Query> {
        Vec::new()
    }

    fn actions(&self, _facts: &[Fact]) -> Result<Vec<Action>, Failure> {
        Ok(if self.acts {
            vec![Action::DaemonReload]
        } else {
            Vec::new()
        })
    }
}

fn runner(steps: usize, acts: bool) -> (Runner, Tree, Arc<Outbox>) {
    let outbox = Arc::new(Outbox::new("bench", || {}));
    let tree = Tree::new(
        Arc::clone(&outbox),
        Arc::new(|| None),
        Start::command("bench", Command::default()),
    );
    let steps = (0..steps)
        .map(|index| {
            Box::new(Noop {
                key: KEYS[index],
                acts,
            }) as Box<dyn StepSpec>
        })
        .collect();
    (Runner::new(ROOT, steps), tree, outbox)
}

fn drive<'id>(
    runner: &mut Session<'id, '_>,
    tree: &mut Tree,
    outbox: &Outbox,
    fail_at: Option<usize>,
) -> Closed<'id> {
    let mut input = None;
    let mut performed = 0;
    loop {
        let next = runner.step(tree, input.take());
        divan::black_box(outbox.drain());
        match next {
            Next::Observe(_) => input = Some(Input::Facts(Ok(Vec::new()))),
            Next::Perform(_) => {
                let outcome = if Some(performed) == fail_at {
                    Err(Failure::Conflict {
                        subject: "bench".into(),
                        expected: "nothing".into(),
                        found: "something".into(),
                    })
                } else {
                    Ok(Performed {
                        undo: vec![Action::DaemonReload],
                    })
                };
                performed += 1;
                input = Some(Input::Done(outcome));
            }
            Next::Finished(closed) => return closed,
        }
    }
}

#[divan::bench(args = [1, 8, 64])]
fn satisfied_steps(bencher: divan::Bencher, steps: usize) {
    bencher
        .with_inputs(|| runner(steps, false))
        .bench_local_values(|(mut runner, mut tree, outbox)| {
            make_guard!(guard);
            let mut runner = runner.brand(guard);
            let closed = drive(&mut runner, &mut tree, &outbox, None);
            assert_eq!(runner.report(closed).verdict, Verdict::Succeeded);
        });
}

#[divan::bench(args = [1, 8, 64])]
fn performed_steps(bencher: divan::Bencher, steps: usize) {
    bencher
        .with_inputs(|| runner(steps, true))
        .bench_local_values(|(mut runner, mut tree, outbox)| {
            make_guard!(guard);
            let mut runner = runner.brand(guard);
            let closed = drive(&mut runner, &mut tree, &outbox, None);
            assert_eq!(runner.report(closed).verdict, Verdict::Succeeded);
        });
}

#[divan::bench(args = [8, 64, 128])]
fn rollback_after_the_last_step_fails(bencher: divan::Bencher, steps: usize) {
    bencher
        .with_inputs(|| runner(steps, true))
        .bench_local_values(|(mut runner, mut tree, outbox)| {
            make_guard!(guard);
            let mut runner = runner.brand(guard);
            let closed = drive(&mut runner, &mut tree, &outbox, Some(steps - 1));
            assert!(matches!(
                runner.report(closed).verdict,
                Verdict::Failed { .. }
            ));
        });
}
