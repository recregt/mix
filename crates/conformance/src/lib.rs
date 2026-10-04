pub mod model;
pub mod observation;
pub mod suite;

use std::future::Future;

use mix_core::identity::InvokingUser;
use mix_core::testkit::Breakage;
use mix_events::v1::command::Request;
use serde_json::Value;

pub use observation::Observation;
pub use suite::FaultKind;

pub enum Faulted {
    Unsupported,
    Crashed,
    Ended(Value),
}

pub trait Backend {
    fn user(&self) -> &InvokingUser;

    fn run(
        &mut self,
        request: Request,
        dry_run: bool,
        as_root: bool,
    ) -> impl Future<Output = Value>;

    fn faulted(
        &mut self,
        request: Request,
        as_root: bool,
        at: usize,
        kind: FaultKind,
    ) -> impl Future<Output = Faulted>;

    fn damage(&mut self, breakage: &Breakage) -> impl Future<Output = bool>;

    fn observe(&mut self) -> impl Future<Output = Observation>;
}
