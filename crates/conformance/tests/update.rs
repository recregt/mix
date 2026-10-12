use std::sync::Arc;

use mix_conformance::model::Model;
use mix_conformance::suite::{bootstrap, doctor, findings, install, repair, succeeded, update};
use mix_core::declared::identity::InvokingUser;
use mix_core::model::World;
use mix_core::model::testkit::{Breakage, Damage};
use mix_exec::Scope;
use mix_nixgen::lock::{LockedInput, NarHash};
use mix_nixgen::{Pins, Rev};
use mix_shell::profile::embedded;
use mix_shell::request::upstream::{Newest, Upstream};

const FLAKE: &str = "/home/alice/.local/state/mix/flake.nix";
const LOCK: &str = "/home/alice/.local/state/mix/flake.lock";

struct Fixed(Pins<LockedInput>);

impl Upstream for Fixed {
    fn newest<'a>(&'a self, _: &'a InvokingUser, _: &'a Scope) -> Newest<'a> {
        Box::pin(async move { Ok(self.0.clone()) })
    }
}

fn newer() -> Pins<LockedInput> {
    Pins {
        home_manager: LockedInput {
            rev: Rev::parse("a1b2c3d4e5f60718293a4b5c6d7e8f9012345678").unwrap(),
            nar_hash: NarHash::parse("sha256-pD3qVlQ4mUCFoTWrYtxecpSitQMsgJvJIN0FUIDZoEU=")
                .unwrap(),
            last_modified: 1_790_000_000,
        },
        nixpkgs: LockedInput {
            rev: Rev::parse("0f1e2d3c4b5a69788796a5b4c3d2e1f001234567").unwrap(),
            nar_hash: NarHash::parse("sha256-V0WmebqwEeAVZMqxfv7Dq7hpKkPPM19W50xQf3D1Kf4=")
                .unwrap(),
            last_modified: 1_790_100_000,
        },
    }
}

