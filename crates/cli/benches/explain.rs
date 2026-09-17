use mix_app::bootstrap::Error as ActivationError;
use mix_app::install::Error as InstallError;
use mix_app::repair::{Error as RepairError, Unfixable};
use mix_cli::explain;

fn main() {
    divan::main();
}

/// A plan the cache-only gate refused: the names are the library's, how many of them are worth
/// printing is decided here.
fn refused(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("package-{i}-1.2.3")).collect()
}

/// The failure with the most to say: a list to shorten, a count to add, and a hint under it.
#[divan::bench(args = [1, 8, 64])]
fn explain_a_refused_source_build(bencher: divan::Bencher, n: usize) {
    let error = anyhow::Error::from(InstallError::Activation(
        ActivationError::SourceBuildRequired(refused(n)),
    ));

    bencher.bench(|| explain::install::explain(divan::black_box(&error)).message());
}

/// The cheapest shape: a raw error from the bottom of the tool, named for the command that hit
/// it.
#[divan::bench]
fn explain_a_held_lock(bencher: divan::Bencher) {
    let error = anyhow::Error::from(InstallError::Core(mix_core::Error::Locked {
        path: "/run/mix.lock".into(),
    }));

    bencher.bench(|| explain::install::explain(divan::black_box(&error)).message());
}

/// The one that is written per artifact rather than per run: `mix repair` prints one of these
/// for every target it could not put back.
#[divan::bench]
fn explain_a_repair_report(bencher: divan::Bencher) {
    let error = RepairError::Unrepairable {
        artifact: "default profile".to_string(),
        reason: Unfixable::MissingRuntime,
    };

    bencher.bench(|| explain::repair::report(divan::black_box("default profile"), &error));
}
