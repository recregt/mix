use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use mix_core::action::Failure;

pub const TRUSTED_DIRS: [&str; 4] = ["/usr/sbin", "/usr/bin", "/sbin", "/bin"];

const WRITABLE_BY_OTHERS: u32 = 0o022;

pub fn trusted(name: &str) -> Result<PathBuf, Failure> {
    trusted_in(name, &TRUSTED_DIRS.map(Path::new), 0)
}

pub fn trusted_in(name: &str, dirs: &[&Path], owner: u32) -> Result<PathBuf, Failure> {
    let mut refused = None;
    for dir in dirs {
        let candidate = dir.join(name);
        let Ok(resolved) = std::fs::canonicalize(&candidate) else {
            continue;
        };
        match vetted(&resolved, owner).and_then(|()| vetted_dirs(dir, owner)) {
            Ok(()) => return Ok(candidate),
            Err(failure) => {
                refused.get_or_insert(failure);
            }
        }
    }
    Err(refused.unwrap_or(Failure::SpawnFailed {
        program: name.to_string(),
        kind: std::io::ErrorKind::NotFound,
    }))
}

fn vetted(binary: &Path, owner: u32) -> Result<(), Failure> {
    let meta = std::fs::metadata(binary).map_err(|error| Failure::Io {
        path: binary.to_path_buf(),
        kind: error.kind(),
    })?;
    if !meta.is_file() || meta.mode() & 0o111 == 0 {
        return Err(untrusted(binary, "an executable file", "something else"));
    }
    for path in binary.ancestors() {
        let meta = std::fs::metadata(path).map_err(|error| Failure::Io {
            path: path.to_path_buf(),
            kind: error.kind(),
        })?;
        if meta.uid() != owner && meta.uid() != 0 {
            return Err(untrusted(
                path,
                format!("owned by root or uid {owner}"),
                format!("owned by uid {}", meta.uid()),
            ));
        }
        if meta.mode() & WRITABLE_BY_OTHERS != 0 {
            return Err(untrusted(
                path,
                "writable by its owner alone",
                format!("mode {:o}", meta.mode() & 0o7777),
            ));
        }
    }
    Ok(())
}

fn vetted_dirs(dir: &Path, owner: u32) -> Result<(), Failure> {
    for path in dir.ancestors() {
        let meta = std::fs::symlink_metadata(path).map_err(|error| Failure::Io {
            path: path.to_path_buf(),
            kind: error.kind(),
        })?;
        if meta.uid() != owner && meta.uid() != 0 {
            return Err(untrusted(
                path,
                format!("owned by root or uid {owner}"),
                format!("owned by uid {}", meta.uid()),
            ));
        }
        if !meta.file_type().is_symlink() && meta.mode() & WRITABLE_BY_OTHERS != 0 {
            return Err(untrusted(
                path,
                "writable by its owner alone",
                format!("mode {:o}", meta.mode() & 0o7777),
            ));
        }
    }
    Ok(())
}

fn untrusted(path: &Path, expected: impl Into<String>, found: impl Into<String>) -> Failure {
    Failure::Conflict {
        subject: path.display().to_string(),
        expected: expected.into(),
        found: found.into(),
    }
}

pub fn root_command(name: &str) -> Result<mix_exec::Command, Failure> {
    Ok(mix_exec::Command::new(trusted(name)?)
        .env_clear()
        .env("PATH", TRUSTED_DIRS.join(":"))
        .env("LC_ALL", "C.UTF-8"))
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn target() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target")
    }

    fn me() -> u32 {
        nix::unistd::Uid::current().as_raw()
    }

    fn checkout() -> u32 {
        target()
            .canonicalize()
            .unwrap()
            .ancestors()
            .map(|dir| std::fs::metadata(dir).unwrap().uid())
            .find(|uid| *uid != 0)
            .unwrap_or(0)
    }

    fn scratch() -> tempfile::TempDir {
        let target = target();
        std::fs::create_dir_all(&target).unwrap();
        let dir = tempfile::tempdir_in(target).unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        dir
    }

    fn tool(dir: &Path, name: &str, mode: u32) {
        let path = dir.join(name);
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    #[ignore = "reads the tools of the host it runs on"]
    fn a_system_tool_resolves_to_an_absolute_root_owned_path() {
        let sh = trusted("sh").expect("every supported host has a root-owned sh");

        assert!(sh.is_absolute());
        assert_eq!(std::fs::metadata(&sh).unwrap().uid(), 0);
    }

    #[test]
    fn a_tool_owned_by_someone_else_is_refused() {
        if me() == 0 {
            return;
        }
        let dir = scratch();
        tool(dir.path(), "useradd", 0o755);

        let refused = trusted_in("useradd", &[dir.path()], 0);

        assert!(
            matches!(refused, Err(Failure::Conflict { .. })),
            "{refused:?}"
        );
    }

    #[test]
    fn a_tool_others_can_rewrite_is_refused() {
        let dir = scratch();
        tool(dir.path(), "useradd", 0o777);

        let refused = trusted_in("useradd", &[dir.path()], checkout());

        assert!(
            matches!(refused, Err(Failure::Conflict { .. })),
            "{refused:?}"
        );
    }

    #[test]
    fn a_tool_in_a_directory_others_can_write_is_refused() {
        let dir = scratch();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
        tool(dir.path(), "useradd", 0o755);

        let refused = trusted_in("useradd", &[dir.path()], checkout());

        assert!(
            matches!(refused, Err(Failure::Conflict { .. })),
            "{refused:?}"
        );
    }

    #[test]
    fn a_tool_only_its_owner_can_change_is_accepted() {
        let dir = scratch();
        tool(dir.path(), "useradd", 0o755);

        let accepted = trusted_in("useradd", &[dir.path()], checkout());

        assert_eq!(accepted.unwrap(), dir.path().join("useradd"));
    }

    #[test]
    fn a_tool_reached_through_a_link_runs_under_the_name_it_was_found_by() {
        let dir = scratch();
        tool(dir.path(), "pgrep", 0o755);
        std::os::unix::fs::symlink("pgrep", dir.path().join("pkill")).unwrap();

        let found = trusted_in("pkill", &[dir.path()], checkout()).unwrap();

        assert_eq!(found.file_name().unwrap(), "pkill");
    }

    #[test]
    fn a_missing_tool_is_a_spawn_failure() {
        assert!(matches!(
            trusted("mix-no-such-tool"),
            Err(Failure::SpawnFailed { .. })
        ));
    }
}
