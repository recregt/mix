use std::borrow::Cow;
use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

use mix_core::declared::paths::is_leftover;
use mix_core::effect::{
    Action, Expect, Fact, Failure, FileId, Ground, Kind, Node, Outcome, Owner, PathFacts,
    Performed, Query, Spot, Verdict, precondition,
};
use rustix::fs::{
    self as sys, AtFlags, FileType, Gid, Mode, OFlags, RenameFlags, ResolveFlags, Statx,
    StatxFlags, Uid,
};
use rustix::io::Errno;

enum Unrenamed {
    AlreadyGone,
    Other(Failure),
}

use Unrenamed::{AlreadyGone, Other};

pub type Prepared<'a> = dyn FnMut(&[Action]) -> Result<(), Failure> + Send + 'a;

const MAXSYMLINKS: usize = 40;

pub struct Files {
    root: OwnedFd,
    trusted: u32,
    request: String,
    pending: Vec<PathBuf>,
    next: u64,
    seen: std::sync::Mutex<Seen>,
    placed: Vec<Place>,
}

#[derive(Default)]
struct Seen {
    path: OsString,
    dir: Option<OwnedFd>,
    name: OsString,
}

pub struct Pinned {
    _node: OwnedFd,
    dev: u64,
    ino: u64,
    id: FileId,
}

pub struct Place {
    path: PathBuf,
    dir: OwnedFd,
    name: OsString,
    held: Option<Pinned>,
}

fn io(path: &Path, errno: Errno) -> Failure {
    Failure::Io {
        path: path.to_path_buf(),
        kind: std::io::Error::from(errno).kind(),
    }
}

fn conflict(path: &Path, expected: impl Into<String>, found: impl Into<String>) -> Failure {
    Failure::Conflict {
        subject: path.display().to_string(),
        expected: expected.into(),
        found: found.into(),
    }
}

const WANTED: StatxFlags = StatxFlags::BASIC_STATS.union(StatxFlags::BTIME);

fn id(stat: &Statx) -> FileId {
    let born = StatxFlags::from_bits_retain(stat.stx_mask)
        .contains(StatxFlags::BTIME)
        .then_some((stat.stx_btime.tv_sec, stat.stx_btime.tv_nsec));
    FileId {
        dev: (u64::from(stat.stx_dev_major) << 32) | u64::from(stat.stx_dev_minor),
        ino: stat.stx_ino,
        born,
    }
}

fn kind(stat: &Statx) -> Kind {
    match FileType::from_raw_mode(u32::from(stat.stx_mode)) {
        FileType::Directory => Kind::Directory,
        FileType::RegularFile => Kind::File,
        FileType::Symlink => Kind::Symlink,
        _ => Kind::Other,
    }
}

fn mode(stat: &Statx) -> u32 {
    u32::from(stat.stx_mode) & 0o7777
}

fn owner_of(stat: &Statx) -> Owner {
    (stat.stx_uid, stat.stx_gid)
}

fn statx_fd(fd: &OwnedFd) -> Result<Statx, Errno> {
    sys::statx(fd, "", AtFlags::EMPTY_PATH, WANTED)
}

fn ids(owner: Owner) -> (Option<Uid>, Option<Gid>) {
    (Some(Uid::from_raw(owner.0)), Some(Gid::from_raw(owner.1)))
}

fn done(undo: Vec<Action>) -> Outcome {
    Ok(Performed { undo })
}

fn running() -> Owner {
    (
        nix::unistd::geteuid().as_raw(),
        nix::unistd::getegid().as_raw(),
    )
}

impl Ground for Files {
    type Handle = Place;

    fn spot(&self, path: &Path) -> (Spot, Option<Self::Handle>) {
        let place = match self.place(path) {
            Ok(place) => place,
            Err(failure) => return (Spot::Blocked(failure), None),
        };
        let node = match sys::openat(
            &place.dir,
            &place.name,
            OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(node) => node,
            Err(Errno::NOENT) => return (Spot::Missing, Some(place)),
            Err(errno) => return (Spot::Blocked(io(&place.path, errno)), None),
        };
        let stat = match statx_fd(&node) {
            Ok(stat) => stat,
            Err(errno) => return (Spot::Blocked(io(&place.path, errno)), None),
        };
        let spot = Spot::Present(Node {
            kind: kind(&stat),
            id: id(&stat),
            mode: mode(&stat),
            owner: owner_of(&stat),
        });
        let held = Some(Pinned {
            _node: node,
            dev: id(&stat).dev,
            ino: stat.stx_ino,
            id: id(&stat),
        });
        (spot, Some(Place { held, ..place }))
    }

    fn running(&self, _: &Path) -> Owner {
        running()
    }
}

impl Files {
    pub fn open(root: &Path, request: impl Into<String>) -> std::io::Result<Self> {
        Self::open_trusting(root, request, 0)
    }

