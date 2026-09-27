use std::borrow::Cow;
use std::collections::BTreeSet;

use crate::escape::NulByte;
use crate::ident::{FileName, Ident, is_identifier, is_identifier_bytes};
use crate::print;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Key {
    text: Cow<'static, str>,
    bare: bool,
}

impl Key {
    pub fn new(s: impl Into<String>) -> Result<Self, NulByte> {
        let s = s.into();
        if s.contains('\0') {
            Err(NulByte)
        } else {
            let bare = is_identifier(&s);
            Ok(Self {
                text: Cow::Owned(s),
                bare,
            })
        }
    }

    pub const fn new_static(s: &'static str) -> Self {
        assert!(!contains_nul(s.as_bytes()), "a key cannot hold a null byte");
        Self {
            text: Cow::Borrowed(s),
            bare: is_identifier_bytes(s.as_bytes()),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub(crate) fn is_bare(&self) -> bool {
        self.bare
    }
}

impl From<&Ident> for Key {
    fn from(ident: &Ident) -> Self {
        Self {
            text: ident.0.clone(),
            bare: true,
        }
    }
}

impl From<Ident> for Key {
    fn from(ident: Ident) -> Self {
        Self {
            text: ident.0,
            bare: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NixStr(Cow<'static, str>);

impl NixStr {
    pub fn new(s: impl Into<String>) -> Result<Self, NulByte> {
        let s = s.into();
        if s.contains('\0') {
            Err(NulByte)
        } else {
            Ok(Self(Cow::Owned(s)))
        }
    }

    pub const fn new_static(s: &'static str) -> Self {
        assert!(
            !contains_nul(s.as_bytes()),
            "a nix string cannot hold a null byte"
        );
        Self(Cow::Borrowed(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Verbatim<'a>(&'a str);

impl<'a> Verbatim<'a> {
    pub const fn new_static(s: &'static str) -> Verbatim<'static> {
        assert!(
            is_verbatim(s.as_bytes()),
            "text that a nix string would escape"
        );
        Verbatim(s)
    }

    pub fn new(s: &'a str) -> Option<Self> {
        is_verbatim(s.as_bytes()).then_some(Self(s))
    }

    pub(crate) fn unchecked(s: &'a str) -> Self {
        debug_assert!(is_verbatim(s.as_bytes()));
        Self(s)
    }

    pub fn as_str(self) -> &'a str {
        self.0
    }
}

pub(crate) const fn is_verbatim(bytes: &[u8]) -> bool {
    let mut i = 0;
    while i < bytes.len() {
        if matches!(bytes[i], b'"' | b'\\' | b'\r' | b'$' | 0) {
            return false;
        }
        i += 1;
    }
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RelPath(Cow<'static, str>);

impl RelPath {
    pub fn new(s: impl Into<String>) -> Result<Self, InvalidRelPath> {
        let s = s.into();
        if is_rel_path_bytes(s.as_bytes()) {
            Ok(Self(Cow::Owned(s)))
        } else {
            Err(InvalidRelPath(s))
        }
    }

    pub const fn new_static(s: &'static str) -> Self {
        assert!(is_rel_path_bytes(s.as_bytes()), "not a valid relative path");
        Self(Cow::Borrowed(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&FileName> for RelPath {
    fn from(name: &FileName) -> Self {
        Self(Cow::Owned(name.as_str().to_owned()))
    }
}

#[derive(Debug, thiserror::Error)]
#[error("`{0}` is not a relative path mix can write")]
pub struct InvalidRelPath(String);

const fn contains_nul(bytes: &[u8]) -> bool {
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0 {
            return true;
        }
        i += 1;
    }
    false
}

const fn is_rel_path_bytes(bytes: &[u8]) -> bool {
    if bytes.is_empty() || bytes[0] == b'.' {
        return false;
    }
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if !(b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.') {
            return false;
        }
        i += 1;
    }
    true
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct AttrSet(Vec<(Key, Expr)>);

impl AttrSet {
    pub fn new() -> Self {
        Self(Vec::new())
    }

    pub fn insert(&mut self, key: Key, value: Expr) -> Option<Expr> {
        match self.0.binary_search_by(|(k, _)| k.cmp(&key)) {
            Ok(index) => Some(std::mem::replace(&mut self.0[index].1, value)),
            Err(index) => {
                self.0.insert(index, (key, value));
                None
            }
        }
    }

    pub fn get_mut(&mut self, key: &Key) -> Option<&mut Expr> {
        self.0
            .binary_search_by(|(k, _)| k.cmp(key))
            .ok()
            .map(|index| &mut self.0[index].1)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Key, &Expr)> {
        self.0.iter().map(|(k, v)| (k, v))
    }

    pub fn values(&self) -> impl Iterator<Item = &Expr> {
        self.0.iter().map(|(_, v)| v)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl FromIterator<(Key, Expr)> for AttrSet {
    fn from_iter<I: IntoIterator<Item = (Key, Expr)>>(entries: I) -> Self {
        let mut entries: Vec<(Key, Expr)> = entries.into_iter().collect();
        entries.sort_by(|(a, _), (b, _)| a.cmp(b));
        entries.dedup_by(|later, kept| {
            if later.0 == kept.0 {
                std::mem::swap(&mut later.1, &mut kept.1);
                true
            } else {
                false
            }
        });
        Self(entries)
    }
}

impl<const N: usize> From<[(Key, Expr); N]> for AttrSet {
    fn from(entries: [(Key, Expr); N]) -> Self {
        entries.into_iter().collect()
    }
}

impl IntoIterator for AttrSet {
    type Item = (Key, Expr);
    type IntoIter = std::vec::IntoIter<(Key, Expr)>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum StrPart {
    Lit(NixStr),
    Interp(Expr),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Expr {
    Str(Vec<StrPart>),
    Bool(bool),
    List(Vec<Expr>),
    Attrs(AttrSet),
    Var(Ident),
    Select(Box<Expr>, Key, Vec<Key>),
    Apply(Box<Expr>, Box<Expr>),
    Lambda {
        formals: BTreeSet<Ident>,
        ellipsis: bool,
        body: Box<Expr>,
    },
    Path(RelPath),
}

impl Expr {
    pub fn string(s: impl Into<String>) -> Result<Self, NulByte> {
        Ok(Self::Str(vec![StrPart::Lit(NixStr::new(s)?)]))
    }

    pub fn attrs<I>(entries: I) -> Self
    where
        I: IntoIterator<Item = (Key, Expr)>,
    {
        Self::Attrs(entries.into_iter().collect())
    }

    pub fn select(base: Expr, first: Key, rest: impl IntoIterator<Item = Key>) -> Self {
        Self::Select(Box::new(base), first, rest.into_iter().collect())
    }

    pub fn apply(function: Expr, argument: Expr) -> Self {
        Self::Apply(Box::new(function), Box::new(argument))
    }

    pub fn print(&self) -> String {
        print::print(self)
    }
}
