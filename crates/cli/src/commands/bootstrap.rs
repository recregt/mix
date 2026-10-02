use std::process::ExitCode;

use crate::client::{Route, bootstrap_request};

pub async fn run(
    mirror: Option<&str>,
    mirror_key: Option<&str>,
    force: bool,
    view: &crate::render::sinks::View,
) -> anyhow::Result<ExitCode> {
    let request = bootstrap_request(mirror, mirror_key, force);
    if let Err(invalid) = mix_core::policy::Policy::new(mirror, mirror_key) {
        return Err(crate::client::Failed {
            request,
            fault: mix_core::diagnose::failed(
                mix_events::v1::Code::InvalidMirror,
                invalid.to_string(),
                None,
            ),
        }
        .into());
    }
    crate::client::run(request, Route::OneShot, view).await?;
    Ok(ExitCode::SUCCESS)
}
