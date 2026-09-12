use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(
        "root privileges required to {0}.\n\
         Please re-run this command with sudo:\n\
         \x20 sudo mix ..."
    )]
    NotRoot(&'static str),

    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("command `{command}` failed: {detail}")]
    Command { command: String, detail: String },

    #[error(
        "network request failed: {0}\n\
         Please check your network connection, proxy settings, or --mirror URL."
    )]
    Network(String),

    #[error("{artifact}: {detail}")]
    Integrity { artifact: String, detail: String },

    #[error(
        "this system already manages its own environment natively.\n\
         `mix` is designed for standard Linux distributions and is not needed on NixOS."
    )]
    UnsupportedHost,

    #[error(
        "a real Linux kernel is required for sandboxed builds (WSL1 is not supported).\n\
         To upgrade this distro to WSL2, run from Windows PowerShell:\n\
         \x20 wsl --set-version <distro> 2"
    )]
    UnsupportedKernel,

    #[error("{hint}")]
    SystemdNotReady { hint: &'static str },

    #[error(
        "an existing, unmanaged runtime was detected on this system.\n\
         `mix` requires a dedicated environment to manage its own reproducible runtime.\n\
         To continue, uninstall the existing Nix installation or remove `/nix`:\n\
         \x20 sudo rm -rf /nix"
    )]
    AlreadyManaged,

    #[error("`mix` does not support this platform ({0})")]
    UnsupportedTarget(String),

    #[error("decompressing archive: {0}")]
    Decompression(String),

    #[error("archive layout was not what `mix` expected: {0}")]
    MalformedArchive(String),

    #[error("background task panicked: {0}")]
    TaskPanicked(String),
}

pub type Result<T> = std::result::Result<T, Error>;
