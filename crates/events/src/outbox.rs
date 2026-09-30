use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError};

use rustc_hash::{FxHashMap, FxHashSet};

use crate::v1::{Envelope, Log, NodeProgress, envelope::Event, node_progress::Progress};

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
    live: bool,
    head: u64,
    seq: u64,
    items: VecDeque<Event>,
    pending: FxHashMap<Slot, u64>,
    superseded: FxHashSet<u64>,
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
                queue.superseded.insert(superseded);
            }
            queue.items.push_back(event);
        }
        (self.wake)();
    }

    pub(crate) fn open(&self) {
        self.queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .live = true;
    }

    pub(crate) fn push_last(&self, event: Event) {
        {
            let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
            queue.live = false;
            queue.items.push_back(event);
        }
        (self.wake)();
    }

    pub fn log(&self, log: Log) {
        {
            let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
            if !queue.live {
                return;
            }
            queue.items.push_back(Event::Log(log));
        }
        (self.wake)();
    }

    pub fn pop(&self) -> Option<Envelope> {
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        self.next(&mut queue)
    }

    pub fn drain(&self) -> Vec<Envelope> {
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        let mut drained = Vec::with_capacity(queue.items.len());
        while let Some(envelope) = self.next(&mut queue) {
            drained.push(envelope);
        }
        drained
    }

    fn next(&self, queue: &mut Queue) -> Option<Envelope> {
        loop {
            let event = queue.items.pop_front()?;
            let position = queue.head;
            queue.head += 1;
            if !queue.superseded.is_empty() && queue.superseded.remove(&position) {
                continue;
            }
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
}
