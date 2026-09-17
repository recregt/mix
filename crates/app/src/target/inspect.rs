//! Measuring one declared target: one inspection, one [`Finding`].
//!
//! An inspection measures, it does not write. It is the only place the state of an artifact is
//! read, so `mix doctor` and `mix repair` cannot disagree about what is wrong with one — and a
//! reconciliation starts from what was measured here rather than looking again.

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use mix_core::identity;
use mix_core::models::Target;

use crate::fs::{DIR_MODE_MASK, Owner, exists};
use crate::systemd::unit_is_active;
use crate::target::Finding;

pub async fn inspect(target: &Target) -> Option<Finding> {
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
        Target::SeededFile { path, owner, .. } => {
            tracing::debug!("checking seeded file: {}", path.display());
            inspect_seeded_file(path, *owner).await
        }
        Target::Group { name, gid } => {
            tracing::debug!("checking group: {name}");
            inspect_group(name, *gid)
        }
        Target::GroupMember { group, user } => {
            tracing::debug!("checking {group} membership: {user}");
            // The group is one of the names mix declares, so the finding can borrow it.
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

async fn inspect_directory(path: &Path, mode: u32, owner: Owner) -> Option<Finding> {
    let meta = match tokio::fs::metadata(path).await {
        Ok(meta) => meta,
        Err(e) => return Some(unreadable(e)),
    };
    if !meta.is_dir() {
        return Some(Finding::NotADirectory);
    }
    let actual = meta.permissions().mode() & DIR_MODE_MASK;
    if actual != mode {
        return Some(Finding::Mode {
            actual,
            expected: mode,
        });
    }
    inspect_owner(&meta, owner)
}

/// A file is read for what mix declares about it, and for nothing else: the contents when mix
/// writes them, the owner when mix declares one. A file with neither is not touched at all.
async fn inspect_file(path: &Path, expected: Option<&str>, owner: Owner) -> Option<Finding> {
    match expected {
        Some(expected) => match tokio::fs::read_to_string(path).await {
            Ok(contents) if contents == expected => inspect_declared_owner(path, owner).await,
            Ok(_) => Some(Finding::ContentDrift),
            Err(e) => Some(unreadable(e)),
        },
        // Nothing is declared about it, so there is nothing to read.
        None if owner.is_none() => None,
        None => match tokio::fs::metadata(path).await {
            Ok(meta) => inspect_owner(&meta, owner),
            // A file mix does not write is nix's to create: not being there yet is not drift.
            Err(_) => None,
        },
    }
}

async fn inspect_seeded_file(path: &Path, owner: Owner) -> Option<Finding> {
    let meta = match tokio::fs::metadata(path).await {
        Ok(meta) => meta,
        Err(e) => return Some(unreadable(e)),
    };
    inspect_owner(&meta, owner)
}

/// Not being there is its own finding: everything else is the path refusing to be read.
fn unreadable(e: std::io::Error) -> Finding {
    match e.kind() {
        std::io::ErrorKind::NotFound => Finding::Missing,
        kind => Finding::Unreadable { kind },
    }
}

/// Reads the owner of a path that has one declared, and nothing when it does not.
async fn inspect_declared_owner(path: &Path, owner: Owner) -> Option<Finding> {
    owner?;
    match tokio::fs::metadata(path).await {
        Ok(meta) => inspect_owner(&meta, owner),
        Err(e) => Some(unreadable(e)),
    }
}

fn inspect_owner(meta: &std::fs::Metadata, owner: Owner) -> Option<Finding> {
    let expected = owner?;
    let actual = (meta.uid(), meta.gid());
    (actual != expected).then_some(Finding::Owner { actual, expected })
}

fn inspect_group(name: &str, gid: u32) -> Option<Finding> {
    match identity::group_gid(name) {
        None => Some(Finding::GroupMissing),
        Some(actual) if actual != gid => Some(Finding::GroupGid {
            actual,
            expected: gid,
        }),
        Some(_) => None,
    }
}

fn inspect_group_member(group: &'static str, user: &str) -> Option<Finding> {
    if identity::group_has_member(group, user) {
        None
    } else if identity::user_exists(user) {
        Some(Finding::NotAMember { group })
    } else {
        // Enrolling an account that is gone is not something repair can do, so the inspection
        // says which of the two it is rather than leaving the caller to look again.
        Some(Finding::NoSuchUser)
    }
}

fn inspect_user(n: u32, uid: u32, gid: u32) -> Option<Finding> {
    let name = identity::user_name(n);
    let expected = (uid, gid);
    match identity::user_ids(&name) {
        None => Some(Finding::UserMissing),
        Some(actual) if actual != expected => Some(Finding::UserIds { actual, expected }),
        Some(_) => None,
    }
}

async fn inspect_systemd_unit(
    name: &str,
    src: &str,
    dest: &str,
    must_be_active: bool,
) -> Option<Finding> {
    // The installed unit is read once: whether it is there and whether it is the one mix ships
    // are the same read.
    match tokio::fs::read(dest).await {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(Finding::UnitMissing),
        Err(e) => return Some(Finding::Unreadable { kind: e.kind() }),
        Ok(installed) => {
            if tokio::fs::read(src).await.as_deref().ok() != Some(installed.as_slice()) {
                return Some(Finding::UnitDrift);
            }
        }
    }
    if must_be_active && !unit_is_active(name).await {
        return Some(Finding::UnitInactive);
    }
    None
}

async fn inspect_path_exists(path: &str) -> Option<Finding> {
    (!exists(path).await).then_some(Finding::RuntimeMissing)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn inspect_directory_passes_when_mode_matches() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let finding = inspect_directory(dir.path(), 0o755, None).await;
        assert!(finding.is_none());
    }

    #[tokio::test]
    async fn inspect_directory_reports_the_mode_it_read_and_the_one_it_wanted() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();

        let finding = inspect_directory(dir.path(), 0o755, None).await;

        assert_eq!(
            finding,
            Some(Finding::Mode {
                actual: 0o700,
                expected: 0o755
            })
        );
    }

    #[tokio::test]
    async fn inspect_directory_reports_a_regular_file_in_its_place() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not-a-dir");
        std::fs::write(&file, "x").unwrap();
        let finding = inspect_directory(&file, 0o755, None).await;
        assert_eq!(finding, Some(Finding::NotADirectory));
    }

    #[tokio::test]
    async fn inspect_directory_reports_a_missing_directory_as_missing() {
        let dir = tempfile::tempdir().unwrap();
        let finding = inspect_directory(&dir.path().join("gone"), 0o755, None).await;
        assert_eq!(finding, Some(Finding::Missing));
    }

    #[tokio::test]
    async fn inspect_directory_passes_when_owner_matches() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let uid = nix::unistd::Uid::current().as_raw();
        let gid = nix::unistd::Gid::current().as_raw();
        let finding = inspect_directory(dir.path(), 0o755, Some((uid, gid))).await;
        assert!(finding.is_none());
    }

    #[tokio::test]
    async fn inspect_directory_reports_both_sides_of_an_owner_drift() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let owner = (
            nix::unistd::Uid::current().as_raw(),
            nix::unistd::Gid::current().as_raw(),
        );

        let finding = inspect_directory(dir.path(), 0o755, Some((999_999, 999_999))).await;

        assert_eq!(
            finding,
            Some(Finding::Owner {
                actual: owner,
                expected: (999_999, 999_999)
            })
        );
    }

    #[tokio::test]
    async fn inspect_file_passes_when_content_matches() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");
        std::fs::write(&file, "expected content").unwrap();
        let finding = inspect_file(&file, Some("expected content"), None).await;
        assert!(finding.is_none());
    }

    #[tokio::test]
    async fn inspect_file_reports_content_drift() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");
        std::fs::write(&file, "modified").unwrap();
        let finding = inspect_file(&file, Some("expected content"), None).await;
        assert_eq!(finding, Some(Finding::ContentDrift));
    }

    #[tokio::test]
    async fn inspect_file_reports_an_owner_drift() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");
        std::fs::write(&file, "expected content").unwrap();
        let finding = inspect_file(&file, Some("expected content"), Some((999_999, 999_999))).await;
        assert!(matches!(finding, Some(Finding::Owner { .. })));
    }

    #[tokio::test]
    async fn inspect_file_with_no_expected_content_is_healthy_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("flake.lock");
        let finding = inspect_file(&file, None, None).await;
        assert!(finding.is_none());
    }

    #[tokio::test]
    async fn inspect_file_with_no_expected_content_ignores_whatever_is_there() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("flake.lock");
        std::fs::write(&file, "anything nix wrote").unwrap();
        let finding = inspect_file(&file, None, None).await;
        assert!(finding.is_none());
    }

    #[tokio::test]
    async fn inspect_file_with_no_expected_content_still_reports_owner_drift() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("flake.lock");
        std::fs::write(&file, "anything nix wrote").unwrap();
        let finding = inspect_file(&file, None, Some((999_999, 999_999))).await;
        assert!(matches!(finding, Some(Finding::Owner { .. })));
    }

    #[tokio::test]
    async fn inspect_file_reports_a_file_mix_wrote_and_cannot_find() {
        let dir = tempfile::tempdir().unwrap();
        let finding = inspect_file(&dir.path().join("home.nix"), Some("content"), None).await;
        assert_eq!(finding, Some(Finding::Missing));
    }

    #[tokio::test]
    async fn inspect_seeded_file_is_unhealthy_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state");
        let finding = inspect_seeded_file(&file, None).await;
        assert_eq!(finding, Some(Finding::Missing));
    }

    #[tokio::test]
    async fn inspect_seeded_file_is_healthy_when_present_regardless_of_content() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state");
        std::fs::write(&file, "whatever mix install has done to it since").unwrap();
        let finding = inspect_seeded_file(&file, None).await;
        assert!(finding.is_none());
    }

    #[tokio::test]
    async fn inspect_seeded_file_still_reports_owner_drift_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state");
        std::fs::write(&file, "content").unwrap();
        let finding = inspect_seeded_file(&file, Some((999_999, 999_999))).await;
        assert!(matches!(finding, Some(Finding::Owner { .. })));
    }

    /// A path that cannot be read is not the same fact as a path that is not there.
    #[test]
    fn a_refused_path_is_not_reported_as_a_missing_one() {
        assert_eq!(
            unreadable(std::io::Error::from(std::io::ErrorKind::NotFound)),
            Finding::Missing
        );
        assert_eq!(
            unreadable(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
            Finding::Unreadable {
                kind: std::io::ErrorKind::PermissionDenied
            }
        );
    }

    #[test]
    fn inspect_group_passes_for_a_known_system_group() {
        assert!(inspect_group("root", 0).is_none());
    }

    #[test]
    fn inspect_group_reports_the_gid_it_found() {
        assert_eq!(
            inspect_group("root", 9999),
            Some(Finding::GroupGid {
                actual: 0,
                expected: 9999
            })
        );
    }

    #[test]
    fn inspect_group_reports_a_group_that_is_not_there() {
        assert_eq!(
            inspect_group("mix-test-nonexistent-group-xyz", 30_000),
            Some(Finding::GroupMissing)
        );
    }

    #[test]
    fn inspect_group_member_passes_for_a_member() {
        assert!(inspect_group_member("root", "root").is_none());
    }

    /// Repair can enrol a user that exists; it cannot enrol one that does not, so the two are
    /// separate findings rather than one sentence covering both.
    #[test]
    fn inspect_group_member_separates_an_unenrolled_user_from_a_missing_one() {
        assert_eq!(
            inspect_group_member("root", "mix-test-nonexistent-user-xyz"),
            Some(Finding::NoSuchUser)
        );
        assert_eq!(
            inspect_group_member("mix-test-nonexistent-group-xyz", "root"),
            Some(Finding::NotAMember {
                group: "mix-test-nonexistent-group-xyz"
            })
        );
    }

    #[test]
    fn inspect_user_reports_a_build_user_that_is_not_there() {
        assert_eq!(inspect_user(1, 30_000, 30_000), Some(Finding::UserMissing));
    }

    #[tokio::test]
    async fn inspect_systemd_unit_tells_a_missing_unit_from_a_drifted_one() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("nix-daemon.service");
        std::fs::write(&src, "[Unit]\n").unwrap();
        let dest = dir.path().join("installed.service");

        assert_eq!(
            inspect_systemd_unit(
                "nix-daemon.service",
                src.to_str().unwrap(),
                dest.to_str().unwrap(),
                false
            )
            .await,
            Some(Finding::UnitMissing)
        );

        std::fs::write(&dest, "[Unit]\nsomething else\n").unwrap();
        assert_eq!(
            inspect_systemd_unit(
                "nix-daemon.service",
                src.to_str().unwrap(),
                dest.to_str().unwrap(),
                false
            )
            .await,
            Some(Finding::UnitDrift)
        );

        std::fs::write(&dest, "[Unit]\n").unwrap();
        assert!(
            inspect_systemd_unit(
                "nix-daemon.service",
                src.to_str().unwrap(),
                dest.to_str().unwrap(),
                false
            )
            .await
            .is_none()
        );
    }

    #[tokio::test]
    async fn inspect_path_exists_reports_the_runtime_as_missing() {
        let finding = inspect_path_exists("/does/not/exist/nix-env").await;
        assert_eq!(finding, Some(Finding::RuntimeMissing));
    }
}
