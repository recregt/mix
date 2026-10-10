use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{Content, Entry, World, conflict, describe};
use crate::declared::identity::InvokingUser;
use crate::declared::paths::{
    INDEX_LOCK, MANAGED_FILES, REPOSITORY_BRANCH as BRANCH, REPOSITORY_CONFIG,
    REPOSITORY_CONFIG_CONTENTS, REPOSITORY_HEAD, REPOSITORY_INDEX, mix_state_dir, repository_dir,
};
use crate::effect::{Action, Expect, Failure};

const HEAD_CONTENTS: &[u8] = b"ref: refs/heads/main\n";
const OBJECTS: &str = "objects";

pub type Snapshot = BTreeMap<String, Arc<[u8]>>;

fn digest(bytes: &[u8]) -> String {
    let hash = bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("{hash:016x}")
}

fn listing(staged: &Snapshot) -> String {
    staged
        .iter()
        .map(|(name, bytes)| format!("{name} {}\n", digest(bytes)))
        .collect()
}

impl World {
    fn readable(&self, path: &Path, uid: u32) -> Option<&Arc<[u8]>> {
        let entry = self.files.get(path)?;
        let Content::File(bytes) = &entry.content else {
            return None;
        };
        let allowed = |entry: &Entry, bit: u32| {
            let bits = if entry.owner.0 == uid {
                entry.mode >> 6
            } else {
                entry.mode
            };
            uid == 0 || bits & bit != 0
        };
        let searchable = path
            .ancestors()
            .skip(1)
            .filter_map(|dir| self.files.get(dir))
            .all(|dir| allowed(dir, 0o1));
        (searchable && allowed(entry, 0o4)).then_some(bytes)
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
        self.intact_object(repository, &commit, uid)
            .then_some(commit)
    }

    fn intact_object(&self, repository: &Path, object: &str, uid: u32) -> bool {
        self.readable(&repository.join(OBJECTS).join(object), uid)
            .is_some_and(|bytes| digest(bytes) == object)
    }

    fn reusable_at(&self, repository: &Path, user: &InvokingUser) -> bool {
        let committed = self.snapshot_at(repository, user.uid);
        self.staged(user)
            .iter()
            .filter(|(name, bytes)| {
                committed.as_ref().and_then(|found| found.get(*name)) != Some(*bytes)
            })
            .all(|(_, bytes)| {
                let object = digest(bytes);
                !self
                    .files
                    .contains_key(&repository.join(OBJECTS).join(&object))
                    || self.intact_object(repository, &object, user.uid)
            })
    }

    pub fn indexed(&self, user: &InvokingUser) -> bool {
        let repository = repository_dir(&user.home);
        let index = repository.join(REPOSITORY_INDEX);
        let wanted = listing(&self.staged(user));
        if !self.files.contains_key(&index) {
            return wanted.is_empty();
        }
        self.readable(&index, user.uid)
            .is_some_and(|index| index.as_ref() == wanted.as_bytes())
    }

    pub fn usable(&self, user: &InvokingUser) -> bool {
        let repository = repository_dir(&user.home);
        let index = repository.join(REPOSITORY_INDEX);
        self.readable(&repository.join(REPOSITORY_CONFIG), user.uid)
            .is_some_and(|config| config.as_ref() == REPOSITORY_CONFIG_CONTENTS.as_bytes())
            && (!self.files.contains_key(&index) || self.is_file(&index))
    }

    fn is_file(&self, path: &Path) -> bool {
        matches!(
            self.files.get(path),
            Some(Entry {
                content: Content::File(_),
                ..
            })
        )
    }

    pub fn reusable(&self, user: &InvokingUser) -> bool {
        self.reusable_at(&repository_dir(&user.home), user)
    }

    fn listed(&self, repository: &Path, commit: &str, uid: u32) -> Option<Vec<(String, String)>> {
        let text = self.readable(&repository.join(OBJECTS).join(commit), uid)?;
        std::str::from_utf8(text)
            .ok()?
            .lines()
            .filter(|line| !line.starts_with("parent ") && !line.starts_with("lost "))
            .map(|line| {
                let (name, object) = line.split_once(' ')?;
                MANAGED_FILES
                    .contains(&name)
                    .then(|| (name.to_string(), object.to_string()))
            })
            .collect()
    }

    fn snapshot_at(&self, repository: &Path, uid: u32) -> Option<Snapshot> {
        if self.unborn(repository, uid) {
            return Some(Snapshot::new());
        }
        let commit = self.head_commit(repository, uid)?;
        self.snapshot_of(repository, &commit, uid)
    }

    fn unborn(&self, repository: &Path, uid: u32) -> bool {
        let heads = repository.join("refs/heads");
        self.is_dir(&repository.join(OBJECTS))
            && self.is_dir(&repository.join("refs"))
            && (self.is_dir(&heads) || !self.files.contains_key(&heads))
            && !self.files.contains_key(&repository.join(BRANCH))
            && self
                .readable(&repository.join(REPOSITORY_HEAD), uid)
                .is_some_and(|head| head.as_ref() == HEAD_CONTENTS)
    }

