use std::process::ExitCode;

use mix_events::v1::DoctorRequest;
use mix_events::v1::command::Request;

use crate::client::Route;

pub async fn run(view: &crate::render::sinks::View) -> anyhow::Result<ExitCode> {
    crate::client::run(Request::Doctor(DoctorRequest {}), Route::Socket, view).await?;
    Ok(ExitCode::SUCCESS)
}
