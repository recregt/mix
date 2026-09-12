#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Core(#[from] mix_core::Error),

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