    fn snapshot_of(&self, repository: &Path, commit: &str, uid: u32) -> Option<Snapshot> {
        self.listed(repository, commit, uid)?
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
        self.is_dir(repository) && self.snapshot_at(repository, uid).is_some()
    }

    #[cfg(any(test, feature = "testkit"))]
    pub fn blob_object(user: &InvokingUser, bytes: &[u8]) -> PathBuf {
        repository_dir(&user.home).join(OBJECTS).join(digest(bytes))
    }

    #[cfg(any(test, feature = "testkit"))]
    pub fn branch_object(&self, user: &InvokingUser) -> Option<PathBuf> {
        let repository = repository_dir(&user.home);
        let Content::File(named) = &self.files.get(&repository.join(BRANCH))?.content else {
            return None;
        };
        let commit = std::str::from_utf8(named).ok()?.trim();
        Some(repository.join(OBJECTS).join(commit))
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
        match self.files.get_mut(path) {
            Some(Entry {
                content: Content::File(found),
                mode,
                owner: current_owner,
                ..
            }) if found.as_ref() == bytes => {
                *mode = 0o444;
                *current_owner = owner;
                Ok(())
            }
            Some(_) => Ok(()),
            None => {
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
        self.write_ref(
            &repository.join(REPOSITORY_CONFIG),
            REPOSITORY_CONFIG_CONTENTS.as_bytes(),
            owner,
        )?;
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
        let index = repository.join(REPOSITORY_INDEX);
        if self.files.contains_key(&index) && !self.is_file(&index) {
            return Err(conflict(
                &index,
                "an index",
                describe(self.files.get(&index)),
            ));
        }
        let lock = repository.join(INDEX_LOCK);
        if self.files.contains_key(&lock) {
            return Err(conflict(&lock, "nothing", "a lock another git left"));
        }
        let staged = self.staged(user);
        let owner = (user.uid, user.gid);
        let objects = repository.join(OBJECTS);
        for bytes in staged.values() {
            self.write_object(&objects.join(digest(bytes)), bytes, owner)?;
        }
        self.write_ref(&index, listing(&staged).as_bytes(), owner)?;
        let listing = listing(&staged);
        let head = self.head_commit(&repository, user.uid);
        match &head {
            Some(head) => {
                let current: Vec<(String, String)> = staged
                    .iter()
                    .map(|(name, bytes)| (name.clone(), digest(bytes)))
                    .collect();
                if self.listed(&repository, head, user.uid).as_ref() == Some(&current) {
                    return Ok(false);
                }
            }
            None if staged.is_empty() => return Ok(false),
            None => {}
        }
        let named = self
            .readable(&repository.join(BRANCH), user.uid)
            .and_then(|named| std::str::from_utf8(named).ok())
            .map(|named| named.trim().to_string());
        let mut contents = String::new();
        match (&head, &named) {
            (Some(parent), _) => contents.push_str(&format!("parent {parent}\n")),
            (None, Some(lost)) => contents.push_str(&format!("lost {lost}\n")),
            (None, None) => {}
        }
        contents.push_str(&listing);
        let commit = digest(contents.as_bytes());
        self.write_object(&objects.join(&commit), contents.as_bytes(), owner)?;
        if !self.intact_object(&repository, &commit, user.uid)
            || self.snapshot_of(&repository, &commit, user.uid).as_ref() != Some(&staged)
        {
            return Err(conflict(&objects, "intact objects", "a damaged object"));
        }
        self.make_dir(&repository.join("refs/heads"), owner)?;
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
        let mut undo = Vec::new();
        let recorded = if existed {
            self.own_config(user).map(|restore| undo.extend(restore))
        } else {
            self.init_repository(user)
        };
        let _ = recorded.and_then(|()| self.sync_repository(user));
        if let Some(entry) = self.files.get(&repository)
            && !existed
        {
            undo.push(Action::RemoveCreatedTree {
                path: repository,
                expect: entry.id,
            });
        }
        undo
    }

    fn own_config(&mut self, user: &InvokingUser) -> Result<Vec<Action>, Failure> {
        let repository = repository_dir(&user.home);
        if !self.is_dir(&repository) {
            return Ok(Vec::new());
        }
        let config = repository.join(REPOSITORY_CONFIG);
        let found = self.files.get(&config);
        if let Some(Entry {
            content: Content::File(bytes),
            ..
        }) = found
            && bytes.as_ref() == REPOSITORY_CONFIG_CONTENTS.as_bytes()
        {
            return Ok(Vec::new());
        }
        let expect = found.map_or(Expect::Absent, |entry| Expect::Present(entry.id));
        self.apply(&Action::PutFile {
            path: config,
            contents: Arc::from(REPOSITORY_CONFIG_CONTENTS.as_bytes()),
            mode: 0o644,
            owner: Some((user.uid, user.gid)),
            expect,
        })
        .map(|performed| performed.undo)
    }

    pub(super) fn create_repository(
        &mut self,
        user: &InvokingUser,
    ) -> Result<Vec<Action>, Failure> {
        let repository = repository_dir(&user.home);
        if let Some(found) = self.files.get(&repository) {
            return Err(conflict(&repository, "nothing", found.content_kind()));
        }
        if let Err(failure) = self.init_repository(user) {
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
