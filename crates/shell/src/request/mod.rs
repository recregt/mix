pub mod context;
mod delivery;
pub mod lock;
pub mod sink;

use std::path::PathBuf;
use std::sync::Arc;

use mix_core::identity::InvokingUser;
use mix_core::locks::Need;
use mix_core::locks::locks_for;
use mix_core::policy::Policy;
use mix_core::targets::UserConfig;
use mix_events::v1::{Cancellation, Code, Command, command};
use mix_events::{Diagnose, Ending, Fault, Outbox, ROOT, Start, Stopped, Tree};
use mix_exec::Scope;

use crate::drive::stopped_by;
use crate::request::context::Context;
use crate::request::lock::{Blocked, Holder, Locks};
use crate::request::sink::{Render, Shared};
use delivery::open;

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
    pub locks: Option<Arc<Locks>>,
    pub caller: Caller,
    pub policy: Option<Policy>,
    pub journals: PathBuf,
}

impl Session {
    pub fn new(scope: Scope) -> Self {
        Self {
            scope,
            render: crate::request::sink::shared(crate::request::sink::Quiet),
            locks: None,
            caller: Caller::Fixed(None),
            policy: None,
            journals: PathBuf::from(crate::effect::journal::JOURNAL_DIR),
        }
    }

    pub fn with_journals(mut self, journals: impl Into<PathBuf>) -> Self {
        self.journals = journals.into();
        self
    }

