use std::fs::{File, OpenOptions};

use nix::fcntl::{Flock, FlockArg};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use mix_core::action::{Abandoned, Action, Failure};
use mix_core::journal::{Record, Recovery, recover};
use mix_core::world::World;
use mix_exec::Scope;

use crate::drive::{Journal, Performer};

pub use mix_core::paths::JOURNAL_DIR;

fn io(path: &Path, error: std::io::Error) -> Failure {
    Failure::Io {
        path: path.to_path_buf(),
        kind: error.kind(),
    }
}

/// A request's journal, held with an exclusive `flock` for as long as the request runs, so a
/// journal nobody holds is one whose request was interrupted.
pub struct FileJournal {
    file: Flock<File>,
    path: PathBuf,
}

impl FileJournal {
    #[allow(clippy::disallowed_methods)]
    pub fn create(dir: &Path, request: &str) -> Result<Self, Failure> {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|error| io(dir, error))?;
        let path = dir.join(format!("{request}.ndjson"));
        let file = OpenOptions::new()
            .append(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(|error| io(&path, error))?;
        let file = Flock::lock(file, FlockArg::LockExclusiveNonblock)
            .map_err(|(_, errno)| io(&path, errno.into()))?;
        let mut journal = Self { file, path };
        journal.append(&Record::Began {
            request: request.to_string(),
        })?;
        Ok(journal)
    }

    #[allow(clippy::disallowed_methods)]
    pub fn finish(self) -> Result<(), Failure> {
        std::fs::remove_file(&self.path).map_err(|error| io(&self.path, error))
    }
}

impl Journal for FileJournal {
    fn append(&mut self, record: &Record) -> Result<(), Failure> {
        let mut line = serde_json::to_vec(record).expect("a record always serialises");
        line.push(b'\n');
        self.file
            .write_all(&line)
            .and_then(|()| self.file.sync_data())
            .map_err(|error| io(&self.path, error))
    }
}

pub fn read(path: &Path) -> Result<Vec<Record>, Failure> {
    let contents = std::fs::read_to_string(path).map_err(|error| io(path, error))?;
    Ok(contents
        .lines()
        .map_while(|line| serde_json::from_str(line).ok())
        .collect())
}

/// Holds `path`'s journal if no running request does.
fn unheld(path: &Path) -> Option<Flock<File>> {
    let file = File::open(path).ok()?;
    Flock::lock(file, FlockArg::LockExclusiveNonblock).ok()
}

/// The requests in `dir` that were interrupted: their journals are there and nothing holds them,
/// with the subjects their recovery still has to put back.
pub fn abandoned(dir: &Path) -> Vec<Abandoned> {
    unfinished(dir)
        .into_iter()
        .filter(|path| unheld(path).is_some())
        .filter_map(|path| {
            let request = path.file_stem()?.to_string_lossy().into_owned();
            let records = read(&path).unwrap_or_default();
            Some(mix_core::journal::abandoned(&request, &records))
        })
        .collect()
}

pub fn unfinished(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "ndjson")
        })
        .collect();
    found.sort();
    found
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Recovered {
    pub requests: usize,
    pub failures: Vec<(Action, Failure)>,
    pub lost: Vec<(Action, Failure)>,
}

fn gone(failure: &Failure) -> bool {
    matches!(
        failure,
        Failure::Conflict { .. }
            | Failure::Io {
                kind: std::io::ErrorKind::NotFound,
                ..
            }
    )
}

fn accept_loss(recovered: &mut Recovered, failed_before: usize) -> bool {
    if recovered.failures[failed_before..]
        .iter()
        .all(|(_, failure)| gone(failure))
    {
        let lost = recovered.failures.split_off(failed_before);
        recovered.lost.extend(lost);
        return true;
    }
    false
}

pub async fn recover_all(dir: &Path, performer: &mut Performer, scope: &Scope) -> Recovered {
    recover_from(dir, performer, scope, true, false).await
}

pub async fn recover_accepting_loss(
    dir: &Path,
    performer: &mut Performer,
    scope: &Scope,
) -> Recovered {
    recover_from(dir, performer, scope, true, true).await
}

pub async fn predict_recovery(dir: &Path, performer: &mut Performer, scope: &Scope) -> Recovered {
    recover_from(dir, performer, scope, false, false).await
}

#[allow(clippy::disallowed_methods)]
async fn recover_from(
    dir: &Path,
    performer: &mut Performer,
    scope: &Scope,
    remove: bool,
    losing: bool,
) -> Recovered {
    let mut recovered = Recovered::default();
    for path in unfinished(dir) {
        let Some(_held) = unheld(&path) else {
            continue;
        };
        let records = match read(&path) {
            Ok(records) => records,
            Err(failure) => {
                recovered.failures.push((Action::Commit, failure));
                continue;
            }
        };
        recovered.requests += 1;
        let resumed = if remove {
            match Resumed::open(&path) {
                Ok(resumed) => Some(resumed),
                Err(failure) => {
                    recovered.failures.push((Action::Commit, failure));
                    continue;
                }
            }
        } else {
            None
        };
        let mut progress: Box<dyn Journal> = match resumed {
            Some(resumed) => Box::new(resumed),
            None => Box::new(Vec::new()),
        };
        let failed_before = recovered.failures.len();
        let finished = recover_records(
            &records,
            performer,
            scope,
            &mut recovered,
            progress.as_mut(),
        )
        .await;
        if !remove || !(finished || losing && accept_loss(&mut recovered, failed_before)) {
            continue;
        }
        if let Err(failure) = std::fs::remove_file(&path).map_err(|error| io(&path, error)) {
            recovered.failures.push((Action::Commit, failure));
        }
    }
    recovered
}

