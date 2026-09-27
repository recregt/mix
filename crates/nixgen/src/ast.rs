use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use crate::escape::NulByte;
use crate::ident::{FileName, Ident, is_identifier};
use crate::print;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Key(Cow<'static, str>);

impl Key {
    pub fn new(s: impl Into<String>) -> Result<Self, NulByte> {
        let s = s.into();
        if s.contains('\0') {
            Err(NulByte)
        } else {
            Ok(Self(Cow::Owned(s)))
        }
    }

    pub const fn new_static(s: &'static str) -> Self {
        assert!(!contains_nul(s.as_bytes()), "a key cannot hold a null byte");
        Self(Cow::Borrowed(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn is_bare(&self) -> bool {
        is_identifier(&self.0)
    }
}

impl From<&Ident> for Key {
    fn from(ident: &Ident) -> Self {
        Self(ident.0.clone())
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
    Attrs(BTreeMap<Key, Expr>),
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
