use std::future::Future;
use std::pin::pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::future::{Either, select};
pub use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use crate::progress::StepObserver;

#[async_trait]
pub trait Step: Send + Sync {
    type Error: std::error::Error + Send + Sync + 'static;

    fn name(&self) -> &'static str;
    async fn check(&self) -> Result<bool, Self::Error>;
    async fn execute(&mut self, token: &CancellationToken) -> Result<(), Self::Error>;

    async fn rollback(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub struct Plan<E> {
    steps: Vec<Box<dyn Step<Error = E>>>,
    failed_rollbacks: Vec<String>,
    step_observer: Option<Arc<dyn StepObserver>>,
}

pub enum Outcome<E> {
    Completed(Result<(), E>),
    Interrupted,
}

enum StopReason<E> {
    Failed(E),
    Interrupted,
}

impl<E: std::error::Error + Send + Sync + 'static> Plan<E> {
    pub fn new(steps: Vec<Box<dyn Step<Error = E>>>) -> Self {
        Self {
            steps,
            failed_rollbacks: Vec::new(),
            step_observer: None,
        }
    }

    pub fn with_step_observer(mut self, step_observer: Arc<dyn StepObserver>) -> Self {
        self.step_observer = Some(step_observer);
        self
    }

    pub fn failed_rollbacks(&self) -> &[String] {
        &self.failed_rollbacks
    }

    pub async fn run(&mut self) -> Result<(), E> {
        match self.run_cancellable(std::future::pending()).await {
            Outcome::Completed(result) => result,
            Outcome::Interrupted => unreachable!("cancel future never resolves"),
        }
    }

    pub async fn run_cancellable(&mut self, cancel: impl Future<Output = ()>) -> Outcome<E> {
        let mut cancel = pin!(cancel);
        let recording = tracing::Level::INFO <= tracing::level_filters::LevelFilter::current();
        let mut attempted = Attempted::default();
        let mut token: Option<CancellationToken> = None;

        let stop = 'run: {
            for (idx, step) in self.steps.iter_mut().enumerate() {
                let check = match select(cancel.as_mut(), step.check()).await {
                    Either::Left(((), _)) => {
                        if let Some(token) = &token {
                            token.cancel();
                        }
                        break 'run Some(StopReason::Interrupted);
                    }
                    Either::Right((result, _)) => result,
                };

                match check {
                    Ok(true) => {
                        tracing::debug!("skipping (already satisfied): {}", step.name());
                        continue;
                    }
                    Ok(false) => {}
                    Err(e) => {
                        tracing::debug!("check failed: {} ({e})", step.name());
                        break 'run Some(StopReason::Failed(e));
                    }
                }

                let name = step.name();
                tracing::info!("running: {name}");
                attempted.insert(idx);

                let token = token.get_or_insert_with(CancellationToken::new);
                let (executed, interrupted) = if recording {
                    let span = observed(&self.step_observer, tracing::info_span!("step", name));
                    // The span outlives the step's own future on purpose: closing it is what
                    // finishes the line it drew, and a finished line keeps the marker it was
                    // left with, so a failure has to be reported before that happens.
                    let ran = match select(
                        cancel.as_mut(),
                        step.execute(token).instrument(span.clone()),
                    )
                    .await
                    {
                        Either::Left(((), executing)) => {
                            token.cancel();
                            (executing.await, true)
                        }
                        Either::Right((result, _)) => (result, false),
                    };
                    if ran.0.is_err() {
                        failed(&self.step_observer, &span);
                    }
                    ran
                } else {
                    match select(cancel.as_mut(), step.execute(token)).await {
                        Either::Left(((), executing)) => {
                            token.cancel();
                            (executing.await, true)
                        }
                        Either::Right((result, _)) => (result, false),
                    }
                };
                if let Err(e) = executed {
                    tracing::debug!("step failed: {name} ({e})");
                    break 'run Some(StopReason::Failed(e));
                }
                if interrupted {
                    break 'run Some(StopReason::Interrupted);
                }
            }

            None
        };

        match stop {
            Some(StopReason::Failed(e)) => {
                Box::pin(self.unwind(&attempted, recording)).await;
                Outcome::Completed(Err(e))
            }
            Some(StopReason::Interrupted) => {
                tracing::info!("interrupted, rolling back");
                Box::pin(self.unwind(&attempted, recording)).await;
                Outcome::Interrupted
            }
            None => Outcome::Completed(Ok(())),
        }
    }

