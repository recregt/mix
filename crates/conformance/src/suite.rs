use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use mix_core::declared::identity::InvokingUser;
use mix_core::declared::state::StateManifest;
use mix_core::model::World;
use mix_core::model::testkit::{Breakage, Damage, owned};
use mix_events::v1::command::Request;
use mix_events::v1::{
    CleanRequest, DoctorRequest, InstallRequest, ListRequest, RemoveRequest, RepairRequest,
    UpdateRequest,
};
use mix_shell::drive::Fault;
use proptest::prelude::*;
use proptest::sample::select;
use proptest_state_machine::{ReferenceStateMachine, StateMachineTest};
use serde_json::Value;

use crate::model::Model;

pub const PACKAGES: [&str; 3] = ["hello", "jq", "ripgrep"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Install(&'static str),
    Remove(&'static str),
    Clean(bool),
    Repair,
    Update,
    List,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultKind {
    Fail,
    CrashBefore,
    CrashAfter,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transition {
    Run(Command),
    Damage(Breakage),
    Faulted(Command, usize, FaultKind),
    Advance,
}

impl FaultKind {
    pub fn fault(self) -> Fault {
        match self {
            FaultKind::Fail => Fault::Fail(mix_core::effect::Failure::Io {
                path: "/injected".into(),
                kind: std::io::ErrorKind::StorageFull,
            }),
            FaultKind::CrashBefore => Fault::CrashBefore,
            FaultKind::CrashAfter => Fault::CrashAfter,
        }
    }
}

impl Command {
    fn request(self) -> (Request, bool) {
        match self {
            Command::Install(package) => (install(&[package]), false),
            Command::Remove(package) => (
                Request::Remove(RemoveRequest {
                    packages: vec![package.to_string()],
                }),
                false,
            ),
            Command::Clean(all) => (Request::Clean(CleanRequest { all }), false),
            Command::Repair => (repair(), true),
            Command::Update => (update(), false),
            Command::List => (list(), false),
        }
    }

    fn effectful(self) -> bool {
        !matches!(self, Command::List)
    }

    fn atomic(self) -> bool {
        matches!(
            self,
            Command::Install(_) | Command::Remove(_) | Command::Update
        )
    }
}

pub fn install(packages: &[&str]) -> Request {
    Request::Install(InstallRequest {
        packages: packages.iter().map(|name| name.to_string()).collect(),
    })
}

pub fn bootstrap() -> Request {
    Request::Bootstrap(Box::default())
}

pub fn doctor() -> Request {
    Request::Doctor(DoctorRequest {})
}

pub fn repair() -> Request {
    Request::Repair(RepairRequest {})
}

pub fn update() -> Request {
    Request::Update(UpdateRequest {})
}

pub fn list() -> Request {
    Request::List(ListRequest {})
}

fn listed(document: &Value) -> impl Iterator<Item = &Value> {
    document["list"]["packages"]
        .as_array()
        .into_iter()
        .flatten()
}

pub fn listed_names(document: &Value) -> Vec<String> {
    listed(document)
        .filter_map(|package| package["name"].as_str().map(str::to_string))
        .collect()
}

pub fn listed_version(document: &Value, name: &str) -> Option<String> {
    listed(document)
        .find(|package| package["name"] == name)
        .and_then(|package| package["version"].as_str().map(str::to_string))
}

fn held(world: &World, user: &InvokingUser) -> Vec<String> {
    let Some(raw) = world.active_list(user) else {
        return Vec::new();
    };
    let mut names = StateManifest::parse(&String::from_utf8_lossy(raw))
        .map(|manifest| manifest.packages)
        .unwrap_or_default();
    names.sort();
    names.dedup();
    names
}

fn reported(document: &Value) -> Vec<(String, Option<String>, Option<String>)> {
    document["update"]["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|package| {
            let field = |key: &str| package[key].as_str().map(str::to_string);
            (
                field("name").unwrap_or_default(),
                field("fromVersion"),
                field("toVersion"),
            )
        })
        .collect()
}

pub fn succeeded(document: &Value) -> bool {
    document["status"] == "STATUS_SUCCEEDED"
}

pub fn warned(document: &Value, code: &str) -> bool {
    document["warnings"]
        .as_array()
        .is_some_and(|warnings| warnings.iter().any(|warning| warning["code"] == code))
}

pub fn changes(document: &Value) -> Vec<String> {
    document["changes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|change| change["status"] == "STATUS_SUCCEEDED")
        .map(|change| {
            format!(
                "{} {} {}",
                change["step"], change["operation"], change["subject"]
            )
        })
        .collect()
}

fn reports(doctor: &Value) -> impl Iterator<Item = &Value> {
    doctor["doctor"]["reports"].as_array().into_iter().flatten()
}

pub fn findings(doctor: &Value) -> Vec<&Value> {
    reports(doctor)
        .filter(|report| report.get("finding").is_some())
        .collect()
}

pub fn fixable(doctor: &Value) -> Vec<String> {
    let refused: Vec<String> = reports(doctor)
        .filter(|report| report.get("unfixable").is_some())
        .filter_map(|report| report["target"].as_str().map(str::to_string))
        .collect();
    reports(doctor)
        .filter(|report| report.get("finding").is_some() && report.get("unfixable").is_none())
        .filter(|report| {
            let target = report["target"].as_str().unwrap_or_default();
            !refused
                .iter()
                .any(|parent| Path::new(target).starts_with(parent))
        })
        .map(|report| format!("{} {}", report["target"], report["finding"]))
        .collect()
}

fn recovery_blocked(doctor: &Value) -> bool {
    reports(doctor).any(|report| {
        report
            .get("unfixable")
            .is_some_and(|reason| reason != "UNFIXABLE_UNRECOVERED")
    })
}

pub fn unfixable(doctor: &Value) -> bool {
    reports(doctor).any(|report| report.get("unfixable").is_some())
}

fn unexplained(document: &Value) -> bool {
    document["exit"] != 0 && document["problems"].as_array().is_none_or(Vec::is_empty)
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime for the model")
}

pub struct Healthy {
    pub world: World,
    pub owned: Arc<[PathBuf]>,
}

pub fn healthy() -> &'static Healthy {
    static HEALTHY: OnceLock<Healthy> = OnceLock::new();
    HEALTHY.get_or_init(|| {
        runtime().block_on(async {
            let model = Model::new();
            let pristine = model.snapshot();
            let bootstrapped = model.run(bootstrap(), false, true).await;
            assert!(succeeded(&bootstrapped), "{bootstrapped:#}");
            let world = model.snapshot();
            Healthy {
                owned: owned(&pristine, &world).into(),
                world,
            }
        })
    })
}

pub async fn violations(model: &Model, breakage: &Breakage) -> Vec<String> {
    if !breakage.apply(&mut model.world.lock().unwrap_or_else(|e| e.into_inner())) {
        return Vec::new();
    }
    let at = format!("{:?} {}", breakage.damage, breakage.path.display());
    let mut found = Vec::new();
    let predicted = model.run(repair(), true, true).await;
    let repaired = model.run(repair(), false, true).await;
    if changes(&predicted) != changes(&repaired) {
        found.push(format!(
            "{at}: the dry run predicted {:?} but repair made {:?}",
            changes(&predicted),
            changes(&repaired)
        ));
    }
    if unexplained(&repaired) {
        found.push(format!(
            "{at}: repair exited {} without naming a problem",
            repaired["exit"]
        ));
    }
    let checked = model.run(doctor(), false, false).await;
    let left = fixable(&checked);
    if !left.is_empty() {
        found.push(format!("{at}: repair left {left:?}"));
    }
    if !unfixable(&checked) {
        let world = model.snapshot();
        if world.committed(model.user()) != Some(world.staged(model.user())) {
            found.push(format!("{at}: the repository does not record the config"));
        }
    }
    let again = model.run(repair(), false, true).await;
    if !changes(&again).is_empty() {
        found.push(format!(
            "{at}: a second repair changed {:?}",
            changes(&again)
        ));
    }
    found
}

async fn faulted(model: &Model, command: Command, at: usize, kind: FaultKind) -> Vec<String> {
    let (request, as_root) = command.request();
    let before = model.snapshot();
    let Some(done) = model.faulted(request, as_root, at, kind).await else {
        return Vec::new();
    };
    let mut found = Vec::new();
    if command.atomic() && !succeeded(&done) && before.logs.is_empty() {
        let mut after = model.snapshot();
        let kept = !after.logs.is_empty();
        after.logs.clear();
        let incomplete = warned(&done, "CODE_ROLLBACK_INCOMPLETE");
        if kept != incomplete {
            found.push(format!(
                "kept a journal {kept} but said the rollback was incomplete {incomplete}"
            ));
        }
        if !kept && after != before {
            found.push("a failed change was not rolled back".to_string());
        }
    }
    if unexplained(&done) {
        found.push(format!("exited {} without a problem", done["exit"]));
    }
    found
}

async fn ran(model: &Model, command: Command) -> Vec<String> {
    let (request, as_root) = command.request();
    let mut found = Vec::new();
    let before = (command == Command::List).then(|| model.snapshot());
    let listed_before = match command {
        Command::Update => Some(model.run(list(), false, false).await),
        _ => None,
    };
    let predicted = model.run(request.clone(), true, as_root).await;
    let done = model.run(request, false, as_root).await;
    if changes(&predicted) != changes(&done) {
        found.push(format!(
            "predicted {:?} but made {:?}",
            changes(&predicted),
            changes(&done)
        ));
    }
    if unexplained(&done) {
        found.push(format!("exited {} without a problem", done["exit"]));
    }
    let world = model.snapshot();
    if command.effectful() && succeeded(&done) && !world.logs.is_empty() {
        found.push(format!("journals left behind {:?}", world.logs.keys()));
    }
    if command.effectful() && !world.pending().is_empty() {
        found.push(format!("left pending {:?}", world.pending()));
    }
    if command.atomic()
        && succeeded(&done)
        && world.contents(
            mix_core::declared::paths::mix_state_dir(&model.user().home)
                .join(mix_core::declared::paths::STATE_FILE),
        ) != world.active_list(model.user())
    {
        found.push("the list differs from the active generation".to_string());
    }
    if command.atomic()
        && succeeded(&done)
        && !changes(&done).is_empty()
        && world.contents(
            mix_core::declared::paths::mix_state_dir(&model.user().home)
                .join(mix_core::declared::paths::FLAKE_LOCK),
        ) != world.active_lock(model.user())
    {
        found.push("the lock differs from the active generation".to_string());
    }
    if let Some(before) = &before {
        if world != *before {
            found.push("a list changed the machine".to_string());
        }
        let active = world
            .profile(model.user())
            .and_then(|profile| profile.active);
        let said = done["list"]["active"]["number"]
            .as_str()
            .and_then(|number| number.parse::<u64>().ok());
        if succeeded(&done) && said != active {
            found.push(format!(
                "listed generation {said:?} but the profile is on {active:?}"
            ));
        }
        if succeeded(&done) && listed_names(&done) != held(&world, model.user()) {
            found.push(format!(
                "listed {:?} but the active generation holds {:?}",
                listed_names(&done),
                held(&world, model.user())
            ));
        }
    }
    if command == Command::Update && succeeded(&done) {
        let listed_after = model.run(list(), false, false).await;
        if !done["update"]["active"].is_null()
            && done["update"]["active"] != listed_after["list"]["active"]
        {
            found.push(format!(
                "update reported generation {} but the list shows {}",
                done["update"]["active"], listed_after["list"]["active"]
            ));
        }
        for (name, from, to) in reported(&done) {
            if let Some(listed_before) = &listed_before
                && from.is_some()
                && listed_version(listed_before, &name) != from
            {
                found.push(format!(
                    "update moved {name} from {from:?} but the list showed {:?}",
                    listed_version(listed_before, &name)
                ));
            }
            if to.is_some() && listed_version(&listed_after, &name) != to {
                found.push(format!(
                    "update moved {name} to {to:?} but the list shows {:?}",
                    listed_version(&listed_after, &name)
                ));
            }
        }
    }
    if command == Command::Repair {
        let checked = model.run(doctor(), false, false).await;
        let left = fixable(&checked);
        if !left.is_empty() {
            found.push(format!("repair left {left:?}"));
        }
        let again = model.run(repair(), false, true).await;
        if !changes(&again).is_empty() {
            found.push(format!("a second repair changed {:?}", changes(&again)));
        }
    }
    found
}

async fn finale(model: &Model) -> Vec<String> {
    let mut found = Vec::new();
    let repaired = model.run(repair(), false, true).await;
    let world = model.snapshot();
    let doctored = model.run(doctor(), false, false).await;
    if !world.logs.is_empty() && !recovery_blocked(&doctored) {
        found.push(format!(
            "a final repair left journals {:?}: {repaired:#}",
            world.logs.keys()
        ));
    }
    let left = fixable(&doctored);
    if !left.is_empty() {
        found.push(format!("a final repair left {left:?}"));
    }
    found
}

#[derive(Debug, Clone)]
pub struct Spec {
    pub owned: Arc<[PathBuf]>,
}

pub struct Mix;

fn command() -> impl Strategy<Value = Command> {
    prop_oneof![
        select(&PACKAGES[..]).prop_map(Command::Install),
        select(&PACKAGES[..]).prop_map(Command::Remove),
        any::<bool>().prop_map(Command::Clean),
        Just(Command::Repair),
        Just(Command::Update),
        Just(Command::List),
    ]
}

impl ReferenceStateMachine for Mix {
    type State = Spec;
    type Transition = Transition;

    fn init_state() -> BoxedStrategy<Spec> {
        Just(Spec {
            owned: Arc::clone(&healthy().owned),
        })
        .boxed()
    }

    fn transitions(state: &Spec) -> BoxedStrategy<Transition> {
        let damage = (select(state.owned.to_vec()), select(&Damage::ALL[..]))
            .prop_map(|(path, damage)| Transition::Damage(Breakage { path, damage }));
        let fault = prop_oneof![
            Just(FaultKind::Fail),
            Just(FaultKind::CrashBefore),
            Just(FaultKind::CrashAfter),
        ];
        prop_oneof![
            3 => command().prop_map(Transition::Run),
            1 => damage,
            1 => (command(), 0..12usize, fault)
                .prop_map(|(command, at, kind)| Transition::Faulted(command, at, kind)),
            1 => Just(Transition::Advance),
        ]
        .boxed()
    }

    fn apply(state: Spec, _: &Transition) -> Spec {
        state
    }
}

pub struct System {
    runtime: tokio::runtime::Runtime,
    model: Model,
    found: Vec<String>,
}

impl StateMachineTest for Mix {
    type SystemUnderTest = System;
    type Reference = Mix;

    fn init_test(_: &Spec) -> System {
        let model = Model::new();
        *model.world.lock().unwrap_or_else(|e| e.into_inner()) = healthy().world.clone();
        System {
            runtime: runtime(),
            model,
            found: Vec::new(),
        }
    }

    fn apply(mut system: System, _: &Spec, transition: Transition) -> System {
        let model = &system.model;
        let found = system.runtime.block_on(async {
            match &transition {
                Transition::Run(command) => ran(model, *command).await,
                Transition::Damage(breakage) => {
                    breakage.apply(&mut model.world.lock().unwrap_or_else(|e| e.into_inner()));
                    Vec::new()
                }
                Transition::Faulted(command, at, kind) => {
                    faulted(model, *command, *at, *kind).await
                }
                Transition::Advance => {
                    model.upstream().advance();
                    Vec::new()
                }
            }
        });
        system.found.extend(
            found
                .into_iter()
                .map(|found| format!("{transition:?}: {found}")),
        );
        system
    }

    fn check_invariants(system: &System, _: &Spec) {
        assert!(system.found.is_empty(), "{}", system.found.join("\n"));
    }

    fn teardown(system: System, _: Spec) {
        let found = system.runtime.block_on(finale(&system.model));
        assert!(found.is_empty(), "{}", found.join("\n"));
    }
}
