mod action;
mod failure;
mod observe;
mod precondition;
mod progress;

pub use action::*;
pub use failure::*;
pub use observe::*;
pub use precondition::*;
pub use progress::*;

use serde::{Deserialize, Serialize};

pub type Owner = (u32, u32);

pub(crate) mod error_kind {
    use std::io::ErrorKind;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(kind: &ErrorKind, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&mix_events::io_kind::name(*kind))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<ErrorKind, D::Error> {
        let name = String::deserialize(deserializer)?;
        Ok(mix_events::io_kind::named(&name))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FileId {
    pub dev: u64,
    pub ino: u64,
    pub born: Option<(i64, u32)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Digest(pub [u8; 32]);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Expect {
    Absent,
    Present(FileId),
}

impl std::fmt::Display for FileId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "inode {} on device {}", self.ino, self.dev)
    }
}

impl std::fmt::Display for Expect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Expect::Absent => f.write_str("nothing"),
            Expect::Present(id) => id.fmt(f),
        }
    }
}

pub fn ids((uid, gid): Owner) -> String {
    format!("{uid}:{gid}")
}

pub fn generation(generation: Option<u64>) -> String {
    match generation {
        Some(generation) => format!("generation {generation}"),
        None => "no generation".to_string(),
    }
}
