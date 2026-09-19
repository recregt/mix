//! Putting one declared target back to what it is declared to be.
//!
//! A reconciliation starts from the [`Finding`] the inspection took: an account whose ids
//! drifted is modified rather than looked up again, an owner that drifted is set rather than
//! compared first, and a unit that is merely stopped is started without its file being read.
//! Nothing here measures what has already been measured.

use std::path::Path;

use mix_core::identity;
use mix_core::models::Target;
use mix_core::{CancellationToken, Result};

use crate::exec::run;
use crate::fs::{self, Owner};
use crate::systemd;
use crate::target::Finding;

pub async fn reconcile(target: &Target, finding: Finding, token: &CancellationToken) -> Result<()> {
    match target {
        Target::Directory { path, mode, owner } => {
            tracing::debug!("reconciling directory: {}", path.display());
            directory(path, *mode, *owner, finding).await
        }
        Target::File {
            path,
            expected,
            owner,
        } => {
            tracing::debug!("reconciling file: {}", path.display());
            file(path, expected.as_deref(), *owner, finding).await
        }
        Target::SeededFile { path, seed, owner } => {
            tracing::debug!("reconciling seeded file: {}", path.display());
            seeded_file(path, seed, *owner, finding).await
        }
        Target::Group { name, gid } => {
            tracing::debug!("reconciling group: {name}");
            group(name, *gid, finding, token).await
        }
        Target::GroupMember { group, user } => {
            tracing::debug!("reconciling {group} membership: {user}");
            run("gpasswd", &["--add", user, group], token).await
        }
        Target::User { n, uid, gid } => {
            tracing::debug!("reconciling user: {}", identity::user_name(*n));
            user(*n, *uid, *gid, finding, token).await
        }
        Target::SystemdUnit {
            name,
            src,
            dest,
            must_be_active,
        } => {
            tracing::debug!("reconciling systemd unit: {name}");
            systemd_unit(name, src, dest, *must_be_active, finding, token).await
        }
        // Part of the Nix installation rather than of the declared environment: a missing one is
        // bound to `Unfixable::MissingRuntime` and never reaches here.
        Target::PathExists { .. } => Ok(()),
    }
}

async fn directory(path: &Path, mode: u32, owner: Owner, finding: Finding) -> Result<()> {
    match finding {
        // The owner is what drifted, and the inspection measured it: set it and nothing else.
        Finding::Owner { .. } => fs::set_owner(path, owner).await,
        // Nothing readable is there, so it and every directory it needs are created owned.
        Finding::Missing | Finding::Unreadable { .. } => {
            fs::create_dir_all_owned(path, mode, owner).await
        }
        // The mode drifted; whether the owner did too was not measured, so it is checked.
        _ => {
            fs::set_mode(path, mode).await?;
            fs::set_owner_if_needed(path, owner).await.map(drop)
        }
    }
}

async fn file(path: &Path, expected: Option<&str>, owner: Owner, finding: Finding) -> Result<()> {
    match finding {
        Finding::Owner { .. } => fs::set_owner(path, owner).await,
        _ => {
            // A file with no declared contents is nix's to write: only its ownership is mix's.
            if let Some(expected) = expected {
                fs::write(path, expected).await?;
            }
            fs::set_owner_if_needed(path, owner).await.map(drop)
        }
    }
}

async fn seeded_file(path: &Path, seed: &str, owner: Owner, finding: Finding) -> Result<()> {
    match finding {
        Finding::Owner { .. } => fs::set_owner(path, owner).await,
        // The seed is only ever written into an empty place: whatever a user has installed
        // since is theirs, and the inspection never reports its contents as drift.
        _ => {
            fs::write(path, seed).await?;
            fs::set_owner_if_needed(path, owner).await.map(drop)
        }
    }
}

async fn group(name: &str, gid: u32, finding: Finding, token: &CancellationToken) -> Result<()> {
    let gid = gid.to_string();
    match finding {
        Finding::GroupGid { .. } => run("groupmod", &["--gid", &gid, name], token).await,
        _ => run("groupadd", &["--system", "--gid", &gid, name], token).await,
    }
}

