use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::tree::Sink;
use crate::v1::{Envelope, envelope::Event};

pub trait Clock: Send + Sync {
    fn wall(&self) -> pbjson_types::Timestamp;
    fn monotonic(&self) -> Duration;
}

pub struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn wall(&self) -> pbjson_types::Timestamp {
        let since = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        pbjson_types::Timestamp {
            seconds: i64::try_from(since.as_secs()).unwrap_or(i64::MAX),
            nanos: i32::try_from(since.subsec_nanos()).unwrap_or(0),
        }
    }

    fn monotonic(&self) -> Duration {
        self.origin.elapsed()
    }
}

struct State {
    seq: u64,
    started: HashMap<u64, Duration>,
}

pub struct Stamper<C, F> {
    request: String,
    clock: C,
    deliver: F,
    state: Mutex<State>,
}

impl<C: Clock, F: Fn(Envelope) + Send + Sync> Stamper<C, F> {
    pub fn new(request: impl Into<String>, clock: C, deliver: F) -> Self {
        Self {
            request: request.into(),
            clock,
            deliver,
            state: Mutex::new(State {
                seq: 0,
                started: HashMap::new(),
            }),
        }
    }
}

impl<C: Clock, F: Fn(Envelope) + Send + Sync> Sink for Stamper<C, F> {
    fn emit(&self, mut event: Event) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let now = self.clock.monotonic();
        match &mut event {
            Event::NodeStarted(node) => {
                state.started.insert(node.id, now);
            }
            Event::NodeFinished(node) => {
                if let Some(started) = state.started.remove(&node.id)
                    && node.elapsed.is_none()
                {
                    node.elapsed = Some(duration(now.saturating_sub(started)));
                }
            }
            _ => {}
        }
        state.seq += 1;
        (self.deliver)(Envelope {
            seq: state.seq,
            time: Some(self.clock.wall()),
            request: self.request.clone(),
            event: Some(event),
        });
    }
}

fn duration(elapsed: Duration) -> pbjson_types::Duration {
    pbjson_types::Duration {
        seconds: i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX),
        nanos: i32::try_from(elapsed.subsec_nanos()).unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::tree::{Ending, Node};
    use crate::v1::{Command, Step, node_started};

    struct Manual(AtomicU64);

    impl Clock for Arc<Manual> {
        fn wall(&self) -> pbjson_types::Timestamp {
            pbjson_types::Timestamp {
                seconds: 1_790_000_000 + self.0.load(Ordering::Relaxed) as i64,
                nanos: 0,
            }
        }

        fn monotonic(&self) -> Duration {
            Duration::from_secs(self.0.load(Ordering::Relaxed))
        }
    }

    type Delivered = Arc<Mutex<Vec<Envelope>>>;

    fn stamped() -> (Arc<Manual>, Delivered, Arc<dyn Sink>) {
        let clock = Arc::new(Manual(AtomicU64::new(0)));
        let delivered = Arc::new(Mutex::new(Vec::new()));
        let into = delivered.clone();
        let sink = Arc::new(Stamper::new("request", clock.clone(), move |envelope| {
            into.lock().unwrap().push(envelope)
        }));
        (clock, delivered, sink)
    }

    #[test]
    fn every_envelope_is_numbered_from_one_and_carries_the_request() {
        let (_, delivered, sink) = stamped();
        let root = Node::root(
            sink,
            Arc::new(|| false),
            "doctor",
            Command::default(),
            vec![],
        );
        root.child("check", node_started::Kind::Step(Step::default()))
            .finish(Ending::succeeded());
        root.finish(Ending::succeeded());

        let delivered = delivered.lock().unwrap();
        let seqs: Vec<u64> = delivered.iter().map(|envelope| envelope.seq).collect();
        assert_eq!(seqs, [1, 2, 3, 4]);
        assert!(
            delivered
                .iter()
                .all(|envelope| envelope.request == "request")
        );
    }

    #[test]
    fn a_finished_node_is_told_how_long_it_ran_on_the_monotonic_clock() {
        let (clock, delivered, sink) = stamped();
        let root = Node::root(
            sink,
            Arc::new(|| false),
            "doctor",
            Command::default(),
            vec![],
        );
        clock.0.store(7, Ordering::Relaxed);
        root.finish(Ending::succeeded());

        let delivered = delivered.lock().unwrap();
        let Some(Event::NodeFinished(node)) = &delivered[1].event else {
            panic!("expected the root to finish, got {:?}", delivered[1].event);
        };
        assert_eq!(
            node.elapsed,
            Some(pbjson_types::Duration {
                seconds: 7,
                nanos: 0
            })
        );
        assert_eq!(delivered[1].time.unwrap().seconds, 1_790_000_007);
    }
}