    pub fn with_render(mut self, render: impl Render + 'static) -> Self {
        self.render = crate::request::sink::shared(render);
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

pub struct Concluded {
    ending: Ending,
    problems_remain: bool,
}

impl Root {
    pub(crate) fn conclude(&self, ending: Ending) -> Concluded {
        Concluded {
            ending,
            problems_remain: false,
        }
    }

    pub(crate) fn conclude_with_problems(
        &self,
        ending: Ending,
        problems_remain: bool,
    ) -> Concluded {
        Concluded {
            ending,
            problems_remain,
        }
    }

    pub(crate) fn refuse(&self, error: impl Diagnose) -> Concluded {
        self.conclude(match error.fault() {
            Fault::Cancelled { .. } => {
                Ending::cancelled((self.stopped)().unwrap_or(Cancellation::Interrupted))
            }
            fault => Ending::from(fault),
        })
    }
}

fn command_of(request: command::Request, dry_run: bool) -> Command {
    Command {
        mix_version: env!("CARGO_PKG_VERSION").to_string(),
        schema_minor: mix_events::SCHEMA_MINOR,
        dry_run,
        request: Some(request),
    }
}

fn ending_of(blocked: &Blocked) -> Ending {
    match blocked {
        Blocked::Stopped(cause) => Ending::cancelled(*cause),
        Blocked::Failed(error) => Ending::from(error.fault()),
    }
}

async fn host(
    session: &Session,
    request: &command::Request,
    dry_run: bool,
    policy: Option<Policy>,
    op: impl AsyncFnOnce(&Context, &mut Root) -> Concluded,
) {
    let key = mix_events::key_of(Some(request));
    let (opened, delivery) = open(&session.render);
    let stopped = stopped_by(&session.scope);
    let need = locks_for(request, dry_run);
    let enrolling = matches!(request, command::Request::Bootstrap(_));
    let mut tree = Tree::new(
        Arc::clone(&opened.outbox),
        Arc::clone(&stopped),
        Start::command(key, command_of(request.clone(), dry_run)),
    );
    let held = match (&session.locks, need) {
        (None, _) | (_, Need::Nothing) => None,
        (Some(locks), need) => {
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
                    return;
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
        render: Arc::clone(&session.render),
        locked,
        journals: session.journals.clone(),
        dry_run,
    };
    let mut root = Root { tree, stopped };
    let concluded = op(&ctx, &mut root).await;
    let _ = root
        .tree
        .finish(ROOT, concluded.ending.for_root(concluded.problems_remain));
    drop(root);
    drop(held);
    delivery.finish().await;
}

#[allow(clippy::disallowed_methods)]
fn stored_policy() -> Policy {
    let stored = std::fs::read_to_string(mix_core::paths::POLICY_FILE).ok();
    Policy::load(stored.as_deref())
}

pub async fn recover(
    locks: &Locks,
    scope: &Scope,
) -> Result<crate::effect::journal::Recovered, mix_core::Error> {
    let mut tree = Tree::new(
        Arc::new(Outbox::new(crate::request::context::request_id(), || {})),
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
    let files = crate::effect::files::Files::open(
        std::path::Path::new("/"),
        crate::request::context::request_id(),
    )
    .map_err(|source| mix_core::Error::Io {
        path: "/".into(),
        source,
    })?;
    let mut performer = crate::drive::Performer::new(files).with_profile(
        crate::effect::generations::ProfileContext {
            mirror: stored_policy()
                .mirror()
                .map(|mirror| mirror.url().to_string()),
        },
    );
    Ok(crate::effect::journal::recover_all(
        std::path::Path::new(crate::effect::journal::JOURNAL_DIR),
        &mut performer,
        &scope.shielded(),
    )
    .await)
}

pub async fn run(session: &Session, command: Command) {
    let dry_run = command.dry_run;
    let Some(request) = command.request else {
        let mut render = session
            .render
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        mix_events::fail(
            crate::request::context::request_id(),
            Command::default(),
            mix_events::Fault::failed(Code::Internal, "the request names no command", None),
            &mut **render,
        );
        return;
    };
    match &request {
        command::Request::Bootstrap(bootstrap) => {
            match Policy::new(bootstrap.mirror.as_deref(), bootstrap.mirror_key.as_deref()) {
                Ok(policy) => {
                    host(
                        session,
                        &request,
                        dry_run,
                        Some(policy),
                        async |ctx, root| {
                            crate::ops::bootstrap::bootstrap(ctx, root, bootstrap).await
                        },
                    )
                    .await
                }
                Err(invalid) => {
                    host(session, &request, dry_run, None, async |_, root| {
                        root.refuse(crate::ops::bootstrap::Error::InvalidMirror(
                            invalid.to_string(),
                        ))
                    })
                    .await
                }
            }
        }
        command::Request::Install(install) => {
            host(session, &request, dry_run, None, async |ctx, root| {
                crate::ops::install::install(ctx, root, install).await
            })
            .await
        }
        command::Request::Remove(remove) => {
            host(session, &request, dry_run, None, async |ctx, root| {
                crate::ops::remove::remove(ctx, root, remove).await
            })
            .await
        }
        command::Request::Clean(clean) => {
            host(session, &request, dry_run, None, async |ctx, root| {
                crate::ops::clean::clean(ctx, root, clean).await
            })
            .await
        }
        command::Request::Repair(repair) => {
            host(session, &request, dry_run, None, async |ctx, root| {
                crate::ops::repair::repair(ctx, root, repair).await
            })
            .await
        }
        command::Request::Doctor(doctor) => {
            host(session, &request, dry_run, None, async |ctx, root| {
                crate::ops::doctor::audit(ctx, root, doctor).await
            })
            .await
        }
        command::Request::Explain(explain) => {
            host(session, &request, dry_run, None, async |ctx, root| {
                crate::ops::explain::explain(ctx, root, explain).await
            })
            .await
        }
    }
}

#[cfg(test)]
pub(crate) mod ran {
    use std::sync::Mutex;

    use mix_events::v1::{Envelope, NodeFinished, envelope, node_finished};

    use super::*;

    pub(crate) struct Recorded(pub(crate) Arc<Mutex<Vec<Envelope>>>);

    impl Render for Recorded {
        fn envelope(&mut self, envelope: Envelope) {
            self.0.lock().unwrap().push(envelope);
        }

        fn detail(&self) -> mix_events::Detail {
            mix_events::Detail::Trace
        }
    }

    pub(crate) struct Ran(pub(crate) Vec<Envelope>);

    impl Ran {
        pub(crate) fn root(&self) -> &NodeFinished {
            self.0
                .iter()
                .find_map(|envelope| match &envelope.event {
                    Some(envelope::Event::NodeFinished(finished)) if finished.id == ROOT => {
                        Some(finished)
                    }
                    _ => None,
                })
                .expect("every request ends its root")
        }

        pub(crate) fn result(&self) -> Option<&node_finished::Result> {
            self.root().result.as_ref()
        }

        pub(crate) fn code(&self) -> Option<Code> {
            self.root()
                .diagnostic
                .as_ref()
                .map(|diagnostic| diagnostic.code())
        }
    }

    pub(crate) async fn ran(session: Session, request: command::Request) -> Ran {
        let recorded: Arc<Mutex<Vec<Envelope>>> = Arc::default();
        let session = session.with_render(Recorded(Arc::clone(&recorded)));
        run(&session, mix_events::command(request)).await;
        let envelopes = std::mem::take(&mut *recorded.lock().unwrap());
        mix_events::validate(envelopes.iter()).expect("every request streams a valid tree");
        Ran(envelopes)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use mix_events::v1::{Envelope, envelope, node_started};

    use super::*;
    use crate::request::lock::Need;

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
        let session = Session::new(Scope::root())
            .with_locks(Arc::new(Locks::new(dir.path().join("lock"))))
            .with_caller(Caller::Account {
                peer_is_root: true,
                account: None,
            });

        let ran = ran::ran(
            session,
            command::Request::Install(mix_events::v1::InstallRequest {
                packages: vec!["hello".to_string()],
            }),
        )
        .await;

        assert_eq!(ran.code(), Some(Code::RootNotAllowed));
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
            .with_render(ran::Recorded(Arc::clone(&recorded)));
        let doctor = tokio::spawn(async move {
            run(
                &session,
                mix_events::command(command::Request::Doctor(mix_events::v1::DoctorRequest {})),
            )
            .await
        });

        while !waiting(&recorded) {
            tokio::task::yield_now().await;
        }
        drop(held);

        doctor.await.unwrap();
        let envelopes = recorded.lock().unwrap();
        assert!(mix_events::validate(envelopes.iter()).is_ok());
        assert!(matches!(
            ran::Ran(envelopes.clone()).result(),
            Some(mix_events::v1::node_finished::Result::Doctor(_))
        ));
    }
}
