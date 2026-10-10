use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use super::{Action, Expect, Failure, FileId, Kind, Owner};

mod accounts;

pub use accounts::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Node {
    pub kind: Kind,
    pub id: FileId,
    pub mode: u32,
    pub owner: Owner,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Spot {
    Blocked(Failure),
    Missing,
    Present(Node),
}

pub trait Ground {
    type Handle;
    fn spot(&self, path: &Path) -> (Spot, Option<Self::Handle>);
    fn running(&self, path: &Path) -> Owner;
}

struct Looking<'g, 'h, G: Ground> {
    ground: &'g G,
    handles: &'h mut Vec<G::Handle>,
}

impl<G: Ground> Looking<'_, '_, G> {
    fn spot(&mut self, path: &Path) -> Spot {
        let (spot, handle) = self.ground.spot(path);
        if let Some(handle) = handle {
            self.handles.push(handle);
        }
        spot
    }

    fn running(&self, path: &Path) -> Owner {
        self.ground.running(path)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Go,
    Done,
    Below(PathBuf),
}

fn conflict(path: &Path, expected: impl Into<String>, found: impl Into<String>) -> Failure {
    Failure::Conflict {
        subject: path.display().to_string(),
        expected: expected.into(),
        found: found.into(),
    }
}

fn not_found(path: &Path) -> Failure {
    Failure::Io {
        path: path.to_path_buf(),
        kind: ErrorKind::NotFound,
    }
}

fn gone(failure: &Failure) -> bool {
    matches!(
        failure,
        Failure::Io {
            kind: ErrorKind::NotFound,
            ..
        }
    )
}

fn unreached(spot: &Spot) -> bool {
    match spot {
        Spot::Missing => true,
        Spot::Blocked(failure) => gone(failure),
        Spot::Present(_) => false,
    }
}

fn absent(path: &Path, spot: Spot) -> Result<(), Failure> {
    match spot {
        Spot::Blocked(failure) => Err(failure),
        Spot::Missing => Ok(()),
        Spot::Present(_) => Err(conflict(path, "nothing", "something already there")),
    }
}

fn holding(path: &Path, expect: FileId, spot: Spot) -> Result<Node, Failure> {
    match spot {
        Spot::Present(node) if node.id == expect => Ok(node),
        Spot::Present(node) => Err(conflict(
            path,
            format!("{expect:?}"),
            format!("{:?}", node.id),
        )),
        Spot::Blocked(failure) if !gone(&failure) => Err(failure),
        Spot::Blocked(_) | Spot::Missing => Err(conflict(path, format!("{expect:?}"), "nothing")),
    }
}

fn node(path: &Path, spot: Spot) -> Result<Node, Failure> {
    match spot {
        Spot::Blocked(failure) => Err(failure),
        Spot::Missing => Err(not_found(path)),
        Spot::Present(node) if node.kind == Kind::Symlink => {
            Err(conflict(path, "no symbolic link", "a symbolic link"))
        }
        Spot::Present(node) => Ok(node),
    }
}

fn source(path: &Path, spot: Spot) -> Result<(), Failure> {
    match spot {
        Spot::Blocked(failure) => Err(failure),
        Spot::Missing => Err(not_found(path)),
        Spot::Present(_) => Ok(()),
    }
}

fn running_as<G: Ground>(
    path: &Path,
    owner: Owner,
    look: &Looking<'_, '_, G>,
) -> Result<(), Failure> {
    let running = look.running(path);
    if running == owner {
        return Ok(());
    }
    Err(conflict(
        path,
        format!("a copy made as {owner:?}"),
        format!("a process running as {running:?}"),
    ))
}

fn created_below<G: Ground>(
    path: &Path,
    look: &mut Looking<'_, '_, G>,
) -> Result<Verdict, Failure> {
    match look.spot(path) {
        Spot::Present(node) if node.kind == Kind::Directory => return Ok(Verdict::Done),
        Spot::Present(_) => return Err(conflict(path, "a directory", "something else")),
        Spot::Blocked(failure) if !gone(&failure) => return Err(failure),
        Spot::Blocked(_) | Spot::Missing => {}
    }
    let mut top = path;
    while let Some(parent) = top.parent()
        && parent.parent().is_some()
        && unreached(&look.spot(parent))
    {
        top = parent;
    }
    absent(top, look.spot(top))?;
    Ok(Verdict::Below(top.to_path_buf()))
}

fn removable(path: &Path, expect: FileId, spot: Spot) -> Result<Verdict, Failure> {
    if unreached(&spot) {
        return Ok(Verdict::Done);
    }
    holding(path, expect, spot).map(|_| Verdict::Go)
}

pub fn precondition<G: Ground>(
    action: &Action,
    ground: &G,
    handles: &mut Vec<G::Handle>,
) -> Result<Verdict, Failure> {
    check(action, &mut Looking { ground, handles })
}

fn check<G: Ground>(action: &Action, look: &mut Looking<'_, '_, G>) -> Result<Verdict, Failure> {
    match action {
        Action::CreateDir { path, .. } => absent(path, look.spot(path))?,
        Action::CreateDirs { path, .. } => return created_below(path, look),
        Action::PutFile { path, expect, .. } => match expect {
            Expect::Absent => absent(path, look.spot(path))?,
            Expect::Present(old) => {
                holding(path, *old, look.spot(path))?;
            }
        },
        Action::SetMode { path, expect, .. } => {
            let found = node(path, look.spot(path))?;
            if found.mode != *expect {
                return Err(conflict(
                    path,
                    format!("mode {expect:o}"),
                    format!("mode {:o}", found.mode),
                ));
            }
        }
        Action::SetOwner { path, expect, .. } => {
            let found = node(path, look.spot(path))?;
            if found.owner != *expect {
                return Err(conflict(
                    path,
                    format!("owner {expect:?}"),
                    format!("owner {:?}", found.owner),
                ));
            }
        }
        Action::SetAside { path, expect } => {
            holding(path, *expect, look.spot(path))?;
        }
        Action::RemoveCreated { path, expect } | Action::RemoveCreatedTree { path, expect } => {
            return removable(path, *expect, look.spot(path));
        }
        Action::Restore { path, from, expect } => {
            let spot = look.spot(path);
            if let Spot::Blocked(failure) = spot {
                return Err(failure);
            }
            if from.parent() != path.parent() {
                return Err(conflict(
                    from,
                    "a sibling of the restored path",
                    "another directory",
                ));
            }
            if from.file_name().is_none() {
                return Err(conflict(from, "a file name", "none"));
            }
            source(from, look.spot(from))?;
            match expect {
                Expect::Absent => absent(path, spot)?,
                Expect::Present(current) => {
                    holding(path, *current, spot)?;
                }
            }
        }
        Action::ReclaimTree {
            path,
            expect,
            owner,
            ..
        } => {
            running_as(path, *owner, look)?;
            holding(path, *expect, look.spot(path))?;
        }
        Action::CopyTree {
            from, to, owner, ..
        } => {
            running_as(to, *owner, look)?;
            source(from, look.spot(from))?;
            absent(to, look.spot(to))?;
        }
        _ => {}
    }
    Ok(Verdict::Go)
}

#[cfg(test)]
mod tests;
