use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::constants::{
    DEFAULT_PROFILE_NIX_ENV, NIX_CONF, NIX_CONF_DEST, NIX_DAEMON_SERVICE_DEST,
    NIX_DAEMON_SOCKET_DEST, NIX_OWNERSHIP_MARKER, NIXBLD_GID, NIXBLD_GROUP, NIXBLD_USER_COUNT,
    PROFILE_SNIPPET, PROFILE_SNIPPET_DEST,
};
use crate::error::{Error, Result};
use crate::steps::create_users_and_groups::{all_users_valid, group_has_gid};

#[derive(Debug, Clone, Copy)]
pub enum ManagedArtifact {
    File {
        path: &'static str,
        expected: &'static str,
    },
    Directory {
        path: &'static str,
        mode: u32,
    },
    Group {
        name: &'static str,
        gid: u32,
    },
    SystemdUnit {
        name: &'static str,
        dest: &'static str,
        must_be_active: bool,
    },
    PathExists {
        name: &'static str,
        path: &'static str,
    },
}

impl ManagedArtifact {
    pub(crate) fn label(&self) -> &'static str {
        match *self {
            ManagedArtifact::Directory { path, .. } => path,
            ManagedArtifact::File { path, .. } => path,
            ManagedArtifact::Group { name, .. } => name,
            ManagedArtifact::SystemdUnit { name, .. } => name,
            ManagedArtifact::PathExists { name, .. } => name,
        }
    }

    pub(crate) async fn check(&self) -> Result<()> {
        match *self {
            ManagedArtifact::Directory { path, mode } => {
                tracing::debug!("checking directory: {path}");
                let meta = tokio::fs::metadata(path)
                    .await
                    .map_err(|_| integrity(path, "missing"))?;
                if !meta.is_dir() {
                    return Err(integrity(path, "exists but is not a directory"));
                }
                let actual_mode = meta.permissions().mode() & 0o777;
                if actual_mode != mode {
                    return Err(integrity(
                        path,
                        &format!("mode is {actual_mode:o}, expected {mode:o}"),
                    ));
                }
            }
            ManagedArtifact::File { path, expected } => {
                tracing::debug!("checking file: {path}");
                let contents = tokio::fs::read_to_string(path)
                    .await
                    .map_err(|_| integrity(path, "missing"))?;
                if contents != expected {
                    return Err(integrity(
                        path,
                        "configuration drift detected (contents modified)",
                    ));
                }
            }
            ManagedArtifact::Group { name, gid } => {
                tracing::debug!("checking group: {name}");
                if !group_has_gid(name, gid) {
                    return Err(integrity(name, "group is missing or has the wrong gid"));
                }
            }
            ManagedArtifact::SystemdUnit {
                name,
                dest,
                must_be_active,
            } => {
                tracing::debug!("checking systemd unit: {name}");
                if !Path::new(dest).exists() {
                    return Err(integrity(name, "unit file missing"));
                }
                if must_be_active && !crate::util::systemd_unit_is_active(name).await {
                    return Err(integrity(name, "unit is not active"));
                }
            }
            ManagedArtifact::PathExists { name, path } => {
                tracing::debug!("checking path: {path} ({name})");
                if !Path::new(path).exists() {
                    return Err(integrity(name, "missing"));
                }
            }
        }
        Ok(())
    }
}

pub type Manifest = &'static [ManagedArtifact];

pub const MANIFEST: Manifest = &[
    ManagedArtifact::Directory {
        path: "/nix",
        mode: 0o755,
    },
    ManagedArtifact::PathExists {
        name: "ownership marker",
        path: NIX_OWNERSHIP_MARKER,
    },
    ManagedArtifact::File {
        path: NIX_CONF_DEST,
        expected: NIX_CONF,
    },
    ManagedArtifact::File {
        path: PROFILE_SNIPPET_DEST,
        expected: PROFILE_SNIPPET,
    },
    ManagedArtifact::Group {
        name: NIXBLD_GROUP,
        gid: NIXBLD_GID,
    },
    ManagedArtifact::SystemdUnit {
        name: "nix-daemon.service",
        dest: NIX_DAEMON_SERVICE_DEST,
        must_be_active: false,
    },
    ManagedArtifact::SystemdUnit {
        name: "nix-daemon.socket",
        dest: NIX_DAEMON_SOCKET_DEST,
        must_be_active: true,
    },
    ManagedArtifact::PathExists {
        name: "default profile",
        path: DEFAULT_PROFILE_NIX_ENV,
    },
];

pub async fn verify() -> Result<()> {
    tracing::info!("verifying managed environment");
    for artifact in MANIFEST {
        artifact.check().await?;
    }
    check_build_users()?;
    Ok(())
}

fn check_build_users() -> Result<()> {
    tracing::debug!("checking build users: nixbld1..{NIXBLD_USER_COUNT}");
    check_build_users_with(all_users_valid())
}

fn check_build_users_with(all_valid: bool) -> Result<()> {
    if !all_valid {
        return Err(integrity(
            NIXBLD_GROUP,
            "build users are missing, incomplete, or have the wrong uid/gid",
        ));
    }
    Ok(())
}

