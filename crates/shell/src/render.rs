use std::sync::{Arc, Mutex, PoisonError};

use mix_events::v1::Envelope;
use mix_events::{NodeId, Outbox};

use crate::drive::Observer;

pub trait Render: Send {
    fn envelope(&mut self, envelope: Envelope);

    fn span(&self, _node: NodeId) -> Option<tracing::Span> {
        None
    }
}

pub struct Quiet;

impl Render for Quiet {
    fn envelope(&mut self, _envelope: Envelope) {}
}

pub type Shared = Arc<Mutex<Box<dyn Render>>>;

pub fn shared(render: impl Render + 'static) -> Shared {
    Arc::new(Mutex::new(Box::new(render)))
}

pub(crate) struct Relay {
    outbox: Arc<Outbox>,
    render: Shared,
}

impl Relay {
    pub(crate) fn new(outbox: Arc<Outbox>, render: Shared) -> Self {
        Self { outbox, render }
    }
}

impl Observer for Relay {
    fn flush(&mut self) {
        let mut render = self.render.lock().unwrap_or_else(PoisonError::into_inner);
        for envelope in self.outbox.drain() {
            render.envelope(envelope);
        }
    }

    fn span(&self, node: NodeId) -> Option<tracing::Span> {
        self.render
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .span(node)
    }
}
