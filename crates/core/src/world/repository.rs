use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{Content, Entry, World, conflict, describe};
use crate::action::{Action, Failure};
use crate::identity::InvokingUser;
use crate::paths::{INDEX_LOCK, MANAGED_FILES, REPOSITORY_HEAD, mix_state_dir, repository_dir};

const HEAD_CONTENTS: &[u8] = b"ref: refs/heads/main\n";
const BRANCH: &str = "refs/heads/main";
const OBJECTS: &str = "objects";
const INDEX: &str = "index";

pub type Snapshot = BTreeMap<String, Arc<[u8]>>;

fn digest(bytes: &[u8]) -> String {
    let hash = bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("{hash:016x}")
}

impl World {
    fn readable(&self, path: &Path, uid: u32) -> Option<&Arc<[u8]>> {
        let entry = self.files.get(path)?;
        let Content::File(bytes) = &entry.content else {
            return None;
        };
        let bits = if entry.owner.0 == uid {
            entry.mode >> 6
        } else {
            entry.mode
        };
        (uid == 0 || bits & 0o4 != 0).then_some(bytes)
    }

    fn is_dir(&self, path: &Path) -> bool {
        matches!(
            self.files.get(path),
            Some(Entry {
                content: Content::Directory,
                ..
            })
        )
    }

    fn head_commit(&self, repository: &Path, uid: u32) -> Option<String> {
        if self
            .readable(&repository.join(REPOSITORY_HEAD), uid)?
            .as_ref()
            != HEAD_CONTENTS
        {
            return None;
        }
        let named = self.readable(&repository.join(BRANCH), uid)?;
        let commit = std::str::from_utf8(named).ok()?.trim().to_string();
        self.readable(&repository.join(OBJECTS).join(&commit), uid)?;
        Some(commit)
    }

    fn listed(&self, repository: &Path, commit: &str, uid: u32) -> Option<Vec<(String, String)>> {
        let text = self.readable(&repository.join(OBJECTS).join(commit), uid)?;
        std::str::from_utf8(text)
            .ok()?
            .lines()
            .map(|line| {
                let (name, object) = line.split_once(' ')?;
                MANAGED_FILES
                    .contains(&name)
                    .then(|| (name.to_string(), object.to_string()))
            })
            .collect()
    }

    fn snapshot_at(&self, repository: &Path, uid: u32) -> Option<Snapshot> {
        let commit = self.head_commit(repository, uid)?;
        self.listed(repository, &commit, uid)?
            .into_iter()
            .map(|(name, object)| {
                let bytes = self.readable(&repository.join(OBJECTS).join(&object), uid)?;
                (digest(bytes) == object).then(|| (name, Arc::clone(bytes)))
            })
            .collect()
    }

    pub fn verifies(&self, user: &InvokingUser) -> bool {
        self.verifies_at(&repository_dir(&user.home), user.uid)
    }

    fn verifies_at(&self, repository: &Path, uid: u32) -> bool {
        self.is_dir(repository)
            && self.snapshot_at(repository, uid).is_some()
            && self.readable(&repository.join(INDEX), uid).is_some()
    }

    pub fn repository_at(&self, repository: &Path) -> (bool, Option<Snapshot>) {
        let uid = self.files.get(repository).map_or(0, |entry| entry.owner.0);
        let verifies = self.verifies_at(repository, uid);
        (
            verifies,
            verifies
                .then(|| self.snapshot_at(repository, uid))
                .flatten(),
        )
    }

    pub fn committed(&self, user: &InvokingUser) -> Option<Snapshot> {
        let repository = repository_dir(&user.home);
        if !self.verifies(user) {
            return None;
        }
        self.snapshot_at(&repository, user.uid)
    }

    pub fn staged(&self, user: &InvokingUser) -> Snapshot {
        let state = mix_state_dir(&user.home);
        MANAGED_FILES
            .iter()
            .filter_map(|name| {
                let bytes = self.readable(&state.join(name), user.uid)?;
                Some(((*name).to_string(), Arc::clone(bytes)))
            })
            .collect()
    }

    fn write_object(
        &mut self,
        path: &Path,
        bytes: &[u8],
        owner: (u32, u32),
    ) -> Result<(), Failure> {
        match self.files.get(path) {
            Some(Entry {
                content: Content::File(found),
                ..
            }) if found.as_ref() == bytes => Ok(()),
            Some(Entry {
                content: Content::Directory,
                ..
            }) => Err(conflict(path, "a file", "a directory")),
            _ => {
                self.parent_is_dir(path)?;
                let id = self.fresh();
                self.files.insert(
                    path.to_path_buf(),
                    Entry {
                        content: Content::File(bytes.into()),
                        mode: 0o444,
                        owner,
                        id,
                        changed: id.ino,
                    },
                );
                Ok(())
            }
        }
    }

