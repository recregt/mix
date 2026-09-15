use std::os::unix::fs::PermissionsExt;

use mix_core::identity;
use mix_core::models::{Category, Target};

use crate::os::{files_match, path_exists, systemd_unit_is_active};

const DIR_MODE_MASK: u32 = 0o7777;

pub struct HealthReport {
    pub name: String,
    pub category: Category,
    pub healthy: bool,
    pub detail: Option<String>,
}

pub async fn audit() -> Vec<HealthReport> {
    tracing::info!("auditing managed environment");
    let mut reports = Vec::new();
    for target in mix_core::models::targets() {
        let name = target.label();
        let category = target.category();
        let detail = inspect(&target).await;
        if let Some(detail) = &detail {
            tracing::debug!("unhealthy: {name}: {detail}");
        }
        reports.push(HealthReport {
            name,
            category,
            healthy: detail.is_none(),
            detail,
        });
    }
    reports
}

async fn inspect(target: &Target) -> Option<String> {
    match *target {
        Target::Directory { path, mode } => {
            tracing::debug!("checking directory: {path}");
            inspect_directory(path, mode).await
        }
        Target::File { path, expected } => {
            tracing::debug!("checking file: {path}");
            inspect_file(path, expected).await
        }
        Target::Group { name, gid } => {
            tracing::debug!("checking group: {name}");
            inspect_group(name, gid)
        }
        Target::User { n, uid, gid } => {
            tracing::debug!("checking user: {}", mix_core::identity::user_name(n));
            inspect_user(n, uid, gid)
        }
        Target::SystemdUnit {
            name,
            src,
            dest,
            must_be_active,
        } => {
            tracing::debug!("checking systemd unit: {name}");
            inspect_systemd_unit(name, src, dest, must_be_active).await
        }
        Target::PathExists { name, path } => {
            tracing::debug!("checking path: {path} ({name})");
            inspect_path_exists(path).await
        }
    }
}

async fn inspect_directory(path: &str, mode: u32) -> Option<String> {
    let meta = match tokio::fs::metadata(path).await {
        Ok(meta) => meta,
        Err(e) => return Some(e.to_string()),
    };
    if !meta.is_dir() {
        return Some("exists but is not a directory".to_string());
    }
    let actual = meta.permissions().mode() & DIR_MODE_MASK;
    if actual != mode {
        return Some(format!("mode is {actual:o}, expected {mode:o}"));
    }
    None
}

async fn inspect_file(path: &str, expected: &str) -> Option<String> {
    match tokio::fs::read_to_string(path).await {
        Ok(contents) if contents == expected => None,
        Ok(_) => Some("configuration drift detected (contents modified)".to_string()),
        Err(e) => Some(e.to_string()),
    }
}

fn inspect_group(name: &str, gid: u32) -> Option<String> {
    if identity::group_has_gid(name, gid) {
        None
    } else {
        Some("group is missing or has the wrong gid".to_string())
    }
}

fn inspect_user(n: u32, uid: u32, gid: u32) -> Option<String> {
    let name = identity::user_name(n);
    if identity::user_matches(&name, uid, gid) {
        None
    } else {
        Some("user is missing or has the wrong uid/gid".to_string())
    }
}

async fn inspect_systemd_unit(
    name: &str,
    src: &str,
    dest: &str,
    must_be_active: bool,
) -> Option<String> {
    if !path_exists(dest).await {
        return Some("unit file missing".to_string());
    }
    if !files_match(src, dest).await {
        return Some("unit file contents drifted from the installed default".to_string());
    }
    if must_be_active && !systemd_unit_is_active(name).await {
        return Some("unit is not active".to_string());
    }
    None
}

async fn inspect_path_exists(path: &str) -> Option<String> {
    if path_exists(path).await {
        None
    } else {
        Some("missing".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn audit_reports_one_entry_per_target() {
        let reports = audit().await;
        assert_eq!(reports.len(), mix_core::models::targets().len());
    }

    #[tokio::test]
    async fn inspect_directory_passes_when_mode_matches() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let detail = inspect_directory(dir.path().to_str().unwrap(), 0o755).await;
        assert!(detail.is_none());
    }

    #[tokio::test]
    async fn inspect_directory_reports_a_mode_drift() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let detail = inspect_directory(dir.path().to_str().unwrap(), 0o755).await;
        assert!(detail.is_some());
    }

    #[tokio::test]
    async fn inspect_directory_reports_a_regular_file_in_its_place() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not-a-dir");
        std::fs::write(&file, "x").unwrap();
        let detail = inspect_directory(file.to_str().unwrap(), 0o755).await;
        assert_eq!(detail.as_deref(), Some("exists but is not a directory"));
    }

    #[tokio::test]
    async fn inspect_file_passes_when_content_matches() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");
        std::fs::write(&file, "expected content").unwrap();
        let detail = inspect_file(file.to_str().unwrap(), "expected content").await;
        assert!(detail.is_none());
    }

    #[tokio::test]
    async fn inspect_file_reports_content_drift() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");
        std::fs::write(&file, "modified").unwrap();
        let detail = inspect_file(file.to_str().unwrap(), "expected content").await;
        assert!(detail.is_some());
    }

    #[test]
    fn inspect_group_passes_for_a_known_system_group() {
        assert!(inspect_group("root", 0).is_none());
    }

    #[test]
    fn inspect_group_reports_the_wrong_gid() {
        assert!(inspect_group("root", 9999).is_some());
    }

    #[tokio::test]
    async fn inspect_path_exists_reports_a_missing_path() {
        let detail = inspect_path_exists("/does/not/exist/nix-env").await;
        assert_eq!(detail.as_deref(), Some("missing"));
    }
}