async fn user(
    n: u32,
    uid: u32,
    gid: u32,
    finding: Finding,
    token: &CancellationToken,
) -> Result<()> {
    let name = identity::user_name(n);

    // The ids the account carries were read by the inspection, so only the ones that actually
    // drifted are modified — and the account is not looked up again to find out which.
    if let Finding::UserIds {
        actual: (actual_uid, actual_gid),
        ..
    } = finding
    {
        if actual_gid != gid {
            run("usermod", &["--gid", &gid.to_string(), &name], token).await?;
        }
        if actual_uid != uid {
            run("usermod", &["--uid", &uid.to_string(), &name], token).await?;
        }
        return Ok(());
    }

    run(
        "useradd",
        &[
            "--system",
            "--no-create-home",
            "--no-user-group",
            "--home-dir",
            identity::NIXBLD_HOME,
            "--shell",
            identity::NIXBLD_SHELL,
            "--uid",
            &uid.to_string(),
            "--gid",
            identity::NIXBLD_GROUP,
            "--groups",
            identity::NIXBLD_GROUP,
            "--comment",
            &format!("mix build user {n}"),
            &name,
        ],
        token,
    )
    .await
}

async fn systemd_unit(
    name: &str,
    src: &str,
    dest: &str,
    must_be_active: bool,
    finding: Finding,
    token: &CancellationToken,
) -> Result<()> {
    // The unit file is there and is the one mix ships; it is only stopped.
    if finding == Finding::UnitInactive {
        return systemd::enable_now(name, token).await;
    }

    fs::copy_atomic(src, dest).await?;
    systemd::daemon_reload(token).await?;

    if must_be_active && !systemd::unit_is_active(name).await {
        systemd::enable_now(name, token).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    use nix::unistd::{Gid, Uid};

    use super::*;
    use crate::target::{Error, apply};

    fn token() -> CancellationToken {
        CancellationToken::new()
    }

    #[tokio::test]
    async fn a_missing_directory_is_created_with_the_mode_it_was_declared_with() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("sticky");

        directory(&path, 0o1777, None, Finding::Missing)
            .await
            .unwrap();

        let meta = tokio::fs::metadata(&path).await.unwrap();
        assert!(meta.is_dir());
        assert_eq!(meta.permissions().mode() & 0o7777, 0o1777);
    }

    #[tokio::test]
    async fn every_directory_created_on_the_way_gets_the_declared_owner() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("a/b/c");
        let owner = (Uid::current().as_raw(), Gid::current().as_raw());

        directory(&path, 0o700, Some(owner), Finding::Missing)
            .await
            .unwrap();

        for p in [root.path().join("a"), root.path().join("a/b"), path.clone()] {
            let meta = tokio::fs::metadata(&p).await.unwrap();
            assert_eq!((meta.uid(), meta.gid()), owner);
        }
    }

    #[tokio::test]
    async fn a_directory_created_on_the_way_is_traversable_and_the_leaf_keeps_its_own_mode() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("a/b");

        directory(&path, 0o700, None, Finding::Missing)
            .await
            .unwrap();

        let intermediate = tokio::fs::metadata(root.path().join("a")).await.unwrap();
        assert_eq!(intermediate.permissions().mode() & 0o7777, 0o755);
        let leaf = tokio::fs::metadata(&path).await.unwrap();
        assert_eq!(leaf.permissions().mode() & 0o7777, 0o700);
    }

    #[tokio::test]
    async fn an_existing_ancestor_is_left_as_it_was() {
        let root = tempfile::tempdir().unwrap();
        let ancestor = root.path().join("a");
        std::fs::create_dir(&ancestor).unwrap();
        std::fs::set_permissions(&ancestor, std::fs::Permissions::from_mode(0o750)).unwrap();

        directory(&ancestor.join("b"), 0o700, None, Finding::Missing)
            .await
            .unwrap();

        let meta = tokio::fs::metadata(&ancestor).await.unwrap();
        assert_eq!(meta.permissions().mode() & 0o7777, 0o750);
    }

    #[tokio::test]
    async fn a_drifted_mode_is_set_in_place() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let target = Target::Directory {
            path: dir.path().to_path_buf(),
            mode: 0o755,
            owner: None,
        };

        assert!(apply(&target, &token()).await.unwrap());

        let meta = tokio::fs::metadata(dir.path()).await.unwrap();
        assert_eq!(meta.permissions().mode() & 0o7777, 0o755);
    }

    /// An owner that drifted was measured by the inspection, so it is set without the artifact
    /// being read again.
    #[tokio::test]
    async fn an_owner_that_drifted_is_set_from_what_was_measured() {
        let dir = tempfile::tempdir().unwrap();
        let owner = (Uid::current().as_raw(), Gid::current().as_raw());

        directory(
            dir.path(),
            0o755,
            Some(owner),
            Finding::Owner {
                actual: (999_999, 999_999),
                expected: owner,
            },
        )
        .await
        .unwrap();

        let meta = tokio::fs::metadata(dir.path()).await.unwrap();
        assert_eq!((meta.uid(), meta.gid()), owner);
    }

    #[tokio::test]
    async fn a_file_mix_writes_is_written_with_the_declared_contents() {
        let dir = tempfile::tempdir().unwrap();
        let target = Target::File {
            path: dir.path().join("nix.conf"),
            expected: Some("expected content".to_string()),
            owner: None,
        };

        assert!(apply(&target, &token()).await.unwrap());

        assert_eq!(
            std::fs::read_to_string(dir.path().join("nix.conf")).unwrap(),
            "expected content"
        );
    }

    #[tokio::test]
    async fn a_declared_file_is_written_into_a_directory_that_is_not_there_yet() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("etc/profile.d/nix.sh");

        file(&path, Some("snippet"), None, Finding::Missing)
            .await
            .unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "snippet");
    }

    #[tokio::test]
    async fn a_file_mix_does_not_write_keeps_whatever_is_in_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("flake.lock");
        std::fs::write(&path, "whatever nix wrote").unwrap();
        let owner = (Uid::current().as_raw(), Gid::current().as_raw());

        file(
            &path,
            None,
            Some(owner),
            Finding::Owner {
                actual: owner,
                expected: owner,
            },
        )
        .await
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "whatever nix wrote"
        );
    }

    #[tokio::test]
    async fn a_seeded_file_is_written_when_there_is_nothing_there() {
        let dir = tempfile::tempdir().unwrap();
        let target = Target::SeededFile {
            path: dir.path().join("state"),
            seed: "seed content".into(),
            owner: None,
        };

        assert!(apply(&target, &token()).await.unwrap());

        assert_eq!(
            std::fs::read_to_string(dir.path().join("state")).unwrap(),
            "seed content"
        );
    }

    /// The seed is a starting point, not a declaration: what `mix install` has written since is
    /// never reported as drift, so it is never reconciled away.
    #[tokio::test]
    async fn a_seeded_file_that_is_there_is_never_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state");
        std::fs::write(&path, "installed by the user since").unwrap();
        let target = Target::SeededFile {
            path: path.clone(),
            seed: "seed content".into(),
            owner: None,
        };

        assert!(!apply(&target, &token()).await.unwrap());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "installed by the user since"
        );
    }

    #[tokio::test]
    async fn an_enrolled_member_is_left_alone() {
        let target = Target::GroupMember {
            group: "root",
            user: "root".to_string(),
        };

        assert!(!apply(&target, &token()).await.unwrap());
    }

    #[tokio::test]
    async fn a_membership_for_an_account_that_is_gone_is_reported_rather_than_attempted() {
        let target = Target::GroupMember {
            group: "root",
            user: "mix-test-nonexistent-user-xyz".to_string(),
        };

        let error = apply(&target, &token()).await.unwrap_err();

        assert!(matches!(
            error,
            Error::Unrepairable {
                reason: crate::target::Unfixable::MissingUser,
                ..
            }
        ));
    }
}
