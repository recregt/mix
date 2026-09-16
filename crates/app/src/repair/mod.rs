use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use mix_core::identity;
use mix_core::models::{Target, targets};
use mix_core::paths::mix_state_dir;
use mix_core::{CancellationToken, Error as CoreError};
use nix::unistd::{Gid, Uid, chown};

use crate::shared::git;
use crate::shared::home_manager::resolve_existing_user_config;
use crate::shared::os::{DIR_MODE_MASK, files_match, path_exists, run, systemd_unit_is_active};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Core(#[from] CoreError),

    #[error("{artifact}: {hint}")]
    Unrepairable {
        artifact: String,
        hint: &'static str,
    },
}

pub(crate) enum Outcome {
    Healthy,
    Repaired,
}

pub struct RepairReport {
    pub name: String,
    pub fixed: bool,
    pub detail: Option<String>,
}

pub async fn repair() -> Vec<RepairReport> {
    tracing::info!("repairing managed environment");
    let token = CancellationToken::new();
    let user_config = resolve_existing_user_config().await;
    let mut reports = Vec::new();
    for target in targets(user_config.as_ref()) {
        let name = target.label();
        tracing::debug!("checking: {name}");
        match fix(&target, &token).await {
            Ok(Outcome::Healthy) => {}
            Ok(Outcome::Repaired) => {
                tracing::debug!("repaired: {name}");
                reports.push(RepairReport {
                    name,
                    fixed: true,
                    detail: None,
                })
            }
            Err(e) => {
                tracing::debug!("failed to repair {name}: {e}");
                reports.push(RepairReport {
                    name,
                    fixed: false,
                    detail: Some(e.to_string()),
                })
            }
        }
    }

    if let Some(cfg) = &user_config {
        let state_dir = mix_state_dir(&cfg.user.home);
        match git::sync(&cfg.user, &state_dir, &token).await {
            Ok(true) => {
                tracing::debug!("committed drift in git-tracked state");
                reports.push(RepairReport {
                    name: "git-tracked state".to_string(),
                    fixed: true,
                    detail: None,
                })
            }
            Ok(false) => {}
            Err(e) => {
                tracing::debug!("failed to commit git-tracked state: {e}");
                reports.push(RepairReport {
                    name: "git-tracked state".to_string(),
                    fixed: false,
                    detail: Some(e.to_string()),
                })
            }
        }
    }

    reports
}

pub(crate) async fn fix(target: &Target, token: &CancellationToken) -> Result<Outcome, Error> {
    match target {
        Target::Directory { path, mode, owner } => fix_directory(path, *mode, *owner).await,
        Target::File {
            path,
            expected,
            owner,
        } => fix_file(path, expected.as_deref(), *owner).await,
        Target::Group { name, gid } => fix_group(name, *gid, token).await,
        Target::User { n, uid, gid } => fix_user(*n, *uid, *gid, token).await,
        Target::SystemdUnit {
            name,
            src,
            dest,
            must_be_active,
        } => fix_systemd_unit(name, src, dest, *must_be_active, token).await,
        Target::PathExists { name, path } => fix_path_exists(name, path).await,
    }
}

async fn fix_directory(
    path: &Path,
    mode: u32,
    owner: Option<(u32, u32)>,
) -> Result<Outcome, Error> {
    let mut repaired = match tokio::fs::metadata(path).await {
        Ok(meta) if meta.is_dir() => {
            if meta.permissions().mode() & DIR_MODE_MASK == mode {
                false
            } else {
                set_mode(path, mode).await?;
                true
            }
        }
        Ok(_) => {
            return Err(Error::Unrepairable {
                artifact: path.display().to_string(),
                hint: "exists but is not a directory; remove it manually and retry",
            });
        }
        Err(_) => {
            create_dir_all_owned(path, mode, owner).await?;
            true
        }
    };

    if set_owner_if_needed(path, owner).await? {
        repaired = true;
    }

    Ok(if repaired {
        Outcome::Repaired
    } else {
        Outcome::Healthy
    })
}

