pub async fn run() -> anyhow::Result<()> {
    mix_bootstrap::doctor().await?;
    println!("mix reset its managed state and reinstalled Nix.");
    Ok(())
}
