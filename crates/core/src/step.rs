use async_trait::async_trait;

#[async_trait]
pub trait Step: Send + Sync {
    type Error: std::error::Error + Send + Sync + 'static;

    fn name(&self) -> &'static str;
    async fn check(&self) -> Result<bool, Self::Error>;
    async fn execute(&mut self) -> Result<(), Self::Error>;

    async fn rollback(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub struct Plan<E> {
    steps: Vec<Box<dyn Step<Error = E>>>,
    failed_rollbacks: Vec<String>,
}

impl<E: std::error::Error + Send + Sync + 'static> Plan<E> {
    pub fn new(steps: Vec<Box<dyn Step<Error = E>>>) -> Self {
        Self {
            steps,
            failed_rollbacks: Vec::new(),
        }
    }

    pub fn failed_rollbacks(&self) -> &[String] {
        &self.failed_rollbacks
    }

    pub async fn run(&mut self) -> Result<(), E> {
        let mut attempted = Attempted::default();

        let failure = 'run: {
            for (idx, step) in self.steps.iter_mut().enumerate() {
                match step.check().await {
                    Ok(true) => {
                        tracing::debug!("skipping (already satisfied): {}", step.name());
                        continue;
                    }
                    Ok(false) => {}
                    Err(e) => {
                        tracing::debug!("check failed: {} ({e})", step.name());
                        break 'run Some(e);
                    }
                }

                tracing::info!("running: {}", step.name());
                attempted.insert(idx);
                if let Err(e) = step.execute().await {
                    tracing::debug!("step failed: {} ({e})", step.name());
                    break 'run Some(e);
                }
            }

            None
        };

        match failure {
            Some(e) => {
                Box::pin(self.unwind(&attempted)).await;
                Err(e)
            }
            None => Ok(()),
        }
    }

    async fn unwind(&mut self, attempted: &Attempted) {
        for idx in (0..self.steps.len()).rev() {
            if !attempted.contains(idx) {
                continue;
            }

            let step = &mut self.steps[idx];
            let name = step.name();

            tracing::info!("rolling back: {name}");
            if let Err(e) = step.rollback().await {
                tracing::error!("rollback failed: {name} ({e})");
                self.failed_rollbacks.push(format!("{name}: {e}"));
            }
        }
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

        async fn execute(&mut self) -> Result<(), ProbeError> {
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

        async fn execute(&mut self) -> Result<(), ProbeError> {
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

        async fn execute(&mut self) -> Result<(), ProbeError> {
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
}