const INTERMEDIATE_DIR_MODE: u32 = 0o755;

async fn create_dir_all_owned(
    path: &Path,
    mode: u32,
    owner: Option<(u32, u32)>,
) -> Result<(), Error> {
    let mut missing = Vec::new();
    let mut cur = path;
    loop {
        if tokio::fs::metadata(cur).await.is_ok() {
            break;
        }
        missing.push(cur);
        match cur.parent() {
            Some(parent) => cur = parent,
            None => break,
        }
    }

    for dir in missing.into_iter().rev() {
        tracing::debug!("creating directory: {}", dir.display());
        match tokio::fs::create_dir(dir).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(io_error(dir, e).into()),
        }
        let dir_mode = if dir == path {
            mode
        } else {
            INTERMEDIATE_DIR_MODE
        };
        set_mode(dir, dir_mode).await?;
        set_owner_if_needed(dir, owner).await?;
    }

    Ok(())
}

async fn set_mode(path: &Path, mode: u32) -> Result<(), Error> {
    tracing::debug!("setting permissions: {} ({mode:o})", path.display());
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .await
        .map_err(|source| io_error(path, source))?;
    Ok(())
}

async fn set_owner_if_needed(path: &Path, owner: Option<(u32, u32)>) -> Result<bool, Error> {
    let Some((uid, gid)) = owner else {
        return Ok(false);
    };
    let meta = tokio::fs::metadata(path)
        .await
        .map_err(|source| io_error(path, source))?;
    if std::os::unix::fs::MetadataExt::uid(&meta) == uid
        && std::os::unix::fs::MetadataExt::gid(&meta) == gid
    {
        return Ok(false);
    }
    tracing::debug!("setting owner: {} ({uid}:{gid})", path.display());
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        chown(&path, Some(Uid::from_raw(uid)), Some(Gid::from_raw(gid)))
            .map_err(|e| io_error(&path, std::io::Error::from(e)))
    })
    .await
    .map_err(|e| CoreError::TaskPanicked(e.to_string()))??;
    Ok(true)
}

async fn fix_file(
    path: &Path,
    expected: Option<&str>,
    owner: Option<(u32, u32)>,
) -> Result<Outcome, Error> {
    let mut repaired = false;

    match expected {
        Some(expected) => {
            let matches = matches!(
                tokio::fs::read_to_string(path).await,
                Ok(contents) if contents == expected
            );
            if !matches {
                if let Some(parent) = path.parent() {
                    tokio::fs::create_dir_all(parent)
                        .await
                        .map_err(|source| io_error(parent, source))?;
                }
                tracing::debug!("writing file: {}", path.display());
                tokio::fs::write(path, expected)
                    .await
                    .map_err(|source| io_error(path, source))?;
                repaired = true;
            }
        }
        None if !path_exists(path).await => {
            return Ok(Outcome::Healthy);
        }
        None => {}
    }

    if set_owner_if_needed(path, owner).await? {
        repaired = true;
    }

    Ok(if repaired {
        Outcome::Repaired
    } else {
        Outcome::Healthy
    })
}

async fn fix_group(name: &str, gid: u32, token: &CancellationToken) -> Result<Outcome, Error> {
    if identity::group_exists(name) {
        if identity::group_has_gid(name, gid) {
            return Ok(Outcome::Healthy);
        }
        run("groupmod", &["--gid", &gid.to_string(), name], token).await?;
        return Ok(Outcome::Repaired);
    }
    run(
        "groupadd",
        &["--system", "--gid", &gid.to_string(), name],
        token,
    )
    .await?;
    Ok(Outcome::Repaired)
}