fn integrity(artifact: &str, detail: &str) -> Error {
    tracing::debug!("integrity check failed: {artifact}: {detail}");
    Error::Integrity {
        artifact: artifact.to_string(),
        detail: detail.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leak(path: std::path::PathBuf) -> &'static str {
        Box::leak(path.to_str().unwrap().to_string().into_boxed_str())
    }

    #[test]
    fn label_uses_the_path_for_path_based_artifacts() {
        let artifact = ManagedArtifact::Directory {
            path: "/nix",
            mode: 0o755,
        };
        assert_eq!(artifact.label(), "/nix");
    }

    #[test]
    fn label_uses_the_name_for_name_based_artifacts() {
        let artifact = ManagedArtifact::Group {
            name: "nixbld",
            gid: 30_000,
        };
        assert_eq!(artifact.label(), "nixbld");
    }

    #[tokio::test]
    async fn directory_check_passes_when_mode_matches() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let artifact = ManagedArtifact::Directory {
            path: leak(dir.path().to_path_buf()),
            mode: 0o755,
        };
        assert!(artifact.check().await.is_ok());
    }

    #[tokio::test]
    async fn directory_check_fails_on_mode_drift() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let artifact = ManagedArtifact::Directory {
            path: leak(dir.path().to_path_buf()),
            mode: 0o755,
        };
        assert!(matches!(
            artifact.check().await,
            Err(Error::Integrity { .. })
        ));
    }

    #[tokio::test]
    async fn directory_check_fails_when_missing() {
        let artifact = ManagedArtifact::Directory {
            path: "/does/not/exist/mix-test",
            mode: 0o755,
        };
        assert!(artifact.check().await.is_err());
    }

    #[tokio::test]
    async fn directory_check_fails_when_path_is_a_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not-a-dir");
        std::fs::write(&file, "x").unwrap();
        let artifact = ManagedArtifact::Directory {
            path: leak(file),
            mode: 0o755,
        };
        assert!(matches!(
            artifact.check().await,
            Err(Error::Integrity { .. })
        ));
    }

    #[tokio::test]
    async fn group_check_passes_for_a_known_system_group() {
        let artifact = ManagedArtifact::Group {
            name: "root",
            gid: 0,
        };
        assert!(artifact.check().await.is_ok());
    }

    #[tokio::test]
    async fn group_check_fails_for_the_wrong_gid() {
        let artifact = ManagedArtifact::Group {
            name: "root",
            gid: 9999,
        };
        assert!(matches!(
            artifact.check().await,
            Err(Error::Integrity { .. })
        ));
    }

    #[tokio::test]
    async fn group_check_fails_for_a_nonexistent_group() {
        let artifact = ManagedArtifact::Group {
            name: "mix-test-nonexistent-group-xyz",
            gid: 0,
        };
        assert!(artifact.check().await.is_err());
    }

    #[test]
    fn check_build_users_ok_when_all_users_exist() {
        assert!(check_build_users_with(true).is_ok());
    }

    #[test]
    fn check_build_users_fails_when_a_user_is_missing() {
        assert!(matches!(
            check_build_users_with(false),
            Err(Error::Integrity { .. })
        ));
    }

    #[tokio::test]
    async fn file_check_passes_when_content_matches() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");
        std::fs::write(&file, "expected content").unwrap();
        let artifact = ManagedArtifact::File {
            path: leak(file),
            expected: "expected content",
        };
        assert!(artifact.check().await.is_ok());
    }

    #[tokio::test]
    async fn file_check_fails_on_content_drift() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");
        std::fs::write(&file, "modified content").unwrap();
        let artifact = ManagedArtifact::File {
            path: leak(file),
            expected: "expected content",
        };
        assert!(matches!(
            artifact.check().await,
            Err(Error::Integrity { .. })
        ));
    }

    #[tokio::test]
    async fn path_exists_check_passes_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("nix-env");
        std::fs::write(&file, "x").unwrap();
        let artifact = ManagedArtifact::PathExists {
            name: "default profile",
            path: leak(file),
        };
        assert!(artifact.check().await.is_ok());
    }

    #[tokio::test]
    async fn path_exists_check_fails_when_missing() {
        let artifact = ManagedArtifact::PathExists {
            name: "default profile",
            path: "/does/not/exist/nix-env",
        };
        assert!(matches!(
            artifact.check().await,
            Err(Error::Integrity { .. })
        ));
    }

    #[tokio::test]
    async fn systemd_unit_check_fails_when_missing() {
        let artifact = ManagedArtifact::SystemdUnit {
            name: "fake.service",
            dest: "/does/not/exist/fake.service",
            must_be_active: false,
        };
        assert!(artifact.check().await.is_err());
    }

    #[tokio::test]
    async fn systemd_unit_check_passes_when_present_and_activity_not_required() {
        let dir = tempfile::tempdir().unwrap();
        let unit = dir.path().join("fake.service");
        std::fs::write(&unit, "[Unit]").unwrap();
        let artifact = ManagedArtifact::SystemdUnit {
            name: "fake.service",
            dest: leak(unit),
            must_be_active: false,
        };
        assert!(artifact.check().await.is_ok());
    }

    #[tokio::test]
    async fn systemd_unit_check_fails_when_required_active_but_not_a_real_unit() {
        let dir = tempfile::tempdir().unwrap();
        let unit = dir.path().join("mix-test-fake.service");
        std::fs::write(&unit, "[Unit]").unwrap();
        let artifact = ManagedArtifact::SystemdUnit {
            name: "mix-test-fake.service",
            dest: leak(unit),
            must_be_active: true,
        };
        assert!(matches!(
            artifact.check().await,
            Err(Error::Integrity { .. })
        ));
    }
}
