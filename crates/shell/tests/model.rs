use std::sync::{Arc, Mutex};

use mix_core::action::UserFacts;
use mix_core::identity::InvokingUser;
use mix_core::paths::RUNNING_PROGRAM;
use mix_core::policy::Policy;
use mix_core::testkit::{Breakage, breakages, owned};
use mix_core::world::World;
use mix_events::Render;
use mix_events::v1::command::Request;
use mix_events::v1::{Command, DoctorRequest, Envelope, InstallRequest, RepairRequest};
use mix_shell::request::context::Host;
use mix_shell::request::lock::Locks;
use mix_shell::{Caller, Session};
use serde_json::Value;

struct Recorded(Arc<Mutex<Vec<Envelope>>>);

impl Render for Recorded {
    fn envelope(&mut self, envelope: Envelope) {
        self.0.lock().unwrap().push(envelope);
    }

    fn detail(&self) -> mix_events::Detail {
        mix_events::Detail::Trace
    }
}

fn alice() -> InvokingUser {
    InvokingUser {
        uid: 1000,
        gid: 1000,
        name: "alice".into(),
        home: "/home/alice".into(),
    }
}

struct Machine {
    world: Arc<Mutex<World>>,
    dir: tempfile::TempDir,
    locks: Arc<Locks>,
    user: InvokingUser,
}

impl Machine {
    fn on(world: World) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let locks = Arc::new(Locks::new(dir.path().join("lock")));
        Self {
            world: Arc::new(Mutex::new(world)),
            dir,
            locks,
            user: alice(),
        }
    }

    fn new() -> Self {
        let user = alice();
        let mut world = World::default();
        world.with_dir("/home", 0o755, (0, 0));
        world.with_dir(&user.home, 0o700, (user.uid, user.gid));
        world.users.insert(
            user.name.clone(),
            UserFacts {
                uid: user.uid,
                gid: user.gid,
                home: user.home.clone(),
                shell: "/bin/sh".into(),
                comment: String::new(),
            },
        );
        world.with_file(
            std::env::current_exe().unwrap(),
            b"mix-daemon",
            0o755,
            (0, 0),
        );
        world.with_file(RUNNING_PROGRAM, b"mix-daemon", 0o755, (0, 0));
        let dir = tempfile::tempdir().unwrap();
        let locks = Arc::new(Locks::new(dir.path().join("lock")));
        Self {
            world: Arc::new(Mutex::new(world)),
            dir,
            locks,
            user,
        }
    }

    async fn run(&self, request: Request, dry_run: bool, as_root: bool) -> Value {
        let recorded: Arc<Mutex<Vec<Envelope>>> = Arc::default();
        let session = Session::new(mix_exec::Scope::root())
            .with_host(Host::Model(Arc::clone(&self.world)))
            .with_locks(Arc::clone(&self.locks))
            .with_journals(self.dir.path().join("journal"))
            .with_policy(Policy::new(None, None).unwrap())
            .with_caller(Caller::Account {
                peer_is_root: as_root,
                account: Some(self.user.clone()),
            })
            .with_render(Recorded(Arc::clone(&recorded)));
        mix_shell::request::run(
            &session,
            Command {
                dry_run,
                ..mix_events::command(request)
            },
        )
        .await;
        let envelopes = recorded.lock().unwrap().clone();
        mix_events::validate(envelopes.iter()).expect("every request streams a valid tree");
        let document = mix_render::document::of(&envelopes).expect("every request ends its root");
        serde_json::to_value(document).unwrap()
    }

    fn snapshot(&self) -> World {
        self.world.lock().unwrap().clone()
    }
}

fn succeeded(document: &Value) -> bool {
    document["status"] == "STATUS_SUCCEEDED"
}

fn install(packages: &[&str]) -> Request {
    Request::Install(InstallRequest {
        packages: packages.iter().map(|name| name.to_string()).collect(),
    })
}

