use mix_conformance::model::Model;
use mix_conformance::suite::{
    FaultKind, Mix, bootstrap, doctor, findings, install, repair, succeeded, violations,
};
use mix_core::testkit::{Breakage, Damage, breakages, owned};
use proptest_state_machine::prop_state_machine;

#[tokio::test]
async fn the_whole_request_path_runs_on_the_model() {
    let model = Model::new();

    let bootstrapped = model.run(bootstrap(), false, true).await;
    assert!(succeeded(&bootstrapped), "{bootstrapped:#}");

    let before = model.snapshot();
    let predicted = model.run(install(&["hello"]), true, false).await;
    assert!(succeeded(&predicted), "{predicted:#}");
    assert!(model.snapshot() == before, "a dry run changed the model");

    let installed = model.run(install(&["hello"]), false, false).await;
    assert!(succeeded(&installed), "{installed:#}");
    assert_eq!(
        predicted["changes"].as_array().map(Vec::len),
        installed["changes"].as_array().map(Vec::len)
    );
    assert_eq!(installed["install"]["added"], serde_json::json!(["hello"]));

    let checked = model.run(doctor(), false, false).await;
    assert!(succeeded(&checked), "{checked:#}");
    let listed = model
        .snapshot()
        .contents("/home/alice/.local/state/mix/state")
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        .unwrap_or_default();
    assert!(listed.contains("\"hello\""), "{listed}");
    assert!(findings(&checked).is_empty(), "{checked:#}");

    let before = model.snapshot();
    let repaired = model.run(repair(), false, true).await;
    assert!(succeeded(&repaired), "{repaired:#}");
    assert!(
        model.snapshot() == before,
        "repair changed a healthy machine: {repaired:#}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn any_single_damage_to_what_mix_owns_is_repaired_as_planned() {
    let model = Model::new();
    let pristine = model.snapshot();
    model.run(bootstrap(), false, true).await;
    model.run(install(&["hello"]), false, false).await;
    let cases = breakages(&owned(&pristine, &model.snapshot()));
    assert!(!cases.is_empty());

    let mut checks = tokio::task::JoinSet::new();
    for breakage in cases {
        let fork = model.fork();
        checks.spawn(async move { violations(&fork, &breakage).await });
    }
    let mut found = Vec::new();
    while let Some(violations) = checks.join_next().await {
        found.extend(violations.unwrap());
    }
    found.sort();

    assert!(
        found.is_empty(),
        "{} violations:\n{}",
        found.len(),
        found.join("\n")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_history_that_cannot_be_created_puts_the_old_repository_back() {
    let model = Model::new();
    model.run(bootstrap(), false, true).await;
    model.run(install(&["hello"]), false, false).await;
    let repository = mix_core::paths::repository_dir(&model.user().home);
    Breakage {
        path: repository.join(mix_core::paths::REPOSITORY_HEAD),
        damage: Damage::Altered,
    }
    .apply(&mut model.world.lock().unwrap_or_else(|e| e.into_inner()));
    let history = |model: &Model| -> Vec<_> {
        let world = model.snapshot();
        world
            .files
            .into_iter()
            .filter(|(path, _)| path.starts_with(&repository))
            .map(|(path, entry)| (path, entry.content))
            .collect()
    };
    let damaged = history(&model);

    for at in 0.. {
        let fork = model.fork();
        let Some(done) = fork.faulted(repair(), true, at, FaultKind::Fail).await else {
            continue;
        };
        let failed: Vec<&serde_json::Value> = done["changes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|change| change["status"] == "STATUS_FAILED")
            .collect();
        assert!(
            !failed.is_empty(),
            "no step created the repository: {done:#}"
        );
        if failed
            .iter()
            .any(|change| change["operation"] == "OPERATION_CREATE_REPOSITORY")
        {
            assert_eq!(history(&fork), damaged, "{done:#}");
            return;
        }
    }
}

prop_state_machine! {
    #![proptest_config(proptest::prelude::ProptestConfig {
        failure_persistence: Some(Box::new(
            proptest::test_runner::FileFailurePersistence::Direct(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/model.proptest-regressions"
            ))
        )),
        ..proptest::prelude::ProptestConfig::default()
    })]
    #[test]
    fn any_sequence_of_commands_damage_and_faults_keeps_every_invariant(
        sequential 1..8 => Mix
    );
}
