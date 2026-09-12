use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wsl {
    No,
    V1,
    V2,
}

pub fn detect() -> Wsl {
    detect_at(
        Path::new("/proc/sys/kernel/osrelease"),
        std::env::var_os("WSL_DISTRO_NAME").is_some(),
        std::env::var_os("WSL_INTEROP").is_some(),
    )
}

fn detect_at(osrelease_path: &Path, has_distro_name: bool, has_interop: bool) -> Wsl {
    match std::fs::read_to_string(osrelease_path) {
        Ok(release) => detect_from_kernel_release(&release),
        Err(_) => detect_from_env(has_distro_name, has_interop),
    }
}

fn detect_from_kernel_release(release: &str) -> Wsl {
    let release = release.to_ascii_lowercase();
    if release.contains("wsl2") {
        Wsl::V2
    } else if release.contains("microsoft") {
        Wsl::V1
    } else {
        Wsl::No
    }
}

fn detect_from_env(has_distro_name: bool, has_interop: bool) -> Wsl {
    match (has_distro_name, has_interop) {
        (true, true) => Wsl::V2,
        (true, false) => Wsl::V1,
        (false, _) => Wsl::No,
    }
}

pub fn systemd_active() -> bool {
    systemd_active_at(Path::new("/run/systemd/system"), Path::new("/proc/1/comm"))
}

fn systemd_active_at(marker: &Path, comm_path: &Path) -> bool {
    marker.exists()
        && std::fs::read_to_string(comm_path)
            .map(|s| s.trim() == "systemd")
            .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_from_env_distro_name_and_interop_is_v2() {
        assert_eq!(detect_from_env(true, true), Wsl::V2);
    }

    #[test]
    fn detect_from_env_distro_name_without_interop_is_v1() {
        assert_eq!(detect_from_env(true, false), Wsl::V1);
    }

    #[test]
    fn detect_from_env_no_distro_name_is_no() {
        assert_eq!(detect_from_env(false, true), Wsl::No);
        assert_eq!(detect_from_env(false, false), Wsl::No);
    }

    #[test]
    fn detect_from_kernel_release_wsl2_signature_is_v2() {
        assert_eq!(
            detect_from_kernel_release("5.15.167.4-microsoft-standard-WSL2\n"),
            Wsl::V2
        );
    }

    #[test]
    fn detect_from_kernel_release_microsoft_signature_is_v1() {
        assert_eq!(
            detect_from_kernel_release("4.4.0-19041-Microsoft\n"),
            Wsl::V1
        );
    }

    #[test]
    fn detect_from_kernel_release_plain_linux_is_no() {
        assert_eq!(detect_from_kernel_release("6.8.0-45-generic\n"), Wsl::No);
    }

    #[test]
    fn detect_at_ignores_env_when_kernel_release_is_readable() {
        let dir = tempfile::tempdir().unwrap();
        let osrelease = dir.path().join("osrelease");
        std::fs::write(&osrelease, "5.15.167.4-microsoft-standard-WSL2\n").unwrap();
        assert_eq!(detect_at(&osrelease, false, false), Wsl::V2);
    }

    #[test]
    fn detect_at_falls_back_to_env_when_kernel_release_is_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        assert_eq!(detect_at(&missing, true, false), Wsl::V1);
        assert_eq!(detect_at(&missing, false, false), Wsl::No);
    }

    #[test]
    fn systemd_active_true_when_marker_exists_and_pid1_is_systemd() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("system");
        std::fs::create_dir(&marker).unwrap();
        let comm = dir.path().join("comm");
        std::fs::write(&comm, "systemd\n").unwrap();
        assert!(systemd_active_at(&marker, &comm));
    }

    #[test]
    fn systemd_active_false_when_marker_missing() {
        let dir = tempfile::tempdir().unwrap();
        let comm = dir.path().join("comm");
        std::fs::write(&comm, "systemd\n").unwrap();
        assert!(!systemd_active_at(&dir.path().join("missing"), &comm));
    }

    #[test]
    fn systemd_active_false_when_pid1_is_not_systemd() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("system");
        std::fs::create_dir(&marker).unwrap();
        let comm = dir.path().join("comm");
        std::fs::write(&comm, "init\n").unwrap();
        assert!(!systemd_active_at(&marker, &comm));
    }
}
