use std::borrow::Cow;
use std::path::{Path, PathBuf};

use crate::identity::{self, NIXBLD_GID, NIXBLD_GROUP, NIXBLD_UID_BASE, NIXBLD_USER_COUNT};
use crate::paths::{
    DEFAULT_PROFILE_NIX_ENV, FLAKE_LOCK, FLAKE_NIX, HOME_NIX, MIX_STATE_DIR_MODE, NIX_CONF_DEST,
    NIX_DAEMON_SERVICE_DEST, NIX_DAEMON_SERVICE_SRC, NIX_DAEMON_SOCKET_DEST, NIX_DAEMON_SOCKET_SRC,
    NIX_OWNERSHIP_MARKER, NIX_PROFILES_DIR_MODE, NIX_STORE, NIX_TREE_MODE, NIX_TREE_PATHS,
    PROFILE_SNIPPET_DEST, mix_state_dir, mix_user_marker, nix_profiles_dir,
};
use crate::privilege::InvokingUser;

pub const NIX_CONF: &str =
    "build-users-group = nixbld\nexperimental-features = nix-command flakes\n";
pub const PROFILE_SNIPPET: &str = "# Managed by mix -- do not edit, changes are overwritten and will trip `mix doctor`.\nif [ -e '/nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh' ]; then\n    . '/nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh'\nfi\n";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserConfig {
    pub user: InvokingUser,
    pub flake: String,
    pub home: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Filesystem,
    Identity,
    Services,
    Configuration,
}

impl Category {
    pub const ALL: [Category; 4] = [
        Category::Filesystem,
        Category::Identity,
        Category::Services,
        Category::Configuration,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            Category::Filesystem => "Filesystem",
            Category::Identity => "Identity/Users",
            Category::Services => "Services",
            Category::Configuration => "Configuration",
        }
    }
}

type Owner = Option<(u32, u32)>;

#[derive(Debug, Clone)]
pub enum Target {
    Directory {
        path: PathBuf,
        mode: u32,
        owner: Owner,
    },
    File {
        path: PathBuf,
        expected: Option<String>,
        owner: Owner,
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
    pub fn label(&self) -> Cow<'_, str> {
        match self {
            Target::Directory { path, .. } => path.to_string_lossy(),
            Target::File { path, .. } => path.to_string_lossy(),
            Target::Group { name, .. } => Cow::Borrowed(name),
            Target::User { n, .. } => identity::user_name(*n),
            Target::SystemdUnit { name, .. } => Cow::Borrowed(name),
            Target::PathExists { name, .. } => Cow::Borrowed(name),
        }
    }

    pub fn category(&self) -> Category {
        match self {
            Target::Directory { .. } => Category::Filesystem,
            Target::File { path, .. }
                if path.as_path() == Path::new(NIX_CONF_DEST)
                    || path.as_path() == Path::new(PROFILE_SNIPPET_DEST) =>
            {
                Category::Configuration
            }
            Target::File { .. } => Category::Filesystem,
            Target::Group { .. } | Target::User { .. } => Category::Identity,
            Target::SystemdUnit { .. } => Category::Services,
            Target::PathExists { .. } => Category::Filesystem,
        }
    }
}

fn push_user_targets(items: &mut Vec<Target>, cfg: &UserConfig) {
    let state_dir = mix_state_dir(&cfg.user.home);
    let owner = Some((cfg.user.uid, cfg.user.gid));
    items.push(Target::Directory {
        path: state_dir.clone(),
        mode: MIX_STATE_DIR_MODE,
        owner,
    });
    items.push(Target::Directory {
        path: nix_profiles_dir(&cfg.user.home),
        mode: NIX_PROFILES_DIR_MODE,
        owner,
    });
    items.push(Target::File {
        path: state_dir.join(HOME_NIX),
        expected: Some(cfg.home.clone()),
        owner,
    });
    items.push(Target::File {
        path: state_dir.join(FLAKE_NIX),
        expected: Some(cfg.flake.clone()),
        owner,
    });
    items.push(Target::File {
        path: state_dir.join(FLAKE_LOCK),
        expected: None,
        owner,
    });
    items.push(Target::File {
        path: mix_user_marker(cfg.user.uid),
        expected: Some(String::new()),
        owner: None,
    });
}

