use crate::identity::{self, NIXBLD_GID, NIXBLD_GROUP, NIXBLD_UID_BASE, NIXBLD_USER_COUNT};
use crate::paths::{
    DEFAULT_PROFILE_NIX_ENV, NIX_CONF_DEST, NIX_DAEMON_SERVICE_DEST, NIX_DAEMON_SERVICE_SRC,
    NIX_DAEMON_SOCKET_DEST, NIX_DAEMON_SOCKET_SRC, NIX_OWNERSHIP_MARKER, NIX_STORE, NIX_TREE_MODE,
    NIX_TREE_PATHS, PROFILE_SNIPPET_DEST,
};

pub const NIX_CONF: &str =
    "build-users-group = nixbld\nexperimental-features = nix-command flakes\n";
pub const PROFILE_SNIPPET: &str = "# Managed by mix -- do not edit, changes are overwritten and will trip `mix doctor`.\nif [ -e '/nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh' ]; then\n    . '/nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh'\nfi\n";

#[derive(Debug, Clone, Copy)]
pub enum Target {
    Directory {
        path: &'static str,
        mode: u32,
    },
    File {
        path: &'static str,
        expected: &'static str,
    },
    Group {
        name: &'static str,
        gid: u32,
    },
    User {
        n: u32,
        uid: u32,
        gid: u32,
    },
    SystemdUnit {
        name: &'static str,
        src: &'static str,
        dest: &'static str,
        must_be_active: bool,
    },
    PathExists {
        name: &'static str,
        path: &'static str,
    },
}

impl Target {
    pub fn label(&self) -> String {
        match *self {
            Target::Directory { path, .. } => path.to_string(),
            Target::File { path, .. } => path.to_string(),
            Target::Group { name, .. } => name.to_string(),
            Target::User { n, .. } => identity::user_name(n),
            Target::SystemdUnit { name, .. } => name.to_string(),
            Target::PathExists { name, .. } => name.to_string(),
        }
    }
}

pub fn targets() -> Vec<Target> {
    let mut items = vec![
        Target::Directory {
            path: "/nix",
            mode: 0o755,
        },
        Target::Directory {
            path: NIX_STORE,
            mode: 0o1775,
        },
    ];
    items.extend(NIX_TREE_PATHS.iter().map(|&path| Target::Directory {
        path,
        mode: NIX_TREE_MODE,
    }));
    items.push(Target::File {
        path: NIX_OWNERSHIP_MARKER,
        expected: "",
    });
    items.push(Target::File {
        path: NIX_CONF_DEST,
        expected: NIX_CONF,
    });
    items.push(Target::File {
        path: PROFILE_SNIPPET_DEST,
        expected: PROFILE_SNIPPET,
    });
    items.push(Target::Group {
        name: NIXBLD_GROUP,
        gid: NIXBLD_GID,
    });
    items.extend((1..=NIXBLD_USER_COUNT).map(|n| Target::User {
        n,
        uid: NIXBLD_UID_BASE + n,
        gid: NIXBLD_GID,
    }));
    items.push(Target::SystemdUnit {
        name: "nix-daemon.service",
        src: NIX_DAEMON_SERVICE_SRC,
        dest: NIX_DAEMON_SERVICE_DEST,
        must_be_active: false,
    });
    items.push(Target::SystemdUnit {
        name: "nix-daemon.socket",
        src: NIX_DAEMON_SOCKET_SRC,
        dest: NIX_DAEMON_SOCKET_DEST,
        must_be_active: true,
    });
    items.push(Target::PathExists {
        name: "default profile",
        path: DEFAULT_PROFILE_NIX_ENV,
    });
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_uses_the_path_for_path_based_targets() {
        let target = Target::Directory {
            path: "/nix",
            mode: 0o755,
        };
        assert_eq!(target.label(), "/nix");
    }

    #[test]
    fn label_computes_the_username_for_user_targets() {
        let target = Target::User {
            n: 3,
            uid: 30_003,
            gid: NIXBLD_GID,
        };
        assert_eq!(target.label(), "nixbld3");
    }

    #[test]
    fn targets_include_every_directory_in_the_managed_nix_tree() {
        let items = targets();
        for &path in NIX_TREE_PATHS {
            assert!(
                items.iter().any(|t| matches!(
                    t,
                    Target::Directory { path: p, mode } if *p == path && *mode == NIX_TREE_MODE
                )),
                "targets() is missing an entry for {path} (mode {NIX_TREE_MODE:o})"
            );
        }
    }

    #[test]
    fn targets_include_all_build_users() {
        let items = targets();
        let user_count = items
            .iter()
            .filter(|t| matches!(t, Target::User { .. }))
            .count();
        assert_eq!(user_count, NIXBLD_USER_COUNT as usize);
    }
}
