pub async fn run() -> anyhow::Result<()> {
    super::report(
        mix_bootstrap::doctor().await?,
        "mix reset its managed state and reinstalled Nix.",
    )
}
