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

fn shown(words: explain::Diagnostic) -> explain::Diagnostic {
    divan::black_box(words.report());
    words
}

/// The cheapest shape: a raw error from the bottom of the tool, named for the command that hit
/// it.
#[divan::bench]
fn explain_a_held_lock(bencher: divan::Bencher) {
    let error = anyhow::Error::from(InstallError::Core(mix_core::Error::Locked {
        path: "/var/lib/mix/lock".into(),
    }));

    bencher.bench(|| {
        shown(explain::install::explain(
            divan::black_box(&error),
            &PACKAGES,
        ))
    });
}

/// A target `mix repair` will not change, named with its reason and the way out, as repair prints
/// it for every target it could not put back.
#[divan::bench]
fn explain_an_unrepairable_target(bencher: divan::Bencher) {
    let error = TargetError::Unrepairable {
        artifact: "default profile".to_string(),
        reason: Unfixable::MissingRuntime,
    };

    bencher.bench(|| shown(explain::target::report(divan::black_box(&error))));
}

fn report(name: &str, finding: Finding) -> HealthReport {
    HealthReport {
        name: name.to_string(),
        category: Category::Filesystem,
        finding: Some(finding),
    }
}

/// A problem as `mix doctor` writes it: the item and what is wrong with it, and a note with what
/// was found against what `mix` set.
#[divan::bench]
fn explain_a_doctor_problem(bencher: divan::Bencher) {
    let report = report(
        "/nix",
        Finding::Mode {
            actual: 0o700,
            expected: 0o755,
        },
    );

    bencher.bench(|| shown(explain::doctor::check(divan::black_box(&report))));
}

/// A problem repair cannot fix, which also carries its own way out.
#[divan::bench]
fn explain_a_doctor_problem_repair_cannot_fix(bencher: divan::Bencher) {
    let report = report("default profile", Finding::RuntimeMissing);

    bencher.bench(|| shown(explain::doctor::check(divan::black_box(&report))));
}

/// The verdict at the end of an audit: the problems are counted and every finding is read to
/// decide whether `mix repair` is worth suggesting, so it grows with the number found.
#[divan::bench(args = [1, 8, 64])]
fn explain_the_doctor_verdict(bencher: divan::Bencher, n: usize) {
    let reports: Vec<HealthReport> = (0..n)
        .map(|i| report(&format!("nixbld{i}"), Finding::RuntimeMissing))
        .collect();

    bencher.bench(|| shown(explain::doctor::unhealthy(divan::black_box(&reports))));
}
