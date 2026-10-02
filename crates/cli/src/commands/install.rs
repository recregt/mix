use std::process::ExitCode;

use mix_events::v1::InstallRequest;
use mix_events::v1::command::Request;

use crate::client::Route;

pub async fn run(
    packages: &[String],
    view: &crate::render::sinks::View,
) -> anyhow::Result<ExitCode> {
    let request = Request::Install(InstallRequest {
        packages: packages.to_vec(),
    });
    crate::client::run(request, Route::Socket, view).await?;
    Ok(ExitCode::SUCCESS)
}