fn text(model: &Model, path: &str) -> String {
    model
        .snapshot()
        .contents(path)
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        .unwrap_or_default()
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

fn moved(document: &serde_json::Value) -> usize {
    document["update"]["inputs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|input| input["outcome"] == "PIN_OUTCOME_UPDATED")
        .count()
}

async fn updated(model: Model) -> Model {
    let model = model.with_upstream(Arc::new(Fixed(newer())));
    let bootstrapped = model.run(bootstrap(), false, true).await;
    assert!(succeeded(&bootstrapped), "{bootstrapped:#}");
    let installed = model.run(install(&["hello"]), false, false).await;
    assert!(succeeded(&installed), "{installed:#}");
    model
}

async fn the_machine_stays_healthy_across_an_update(model: Model) {
    let model = updated(model).await;
    let embedded = embedded();

    let before = model.snapshot();
    let planned = model.run(update(), true, false).await;
    assert!(succeeded(&planned), "{planned:#}");
    assert!(model.snapshot() == before, "a dry run changed the model");
    assert_eq!(moved(&planned), 2);
    assert!(
        planned["update"].get("packages").is_none(),
        "a dry run builds no generation to list: {planned:#}"
    );

    let done = model.run(update(), false, false).await;
    assert!(succeeded(&done), "{done:#}");
    assert_eq!(moved(&done), 2);
    assert!(
        done["update"].get("packages").is_none(),
        "the model has no profile on disk to list: {done:#}"
    );
    let nixpkgs = &done["update"]["inputs"][1];
    assert_eq!(nixpkgs["before"]["name"], "nixpkgs");
    assert_eq!(nixpkgs["before"]["rev"], embedded.nixpkgs.rev.as_str());
    assert_eq!(nixpkgs["after"]["rev"], newer().nixpkgs.rev.as_str());
    assert_eq!(nixpkgs["after"]["owner"], "NixOS");
    assert_eq!(nixpkgs["after"]["branch"], "nixos-unstable");
    assert_eq!(
        nixpkgs["after"]["narHash"],
        newer().nixpkgs.nar_hash.as_str()
    );
    assert_eq!(nixpkgs["after"]["lastModified"], "1790100000");
    for (path, pins) in [(FLAKE, &newer()), (LOCK, &newer())] {
        let written = text(&model, path);
        assert!(written.contains(pins.nixpkgs.rev.as_str()), "{path}");
        assert!(written.contains(pins.home_manager.rev.as_str()), "{path}");
        assert!(!written.contains(embedded.nixpkgs.rev.as_str()), "{path}");
    }

    let operations: Vec<&str> = done["changes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|change| change["operation"].as_str())
        .collect();
    assert!(
        operations.contains(&"OPERATION_ACTIVATE_PROFILE"),
        "no generation was built for the new pins: {operations:?}"
    );
    let built = model.snapshot();
    assert_eq!(
        built.active_lock(model.user()),
        built.contents(LOCK),
        "the active generation was not built with the new lock"
    );

    let checked = model.run(doctor(), false, false).await;
    assert!(succeeded(&checked), "{checked:#}");
    assert!(findings(&checked).is_empty(), "{checked:#}");

    let settled = model.snapshot();
    let repaired = model.run(repair(), false, true).await;
    assert!(succeeded(&repaired), "{repaired:#}");
    assert!(
        model.snapshot() == settled,
        "repair moved the pins back: {repaired:#}"
    );

    let again = model.run(update(), false, false).await;
    assert!(succeeded(&again), "{again:#}");
    assert_eq!(moved(&again), 0);
    assert!(again["update"].get("packages").is_none(), "{again:#}");
    assert_eq!(
        again["update"]["inputs"][1]["outcome"],
        "PIN_OUTCOME_CURRENT"
    );
    assert!(
        model.snapshot() == settled,
        "an update to the same pins changed the model"
    );
}

#[tokio::test]
async fn an_update_keeps_the_machine_healthy_while_the_daemon_remembers_the_pins() {
    the_machine_stays_healthy_across_an_update(Model::new().remembering()).await;
}

#[tokio::test]
async fn an_update_keeps_the_machine_healthy_when_the_daemon_restarts_before_every_request() {
    the_machine_stays_healthy_across_an_update(Model::new()).await;
}

#[tokio::test]
async fn damage_outside_the_pins_is_found_and_put_back_exactly_after_an_update() {
    let model = updated(Model::new()).await;
    model.run(update(), false, false).await;
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
async fn a_lock_that_cannot_be_read_falls_back_to_the_embedded_pins_when_the_daemon_restarts() {
    let model = updated(Model::new()).await;
    model.run(update(), false, false).await;

    Breakage {
        path: LOCK.into(),
        damage: Damage::Altered,
    }
    .apply(&mut model.world.lock().unwrap_or_else(|e| e.into_inner()));
    let repaired = model.run(repair(), false, true).await;
    assert!(succeeded(&repaired), "{repaired:#}");

    let embedded = embedded();
    for path in [FLAKE, LOCK] {
        assert!(
            text(&model, path).contains(embedded.nixpkgs.rev.as_str()),
            "{path}"
        );
    }
    let checked = model.run(doctor(), false, false).await;
    assert!(findings(&checked).is_empty(), "{checked:#}");
}

#[tokio::test]
async fn a_daemon_that_remembers_the_pins_restores_them_after_the_lock_is_damaged() {
    let model = updated(Model::new().remembering()).await;
    model.run(update(), false, false).await;
    let settled = model.snapshot();

    Breakage {
        path: LOCK.into(),
        damage: Damage::Altered,
    }
    .apply(&mut model.world.lock().unwrap_or_else(|e| e.into_inner()));
    let repaired = model.run(repair(), false, true).await;
    assert!(succeeded(&repaired), "{repaired:#}");
    assert_eq!(
        differences(&settled, &model.snapshot()),
        Vec::<String>::new(),
        "{repaired:#}"
    );
}
