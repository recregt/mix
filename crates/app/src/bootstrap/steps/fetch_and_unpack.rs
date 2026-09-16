use std::io::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use mix_core::{CancellationToken, DownloadProgress, Error as CoreError, Step};
use nix::unistd::{Gid, Uid, User};

use mix_core::identity::NIXBLD_GID;
use mix_core::paths::{NIX_PROVISIONING_MANIFEST, NIX_STORE};

use crate::bootstrap::error::{Error, Result};
use crate::bootstrap::pins::NIX_VERSION;
use crate::bootstrap::tarball;
use crate::bootstrap::util::{ensure_ownership_under, is_file};

const DEFAULT_PROFILE: &str = "/nix/var/nix/profiles/default";

pub struct FetchAndUnpack {
    mirror: Option<String>,
    progress: Arc<dyn DownloadProgress>,
    installed: Option<Installed>,
}

#[derive(Default)]
struct Installed {
    store_dir_created: bool,
    store_paths: Vec<PathBuf>,
    profile_created: bool,
}

impl FetchAndUnpack {
    pub fn new(mirror: Option<&str>, progress: Arc<dyn DownloadProgress>) -> Self {
        Self {
            mirror: mirror.map(String::from),
            progress,
            installed: None,
        }
    }
}

#[async_trait]
impl Step for FetchAndUnpack {
    type Error = Error;

    fn name(&self) -> &'static str {
        "fetch and activate the managed runtime"
    }

    async fn check(&self) -> Result<bool> {
        Ok(is_file(Path::new(DEFAULT_PROFILE).join("bin/nix-env")).await)
    }

    async fn execute(&mut self, token: &CancellationToken) -> Result<()> {
        let bytes = tarball::bytes(self.mirror.as_deref(), self.progress.as_ref()).await?;

        let token = token.clone();
        let (installed, result) = tokio::task::spawn_blocking(move || {
            let mut installed = Installed::default();
            let result = provision(&bytes, &mut installed, &token);
            (installed, result)
        })
        .await
        .map_err(|e| CoreError::TaskPanicked(e.to_string()))?;

        self.installed = Some(installed);
        result
    }

    async fn rollback(&mut self) -> Result<()> {
        let Some(installed) = self.installed.take() else {
            return Ok(());
        };

        tokio::task::spawn_blocking(move || teardown(installed))
            .await
            .map_err(|e| CoreError::TaskPanicked(e.to_string()))??;

        Ok(())
    }
}

fn provision(
    tarball_bytes: &[u8],
    installed: &mut Installed,
    token: &CancellationToken,
) -> Result<()> {
    let scratch_root = match recover_scratch() {
        Some(path) => path,
        None => {
            let scratch = tempfile::Builder::new()
                .prefix("temp-install-dir-")
                .tempdir_in("/nix")
                .map_err(|e| CoreError::Io {
                    path: "/nix".into(),
                    source: e,
                })?;
            tarball::unpack(tarball_bytes, scratch.path())?;
            let path = scratch.keep();
            write_manifest(&path)?;
            path
        }
    };

    let unpacked_root = find_single_child(&scratch_root, |name| name.starts_with("nix-"))?;
    tracing::debug!("moving Nix store into place");
    installed.store_dir_created = !Path::new(NIX_STORE).exists();
    move_store_into_place(&unpacked_root, &mut installed.store_paths, token)?;

    if token.is_cancelled() {
        return Ok(());
    }
    let nix_pkg = resolve_backlink(&unpacked_root, |name| {
        name.ends_with(&format!("-nix-{NIX_VERSION}"))
    })?;
    let nss_cacert_pkg = resolve_backlink(&unpacked_root, |name| name.contains("-nss-cacert-"))?;

    if token.is_cancelled() {
        return Ok(());
    }
    tracing::debug!("loading Nix database");
    load_db(&nix_pkg, &unpacked_root.join(".reginfo"))?;

    if token.is_cancelled() {
        return Ok(());
    }
    installed.profile_created = !Path::new(DEFAULT_PROFILE).exists();
    tracing::debug!("activating default profile");
    activate_default_profile(&nix_pkg, &nss_cacert_pkg)?;

    remove_manifest();
    let _ = std::fs::remove_dir_all(&scratch_root);

    Ok(())
}

