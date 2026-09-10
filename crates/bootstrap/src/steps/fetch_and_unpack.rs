use std::io::Write as _;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use mix_core::{Error, Result, Step};
use nix::fcntl::{AT_FDCWD, AtFlags};
use nix::unistd::{Gid, Uid, fchownat};

use crate::constants::NIXBLD_GID;
use crate::pins::NIX_VERSION;
use crate::tarball;

const NIX_STORE: &str = "/nix/store";
const DEFAULT_PROFILE: &str = "/nix/var/nix/profiles/default";

pub struct FetchAndUnpack;

#[async_trait]
impl Step for FetchAndUnpack {
    fn name(&self) -> &'static str {
        "fetch, unpack, and activate Nix"
    }

    async fn check(&self) -> Result<bool> {
        Ok(Path::new(DEFAULT_PROFILE).join("bin/nix-env").is_file())
    }

    async fn execute(&mut self) -> Result<()> {
        let bytes = tarball::bytes().await?;

        tokio::task::spawn_blocking(move || provision(&bytes))
            .await
            .map_err(|e| Error::Other(format!("provisioning task panicked: {e}")))??;

        Ok(())
    }
}

fn provision(tarball_bytes: &[u8]) -> Result<()> {
    let scratch = tempfile::Builder::new()
        .prefix("temp-install-dir-")
        .tempdir_in("/nix")
        .map_err(|e| Error::Io {
            path: "/nix".into(),
            source: e,
        })?;

    tarball::unpack(tarball_bytes, scratch.path())?;

    let unpacked_root = find_single_child(scratch.path(), |name| name.starts_with("nix-"))?;
    move_store_into_place(&unpacked_root)?;
    ensure_store_ownership()?;

    let nix_pkg = resolve_backlink(&unpacked_root, |name| {
        name.ends_with(&format!("-nix-{NIX_VERSION}"))
    })?;
    let nss_cacert_pkg = resolve_backlink(&unpacked_root, |name| name.contains("-nss-cacert-"))?;

    load_db(&nix_pkg, &unpacked_root.join(".reginfo"))?;
    activate_default_profile(&nix_pkg, &nss_cacert_pkg)?;

    Ok(())
}

fn find_single_child(dir: &Path, pred: impl Fn(&str) -> bool) -> Result<PathBuf> {
    let entries = std::fs::read_dir(dir).map_err(|e| Error::Io {
        path: dir.to_path_buf(),
        source: e,
    })?;

    let mut matches = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| Error::Io {
            path: dir.to_path_buf(),
            source: e,
        })?;
        if pred(&entry.file_name().to_string_lossy()) {
            matches.push(entry.path());
        }
    }

    match matches.len() {
        1 => Ok(matches.remove(0)),
        n => Err(Error::Other(format!(
            "expected exactly one matching entry in {}, found {n}",
            dir.display()
        ))),
    }
}

fn resolve_backlink(unpacked_root: &Path, pred: impl Fn(&str) -> bool) -> Result<PathBuf> {
    let link = find_single_child(&unpacked_root.join("store"), pred)?;
    std::fs::read_link(&link).map_err(|e| Error::Io {
        path: link,
        source: e,
    })
}

fn move_store_into_place(unpacked_root: &Path) -> Result<()> {
    let src_store = unpacked_root.join("store");
    std::fs::create_dir_all(NIX_STORE).map_err(|e| Error::Io {
        path: NIX_STORE.into(),
        source: e,
    })?;

    let entries: Vec<_> = std::fs::read_dir(&src_store)
        .map_err(|e| Error::Io {
            path: src_store.clone(),
            source: e,
        })?
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|e| Error::Io {
            path: src_store.clone(),
            source: e,
        })?;

    for entry in entries {
        let src_path = entry.path();
        let dest_path = Path::new(NIX_STORE).join(entry.file_name());

        if dest_path.exists() {
            let remove = if dest_path.is_dir() {
                std::fs::remove_dir_all(&dest_path)
            } else {
                std::fs::remove_file(&dest_path)
            };
            remove.map_err(|e| Error::Io {
                path: dest_path.clone(),
                source: e,
            })?;
        }

        std::fs::rename(&src_path, &dest_path).map_err(|e| Error::Io {
            path: dest_path.clone(),
            source: e,
        })?;
        make_read_only(&dest_path)?;

        std::os::unix::fs::symlink(&dest_path, &src_path).map_err(|e| Error::Io {
            path: src_path,
            source: e,
        })?;
    }

    Ok(())
}

fn make_read_only(root: &Path) -> Result<()> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        let meta = std::fs::symlink_metadata(&path).map_err(|e| Error::Io {
            path: path.clone(),
            source: e,
        })?;
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            let entries = std::fs::read_dir(&path).map_err(|e| Error::Io {
                path: path.clone(),
                source: e,
            })?;
            for entry in entries {
                let entry = entry.map_err(|e| Error::Io {
                    path: path.clone(),
                    source: e,
                })?;
                stack.push(entry.path());
            }
        }
        let mut perms = meta.permissions();
        perms.set_mode(perms.mode() & !0o222);
        std::fs::set_permissions(&path, perms).map_err(|e| Error::Io { path, source: e })?;
    }
    Ok(())
}

