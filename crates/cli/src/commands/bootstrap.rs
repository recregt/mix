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
        let reporters = mix_ui::reporters();
        let ctx = mix_app::Context::new(mix_exec::Scope::root())
            .with_user(
                mix_core::privilege::invoking_user().and_then(mix_app::profile::user_config_for),
            )
            .with_reporters(mix_app::Reporters {
                downloads: reporters.downloads,
                steps: reporters.steps,
                activity: reporters.activity,
            })
            .with_env(super::request_env(mirror, mirror_key))
            .with_host(super::host_config());
        let _watch = crate::interrupt::watch(
            &ctx.scope,
            crate::interrupt::BOOTSTRAP,
            std::future::pending(),
            crate::interrupt::Side::Client,
        );
        mix_app::bootstrap::bootstrap(&ctx, force).await?;
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
