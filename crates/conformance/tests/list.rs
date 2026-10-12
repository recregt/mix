use mix_conformance::model::{Channel, Model};
use mix_conformance::suite::{
    FaultKind, bootstrap, install, list, listed_names, listed_version, repair, succeeded, update,
    warned,
};
use mix_events::v1::RemoveRequest;
use mix_events::v1::command::Request;
use mix_nixgen::lock::EMBEDDED;

async fn bootstrapped() -> Model {
    let model = Model::new();
    let done = model.run(bootstrap(), false, true).await;
    assert!(succeeded(&done), "{done:#}");
    model
}

fn remove(package: &str) -> Request {
    Request::Remove(RemoveRequest {
        packages: vec![package.to_string()],
    })
}

#[tokio::test]
async fn a_user_who_was_never_set_up_is_told_to_bootstrap() {
    let model = Model::new();

    let refused = model.run(list(), false, false).await;

    assert!(!succeeded(&refused), "{refused:#}");
    assert_eq!(refused["problems"][0]["code"], "CODE_NOT_BOOTSTRAPPED");
}

#[tokio::test]
async fn root_is_refused_the_way_every_profile_command_refuses_it() {
    let model = bootstrapped().await;

    let refused = model.run(list(), false, true).await;

    assert!(!succeeded(&refused), "{refused:#}");
    assert_eq!(refused["problems"][0]["code"], "CODE_ROOT_NOT_ALLOWED");
}

#[tokio::test]
async fn a_list_follows_every_change_made_to_the_profile() {
    let model = bootstrapped().await;
    let names = |document: &serde_json::Value| listed_names(document);

    let seeded = model.run(list(), false, false).await;
    assert!(succeeded(&seeded), "{seeded:#}");
    assert_eq!(names(&seeded), ["git"]);

    let added = model.run(install(&["hello", "jq"]), false, false).await;
    assert!(succeeded(&added), "{added:#}");
    let grown = model.run(list(), false, false).await;
    assert_eq!(names(&grown), ["git", "hello", "jq"]);

    let removed = model.run(remove("jq"), false, false).await;
    assert!(succeeded(&removed), "{removed:#}");
    let shrunk = model.run(list(), false, false).await;
    assert_eq!(names(&shrunk), ["git", "hello"]);
}

#[tokio::test]
async fn a_list_names_the_generation_the_profile_is_on() {
    let model = bootstrapped().await;
    let active = |model: &Model| {
        model
            .snapshot()
            .profile(model.user())
            .and_then(|profile| profile.active)
            .expect("an active generation")
    };

    let first = model.run(list(), false, false).await;
    assert_eq!(
        first["list"]["active"]["number"],
        active(&model).to_string()
    );
    let path = first["list"]["active"]["storePath"].clone();

    model.run(install(&["hello"]), false, false).await;
    let second = model.run(list(), false, false).await;

    assert_eq!(
        second["list"]["active"]["number"],
        active(&model).to_string()
    );
    assert_ne!(second["list"]["active"]["storePath"], path);
}

#[tokio::test]
async fn a_list_changes_nothing() {
    let model = bootstrapped().await;
    model.run(install(&["hello"]), false, false).await;
    let before = model.snapshot();

    let done = model.run(list(), false, false).await;
    let predicted = model.run(list(), true, false).await;

    assert!(succeeded(&done), "{done:#}");
    assert!(succeeded(&predicted), "{predicted:#}");
    assert!(model.snapshot() == before, "a list changed the model");
}

#[tokio::test]
async fn the_versions_a_list_shows_are_the_ones_an_update_reports() {
    let model = bootstrapped().await;
    model.run(install(&["hello"]), false, false).await;
    let newest = model.upstream().pins();

    let before = model.run(list(), false, false).await;
    assert_eq!(
        listed_version(&before, "hello"),
        Some(Channel::version(&EMBEDDED))
    );

    let moved = model.run(update(), false, false).await;
    assert!(succeeded(&moved), "{moved:#}");
    let hello = moved["update"]["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|package| package["name"] == "hello")
        .expect("update names the package whose version moved");
    assert_eq!(hello["fromVersion"], Channel::version(&EMBEDDED));
    assert_eq!(hello["toVersion"], Channel::version(&newest));

    let after = model.run(list(), false, false).await;
    assert_eq!(
        listed_version(&after, "hello"),
        Some(Channel::version(&newest))
    );
    assert_eq!(moved["update"]["active"], after["list"]["active"]);
}

#[tokio::test]
async fn a_list_taken_before_an_interrupted_request_is_put_back_says_so() {
    let model = bootstrapped().await;
    let installed = model.run(install(&["ripgrep"]), false, false).await;
    assert!(succeeded(&installed), "{installed:#}");
    let crashed = model
        .faulted(remove("ripgrep"), false, 3, FaultKind::CrashBefore)
        .await;
    assert!(crashed.is_none(), "the remove was meant to be interrupted");

    let pending = model.run(list(), false, false).await;
    assert!(succeeded(&pending), "{pending:#}");
    assert!(warned(&pending, "CODE_INTERRUPTED_REQUEST"), "{pending:#}");
    assert_eq!(listed_names(&pending), ["git"]);

    let repaired = model.run(repair(), false, true).await;
    assert!(succeeded(&repaired), "{repaired:#}");
    let settled = model.run(list(), false, false).await;
    assert!(!warned(&settled, "CODE_INTERRUPTED_REQUEST"), "{settled:#}");
    assert_eq!(listed_names(&settled), ["git", "ripgrep"]);
}
