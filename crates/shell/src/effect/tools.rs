use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use mix_core::effect::Failure;

pub const TRUSTED_DIRS: [&str; 4] = ["/usr/sbin", "/usr/bin", "/sbin", "/bin"];

const WRITABLE_BY_OTHERS: u32 = 0o022;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Entry {
    uid: u32,
    mode: u32,
    file: bool,
    link: bool,
}

trait Inspect {
    fn resolve(&self, path: &Path) -> Option<PathBuf>;
    fn entry(&self, path: &Path, follow: bool) -> std::io::Result<Entry>;
}

struct Host;

impl Inspect for Host {
    fn resolve(&self, path: &Path) -> Option<PathBuf> {
        std::fs::canonicalize(path).ok()
    }

    fn entry(&self, path: &Path, follow: bool) -> std::io::Result<Entry> {
        let meta = if follow {
            std::fs::metadata(path)?
        } else {
            std::fs::symlink_metadata(path)?
        };
        Ok(Entry {
            uid: meta.uid(),
            mode: meta.mode(),
            file: meta.is_file(),
            link: meta.file_type().is_symlink(),
        })
    }
}

pub fn trusted(name: &str) -> Result<PathBuf, Failure> {
    trusted_in(&Host, name, &TRUSTED_DIRS.map(Path::new), 0)
}

