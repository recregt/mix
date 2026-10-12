use mix_conformance::model::{Channel, Model};
use mix_conformance::suite::{bootstrap, doctor, findings, install, repair, succeeded, update};
use mix_core::declared::policy::Policy;
use mix_core::model::World;
use mix_core::model::testkit::{Breakage, Damage};
use mix_events::v1::BootstrapRequest;
use mix_events::v1::command::Request;
use mix_nixgen::Pins;
use mix_nixgen::lock::{EMBEDDED, LockedInput};
use serde_json::Value;

const FLAKE: &str = "/home/alice/.local/state/mix/flake.nix";
const LOCK: &str = "/home/alice/.local/state/mix/flake.lock";

fn text(model: &Model, path: &str) -> String {
    model
        .snapshot()
        .contents(path)
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        .unwrap_or_default()
}

fn pinned(model: &Model) -> Option<Pins<LockedInput>> {
    mix_core::declared::pins::read(&text(model, LOCK))
}

fn differences(before: &World, after: &World) -> Vec<String> {
    let mut found = Vec::new();
    for (path, entry) in &before.files {
        match after.files.get(path) {
            None => found.push(format!("removed {}", path.display())),
            Some(now) if now.content != entry.content => {
                found.push(format!("changed {}", path.display()));
            }
            Some(_) => {}
        }
    }
    for path in after.files.keys() {
        if !before.files.contains_key(path) {
            found.push(format!("added {}", path.display()));
        }
    }
    found
}

