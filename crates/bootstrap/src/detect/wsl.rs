use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wsl {
    No,
    V1,
    V2,
}

pub fn detect() -> Wsl {
    detect_from(
        std::env::var_os("WSL_DISTRO_NAME").is_some(),
        std::env::var_os("WSL_INTEROP").is_some(),
    )
}

fn detect_from(has_distro_name: bool, has_interop: bool) -> Wsl {
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
    fn detect_from_distro_name_and_interop_is_v2() {
        assert_eq!(detect_from(true, true), Wsl::V2);
    }

    #[test]
    fn detect_from_distro_name_without_interop_is_v1() {
        assert_eq!(detect_from(true, false), Wsl::V1);
    }

    #[test]
    fn detect_from_no_distro_name_is_no() {
        assert_eq!(detect_from(false, true), Wsl::No);
        assert_eq!(detect_from(false, false), Wsl::No);
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
