use std::path::PathBuf;
use std::sync::Arc;

use mix_events::v1::{Command, LockWait, envelope, node_started};
use mix_events::{Outbox, Start, Stopped, Tree};
use mix_exec::{Reason, Scope};
use mix_shell::request::lock::{Blocked, Held, Holder, Locks, Need};

struct Request {
    outbox: Arc<Outbox>,
    tree: Tree,
    scope: Scope,
}

fn request() -> Request {
    let outbox = Arc::new(Outbox::new("r", || {}));
    let tree = Tree::new(
        Arc::clone(&outbox),
        Arc::new(|| None),
        Start::command("install", Command::default()),
    );
    Request {
        outbox,
        tree,
        scope: Scope::root(),
    }
}

fn holder(uid: u32, command: &str) -> (Holder, Option<u32>) {
    (
        Holder {
            user: format!("user{uid}"),
            command: command.into(),
        },
        Some(uid),
    )
}

fn no_stop() -> Stopped {
    Arc::new(|| None)
}

async fn acquire(
    locks: &Locks,
    request: &mut Request,
    (holder, uid): (Holder, Option<u32>),
    need: Need,
) -> Result<Held, Blocked> {
    locks
        .acquire(
            holder,
            need,
            uid,
            &mut request.tree,
            &request.scope,
            &no_stop(),
        )
        .await
}

fn waits(outbox: &Outbox) -> Vec<LockWait> {
    outbox
        .drain()
        .into_iter()
        .filter_map(|envelope| match envelope.event {
            Some(envelope::Event::NodeStarted(started)) => match started.kind {
                Some(node_started::Kind::LockWait(wait)) => Some(wait),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

async fn until_waiting(outbox: &Outbox) -> LockWait {
    loop {
        if let Some(wait) = waits(outbox).pop() {
            return wait;
        }
        tokio::task::yield_now().await;
    }
}

fn lock_in(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("lock")
}

#[tokio::test]
async fn shared_holders_hold_together_and_an_exclusive_one_waits_for_both() {
    let dir = tempfile::tempdir().unwrap();
    let locks = Arc::new(Locks::new(lock_in(&dir)));
    let mut first = request();
    let mut second = request();
    let a = acquire(&locks, &mut first, holder(1, "install"), Need::Shared)
        .await
        .unwrap();
    let b = acquire(&locks, &mut second, holder(2, "install"), Need::Shared)
        .await
        .unwrap();
    assert!(waits(&first.outbox).is_empty() && waits(&second.outbox).is_empty());

    let mut third = request();
    let outbox = Arc::clone(&third.outbox);
    let shared = Arc::clone(&locks);
    let exclusive = tokio::spawn(async move {
        acquire(&shared, &mut third, holder(0, "repair"), Need::Exclusive)
            .await
            .map(drop)
            .is_ok()
    });
    let wait = until_waiting(&outbox).await;
    assert_eq!(wait.holder.as_deref(), Some("user1"));
    assert_eq!(wait.command.as_deref(), Some("install"));

    drop(a);
    drop(b);
    assert!(exclusive.await.unwrap());
}

#[tokio::test]
async fn one_user_queues_and_two_users_do_not() {
    let dir = tempfile::tempdir().unwrap();
    let locks = Arc::new(Locks::new(lock_in(&dir)));
    let mut first = request();
    let held = acquire(
        &locks,
        &mut first,
        holder(1, "install"),
        Need::SharedForUser,
    )
    .await
    .unwrap();

    let mut other = request();
    let other_user = acquire(
        &locks,
        &mut other,
        holder(2, "install"),
        Need::SharedForUser,
    )
    .await
    .unwrap();
    assert!(waits(&other.outbox).is_empty());
    drop(other_user);

    let mut same = request();
    let outbox = Arc::clone(&same.outbox);
    let shared = Arc::clone(&locks);
    let queued = tokio::spawn(async move {
        acquire(&shared, &mut same, holder(1, "remove"), Need::SharedForUser)
            .await
            .map(drop)
            .is_ok()
    });
    let wait = until_waiting(&outbox).await;
    assert_eq!(wait.lock, "user user1");
    assert_eq!(wait.command.as_deref(), Some("install"));

    drop(held);
    assert!(queued.await.unwrap());
}

#[tokio::test]
async fn a_cancelled_waiter_leaves_the_lock_free_for_the_next_request() {
    let dir = tempfile::tempdir().unwrap();
    let locks = Arc::new(Locks::new(lock_in(&dir)));
    let mut first = request();
    let held = acquire(&locks, &mut first, holder(0, "repair"), Need::Exclusive)
        .await
        .unwrap();

    let mut waiting = request();
    let outbox = Arc::clone(&waiting.outbox);
    let scope = waiting.scope.clone();
    let shared = Arc::clone(&locks);
    let cancelled = tokio::spawn(async move {
        matches!(
            acquire(&shared, &mut waiting, holder(1, "install"), Need::Shared).await,
            Err(Blocked::Stopped(_))
        )
    });
    until_waiting(&outbox).await;
    scope.cancel(Reason::Interrupted);
    assert!(cancelled.await.unwrap());

    drop(held);
    loop {
        let mut probe = request();
        match locks
            .acquire(
                holder(2, "repair").0,
                Need::Exclusive,
                None,
                &mut probe.tree,
                &probe.scope,
                &no_stop(),
            )
            .await
        {
            Ok(acquired) if waits(&probe.outbox).is_empty() => {
                drop(acquired);
                break;
            }
            Ok(acquired) => drop(acquired),
            Err(_) => unreachable!("an uncancelled request is never stopped"),
        }
    }
}

#[tokio::test]
async fn another_process_reads_the_exclusive_holder_from_the_lock_file() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Locks::new(lock_in(&dir));
    let one_shot = Arc::new(Locks::new(lock_in(&dir)));
    let mut first = request();
    let held = acquire(&daemon, &mut first, holder(0, "bootstrap"), Need::Exclusive)
        .await
        .unwrap();

    let mut waiting = request();
    let outbox = Arc::clone(&waiting.outbox);
    let shared = Arc::clone(&one_shot);
    let task = tokio::spawn(async move {
        acquire(&shared, &mut waiting, holder(1, "install"), Need::Shared)
            .await
            .map(drop)
            .is_ok()
    });
    let wait = until_waiting(&outbox).await;
    assert_eq!(wait.holder.as_deref(), Some("user0"));
    assert_eq!(wait.command.as_deref(), Some("bootstrap"));

    drop(held);
    assert!(task.await.unwrap());
    assert_eq!(std::fs::read_to_string(lock_in(&dir)).unwrap(), "");
}
