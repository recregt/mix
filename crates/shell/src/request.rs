use std::sync::Arc;

use mix_core::locks::locks_for;
use mix_core::models::UserConfig;
use mix_core::policy::Policy;
use mix_core::privilege::InvokingUser;
use mix_events::v1::{
    BootstrapRequest, Cancellation, CleanRequest, Code, Command, DoctorRequest, InstallRequest,
    RemoveRequest, RepairRequest, command,
};
use mix_events::{Diagnose, Ending, Fault, Outbox, ROOT, Start, Stopped, Tree};
use mix_exec::Scope;
use tokio::sync::{Notify, oneshot};
use tokio::task::JoinHandle;

use crate::context::{Context, HostConfig, Request};
use crate::drive::stopped_by;
use crate::lock::{Blocked, Holder, Locks};
use crate::ops::bootstrap::Environment;
use crate::ops::clean::Cleaned;
use crate::ops::doctor::HealthReport;
use crate::ops::install::Installed;
use crate::ops::remove::Removed;
use crate::ops::repair::{Repair, RepairReport};
use crate::render::{Render, Shared};

pub struct Locked(());

impl Locked {
    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        Self(())
    }
}

pub enum Caller {
    Account {
        peer_is_root: bool,
        account: Option<InvokingUser>,
    },
    Fixed(Option<UserConfig>),
}

pub struct Session {
    pub scope: Scope,
    pub render: Shared,
    pub host: HostConfig,
    pub locks: Option<Arc<Locks>>,
    pub caller: Caller,
    pub policy: Option<Policy>,
}

impl Session {
    pub fn new(scope: Scope) -> Self {
        Self {
            scope,
            render: crate::render::shared(crate::render::Quiet),
            host: HostConfig::default(),
            locks: None,
            caller: Caller::Fixed(None),
            policy: None,
        }
    }

    pub fn with_render(mut self, render: impl Render + 'static) -> Self {
        self.render = crate::render::shared(render);
        self
    }

    pub fn with_host(mut self, host: HostConfig) -> Self {
        self.host = host;
        self
    }

    pub fn with_locks(mut self, locks: Arc<Locks>) -> Self {
        self.locks = Some(locks);
        self
    }

    pub fn with_caller(mut self, caller: Caller) -> Self {
        self.caller = caller;
        self
    }

    pub fn with_user(self, user: Option<UserConfig>) -> Self {
        self.with_caller(Caller::Fixed(user))
    }

    pub fn with_policy(mut self, policy: Policy) -> Self {
        self.policy = Some(policy);
        self
    }

    fn holder(&self, command: &str) -> (Holder, Option<u32>) {
        let named = match &self.caller {
            Caller::Account {
                account: Some(account),
                ..
            } => Some((account.name.clone(), account.uid)),
            Caller::Fixed(Some(config)) => Some((config.user.name.clone(), config.user.uid)),
            Caller::Account { account: None, .. } | Caller::Fixed(None) => None,
        };
        let (user, uid) = match named {
            Some((user, uid)) => (user, Some(uid)),
            None => ("root".to_string(), None),
        };
        (
            Holder {
                user,
                command: command.to_string(),
            },
            uid,
        )
    }

    fn user(&self, enrolling: bool, locked: &Locked) -> (Option<UserConfig>, bool) {
        match &self.caller {
            Caller::Fixed(config) => (config.clone(), false),
            Caller::Account {
                peer_is_root,
                account,
            } => {
                let config = account.clone().and_then(|account| {
                    if enrolling {
                        crate::profile::user_config_for(account, locked)
                    } else {
                        crate::profile::existing_user_config_for(account, locked)
                    }
                });
                (config, *peer_is_root)
            }
        }
    }
}

pub struct Root {
    pub(crate) tree: Tree,
    pub(crate) stopped: Stopped,
}

pub struct Concluded<T> {
    value: T,
    ending: Ending,
    problems_remain: bool,
}

impl<T> Concluded<T> {
    pub(crate) fn map<U>(self, change: impl FnOnce(T) -> U) -> Concluded<U> {
        Concluded {
            value: change(self.value),
            ending: self.ending,
            problems_remain: self.problems_remain,
        }
    }
}

