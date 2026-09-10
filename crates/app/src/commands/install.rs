pub async fn run() -> anyhow::Result<()> {
    mix_bootstrap::install().await?;
    println!("Nix installed.");
    Ok(())
}
