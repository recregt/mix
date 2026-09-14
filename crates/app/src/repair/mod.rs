use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use mix_core::identity;
use mix_core::models::{Target, targets};
use mix_core::{CancellationToken, Error as CoreError};

use crate::os::{files_match, path_exists, run, systemd_unit_is_active};

const DIR_MODE_MASK: u32 = 0o7777;

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

enum Outcome {
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
    let mut reports = Vec::new();
    for target in targets() {
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
    reports
}

async fn fix(target: &Target, token: &CancellationToken) -> Result<Outcome, Error> {
    match *target {
        Target::Directory { path, mode } => fix_directory(path, mode).await,
        Target::File { path, expected } => fix_file(path, expected).await,
        Target::Group { name, gid } => fix_group(name, gid, token).await,
        Target::User { n, uid, gid } => fix_user(n, uid, gid, token).await,
        Target::SystemdUnit {
            name,
            src,
            dest,
            must_be_active,
        } => fix_systemd_unit(name, src, dest, must_be_active, token).await,
        Target::PathExists { name, path } => fix_path_exists(name, path).await,
    }
}

async fn fix_directory(path: &str, mode: u32) -> Result<Outcome, Error> {
    match tokio::fs::metadata(path).await {
        Ok(meta) if meta.is_dir() => {
            if meta.permissions().mode() & DIR_MODE_MASK == mode {
                return Ok(Outcome::Healthy);
            }
            set_mode(path, mode).await?;
            Ok(Outcome::Repaired)
        }
        Ok(_) => Err(Error::Unrepairable {
            artifact: path.to_string(),
            hint: "exists but is not a directory; remove it manually and retry",
        }),
        Err(_) => {
            tracing::debug!("creating directory: {path}");
            tokio::fs::create_dir_all(path)
                .await
                .map_err(|source| io_error(path, source))?;
            set_mode(path, mode).await?;
            Ok(Outcome::Repaired)
        }
    }
}

async fn set_mode(path: &str, mode: u32) -> Result<(), Error> {
    tracing::debug!("setting permissions: {path} ({mode:o})");
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .await
        .map_err(|source| io_error(path, source))?;
    Ok(())
}

async fn fix_file(path: &str, expected: &str) -> Result<Outcome, Error> {
    if let Ok(contents) = tokio::fs::read_to_string(path).await
        && contents == expected
    {
        return Ok(Outcome::Healthy);
    }
    if let Some(parent) = std::path::Path::new(path).parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|source| io_error(path, source))?;
    }
    tracing::debug!("writing file: {path}");
    tokio::fs::write(path, expected)
        .await
        .map_err(|source| io_error(path, source))?;
    Ok(Outcome::Repaired)
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
            .map_err(|source| io_error(src, source))?;
        tokio::fs::write(dest, contents)
            .await
            .map_err(|source| io_error(dest, source))?;
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

fn io_error(path: &str, source: std::io::Error) -> CoreError {
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
        let path = path.to_str().unwrap();

        let outcome = fix_directory(path, 0o1777).await.unwrap();

        assert!(matches!(outcome, Outcome::Repaired));
        let meta = tokio::fs::metadata(path).await.unwrap();
        assert!(meta.is_dir());
        assert_eq!(meta.permissions().mode() & 0o7777, 0o1777);
    }

    #[tokio::test]
    async fn fix_directory_repairs_a_drifted_mode_in_place() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.path().to_str().unwrap();

        let outcome = fix_directory(path, 0o755).await.unwrap();

        assert!(matches!(outcome, Outcome::Repaired));
        let meta = tokio::fs::metadata(path).await.unwrap();
        assert_eq!(meta.permissions().mode() & 0o7777, 0o755);
    }

    #[tokio::test]
    async fn fix_directory_is_a_noop_when_already_healthy() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = dir.path().to_str().unwrap();

        let outcome = fix_directory(path, 0o755).await.unwrap();

        assert!(matches!(outcome, Outcome::Healthy));
    }

    #[tokio::test]
    async fn fix_directory_refuses_to_touch_a_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not-a-dir");
        std::fs::write(&file, "x").unwrap();

        let result = fix_directory(file.to_str().unwrap(), 0o755).await;

        assert!(matches!(result, Err(Error::Unrepairable { .. })));
    }

    #[tokio::test]
    async fn fix_file_writes_the_expected_content() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");

        let outcome = fix_file(file.to_str().unwrap(), "expected content")
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

        let outcome = fix_file(file.to_str().unwrap(), "expected content")
            .await
            .unwrap();

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
