use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use mix_core::error::Error;
use mix_events::v1::{Cancellation, LockWait, node_started};
use mix_events::{Ending, NodeId, ROOT, Start, Stopped, Tree};
use mix_exec::Scope;
use nix::errno::Errno;
use nix::fcntl::{Flock, FlockArg};
use tokio::sync::OwnedMutexGuard;

const LOCK_MODE: u32 = 0o644;
const LOCK_DIR_MODE: u32 = 0o755;

pub use mix_core::locks::Need;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder {
    pub user: String,
    pub command: String,
}

#[derive(Debug)]
pub enum Blocked {
    Stopped(Cancellation),
    Failed(Error),
}

#[derive(Default)]
struct Registry {
    next: u64,
    machine: Vec<(u64, Holder)>,
    users: HashMap<u32, Holder>,
}

pub struct Locks {
    path: PathBuf,
    users: Mutex<HashMap<u32, Arc<tokio::sync::Mutex<()>>>>,
    registry: Arc<Mutex<Registry>>,
    waiting: tokio::sync::watch::Sender<usize>,
}

struct Waiting<'a>(&'a tokio::sync::watch::Sender<usize>);

impl<'a> Waiting<'a> {
    fn begin(count: &'a tokio::sync::watch::Sender<usize>) -> Self {
        count.send_modify(|waiting| *waiting += 1);
        Self(count)
    }
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.0.send_modify(|waiting| *waiting -= 1);
    }
}

pub struct Held {
    flock: Option<Flock<File>>,
    exclusive: bool,
    _user: Option<OwnedMutexGuard<()>>,
    registry: Arc<Mutex<Registry>>,
    ticket: u64,
    uid: Option<u32>,
}

impl Drop for Held {
    fn drop(&mut self) {
        let mut registry = self.registry.lock().unwrap_or_else(PoisonError::into_inner);
        registry
            .machine
            .retain(|(ticket, _)| *ticket != self.ticket);
        if let Some(uid) = self.uid {
            registry.users.remove(&uid);
        }
        drop(registry);
        if self.exclusive
            && let Some(flock) = &mut self.flock
        {
            let _ = flock.set_len(0);
        }
        self.flock.take();
    }
}

impl Locks {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            users: Mutex::new(HashMap::new()),
            registry: Arc::new(Mutex::new(Registry::default())),
            waiting: tokio::sync::watch::Sender::new(0),
        }
    }

    pub fn waiting(&self) -> tokio::sync::watch::Receiver<usize> {
        self.waiting.subscribe()
    }

    fn named_machine_holder(&self, file: &mut File) -> Option<Holder> {
        let registry = self.registry.lock().unwrap_or_else(PoisonError::into_inner);
        registry
            .machine
            .first()
            .map(|(_, holder)| holder.clone())
            .or_else(|| written(file))
    }

    pub async fn acquire(
        &self,
        holder: Holder,
        need: Need,
        uid: Option<u32>,
        tree: &mut Tree,
        scope: &Scope,
        stopped: &Stopped,
    ) -> Result<Held, Blocked> {
        let exclusive = need == Need::Exclusive;
        let file = if need == Need::Observe {
            match open_existing(&self.path).map_err(Blocked::Failed)? {
                Some(file) => file,
                None => {
                    return Ok(Held {
                        flock: None,
                        exclusive: false,
                        _user: None,
                        registry: Arc::clone(&self.registry),
                        ticket: 0,
                        uid: None,
                    });
                }
            }
        } else {
            open(&self.path).map_err(Blocked::Failed)?
        };
        let mut flock = match Flock::lock(file, flock_arg(exclusive, false)) {
            Ok(flock) => flock,
            Err((mut file, Errno::EWOULDBLOCK)) => {
                let named = self.named_machine_holder(&mut file);
                let node = start_wait(
                    tree,
                    "machine-lock",
                    &self.path.display().to_string(),
                    named,
                );
                let (sender, receiver) = tokio::sync::oneshot::channel();
                std::thread::spawn(move || {
                    if let Ok(flock) = Flock::lock(file, flock_arg(exclusive, true)) {
                        let _ = sender.send(flock);
                    }
                });
                let waited = {
                    let _waiting = Waiting::begin(&self.waiting);
                    scope.guard(receiver).await
                };
                match waited {
                    Ok(Ok(flock)) => {
                        let _ = tree.finish(node, Ending::succeeded());
                        flock
                    }
                    Ok(Err(_)) => {
                        let error = Error::Io {
                            path: self.path.clone(),
                            source: std::io::Error::other("waiting for the lock failed"),
                        };
                        let _ =
                            tree.finish(node, Ending::from(mix_events::Diagnose::fault(&error)));
                        return Err(Blocked::Failed(error));
                    }
                    Err(_) => return Err(cancelled(tree, node, stopped)),
                }
            }
            Err((_, errno)) => {
                return Err(Blocked::Failed(Error::Io {
                    path: self.path.clone(),
                    source: errno.into(),
                }));
            }
        };
        if exclusive {
            let _ = write_holder(&mut flock, &holder);
        }
        let user_uid = uid.filter(|_| matches!(need, Need::SharedForUser | Need::Observe));
        let user = if let Some(uid) = user_uid {
            let mutex = Arc::clone(
                self.users
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .entry(uid)
                    .or_default(),
            );
            Some(match Arc::clone(&mutex).try_lock_owned() {
                Ok(guard) => guard,
                Err(_) => {
                    let named = self
                        .registry
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .users
                        .get(&uid)
                        .cloned();
                    let node =
                        start_wait(tree, "user-lock", &format!("user {}", holder.user), named);
                    let waited = {
                        let _waiting = Waiting::begin(&self.waiting);
                        scope.guard(mutex.lock_owned()).await
                    };
                    match waited {
                        Ok(guard) => {
                            let _ = tree.finish(node, Ending::succeeded());
                            guard
                        }
                        Err(_) => return Err(cancelled(tree, node, stopped)),
                    }
                }
            })
        } else {
            None
        };
        let mut registry = self.registry.lock().unwrap_or_else(PoisonError::into_inner);
        registry.next += 1;
        let ticket = registry.next;
        registry.machine.push((ticket, holder.clone()));
        let uid = user.is_some().then_some(user_uid).flatten();
        if let Some(uid) = uid {
            registry.users.insert(uid, holder);
        }
        drop(registry);
        Ok(Held {
            flock: Some(flock),
            exclusive,
            _user: user,
            registry: Arc::clone(&self.registry),
            ticket,
            uid,
        })
    }
}

