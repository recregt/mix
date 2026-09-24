use mix_app::doctor::HealthReport;
use mix_app::profile::Error as ActivationError;
use mix_app::profile::change::Error as InstallError;
use mix_app::target::{Error as TargetError, Finding, Unfixable};
use mix_cli::explain;
use mix_core::Category;

fn main() {
    divan::main();
}

fn refused(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("package-{i}-1.2.3")).collect()
}

#[divan::bench(args = [1, 8, 64])]
fn explain_a_refused_source_build(bencher: divan::Bencher, n: usize) {
    let error = anyhow::Error::from(InstallError::Activation(
        ActivationError::SourceBuildRequired {
            packages: Some(refused(n)),
        },
    ));

    bencher.bench(|| {
        explain::install::explain(divan::black_box(&error), "mix install package --build").message()
    });
}

/// The cheapest shape: a raw error from the bottom of the tool, named for the command that hit
/// it.
#[divan::bench]
fn explain_a_held_lock(bencher: divan::Bencher) {
    let error = anyhow::Error::from(InstallError::Core(mix_core::Error::Locked {
        path: "/run/mix.lock".into(),
    }));

    bencher.bench(|| {
        explain::install::explain(divan::black_box(&error), "mix install package --build").message()
    });
}

/// The one that is written per artifact rather than per run: `mix repair` prints one of these
/// for every target it could not put back.
#[divan::bench]
fn explain_a_repair_report(bencher: divan::Bencher) {
    let error = TargetError::Unrepairable {
        artifact: "default profile".to_string(),
        reason: Unfixable::MissingRuntime,
    };

    bencher.bench(|| explain::target::report(divan::black_box(&error)));
}

fn report(name: &str, finding: Finding) -> HealthReport {
    HealthReport {
        name: name.to_string(),
        category: Category::Filesystem,
        finding: Some(finding),
    }
}

/// The line `mix doctor` writes per failed check: the measurement the audit handed over, put
/// into words.
#[divan::bench]
fn explain_a_health_check(bencher: divan::Bencher) {
    let report = report(
        "/nix",
        Finding::Mode {
            actual: 0o700,
            expected: 0o755,
        },
    );

    bencher.bench(|| explain::doctor::check(divan::black_box(&report)));
}

/// The same line for a finding repair cannot reconcile: the way out is looked up from the
/// finding and written under it.
#[divan::bench]
fn explain_an_unfixable_health_check(bencher: divan::Bencher) {
    let report = report("default profile", Finding::RuntimeMissing);

    bencher.bench(|| explain::doctor::check(divan::black_box(&report)));
}

/// The verdict at the end of an audit: every finding is read to decide whether `mix repair` is
/// worth suggesting, so this is the one that grows with the number of checks that failed.
#[divan::bench(args = [1, 8, 64])]
fn explain_the_audit_verdict(bencher: divan::Bencher, n: usize) {
    let reports: Vec<HealthReport> = (0..n)
        .map(|i| report(&format!("nixbld{i}"), Finding::RuntimeMissing))
        .collect();

    bencher.bench(|| explain::doctor::unhealthy(divan::black_box(&reports)).message());
}
