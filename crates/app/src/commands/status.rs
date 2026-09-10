pub async fn run() -> anyhow::Result<()> {
    match mix_bootstrap::Environment::open().await {
        Ok(_) => println!("ok: mix's managed state is intact."),
        Err(e) => println!(
            "not ok: {e}\n\nrun `mix install` (or `mix doctor` if this was working before)."
        ),
    }
    Ok(())
}
