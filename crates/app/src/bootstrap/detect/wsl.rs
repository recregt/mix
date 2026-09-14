use std::path::Path;

use crate::bootstrap::util::path_exists;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wsl {
    No,
    V1,
    V2,
}

pub async fn detect() -> Wsl {
    detect_at(
        Path::new("/proc/sys/kernel/osrelease"),
        Path::new("/sys/fs/cgroup/cgroup.controllers"),
        std::env::var_os("WSL_DISTRO_NAME").is_some(),
        std::env::var_os("WSL_INTEROP").is_some(),
    )
    .await
}

async fn detect_at(
    osrelease_path: &Path,
    cgroup_controllers_path: &Path,
    has_distro_name: bool,
    has_interop: bool,
) -> Wsl {
    match tokio::fs::read_to_string(osrelease_path).await {
        Ok(release) => {
            let detected = detect_from_kernel_release(&release);
            if detected == Wsl::V1 && has_real_cgroup2_at(cgroup_controllers_path).await {
                Wsl::V2
            } else {
                detected
            }
        }
        Err(_) => detect_from_env(
            has_distro_name,
            has_interop,
            has_real_cgroup2_at(cgroup_controllers_path).await,
        ),
    }
}

async fn has_real_cgroup2_at(controllers_path: &Path) -> bool {
    tokio::fs::read_to_string(controllers_path)
        .await
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
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

fn detect_from_env(has_distro_name: bool, has_interop: bool, has_real_cgroup2: bool) -> Wsl {
    if !has_distro_name && !has_interop {
        return Wsl::No;
    }
    if has_real_cgroup2 { Wsl::V2 } else { Wsl::V1 }
}

pub async fn systemd_active() -> bool {
    systemd_active_at(Path::new("/run/systemd/system"), Path::new("/proc/1/comm")).await
}

async fn systemd_active_at(marker: &Path, comm_path: &Path) -> bool {
    path_exists(marker).await
        && tokio::fs::read_to_string(comm_path)
            .await
            .map(|s| s.trim() == "systemd")
            .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_from_env_no_wsl_signal_is_no_regardless_of_cgroup2() {
        assert_eq!(detect_from_env(false, false, true), Wsl::No);
        assert_eq!(detect_from_env(false, false, false), Wsl::No);
    }

    #[test]
    fn detect_from_env_wsl_signal_with_real_cgroup2_is_v2() {
        assert_eq!(detect_from_env(true, false, true), Wsl::V2);
        assert_eq!(detect_from_env(false, true, true), Wsl::V2);
    }

    #[test]
    fn detect_from_env_wsl_signal_without_real_cgroup2_is_v1() {
        assert_eq!(detect_from_env(true, false, false), Wsl::V1);
        assert_eq!(detect_from_env(true, true, false), Wsl::V1);
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

    #[tokio::test]
    async fn detect_at_ignores_env_when_kernel_release_is_readable() {
        let dir = tempfile::tempdir().unwrap();
        let osrelease = dir.path().join("osrelease");
        std::fs::write(&osrelease, "5.15.167.4-microsoft-standard-WSL2\n").unwrap();
        let cgroup = dir.path().join("missing-cgroup");
        assert_eq!(detect_at(&osrelease, &cgroup, false, false).await, Wsl::V2);
    }

    #[tokio::test]
    async fn detect_at_falls_back_to_env_when_kernel_release_is_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        let cgroup = dir.path().join("missing-cgroup");
        assert_eq!(detect_at(&missing, &cgroup, true, false).await, Wsl::V1);
        assert_eq!(detect_at(&missing, &cgroup, false, false).await, Wsl::No);
    }

    #[tokio::test]
    async fn detect_at_fallback_trusts_cgroup2_over_stripped_interop_var() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        let cgroup = dir.path().join("cgroup.controllers");
        std::fs::write(&cgroup, "cpuset cpu io memory hugetlb pids rdma\n").unwrap();
        assert_eq!(detect_at(&missing, &cgroup, true, false).await, Wsl::V2);
    }

    #[tokio::test]
    async fn detect_at_stays_v1_when_no_real_cgroup2_is_present() {
        let dir = tempfile::tempdir().unwrap();
        let osrelease = dir.path().join("osrelease");
        std::fs::write(&osrelease, "4.4.0-19041-Microsoft\n").unwrap();
        let cgroup = dir.path().join("missing-cgroup");
        assert_eq!(detect_at(&osrelease, &cgroup, false, false).await, Wsl::V1);
    }

    #[tokio::test]
    async fn detect_at_overrides_v1_to_v2_when_real_cgroup2_is_present() {
        let dir = tempfile::tempdir().unwrap();
        let osrelease = dir.path().join("osrelease");
        std::fs::write(&osrelease, "6.6.0-custom-Microsoft\n").unwrap();
        let cgroup = dir.path().join("cgroup.controllers");
        std::fs::write(&cgroup, "cpuset cpu io memory hugetlb pids rdma\n").unwrap();
        assert_eq!(detect_at(&osrelease, &cgroup, false, false).await, Wsl::V2);
    }

    #[tokio::test]
    async fn has_real_cgroup2_at_false_when_empty_or_missing() {
        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("empty");
        std::fs::write(&empty, "  \n").unwrap();
        assert!(!has_real_cgroup2_at(&empty).await);
        assert!(!has_real_cgroup2_at(&dir.path().join("missing")).await);
    }

    #[tokio::test]
    async fn has_real_cgroup2_at_true_when_controllers_listed() {
        let dir = tempfile::tempdir().unwrap();
        let controllers = dir.path().join("cgroup.controllers");
        std::fs::write(&controllers, "cpuset cpu io memory\n").unwrap();
        assert!(has_real_cgroup2_at(&controllers).await);
    }

    #[tokio::test]
    async fn systemd_active_true_when_marker_exists_and_pid1_is_systemd() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("system");
        std::fs::create_dir(&marker).unwrap();
        let comm = dir.path().join("comm");
        std::fs::write(&comm, "systemd\n").unwrap();
        assert!(systemd_active_at(&marker, &comm).await);
    }

    #[tokio::test]
    async fn systemd_active_false_when_marker_missing() {
        let dir = tempfile::tempdir().unwrap();
        let comm = dir.path().join("comm");
        std::fs::write(&comm, "systemd\n").unwrap();
        assert!(!systemd_active_at(&dir.path().join("missing"), &comm).await);
    }

    #[tokio::test]
    async fn systemd_active_false_when_pid1_is_not_systemd() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("system");
        std::fs::create_dir(&marker).unwrap();
        let comm = dir.path().join("comm");
        std::fs::write(&comm, "init\n").unwrap();
        assert!(!systemd_active_at(&marker, &comm).await);
    }
}
