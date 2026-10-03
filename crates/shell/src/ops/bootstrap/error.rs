//! What bootstrap could not do, and the context that makes it legible.
//!
//! A variant here names the artifact, the host or the derivations: facts `mix-core` does not
//! have and this crate does. What a reader should do about it depends on the command they ran,
//! which this crate does not know, so the advice is written in `mix-explain` instead.

/// Where systemd was looked for, which is what decides how it is turned on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Host {
    /// An ordinary Linux distribution.
    Native,
    /// A WSL distro, where systemd is opt-in.
    Wsl,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Core(#[from] mix_core::Error),

    #[error("network request failed: {0}")]
    Network(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("{artifact}: {detail}")]
    Integrity { artifact: String, detail: String },

    #[error("unsupported platform: {0}")]
    UnsupportedTarget(String),

    #[error("{0}")]
    InvalidMirror(String),

    #[error("{subject} changed while mix was working: expected {expected}, found {found}")]
    Conflict {
        subject: String,
        expected: String,
        found: String,
    },

    #[error(transparent)]
    Target(#[from] crate::target::Error),

    #[error("decompressing archive: {0}")]
    Decompression(String),

    #[error("unexpected archive layout: {0}")]
    MalformedArchive(String),

    #[error("root privileges are required to {0}")]
    NotRoot(&'static str),

    #[error("this host is NixOS, which manages its own environment natively")]
    UnsupportedHost,

    #[error("WSL1 does not provide the real Linux kernel that sandboxed builds need")]
    UnsupportedKernel,

    #[error("systemd is not active: `/run/systemd/system` is missing or PID 1 is not systemd")]
    SystemdNotReady { host: Host },

    #[error("systemd did not answer on the system bus")]
    SystemdUnreachable,

    #[error("systemd could not {operation} {unit}: {detail}")]
    Unit {
        operation: String,
        unit: String,
        detail: String,
        invocation: Option<String>,
    },

    #[error("an existing, unmanaged Nix installation was found on this system")]
    AlreadyManaged,

    #[error(
        "cannot move {} into /nix/store: it is on a different filesystem",
        path.display()
    )]
    CrossDeviceStore { path: std::path::PathBuf },

    #[error("interrupted")]
    Interrupted,
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_error_preserves_the_source_chain() {
        let boxed: Box<dyn std::error::Error + Send + Sync> = "connection reset".into();
        let err = Error::Network(boxed);

        let source = std::error::Error::source(&err).expect("source should be preserved");
        assert_eq!(source.to_string(), "connection reset");
    }

    /// The path is the context this crate has; what to do about the mount it is on is the
    /// command's business, not the library's.
    #[test]
    fn cross_device_store_names_the_offending_path() {
        let err = Error::CrossDeviceStore {
            path: "/nix/store/pkg-a".into(),
        };

        assert_eq!(
            err.to_string(),
            "cannot move /nix/store/pkg-a into /nix/store: it is on a different filesystem"
        );
    }
}
