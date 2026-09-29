pub use mix_core::health::{Finding, Unfixable};

/// What reconciling a target could not do.
///
/// Facts and the context around them: which artifact, and — for something beyond repair's
/// reach — which of the reasons it is out of reach for. The words are `mix-cli`'s.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Core(#[from] mix_core::Error),

    #[error("{artifact}: {reason}")]
    Unrepairable { artifact: String, reason: Unfixable },
}

pub type Result<T> = std::result::Result<T, Error>;
