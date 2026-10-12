use mix_conformance::model::{Channel, Model};
use mix_conformance::suite::{
    FaultKind, bootstrap, install, list, listed_names, listed_version, repair, succeeded, update,
    warned,
};
use std::sync::Arc;

use mix_core::declared::paths::{FLAKE_LOCK, FLAKE_NIX, GENERATION_INPUTS, mix_state_dir};
use mix_core::model::Content;
use mix_events::v1::RemoveRequest;
use mix_events::v1::command::Request;
use mix_nixgen::Pins;
use mix_nixgen::lock::{EMBEDDED, LockedInput};

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

fn rewound_to(model: &Model, older: &Pins<LockedInput>) {
    let rewind = |source: &str, text: &[u8]| -> Arc<[u8]> {
        if source == FLAKE_LOCK {
            return Arc::from(mix_nixgen::lock::render(older.clone()).as_bytes());
        }
        let text = String::from_utf8_lossy(text)
            .replace(EMBEDDED.nixpkgs.rev.as_str(), older.nixpkgs.rev.as_str())
            .replace(
                EMBEDDED.home_manager.rev.as_str(),
                older.home_manager.rev.as_str(),
            );
        Arc::from(text.as_bytes())
    };
    let mut world = model.world.lock().unwrap();
    let dir = mix_state_dir(&model.user().home);
    let uid = model.user().uid;
    for (index, (source, _)) in GENERATION_INPUTS.iter().enumerate() {
        if *source != FLAKE_LOCK && *source != FLAKE_NIX {
            continue;
        }
        let entry = world.files.get_mut(&dir.join(source)).unwrap();
        if let Content::File(text) = &entry.content {
            entry.content = Content::File(rewind(source, text));
        }
        let profile = world.profiles.get_mut(&uid).unwrap();
        let active = profile.active.unwrap();
        let built = profile.built.get_mut(&active).unwrap();
        if let Some(text) = built[index].clone() {
            built[index] = Some(rewind(source, &text));
        }
    }
}

#[tokio::test]
async fn a_profile_built_before_this_mix_is_reported_at_the_versions_it_holds() {
    let model = bootstrapped().await;
    model.run(install(&["hello"]), false, false).await;
    let older = Channel::at(-3).pins();
    assert!(older.nixpkgs.last_modified < EMBEDDED.nixpkgs.last_modified);
    rewound_to(&model, &older);
    let newest = model.upstream().pins();

    let before = model.run(list(), false, false).await;
    assert!(succeeded(&before), "{before:#}");
    assert_eq!(
        listed_version(&before, "hello"),
        Some(Channel::version(&older))
    );

    let moved = model.run(update(), false, false).await;
    assert!(succeeded(&moved), "{moved:#}");
    let hello = moved["update"]["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|package| package["name"] == "hello")
        .expect("update names the package whose version moved");
    assert_eq!(hello["fromVersion"], Channel::version(&older));
    assert_eq!(hello["toVersion"], Channel::version(&newest));

    let after = model.run(list(), false, false).await;
    assert_eq!(
        listed_version(&after, "hello"),
        Some(Channel::version(&newest))
    );
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
