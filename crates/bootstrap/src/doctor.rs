use mix_core::Result;

use crate::preflight::RootStatus;
use crate::{Outcome, bootstrap, preflight, teardown};

pub async fn doctor() -> Result<Outcome> {
    if let RootStatus::ReExecuted { exit_code } =
        preflight::ensure_root("reset mix's managed state")?
    {
        return Ok(Outcome::ReExecuted { exit_code });
    }

    teardown::teardown().await?;
    bootstrap::bootstrap().await
}
