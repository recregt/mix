use std::process::ExitCode;

use mix_core::paths::PROFILE_SNIPPET_DEST;

pub async fn run(
    mirror: Option<&str>,
    mirror_key: Option<&str>,
    force: bool,
    verbosity: u8,
) -> anyhow::Result<ExitCode> {
    if mix_core::privilege::is_root() {
        let _lock = super::acquire_lock()?;
        let scope = mix_exec::Scope::root();
        let _watch =
            crate::interrupt::watch(&scope, crate::interrupt::BOOTSTRAP, std::future::pending());
        mix_app::bootstrap::bootstrap(
            mix_core::privilege::invoking_user(),
            mirror,
            mirror_key,
            force,
            {
                let reporters = mix_ui::reporters();
                mix_app::Reporters {
                    downloads: reporters.downloads,
                    steps: reporters.steps,
                    activity: reporters.activity,
                }
            },
            &scope,
        )
        .await?;
    } else {
        crate::remote::client::bootstrap(mirror, mirror_key, force, verbosity).await?;
    }
    mix_ui::ok("mix is ready!");
    mix_ui::info("");
    mix_ui::info(format!(
        "to use installed packages in this terminal session, run:\n  source {PROFILE_SNIPPET_DEST}"
    ));
    Ok(ExitCode::SUCCESS)
}
