use std::sync::{Arc, Mutex, PoisonError};

use mix_events::Detail;
use mix_events::v1::Envelope;

use crate::drive::Observer;

pub trait Render: Send {
    fn envelope(&mut self, envelope: Envelope);

    fn detail(&self) -> Detail;
}

pub struct Quiet;

impl Render for Quiet {
    fn envelope(&mut self, _envelope: Envelope) {}

    fn detail(&self) -> Detail {
        Detail::Outcome
    }
}

pub type Shared = Arc<Mutex<Box<dyn Render>>>;

pub fn shared(render: impl Render + 'static) -> Shared {
    Arc::new(Mutex::new(Box::new(render)))
}

pub(crate) struct Relay {
    render: Shared,
}

impl Relay {
    pub(crate) fn new(render: Shared) -> Self {
        Self { render }
    }
}

impl Observer for Relay {
    fn wants(&self, detail: Detail) -> bool {
        self.render
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .detail()
            >= detail
    }
}
