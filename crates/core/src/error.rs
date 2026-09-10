use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("mix must run as root for this command (needed to {0})")]
    NotRoot(&'static str),

    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("command `{command}` failed: {detail}")]
    Command { command: String, detail: String },

    #[error("network error: {0}")]
    Network(String),

    #[error("{artifact}: {detail}\n\nRun `mix doctor --fix` to reconcile configuration drift.")]
    Integrity { artifact: String, detail: String },

    #[error(
        "this system already manages its own environment natively; mix's bootstrap isn't needed here."
    )]
    UnsupportedHost,

    #[error(
        "mix's sandboxed build environment needs a real Linux kernel. If this is WSL, upgrade to \
         WSL2 (`wsl --set-version <distro> 2`) and retry."
    )]
    UnsupportedKernel,

    #[error("{hint}\n\nmix requires systemd to manage its background services.")]
    SystemdNotReady { hint: &'static str },

    #[error(
        "an existing, unmanaged runtime was detected on this system; mix requires a dedicated \
         managed environment. Remove it before continuing."
    )]
    AlreadyManaged,

    #[error("mix does not support this platform ({0})")]
    UnsupportedTarget(String),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;
