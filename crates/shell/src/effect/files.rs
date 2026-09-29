use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

use mix_core::action::{
    Action, Expect, Fact, Failure, FileId, Kind, Outcome, Owner, PathFacts, Performed, Query,
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
}

struct Place {
    path: PathBuf,
    dir: OwnedFd,
    name: OsString,
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
        })
    }

    pub fn pending(&self) -> &[PathBuf] {
        &self.pending
    }

    pub fn request(&self) -> &str {
        &self.request
    }

    pub fn tree_owner(&self, path: &Path) -> Option<u32> {
        let parts: Vec<&OsStr> = path
            .components()
            .filter_map(|component| match component {
                Component::Normal(part) => Some(part),
                _ => None,
            })
            .collect();
        let (_, ancestors) = parts.split_last()?;
        let mut dir = sys::openat(
            &self.root,
            ".",
            OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .ok()?;
        for name in ancestors {
            let stat = sys::statx(&dir, *name, AtFlags::SYMLINK_NOFOLLOW, WANTED).ok()?;
            if kind(&stat) != Kind::Directory {
                return None;
            }
            if stat.stx_uid != 0 {
                return Some(stat.stx_uid);
            }
            dir = sys::openat(
                &dir,
                *name,
                OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .ok()?;
        }
        None
    }

    pub fn adopt(&mut self, pending: impl IntoIterator<Item = PathBuf>) {
        self.pending.extend(pending);
    }

    fn place(&self, path: &Path) -> Result<Place, Failure> {
        self.place_with(path, ResolveFlags::NO_SYMLINKS | ResolveFlags::BENEATH)
    }

    fn place_for_reading(&self, path: &Path) -> Result<Place, Failure> {
        let (dir, name) = self.resolve(path, false)?;
        Ok(Place {
            path: path.to_path_buf(),
            dir,
            name,
        })
    }

    fn resolve(&self, path: &Path, follow_last: bool) -> Result<(OwnedFd, OsString), Failure> {
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

    fn expect_id(place: &Place, name: &OsStr, expect: FileId) -> Result<Statx, Failure> {
        match Self::stat(place, name)? {
            Some(stat) if id(&stat) == expect => Ok(stat),
            Some(stat) => Err(conflict(
                &place.path,
                format!("{expect:?}"),
                format!("{:?}", id(&stat)),
            )),
            None => Err(conflict(&place.path, format!("{expect:?}"), "nothing")),
        }
    }

    pub fn perform(&mut self, action: &Action, prepared: &mut Prepared<'_>) -> Option<Outcome> {
        Some(match action {
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
                path,
                expect,
                owner,
                mode,
            } => self.reclaim(path, *expect, *owner, *mode, prepared),
            Action::Commit => self.commit(),
            _ => return None,
        })
    }

    fn create_dir(
        &mut self,
        path: &Path,
        mode: u32,
        owner: Option<Owner>,
        prepared: &mut Prepared<'_>,
    ) -> Outcome {
        let place = self.place(path)?;
        if Self::stat(&place, &place.name)?.is_some() {
            return Err(conflict(path, "nothing", "something already there"));
        }
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
        let place = self.place(path)?;
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
                if let Err(failure) = Self::expect_id(&place, &backup, old) {
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
        let place = self.place(path)?;
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
        let place = self.place(path)?;
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
        let place = self.place(path)?;
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
        if let Err(failure) = Self::expect_id(&place, &aside, expect) {
            let _ = Self::rename(&place, &aside, &place.name, RenameFlags::NOREPLACE);
            return Err(failure);
        }
        Self::sync(&place)?;
        self.pending.push(path.with_file_name(&aside));
        done(undo)
    }

    fn remove_created(&mut self, path: &Path, expect: FileId) -> Outcome {
        let place = self.place(path)?;
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
        let stat = match Self::expect_id(&place, &doomed, expect) {
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
        let place = self.place(path)?;
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
        if let Err(failure) = Self::expect_id(&place, &doomed, expect) {
            let _ = Self::rename(&place, &doomed, &place.name, RenameFlags::NOREPLACE);
            return Err(failure);
        }
        self.remove_tree(&place, &doomed)?;
        Self::sync(&place)?;
        done(Vec::new())
    }

    fn restore(&mut self, path: &Path, from: &Path, expect: Expect) -> Outcome {
        let place = self.place(path)?;
        if from.parent() != path.parent() {
            return Err(conflict(
                from,
                "a sibling of the restored path",
                "another directory",
            ));
        }
        let from_name = from
            .file_name()
            .ok_or_else(|| conflict(from, "a file name", "none"))?
            .to_os_string();
        match expect {
            Expect::Absent => {
                Self::rename(&place, &from_name, &place.name, RenameFlags::NOREPLACE).map_err(
                    |errno| match errno {
                        Errno::EXIST => conflict(path, "nothing", "something already there"),
                        errno => io(path, errno),
                    },
                )?;
            }
            Expect::Present(current) => {
                Self::rename(&place, &from_name, &place.name, RenameFlags::EXCHANGE).map_err(
                    |errno| match errno {
                        Errno::NOENT => conflict(path, format!("{current:?}"), "nothing"),
                        errno => io(path, errno),
                    },
                )?;
                if let Err(failure) = Self::expect_id(&place, &from_name, current) {
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
        owner: Owner,
        mode: u32,
        prepared: &mut Prepared<'_>,
    ) -> Outcome {
        let running = (
            nix::unistd::geteuid().as_raw(),
            nix::unistd::getegid().as_raw(),
        );
        if running != owner {
            return Err(conflict(
                path,
                format!("a copy made as {owner:?}"),
                format!("a process running as {running:?}"),
            ));
        }
        let place = self.place(path)?;
        Self::expect_id(&place, &place.name, expect)?;
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
        if let Err(failure) = Self::expect_id(&place, &aside, expect) {
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
        let running = nix::unistd::geteuid().as_raw();
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
            _ => return None,
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
        let Ok(place) = self.place_for_reading(path) else {
            return missing;
        };
        match Self::stat(&place, &place.name) {
            Ok(Some(stat)) => PathFacts {
                kind: kind(&stat),
                mode: mode(&stat),
                owner: owner_of(&stat),
                id: Some(id(&stat)),
                digest: None,
                changed: Some((stat.stx_mtime.tv_sec, stat.stx_mtime.tv_nsec)),
            },
            _ => missing,
        }
    }

    fn contents(&self, path: &Path) -> Option<Vec<u8>> {
        let (dir, name) = self.resolve(path, true).ok()?;
        let node = sys::openat(
            &dir,
            &name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .ok()?;
        if kind(&statx_fd(&node).ok()?) != Kind::File {
            return None;
        }
        let mut contents = Vec::new();
        std::fs::File::from(node).read_to_end(&mut contents).ok()?;
        Some(contents)
    }
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