    pub fn open_trusting(
        root: &Path,
        request: impl Into<String>,
        trusted: u32,
    ) -> std::io::Result<Self> {
        let root = sys::open(
            root,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        Ok(Self {
            root,
            trusted,
            request: request.into(),
            pending: Vec::new(),
            next: 0,
            seen: std::sync::Mutex::new(Seen::default()),
            placed: Vec::new(),
        })
    }

    pub fn pending(&self) -> &[PathBuf] {
        &self.pending
    }

    pub fn request(&self) -> &str {
        &self.request
    }

    pub fn tree_owner(&self, path: &Path) -> Option<u32> {
        walk(&self.root, path)
    }

    pub fn adopt(&mut self, pending: impl IntoIterator<Item = PathBuf>) {
        self.pending.extend(pending);
    }

    fn place(&self, path: &Path) -> Result<Place, Failure> {
        self.place_with(path, ResolveFlags::NO_SYMLINKS | ResolveFlags::BENEATH)
    }

    fn take(&mut self, path: &Path) -> Result<Place, Failure> {
        match self.placed.iter().position(|place| place.path == path) {
            Some(at) => Ok(self.placed.swap_remove(at)),
            None => self.place(path),
        }
    }

    fn place_of(&mut self, path: &Path) -> Result<Option<Place>, Failure> {
        match self.take(path) {
            Ok(place) => Ok(Some(place)),
            Err(Failure::Io {
                kind: std::io::ErrorKind::NotFound,
                ..
            }) => Ok(None),
            Err(failure) => Err(failure),
        }
    }

    fn place_holding(&mut self, path: &Path, expect: FileId) -> Result<Place, Failure> {
        self.place_of(path)?
            .ok_or_else(|| conflict(path, format!("{expect:?}"), "nothing"))
    }

    fn forget(&self) {
        if let Ok(mut seen) = self.seen.lock() {
            seen.dir = None;
        }
    }

    fn remember(&self, path: &Path, dir: OwnedFd, name: &OsStr) {
        if let Ok(mut seen) = self.seen.lock() {
            seen.path.clear();
            seen.path.push(path.as_os_str());
            seen.name.clear();
            seen.name.push(name);
            seen.dir = Some(dir);
        }
    }

    fn open_seen(&self, path: &Path) -> Option<Result<OwnedFd, Errno>> {
        let mut seen = self.seen.lock().ok()?;
        if seen.dir.is_none() || seen.path.as_bytes() != path.as_os_str().as_bytes() {
            return None;
        }
        let dir = seen.dir.take()?;
        Some(sys::openat(
            &dir,
            &seen.name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ))
    }

    fn place_for_reading(&self, path: &Path) -> Result<Place, Failure> {
        let (dir, name) = self.resolve(path, false)?;
        Ok(Place {
            path: path.to_path_buf(),
            dir,
            name: name.into_owned(),
            held: None,
        })
    }

    fn resolve<'p>(
        &self,
        path: &'p Path,
        follow_last: bool,
    ) -> Result<(OwnedFd, Cow<'p, OsStr>), Failure> {
        let bytes = path.as_os_str().as_bytes();
        let Some(slash) = bytes.iter().rposition(|byte| *byte == b'/') else {
            return self.resolve_owned(path, follow_last);
        };
        let name = &bytes[slash + 1..];
        if matches!(name, b"" | b"." | b"..") {
            return self.resolve_owned(path, follow_last);
        }
        let parent = match bytes[..slash].iter().position(|byte| *byte != b'/') {
            Some(start) => OsStr::from_bytes(&bytes[start..slash]),
            None => OsStr::new("."),
        };
        let name = OsStr::from_bytes(name);
        let dir = match sys::openat2(
            &self.root,
            parent,
            OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
            ResolveFlags::NO_SYMLINKS | ResolveFlags::BENEATH,
        ) {
            Ok(dir) => dir,
            Err(Errno::LOOP | Errno::XDEV | Errno::AGAIN) => {
                return self.resolve_owned(path, follow_last);
            }
            Err(errno) => return Err(io(path, errno)),
        };
        if follow_last
            && let Ok(stat) = sys::statx(&dir, name, AtFlags::SYMLINK_NOFOLLOW, WANTED)
            && kind(&stat) == Kind::Symlink
        {
            return self.resolve_owned(path, follow_last);
        }
        Ok((dir, Cow::Borrowed(name)))
    }

