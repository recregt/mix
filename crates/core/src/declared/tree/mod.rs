use std::borrow::Cow;
use std::collections::BTreeMap;
use std::ops::Range;
use std::path::PathBuf;

use crate::declared::targets::Target;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Handle(usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Down,
    Up,
}

#[derive(Debug, Clone)]
pub struct Check<'a> {
    pub target: Target<'a>,
    pub phase: Phase,
    pub after: Vec<usize>,
    pub within: Range<usize>,
}

impl Check<'_> {
    pub fn alone(target: Target<'_>) -> Check<'_> {
        Check {
            target,
            phase: Phase::Down,
            after: Vec::new(),
            within: 0..0,
        }
    }

    pub fn label(&self) -> Cow<'_, str> {
        self.target.label()
    }

    pub fn into_owned(self) -> Check<'static> {
        Check {
            target: self.target.into_owned(),
            phase: self.phase,
            after: self.after,
            within: self.within,
        }
    }

    pub fn waits_on(&self) -> impl Iterator<Item = usize> + '_ {
        self.after.iter().copied().chain(self.within.clone())
    }
}

#[derive(Debug, Clone)]
pub struct Tree<'a> {
    checks: Vec<Check<'a>>,
}

impl<'a> Tree<'a> {
    pub fn checks(&self) -> &[Check<'a>] {
        &self.checks
    }

    pub fn into_checks(self) -> Vec<Check<'a>> {
        self.checks
    }

    pub fn len(&self) -> usize {
        self.checks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.checks.is_empty()
    }
}

struct Draft<'a> {
    down: Option<Target<'a>>,
    up: Vec<Target<'a>>,
    refers: Vec<Handle>,
    parent: Option<Handle>,
    children: Vec<Handle>,
    path: Option<PathBuf>,
}

impl Draft<'_> {
    fn new(down: Option<Target<'_>>, path: Option<PathBuf>) -> Draft<'_> {
        Draft {
            down,
            up: Vec::new(),
            refers: Vec::new(),
            parent: None,
            children: Vec::new(),
            path,
        }
    }
}

pub struct Builder<'a> {
    drafts: Vec<Draft<'a>>,
    paths: BTreeMap<PathBuf, Handle>,
    leftovers: Option<&'static str>,
}

const MACHINE: Handle = Handle(0);
const ACCOUNTS: Handle = Handle(1);
const ROOT: Handle = Handle(2);

impl<'a> Builder<'a> {
    pub fn new(leftovers: Option<&'static str>) -> Self {
        let mut builder = Self {
            drafts: vec![
                Draft::new(None, None),
                Draft::new(None, None),
                Draft::new(None, Some(PathBuf::from("/"))),
            ],
            paths: BTreeMap::new(),
            leftovers,
        };
        builder.drafts[ACCOUNTS.0].parent = Some(MACHINE);
        builder.drafts[ROOT.0].parent = Some(MACHINE);
        builder.paths.insert(PathBuf::from("/"), ROOT);
        builder
    }

    pub fn path(&mut self, path: impl Into<PathBuf>, down: Target<'a>) -> Handle {
        let path = path.into();
        if let Some(&found) = self.paths.get(&path) {
            let draft = &mut self.drafts[found.0];
            if draft.down.is_none() {
                draft.down = Some(down);
            }
            return found;
        }
        let handle = Handle(self.drafts.len());
        self.drafts.push(Draft::new(Some(down), Some(path.clone())));
        self.paths.insert(path, handle);
        handle
    }

    pub fn place(&mut self, path: impl Into<PathBuf>) -> Handle {
        let path = path.into();
        if let Some(&found) = self.paths.get(&path) {
            return found;
        }
        let handle = Handle(self.drafts.len());
        self.drafts.push(Draft::new(None, Some(path.clone())));
        self.paths.insert(path, handle);
        handle
    }

    pub fn account(&mut self, parent: Option<Handle>, down: Target<'a>) -> Handle {
        let handle = Handle(self.drafts.len());
        let mut draft = Draft::new(Some(down), None);
        draft.parent = Some(parent.unwrap_or(ACCOUNTS));
        self.drafts.push(draft);
        handle
    }

