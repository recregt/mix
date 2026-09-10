use crate::ui;

pub async fn run() -> anyhow::Result<()> {
    super::ensure_root_or_exit("initialize the managed environment");

    mix_bootstrap::bootstrap().await?;
    ui::ok("System environment initialized and ready.");
    Ok(())
}
