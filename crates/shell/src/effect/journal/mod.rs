use std::fs::{File, OpenOptions};

use nix::fcntl::{Flock, FlockArg};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use mix_core::action::{Action, Failure};
use mix_core::journal::{Record, Recovery, recover};
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

/// The requests in `dir` that were interrupted: their journals are there and nothing holds them.
pub fn abandoned(dir: &Path) -> Vec<String> {
    unfinished(dir)
        .into_iter()
        .filter(|path| unheld(path).is_some())
        .filter_map(|path| Some(path.file_stem()?.to_string_lossy().into_owned()))
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
}

#[allow(clippy::disallowed_methods)]
pub async fn recover_all(dir: &Path, performer: &mut Performer, scope: &Scope) -> Recovered {
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
        let failed_before = recovered.failures.len();
        recovered.requests += 1;
        let mut ignore = |_: &[Action]| Ok(());
        let mut quiet = |_| {};
        match recover(&records) {
            Recovery::Nothing => {}
            Recovery::RollBack { uncertain, certain } => {
                for action in uncertain {
                    let _ = performer
                        .perform(&action, scope, &mut quiet, &mut ignore)
                        .await;
                }
                for action in certain {
                    if let Err(failure) = performer
                        .perform(&action, scope, &mut quiet, &mut ignore)
                        .await
                    {
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
        if recovered.failures.len() > failed_before {
            continue;
        }
        if let Err(failure) = std::fs::remove_file(&path).map_err(|error| io(&path, error)) {
            recovered.failures.push((Action::Commit, failure));
        }
    }
    recovered
}

#[cfg(test)]
mod tests;
