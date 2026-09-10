pub async fn run(fix: bool) -> anyhow::Result<()> {
    if fix {
        return super::report(
            mix_bootstrap::doctor().await?,
            "mix reset its managed state and reinstalled Nix.",
        );
    }

    report_health().await
}

pub async fn report_health() -> anyhow::Result<()> {
    match mix_bootstrap::Environment::open().await {
        Ok(_) => {
            println!("ok: mix's managed state is intact.");
            Ok(())
        }
        Err(e) => {
            println!("not ok: {e}\n\nrun `mix doctor --fix` to reset it.");
            std::process::exit(1);
        }
    }
}