fn outcomes(document: &Value) -> Vec<(String, String)> {
    document["update"]["inputs"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|input| {
            (
                input["name"].as_str().unwrap_or_default().to_string(),
                input["outcome"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

fn all(outcome: &str) -> Vec<(String, String)> {
    ["home-manager", "nixpkgs"]
        .map(|name| (name.to_string(), outcome.to_string()))
        .to_vec()
}

fn packages(document: &Value) -> Vec<(String, String, String)> {
    document["update"]["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|package| {
            let field = |name: &str| package[name].as_str().unwrap_or("-").to_string();
            (field("name"), field("fromVersion"), field("toVersion"))
        })
        .collect()
}

async fn installed(model: Model) -> Model {
    let bootstrapped = model.run(bootstrap(), false, true).await;
    assert!(succeeded(&bootstrapped), "{bootstrapped:#}");
    let installed = model.run(install(&["hello", "jq"]), false, false).await;
    assert!(succeeded(&installed), "{installed:#}");
    model
}

async fn updated(model: Model) -> Model {
    let model = installed(model).await;
    let done = model.run(update(), false, false).await;
    assert!(succeeded(&done), "{done:#}");
    assert_eq!(pinned(&model), Some(model.upstream().pins()));
    model
}

fn healthy(document: &Value) {
    assert!(succeeded(document), "{document:#}");
    assert!(findings(document).is_empty(), "{document:#}");
}

#[tokio::test]
async fn an_update_moves_the_pins_rebuilds_the_profile_and_names_what_changed() {
    let model = installed(Model::new()).await;
    let newest = model.upstream().pins();
    let moved: Vec<(String, String, String)> = ["git", "hello", "jq"]
        .map(|name| {
            (
                name.to_string(),
                Channel::version(&EMBEDDED),
                Channel::version(&newest),
            )
        })
        .to_vec();

    let before = model.snapshot();
    let planned = model.run(update(), true, false).await;
    assert!(succeeded(&planned), "{planned:#}");
    assert!(model.snapshot() == before, "a dry run changed the model");
    assert_eq!(outcomes(&planned), all("INPUT_OUTCOME_UPDATED"));
    assert_eq!(packages(&planned), moved);
    assert!(planned["update"].get("active").is_none(), "{planned:#}");

    let done = model.run(update(), false, false).await;
    assert!(succeeded(&done), "{done:#}");
    assert_eq!(outcomes(&done), all("INPUT_OUTCOME_UPDATED"));
    assert_eq!(packages(&done), moved);
    let nixpkgs = &done["update"]["inputs"][1];
    assert_eq!(nixpkgs["owner"], "NixOS");
    assert_eq!(nixpkgs["branch"], "nixos-unstable");
    assert_eq!(nixpkgs["from"]["rev"], EMBEDDED.nixpkgs.rev.as_str());
    assert_eq!(nixpkgs["to"]["rev"], newest.nixpkgs.rev.as_str());
    assert_eq!(nixpkgs["to"]["narHash"], newest.nixpkgs.nar_hash.as_str());

    let built = model.snapshot();
    let active = built
        .profile(model.user())
        .and_then(|profile| profile.active)
        .expect("an active generation");
    assert_eq!(done["update"]["active"]["number"], active.to_string());
    assert_eq!(pinned(&model), Some(newest.clone()));
    assert!(text(&model, FLAKE).contains(newest.nixpkgs.rev.as_str()));
    assert_eq!(
        built.active_lock(model.user()),
        built.contents(LOCK),
        "the active generation was not built with the new lock"
    );

    healthy(&model.run(doctor(), false, false).await);
    let repaired = model.run(repair(), false, true).await;
    assert!(succeeded(&repaired), "{repaired:#}");
    assert!(model.snapshot() == built, "repair moved the pins back");
}

#[tokio::test]
async fn an_update_to_the_pins_already_held_changes_nothing() {
    let model = updated(Model::new()).await;
    let settled = model.snapshot();

    let again = model.run(update(), false, false).await;

    assert!(succeeded(&again), "{again:#}");
    assert_eq!(outcomes(&again), all("INPUT_OUTCOME_CURRENT"));
    assert!(packages(&again).is_empty(), "{again:#}");
    assert!(again["update"].get("active").is_none(), "{again:#}");
    assert!(again["changes"].as_array().is_none_or(Vec::is_empty));
    assert!(model.snapshot() == settled);
}

#[tokio::test]
async fn an_upstream_behind_the_held_pins_keeps_them() {
    let model = installed(Model::new().with_upstream(Channel::at(-1))).await;
    let settled = model.snapshot();

    let done = model.run(update(), false, false).await;

    assert!(succeeded(&done), "{done:#}");
    assert_eq!(outcomes(&done), all("INPUT_OUTCOME_KEPT"));
    assert_eq!(pinned(&model), Some(EMBEDDED));
    assert!(model.snapshot() == settled);
}

#[tokio::test]
async fn successive_updates_follow_the_upstream_as_it_moves() {
    let model = updated(Model::new()).await;
    model.upstream().advance();

    let done = model.run(update(), false, false).await;

    assert!(succeeded(&done), "{done:#}");
    assert_eq!(outcomes(&done), all("INPUT_OUTCOME_UPDATED"));
    assert_eq!(pinned(&model), Some(model.upstream().pins()));
    healthy(&model.run(doctor(), false, false).await);
}

#[tokio::test]
async fn a_damaged_lock_is_restored_to_the_pins_of_the_active_generation() {
    let model = updated(Model::new()).await;
    let settled = model.snapshot();

    Breakage {
        path: LOCK.into(),
        damage: Damage::Altered,
    }
    .apply(&mut model.world.lock().unwrap_or_else(|e| e.into_inner()));
    let checked = model.run(doctor(), false, false).await;
    assert!(!findings(&checked).is_empty(), "{checked:#}");

    let repaired = model.run(repair(), false, true).await;
    assert!(succeeded(&repaired), "{repaired:#}");
    assert_eq!(
        differences(&settled, &model.snapshot()),
        Vec::<String>::new(),
        "{repaired:#}"
    );
}

#[tokio::test]
async fn damage_outside_the_pins_is_put_back_exactly_after_an_update() {
    let model = updated(Model::new()).await;
    let settled = model.snapshot();

    Breakage {
        path: FLAKE.into(),
        damage: Damage::Altered,
    }
    .apply(&mut model.world.lock().unwrap_or_else(|e| e.into_inner()));
    let checked = model.run(doctor(), false, false).await;
    assert!(!findings(&checked).is_empty(), "{checked:#}");

    let repaired = model.run(repair(), false, true).await;
    assert!(succeeded(&repaired), "{repaired:#}");
    assert_eq!(
        differences(&settled, &model.snapshot()),
        Vec::<String>::new(),
        "{repaired:#}"
    );
}

#[tokio::test]
async fn a_forced_bootstrap_keeps_the_updated_pins_like_it_keeps_the_packages() {
    let model = updated(Model::new()).await;
    let newest = model.upstream().pins();

    let forced = model
        .run(
            Request::Bootstrap(Box::new(BootstrapRequest {
                force: true,
                ..BootstrapRequest::default()
            })),
            false,
            true,
        )
        .await;
    assert!(succeeded(&forced), "{forced:#}");
    assert_eq!(pinned(&model), Some(newest.clone()));
    healthy(&model.run(doctor(), false, false).await);

    let installed = model.run(install(&["ripgrep"]), false, false).await;
    assert!(succeeded(&installed), "{installed:#}");
    assert_eq!(pinned(&model), Some(newest));
}

#[tokio::test]
async fn a_machine_that_moves_to_a_mirror_goes_back_to_the_embedded_pins() {
    let model = updated(Model::new()).await;
    let mirrored = Model::on(
        model.snapshot(),
        model.user().clone(),
        Policy::new(Some("http://mirror.internal"), None).unwrap(),
    );

    let checked = mirrored.run(doctor(), false, false).await;
    assert!(!findings(&checked).is_empty(), "{checked:#}");
    let repaired = mirrored.run(repair(), false, true).await;
    assert!(succeeded(&repaired), "{repaired:#}");

    assert_eq!(pinned(&mirrored), Some(EMBEDDED));
    let built = mirrored.snapshot();
    assert_eq!(built.active_lock(mirrored.user()), built.contents(LOCK));
    healthy(&mirrored.run(doctor(), false, false).await);
}

#[tokio::test]
async fn a_mirrored_machine_refuses_to_update_and_changes_nothing() {
    let model = installed(
        Model::new().with_policy(Policy::new(Some("http://mirror.internal"), None).unwrap()),
    )
    .await;
    let settled = model.snapshot();

    let refused = model.run(update(), false, false).await;

    assert!(!succeeded(&refused), "{refused:#}");
    assert_eq!(
        refused["problems"][0]["code"], "CODE_UPDATE_UNAVAILABLE",
        "{refused:#}"
    );
    assert!(model.snapshot() == settled);
}
