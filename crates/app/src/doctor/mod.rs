use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use futures_util::future::join_all;
use mix_core::identity;
use mix_core::models::{Category, Target, UserConfig};

use crate::shared::os::{DIR_MODE_MASK, files_match, path_exists, systemd_unit_is_active};

pub struct HealthReport {
    pub name: String,
    pub category: Category,
    pub healthy: bool,
    pub detail: Option<String>,
}

pub async fn audit(user_config: Option<&UserConfig>) -> Vec<HealthReport> {
    tracing::info!("auditing managed environment");
    let items = mix_core::models::targets(user_config);
    let details = join_all(items.iter().map(inspect)).await;
    items
        .iter()
        .zip(details)
        .map(|(target, detail)| {
            let name = target.label().into_owned();
            if let Some(detail) = &detail {
                tracing::debug!("unhealthy: {name}: {detail}");
            }
            HealthReport {
                name,
                category: target.category(),
                healthy: detail.is_none(),
                detail,
            }
        })
        .collect()
}

async fn inspect(target: &Target) -> Option<String> {
    match target {
        Target::Directory { path, mode, owner } => {
            tracing::debug!("checking directory: {}", path.display());
            inspect_directory(path, *mode, *owner).await
        }
        Target::File {
            path,
            expected,
            owner,
        } => {
            tracing::debug!("checking file: {}", path.display());
            inspect_file(path, expected.as_deref(), *owner).await
        }
        Target::Group { name, gid } => {
            tracing::debug!("checking group: {name}");
            inspect_group(name, *gid)
        }
        Target::GroupMember { group, user } => {
            tracing::debug!("checking {group} membership: {user}");
            inspect_group_member(group, user)
        }
        Target::User { n, uid, gid } => {
            tracing::debug!("checking user: {}", mix_core::identity::user_name(*n));
            inspect_user(*n, *uid, *gid)
        }
        Target::SystemdUnit {
            name,
            src,
            dest,
            must_be_active,
        } => {
            tracing::debug!("checking systemd unit: {name}");
            inspect_systemd_unit(name, src, dest, *must_be_active).await
        }
        Target::PathExists { name, path } => {
            tracing::debug!("checking path: {path} ({name})");
            inspect_path_exists(path).await
        }
    }
}

async fn inspect_directory(path: &Path, mode: u32, owner: Option<(u32, u32)>) -> Option<String> {
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
    inspect_owner(&meta, owner)
}

async fn inspect_file(
    path: &Path,
    expected: Option<&str>,
    owner: Option<(u32, u32)>,
) -> Option<String> {
    let meta = match tokio::fs::metadata(path).await {
        Ok(meta) => meta,
        Err(_) if expected.is_none() => return None,
        Err(e) => return Some(e.to_string()),
    };
    match expected {
        Some(expected) => match tokio::fs::read_to_string(path).await {
            Ok(contents) if contents == expected => inspect_owner(&meta, owner),
            Ok(_) => Some("configuration drift detected (contents modified)".to_string()),
            Err(e) => Some(e.to_string()),
        },
        None => inspect_owner(&meta, owner),
    }
}

fn inspect_owner(meta: &std::fs::Metadata, owner: Option<(u32, u32)>) -> Option<String> {
    let (uid, gid) = owner?;
    if meta.uid() != uid || meta.gid() != gid {
        return Some(format!(
            "owned by {}:{}, expected {uid}:{gid}",
            meta.uid(),
            meta.gid()
        ));
    }
    None
}

fn inspect_group(name: &str, gid: u32) -> Option<String> {
    if identity::group_has_gid(name, gid) {
        None
    } else {
        Some("group is missing or has the wrong gid".to_string())
    }
}