fn trusted_in(
    host: &impl Inspect,
    name: &str,
    dirs: &[&Path],
    owner: u32,
) -> Result<PathBuf, Failure> {
    let mut refused = None;
    for dir in dirs {
        let candidate = dir.join(name);
        let Some(resolved) = host.resolve(&candidate) else {
            continue;
        };
        match vetted(host, &resolved, owner).and_then(|()| vetted_dirs(host, dir, owner)) {
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

fn read(host: &impl Inspect, path: &Path, follow: bool) -> Result<Entry, Failure> {
    host.entry(path, follow).map_err(|error| Failure::Io {
        path: path.to_path_buf(),
        kind: error.kind(),
    })
}

fn vetted(host: &impl Inspect, binary: &Path, owner: u32) -> Result<(), Failure> {
    let entry = read(host, binary, true)?;
    if !entry.file || entry.mode & 0o111 == 0 {
        return Err(untrusted(binary, "an executable file", "something else"));
    }
    for path in binary.ancestors() {
        owned(path, read(host, path, true)?, owner)?;
    }
    Ok(())
}

fn vetted_dirs(host: &impl Inspect, dir: &Path, owner: u32) -> Result<(), Failure> {
    for path in dir.ancestors() {
        owned(path, read(host, path, false)?, owner)?;
    }
    Ok(())
}

fn owned(path: &Path, entry: Entry, owner: u32) -> Result<(), Failure> {
    if entry.uid != owner && entry.uid != 0 {
        return Err(untrusted(
            path,
            format!("owned by root or uid {owner}"),
            format!("owned by uid {}", entry.uid),
        ));
    }
    if !entry.link && entry.mode & WRITABLE_BY_OTHERS != 0 {
        return Err(untrusted(
            path,
            "writable by its owner alone",
            format!("mode {:o}", entry.mode & 0o7777),
        ));
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
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    const USER: u32 = 1000;
    const STRANGER: u32 = 1001;

    #[derive(Default)]
    struct Tree {
        entries: BTreeMap<PathBuf, Entry>,
        links: BTreeMap<PathBuf, PathBuf>,
    }

    impl Tree {
        fn new() -> Self {
            let mut tree = Self::default();
            tree.dir("/", 0, 0o755);
            tree.dir("/opt", 0, 0o755);
            tree.dir("/opt/tools", USER, 0o755);
            tree
        }

        fn dir(&mut self, path: &str, uid: u32, mode: u32) {
            self.entries.insert(
                path.into(),
                Entry {
                    uid,
                    mode,
                    file: false,
                    link: false,
                },
            );
        }

        fn tool(&mut self, path: &str, uid: u32, mode: u32) {
            self.entries.insert(
                path.into(),
                Entry {
                    uid,
                    mode,
                    file: true,
                    link: false,
                },
            );
        }

        fn link(&mut self, path: &str, target: &str) {
            self.links.insert(path.into(), target.into());
        }
    }

    impl Inspect for Tree {
        fn resolve(&self, path: &Path) -> Option<PathBuf> {
            let resolved = self.links.get(path).map_or(path, PathBuf::as_path);
            self.entries
                .contains_key(resolved)
                .then(|| resolved.to_path_buf())
        }

        fn entry(&self, path: &Path, follow: bool) -> std::io::Result<Entry> {
            if let Some(target) = self.links.get(path) {
                if follow {
                    return self.entry(target, true);
                }
                return Ok(Entry {
                    uid: 0,
                    mode: 0o777,
                    file: false,
                    link: true,
                });
            }
            self.entries
                .get(path)
                .copied()
                .ok_or_else(|| std::io::ErrorKind::NotFound.into())
        }
    }

    fn refused_at(result: Result<PathBuf, Failure>) -> String {
        match result {
            Err(Failure::Conflict { subject, .. }) => subject,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    #[ignore = "reads the tools of the host it runs on"]
    fn a_system_tool_resolves_to_an_absolute_root_owned_path() {
        let sh = trusted("sh").expect("every supported host has a root-owned sh");

        assert!(sh.is_absolute());
        assert_eq!(std::fs::metadata(&sh).unwrap().uid(), 0);
    }

    #[test]
    fn a_tool_only_its_owner_can_change_is_accepted() {
        let mut tree = Tree::new();
        tree.tool("/opt/tools/useradd", USER, 0o755);

        let accepted = trusted_in(&tree, "useradd", &[Path::new("/opt/tools")], USER);

        assert_eq!(accepted.unwrap(), Path::new("/opt/tools/useradd"));
    }

    #[test]
    fn a_tool_owned_by_someone_else_is_refused() {
        let mut tree = Tree::new();
        tree.tool("/opt/tools/useradd", STRANGER, 0o755);

        let refused = trusted_in(&tree, "useradd", &[Path::new("/opt/tools")], USER);

        assert_eq!(refused_at(refused), "/opt/tools/useradd");
    }

    #[test]
    fn a_tool_others_can_rewrite_is_refused() {
        let mut tree = Tree::new();
        tree.tool("/opt/tools/useradd", USER, 0o777);

        let refused = trusted_in(&tree, "useradd", &[Path::new("/opt/tools")], USER);

        assert_eq!(refused_at(refused), "/opt/tools/useradd");
    }

    #[test]
    fn a_tool_in_a_directory_others_can_write_is_refused() {
        let mut tree = Tree::new();
        tree.dir("/opt/tools", USER, 0o777);
        tree.tool("/opt/tools/useradd", USER, 0o755);

        let refused = trusted_in(&tree, "useradd", &[Path::new("/opt/tools")], USER);

        assert_eq!(refused_at(refused), "/opt/tools");
    }

    #[test]
    fn a_tool_under_a_directory_a_stranger_owns_is_refused() {
        let mut tree = Tree::new();
        tree.dir("/opt", STRANGER, 0o755);
        tree.tool("/opt/tools/useradd", USER, 0o755);

        let refused = trusted_in(&tree, "useradd", &[Path::new("/opt/tools")], USER);

        assert_eq!(refused_at(refused), "/opt");
    }

    #[test]
    fn a_tool_reached_through_a_link_runs_under_the_name_it_was_found_by() {
        let mut tree = Tree::new();
        tree.tool("/opt/tools/pgrep", USER, 0o755);
        tree.link("/opt/tools/pkill", "/opt/tools/pgrep");

        let found = trusted_in(&tree, "pkill", &[Path::new("/opt/tools")], USER).unwrap();

        assert_eq!(found, Path::new("/opt/tools/pkill"));
    }

    #[test]
    fn a_link_to_a_tool_someone_else_can_change_is_refused() {
        let mut tree = Tree::new();
        tree.dir("/tmp", STRANGER, 0o755);
        tree.tool("/tmp/pgrep", STRANGER, 0o755);
        tree.link("/opt/tools/pkill", "/tmp/pgrep");

        let refused = trusted_in(&tree, "pkill", &[Path::new("/opt/tools")], USER);

        assert_eq!(refused_at(refused), "/tmp/pgrep");
    }

    #[test]
    fn the_first_trusted_directory_with_the_tool_wins() {
        let mut tree = Tree::new();
        tree.dir("/opt/other", USER, 0o755);
        tree.tool("/opt/other/useradd", USER, 0o755);

        let found = trusted_in(
            &tree,
            "useradd",
            &[Path::new("/opt/tools"), Path::new("/opt/other")],
            USER,
        );

        assert_eq!(found.unwrap(), Path::new("/opt/other/useradd"));
    }

    #[test]
    fn a_missing_tool_is_a_spawn_failure() {
        assert!(matches!(
            trusted_in(
                &Tree::new(),
                "mix-no-such-tool",
                &[Path::new("/opt/tools")],
                USER
            ),
            Err(Failure::SpawnFailed { .. })
        ));
    }
}
