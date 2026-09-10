pub mod bootstrap;
pub mod doctor;

fn report(outcome: mix_bootstrap::Outcome, message: &str) -> anyhow::Result<()> {
    match outcome {
        mix_bootstrap::Outcome::Bootstrapped(_) => {
            println!("{message}");
            Ok(())
        }
        mix_bootstrap::Outcome::ReExecuted { exit_code } => std::process::exit(exit_code),
    }
}
