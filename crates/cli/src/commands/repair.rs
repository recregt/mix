use std::process::ExitCode;

use mix_events::v1::RepairRequest;
use mix_events::v1::command::Request;

use crate::remote::client::Route;

pub async fn run(view: &crate::render::sinks::View) -> anyhow::Result<ExitCode> {
    crate::remote::client::run(Request::Repair(RepairRequest {}), Route::Socket, view).await?;
    Ok(ExitCode::SUCCESS)
}
