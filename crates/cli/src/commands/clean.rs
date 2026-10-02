use std::process::ExitCode;

use mix_events::v1::CleanRequest;
use mix_events::v1::command::Request;

use crate::remote::client::Route;

pub async fn run(all: bool, view: &crate::render::sinks::View) -> anyhow::Result<ExitCode> {
    crate::remote::client::run(Request::Clean(CleanRequest { all }), Route::Socket, view).await?;
    Ok(ExitCode::SUCCESS)
}
