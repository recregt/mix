use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, PoisonError};

use crate::v1::{Envelope, NodeProgress, envelope::Event, node_progress::Progress};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Gauge {
    Bytes,
    Builds,
}

type Slot = (u64, Gauge);

fn slot(event: &Event) -> Option<Slot> {
    match event {
        Event::NodeProgress(NodeProgress {
            id,
            progress: Some(Progress::Bytes(_)),
        }) => Some((*id, Gauge::Bytes)),
        Event::NodeProgress(NodeProgress {
            id,
            progress: Some(Progress::Builds(_)),
        }) => Some((*id, Gauge::Builds)),
        _ => None,
    }
}

#[derive(Default)]
struct Queue {
    head: u64,
    seq: u64,
    items: VecDeque<Option<Event>>,
    pending: HashMap<Slot, u64>,
}

pub struct Outbox {
    request: String,
    queue: Mutex<Queue>,
    wake: Box<dyn Fn() + Send + Sync>,
}

impl Outbox {
    pub fn new(request: impl Into<String>, wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            request: request.into(),
            queue: Mutex::new(Queue::default()),
            wake: Box::new(wake),
        }
    }

    pub(crate) fn push(&self, event: Event) {
        {
            let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
            let position = queue.head + queue.items.len() as u64;
            if let Some(slot) = slot(&event)
                && let Some(superseded) = queue.pending.insert(slot, position)
            {
                let index = (superseded - queue.head) as usize;
                queue.items[index] = None;
            }
            queue.items.push_back(Some(event));
        }
        (self.wake)();
    }

    pub fn pop(&self) -> Option<Envelope> {
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            let item = queue.items.pop_front()?;
            let position = queue.head;
            queue.head += 1;
            let Some(event) = item else { continue };
            if let Some(slot) = slot(&event)
                && queue.pending.get(&slot) == Some(&position)
            {
                queue.pending.remove(&slot);
            }
            queue.seq += 1;
            return Some(Envelope {
                seq: queue.seq,
                request: self.request.clone(),
                event: Some(event),
            });
        }
    }

    pub fn drain(&self) -> Vec<Envelope> {
        std::iter::from_fn(|| self.pop()).collect()
    }
}