    fn write_ref(&mut self, path: &Path, bytes: &[u8], owner: (u32, u32)) -> Result<(), Failure> {
        if self.is_dir(path) {
            return Err(conflict(path, "a file", "a directory"));
        }
        self.parent_is_dir(path)?;
        let id = self.fresh();
        self.files.insert(
            path.to_path_buf(),
            Entry {
                content: Content::File(bytes.into()),
                mode: 0o644,
                owner,
                id,
                changed: id.ino,
            },
        );
        Ok(())
    }

    fn make_dir(&mut self, path: &Path, owner: (u32, u32)) -> Result<(), Failure> {
        if self.is_dir(path) {
            return Ok(());
        }
        if let Some(found) = self.files.get(path) {
            return Err(conflict(path, "a directory", describe(Some(found))));
        }
        self.parent_is_dir(path)?;
        let id = self.fresh();
        self.files.insert(
            path.to_path_buf(),
            Entry {
                content: Content::Directory,
                mode: 0o755,
                owner,
                id,
                changed: id.ino,
            },
        );
        Ok(())
    }

    fn init_repository(&mut self, user: &InvokingUser) -> Result<(), Failure> {
        let repository = repository_dir(&user.home);
        let owner = (user.uid, user.gid);
        self.make_dir(&repository, owner)?;
        self.write_ref(&repository.join(REPOSITORY_HEAD), HEAD_CONTENTS, owner)?;
        for dir in [OBJECTS, "refs", "refs/heads"] {
            self.make_dir(&repository.join(dir), owner)?;
        }
        Ok(())
    }

    fn sync_repository(&mut self, user: &InvokingUser) -> Result<bool, Failure> {
        let repository = repository_dir(&user.home);
        if !self.files.contains_key(&repository) {
            return Ok(false);
        }
        if !self.is_dir(&repository)
            || self
                .readable(&repository.join(REPOSITORY_HEAD), user.uid)
                .is_none_or(|head| head.as_ref() != HEAD_CONTENTS)
        {
            return Err(conflict(&repository, "a git repository", "something else"));
        }
        let lock = repository.join(INDEX_LOCK);
        if self.files.contains_key(&lock) {
            return Err(conflict(&lock, "nothing", "a lock another git left"));
        }
        let staged = self.staged(user);
        if staged.is_empty() {
            return Ok(false);
        }
        if self.snapshot_at(&repository, user.uid).as_ref() == Some(&staged) {
            return Ok(false);
        }
        let owner = (user.uid, user.gid);
        let objects = repository.join(OBJECTS);
        let mut listing = String::new();
        for (name, bytes) in &staged {
            let object = digest(bytes);
            self.write_object(&objects.join(&object), bytes, owner)?;
            listing.push_str(&format!("{name} {object}\n"));
        }
        let commit = digest(listing.as_bytes());
        self.write_object(&objects.join(&commit), listing.as_bytes(), owner)?;
        self.write_ref(&repository.join(INDEX), listing.as_bytes(), owner)?;
        self.write_ref(
            &repository.join(BRANCH),
            format!("{commit}\n").as_bytes(),
            owner,
        )?;
        Ok(true)
    }

    pub(super) fn record_state(&mut self, user: &InvokingUser) -> Vec<Action> {
        let repository = repository_dir(&user.home);
        let existed = self.files.contains_key(&repository);
        let recorded = if existed {
            Ok(())
        } else {
            self.init_repository(user)
        };
        let _ = recorded.and_then(|()| self.sync_repository(user));
        match self.files.get(&repository) {
            Some(entry) if !existed => vec![Action::RemoveCreatedTree {
                path: repository,
                expect: entry.id,
            }],
            _ => Vec::new(),
        }
    }

    pub(super) fn create_repository(
        &mut self,
        user: &InvokingUser,
    ) -> Result<Vec<Action>, Failure> {
        let repository = repository_dir(&user.home);
        if let Some(found) = self.files.get(&repository) {
            return Err(conflict(&repository, "nothing", found.content_kind()));
        }
        let created = self
            .init_repository(user)
            .and_then(|()| self.sync_repository(user));
        if let Err(failure) = created {
            for path in self.subtree(&repository) {
                self.files.remove(&path);
            }
            return Err(failure);
        }
        let id = self.files[&repository].id;
        Ok(vec![Action::RemoveCreatedTree {
            path: repository,
            expect: id,
        }])
    }

    pub fn repositories(&self) -> Vec<PathBuf> {
        self.files
            .iter()
            .filter(|(path, entry)| {
                path.file_name().is_some_and(|name| name == ".git")
                    && matches!(entry.content, Content::Directory)
            })
            .map(|(path, _)| path.clone())
            .collect()
    }
}
