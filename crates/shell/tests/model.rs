use std::sync::{Arc, Mutex};

use mix_core::action::UserFacts;
use mix_core::identity::InvokingUser;
use mix_core::paths::RUNNING_PROGRAM;
use mix_core::policy::Policy;
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

struct Machine {
    world: Arc<Mutex<World>>,
    dir: tempfile::TempDir,
    locks: Arc<Locks>,
    user: InvokingUser,
}

impl Machine {
    fn new() -> Self {
        let user = InvokingUser {
            uid: 1000,
            gid: 1000,
            name: "alice".into(),
            home: "/home/alice".into(),
        };
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