fn teardown(installed: Installed) -> Result<()> {
    if installed.profile_created {
        tracing::debug!("removing default profile");
        remove_profile_default()?;
    }

    if installed.store_dir_created {
        tracing::debug!("removing Nix store directory created by this run");
        remove_path(Path::new(NIX_STORE))?;
    } else {
        tracing::debug!("removing Nix store paths added by this run");
        for path in installed.store_paths.iter().rev() {
            remove_path(path)?;
        }
    }

    if let Some(scratch_root) = read_manifest() {
        remove_path(&scratch_root)?;
    }
    remove_manifest();

    Ok(())
}

fn write_manifest(scratch_root: &Path) -> Result<()> {
    write_manifest_at(Path::new(NIX_PROVISIONING_MANIFEST), scratch_root)
}

fn read_manifest() -> Option<PathBuf> {
    read_manifest_at(Path::new(NIX_PROVISIONING_MANIFEST))
}

fn remove_manifest() {
    remove_manifest_at(Path::new(NIX_PROVISIONING_MANIFEST));
}

fn recover_scratch() -> Option<PathBuf> {
    recover_scratch_at(Path::new(NIX_PROVISIONING_MANIFEST))
}

fn write_manifest_at(manifest_path: &Path, scratch_root: &Path) -> Result<()> {
    let dir = manifest_path
        .parent()
        .expect("manifest path has a parent directory");
    let temp_path = dir.join(format!(
        ".{}.mix-tmp-{}",
        manifest_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy(),
        std::process::id()
    ));

    let write = || -> std::io::Result<()> {
        let mut file = std::fs::File::create(&temp_path)?;
        file.write_all(scratch_root.to_string_lossy().as_bytes())?;
        file.sync_all()
    };
    if let Err(e) = write() {
        let _ = std::fs::remove_file(&temp_path);
        return Err(CoreError::Io {
            path: temp_path,
            source: e,
        }
        .into());
    }

    std::fs::rename(&temp_path, manifest_path).map_err(|e| CoreError::Io {
        path: manifest_path.to_path_buf(),
        source: e,
    })?;
    if let Ok(dir_handle) = std::fs::File::open(dir) {
        let _ = dir_handle.sync_all();
    }

    Ok(())
}

fn read_manifest_at(manifest_path: &Path) -> Option<PathBuf> {
    std::fs::read_to_string(manifest_path)
        .ok()
        .map(PathBuf::from)
}

fn remove_manifest_at(manifest_path: &Path) {
    let _ = std::fs::remove_file(manifest_path);
}

fn recover_scratch_at(manifest_path: &Path) -> Option<PathBuf> {
    let path = read_manifest_at(manifest_path)?;
    if find_single_child(&path, |name| name.starts_with("nix-")).is_ok() {
        tracing::info!("resuming interrupted provisioning from {}", path.display());
        Some(path)
    } else {
        remove_manifest_at(manifest_path);
        None
    }
}

fn remove_profile_default() -> Result<()> {
    if let Ok(target) = std::fs::read_link(DEFAULT_PROFILE) {
        let target = if target.is_absolute() {
            target
        } else {
            Path::new(DEFAULT_PROFILE)
                .parent()
                .expect("DEFAULT_PROFILE has a parent directory")
                .join(target)
        };
        remove_path(&target)?;
    }
    remove_path(Path::new(DEFAULT_PROFILE))
}

fn remove_path(path: &Path) -> Result<()> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(CoreError::Io {
                path: path.to_path_buf(),
                source: e,
            }
            .into());
        }
    };

    let result = if meta.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };

    result.map_err(|e| {
        CoreError::Io {
            path: path.to_path_buf(),
            source: e,
        }
        .into()
    })
}