fn flock_arg(exclusive: bool, wait: bool) -> FlockArg {
    match (exclusive, wait) {
        (true, true) => FlockArg::LockExclusive,
        (true, false) => FlockArg::LockExclusiveNonblock,
        (false, true) => FlockArg::LockShared,
        (false, false) => FlockArg::LockSharedNonblock,
    }
}

fn start_wait(tree: &mut Tree, key: &'static str, lock: &str, holder: Option<Holder>) -> NodeId {
    tree.start(
        ROOT,
        Start::new(
            key,
            node_started::Kind::LockWait(LockWait {
                lock: lock.to_string(),
                holder: holder.as_ref().map(|holder| holder.user.clone()),
                command: holder.map(|holder| holder.command),
            }),
        ),
    )
    .expect("the root is open")
}

fn cancelled(tree: &mut Tree, node: NodeId, stopped: &Stopped) -> Blocked {
    let cause = stopped().unwrap_or(Cancellation::Interrupted);
    let _ = tree.finish(node, Ending::cancelled(cause));
    Blocked::Stopped(cause)
}

fn write_holder(file: &mut File, holder: &Holder) -> std::io::Result<()> {
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    writeln!(
        file,
        "{} {} {}",
        std::process::id(),
        holder.user,
        holder.command
    )?;
    file.flush()
}

fn written(file: &mut File) -> Option<Holder> {
    let mut text = String::new();
    file.seek(SeekFrom::Start(0)).ok()?;
    file.read_to_string(&mut text).ok()?;
    let mut fields = text.split_whitespace();
    let pid: i32 = fields.next()?.parse().ok()?;
    let user = fields.next()?.to_string();
    let command = fields.next()?.to_string();
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).ok()?;
    Some(Holder { user, command })
}

fn open_existing(path: &Path) -> Result<Option<File>, Error> {
    match File::open(path) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Error::Io {
            path: path.to_path_buf(),
            source: error,
        }),
    }
}

#[allow(clippy::disallowed_methods)]
fn open(path: &Path) -> Result<File, Error> {
    let refused = |at: &Path, e: std::io::Error| match e.kind() {
        ErrorKind::PermissionDenied | ErrorKind::ReadOnlyFilesystem => Error::LockMissing {
            path: path.to_path_buf(),
        },
        _ => Error::Io {
            path: at.to_path_buf(),
            source: e,
        },
    };
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty())
        && !parent.exists()
    {
        std::fs::create_dir_all(parent).map_err(|e| refused(parent, e))?;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(LOCK_DIR_MODE))
            .map_err(|e| refused(parent, e))?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| refused(path, e))?;
    file.set_permissions(std::fs::Permissions::from_mode(LOCK_MODE))
        .map_err(|e| refused(path, e))?;
    Ok(file)
}
