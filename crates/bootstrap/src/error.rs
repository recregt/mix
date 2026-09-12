#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Core(#[from] mix_core::Error),

    #[error(
        "network request failed: {0}\n\
         Please check your network connection, proxy settings, or --mirror URL."
    )]
    Network(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("{artifact}: {detail}")]
    Integrity { artifact: String, detail: String },

    #[error("unsupported platform: {0}")]
    UnsupportedTarget(String),

    #[error("decompressing archive: {0}")]
    Decompression(String),

    #[error("unexpected archive layout: {0}")]
    MalformedArchive(String),

    #[error(
        "root privileges required to {0}.\n\
         Please re-run this command with sudo:\n\
         \x20 sudo mix ..."
    )]
    NotRoot(&'static str),

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
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_error_preserves_the_source_chain_and_the_mirror_hint() {
        let boxed: Box<dyn std::error::Error + Send + Sync> = "connection reset".into();
        let err = Error::Network(boxed);

        assert!(err.to_string().contains("--mirror"));
        let source = std::error::Error::source(&err).expect("source should be preserved");
        assert_eq!(source.to_string(), "connection reset");
    }
}
