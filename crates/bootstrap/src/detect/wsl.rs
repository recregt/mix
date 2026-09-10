use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wsl {
    No,
    V1,
    V2,
}

pub fn detect() -> Wsl {
    let has_distro_name = std::env::var_os("WSL_DISTRO_NAME").is_some();
    let has_interop = std::env::var_os("WSL_INTEROP").is_some();

    match (has_distro_name, has_interop) {
        (true, true) => Wsl::V2,
        (true, false) => Wsl::V1,
        (false, _) => Wsl::No,
    }
}

pub fn systemd_active() -> bool {
    Path::new("/run/systemd/system").exists()
        && std::fs::read_to_string("/proc/1/comm")
            .map(|s| s.trim() == "systemd")
            .unwrap_or(false)
}