async fn fix_user(n: u32, uid: u32, gid: u32, token: &CancellationToken) -> Result<Outcome, Error> {
    let name = identity::user_name(n);

    if identity::user_exists(&name) {
        if identity::user_has_uid(&name, uid) && identity::user_has_gid(&name, gid) {
            return Ok(Outcome::Healthy);
        }
        if !identity::user_has_gid(&name, gid) {
            run("usermod", &["--gid", &gid.to_string(), &name], token).await?;
        }
        if !identity::user_has_uid(&name, uid) {
            run("usermod", &["--uid", &uid.to_string(), &name], token).await?;
        }
        return Ok(Outcome::Repaired);
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
    .await?;
    Ok(Outcome::Repaired)
}

async fn fix_systemd_unit(
    name: &str,
    src: &str,
    dest: &str,
    must_be_active: bool,
    token: &CancellationToken,
) -> Result<Outcome, Error> {
    let mut changed = false;

    if !files_match(src, dest).await {
        tracing::debug!("copying unit file: {src} -> {dest}");
        let contents = tokio::fs::read(src)
            .await
            .map_err(|source| io_error(Path::new(src), source))?;
        tokio::fs::write(dest, contents)
            .await
            .map_err(|source| io_error(Path::new(dest), source))?;
        run("systemctl", &["daemon-reload"], token).await?;
        changed = true;
    }

    if must_be_active && !systemd_unit_is_active(name).await {
        run("systemctl", &["enable", "--now", name], token).await?;
        changed = true;
    }

    Ok(if changed {
        Outcome::Repaired
    } else {
        Outcome::Healthy
    })
}

async fn fix_path_exists(name: &str, path: &str) -> Result<Outcome, Error> {
    if path_exists(path).await {
        Ok(Outcome::Healthy)
    } else {
        Err(Error::Unrepairable {
            artifact: name.to_string(),
            hint: "produced by the Nix installation itself; run `mix bootstrap` to restore it",
        })
    }
}

fn io_error(path: &Path, source: std::io::Error) -> CoreError {
    CoreError::Io {
        path: PathBuf::from(path),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fix_directory_creates_a_missing_directory_with_the_right_mode() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("sticky");

        let outcome = fix_directory(&path, 0o1777, None).await.unwrap();

        assert!(matches!(outcome, Outcome::Repaired));
        let meta = tokio::fs::metadata(&path).await.unwrap();
        assert!(meta.is_dir());
        assert_eq!(meta.permissions().mode() & 0o7777, 0o1777);
    }

    #[tokio::test]
    async fn fix_directory_owns_every_intermediate_directory_it_creates() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("a/b/c");
        let uid = Uid::current().as_raw();
        let gid = Gid::current().as_raw();

        let outcome = fix_directory(&path, 0o700, Some((uid, gid))).await.unwrap();

        assert!(matches!(outcome, Outcome::Repaired));
        for p in [root.path().join("a"), root.path().join("a/b"), path.clone()] {
            let meta = tokio::fs::metadata(&p).await.unwrap();
            assert_eq!(std::os::unix::fs::MetadataExt::uid(&meta), uid);
            assert_eq!(std::os::unix::fs::MetadataExt::gid(&meta), gid);
        }
    }

    #[tokio::test]
    async fn fix_directory_gives_intermediate_directories_a_traversable_mode_and_the_leaf_its_own()
    {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("a/b");

        fix_directory(&path, 0o700, None).await.unwrap();

        let intermediate = tokio::fs::metadata(root.path().join("a")).await.unwrap();
        assert_eq!(intermediate.permissions().mode() & 0o7777, 0o755);
        let leaf = tokio::fs::metadata(&path).await.unwrap();
        assert_eq!(leaf.permissions().mode() & 0o7777, 0o700);
    }

    #[tokio::test]
    async fn fix_directory_leaves_an_existing_ancestor_untouched() {
        let root = tempfile::tempdir().unwrap();
        let ancestor = root.path().join("a");
        std::fs::create_dir(&ancestor).unwrap();
        std::fs::set_permissions(&ancestor, std::fs::Permissions::from_mode(0o750)).unwrap();
        let path = ancestor.join("b");

        fix_directory(&path, 0o700, None).await.unwrap();

        let meta = tokio::fs::metadata(&ancestor).await.unwrap();
        assert_eq!(meta.permissions().mode() & 0o7777, 0o750);
    }

    #[tokio::test]
    async fn fix_directory_repairs_a_drifted_mode_in_place() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();

        let outcome = fix_directory(dir.path(), 0o755, None).await.unwrap();

        assert!(matches!(outcome, Outcome::Repaired));
        let meta = tokio::fs::metadata(dir.path()).await.unwrap();
        assert_eq!(meta.permissions().mode() & 0o7777, 0o755);
    }

    #[tokio::test]
    async fn fix_directory_is_a_noop_when_already_healthy() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();

        let outcome = fix_directory(dir.path(), 0o755, None).await.unwrap();

        assert!(matches!(outcome, Outcome::Healthy));
    }

    #[tokio::test]
    async fn fix_directory_refuses_to_touch_a_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not-a-dir");
        std::fs::write(&file, "x").unwrap();

        let result = fix_directory(&file, 0o755, None).await;

        assert!(matches!(result, Err(Error::Unrepairable { .. })));
    }

    #[tokio::test]
    async fn fix_directory_is_healthy_when_owner_already_matches() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let uid = Uid::current().as_raw();
        let gid = Gid::current().as_raw();

        let outcome = fix_directory(dir.path(), 0o755, Some((uid, gid)))
            .await
            .unwrap();

        assert!(matches!(outcome, Outcome::Healthy));
    }

    #[tokio::test]
    async fn fix_file_writes_the_expected_content() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");

        let outcome = fix_file(&file, Some("expected content"), None)
            .await
            .unwrap();

        assert!(matches!(outcome, Outcome::Repaired));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "expected content");
    }

    #[tokio::test]
    async fn fix_file_is_a_noop_when_content_already_matches() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");
        std::fs::write(&file, "expected content").unwrap();

        let outcome = fix_file(&file, Some("expected content"), None)
            .await
            .unwrap();

        assert!(matches!(outcome, Outcome::Healthy));
    }

    #[tokio::test]
    async fn fix_file_is_healthy_when_owner_already_matches() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");
        std::fs::write(&file, "expected content").unwrap();
        let uid = Uid::current().as_raw();
        let gid = Gid::current().as_raw();

        let outcome = fix_file(&file, Some("expected content"), Some((uid, gid)))
            .await
            .unwrap();

        assert!(matches!(outcome, Outcome::Healthy));
    }

    #[tokio::test]
    async fn fix_file_with_no_expected_content_is_healthy_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("flake.lock");

        let outcome = fix_file(&file, None, None).await.unwrap();

        assert!(matches!(outcome, Outcome::Healthy));
        assert!(!file.exists());
    }

    #[tokio::test]
    async fn fix_file_with_no_expected_content_never_rewrites_present_content() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("flake.lock");
        std::fs::write(&file, "whatever nix wrote").unwrap();

        let outcome = fix_file(&file, None, None).await.unwrap();

        assert!(matches!(outcome, Outcome::Healthy));
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "whatever nix wrote"
        );
    }

    #[tokio::test]
    async fn fix_file_with_no_expected_content_still_fixes_ownership_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("flake.lock");
        std::fs::write(&file, "whatever nix wrote").unwrap();
        let uid = Uid::current().as_raw();
        let gid = Gid::current().as_raw();

        let outcome = fix_file(&file, None, Some((uid, gid))).await.unwrap();

        assert!(matches!(outcome, Outcome::Healthy));
    }

    #[tokio::test]
    async fn path_exists_target_reports_unrepairable_when_missing() {
        let result = fix_path_exists("default profile", "/does/not/exist/nix-env").await;

        assert!(matches!(result, Err(Error::Unrepairable { .. })));
    }

    #[tokio::test]
    async fn path_exists_target_is_healthy_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("nix-env");
        std::fs::write(&file, "x").unwrap();

        let outcome = fix_path_exists("default profile", file.to_str().unwrap())
            .await
            .unwrap();

        assert!(matches!(outcome, Outcome::Healthy));
    }
}
