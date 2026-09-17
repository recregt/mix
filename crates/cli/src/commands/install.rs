use std::process::ExitCode;

use mix_app::install::Error;
use mix_core::privilege::is_root;

pub async fn run(
    packages: Vec<String>,
    mirror: Option<String>,
    mirror_key: Option<String>,
) -> anyhow::Result<ExitCode> {
    if is_root() {
        return Err(Error::NotRoot.into());
    }
    let _lock = super::acquire_lock()?;

    let Some(user_config) = mix_app::resolve_existing_user_config() else {
        return Err(Error::NotBootstrapped.into());
    };

    let installed = mix_app::install::install(
        &user_config,
        &packages,
        mirror.as_deref(),
        mirror_key.as_deref(),
    )
    .await?;

    mix_ui::ok(format!("installed: {}", installed.join(", ")));
    Ok(ExitCode::SUCCESS)
}