fn inspect_group_member(group: &str, user: &str) -> Option<String> {
    if identity::group_has_member(group, user) {
        None
    } else {
        Some(format!("not a member of the {group} group"))
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
    use std::path::PathBuf;

    use mix_core::privilege::InvokingUser;

    use super::*;

    fn user_config() -> UserConfig {
        UserConfig {
            user: InvokingUser {
                uid: 1000,
                gid: 1000,
                name: "mix-user".to_string(),
                home: PathBuf::from("/home/mix-user"),
            },
            flake: "flake-content".to_string(),
            home: "home-content".to_string(),
        }
    }

    #[tokio::test]
    async fn audit_reports_one_entry_per_target() {
        let reports = audit(None).await;
        assert_eq!(reports.len(), mix_core::models::targets(None).len());
    }

    #[tokio::test]
    async fn audit_covers_the_per_user_targets_of_the_config_it_is_given() {
        let cfg = user_config();

        let reports = audit(Some(&cfg)).await;

        assert_eq!(
            reports.len(),
            mix_core::models::targets(Some(&cfg)).len(),
            "every target of the injected config must be reported"
        );
        assert!(reports.len() > audit(None).await.len());
        assert!(
            reports
                .iter()
                .any(|report| report.name.contains("/home/mix-user"))
        );
    }

    #[tokio::test]
    async fn inspect_directory_passes_when_mode_matches() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let detail = inspect_directory(dir.path(), 0o755, None).await;
        assert!(detail.is_none());
    }

    #[tokio::test]
    async fn inspect_directory_reports_a_mode_drift() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let detail = inspect_directory(dir.path(), 0o755, None).await;
        assert!(detail.is_some());
    }

    #[tokio::test]
    async fn inspect_directory_reports_a_regular_file_in_its_place() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not-a-dir");
        std::fs::write(&file, "x").unwrap();
        let detail = inspect_directory(&file, 0o755, None).await;
        assert_eq!(detail.as_deref(), Some("exists but is not a directory"));
    }

    #[tokio::test]
    async fn inspect_directory_passes_when_owner_matches() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let uid = nix::unistd::Uid::current().as_raw();
        let gid = nix::unistd::Gid::current().as_raw();
        let detail = inspect_directory(dir.path(), 0o755, Some((uid, gid))).await;
        assert!(detail.is_none());
    }

    #[tokio::test]
    async fn inspect_directory_reports_an_owner_drift() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let detail = inspect_directory(dir.path(), 0o755, Some((999_999, 999_999))).await;
        assert!(detail.is_some());
    }

    #[tokio::test]
    async fn inspect_file_passes_when_content_matches() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");
        std::fs::write(&file, "expected content").unwrap();
        let detail = inspect_file(&file, Some("expected content"), None).await;
        assert!(detail.is_none());
    }

    #[tokio::test]
    async fn inspect_file_reports_content_drift() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");
        std::fs::write(&file, "modified").unwrap();
        let detail = inspect_file(&file, Some("expected content"), None).await;
        assert!(detail.is_some());
    }

    #[tokio::test]
    async fn inspect_file_reports_an_owner_drift() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");
        std::fs::write(&file, "expected content").unwrap();
        let detail = inspect_file(&file, Some("expected content"), Some((999_999, 999_999))).await;
        assert!(detail.is_some());
    }

    #[tokio::test]
    async fn inspect_file_with_no_expected_content_is_healthy_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("flake.lock");
        let detail = inspect_file(&file, None, None).await;
        assert!(detail.is_none());
    }

    #[tokio::test]
    async fn inspect_file_with_no_expected_content_ignores_whatever_is_there() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("flake.lock");
        std::fs::write(&file, "anything nix wrote").unwrap();
        let detail = inspect_file(&file, None, None).await;
        assert!(detail.is_none());
    }

    #[tokio::test]
    async fn inspect_file_with_no_expected_content_still_reports_owner_drift() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("flake.lock");
        std::fs::write(&file, "anything nix wrote").unwrap();
        let detail = inspect_file(&file, None, Some((999_999, 999_999))).await;
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

    #[test]
    fn inspect_group_member_passes_for_a_member() {
        assert!(inspect_group_member("root", "root").is_none());
    }

    #[test]
    fn inspect_group_member_reports_a_user_outside_the_group() {
        assert_eq!(
            inspect_group_member("root", "mix-test-nonexistent-user-xyz").as_deref(),
            Some("not a member of the root group")
        );
    }

    #[tokio::test]
    async fn inspect_path_exists_reports_a_missing_path() {
        let detail = inspect_path_exists("/does/not/exist/nix-env").await;
        assert_eq!(detail.as_deref(), Some("missing"));
    }
}
