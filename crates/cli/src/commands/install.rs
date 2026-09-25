use std::process::ExitCode;

pub async fn run(
    packages: &[String],
    mirror: Option<&str>,
    mirror_key: Option<&str>,
    json: bool,
    build: bool,
) -> anyhow::Result<ExitCode> {
    let (_lock, user_config) = super::acquire_profile()?;

    let installed = mix_app::install::install(
        &user_config,
        packages,
        mirror,
        mirror_key,
        mix_ui::activity_reporter(),
        build,
    )
    .await?;

    if let Some(note) = installed
        .restored
        .and_then(crate::explain::change::restored)
    {
        mix_ui::warn(note.message());
    }

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