impl Root {
    pub(crate) fn conclude<T>(&self, ending: Ending, value: T) -> Concluded<T> {
        Concluded {
            value,
            ending,
            problems_remain: false,
        }
    }

    pub(crate) fn conclude_with_problems<T>(
        &self,
        ending: Ending,
        problems_remain: bool,
        value: T,
    ) -> Concluded<T> {
        Concluded {
            value,
            ending,
            problems_remain,
        }
    }

    pub(crate) fn refuse<X, E: Diagnose>(&self, error: E) -> Concluded<Result<X, E>> {
        let ending = match error.fault() {
            Fault::Cancelled { .. } => {
                Ending::cancelled((self.stopped)().unwrap_or(Cancellation::Interrupted))
            }
            fault => Ending::from(fault),
        };
        self.conclude(ending, Err(error))
    }
}

pub trait Unrun {
    fn unrun(blocked: Blocked, command: &str) -> Self;
}

impl<X, E: From<mix_core::Error>> Unrun for Result<X, E> {
    fn unrun(blocked: Blocked, command: &str) -> Self {
        Err(match blocked {
            Blocked::Stopped(_) => mix_core::Error::Cancelled {
                command: format!("mix {command}"),
            },
            Blocked::Failed(error) => error,
        }
        .into())
    }
}

impl Unrun for Repair {
    fn unrun(blocked: Blocked, _command: &str) -> Self {
        match blocked {
            Blocked::Stopped(_) => Repair {
                reports: Vec::new(),
                interrupted: true,
            },
            Blocked::Failed(error) => Repair {
                reports: vec![RepairReport::failed("lock", error)],
                interrupted: false,
            },
        }
    }
}

impl Unrun for Vec<HealthReport> {
    fn unrun(_blocked: Blocked, _command: &str) -> Self {
        Vec::new()
    }
}

struct Delivery {
    close: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

fn pass(outbox: &Outbox, render: &Shared) {
    let mut render = render
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for envelope in outbox.drain() {
        render.envelope(envelope);
    }
}

fn open(render: &Shared) -> (Request, Delivery) {
    let id = crate::context::request_id();
    let notify = Arc::new(Notify::new());
    let wake = Arc::clone(&notify);
    let outbox = Arc::new(Outbox::new(id.clone(), move || wake.notify_one()));
    let (close, mut closed) = oneshot::channel();
    let delivered = Arc::clone(&outbox);
    let render = Arc::clone(render);
    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                _ = &mut closed => {
                    pass(&delivered, &render);
                    return;
                }
                () = notify.notified() => pass(&delivered, &render),
            }
        }
    });
    (Request { id, outbox }, Delivery { close, task })
}

impl Delivery {
    async fn finish(self) {
        let _ = self.close.send(());
        let _ = self.task.await;
    }
}

fn command_of(request: command::Request) -> Command {
    Command {
        mix_version: env!("CARGO_PKG_VERSION").to_string(),
        schema_minor: mix_events::SCHEMA_MINOR,
        request: Some(request),
    }
}

fn ending_of(blocked: &Blocked) -> Ending {
    match blocked {
        Blocked::Stopped(cause) => Ending::cancelled(*cause),
        Blocked::Failed(error) => Ending::from(error.fault()),
    }
}

async fn host<T: Unrun>(
    session: &Session,
    key: &'static str,
    request: command::Request,
    policy: Option<Policy>,
    op: impl AsyncFnOnce(&Context, &mut Root) -> Concluded<T>,
) -> T {
    let (opened, delivery) = open(&session.render);
    let stopped = stopped_by(&session.scope);
    let need = locks_for(&request);
    let enrolling = matches!(request, command::Request::Bootstrap(_));
    let mut tree = Tree::new(
        Arc::clone(&opened.outbox),
        Arc::clone(&stopped),
        Start::command(key, command_of(request)),
    );
    let held = match &session.locks {
        None => None,
        Some(locks) => {
            let (holder, uid) = session.holder(key);
            match locks
                .acquire(holder, need, uid, &mut tree, &session.scope, &stopped)
                .await
            {
                Ok(held) => Some(held),
                Err(blocked) => {
                    let _ = tree.finish(ROOT, ending_of(&blocked).for_root(false));
                    drop(tree);
                    delivery.finish().await;
                    return T::unrun(blocked, key);
                }
            }
        }
    };
    let locked = Locked(());
    let (user, caller_is_root) = session.user(enrolling, &locked);
    let ctx = Context {
        request: opened,
        user,
        caller_is_root,
        scope: session.scope.clone(),
        policy: policy
            .or_else(|| session.policy.clone())
            .unwrap_or_else(stored_policy),
        host: session.host.clone(),
        render: Arc::clone(&session.render),
        locked,
    };
    let mut root = Root { tree, stopped };
    let concluded = op(&ctx, &mut root).await;
    let _ = root
        .tree
        .finish(ROOT, concluded.ending.for_root(concluded.problems_remain));
    drop(root);
    drop(held);
    delivery.finish().await;
    concluded.value
}

