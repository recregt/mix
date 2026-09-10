use mix_core::{Error, Result};

use crate::{Environment, bootstrap, preflight, teardown};

pub async fn doctor() -> Result<Environment> {
    if !preflight::is_root() {
        return Err(Error::NotRoot("reset the managed environment"));
    }

    teardown::teardown().await?;
    bootstrap::bootstrap().await
}
