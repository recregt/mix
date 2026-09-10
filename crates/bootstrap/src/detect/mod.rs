pub mod os_release;
pub mod wsl;

pub use os_release::Distro;
pub use wsl::Wsl;

#[derive(Debug, Clone)]
pub struct Host {
    pub distro: Distro,
    pub wsl: Wsl,
    pub systemd_active: bool,
}

pub fn detect() -> Host {
    Host {
        distro: os_release::detect(),
        wsl: wsl::detect(),
        systemd_active: wsl::systemd_active(),
    }
}
