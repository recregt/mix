use crate::ui;

pub async fn run(mirror: Option<String>) -> anyhow::Result<()> {
    super::ensure_root_or_exit("initialize the managed environment");

    mix_bootstrap::bootstrap(mirror.as_deref()).await?;
    ui::ok("System environment initialized and ready.");
    Ok(())
}
