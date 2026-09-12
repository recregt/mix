use async_trait::async_trait;

#[async_trait]
pub trait Step: Send + Sync {
    type Error: std::error::Error + Send + Sync + 'static;

    fn name(&self) -> &'static str;
    async fn check(&self) -> Result<bool, Self::Error>;
    async fn execute(&mut self) -> Result<(), Self::Error>;
}

pub struct Plan<E> {
    steps: Vec<Box<dyn Step<Error = E>>>,
}

impl<E: std::error::Error + Send + Sync + 'static> Plan<E> {
    pub fn new(steps: Vec<Box<dyn Step<Error = E>>>) -> Self {
        Self { steps }
    }

    pub async fn run(&mut self) -> Result<(), E> {
        for step in &mut self.steps {
            if step.check().await? {
                tracing::debug!("skipping (already satisfied): {}", step.name());
                continue;
            }

            tracing::info!("running: {}", step.name());
            if let Err(e) = step.execute().await {
                tracing::debug!("step failed: {} ({e})", step.name());
                return Err(e);
            }
        }

        Ok(())
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
}
