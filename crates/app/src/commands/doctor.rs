use crate::ui;

pub async fn run(fix: bool, mirror: Option<String>) -> anyhow::Result<()> {
    if fix {
        super::ensure_root_or_exit("reset the managed environment");

        mix_bootstrap::doctor(mirror.as_deref()).await?;
        ui::ok("System state successfully restored to pristine condition.");
        return Ok(());
    }

    check().await
}

pub async fn check() -> anyhow::Result<()> {
    match mix_bootstrap::Environment::open().await {
        Ok(_) => {
            ui::ok("System health is intact.");
            Ok(())
        }
        Err(e) => {
            ui::fail(check_failed_message(e));
            std::process::exit(1);
        }
    }
}

pub fn check_failed_message(e: impl std::fmt::Display) -> String {
    format!(
        "System health check failed: {e}\n\nRun `mix doctor --fix` to reconcile configuration drift."
    )
}