#[allow(clippy::disallowed_methods)]
fn stored_policy() -> Policy {
    let stored = std::fs::read_to_string(mix_core::paths::POLICY_FILE).ok();
    Policy::load(stored.as_deref())
}

pub async fn bootstrap(
    session: &Session,
    force: bool,
    mirror: Option<&str>,
    mirror_key: Option<&str>,
) -> crate::ops::bootstrap::Result<Environment> {
    let request = command::Request::Bootstrap(Box::new(BootstrapRequest {
        force,
        mirror: mirror.map(str::to_string),
        mirror_key: mirror.and(mirror_key).map(str::to_string),
    }));
    match Policy::new(mirror, mirror_key) {
        Ok(policy) => {
            host(
                session,
                "bootstrap",
                request,
                Some(policy),
                async |ctx, root| crate::ops::bootstrap::bootstrap(ctx, root, force).await,
            )
            .await
        }
        Err(invalid) => {
            host(session, "bootstrap", request, None, async |_, root| {
                root.refuse(crate::ops::bootstrap::Error::InvalidMirror(
                    invalid.to_string(),
                ))
            })
            .await
        }
    }
}

pub async fn install(
    session: &Session,
    packages: &[String],
) -> crate::profile::change::Result<Installed> {
    let request = command::Request::Install(InstallRequest {
        packages: packages.to_vec(),
    });
    host(session, "install", request, None, async |ctx, root| {
        crate::ops::install::install(ctx, root, packages).await
    })
    .await
}

pub async fn remove(session: &Session, packages: &[String]) -> crate::ops::remove::Result<Removed> {
    let request = command::Request::Remove(RemoveRequest {
        packages: packages.to_vec(),
    });
    host(session, "remove", request, None, async |ctx, root| {
        crate::ops::remove::remove(ctx, root, packages).await
    })
    .await
}

pub async fn clean(session: &Session, all: bool) -> crate::profile::change::Result<Cleaned> {
    let request = command::Request::Clean(CleanRequest { all });
    host(session, "clean", request, None, async |ctx, root| {
        crate::ops::clean::clean(ctx, root, all).await
    })
    .await
}

pub async fn repair(session: &Session) -> Repair {
    let request = command::Request::Repair(RepairRequest {});
    host(session, "repair", request, None, async |ctx, root| {
        crate::ops::repair::repair(ctx, root).await
    })
    .await
}

pub async fn doctor(session: &Session) -> Vec<HealthReport> {
    let request = command::Request::Doctor(DoctorRequest {});
    host(session, "doctor", request, None, async |ctx, root| {
        crate::ops::doctor::audit(ctx, root).await
    })
    .await
}

pub async fn recover(
    locks: &Locks,
    scope: &Scope,
) -> Result<crate::effect::journal::Recovered, mix_core::Error> {
    let mut tree = Tree::new(
        Arc::new(Outbox::new(crate::context::request_id(), || {})),
        Arc::new(|| None),
        Start::command("recover", Command::default()),
    );
    let _held = locks
        .acquire(
            Holder {
                user: "root".to_string(),
                command: "recover".to_string(),
            },
            mix_core::locks::Need::Exclusive,
            None,
            &mut tree,
            scope,
            &(Arc::new(|| None) as Stopped),
        )
        .await
        .map_err(|blocked| match blocked {
            Blocked::Stopped(_) => mix_core::Error::Cancelled {
                command: "mix-daemon serve".to_string(),
            },
            Blocked::Failed(error) => error,
        })?;
    let files =
        crate::effect::files::Files::open(std::path::Path::new("/"), crate::context::request_id())
            .map_err(|source| mix_core::Error::Io {
                path: "/".into(),
                source,
            })?;
    let mut performer = crate::drive::Performer::new(files);
    Ok(crate::effect::journal::recover_all(
        std::path::Path::new(crate::effect::journal::JOURNAL_DIR),
        &mut performer,
        &scope.shielded(),
    )
    .await)
}