    async fn unwind(&mut self, attempted: &Attempted, recording: bool) {
        for idx in (0..self.steps.len()).rev() {
            if !attempted.contains(idx) {
                continue;
            }

            let step = &mut self.steps[idx];
            let name = step.name();

            tracing::info!("rolling back: {name}");
            let span = recording
                .then(|| observed(&self.step_observer, tracing::info_span!("rollback", name)));
            let rolled_back = match span {
                Some(span) => step.rollback().instrument(span).await,
                None => step.rollback().await,
            };
            if let Err(e) = rolled_back {
                tracing::error!("rollback failed: {name} ({e})");
                self.failed_rollbacks.push(format!("{name}: {e}"));
            }
        }
    }
}

fn observed(step_observer: &Option<Arc<dyn StepObserver>>, span: tracing::Span) -> tracing::Span {
    if let Some(step_observer) = step_observer {
        step_observer.on_step_span(&span);
    }

    span
}

fn failed(step_observer: &Option<Arc<dyn StepObserver>>, span: &tracing::Span) {
    if let Some(step_observer) = step_observer {
        step_observer.on_step_failed(span);
    }
}

#[derive(Default)]
struct Attempted {
    inline: u64,
    spilled: Vec<u64>,
}

const INLINE_BITS: usize = u64::BITS as usize;

impl Attempted {
    #[inline]
    fn insert(&mut self, idx: usize) {
        if idx < INLINE_BITS {
            self.inline |= 1 << idx;
        } else {
            self.insert_spilled(idx);
        }
    }

    #[inline]
    fn contains(&self, idx: usize) -> bool {
        if idx < INLINE_BITS {
            self.inline & (1 << idx) != 0
        } else {
            self.contains_spilled(idx)
        }
    }

    #[cold]
    fn insert_spilled(&mut self, idx: usize) {
        let (word, bit) = Self::spilled_position(idx);
        if self.spilled.len() <= word {
            self.spilled.resize(word + 1, 0);
        }
        self.spilled[word] |= bit;
    }

    #[cold]
    fn contains_spilled(&self, idx: usize) -> bool {
        let (word, bit) = Self::spilled_position(idx);
        self.spilled.get(word).is_some_and(|bits| bits & bit != 0)
    }