fn find_single_child(dir: &Path, pred: impl Fn(&str) -> bool) -> Result<PathBuf> {
    let entries = std::fs::read_dir(dir).map_err(|e| CoreError::Io {
        path: dir.to_path_buf(),
        source: e,
    })?;

    let mut matches = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| CoreError::Io {
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
    Ok(std::fs::read_link(&link).map_err(|e| CoreError::Io {
        path: link,
        source: e,
    })?)
}

fn move_store_into_place(
    unpacked_root: &Path,
    created: &mut Vec<PathBuf>,
    token: &CancellationToken,
) -> Result<()> {
    move_entries_into(
        &unpacked_root.join("store"),
        Path::new(NIX_STORE),
        created,
        Uid::from_raw(0),
        Gid::from_raw(NIXBLD_GID),
        token,
    )
}

fn move_entries_into(
    src_store: &Path,
    dest_store: &Path,
    created: &mut Vec<PathBuf>,
    uid: Uid,
    gid: Gid,
    token: &CancellationToken,
) -> Result<()> {
    std::fs::create_dir_all(dest_store).map_err(|e| CoreError::Io {
        path: dest_store.to_path_buf(),
        source: e,
    })?;

    let entries: Vec<_> = std::fs::read_dir(src_store)
        .map_err(|e| CoreError::Io {
            path: src_store.to_path_buf(),
            source: e,
        })?
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|e| CoreError::Io {
            path: src_store.to_path_buf(),
            source: e,
        })?;

    for entry in entries {
        if token.is_cancelled() {
            break;
        }

        let already_moved = entry.file_type().is_ok_and(|t| t.is_symlink());
        if already_moved {
            continue;
        }

        let src_path = entry.path();
        let dest_path = dest_store.join(entry.file_name());
        let is_new = dest_path.symlink_metadata().is_err();
        let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());

        ensure_ownership_under(&src_path, uid, gid)?;
        if is_dir {
            make_contents_read_only(&src_path)?;
        } else {
            make_read_only(&src_path)?;
        }

        if !is_new {
            let remove = if dest_path.is_dir() {
                std::fs::remove_dir_all(&dest_path)
            } else {
                std::fs::remove_file(&dest_path)
            };
            remove.map_err(|e| CoreError::Io {
                path: dest_path.clone(),
                source: e,
            })?;
        }

        std::fs::rename(&src_path, &dest_path).map_err(|e| rename_error(e, &dest_path))?;

        if is_dir {
            strip_write_bit(&dest_path)?;
        }

        if is_new {
            created.push(dest_path.clone());
        }

        std::os::unix::fs::symlink(&dest_path, &src_path).map_err(|e| CoreError::Io {
            path: src_path,
            source: e,
        })?;
    }

    Ok(())
}

fn make_read_only(root: &Path) -> Result<()> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        let meta = std::fs::symlink_metadata(&path).map_err(|e| CoreError::Io {
            path: path.clone(),
            source: e,
        })?;
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            let entries = std::fs::read_dir(&path).map_err(|e| CoreError::Io {
                path: path.clone(),
                source: e,
            })?;
            for entry in entries {
                let entry = entry.map_err(|e| CoreError::Io {
                    path: path.clone(),
                    source: e,
                })?;
                stack.push(entry.path());
            }
        }
        let mut perms = meta.permissions();
        perms.set_mode(perms.mode() & !0o222);
        std::fs::set_permissions(&path, perms).map_err(|e| CoreError::Io { path, source: e })?;
    }
    Ok(())
}

fn make_contents_read_only(root: &Path) -> Result<()> {
    let entries = std::fs::read_dir(root).map_err(|e| CoreError::Io {
        path: root.to_path_buf(),
        source: e,
    })?;
    for entry in entries {
        let entry = entry.map_err(|e| CoreError::Io {
            path: root.to_path_buf(),
            source: e,
        })?;
        make_read_only(&entry.path())?;
    }
    Ok(())
}

fn rename_error(e: std::io::Error, dest_path: &Path) -> Error {
    if e.kind() == std::io::ErrorKind::CrossesDevices {
        return Error::CrossDeviceStore {
            path: dest_path.to_path_buf(),
        };
    }
    CoreError::Io {
        path: dest_path.to_path_buf(),
        source: e,
    }
    .into()
}

fn strip_write_bit(path: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| CoreError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let mut perms = meta.permissions();
    perms.set_mode(perms.mode() & !0o222);
    std::fs::set_permissions(path, perms).map_err(|e| CoreError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    Ok(())
}

fn load_db(nix_pkg: &Path, reginfo_path: &Path) -> Result<()> {
    let reginfo = std::fs::read(reginfo_path).map_err(|e| CoreError::Io {
        path: reginfo_path.to_path_buf(),
        source: e,
    })?;

    let nix_store = nix_pkg.join("bin/nix-store");
    let command_line =
        crate::shared::os::format_command(&nix_store.to_string_lossy(), &["--load-db"]);
    tracing::debug!("running command: {command_line}");

    let mut child = std::process::Command::new(&nix_store)
        .arg("--load-db")
        .env("HOME", root_home())
        .env_remove("NIX_REMOTE")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| CoreError::Exec {
            command: command_line.clone(),
            source: e,
        })?;

    let mut stdin = child.stdin.take().expect("stdin was piped");
    let writer = std::thread::spawn(move || stdin.write_all(&reginfo));

    let output = child.wait_with_output().map_err(|e| CoreError::Exec {
        command: command_line.clone(),
        source: e,
    })?;

    tracing::trace!(
        "command output: {command_line}\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    if !output.status.success() {
        return Err(Error::Core(crate::shared::os::command_error(
            command_line,
            &output,
        )));
    }

    writer
        .join()
        .map_err(|e| CoreError::TaskPanicked(format!("{e:?}")))?
        .map_err(|e| CoreError::Exec {
            command: command_line,
            source: e,
        })?;

    Ok(())
}

