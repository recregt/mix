use std::process::ExitCode;

use mix_app::install::Error;
use mix_core::privilege::is_root;

pub async fn run(
    packages: Vec<String>,
    mirror: Option<String>,
    mirror_key: Option<String>,
    json: bool,
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
        mix_ui::activity_reporter(),
    )
    .await?;

    // One line of JSON on stdout and nothing else, so a script never has to parse prose.
    if json {
        println!("{}", installed.to_json());
        return Ok(ExitCode::SUCCESS);
    }

    if !installed.skipped.is_empty() {
        mix_ui::skipped(format!(
            "already installed: {}",
            installed.skipped.join(", ")
        ));
    }
    if installed.changed_nothing() {
        mix_ui::ok("nothing to install");
    } else {
        mix_ui::ok(format!("installed: {}", installed.added.join(", ")));
    }
    Ok(ExitCode::SUCCESS)
}