#[tokio::test]
async fn the_whole_request_path_runs_on_the_model() {
    let machine = Machine::new();

    let bootstrap = machine
        .run(Request::Bootstrap(Box::default()), false, true)
        .await;
    assert!(succeeded(&bootstrap), "{bootstrap:#}");

    let before = machine.snapshot();
    let predicted = machine.run(install(&["hello"]), true, false).await;
    assert!(succeeded(&predicted), "{predicted:#}");
    assert!(machine.snapshot() == before, "a dry run changed the model");

    let installed = machine.run(install(&["hello"]), false, false).await;
    assert!(succeeded(&installed), "{installed:#}");
    assert_eq!(
        predicted["changes"].as_array().map(Vec::len),
        installed["changes"].as_array().map(Vec::len)
    );

    let doctor = machine
        .run(Request::Doctor(DoctorRequest {}), false, false)
        .await;
    assert!(succeeded(&doctor), "{doctor:#}");
    assert_eq!(installed["install"]["added"], serde_json::json!(["hello"]));
    let listed = machine
        .snapshot()
        .contents("/home/alice/.local/state/mix/state")
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        .unwrap_or_default();
    assert!(listed.contains("\"hello\""), "{listed}");
    let findings: Vec<&Value> = doctor["doctor"]["reports"]
        .as_array()
        .expect("doctor reports every target")
        .iter()
        .filter(|report| report.get("finding").is_some())
        .collect();
    assert!(findings.is_empty(), "{findings:#?}");

    let before = machine.snapshot();
    let repair = machine
        .run(Request::Repair(RepairRequest {}), false, true)
        .await;
    assert!(succeeded(&repair), "{repair:#}");
    assert!(
        machine.snapshot() == before,
        "repair changed a healthy machine: {repair:#}"
    );
}

fn changes(document: &Value) -> Vec<String> {
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

fn fixable(doctor: &Value) -> Vec<String> {
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
                .any(|parent| std::path::Path::new(target).starts_with(parent))
        })
        .map(|report| format!("{} {}", report["target"], report["finding"]))
        .collect()
}

fn unfixable(doctor: &Value) -> bool {
    reports(doctor).any(|report| report.get("unfixable").is_some())
}

async fn violations(healthy: World, breakage: Breakage) -> Vec<String> {
    let machine = Machine::on(healthy);
    {
        let mut world = machine.world.lock().unwrap();
        if !breakage.apply(&mut world) {
            return Vec::new();
        }
    }
    let at = format!("{:?} {}", breakage.damage, breakage.path.display());
    let mut found = Vec::new();
    let predicted = machine
        .run(Request::Repair(RepairRequest {}), true, true)
        .await;
    let repaired = machine
        .run(Request::Repair(RepairRequest {}), false, true)
        .await;
    if changes(&predicted) != changes(&repaired) {
        found.push(format!(
            "{at}: the dry run predicted {:?} but repair made {:?}",
            changes(&predicted),
            changes(&repaired)
        ));
    }
    if repaired["exit"] != 0 && repaired["problems"].as_array().is_none_or(Vec::is_empty) {
        found.push(format!(
            "{at}: repair exited {} without naming a problem",
            repaired["exit"]
        ));
    }
    let doctor = machine
        .run(Request::Doctor(DoctorRequest {}), false, false)
        .await;
    let left = fixable(&doctor);
    if !left.is_empty() {
        found.push(format!("{at}: repair left {left:?}"));
    }
    if !unfixable(&doctor) {
        let world = machine.snapshot();
        if world.committed(&machine.user) != Some(world.staged(&machine.user)) {
            found.push(format!("{at}: the repository does not record the config"));
        }
    }
    let again = machine
        .run(Request::Repair(RepairRequest {}), false, true)
        .await;
    if !changes(&again).is_empty() {
        found.push(format!(
            "{at}: a second repair changed {:?}",
            changes(&again)
        ));
    }
    found
}

#[tokio::test(flavor = "multi_thread")]
async fn any_single_damage_to_what_mix_owns_is_repaired_as_planned() {
    let machine = Machine::new();
    let pristine = machine.snapshot();
    machine
        .run(Request::Bootstrap(Box::default()), false, true)
        .await;
    machine.run(install(&["hello"]), false, false).await;
    let healthy = machine.snapshot();
    let cases = breakages(&owned(&pristine, &healthy));
    assert!(!cases.is_empty());

    let mut checks = tokio::task::JoinSet::new();
    for breakage in cases {
        checks.spawn(violations(healthy.clone(), breakage));
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
