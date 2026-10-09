#![allow(unreachable_code)]

use mix_core::run::{Closed, Runner, make_guard};
use mix_events::ROOT;

fn main() {
    let mut runner = Runner::new(ROOT, Vec::new());
    make_guard!(guard);
    let session = runner.brand(guard);
    session.report(Closed(unimplemented!()));
}
