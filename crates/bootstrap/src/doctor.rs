use mix_core::Result;

use crate::{install, preflight, teardown};

pub async fn doctor() -> Result<()> {
    preflight::ensure_root("reset mix's managed state")?;
    teardown::teardown().await?;
    install::install().await
}
