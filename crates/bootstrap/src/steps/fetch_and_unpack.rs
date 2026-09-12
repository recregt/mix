use std::io::Write as _;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use mix_core::{Error, Result, Step};
use nix::fcntl::{AT_FDCWD, AtFlags};
use nix::unistd::{Gid, Uid, User, fchownat};

use crate::constants::NIXBLD_GID;
use crate::pins::NIX_VERSION;
use crate::tarball;

const NIX_STORE: &str = "/nix/store";
const DEFAULT_PROFILE: &str = "/nix/var/nix/profiles/default";

pub struct FetchAndUnpack {
    mirror: Option<String>,
}

impl FetchAndUnpack {
    pub fn new(mirror: Option<&str>) -> Self {
        Self {
            mirror: mirror.map(String::from),
        }
    }
}

#[async_trait]
impl Step for FetchAndUnpack {
    fn name(&self) -> &'static str {
        "fetch and activate the managed runtime"
    }

    async fn check(&self) -> Result<bool> {
        Ok(Path::new(DEFAULT_PROFILE).join("bin/nix-env").is_file())
    }

    async fn execute(&mut self) -> Result<()> {
        let bytes = tarball::bytes(self.mirror.as_deref()).await?;

        tokio::task::spawn_blocking(move || provision(&bytes))
            .await
            .map_err(|e| Error::TaskPanicked(e.to_string()))??;

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
    tracing::debug!("moving Nix store into place");
    move_store_into_place(&unpacked_root)?;
    tracing::debug!("fixing Nix store ownership");
    ensure_store_ownership()?;

    let nix_pkg = resolve_backlink(&unpacked_root, |name| {
        name.ends_with(&format!("-nix-{NIX_VERSION}"))
    })?;
    let nss_cacert_pkg = resolve_backlink(&unpacked_root, |name| name.contains("-nss-cacert-"))?;

    tracing::debug!("loading Nix database");
    load_db(&nix_pkg, &unpacked_root.join(".reginfo"))?;
    tracing::debug!("activating default profile");
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
        n => Err(Error::MalformedArchive(format!(
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
    move_entries_into(&unpacked_root.join("store"), Path::new(NIX_STORE))
}

fn move_entries_into(src_store: &Path, dest_store: &Path) -> Result<()> {
    std::fs::create_dir_all(dest_store).map_err(|e| Error::Io {
        path: dest_store.to_path_buf(),
        source: e,
    })?;

    let entries: Vec<_> = std::fs::read_dir(src_store)
        .map_err(|e| Error::Io {
            path: src_store.to_path_buf(),
            source: e,
        })?
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|e| Error::Io {
            path: src_store.to_path_buf(),
            source: e,
        })?;

    for entry in entries {
        let src_path = entry.path();
        let dest_path = dest_store.join(entry.file_name());

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
    ensure_ownership_under(
        Path::new(NIX_STORE),
        Uid::from_raw(0),
        Gid::from_raw(NIXBLD_GID),
    )
}

fn ensure_ownership_under(root: &Path, uid: Uid, gid: Gid) -> Result<()> {
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

        if meta.gid() != gid.as_raw() || meta.uid() != uid.as_raw() {
            fchownat(
                AT_FDCWD,
                &path,
                Some(uid),
                Some(gid),
                AtFlags::AT_SYMLINK_NOFOLLOW,
            )
            .map_err(|e| Error::Io {
                path: path.clone(),
                source: std::io::Error::from(e),
            })?;
        }
    }
    Ok(())
}

fn load_db(nix_pkg: &Path, reginfo_path: &Path) -> Result<()> {
    let reginfo = std::fs::read(reginfo_path).map_err(|e| Error::Io {
        path: reginfo_path.to_path_buf(),
        source: e,
    })?;

    let nix_store = nix_pkg.join("bin/nix-store");
    tracing::debug!("running command: {} --load-db", nix_store.display());

    let mut child = std::process::Command::new(&nix_store)
        .arg("--load-db")
        .env("HOME", root_home())
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

    tracing::trace!(
        "command output: nix-store --load-db\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    if !output.status.success() {
        return Err(crate::util::command_error("nix-store --load-db", &output));
    }

    Ok(())
}

fn activate_default_profile(nix_pkg: &Path, nss_cacert_pkg: &Path) -> Result<()> {
    let nix_env = nix_pkg.join("bin/nix-env");
    tracing::debug!(
        "running command: {} --profile {DEFAULT_PROFILE} --install {} {}",
        nix_env.display(),
        nix_pkg.display(),
        nss_cacert_pkg.display()
    );

    let output = std::process::Command::new(&nix_env)
        .arg("--profile")
        .arg(DEFAULT_PROFILE)
        .arg("--install")
        .arg(nix_pkg)
        .arg(nss_cacert_pkg)
        .args(["--option", "substitute", "false"])
        .args(["--option", "post-build-hook", ""])
        .env("HOME", root_home())
        .env_remove("NIX_REMOTE")
        .output()
        .map_err(|e| Error::Command {
            command: "nix-env --install".into(),
            detail: e.to_string(),
        })?;

    tracing::trace!(
        "command output: nix-env --install\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    if !output.status.success() {
        return Err(crate::util::command_error("nix-env --install", &output));
    }

    Ok(())
}

fn root_home() -> String {
    root_home_with(std::env::var("HOME").ok(), || {
        User::from_uid(Uid::from_raw(0))
            .ok()
            .flatten()
            .map(|u| u.dir)
    })
}

fn root_home_with(
    home_env: Option<String>,
    passwd_dir: impl FnOnce() -> Option<PathBuf>,
) -> String {
    if let Some(home) = home_env.filter(|home| !home.is_empty()) {
        return home;
    }

    passwd_dir()
        .and_then(|dir| dir.to_str().map(str::to_string))
        .unwrap_or_else(|| "/root".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_home_with_prefers_non_empty_home_env() {
        assert_eq!(
            root_home_with(Some("/home/nix".to_string()), || Some(PathBuf::from(
                "/should-not-be-used"
            ))),
            "/home/nix"
        );
    }

    #[test]
    fn root_home_with_falls_back_to_passwd_dir_when_home_is_missing() {
        assert_eq!(
            root_home_with(None, || Some(PathBuf::from("/var/lib/root"))),
            "/var/lib/root"
        );
    }

    #[test]
    fn root_home_with_falls_back_to_passwd_dir_when_home_is_empty() {
        assert_eq!(
            root_home_with(Some(String::new()), || Some(PathBuf::from("/root"))),
            "/root"
        );
    }

    #[test]
    fn root_home_with_falls_back_to_slash_root_when_passwd_lookup_fails() {
        assert_eq!(root_home_with(None, || None), "/root");
    }

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

    #[test]
    fn move_entries_into_relocates_and_symlinks_back() {
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("pkg-a"), "content").unwrap();

        let dest = tempfile::tempdir().unwrap();
        let dest_store = dest.path().join("store");

        move_entries_into(src.path(), &dest_store).unwrap();

        let moved = dest_store.join("pkg-a");
        assert!(moved.is_file());
        assert_eq!(std::fs::read_to_string(&moved).unwrap(), "content");

        let backlink = src.path().join("pkg-a");
        assert!(backlink.is_symlink());
        assert_eq!(std::fs::read_link(&backlink).unwrap(), moved);
    }

    #[test]
    fn move_entries_into_makes_moved_files_read_only() {
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("pkg-a"), "content").unwrap();

        let dest = tempfile::tempdir().unwrap();
        let dest_store = dest.path().join("store");

        move_entries_into(src.path(), &dest_store).unwrap();

        let mode = std::fs::metadata(dest_store.join("pkg-a"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o222, 0);
    }

    #[test]
    fn move_entries_into_replaces_an_existing_destination_entry() {
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("pkg-a"), "new content").unwrap();

        let dest = tempfile::tempdir().unwrap();
        let dest_store = dest.path().join("store");
        std::fs::create_dir_all(&dest_store).unwrap();
        std::fs::write(dest_store.join("pkg-a"), "stale content").unwrap();

        move_entries_into(src.path(), &dest_store).unwrap();

        assert_eq!(
            std::fs::read_to_string(dest_store.join("pkg-a")).unwrap(),
            "new content"
        );
    }

    #[test]
    fn make_read_only_strips_write_bits_recursively() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        std::fs::write(nested.join("file"), "x").unwrap();

        make_read_only(dir.path()).unwrap();

        let mode = std::fs::metadata(nested.join("file"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o222, 0);
    }

    #[test]
    fn make_read_only_does_not_follow_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        std::fs::write(target.path().join("outside"), "x").unwrap();
        std::os::unix::fs::symlink(target.path().join("outside"), dir.path().join("link")).unwrap();

        make_read_only(dir.path()).unwrap();

        let mode = std::fs::metadata(target.path().join("outside"))
            .unwrap()
            .permissions()
            .mode();
        assert_ne!(mode & 0o222, 0);
    }

    #[test]
    fn ensure_ownership_under_is_a_noop_when_already_matching() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f"), "x").unwrap();

        let uid = Uid::current();
        let gid = Gid::current();

        assert!(ensure_ownership_under(dir.path(), uid, gid).is_ok());
    }

    #[test]
    fn ensure_ownership_under_skips_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(target.path(), dir.path().join("link")).unwrap();

        let uid = Uid::current();
        let gid = Gid::current();

        assert!(ensure_ownership_under(dir.path(), uid, gid).is_ok());
    }
}
