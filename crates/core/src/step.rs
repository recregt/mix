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
                tracing::debug!(step = step.name(), "already satisfied, skipping");
                continue;
            }

            tracing::info!(step = step.name(), "running");
            step.execute().await?;
        }

        Ok(())
    }
}