fn activate_default_profile(nix_pkg: &Path, nss_cacert_pkg: &Path) -> Result<()> {
    let nix_env = nix_pkg.join("bin/nix-env");
    let command_line = crate::shared::os::format_command(
        &nix_env.to_string_lossy(),
        &[
            "--profile",
            DEFAULT_PROFILE,
            "--install",
            &nix_pkg.to_string_lossy(),
            &nss_cacert_pkg.to_string_lossy(),
            "--option",
            "substitute",
            "false",
            "--option",
            "post-build-hook",
            "",
        ],
    );
    tracing::debug!("running command: {command_line}");

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
        .map_err(|e| CoreError::Exec {
            command: command_line.clone(),
            source: e,
        })?;

    tracing::trace!(
        "command output: {command_line}\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    if !output.status.success() {
        return Err(Error::Core(crate::shared::os::command_error(
            command_line,
            &output,
        )));
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

    fn move_entries_into_unprivileged(
        src_store: &Path,
        dest_store: &Path,
        created: &mut Vec<PathBuf>,
    ) -> Result<()> {
        move_entries_into(
            src_store,
            dest_store,
            created,
            Uid::current(),
            Gid::current(),
            &CancellationToken::new(),
        )
    }

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

        move_entries_into_unprivileged(src.path(), &dest_store, &mut Vec::new()).unwrap();

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

        move_entries_into_unprivileged(src.path(), &dest_store, &mut Vec::new()).unwrap();

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

        move_entries_into_unprivileged(src.path(), &dest_store, &mut Vec::new()).unwrap();

        assert_eq!(
            std::fs::read_to_string(dest_store.join("pkg-a")).unwrap(),
            "new content"
        );
    }

    #[test]
    fn move_entries_into_reports_only_newly_created_destinations() {
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("pkg-a"), "new").unwrap();
        std::fs::write(src.path().join("pkg-b"), "new").unwrap();

        let dest = tempfile::tempdir().unwrap();
        let dest_store = dest.path().join("store");
        std::fs::create_dir_all(&dest_store).unwrap();
        std::fs::write(dest_store.join("pkg-a"), "stale").unwrap();

        let mut created = Vec::new();
        move_entries_into_unprivileged(src.path(), &dest_store, &mut created).unwrap();

        assert_eq!(created, vec![dest_store.join("pkg-b")]);
    }

    #[test]
    fn move_entries_into_preserves_already_recorded_entries_when_a_later_call_fails() {
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("pkg-a"), "content").unwrap();

        let dest = tempfile::tempdir().unwrap();
        let dest_store = dest.path().join("store");

        let mut created = Vec::new();
        move_entries_into_unprivileged(src.path(), &dest_store, &mut created).unwrap();
        assert_eq!(created, vec![dest_store.join("pkg-a")]);

        let missing_src = dest.path().join("does-not-exist");
        assert!(move_entries_into_unprivileged(&missing_src, &dest_store, &mut created).is_err());

        assert_eq!(created, vec![dest_store.join("pkg-a")]);
    }

    #[test]
    fn move_entries_into_does_not_record_a_dangling_symlink_as_newly_created() {
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("pkg-a"), "new content").unwrap();

        let dest = tempfile::tempdir().unwrap();
        let dest_store = dest.path().join("store");
        std::fs::create_dir_all(&dest_store).unwrap();
        std::os::unix::fs::symlink("/does/not/exist", dest_store.join("pkg-a")).unwrap();

        let mut created = Vec::new();
        move_entries_into_unprivileged(src.path(), &dest_store, &mut created).unwrap();

        assert!(created.is_empty());
        assert_eq!(
            std::fs::read_to_string(dest_store.join("pkg-a")).unwrap(),
            "new content"
        );
    }

    #[test]
    fn move_entries_into_skips_source_entries_already_converted_to_symlinks() {
        let src = tempfile::tempdir().unwrap();
        std::fs::create_dir(src.path().join("pkg-real")).unwrap();
        std::fs::write(src.path().join("pkg-real").join("file"), "content").unwrap();
        std::os::unix::fs::symlink("/nix/store/pkg-done", src.path().join("pkg-done")).unwrap();

        let dest = tempfile::tempdir().unwrap();
        let dest_store = dest.path().join("store");

        let mut created = Vec::new();
        move_entries_into_unprivileged(src.path(), &dest_store, &mut created).unwrap();

        assert_eq!(created, vec![dest_store.join("pkg-real")]);
        assert!(dest_store.join("pkg-done").symlink_metadata().is_err());
        assert!(src.path().join("pkg-done").is_symlink());
    }

    #[test]
    fn move_entries_into_does_nothing_when_the_token_is_already_cancelled() {
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("pkg-a"), "content").unwrap();

        let dest = tempfile::tempdir().unwrap();
        let dest_store = dest.path().join("store");

        let token = CancellationToken::new();
        token.cancel();

        let mut created = Vec::new();
        move_entries_into(
            src.path(),
            &dest_store,
            &mut created,
            Uid::current(),
            Gid::current(),
            &token,
        )
        .unwrap();

        assert!(created.is_empty());
        assert!(src.path().join("pkg-a").exists());
        assert!(dest_store.join("pkg-a").symlink_metadata().is_err());
    }

    #[test]
    fn write_manifest_at_then_read_manifest_at_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let manifest_path = dir.path().join("manifest");
        let scratch = tempfile::tempdir().unwrap();

        write_manifest_at(&manifest_path, scratch.path()).unwrap();

        assert_eq!(read_manifest_at(&manifest_path).unwrap(), scratch.path());
    }

    #[test]
    fn write_manifest_at_leaves_no_temp_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let manifest_path = dir.path().join("manifest");
        let scratch = tempfile::tempdir().unwrap();

        write_manifest_at(&manifest_path, scratch.path()).unwrap();

        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, vec![std::ffi::OsString::from("manifest")]);
    }

    #[test]
    fn recover_scratch_at_returns_none_when_no_manifest_exists() {
        let dir = tempfile::tempdir().unwrap();
        let manifest_path = dir.path().join("manifest");

        assert!(recover_scratch_at(&manifest_path).is_none());
    }

    #[test]
    fn recover_scratch_at_discards_a_manifest_pointing_at_a_missing_directory() {
        let dir = tempfile::tempdir().unwrap();
        let manifest_path = dir.path().join("manifest");
        write_manifest_at(&manifest_path, Path::new("/does/not/exist/mix-test")).unwrap();

        assert!(recover_scratch_at(&manifest_path).is_none());
        assert!(!manifest_path.exists());
    }

    #[test]
    fn recover_scratch_at_returns_the_scratch_root_when_it_still_holds_an_unpacked_tree() {
        let dir = tempfile::tempdir().unwrap();
        let manifest_path = dir.path().join("manifest");
        let scratch = tempfile::tempdir().unwrap();
        std::fs::create_dir(scratch.path().join("nix-2.35.2-x86_64-linux")).unwrap();
        write_manifest_at(&manifest_path, scratch.path()).unwrap();

        assert_eq!(recover_scratch_at(&manifest_path).unwrap(), scratch.path());
        assert!(manifest_path.exists());
    }

    #[test]
    fn remove_path_removes_a_directory_recursively() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        std::fs::write(nested.join("file"), "x").unwrap();

        remove_path(dir.path()).unwrap();

        assert!(!dir.path().exists());
    }

    #[test]
    fn remove_path_removes_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");
        std::fs::write(&file, "x").unwrap();

        remove_path(&file).unwrap();

        assert!(!file.exists());
    }

    #[test]
    fn remove_path_is_a_noop_when_missing() {
        assert!(remove_path(Path::new("/does/not/exist/mix-test")).is_ok());
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
    fn rename_error_reports_cross_device_store_for_exdev() {
        let dest = Path::new("/nix/store/pkg-a");
        let err = rename_error(std::io::Error::from_raw_os_error(18), dest);

        assert!(matches!(err, Error::CrossDeviceStore { path } if path == dest));
    }

    #[test]
    fn rename_error_falls_back_to_a_plain_io_error_otherwise() {
        let dest = Path::new("/nix/store/pkg-a");
        let err = rename_error(
            std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            dest,
        );

        assert!(!matches!(err, Error::CrossDeviceStore { .. }));
    }
}
