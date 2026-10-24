use mix_conformance::model::Model;
use mix_conformance::suite::{FaultKind, bootstrap, doctor, findings, install, repair, succeeded};
use serde_json::Value;

const JOURNALS: &str = "/var/lib/mix/journal";

async fn interrupted(kind: FaultKind, at: usize) -> Option<Model> {
    let model = Model::new();
    let done = model.run(bootstrap(), false, true).await;
    assert!(succeeded(&done), "{done:#}");
    let crashed = model.faulted(install(&["hello"]), false, at, kind).await;
    crashed.is_none().then_some(model)
}

async fn every_interrupted_install() -> Vec<(String, Model)> {
    let mut models = Vec::new();
    for kind in [FaultKind::CrashBefore, FaultKind::CrashAfter] {
        let mut at = 0;
        while let Some(model) = interrupted(kind, at).await {
            models.push((format!("{kind:?} at {at}"), model));
            at += 1;
        }
    }
    assert!(!models.is_empty(), "no install was interrupted");
    models
}

fn reports(report: &Value) -> &[Value] {
    report["doctor"]["reports"]
        .as_array()
        .map_or(&[], Vec::as_slice)
}

fn pending(report: &Value) -> Vec<String> {
    let journal = reports(report)
        .iter()
        .find(|found| found["target"] == JOURNALS)
        .expect("the journals are checked");
    journal["finding"]["interrupted"]["pending"]
        .as_array()
        .expect("the request is still interrupted")
        .iter()
        .filter_map(|path| path.as_str().map(str::to_string))
        .collect()
}

#[tokio::test]
async fn an_interrupted_request_is_reported_as_left_for_repair_or_the_next_change() {
    for (case, model) in every_interrupted_install().await {
        let report = model.run(doctor(), false, false).await;

        let journal = reports(&report)
            .iter()
            .find(|found| found["target"] == JOURNALS)
            .unwrap_or_else(|| panic!("{case}: {report:#}"));
        assert!(
            journal["finding"].get("interrupted").is_some(),
            "{case}: {journal:#}"
        );
        assert_eq!(journal["unfixable"], "UNFIXABLE_UNSPECIFIED", "{case}");
    }
}

#[tokio::test]
async fn a_file_an_interrupted_request_holds_waits_on_it_instead_of_drifting() {
    for (case, model) in every_interrupted_install().await {
        let report = model.run(doctor(), false, false).await;
        let pending = pending(&report);

        for found in reports(&report) {
            let target = found["target"].as_str().unwrap_or_default();
            if pending.iter().any(|path| path == target) {
                assert_eq!(found["blockedBy"], JOURNALS, "{case}: {found:#}");
                assert!(found.get("finding").is_none(), "{case}: {found:#}");
            }
        }
    }
}

#[tokio::test]
async fn repair_or_the_next_change_leaves_nothing_for_doctor_to_find() {
    for (case, model) in every_interrupted_install().await {
        let next = model.fork();

        let repaired = model.run(repair(), false, true).await;
        assert!(succeeded(&repaired), "{case}: {repaired:#}");
        let after_repair = model.run(doctor(), false, false).await;
        assert_eq!(findings(&after_repair), Vec::<&Value>::new(), "{case}");

        let changed = next.run(install(&["jq"]), false, false).await;
        assert!(succeeded(&changed), "{case}: {changed:#}");
        let after_change = next.run(doctor(), false, false).await;
        assert_eq!(findings(&after_change), Vec::<&Value>::new(), "{case}");
    }
}
