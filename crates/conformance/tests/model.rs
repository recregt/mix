use mix_conformance::Backend;
use mix_conformance::model::Model;
use mix_conformance::suite::{
    bootstrap, doctor, findings, install, repair, step, succeeded, violations, walk,
};
use mix_core::testkit::{breakages, owned};

#[tokio::test]
async fn the_whole_request_path_runs_on_the_model() {
    let mut model = Model::new();

    let bootstrapped = model.run(bootstrap(), false, true).await;
    assert!(succeeded(&bootstrapped), "{bootstrapped:#}");

    let before = model.observe().await;
    let predicted = model.run(install(&["hello"]), true, false).await;
    assert!(succeeded(&predicted), "{predicted:#}");
    assert!(
        model.observe().await == before,
        "a dry run changed the model"
    );

    let installed = model.run(install(&["hello"]), false, false).await;
    assert!(succeeded(&installed), "{installed:#}");
    assert_eq!(
        predicted["changes"].as_array().map(Vec::len),
        installed["changes"].as_array().map(Vec::len)
    );
    assert_eq!(installed["install"]["added"], serde_json::json!(["hello"]));

    let checked = model.run(doctor(), false, false).await;
    assert!(succeeded(&checked), "{checked:#}");
    let listed = model.observe().await.list.unwrap_or_default();
    assert!(listed.contains("\"hello\""), "{listed}");
    assert!(findings(&checked).is_empty(), "{checked:#}");

    let before = model.observe().await;
    let repaired = model.run(repair(), false, true).await;
    assert!(succeeded(&repaired), "{repaired:#}");
    assert!(
        model.observe().await == before,
        "repair changed a healthy machine: {repaired:#}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn any_single_damage_to_what_mix_owns_is_repaired_as_planned() {
    let mut model = Model::new();
    let pristine = model.snapshot();
    model.run(bootstrap(), false, true).await;
    model.run(install(&["hello"]), false, false).await;
    let cases = breakages(&owned(&pristine, &model.snapshot()));
    assert!(!cases.is_empty());

    let mut checks = tokio::task::JoinSet::new();
    for breakage in cases {
        let mut fork = model.fork();
        checks.spawn(async move { violations(&mut fork, &breakage).await });
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

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig {
        cases: 256,
        failure_persistence: Some(Box::new(
            proptest::test_runner::FileFailurePersistence::Direct(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/model.proptest-regressions"
            ))
        )),
        ..proptest::prelude::ProptestConfig::default()
    })]
    #[test]
    fn any_sequence_of_commands_and_damage_keeps_every_invariant(
        steps in proptest::collection::vec(step(), 1..8)
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let found = runtime.block_on(async {
            let mut model = Model::new();
            let pristine = model.snapshot();
            model.run(bootstrap(), false, true).await;
            let cases = breakages(&owned(&pristine, &model.snapshot()));
            walk(&mut model, &cases, &steps).await
        });
        proptest::prop_assert!(found.is_empty(), "{}", found.join("\n"));
    }
}