    fn spilled_position(idx: usize) -> (usize, u64) {
        (idx / INLINE_BITS - 1, 1 << (idx % INLINE_BITS))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, thiserror::Error)]
    #[error("probe error")]
    struct ProbeError;

    struct Noop {
        satisfied: bool,
        fail: bool,
    }

    #[async_trait]
    impl Step for Noop {
        type Error = ProbeError;

        fn name(&self) -> &'static str {
            "noop"
        }

        async fn check(&self) -> Result<bool, ProbeError> {
            Ok(self.satisfied)
        }

        async fn execute(&mut self, _token: &CancellationToken) -> Result<(), ProbeError> {
            if self.fail { Err(ProbeError) } else { Ok(()) }
        }
    }

    #[tokio::test]
    async fn dyn_step_with_a_bound_error_type_runs_in_a_plan() {
        let steps: Vec<Box<dyn Step<Error = ProbeError>>> = vec![Box::new(Noop {
            satisfied: false,
            fail: false,
        })];
        let mut plan = Plan::new(steps);
        assert!(plan.run().await.is_ok());
    }

    #[tokio::test]
    async fn plan_stops_and_propagates_the_steps_own_error_type() {
        let steps: Vec<Box<dyn Step<Error = ProbeError>>> = vec![Box::new(Noop {
            satisfied: false,
            fail: true,
        })];
        let mut plan = Plan::new(steps);
        assert!(matches!(plan.run().await, Err(ProbeError)));
    }

    #[tokio::test]
    async fn default_rollback_is_a_noop() {
        let mut step = Noop {
            satisfied: false,
            fail: false,
        };
        assert!(step.rollback().await.is_ok());
    }

    struct Recorder {
        name: &'static str,
        fail_execute: bool,
        fail_rollback: bool,
        log: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl Step for Recorder {
        type Error = ProbeError;

        fn name(&self) -> &'static str {
            self.name
        }

        async fn check(&self) -> Result<bool, ProbeError> {
            Ok(false)
        }

        async fn execute(&mut self, _token: &CancellationToken) -> Result<(), ProbeError> {
            self.log
                .lock()
                .unwrap()
                .push(format!("execute:{}", self.name));
            if self.fail_execute {
                Err(ProbeError)
            } else {
                Ok(())
            }
        }

        async fn rollback(&mut self) -> Result<(), ProbeError> {
            self.log
                .lock()
                .unwrap()
                .push(format!("rollback:{}", self.name));
            if self.fail_rollback {
                Err(ProbeError)
            } else {
                Ok(())
            }
        }
    }

    #[tokio::test]
    async fn a_failed_step_rolls_back_prior_successes_in_reverse_order() {
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let steps: Vec<Box<dyn Step<Error = ProbeError>>> = vec![
            Box::new(Recorder {
                name: "a",
                fail_execute: false,
                fail_rollback: false,
                log: log.clone(),
            }),
            Box::new(Recorder {
                name: "b",
                fail_execute: false,
                fail_rollback: false,
                log: log.clone(),
            }),
            Box::new(Recorder {
                name: "c",
                fail_execute: true,
                fail_rollback: false,
                log: log.clone(),
            }),
        ];
        let mut plan = Plan::new(steps);
        assert!(matches!(plan.run().await, Err(ProbeError)));

        assert_eq!(
            *log.lock().unwrap(),
            vec![
                "execute:a",
                "execute:b",
                "execute:c",
                "rollback:c",
                "rollback:b",
                "rollback:a",
            ]
        );
    }

    #[tokio::test]
    async fn a_rollback_failure_is_reported_but_does_not_stop_remaining_rollbacks() {
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let steps: Vec<Box<dyn Step<Error = ProbeError>>> = vec![
            Box::new(Recorder {
                name: "a",
                fail_execute: false,
                fail_rollback: false,
                log: log.clone(),
            }),
            Box::new(Recorder {
                name: "b",
                fail_execute: false,
                fail_rollback: true,
                log: log.clone(),
            }),
            Box::new(Recorder {
                name: "c",
                fail_execute: true,
                fail_rollback: false,
                log: log.clone(),
            }),
        ];
        let mut plan = Plan::new(steps);
        assert!(matches!(plan.run().await, Err(ProbeError)));

        assert_eq!(
            *log.lock().unwrap(),
            vec![
                "execute:a",
                "execute:b",
                "execute:c",
                "rollback:c",
                "rollback:b",
                "rollback:a",
            ]
        );
        assert_eq!(plan.failed_rollbacks(), ["b: probe error"]);
    }

    #[tokio::test]
    async fn rollback_covers_plans_larger_than_the_inline_bitset() {
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut steps: Vec<Box<dyn Step<Error = ProbeError>>> = (0..64)
            .map(|_| {
                Box::new(Recorder {
                    name: "early",
                    fail_execute: false,
                    fail_rollback: false,
                    log: log.clone(),
                }) as Box<dyn Step<Error = ProbeError>>
            })
            .collect();
        steps.push(Box::new(Noop {
            satisfied: true,
            fail: false,
        }));
        steps.push(Box::new(Recorder {
            name: "late",
            fail_execute: true,
            fail_rollback: false,
            log: log.clone(),
        }));

        let mut plan = Plan::new(steps);
        assert!(matches!(plan.run().await, Err(ProbeError)));

        let log = log.lock().unwrap();
        assert_eq!(log.len(), 130);
        assert_eq!(log[64], "execute:late");
        assert_eq!(log[65], "rollback:late");
        assert!(log[66..].iter().all(|entry| entry == "rollback:early"));
    }

    #[tokio::test]
    async fn a_skipped_step_is_not_rolled_back() {
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let steps: Vec<Box<dyn Step<Error = ProbeError>>> = vec![
            Box::new(Noop {
                satisfied: true,
                fail: false,
            }),
            Box::new(Recorder {
                name: "b",
                fail_execute: true,
                fail_rollback: false,
                log: log.clone(),
            }),
        ];
        let mut plan = Plan::new(steps);
        assert!(matches!(plan.run().await, Err(ProbeError)));
        assert_eq!(*log.lock().unwrap(), vec!["execute:b", "rollback:b"]);
    }

    struct PartiallyMutatingStep {
        progress: u32,
        fail_at: u32,
        log: std::sync::Arc<std::sync::Mutex<Vec<u32>>>,
    }

    #[async_trait]
    impl Step for PartiallyMutatingStep {
        type Error = ProbeError;

        fn name(&self) -> &'static str {
            "partially-mutating"
        }

        async fn check(&self) -> Result<bool, ProbeError> {
            Ok(false)
        }

        async fn execute(&mut self, _token: &CancellationToken) -> Result<(), ProbeError> {
            while self.progress < self.fail_at {
                self.progress += 1;
            }
            Err(ProbeError)
        }

        async fn rollback(&mut self) -> Result<(), ProbeError> {
            while self.progress > 0 {
                self.progress -= 1;
                self.log.lock().unwrap().push(self.progress);
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_step_that_fails_partway_through_execute_still_rolls_back_its_own_partial_work() {
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let steps: Vec<Box<dyn Step<Error = ProbeError>>> = vec![Box::new(PartiallyMutatingStep {
            progress: 0,
            fail_at: 3,
            log: log.clone(),
        })];
        let mut plan = Plan::new(steps);
        assert!(matches!(plan.run().await, Err(ProbeError)));

        assert_eq!(*log.lock().unwrap(), vec![2, 1, 0]);
    }

    #[tokio::test]
    async fn interrupting_before_any_step_runs_executes_nothing() {
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let steps: Vec<Box<dyn Step<Error = ProbeError>>> = vec![Box::new(Recorder {
            name: "a",
            fail_execute: false,
            fail_rollback: false,
            log: log.clone(),
        })];
        let mut plan = Plan::new(steps);

        let outcome = plan.run_cancellable(std::future::ready(())).await;

        assert!(matches!(outcome, Outcome::Interrupted));
        assert!(log.lock().unwrap().is_empty());
    }

    struct SetFlagOnExecute {
        flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
        log: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl Step for SetFlagOnExecute {
        type Error = ProbeError;

        fn name(&self) -> &'static str {
            "a"
        }

        async fn check(&self) -> Result<bool, ProbeError> {
            Ok(false)
        }

        async fn execute(&mut self, _token: &CancellationToken) -> Result<(), ProbeError> {
            self.log.lock().unwrap().push("execute:a".to_string());
            self.flag.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }

        async fn rollback(&mut self) -> Result<(), ProbeError> {
            self.log.lock().unwrap().push("rollback:a".to_string());
            Ok(())
        }
    }

    #[tokio::test]
    async fn interrupting_after_a_step_rolls_back_only_that_step() {
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let steps: Vec<Box<dyn Step<Error = ProbeError>>> = vec![
            Box::new(SetFlagOnExecute {
                flag: flag.clone(),
                log: log.clone(),
            }),
            Box::new(Recorder {
                name: "b",
                fail_execute: false,
                fail_rollback: false,
                log: log.clone(),
            }),
        ];
        let mut plan = Plan::new(steps);
        let cancel = std::future::poll_fn(move |cx| {
            if flag.load(std::sync::atomic::Ordering::SeqCst) {
                std::task::Poll::Ready(())
            } else {
                cx.waker().wake_by_ref();
                std::task::Poll::Pending
            }
        });

        let outcome = plan.run_cancellable(cancel).await;

        assert!(matches!(outcome, Outcome::Interrupted));
        assert_eq!(*log.lock().unwrap(), vec!["execute:a", "rollback:a"]);
    }

    struct CooperativeStep {
        total_iterations: u32,
        log: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl Step for CooperativeStep {
        type Error = ProbeError;

        fn name(&self) -> &'static str {
            "cooperative"
        }

        async fn check(&self) -> Result<bool, ProbeError> {
            Ok(false)
        }

        async fn execute(&mut self, token: &CancellationToken) -> Result<(), ProbeError> {
            for i in 0..self.total_iterations {
                if token.is_cancelled() {
                    self.log.lock().unwrap().push(format!("cancelled-at:{i}"));
                    return Ok(());
                }
                self.log.lock().unwrap().push(format!("tick:{i}"));
                tokio::task::yield_now().await;
            }
            self.log.lock().unwrap().push("completed".to_string());
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_step_already_executing_stops_at_the_next_checkpoint_instead_of_running_to_completion()
     {
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let steps: Vec<Box<dyn Step<Error = ProbeError>>> = vec![Box::new(CooperativeStep {
            total_iterations: 1_000_000,
            log: log.clone(),
        })];
        let mut plan = Plan::new(steps);

        let checkpoint_log = log.clone();
        let cancel = std::future::poll_fn(move |cx| {
            if checkpoint_log.lock().unwrap().len() >= 3 {
                std::task::Poll::Ready(())
            } else {
                cx.waker().wake_by_ref();
                std::task::Poll::Pending
            }
        });

        let outcome = plan.run_cancellable(cancel).await;

        assert!(matches!(outcome, Outcome::Interrupted));
        let log = log.lock().unwrap();
        assert!(
            log.len() < 50,
            "step ran {} ticks instead of stopping at the next checkpoint: {log:?}",
            log.len()
        );
        assert!(log.last().unwrap().starts_with("cancelled-at:"));
    }

    #[tokio::test]
    async fn a_second_signal_sent_while_the_first_is_already_being_handled_has_no_additional_effect()
     {
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let steps: Vec<Box<dyn Step<Error = ProbeError>>> = vec![Box::new(CooperativeStep {
            total_iterations: 1_000_000,
            log: log.clone(),
        })];
        let mut plan = Plan::new(steps);

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let sender_log = log.clone();
        tokio::spawn(async move {
            while sender_log.lock().unwrap().len() < 3 {
                tokio::task::yield_now().await;
            }
            tx.send(()).unwrap();
            tx.send(()).unwrap();
        });

        let cancel = async {
            let _ = rx.recv().await;
        };
        let outcome = plan.run_cancellable(cancel).await;

        assert!(matches!(outcome, Outcome::Interrupted));
        {
            let log = log.lock().unwrap();
            assert!(log.len() < 50, "step ran far past the checkpoint: {log:?}");
            assert!(log.last().unwrap().starts_with("cancelled-at:"));
        }

        assert_eq!(
            rx.try_recv(),
            Ok(()),
            "the second signal must still be sitting unread in the channel: \
             run_cancellable only ever consumes the first"
        );
    }

    struct RecordingObserver(std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>);

    impl StepObserver for RecordingObserver {
        fn on_step_span(&self, span: &tracing::Span) {
            let name = span
                .metadata()
                .map(|metadata| metadata.name())
                .unwrap_or("");
            self.0.lock().unwrap().push(name);
        }

        fn on_step_failed(&self, _span: &tracing::Span) {
            self.0.lock().unwrap().push("failed");
        }
    }

    struct EnablingSubscriber;

    impl tracing::Subscriber for EnablingSubscriber {
        fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, _attributes: &tracing::span::Attributes<'_>) -> tracing::Id {
            tracing::Id::from_u64(1)
        }

        fn record(&self, _span: &tracing::Id, _values: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _span: &tracing::Id, _follows: &tracing::Id) {}
        fn event(&self, _event: &tracing::Event<'_>) {}
        fn enter(&self, _span: &tracing::Id) {}
        fn exit(&self, _span: &tracing::Id) {}
    }

    #[test]
    fn a_step_observer_sees_the_step_and_rollback_spans_while_a_subscriber_is_installed() {
        let observed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let steps: Vec<Box<dyn Step<Error = ProbeError>>> = vec![
            Box::new(Recorder {
                name: "a",
                fail_execute: false,
                fail_rollback: false,
                log: log.clone(),
            }),
            Box::new(Recorder {
                name: "b",
                fail_execute: true,
                fail_rollback: false,
                log: log.clone(),
            }),
        ];
        let mut plan = Plan::new(steps)
            .with_step_observer(std::sync::Arc::new(RecordingObserver(observed.clone())));

        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        // Process-wide rather than scoped: tracing caches callsite interest globally, so a sibling
        // test reaching the same span callsite without a subscriber can otherwise leave these spans
        // disabled and strip their metadata.
        let _ = tracing::subscriber::set_global_default(EnablingSubscriber);
        assert!(matches!(runtime.block_on(plan.run()), Err(ProbeError)));

        // The failure is reported while the failed step's span is still open, so a presentation
        // layer can still reach the line that span is drawing.
        assert_eq!(
            *observed.lock().unwrap(),
            ["step", "step", "failed", "rollback", "rollback"]
        );
    }
}
