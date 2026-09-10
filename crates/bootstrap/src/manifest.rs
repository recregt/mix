use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use mix_core::{Error, ManagedArtifact, Manifest, Result};

use crate::constants::{
    NIX_CONF, NIX_CONF_DEST, NIX_DAEMON_SERVICE_DEST, NIX_DAEMON_SOCKET_DEST, NIXBLD_GID,
    NIXBLD_GROUP, PROFILE_SNIPPET, PROFILE_SNIPPET_DEST,
};
use crate::steps::create_users_and_groups::{all_users_exist, group_has_gid};

pub const MANIFEST: Manifest = &[
    ManagedArtifact::Directory {
        path: "/nix",
        mode: 0o755,
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
    },
    ManagedArtifact::SystemdUnit {
        name: "nix-daemon.socket",
        dest: NIX_DAEMON_SOCKET_DEST,
    },
];

pub async fn verify() -> Result<()> {
    for artifact in MANIFEST {
        check_one(artifact).await?;
    }
    check_build_users()?;
    Ok(())
}

async fn check_one(artifact: &ManagedArtifact) -> Result<()> {
    match *artifact {
        ManagedArtifact::Directory { path, mode } => {
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
            if !group_has_gid(name, gid) {
                return Err(integrity(name, "group is missing or has the wrong gid"));
            }
        }
        ManagedArtifact::SystemdUnit { name, dest } => {
            if !Path::new(dest).exists() {
                return Err(integrity(name, "unit file missing"));
            }
        }
    }
    Ok(())
}

fn check_build_users() -> Result<()> {
    if !all_users_exist() {
        return Err(integrity(
            NIXBLD_GROUP,
            "build users are missing or incomplete",
        ));
    }
    Ok(())
}

fn integrity(artifact: &str, detail: &str) -> Error {
    Error::Integrity {
        artifact: artifact.to_string(),
        detail: detail.to_string(),
    }
}
