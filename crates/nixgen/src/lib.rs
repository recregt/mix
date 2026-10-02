mod escape;
mod flake;
#[cfg(test)]
mod flake_write;
mod header;
mod home;
mod ident;
mod inputs;
mod installable;
mod print;

pub mod ast;
pub mod lock;
#[cfg(any(test, feature = "parse"))]
pub mod parse;

pub use escape::NulByte;
pub use flake::{FlakeConfig, Rev, System};
pub(crate) use header::GENERATED_HEADER;
pub use home::{CopyIntoGeneration, HomeModule, InvalidInput, StateVersion};
pub use ident::{FileName, Ident, InvalidIdent, is_identifier};
pub use inputs::{INPUTS, Input, Pin, Pins};
pub use installable::{AttrPath, FlakeRef, Installable, InvalidInstallable, PublicKey};
