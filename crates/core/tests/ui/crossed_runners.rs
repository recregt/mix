use mix_core::plan::{Closed, Runner, Session, make_guard};
use mix_events::ROOT;

fn finished<'id>(_: &Session<'id, '_>) -> Closed<'id> {
    unimplemented!()
}

fn main() {
    let mut first = Runner::new(ROOT, Vec::new());
    let mut second = Runner::new(ROOT, Vec::new());
    make_guard!(a);
    make_guard!(b);
    let first = first.brand(a);
    let second = second.brand(b);
    let closed = finished(&first);
    second.report(closed);
}