pub fn user_targets(cfg: &UserConfig) -> Vec<Target> {
    let mut items = Vec::new();
    push_user_targets(&mut items, cfg);
    items
}

pub fn targets(user_config: Option<&UserConfig>) -> Vec<Target> {
    let mut items = vec![
        Target::Directory {
            path: PathBuf::from("/nix"),
            mode: 0o755,
            owner: None,
        },
        Target::Directory {
            path: PathBuf::from(NIX_STORE),
            mode: 0o1775,
            owner: None,
        },
    ];
    items.extend(NIX_TREE_PATHS.iter().map(|&path| Target::Directory {
        path: PathBuf::from(path),
        mode: NIX_TREE_MODE,
        owner: None,
    }));
    items.push(Target::File {
        path: PathBuf::from(NIX_OWNERSHIP_MARKER),
        expected: Some(String::new()),
        owner: None,
    });
    items.push(Target::File {
        path: PathBuf::from(NIX_CONF_DEST),
        expected: Some(NIX_CONF.to_string()),
        owner: None,
    });
    items.push(Target::File {
        path: PathBuf::from(PROFILE_SNIPPET_DEST),
        expected: Some(PROFILE_SNIPPET.to_string()),
        owner: None,
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

    if let Some(cfg) = user_config {
        push_user_targets(&mut items, cfg);
    }

    items
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_user_config() -> UserConfig {
        UserConfig {
            user: InvokingUser {
                uid: 1000,
                gid: 1000,
                name: "mix-user".to_string(),
                home: PathBuf::from("/home/mix-user"),
            },
            flake: "flake-content".to_string(),
            home: "home-content".to_string(),
        }
    }

    #[test]
    fn label_uses_the_path_for_path_based_targets() {
        let target = Target::Directory {
            path: PathBuf::from("/nix"),
            mode: 0o755,
            owner: None,
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
    fn label_borrows_instead_of_allocating_for_every_target() {
        let cfg = sample_user_config();
        for target in targets(Some(&cfg)) {
            assert!(
                matches!(target.label(), Cow::Borrowed(_)),
                "label() allocated for {target:?}"
            );
        }
    }

    #[test]
    fn category_groups_directories_and_the_ownership_marker_as_filesystem() {
        assert_eq!(
            Target::Directory {
                path: PathBuf::from("/nix"),
                mode: 0o755,
                owner: None,
            }
            .category(),
            Category::Filesystem
        );
        assert_eq!(
            Target::File {
                path: PathBuf::from(NIX_OWNERSHIP_MARKER),
                expected: Some(String::new()),
                owner: None,
            }
            .category(),
            Category::Filesystem
        );
    }

    #[test]
    fn category_groups_nix_conf_and_the_profile_snippet_as_configuration() {
        assert_eq!(
            Target::File {
                path: PathBuf::from(NIX_CONF_DEST),
                expected: Some(NIX_CONF.to_string()),
                owner: None,
            }
            .category(),
            Category::Configuration
        );
        assert_eq!(
            Target::File {
                path: PathBuf::from(PROFILE_SNIPPET_DEST),
                expected: Some(PROFILE_SNIPPET.to_string()),
                owner: None,
            }
            .category(),
            Category::Configuration
        );
    }

    #[test]
    fn category_groups_the_group_and_user_targets_as_identity() {
        assert_eq!(
            Target::Group {
                name: NIXBLD_GROUP,
                gid: NIXBLD_GID,
            }
            .category(),
            Category::Identity
        );
        assert_eq!(
            Target::User {
                n: 1,
                uid: NIXBLD_UID_BASE + 1,
                gid: NIXBLD_GID,
            }
            .category(),
            Category::Identity
        );
    }

    #[test]
    fn category_groups_systemd_units_as_services() {
        assert_eq!(
            Target::SystemdUnit {
                name: "nix-daemon.socket",
                src: NIX_DAEMON_SOCKET_SRC,
                dest: NIX_DAEMON_SOCKET_DEST,
                must_be_active: true,
            }
            .category(),
            Category::Services
        );
    }

    #[test]
    fn category_groups_path_exists_as_filesystem() {
        assert_eq!(
            Target::PathExists {
                name: "default profile",
                path: DEFAULT_PROFILE_NIX_ENV,
            }
            .category(),
            Category::Filesystem
        );
    }

    #[test]
    fn targets_include_every_directory_in_the_managed_nix_tree() {
        let items = targets(None);
        for &path in NIX_TREE_PATHS {
            assert!(
                items.iter().any(|t| matches!(
                    t,
                    Target::Directory { path: p, mode, .. }
                        if p.as_path() == Path::new(path) && *mode == NIX_TREE_MODE
                )),
                "targets() is missing an entry for {path} (mode {NIX_TREE_MODE:o})"
            );
        }
    }

    #[test]
    fn targets_include_all_build_users() {
        let items = targets(None);
        let user_count = items
            .iter()
            .filter(|t| matches!(t, Target::User { .. }))
            .count();
        assert_eq!(user_count, NIXBLD_USER_COUNT as usize);
    }

    #[test]
    fn targets_excludes_per_user_entries_when_no_user_is_given() {
        let items = targets(None);
        assert!(
            !items
                .iter()
                .any(|t| matches!(t, Target::Directory { path, .. } if path.ends_with("mix")))
        );
    }

    #[test]
    fn targets_includes_per_user_entries_when_a_user_is_given() {
        let cfg = sample_user_config();
        let items = targets(Some(&cfg));

        assert!(items.iter().any(|t| matches!(
            t,
            Target::Directory { path, mode, owner: Some((1000, 1000)) }
                if path.ends_with(".local/state/mix") && *mode == MIX_STATE_DIR_MODE
        )));
        assert!(items.iter().any(|t| matches!(
            t,
            Target::File { path, expected, owner: Some((1000, 1000)) }
                if path.ends_with("flake.nix") && expected.as_deref() == Some("flake-content")
        )));
        assert!(items.iter().any(|t| matches!(
            t,
            Target::File { path, expected, owner: Some((1000, 1000)) }
                if path.ends_with("home.nix") && expected.as_deref() == Some("home-content")
        )));
    }

    #[test]
    fn user_targets_returns_exactly_the_six_per_user_entries() {
        let cfg = sample_user_config();
        assert_eq!(user_targets(&cfg).len(), 6);
    }

    #[test]
    fn user_targets_includes_the_nix_profiles_directory() {
        let cfg = sample_user_config();
        assert!(user_targets(&cfg).iter().any(|t| matches!(
            t,
            Target::Directory { path, mode, owner: Some((1000, 1000)) }
                if path.ends_with(".local/state/nix/profiles") && *mode == 0o755
        )));
    }

    #[test]
    fn user_targets_includes_a_flake_lock_with_no_expected_content() {
        let cfg = sample_user_config();
        assert!(user_targets(&cfg).iter().any(|t| matches!(
            t,
            Target::File { path, expected: None, owner: Some((1000, 1000)) }
                if path.ends_with("flake.lock")
        )));
    }

    #[test]
    fn user_targets_includes_a_marker_keyed_by_uid_under_the_root_owned_tree() {
        let cfg = sample_user_config();
        assert!(user_targets(&cfg).iter().any(|t| matches!(
            t,
            Target::File { path, expected, owner: None }
                if path.as_path() == Path::new("/nix/.mix-managed-users/1000") && expected.as_deref() == Some("")
        )));
    }
}