fn ensure_store_ownership() -> Result<()> {
    let target_gid = Gid::from_raw(NIXBLD_GID);
    let root_uid = Uid::from_raw(0);

    let mut stack = vec![PathBuf::from(NIX_STORE)];
    while let Some(path) = stack.pop() {
        let meta = std::fs::symlink_metadata(&path).map_err(|e| Error::Io {
            path: path.clone(),
            source: e,
        })?;
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            let entries = std::fs::read_dir(&path).map_err(|e| Error::Io {
                path: path.clone(),
                source: e,
            })?;
            for entry in entries {
                let entry = entry.map_err(|e| Error::Io {
                    path: path.clone(),
                    source: e,
                })?;
                stack.push(entry.path());
            }
        }

        if meta.gid() != NIXBLD_GID || meta.uid() != 0 {
            fchownat(
                AT_FDCWD,
                &path,
                Some(root_uid),
                Some(target_gid),
                AtFlags::AT_SYMLINK_NOFOLLOW,
            )
            .map_err(|e| Error::Other(format!("chown {}: {e}", path.display())))?;
        }
    }
    Ok(())
}

fn load_db(nix_pkg: &Path, reginfo_path: &Path) -> Result<()> {
    let reginfo = std::fs::read(reginfo_path).map_err(|e| Error::Io {
        path: reginfo_path.to_path_buf(),
        source: e,
    })?;

    let mut child = std::process::Command::new(nix_pkg.join("bin/nix-store"))
        .arg("--load-db")
        .env("HOME", root_home()?)
        .env_remove("NIX_REMOTE")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| Error::Command {
            command: "nix-store --load-db".into(),
            detail: e.to_string(),
        })?;

    child
        .stdin
        .take()
        .expect("stdin was piped")
        .write_all(&reginfo)
        .map_err(|e| Error::Command {
            command: "nix-store --load-db".into(),
            detail: e.to_string(),
        })?;

    let output = child.wait_with_output().map_err(|e| Error::Command {
        command: "nix-store --load-db".into(),
        detail: e.to_string(),
    })?;
    if !output.status.success() {
        return Err(Error::Command {
            command: "nix-store --load-db".into(),
            detail: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }

    Ok(())
}

fn activate_default_profile(nix_pkg: &Path, nss_cacert_pkg: &Path) -> Result<()> {
    let output = std::process::Command::new(nix_pkg.join("bin/nix-env"))
        .arg("--profile")
        .arg(DEFAULT_PROFILE)
        .arg("--install")
        .arg(nix_pkg)
        .arg(nss_cacert_pkg)
        .args(["--option", "substitute", "false"])
        .args(["--option", "post-build-hook", ""])
        .env("HOME", root_home()?)
        .env_remove("NIX_REMOTE")
        .output()
        .map_err(|e| Error::Command {
            command: "nix-env --install".into(),
            detail: e.to_string(),
        })?;

    if !output.status.success() {
        return Err(Error::Command {
            command: "nix-env --install".into(),
            detail: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }

    Ok(())
}

fn root_home() -> Result<String> {
    std::env::var("HOME").map_err(|_| {
        Error::Other("$HOME is not set -- mix expects to run as root via `sudo --set-home`".into())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_single_child_matches_exactly_one() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("nix-2.35.2-x86_64-linux")).unwrap();
        std::fs::create_dir(dir.path().join("not-nix")).unwrap();

        let found = find_single_child(dir.path(), |name| name.starts_with("nix-")).unwrap();
        assert_eq!(found, dir.path().join("nix-2.35.2-x86_64-linux"));
    }

    #[test]
    fn find_single_child_rejects_ambiguous_matches() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("aaaa-nix-2.35.2")).unwrap();
        std::fs::create_dir(dir.path().join("bbbb-nix-cmd-2.35.2")).unwrap();
        std::fs::create_dir(dir.path().join("cccc-nix-util-2.35.2")).unwrap();

        assert!(find_single_child(dir.path(), |name| name.contains("-nix-")).is_err());

        let found = find_single_child(dir.path(), |name| {
            name.ends_with(&format!("-nix-{NIX_VERSION}"))
        })
        .unwrap();
        assert_eq!(found, dir.path().join("aaaa-nix-2.35.2"));
    }

    #[test]
    fn resolve_backlink_follows_symlink_left_by_the_move() {
        let root = tempfile::tempdir().unwrap();
        let store = root.path().join("store");
        std::fs::create_dir(&store).unwrap();

        let real_location = tempfile::tempdir().unwrap();
        let real_pkg = real_location.path().join("hash-nss-cacert-3.123");
        std::fs::create_dir(&real_pkg).unwrap();
        std::os::unix::fs::symlink(&real_pkg, store.join("hash-nss-cacert-3.123")).unwrap();

        let resolved = resolve_backlink(root.path(), |name| name.contains("-nss-cacert-")).unwrap();
        assert_eq!(resolved, real_pkg);
    }
}
