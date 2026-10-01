use std::process::ExitCode;

use crate::remote::client::{Route, bootstrap_request};

pub async fn run(
    mirror: Option<&str>,
    mirror_key: Option<&str>,
    force: bool,
    view: &crate::render::sinks::View,
) -> anyhow::Result<ExitCode> {
    super::requested_policy(mirror, mirror_key)?;
    let request = bootstrap_request(mirror, mirror_key, force);
    crate::remote::client::run(request, Route::OneShot, view).await?;
    Ok(ExitCode::SUCCESS)
}
