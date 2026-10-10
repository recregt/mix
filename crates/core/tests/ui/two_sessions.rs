use mix_core::run::{Runner, make_guard};
use mix_events::ROOT;
use mix_events::v1::Cancellation;

fn main() {
    let mut runner = Runner::new(ROOT, Vec::new());
    make_guard!(a);
    make_guard!(b);
    let mut first = runner.brand(a);
    let second = runner.brand(b);
    first.stop(Cancellation::Interrupted);
    drop(second);
}