pub async fn run(session: &Session, command: Command) {
    match command.request {
        Some(command::Request::Bootstrap(request)) => {
            let _ = bootstrap(
                session,
                request.force,
                request.mirror.as_deref(),
                request.mirror_key.as_deref(),
            )
            .await;
        }
        Some(command::Request::Install(request)) => {
            let _ = install(session, &request.packages).await;
        }
        Some(command::Request::Remove(request)) => {
            let _ = remove(session, &request.packages).await;
        }
        Some(command::Request::Clean(request)) => {
            let _ = clean(session, request.all).await;
        }
        Some(command::Request::Repair(_)) => {
            repair(session).await;
        }
        Some(command::Request::Doctor(_)) => {
            doctor(session).await;
        }
        None => {
            let mut render = session
                .render
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            mix_events::fail(
                Command::default(),
                mix_core::diagnose::failed(Code::Internal, "the request names no command", None),
                &mut **render,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use mix_events::v1::{Envelope, envelope, node_started};

    use super::*;
    use crate::lock::Need;

    struct Recorded(Arc<Mutex<Vec<Envelope>>>);

    impl Render for Recorded {
        fn envelope(&mut self, envelope: Envelope) {
            self.0.lock().unwrap().push(envelope);
        }

        fn detail(&self) -> mix_events::Detail {
            mix_events::Detail::Trace
        }
    }

    fn waiting(recorded: &Mutex<Vec<Envelope>>) -> bool {
        recorded.lock().unwrap().iter().any(|envelope| {
            matches!(
                &envelope.event,
                Some(envelope::Event::NodeStarted(started))
                    if matches!(started.kind, Some(node_started::Kind::LockWait(_)))
            )
        })
    }

    #[tokio::test]
    async fn a_root_caller_is_refused_a_package_change_and_the_stream_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let recorded = Arc::default();
        let session = Session::new(Scope::root())
            .with_locks(Arc::new(Locks::new(dir.path().join("lock"))))
            .with_render(Recorded(Arc::clone(&recorded)))
            .with_caller(Caller::Account {
                peer_is_root: true,
                account: None,
            });

        let refused = install(&session, &["hello".to_string()]).await;

        assert!(matches!(
            refused,
            Err(crate::profile::change::Error::NotRoot)
        ));
        let envelopes = recorded.lock().unwrap();
        assert!(mix_events::validate(envelopes.iter()).is_ok());
    }

    #[tokio::test]
    async fn a_wait_reaches_the_client_while_the_request_is_still_waiting() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lock");
        let holder = Locks::new(&path);
        let outbox = Arc::new(Outbox::new("other", || {}));
        let mut tree = Tree::new(
            Arc::clone(&outbox),
            Arc::new(|| None),
            Start::command("repair", Command::default()),
        );
        let held = holder
            .acquire(
                Holder {
                    user: "root".into(),
                    command: "repair".into(),
                },
                Need::Exclusive,
                None,
                &mut tree,
                &Scope::root(),
                &(Arc::new(|| None) as Stopped),
            )
            .await
            .unwrap();
        let recorded: Arc<Mutex<Vec<Envelope>>> = Arc::default();
        let session = Session::new(Scope::root())
            .with_locks(Arc::new(Locks::new(&path)))
            .with_policy(Policy::default())
            .with_render(Recorded(Arc::clone(&recorded)));
        let doctor = tokio::spawn(async move { doctor(&session).await });

        while !waiting(&recorded) {
            tokio::task::yield_now().await;
        }
        drop(held);

        assert!(!doctor.await.unwrap().is_empty());
        assert!(mix_events::validate(recorded.lock().unwrap().iter()).is_ok());
    }
}
