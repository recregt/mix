use std::sync::Arc;

use mix_core::plan::{Next, Runner, Verdict, make_guard};
use mix_events::v1::Command;
use mix_events::{Outbox, ROOT, Start, Tree};

fn main() {
    let mut tree = Tree::new(
        Arc::new(Outbox::new("request", || {})),
        Arc::new(|| None),
        Start::command("bootstrap", Command::default()),
    );
    let mut runner = Runner::new(ROOT, Vec::new());
    make_guard!(guard);
    let mut session = runner.brand(guard);
    let Next::Finished(closed) = session.step(&mut tree, None) else {
        panic!("an empty plan finishes at once");
    };
    assert_eq!(session.report(closed).verdict, Verdict::Succeeded);
}