    pub fn up(&mut self, at: Handle, check: Target<'a>) {
        self.drafts[at.0].up.push(check);
    }

    pub fn refers(&mut self, from: Handle, to: Handle) {
        self.drafts[from.0].refers.push(to);
    }

    fn link(&mut self) {
        let placed: Vec<(PathBuf, Handle)> = self
            .paths
            .iter()
            .filter(|(_, handle)| **handle != ROOT)
            .map(|(path, handle)| (path.clone(), *handle))
            .collect();
        for (path, handle) in placed {
            let parent = path
                .ancestors()
                .skip(1)
                .find_map(|ancestor| self.paths.get(ancestor).copied())
                .unwrap_or(ROOT);
            self.drafts[handle.0].parent = Some(parent);
        }
        for index in 1..self.drafts.len() {
            if let Some(parent) = self.drafts[index].parent {
                self.drafts[parent.0].children.push(Handle(index));
            }
        }
        for index in 0..self.drafts.len() {
            if index == MACHINE.0 || index == ROOT.0 {
                continue;
            }
            let mut children = std::mem::take(&mut self.drafts[index].children);
            children.sort_by(|left, right| {
                self.drafts[left.0]
                    .path
                    .cmp(&self.drafts[right.0].path)
                    .then(left.cmp(right))
            });
            self.drafts[index].children = children;
        }
    }

    fn mark_leftovers(&mut self) {
        let Some(journals) = self.leftovers else {
            return;
        };
        for index in 0..self.drafts.len() {
            let writes = self.drafts[index].children.iter().any(|child| {
                self.drafts[child.0]
                    .down
                    .as_ref()
                    .is_some_and(Target::writes)
            });
            if let (true, Some(dir)) = (writes, self.drafts[index].path.clone()) {
                self.drafts[index].up.push(Target::Leftovers {
                    dir: Cow::Owned(dir),
                    journals,
                });
            }
        }
    }

    pub fn finish(mut self) -> Tree<'a> {
        self.link();
        self.mark_leftovers();
        let mut down_at: Vec<Option<usize>> = vec![None; self.drafts.len()];
        let mut checks: Vec<Check<'a>> = Vec::with_capacity(self.drafts.len() * 2);
        let mut stack: Vec<(Handle, Option<usize>)> = vec![(MACHINE, None)];
        while let Some((node, opened)) = stack.pop() {
            if let Some(opened) = opened {
                for target in std::mem::take(&mut self.drafts[node.0].up) {
                    checks.push(Check {
                        after: self.referred(node, &down_at),
                        within: opened..checks.len(),
                        target,
                        phase: Phase::Up,
                    });
                }
                continue;
            }
            let opened = checks.len();
            if let Some(target) = self.drafts[node.0].down.take() {
                let mut after: Vec<usize> = self.nearest_down(node, &down_at).into_iter().collect();
                after.extend(self.referred(node, &down_at));
                down_at[node.0] = Some(checks.len());
                checks.push(Check {
                    target,
                    phase: Phase::Down,
                    after,
                    within: 0..0,
                });
            }
            stack.push((node, Some(opened)));
            let children = self.drafts[node.0].children.clone();
            stack.extend(children.into_iter().rev().map(|child| (child, None)));
        }
        Tree { checks }
    }

    fn nearest_down(&self, node: Handle, down_at: &[Option<usize>]) -> Option<usize> {
        let mut at = self.drafts[node.0].parent;
        while let Some(parent) = at {
            if let Some(index) = down_at[parent.0] {
                return Some(index);
            }
            at = self.drafts[parent.0].parent;
        }
        None
    }

    fn referred(&self, node: Handle, down_at: &[Option<usize>]) -> Vec<usize> {
        self.drafts[node.0]
            .refers
            .iter()
            .filter_map(|target| down_at[target.0])
            .collect()
    }
}

#[cfg(test)]
mod tests;
