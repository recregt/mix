use mix_cli::explain;
use mix_core::Category;
use mix_shell::ops::doctor::HealthReport;
use mix_shell::profile::change::Error as InstallError;
use mix_shell::target::{Error as TargetError, Finding, Unfixable};

static PACKAGES: std::sync::LazyLock<Vec<String>> =
    std::sync::LazyLock::new(|| vec!["package".to_string()]);

fn main() {
    divan::main();
}

/// The cheapest shape: a raw error from the bottom of the tool, named for the command that hit
/// it.
#[divan::bench]
fn explain_a_held_lock(bencher: divan::Bencher) {
    let error = anyhow::Error::from(InstallError::Core(mix_core::Error::Locked {
        path: "/var/lib/mix/lock".into(),
    }));

    bencher.bench(|| explain::install::explain(divan::black_box(&error), &PACKAGES).message());
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