    fn resolve_owned<'p>(
        &self,
        path: &Path,
        follow_last: bool,
    ) -> Result<(OwnedFd, Cow<'p, OsStr>), Failure> {
        self.resolve_through_links(path, follow_last)
            .map(|(dir, name)| (dir, Cow::Owned(name)))
    }

    fn resolve_through_links(
        &self,
        path: &Path,
        follow_last: bool,
    ) -> Result<(OwnedFd, OsString), Failure> {
        let open_dir = |parent: &OwnedFd, name: &OsStr| {
            sys::openat(
                parent,
                name,
                OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
        };
        let reopen_root = || {
            sys::openat(
                &self.root,
                ".",
                OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
                Mode::empty(),
            )
        };
        let mut remaining: std::collections::VecDeque<OsString> = std::collections::VecDeque::new();
        for component in path.components() {
            match component {
                Component::RootDir => {}
                Component::Normal(part) => remaining.push_back(part.to_os_string()),
                _ => {
                    return Err(conflict(
                        path,
                        "an absolute, normalised path",
                        "a relative one",
                    ));
                }
            }
        }
        let mut stack = vec![reopen_root().map_err(|errno| io(path, errno))?];
        let mut hops = 0;
        while let Some(name) = remaining.pop_front() {
            if name == ".." {
                if stack.len() > 1 {
                    stack.pop();
                }
                continue;
            }
            let last = remaining.is_empty();
            let dir = stack.last().expect("the root stays on the stack");
            let stat = match sys::statx(dir, &name, AtFlags::SYMLINK_NOFOLLOW, WANTED) {
                Ok(stat) => stat,
                Err(Errno::NOENT) if last => {
                    let dir = stack.pop().expect("the root stays on the stack");
                    return Ok((dir, name));
                }
                Err(errno) => return Err(io(path, errno)),
            };
            if kind(&stat) == Kind::Symlink && (!last || follow_last) {
                if stat.stx_uid != self.trusted {
                    return Err(conflict(
                        path,
                        format!("links owned by uid {}", self.trusted),
                        format!("a link owned by uid {}", stat.stx_uid),
                    ));
                }
                hops += 1;
                if hops > MAXSYMLINKS {
                    return Err(Failure::Io {
                        path: path.to_path_buf(),
                        kind: std::io::Error::from(Errno::LOOP).kind(),
                    });
                }
                let target =
                    sys::readlinkat(dir, &name, Vec::new()).map_err(|errno| io(path, errno))?;
                let target = PathBuf::from(OsStr::from_bytes(target.as_bytes()));
                if target.is_absolute() {
                    stack.truncate(1);
                }
                for part in target.components().rev() {
                    match part {
                        Component::Normal(part) => remaining.push_front(part.to_os_string()),
                        Component::ParentDir => remaining.push_front(OsString::from("..")),
                        _ => {}
                    }
                }
                continue;
            }
            if last {
                let dir = stack.pop().expect("the root stays on the stack");
                return Ok((dir, name));
            }
            let next = open_dir(dir, &name).map_err(|errno| io(path, errno))?;
            stack.push(next);
        }
        Err(conflict(path, "a path below the root", "the root itself"))
    }

    fn place_with(&self, path: &Path, resolve: ResolveFlags) -> Result<Place, Failure> {
        let mut parts = Vec::new();
        for component in path.components() {
            match component {
                Component::RootDir => {}
                Component::Normal(part) => parts.push(part),
                _ => {
                    return Err(conflict(
                        path,
                        "an absolute, normalised path",
                        "a relative one",
                    ));
                }
            }
        }
        let name = parts
            .pop()
            .ok_or_else(|| conflict(path, "a path below the root", "the root itself"))?;
        let parent: PathBuf = parts.iter().collect();
        let parent = if parent.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            parent
        };
        let dir = sys::openat2(
            &self.root,
            &parent,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
            resolve,
        )
        .map_err(|errno| match errno {
            Errno::LOOP => conflict(path, "no symbolic link on the way", "a symbolic link"),
            errno => io(path, errno),
        })?;
        Ok(Place {
            path: path.to_path_buf(),
            dir,
            name: name.to_os_string(),
            held: None,
        })
    }

    fn sibling(&mut self, place: &Place, purpose: &str) -> OsString {
        self.next += 1;
        let mut name = OsString::from(".");
        name.push(&place.name);
        name.push(format!(".mix-{purpose}-{}-{}", self.request, self.next));
        name
    }

    fn stat(place: &Place, name: &OsStr) -> Result<Option<Statx>, Failure> {
        match sys::statx(&place.dir, name, AtFlags::SYMLINK_NOFOLLOW, WANTED) {
            Ok(stat) => Ok(Some(stat)),
            Err(Errno::NOENT) => Ok(None),
            Err(errno) => Err(io(&place.path, errno)),
        }
    }

    fn rename(place: &Place, from: &OsStr, to: &OsStr, flags: RenameFlags) -> Result<(), Errno> {
        sys::renameat_with(&place.dir, from, &place.dir, to, flags)
    }

    fn sync(place: &Place) -> Result<(), Failure> {
        sys::fsync(&place.dir).map_err(|errno| io(&place.path, errno))
    }

    fn open_node(place: &Place, name: &OsStr) -> Result<OwnedFd, Failure> {
        sys::openat(
            &place.dir,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|errno| match errno {
            Errno::LOOP => conflict(&place.path, "no symbolic link", "a symbolic link"),
            errno => io(&place.path, errno),
        })
    }

    fn finish_node(
        place: &Place,
        node: &OwnedFd,
        mode: u32,
        owner: Option<Owner>,
    ) -> Result<FileId, Failure> {
        let failed = |errno| io(&place.path, errno);
        sys::fchmod(node, Mode::from_raw_mode(mode)).map_err(failed)?;
        if let Some(owner) = owner {
            let (uid, gid) = ids(owner);
            sys::fchown(node, uid, gid).map_err(failed)?;
        }
        Ok(id(&statx_fd(node).map_err(failed)?))
    }

    fn write_new(
        &mut self,
        place: &Place,
        purpose: &str,
        contents: &[u8],
        mode: u32,
        owner: Option<Owner>,
    ) -> Result<(OsString, FileId), Failure> {
        let name = self.sibling(place, purpose);
        let file = sys::openat(
            &place.dir,
            &name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|errno| io(&place.path, errno))?;
        let written = (|| {
            let mut writer = std::fs::File::from(file.try_clone()?);
            writer.write_all(contents)?;
            writer.sync_all()
        })();
        let finished = written
            .map_err(|error| Failure::Io {
                path: place.path.clone(),
                kind: error.kind(),
            })
            .and_then(|()| Self::finish_node(place, &file, mode, owner));
        match finished {
            Ok(id) => Ok((name, id)),
            Err(failure) => {
                let _ = sys::unlinkat(&place.dir, &name, AtFlags::empty());
                Err(failure)
            }
        }
    }

    fn pin(place: &mut Place, expect: FileId) -> Result<Option<Pinned>, Failure> {
        let name = &place.name;
        if let Some(held) = place.held.take() {
            if held.id != expect {
                return Err(conflict(
                    &place.path,
                    format!("{expect:?}"),
                    format!("{:?}", held.id),
                ));
            }
            return Ok(Some(held));
        }
        let node = match sys::openat(
            &place.dir,
            name,
            OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(node) => node,
            Err(Errno::NOENT) => return Ok(None),
            Err(errno) => return Err(io(&place.path, errno)),
        };
        let stat = statx_fd(&node).map_err(|errno| io(&place.path, errno))?;
        if id(&stat) != expect {
            return Err(conflict(
                &place.path,
                format!("{expect:?}"),
                format!("{:?}", id(&stat)),
            ));
        }
        Ok(Some(Pinned {
            _node: node,
            dev: id(&stat).dev,
            ino: stat.stx_ino,
            id: id(&stat),
        }))
    }

    fn pinned(place: &mut Place, expect: FileId) -> Result<Pinned, Failure> {
        Self::pin(place, expect)?
            .ok_or_else(|| conflict(&place.path, format!("{expect:?}"), "nothing"))
    }

    fn holds(place: &Place, name: &OsStr, pinned: &Pinned) -> Result<Statx, Failure> {
        match Self::stat(place, name)? {
            Some(stat) if (id(&stat).dev, stat.stx_ino) == (pinned.dev, pinned.ino) => Ok(stat),
            Some(stat) => Err(conflict(
                &place.path,
                format!(
                    "the object checked before the rename (inode {})",
                    pinned.ino
                ),
                format!("inode {}", stat.stx_ino),
            )),
            None => Err(conflict(
                &place.path,
                "the object checked before",
                "nothing",
            )),
        }
    }

    pub fn perform(&mut self, action: &Action, prepared: &mut Prepared<'_>) -> Option<Outcome> {
        self.forget();
        if !matches!(
            action,
            Action::CreateDirs { .. }
                | Action::CreateDir { .. }
                | Action::PutFile { .. }
                | Action::SetMode { .. }
                | Action::SetOwner { .. }
                | Action::SetAside { .. }
                | Action::RemoveCreated { .. }
                | Action::RemoveCreatedTree { .. }
                | Action::Restore { .. }
                | Action::ReclaimTree { .. }
                | Action::CopyTree { .. }
                | Action::Commit
        ) {
            return None;
        }
        let mut placed = std::mem::take(&mut self.placed);
        let checked = precondition(action, self, &mut placed);
        self.placed = placed;
        let verdict = match checked {
            Ok(Verdict::Done) => {
                self.placed.clear();
                return Some(done(Vec::new()));
            }
            Ok(verdict) => verdict,
            Err(failure) => {
                self.placed.clear();
                return Some(Err(failure));
            }
        };
        let outcome = match action {
            Action::CreateDirs { path, mode, owner } => match verdict {
                Verdict::Below(top) => self.create_dirs(&top, path, *mode, *owner, prepared),
                _ => Err(conflict(path, "a top to create", "none")),
            },
            Action::CreateDir { path, mode, owner } => {
                self.create_dir(path, *mode, *owner, prepared)
            }
            Action::PutFile {
                path,
                contents,
                mode,
                owner,
                expect,
            } => self.put_file(path, contents, *mode, *owner, *expect, prepared),
            Action::SetMode { path, mode, expect } => self.set_mode(path, *mode, *expect, prepared),
            Action::SetOwner {
                path,
                owner,
                expect,
            } => self.set_owner(path, *owner, *expect, prepared),
            Action::SetAside { path, expect } => self.set_aside(path, *expect, prepared),
            Action::RemoveCreated { path, expect } => self.remove_created(path, *expect),
            Action::RemoveCreatedTree { path, expect } => self.remove_created_tree(path, *expect),
            Action::Restore { path, from, expect } => self.restore(path, from, *expect),
            Action::ReclaimTree {
                path, expect, mode, ..
            } => self.reclaim(path, *expect, *mode, prepared),
            Action::CopyTree { from, to, mode, .. } => self.copy(from, to, *mode, prepared),
            Action::Commit => self.commit(),
            _ => return None,
        };
        self.placed.clear();
        Some(outcome)
    }

    fn create_dirs(
        &mut self,
        top: &Path,
        path: &Path,
        mode: u32,
        owner: Option<Owner>,
        prepared: &mut Prepared<'_>,
    ) -> Outcome {
        let place = self.take(top)?;
        let staged = self.sibling(&place, "new");
        let built = build_chain(&place.dir, &staged, top, path, mode, owner);
        let discard = |files: &Self| {
            let _ = files.remove_tree(&place, &staged);
        };
        let created = match built.and_then(|()| {
            Self::stat(&place, &staged)?.ok_or_else(|| conflict(top, "the staged tree", "nothing"))
        }) {
            Ok(stat) => id(&stat),
            Err(failure) => {
                discard(self);
                return Err(failure);
            }
        };
        let undo = vec![Action::RemoveCreatedTree {
            path: top.to_path_buf(),
            expect: created,
        }];
        if let Err(failure) = prepared(&undo) {
            discard(self);
            return Err(failure);
        }
        if let Err(errno) = Self::rename(&place, &staged, &place.name, RenameFlags::NOREPLACE) {
            discard(self);
            return Err(match errno {
                Errno::EXIST => conflict(top, "nothing", "something already there"),
                errno => io(top, errno),
            });
        }
        Self::sync(&place)?;
        done(undo)
    }

    fn create_dir(
        &mut self,
        path: &Path,
        mode: u32,
        owner: Option<Owner>,
        prepared: &mut Prepared<'_>,
    ) -> Outcome {
        let place = self.take(path)?;
        let staged = self.sibling(&place, "new");
        sys::mkdirat(&place.dir, &staged, Mode::from_raw_mode(0o700))
            .map_err(|errno| io(path, errno))?;
        let discard = || {
            let _ = sys::unlinkat(&place.dir, &staged, AtFlags::REMOVEDIR);
        };
        let finished = Self::open_node(&place, &staged)
            .and_then(|node| Self::finish_node(&place, &node, mode, owner));
        let id = match finished {
            Ok(id) => id,
            Err(failure) => {
                discard();
                return Err(failure);
            }
        };
        let undo = vec![Action::RemoveCreated {
            path: path.to_path_buf(),
            expect: id,
        }];
        if let Err(failure) = prepared(&undo) {
            discard();
            return Err(failure);
        }
        if let Err(errno) = Self::rename(&place, &staged, &place.name, RenameFlags::NOREPLACE) {
            discard();
            return Err(match errno {
                Errno::EXIST => conflict(path, "nothing", "something already there"),
                errno => io(path, errno),
            });
        }
        Self::sync(&place)?;
        done(undo)
    }

    fn put_file(
        &mut self,
        path: &Path,
        contents: &[u8],
        mode: u32,
        owner: Option<Owner>,
        expect: Expect,
        prepared: &mut Prepared<'_>,
    ) -> Outcome {
        let mut place = self.take(path)?;
        match expect {
            Expect::Absent => {
                let (new, id) = self.write_new(&place, "new", contents, mode, owner)?;
                let undo = vec![Action::RemoveCreated {
                    path: path.to_path_buf(),
                    expect: id,
                }];
                if let Err(failure) = prepared(&undo) {
                    let _ = sys::unlinkat(&place.dir, &new, AtFlags::empty());
                    return Err(failure);
                }
                if let Err(errno) = Self::rename(&place, &new, &place.name, RenameFlags::NOREPLACE)
                {
                    let _ = sys::unlinkat(&place.dir, &new, AtFlags::empty());
                    return Err(match errno {
                        Errno::EXIST => conflict(path, "nothing", "something already there"),
                        errno => io(path, errno),
                    });
                }
                Self::sync(&place)?;
                done(undo)
            }
            Expect::Present(old) => {
                let pinned = Self::pinned(&mut place, old)?;
                let (backup, id) = self.write_new(&place, "backup", contents, mode, owner)?;
                let undo = vec![Action::Restore {
                    path: path.to_path_buf(),
                    from: path.with_file_name(&backup),
                    expect: Expect::Present(id),
                }];
                if let Err(failure) = prepared(&undo) {
                    let _ = sys::unlinkat(&place.dir, &backup, AtFlags::empty());
                    return Err(failure);
                }
                if let Err(errno) =
                    Self::rename(&place, &backup, &place.name, RenameFlags::EXCHANGE)
                {
                    let _ = sys::unlinkat(&place.dir, &backup, AtFlags::empty());
                    return Err(match errno {
                        Errno::NOENT => conflict(path, format!("{old:?}"), "nothing"),
                        errno => io(path, errno),
                    });
                }
                if let Err(failure) = Self::holds(&place, &backup, &pinned) {
                    let _ = Self::rename(&place, &backup, &place.name, RenameFlags::EXCHANGE);
                    let _ = sys::unlinkat(&place.dir, &backup, AtFlags::empty());
                    return Err(failure);
                }
                Self::sync(&place)?;
                self.pending.push(path.with_file_name(&backup));
                done(undo)
            }
        }
    }

    fn set_mode(
        &mut self,
        path: &Path,
        mode: u32,
        expect: u32,
        prepared: &mut Prepared<'_>,
    ) -> Outcome {
        let place = self.take(path)?;
        let node = Self::open_node(&place, &place.name)?;
        let stat = statx_fd(&node).map_err(|errno| io(path, errno))?;
        if self::mode(&stat) != expect {
            return Err(conflict(
                path,
                format!("mode {expect:o}"),
                format!("mode {:o}", self::mode(&stat)),
            ));
        }
        let undo = vec![Action::SetMode {
            path: path.to_path_buf(),
            mode: expect,
            expect: mode,
        }];
        prepared(&undo)?;
        sys::fchmod(&node, Mode::from_raw_mode(mode)).map_err(|errno| io(path, errno))?;
        done(undo)
    }

    fn set_owner(
        &mut self,
        path: &Path,
        owner: Owner,
        expect: Owner,
        prepared: &mut Prepared<'_>,
    ) -> Outcome {
        let place = self.take(path)?;
        let node = Self::open_node(&place, &place.name)?;
        let stat = statx_fd(&node).map_err(|errno| io(path, errno))?;
        if owner_of(&stat) != expect {
            return Err(conflict(
                path,
                format!("owner {expect:?}"),
                format!("owner {:?}", owner_of(&stat)),
            ));
        }
        let undo = vec![Action::SetOwner {
            path: path.to_path_buf(),
            owner: expect,
            expect: owner,
        }];
        prepared(&undo)?;
        let (uid, gid) = ids(owner);
        sys::fchown(&node, uid, gid).map_err(|errno| io(path, errno))?;
        done(undo)
    }

    fn set_aside(&mut self, path: &Path, expect: FileId, prepared: &mut Prepared<'_>) -> Outcome {
        let mut place = self.place_holding(path, expect)?;
        let pinned = Self::pinned(&mut place, expect)?;
        let aside = self.sibling(&place, "aside");
        let undo = vec![Action::Restore {
            path: path.to_path_buf(),
            from: path.with_file_name(&aside),
            expect: Expect::Absent,
        }];
        prepared(&undo)?;
        Self::rename(&place, &place.name, &aside, RenameFlags::NOREPLACE).map_err(|errno| {
            match errno {
                Errno::NOENT => conflict(path, format!("{expect:?}"), "nothing"),
                errno => io(path, errno),
            }
        })?;
        if let Err(failure) = Self::holds(&place, &aside, &pinned) {
            let _ = Self::rename(&place, &aside, &place.name, RenameFlags::NOREPLACE);
            return Err(failure);
        }
        Self::sync(&place)?;
        self.pending.push(path.with_file_name(&aside));
        done(undo)
    }

    fn remove_created(&mut self, path: &Path, expect: FileId) -> Outcome {
        let Some(mut place) = self.place_of(path)? else {
            return done(Vec::new());
        };
        let Some(pinned) = Self::pin(&mut place, expect)? else {
            return done(Vec::new());
        };
        let doomed = self.sibling(&place, "remove");
        let renamed =
            Self::rename(&place, &place.name, &doomed, RenameFlags::NOREPLACE).map_err(|errno| {
                match errno {
                    Errno::NOENT => AlreadyGone,
                    errno => Other(io(path, errno)),
                }
            });
        match renamed {
            Err(AlreadyGone) => return done(Vec::new()),
            Err(Other(failure)) => return Err(failure),
            Ok(()) => {}
        }
        let put_back = || {
            let _ = Self::rename(&place, &doomed, &place.name, RenameFlags::NOREPLACE);
        };
        let stat = match Self::holds(&place, &doomed, &pinned) {
            Ok(stat) => stat,
            Err(failure) => {
                put_back();
                return Err(failure);
            }
        };
        let flags = if kind(&stat) == Kind::Directory {
            AtFlags::REMOVEDIR
        } else {
            AtFlags::empty()
        };
        if let Err(errno) = sys::unlinkat(&place.dir, &doomed, flags) {
            put_back();
            return Err(match errno {
                Errno::NOTEMPTY | Errno::EXIST => {
                    conflict(path, "an empty directory", "a directory with contents")
                }
                errno => io(path, errno),
            });
        }
        Self::sync(&place)?;
        done(Vec::new())
    }

    fn remove_created_tree(&mut self, path: &Path, expect: FileId) -> Outcome {
        let Some(mut place) = self.place_of(path)? else {
            return done(Vec::new());
        };
        let Some(pinned) = Self::pin(&mut place, expect)? else {
            return done(Vec::new());
        };
        let doomed = self.sibling(&place, "remove");
        match Self::rename(&place, &place.name, &doomed, RenameFlags::NOREPLACE) {
            Err(Errno::NOENT) => return done(Vec::new()),
            Err(Errno::XDEV) => return Self::remove_in_place(&place, expect),
            Err(errno) => return Err(io(path, errno)),
            Ok(()) => {}
        }
        if let Err(failure) = Self::holds(&place, &doomed, &pinned) {
            let _ = Self::rename(&place, &doomed, &place.name, RenameFlags::NOREPLACE);
            return Err(failure);
        }
        self.remove_tree(&place, &doomed)?;
        Self::sync(&place)?;
        done(Vec::new())
    }

    fn remove_in_place(place: &Place, expect: FileId) -> Outcome {
        let top = sys::openat(
            &place.dir,
            &place.name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|errno| io(&place.path, errno))?;
        let found = id(&statx_fd(&top).map_err(|errno| io(&place.path, errno))?);
        if found != expect {
            return Err(conflict(
                &place.path,
                format!("{expect:?}"),
                format!("{found:?}"),
            ));
        }
        let names: Vec<OsString> = sys::Dir::read_from(&top)
            .map_err(|errno| io(&place.path, errno))?
            .filter_map(Result::ok)
            .map(|entry| OsStr::from_bytes(entry.file_name().to_bytes()).to_os_string())
            .filter(|entry| entry != "." && entry != "..")
            .collect();
        for name in names {
            remove_tree_at(&top, &name).map_err(|errno| io(&place.path, errno))?;
        }
        sys::unlinkat(&place.dir, &place.name, AtFlags::REMOVEDIR)
            .map_err(|errno| io(&place.path, errno))?;
        Self::sync(place)?;
        done(Vec::new())
    }

    fn copy(&mut self, from: &Path, to: &Path, mode: u32, prepared: &mut Prepared<'_>) -> Outcome {
        if to != from && to.starts_with(from) {
            return Err(conflict(
                to,
                "a place outside the tree being copied",
                "a place inside it",
            ));
        }
        let source = self.take(from)?;
        let place = self.take(to)?;
        let staged = self.sibling(&place, "new");
        let discard = |files: &Self| {
            let _ = files.remove_tree(&place, &staged);
        };
        if let Err(errno) = copy_tree_at(&source.dir, &source.name, &place.dir, &staged, Some(mode))
        {
            discard(self);
            return Err(io(from, errno));
        }
        let copied = match Self::stat(&place, &staged) {
            Ok(Some(stat)) => id(&stat),
            Ok(None) => return Err(conflict(to, "the copy", "nothing")),
            Err(failure) => {
                discard(self);
                return Err(failure);
            }
        };
        let undo = vec![Action::RemoveCreatedTree {
            path: to.to_path_buf(),
            expect: copied,
        }];
        if let Err(failure) = prepared(&undo) {
            discard(self);
            return Err(failure);
        }
        if let Err(errno) = Self::rename(&place, &staged, &place.name, RenameFlags::NOREPLACE) {
            discard(self);
            return Err(io(to, errno));
        }
        Self::sync(&place)?;
        done(undo)
    }

    fn restore(&mut self, path: &Path, from: &Path, expect: Expect) -> Outcome {
        let mut place = self.take(path)?;
        let from_name = from
            .file_name()
            .ok_or_else(|| conflict(from, "a file name", "none"))?
            .to_os_string();
        match expect {
            Expect::Absent => {
                Self::rename(&place, &from_name, &place.name, RenameFlags::NOREPLACE).map_err(
                    |errno| match errno {
                        Errno::EXIST => conflict(path, "nothing", "something already there"),
                        Errno::NOENT => io(from, errno),
                        errno => io(path, errno),
                    },
                )?;
            }
            Expect::Present(current) => {
                let pinned = Self::pinned(&mut place, current)?;
                Self::rename(&place, &from_name, &place.name, RenameFlags::EXCHANGE).map_err(
                    |errno| match errno {
                        Errno::NOENT
                            if Self::stat(&place, &place.name).ok().flatten().is_some() =>
                        {
                            io(from, errno)
                        }
                        Errno::NOENT => conflict(path, format!("{current:?}"), "nothing"),
                        errno => io(path, errno),
                    },
                )?;
                if let Err(failure) = Self::holds(&place, &from_name, &pinned) {
                    let _ = Self::rename(&place, &from_name, &place.name, RenameFlags::EXCHANGE);
                    return Err(failure);
                }
                self.remove_tree(&place, &from_name)?;
            }
        }
        Self::sync(&place)?;
        self.pending.retain(|pending| pending != from);
        done(Vec::new())
    }

    fn reclaim(
        &mut self,
        path: &Path,
        expect: FileId,
        mode: u32,
        prepared: &mut Prepared<'_>,
    ) -> Outcome {
        let mut place = self.place_holding(path, expect)?;
        let pinned = Self::pinned(&mut place, expect)?;
        let staged = self.sibling(&place, "reclaim");
        let discard = |files: &Self| {
            let _ = files.remove_tree(&place, &staged);
        };
        if let Err(errno) = copy_tree_at(&place.dir, &place.name, &place.dir, &staged, Some(mode)) {
            discard(self);
            return Err(io(path, errno));
        }
        let copied = match Self::stat(&place, &staged) {
            Ok(Some(stat)) => id(&stat),
            Ok(None) => return Err(conflict(path, "the copy", "nothing")),
            Err(failure) => {
                discard(self);
                return Err(failure);
            }
        };
        let aside = self.sibling(&place, "aside");
        let undo = vec![
            Action::RemoveCreatedTree {
                path: path.to_path_buf(),
                expect: copied,
            },
            Action::Restore {
                path: path.to_path_buf(),
                from: path.with_file_name(&aside),
                expect: Expect::Absent,
            },
        ];
        if let Err(failure) = prepared(&undo) {
            discard(self);
            return Err(failure);
        }
        if let Err(errno) = Self::rename(&place, &place.name, &aside, RenameFlags::NOREPLACE) {
            discard(self);
            return Err(io(path, errno));
        }
        if let Err(failure) = Self::holds(&place, &aside, &pinned) {
            let _ = Self::rename(&place, &aside, &place.name, RenameFlags::NOREPLACE);
            discard(self);
            return Err(failure);
        }
        if let Err(errno) = Self::rename(&place, &staged, &place.name, RenameFlags::NOREPLACE) {
            let _ = Self::rename(&place, &aside, &place.name, RenameFlags::NOREPLACE);
            discard(self);
            return Err(io(path, errno));
        }
        Self::sync(&place)?;
        done(undo)
    }

    pub fn owner_of(&self, path: &Path) -> Option<u32> {
        let place = self.place_for_reading(path).ok()?;
        Self::stat(&place, &place.name)
            .ok()
            .flatten()
            .map(|stat| stat.stx_uid)
    }

    fn commit(&mut self) -> Outcome {
        let mut first = None;
        let mut kept = Vec::new();
        let running = running().0;
        for pending in std::mem::take(&mut self.pending) {
            let removed = self.place(&pending).and_then(|place| {
                match Self::stat(&place, &place.name)? {
                    Some(stat) if stat.stx_uid != running => {
                        return Err(conflict(
                            &pending,
                            format!("something owned by uid {running}"),
                            format!("something owned by uid {}", stat.stx_uid),
                        ));
                    }
                    _ => {}
                }
                self.remove_tree(&place, &place.name)?;
                Self::sync(&place)
            });
            if let Err(failure) = removed {
                first.get_or_insert(failure);
                kept.push(pending);
            }
        }
        self.pending = kept;
        match first {
            None => done(Vec::new()),
            Some(failure) => Err(failure),
        }
    }

    fn remove_tree(&self, place: &Place, name: &OsStr) -> Result<(), Failure> {
        remove_tree_at(&place.dir, name).map_err(|errno| io(&place.path, errno))
    }

    pub fn observe(&self, query: &Query) -> Option<Fact> {
        Some(match query {
            Query::Path(path) => Fact::Path(self.path_facts(path)),
            Query::Contents(path) => Fact::Contents(self.contents(path).map(Into::into)),
            Query::TreeOwner(path) => Fact::TreeOwner(self.tree_owner(path)),
            Query::Leftovers(dir) => Fact::Leftovers(self.leftovers(dir)),
            Query::Strangers { path, owner } => Fact::Stranger(self.stranger(path, *owner)),
            _ => return None,
        })
    }

    fn open_dir(&self, path: &Path) -> Option<OwnedFd> {
        let (parent, name) = self.resolve(path, false).ok()?;
        sys::openat(
            &parent,
            &*name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .ok()
    }

    /// The siblings in `dir` that an interrupted write of mix's left behind.
    fn leftovers(&self, dir: &Path) -> Vec<(PathBuf, FileId)> {
        let Some(fd) = self.open_dir(dir) else {
            return Vec::new();
        };
        let mut found: Vec<(PathBuf, FileId)> = entries(&fd)
            .into_iter()
            .filter(|name| is_leftover(&name.to_string_lossy()))
            .filter(|name| !self.pending.contains(&dir.join(name)))
            .filter_map(|name| {
                let stat = sys::statx(&fd, &name, AtFlags::SYMLINK_NOFOLLOW, WANTED).ok()?;
                Some((dir.join(&name), id(&stat)))
            })
            .collect();
        found.sort_by(|(a, _), (b, _)| a.cmp(b));
        found
    }

    /// The first entry under `path`, itself included, that is not owned by `owner`.
    fn stranger(&self, path: &Path, owner: Owner) -> Option<(PathBuf, Owner)> {
        let (parent, name) = self.resolve(path, false).ok()?;
        stranger_at(&parent, &name, path, &|found| found != owner)
    }

    /// Whether anything under `path`, itself included, is owned by another user than `uid`.
    pub fn holds_other_than(&self, path: &Path, uid: u32) -> bool {
        self.resolve(path, false)
            .ok()
            .is_some_and(|(parent, name)| {
                stranger_at(&parent, &name, path, &|(found, _)| found != uid).is_some()
            })
    }

    fn path_facts(&self, path: &Path) -> PathFacts {
        let missing = PathFacts {
            kind: Kind::Missing,
            mode: 0,
            owner: (0, 0),
            id: None,
            digest: None,
            changed: None,
        };
        let unreadable = |failure: Failure| match failure {
            Failure::Io {
                kind: std::io::ErrorKind::NotFound,
                ..
            } => missing.clone(),
            Failure::Io { kind, .. } => PathFacts {
                kind: Kind::Unreadable(kind),
                ..missing.clone()
            },
            _ => PathFacts {
                kind: Kind::Unreadable(std::io::ErrorKind::PermissionDenied),
                ..missing.clone()
            },
        };
        let (dir, name) = match self.resolve(path, false) {
            Ok(resolved) => resolved,
            Err(failure) => return unreadable(failure),
        };
        match sys::statx(&dir, &*name, AtFlags::SYMLINK_NOFOLLOW, WANTED) {
            Ok(stat) => {
                let facts = PathFacts {
                    kind: kind(&stat),
                    mode: mode(&stat),
                    owner: owner_of(&stat),
                    id: Some(id(&stat)),
                    digest: None,
                    changed: Some((stat.stx_mtime.tv_sec, stat.stx_mtime.tv_nsec)),
                };
                if facts.kind != Kind::Symlink {
                    self.remember(path, dir, &name);
                }
                facts
            }
            Err(Errno::NOENT) => missing,
            Err(errno) => unreadable(io(path, errno)),
        }
    }

    fn contents(&self, path: &Path) -> Option<Vec<u8>> {
        let node = match self.open_seen(path) {
            Some(node) => node,
            None => {
                let (dir, name) = self.resolve(path, true).ok()?;
                sys::openat(
                    &dir,
                    &*name,
                    OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
            }
        }
        .ok()?;
        if kind(&statx_fd(&node).ok()?) != Kind::File {
            return None;
        }
        let mut contents = Vec::new();
        std::fs::File::from(node).read_to_end(&mut contents).ok()?;
        Some(contents)
    }
}

fn build_chain(
    parent: &OwnedFd,
    staged: &OsStr,
    top: &Path,
    leaf: &Path,
    mode: u32,
    owner: Option<Owner>,
) -> Result<(), Failure> {
    let fail = |errno| io(top, errno);
    let mut names = vec![staged.to_os_string()];
    names.extend(
        leaf.strip_prefix(top)
            .map_err(|_| conflict(leaf, "a path below the created top", "another path"))?
            .components()
            .filter_map(|component| match component {
                Component::Normal(part) => Some(part.to_os_string()),
                _ => None,
            }),
    );
    let mut dirs: Vec<OwnedFd> = Vec::new();
    for name in &names {
        let at = dirs.last().unwrap_or(parent);
        sys::mkdirat(at, name, Mode::from_raw_mode(0o700)).map_err(fail)?;
        let dir = sys::openat(
            at,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(fail)?;
        dirs.push(dir);
    }
    let last = dirs.len() - 1;
    for (index, dir) in dirs.iter().enumerate().rev() {
        if let Some(owner) = owner {
            let (uid, gid) = ids(owner);
            sys::fchown(dir, uid, gid).map_err(fail)?;
        }
        let wanted = if index == last { mode } else { 0o755 };
        sys::fchmod(dir, Mode::from_raw_mode(wanted)).map_err(fail)?;
        sys::fsync(dir).map_err(fail)?;
    }
    Ok(())
}

fn copy_tree_at(
    from_dir: &OwnedFd,
    from: &OsStr,
    to_dir: &OwnedFd,
    to: &OsStr,
    top_mode: Option<u32>,
) -> Result<(), Errno> {
    let stat = sys::statx(from_dir, from, AtFlags::SYMLINK_NOFOLLOW, WANTED)?;
    let mode = Mode::from_raw_mode(top_mode.unwrap_or(self::mode(&stat) & !0o6000));
    match kind(&stat) {
        Kind::Directory => {
            sys::mkdirat(to_dir, to, Mode::from_raw_mode(0o700))?;
            let source = sys::openat(
                from_dir,
                from,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )?;
            let target = sys::openat(
                to_dir,
                to,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )?;
            let names: Vec<OsString> = sys::Dir::read_from(&source)?
                .filter_map(Result::ok)
                .map(|entry| OsStr::from_bytes(entry.file_name().to_bytes()).to_os_string())
                .filter(|entry| entry != "." && entry != "..")
                .collect();
            for name in names {
                copy_tree_at(&source, &name, &target, &name, None)?;
            }
            sys::fchmod(&target, mode)?;
            sys::fsync(&target)
        }
        Kind::File => {
            let source = sys::openat(
                from_dir,
                from,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )?;
            let target = sys::openat(
                to_dir,
                to,
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(0o600),
            )?;
            let mut reader = std::fs::File::from(source);
            let mut writer = std::fs::File::from(target);
            std::io::copy(&mut reader, &mut writer)
                .map_err(|error| Errno::from_io_error(&error).unwrap_or(Errno::IO))?;
            sys::fchmod(&writer, mode)?;
            sys::fsync(&writer)
        }
        Kind::Symlink => {
            let target = sys::readlinkat(from_dir, from, Vec::new())?;
            sys::symlinkat(target.as_c_str(), to_dir, to)
        }
        _ => Err(Errno::OPNOTSUPP),
    }
}

fn entries(dir: &OwnedFd) -> Vec<OsString> {
    let Ok(listing) = sys::Dir::read_from(dir) else {
        return Vec::new();
    };
    listing
        .filter_map(Result::ok)
        .map(|entry| OsStr::from_bytes(entry.file_name().to_bytes()).to_os_string())
        .filter(|entry| entry != "." && entry != "..")
        .collect()
}

fn stranger_at(
    dir: &OwnedFd,
    name: &OsStr,
    path: &Path,
    foreign: &dyn Fn(Owner) -> bool,
) -> Option<(PathBuf, Owner)> {
    let stat = sys::statx(dir, name, AtFlags::SYMLINK_NOFOLLOW, WANTED).ok()?;
    let found = owner_of(&stat);
    if foreign(found) {
        return Some((path.to_path_buf(), found));
    }
    if kind(&stat) != Kind::Directory {
        return None;
    }
    let child = sys::openat(
        dir,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .ok()?;
    entries(&child)
        .into_iter()
        .find_map(|entry| stranger_at(&child, &entry, &path.join(&entry), foreign))
}

fn walk(root: &OwnedFd, path: &Path) -> Option<u32> {
    let mut parts = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part),
            _ => None,
        })
        .peekable();
    let mut dir: Option<OwnedFd> = None;
    while let Some(name) = parts.next() {
        parts.peek()?;
        let parent = dir.as_ref().unwrap_or(root);
        let stat = sys::statx(parent, name, AtFlags::SYMLINK_NOFOLLOW, WANTED).ok()?;
        if kind(&stat) != Kind::Directory {
            return None;
        }
        if stat.stx_uid != 0 {
            return Some(stat.stx_uid);
        }
        dir = Some(
            sys::openat(
                parent,
                name,
                OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .ok()?,
        );
    }
    None
}

fn remove_tree_at(dir: &OwnedFd, name: &OsStr) -> Result<(), Errno> {
    let stat = match sys::statx(dir, name, AtFlags::SYMLINK_NOFOLLOW, WANTED) {
        Ok(stat) => stat,
        Err(Errno::NOENT) => return Ok(()),
        Err(errno) => return Err(errno),
    };
    if kind(&stat) != Kind::Directory {
        return sys::unlinkat(dir, name, AtFlags::empty());
    }
    let child = sys::openat(
        dir,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let names: Vec<OsString> = sys::Dir::read_from(&child)?
        .filter_map(Result::ok)
        .map(|entry| OsStr::from_bytes(entry.file_name().to_bytes()).to_os_string())
        .filter(|entry| entry != "." && entry != "..")
        .collect();
    for entry in names {
        remove_tree_at(&child, &entry)?;
    }
    sys::unlinkat(dir, name, AtFlags::REMOVEDIR)
}

#[cfg(test)]
mod tests;
