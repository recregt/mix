use async_trait::async_trait;

use crate::error::Result;

#[async_trait]
pub trait Step: Send + Sync {
    fn name(&self) -> &'static str;
    async fn check(&self) -> Result<bool>;
    async fn execute(&mut self) -> Result<()>;
}

pub struct Plan {
    steps: Vec<Box<dyn Step>>,
}

impl Plan {
    pub fn new(steps: Vec<Box<dyn Step>>) -> Self {
        Self { steps }
    }

    pub async fn run(&mut self) -> Result<()> {
        for step in &mut self.steps {
            if step.check().await? {
                tracing::debug!("skipping (already satisfied): {}", step.name());
                continue;
            }

            tracing::info!("running: {}", step.name());
            if let Err(e) = step.execute().await {
                tracing::error!("step failed: {} ({e})", step.name());
                return Err(e);
            }
        }

        Ok(())
    }
}
