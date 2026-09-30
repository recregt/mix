use std::sync::{Arc, Mutex, PoisonError};

use mix_events::v1::Envelope;
use mix_events::{NodeId, Outbox};
use tokio::sync::Notify;

use crate::drive::Observer;

pub trait Render: Send {
    fn envelope(&mut self, envelope: Envelope);

    fn span(&self, _node: NodeId) -> Option<tracing::Span> {
        None
    }

    fn logs(&self) -> Option<tracing::Level> {
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
    woken: Arc<Notify>,
}

impl Relay {
    pub(crate) fn new(outbox: Arc<Outbox>, render: Shared, woken: Arc<Notify>) -> Self {
        Self {
            outbox,
            render,
            woken,
        }
    }
}

impl Observer for Relay {
    fn woken(&self) -> Option<Arc<Notify>> {
        Some(Arc::clone(&self.woken))
    }

    fn flush(&mut self) {
        let mut render = self.render.lock().unwrap_or_else(PoisonError::into_inner);
        for envelope in self.outbox.drain() {
            render.envelope(envelope);
        }
    }

    fn span(&self, node: NodeId) -> Option<tracing::Span> {
        let render = self.render.lock().unwrap_or_else(PoisonError::into_inner);
        let parent = render.span(node);
        let Some(level) = render.logs() else {
            return parent;
        };
        Some(crate::logs::anchor(
            parent.as_ref(),
            &self.outbox,
            node,
            level,
        ))
    }
}
