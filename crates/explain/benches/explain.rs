use mix_core::Category;
use mix_core::ops::health::HealthReport;
use mix_core::report::inspection;
use mix_events::Diagnose;
use mix_events::v1::InspectionReport;
use mix_explain as explain;
use mix_shell::profile::change::Error as InstallError;
use mix_shell::target::{Error as TargetError, Finding, Unfixable};

fn main() {
    divan::main();
}

fn shown(words: explain::Diagnostic) -> explain::Diagnostic {
    divan::black_box(words.report());
    words
}

/// The cheapest shape: a raw error from the bottom of the tool, worded for the command it ended.
#[divan::bench]
fn word_a_missing_lock(bencher: divan::Bencher) {
    let fault = InstallError::Core(mix_core::Error::LockMissing {
        path: "/var/lib/mix/lock".into(),
    })
    .fault();

    bencher.bench(|| {
        shown(explain::outcome(
            divan::black_box("mix install"),
            &divan::black_box("install package"),
            divan::black_box(&fault),
        ))
    });
}

/// A target `mix repair` will not change, named with its reason and the way out, as repair prints
/// it for every target it could not put back.
#[divan::bench]
fn word_an_unrepairable_target(bencher: divan::Bencher) {
    let fault = TargetError::Unrepairable {
        artifact: "default profile".to_string(),
        reason: Unfixable::MissingRuntime,
    }
    .fault();

    bencher.bench(|| {
        shown(explain::outcome(
            divan::black_box("mix repair"),
            &divan::black_box("finish the repair"),
            divan::black_box(&fault),
        ))
    });
}

fn report(name: &str, finding: Finding) -> InspectionReport {
    inspection::report(&HealthReport {
        name: name.to_string(),
        category: Category::Filesystem,
        finding: Some(finding),
        drift: None,
        blocked_by: None,
    })
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
    let reports: Vec<InspectionReport> = (0..n)
        .map(|i| report(&format!("nixbld{i}"), Finding::RuntimeMissing))
        .collect();

    bencher.bench(|| shown(explain::doctor::unhealthy(divan::black_box(&reports))));
}
