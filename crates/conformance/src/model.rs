use std::sync::{Arc, Mutex, PoisonError};

use mix_core::declared::identity::InvokingUser;
use mix_core::declared::paths::RUNNING_PROGRAM;
use mix_core::declared::policy::Policy;
use mix_core::effect::UserFacts;
use mix_core::model::World;
use mix_events::Render;
use mix_events::v1::command::Request;
use mix_events::v1::{Command, Envelope};
use mix_shell::drive::Faults;
use mix_shell::request::context::Host;
use mix_shell::request::lock::Locks;
use mix_shell::{Caller, Session};
use serde_json::Value;

use crate::suite::FaultKind;

struct Recorded(Arc<Mutex<Vec<Envelope>>>);

impl Render for Recorded {
    fn envelope(&mut self, envelope: Envelope) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(envelope);
    }

    fn detail(&self) -> mix_events::Detail {
        mix_events::Detail::Trace
    }
}

pub fn alice() -> InvokingUser {
    InvokingUser {
        uid: 1000,
        gid: 1000,
        name: "alice".into(),
        home: "/home/alice".into(),
    }
}

pub fn pristine(user: &InvokingUser) -> World {
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
    if let Ok(running) = std::env::current_exe() {
        world.with_file(running, b"mix-daemon", 0o755, (0, 0));
    }
    world.with_file(RUNNING_PROGRAM, b"mix-daemon", 0o755, (0, 0));
    world
}

pub struct Model {
    pub world: Arc<Mutex<World>>,
    dir: tempfile::TempDir,
    locks: Arc<Locks>,
    user: InvokingUser,
    policy: Policy,
}

impl Model {
    pub fn on(world: World, user: InvokingUser, policy: Policy) -> Self {
        let dir = tempfile::tempdir().expect("a temporary directory for the model's locks");
        let locks = Arc::new(Locks::new(dir.path().join("lock")));
        Self {
            world: Arc::new(Mutex::new(world)),
            dir,
            locks,
            user,
            policy,
        }
    }

    pub fn new() -> Self {
        let user = alice();
        Self::on(
            pristine(&user),
            user,
            Policy::new(None, None).expect("the default policy is valid"),
        )
    }

    pub fn snapshot(&self) -> World {
        self.world
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn user(&self) -> &InvokingUser {
        &self.user
    }

    pub async fn run(&self, request: Request, dry_run: bool, as_root: bool) -> Value {
        self.request(request, dry_run, as_root, None)
            .await
            .expect("only an armed fault crashes a request")
    }

    pub async fn faulted(
        &self,
        request: Request,
        as_root: bool,
        at: usize,
        kind: FaultKind,
    ) -> Option<Value> {
        let faults = Arc::new(Faults::default());
        faults.arm(at, kind.fault());
        let done = self
            .request(request, false, as_root, Some(Arc::clone(&faults)))
            .await;
        faults.disarm();
        done
    }

    pub fn fork(&self) -> Self {
        Self::on(self.snapshot(), self.user.clone(), self.policy.clone())
    }

    async fn request(
        &self,
        request: Request,
        dry_run: bool,
        as_root: bool,
        faults: Option<Arc<Faults>>,
    ) -> Option<Value> {
        let recorded: Arc<Mutex<Vec<Envelope>>> = Arc::default();
        let mut session = Session::new(mix_exec::Scope::root())
            .with_host(Host::Model(Arc::clone(&self.world)))
            .with_locks(Arc::clone(&self.locks))
            .with_journals(self.dir.path().join("journal"))
            .with_policy(self.policy.clone())
            .with_caller(Caller::Account {
                peer_is_root: as_root,
                account: Some(self.user.clone()),
            })
            .with_render(Recorded(Arc::clone(&recorded)));
        if let Some(faults) = &faults {
            session = session.with_faults(Arc::clone(faults));
        }
        let ran = mix_shell::request::run(
            &session,
            Command {
                dry_run,
                ..mix_events::command(request)
            },
        );
        match &faults {
            Some(faults) => {
                tokio::select! {
                    () = ran => {}
                    () = faults.crashed() => return None,
                }
            }
            None => ran.await,
        }
        let envelopes = recorded
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        mix_events::validate(envelopes.iter()).expect("every request streams a valid tree");
        let document = mix_render::document::of(&envelopes).expect("every request ends its root");
        Some(serde_json::to_value(document).expect("a document is plain data"))
    }
}

impl Default for Model {
    fn default() -> Self {
        Self::new()
    }
}