pub async fn recover_records(
    records: &[Record],
    performer: &mut Performer,
    scope: &Scope,
    recovered: &mut Recovered,
    progress: &mut dyn Journal,
) -> bool {
    let failed_before = recovered.failures.len();
    let mut ignore = |_: &[Action]| Ok(());
    let mut quiet = |_| {};
    match recover(records) {
        Recovery::Nothing => {}
        Recovery::RollBack { uncertain, certain } => {
            for action in uncertain {
                if performer
                    .perform(&action, scope, &mut quiet, &mut ignore)
                    .await
                    .is_ok()
                    && let Err(failure) = progress.append(&Record::Reverted {
                        action: action.clone(),
                    })
                {
                    recovered.failures.push((action, failure));
                    return false;
                }
            }
            for action in certain {
                let undone = performer
                    .perform(&action, scope, &mut quiet, &mut ignore)
                    .await
                    .and_then(|_| {
                        progress.append(&Record::Reverted {
                            action: action.clone(),
                        })
                    });
                if let Err(failure) = undone {
                    recovered.failures.push((action, failure));
                }
            }
        }
        Recovery::FinishCommit { pending } => {
            let committed = match performer.adopt(pending, scope).await {
                Ok(()) => {
                    performer
                        .perform(&Action::Commit, scope, &mut quiet, &mut ignore)
                        .await
                }
                Err(failure) => Err(failure),
            };
            if let Err(failure) = committed {
                recovered.failures.push((Action::Commit, failure));
            }
        }
    }
    recovered.failures.len() == failed_before
}

struct Resumed {
    file: File,
    path: PathBuf,
}

impl Resumed {
    #[allow(clippy::disallowed_methods)]
    fn open(path: &Path) -> Result<Self, Failure> {
        let file = OpenOptions::new()
            .append(true)
            .open(path)
            .map_err(|error| io(path, error))?;
        Ok(Self {
            file,
            path: path.to_path_buf(),
        })
    }
}

impl Journal for Resumed {
    fn append(&mut self, record: &Record) -> Result<(), Failure> {
        let mut line = serde_json::to_vec(record).expect("a record always serialises");
        line.push(b'\n');
        self.file
            .write_all(&line)
            .and_then(|()| self.file.sync_data())
            .map_err(|error| io(&self.path, error))
    }
}

struct ModelProgress {
    world: Arc<Mutex<World>>,
    request: String,
}

impl Journal for ModelProgress {
    fn append(&mut self, record: &Record) -> Result<(), Failure> {
        lock(&self.world)
            .logs
            .entry(self.request.clone())
            .or_default()
            .push(record.clone());
        Ok(())
    }
}

pub struct ModelJournal {
    world: Arc<Mutex<World>>,
    request: String,
}

impl ModelJournal {
    pub fn open(world: Arc<Mutex<World>>, request: &str) -> Self {
        {
            let mut locked = lock(&world);
            locked.held.insert(request.to_string());
            locked.logs.insert(
                request.to_string(),
                vec![Record::Began {
                    request: request.to_string(),
                }],
            );
        }
        Self {
            world,
            request: request.to_string(),
        }
    }
}

impl Drop for ModelJournal {
    fn drop(&mut self) {
        let mut world = lock(&self.world);
        world.held.remove(&self.request);
        world.forget(&self.request);
    }
}

pub enum RequestJournal {
    File(FileJournal),
    Model(ModelJournal),
}

impl RequestJournal {
    pub fn finish(self) -> Result<(), Failure> {
        match self {
            RequestJournal::File(journal) => journal.finish(),
            RequestJournal::Model(journal) => {
                lock(&journal.world).logs.remove(&journal.request);
                Ok(())
            }
        }
    }
}

impl Journal for RequestJournal {
    fn append(&mut self, record: &Record) -> Result<(), Failure> {
        match self {
            RequestJournal::File(journal) => journal.append(record),
            RequestJournal::Model(journal) => {
                lock(&journal.world)
                    .logs
                    .entry(journal.request.clone())
                    .or_default()
                    .push(record.clone());
                Ok(())
            }
        }
    }
}

fn lock(world: &Mutex<World>) -> std::sync::MutexGuard<'_, World> {
    world.lock().unwrap_or_else(PoisonError::into_inner)
}

pub async fn recover_model(
    world: &Arc<Mutex<World>>,
    performer: &mut Performer,
    scope: &Scope,
    losing: bool,
) -> Recovered {
    let mut recovered = Recovered::default();
    let open: Vec<(String, Vec<Record>)> = {
        let locked = lock(world);
        locked
            .logs
            .iter()
            .filter(|(request, _)| !locked.held.contains(*request))
            .map(|(request, records)| (request.clone(), records.clone()))
            .collect()
    };
    for (request, records) in open {
        recovered.requests += 1;
        let mut progress = ModelProgress {
            world: Arc::clone(world),
            request: request.clone(),
        };
        let failed_before = recovered.failures.len();
        let finished =
            recover_records(&records, performer, scope, &mut recovered, &mut progress).await;
        if finished || losing && accept_loss(&mut recovered, failed_before) {
            lock(world).logs.remove(&request);
        }
    }
    recovered
}

#[cfg(test)]
mod tests;
