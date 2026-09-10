pub async fn run() -> anyhow::Result<()> {
    super::report(mix_bootstrap::bootstrap().await?, "Nix installed.")
}
