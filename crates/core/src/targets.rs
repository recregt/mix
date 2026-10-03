use std::borrow::Cow;
use std::path::Path;

use crate::identity::InvokingUser;
use crate::identity::{
    self, MIX_USERS_GID, MIX_USERS_GROUP, NIXBLD_GID, NIXBLD_GROUP, NIXBLD_UID_BASE,
    NIXBLD_USER_COUNT,
};
use crate::paths::{
    DEFAULT_PROFILE_NIX_ENV, FLAKE_LOCK, FLAKE_NIX, HOME_NIX, MIX_DAEMON_SERVICE_DEST,
    MIX_DAEMON_SERVICE_UNIT, MIX_DAEMON_SOCKET_DEST, MIX_DAEMON_SOCKET_UNIT, MIX_STATE_DIR_MODE,
    NIX_CONF_DEST, NIX_DAEMON_SERVICE_DEST, NIX_DAEMON_SERVICE_SRC, NIX_DAEMON_SERVICE_UNIT,
    NIX_DAEMON_SOCKET_DEST, NIX_DAEMON_SOCKET_SRC, NIX_DAEMON_SOCKET_UNIT, NIX_OWNERSHIP_MARKER,
    NIX_PROFILES_DIR_MODE, NIX_STORE, NIX_TREE_MODE, NIX_TREE_PATHS, POLICY_FILE,
    PROFILE_SNIPPET_DEST, STATE_FILE, mix_state_dir, nix_profiles_dir,
};
use crate::policy::Policy;

pub const MIX_DAEMON_SOCKET: &str = "[Unit]
Description=mix daemon socket

[Socket]
ListenStream=/run/mix/daemon.sock
SocketMode=0666
DirectoryMode=0755

[Install]
WantedBy=sockets.target
";

pub const MIX_DAEMON_SERVICE: &str = "[Unit]
Description=mix daemon
Requires=mix-daemon.socket
After=mix-daemon.socket nix-daemon.socket

[Service]
ExecStart=/var/lib/mix/bin/mix-daemon serve
";

pub const PROFILE_SNIPPET: &str = "# Managed by mix -- do not edit, changes are overwritten and will trip `mix doctor`.\nif [ -e '/nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh' ]; then\n    . '/nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh'\nfi\n";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserConfig {
    pub user: InvokingUser,
    pub flake: String,
    pub lock: String,
    pub home: String,
    pub restored_state: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Filesystem,
    Identity,
    Services,
    Configuration,
}

impl Category {
    #[cfg(test)]
    pub(crate) const ALL: [Category; 4] = [
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitSource {
    File(&'static str),
    Text(&'static str),
}

#[derive(Debug, Clone)]
pub enum Target<'a> {
    Directory {
        path: Cow<'a, Path>,
        mode: u32,
        owner: Owner,
    },
    File {
        path: Cow<'a, Path>,
        expected: Option<Cow<'a, str>>,
        owner: Owner,
    },
    SeededFile {
        path: Cow<'a, Path>,
        seed: Cow<'a, str>,
        owner: Owner,
    },
    Group {
        name: &'static str,
        gid: u32,
    },
    GroupMember {
        group: &'static str,
        user: String,
    },
    User {
        n: u32,
        uid: u32,
        gid: u32,
    },
    SystemdUnit {
        name: &'static str,
        src: UnitSource,
        dest: &'static str,
        must_be_active: bool,
    },
    PathExists {
        name: &'static str,
        path: &'static str,
    },
}

impl Target<'_> {
    pub fn into_owned(self) -> Target<'static> {
        let own_path = |path: Cow<'_, Path>| Cow::Owned(path.into_owned());
        match self {
            Target::Directory { path, mode, owner } => Target::Directory {
                path: own_path(path),
                mode,
                owner,
            },
            Target::File {
                path,
                expected,
                owner,
            } => Target::File {
                path: own_path(path),
                expected: expected.map(|expected| Cow::Owned(expected.into_owned())),
                owner,
            },
            Target::SeededFile { path, seed, owner } => Target::SeededFile {
                path: own_path(path),
                seed: Cow::Owned(seed.into_owned()),
                owner,
            },
            Target::Group { name, gid } => Target::Group { name, gid },
            Target::GroupMember { group, user } => Target::GroupMember { group, user },
            Target::User { n, uid, gid } => Target::User { n, uid, gid },
            Target::SystemdUnit {
                name,
                src,
                dest,
                must_be_active,
            } => Target::SystemdUnit {
                name,
                src,
                dest,
                must_be_active,
            },
            Target::PathExists { name, path } => Target::PathExists { name, path },
        }
    }

    pub fn label(&self) -> Cow<'_, str> {
        match self {
            Target::Directory { path, .. } => path.to_string_lossy(),
            Target::File { path, .. } => path.to_string_lossy(),
            Target::SeededFile { path, .. } => path.to_string_lossy(),
            Target::Group { name, .. } => Cow::Borrowed(name),
            Target::GroupMember { user, .. } => Cow::Borrowed(user),
            Target::User { n, .. } => identity::user_name(*n),
            Target::SystemdUnit { name, .. } => Cow::Borrowed(name),
            Target::PathExists { name, .. } => Cow::Borrowed(name),
        }
    }

    pub fn category(&self) -> Category {
        match self {
            Target::Directory { .. } => Category::Filesystem,
            Target::File { path, .. } if is_configuration(path) => Category::Configuration,
            Target::File { .. } => Category::Filesystem,
            Target::SeededFile { .. } => Category::Filesystem,
            Target::Group { .. } | Target::GroupMember { .. } | Target::User { .. } => {
                Category::Identity
            }
            Target::SystemdUnit { .. } => Category::Services,
            Target::PathExists { .. } => Category::Filesystem,
        }
    }
}

fn is_configuration(path: &Path) -> bool {
    let path = path.as_os_str().as_encoded_bytes();
    [NIX_CONF_DEST, POLICY_FILE, PROFILE_SNIPPET_DEST]
        .iter()
        .any(|configuration| configuration.as_bytes() == path)
}

fn push_user_targets<'a>(items: &mut Vec<Target<'a>>, cfg: &'a UserConfig) {
    let state_dir = mix_state_dir(&cfg.user.home);
    let owner = Some((cfg.user.uid, cfg.user.gid));
    items.push(Target::Directory {
        path: Cow::Owned(state_dir.clone()),
        mode: MIX_STATE_DIR_MODE,
        owner,
    });
    items.push(Target::Directory {
        path: Cow::Owned(nix_profiles_dir(&cfg.user.home)),
        mode: NIX_PROFILES_DIR_MODE,
        owner,
    });
    items.push(Target::File {
        path: Cow::Owned(state_dir.join(HOME_NIX)),
        expected: Some(Cow::Borrowed(&cfg.home)),
        owner,
    });
    items.push(Target::File {
        path: Cow::Owned(state_dir.join(FLAKE_NIX)),
        expected: Some(Cow::Borrowed(&cfg.flake)),
        owner,
    });
    items.push(Target::File {
        path: Cow::Owned(state_dir.join(FLAKE_LOCK)),
        expected: Some(Cow::Borrowed(&cfg.lock)),
        owner,
    });
    match &cfg.restored_state {
        Some(restored) => items.push(Target::File {
            path: Cow::Owned(state_dir.join(STATE_FILE)),
            expected: Some(Cow::Borrowed(restored)),
            owner,
        }),
        None => items.push(Target::SeededFile {
            path: Cow::Owned(state_dir.join(STATE_FILE)),
            seed: Cow::Borrowed(crate::state::StateManifest::seed_rendered()),
            owner,
        }),
    }
    items.push(Target::GroupMember {
        group: MIX_USERS_GROUP,
        user: cfg.user.name.clone(),
    });
}

const SYSTEM_TARGET_COUNT: usize = 13 + NIX_TREE_PATHS.len() + NIXBLD_USER_COUNT as usize;
const USER_TARGET_COUNT: usize = 7;

pub fn user_targets(cfg: &UserConfig) -> Vec<Target<'_>> {
    let mut items = Vec::with_capacity(USER_TARGET_COUNT);
    push_user_targets(&mut items, cfg);
    items
}

pub fn targets<'a>(user_config: Option<&'a UserConfig>, policy: &'a Policy) -> Vec<Target<'a>> {
    let mut items = Vec::with_capacity(
        SYSTEM_TARGET_COUNT
            + if user_config.is_some() {
                USER_TARGET_COUNT
            } else {
                0
            },
    );
    items.push(Target::Directory {
        path: Cow::Borrowed(Path::new("/nix")),
        mode: 0o755,
        owner: None,
    });
    items.push(Target::Directory {
        path: Cow::Borrowed(Path::new(NIX_STORE)),
        mode: 0o1775,
        owner: None,
    });
    items.extend(NIX_TREE_PATHS.iter().map(|&path| Target::Directory {
        path: Cow::Borrowed(Path::new(path)),
        mode: NIX_TREE_MODE,
        owner: None,
    }));
    items.push(Target::File {
        path: Cow::Borrowed(Path::new(NIX_OWNERSHIP_MARKER)),
        expected: Some(Cow::Borrowed("")),
        owner: None,
    });
    items.push(Target::File {
        path: Cow::Borrowed(Path::new(POLICY_FILE)),
        expected: Some(Cow::Borrowed(policy.render())),
        owner: None,
    });
    items.push(Target::File {
        path: Cow::Borrowed(Path::new(NIX_CONF_DEST)),
        expected: Some(Cow::Borrowed(policy.nix_conf())),
        owner: None,
    });
    items.push(Target::File {
        path: Cow::Borrowed(Path::new(PROFILE_SNIPPET_DEST)),
        expected: Some(Cow::Borrowed(PROFILE_SNIPPET)),
        owner: None,
    });
    items.push(Target::Group {
        name: NIXBLD_GROUP,
        gid: NIXBLD_GID,
    });
    items.push(Target::Group {
        name: MIX_USERS_GROUP,
        gid: MIX_USERS_GID,
    });
    items.extend((1..=NIXBLD_USER_COUNT).map(|n| Target::User {
        n,
        uid: NIXBLD_UID_BASE + n,
        gid: NIXBLD_GID,
    }));
    items.push(Target::SystemdUnit {
        name: NIX_DAEMON_SERVICE_UNIT,
        src: UnitSource::File(NIX_DAEMON_SERVICE_SRC),
        dest: NIX_DAEMON_SERVICE_DEST,
        must_be_active: false,
    });
    items.push(Target::SystemdUnit {
        name: NIX_DAEMON_SOCKET_UNIT,
        src: UnitSource::File(NIX_DAEMON_SOCKET_SRC),
        dest: NIX_DAEMON_SOCKET_DEST,
        must_be_active: true,
    });
    items.push(Target::SystemdUnit {
        name: MIX_DAEMON_SERVICE_UNIT,
        src: UnitSource::Text(MIX_DAEMON_SERVICE),
        dest: MIX_DAEMON_SERVICE_DEST,
        must_be_active: false,
    });
    items.push(Target::SystemdUnit {
        name: MIX_DAEMON_SOCKET_UNIT,
        src: UnitSource::Text(MIX_DAEMON_SOCKET),
        dest: MIX_DAEMON_SOCKET_DEST,
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
    use std::path::PathBuf;

    use super::*;

    fn default_policy() -> &'static Policy {
        static POLICY: std::sync::OnceLock<Policy> = std::sync::OnceLock::new();
        POLICY.get_or_init(Policy::default)
    }

    fn sample_user_config() -> UserConfig {
        UserConfig {
            user: InvokingUser {
                uid: 1000,
                gid: 1000,
                name: "mix-user".to_string(),
                home: PathBuf::from("/home/mix-user"),
            },
            flake: "flake-content".to_string(),
            lock: "lock-content".to_string(),
            home: "home-content".to_string(),
            restored_state: None,
        }
    }

    #[test]
    fn label_uses_the_path_for_path_based_targets() {
        let target = Target::Directory {
            path: PathBuf::from("/nix").into(),
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
        for target in targets(Some(&cfg), default_policy()) {
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
                path: PathBuf::from("/nix").into(),
                mode: 0o755,
                owner: None,
            }
            .category(),
            Category::Filesystem
        );
        assert_eq!(
            Target::File {
                path: PathBuf::from(NIX_OWNERSHIP_MARKER).into(),
                expected: Some(String::new().into()),
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
                path: PathBuf::from(NIX_CONF_DEST).into(),
                expected: Some(default_policy().nix_conf().into()),
                owner: None,
            }
            .category(),
            Category::Configuration
        );
        assert_eq!(
            Target::File {
                path: PathBuf::from(PROFILE_SNIPPET_DEST).into(),
                expected: Some(PROFILE_SNIPPET.to_string().into()),
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
        assert_eq!(
            Target::GroupMember {
                group: MIX_USERS_GROUP,
                user: "mix-user".to_string(),
            }
            .category(),
            Category::Identity
        );
    }

    #[test]
    fn category_groups_systemd_units_as_services() {
        assert_eq!(
            Target::SystemdUnit {
                name: NIX_DAEMON_SOCKET_UNIT,
                src: UnitSource::File(NIX_DAEMON_SOCKET_SRC),
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
        let items = targets(None, default_policy());
        for &path in NIX_TREE_PATHS {
            assert!(
                items.iter().any(|t| matches!(
                    t,
                    Target::Directory { path: p, mode, .. }
                        if p.as_ref() == Path::new(path) && *mode == NIX_TREE_MODE
                )),
                "targets() is missing an entry for {path} (mode {NIX_TREE_MODE:o})"
            );
        }
    }

    #[test]
    fn targets_include_all_build_users() {
        let items = targets(None, default_policy());
        let user_count = items
            .iter()
            .filter(|t| matches!(t, Target::User { .. }))
            .count();
        assert_eq!(user_count, NIXBLD_USER_COUNT as usize);
    }

    #[test]
    fn targets_excludes_per_user_entries_when_no_user_is_given() {
        let items = targets(None, default_policy());
        assert!(
            !items
                .iter()
                .any(|t| matches!(t, Target::Directory { path, .. } if path.ends_with("mix")))
        );
    }

    #[test]
    fn targets_includes_per_user_entries_when_a_user_is_given() {
        let cfg = sample_user_config();
        let items = targets(Some(&cfg), default_policy());

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
    fn the_declared_nix_conf_and_policy_follow_the_policy() {
        let policy = Policy::new(Some("https://mirror.internal"), Some("m:AAAA")).unwrap();

        let items = targets(None, &policy);

        let expected = |wanted: &Path| {
            items.iter().find_map(|target| match target {
                Target::File { path, expected, .. } if path.as_ref() == wanted => {
                    expected.clone().map(Cow::into_owned)
                }
                _ => None,
            })
        };
        assert_eq!(
            expected(Path::new(NIX_CONF_DEST)).as_deref(),
            Some(policy.nix_conf())
        );
        assert_eq!(
            expected(Path::new(POLICY_FILE)).as_deref(),
            Some(policy.render())
        );
    }

    #[test]
    fn the_target_lists_are_built_at_their_exact_size() {
        let cfg = sample_user_config();
        assert_eq!(targets(None, default_policy()).len(), SYSTEM_TARGET_COUNT);
        assert_eq!(user_targets(&cfg).len(), USER_TARGET_COUNT);
        assert_eq!(
            targets(Some(&cfg), default_policy()).len(),
            SYSTEM_TARGET_COUNT + USER_TARGET_COUNT
        );
    }

    #[test]
    fn every_user_gets_the_same_nix_conf() {
        let cfg = sample_user_config();
        let policy = Policy::default();
        let expected: Option<Cow<'_, str>> = Some(Cow::Borrowed(policy.nix_conf()));

        for items in [targets(None, &policy), targets(Some(&cfg), &policy)] {
            assert!(items.iter().any(|t| matches!(
                t,
                Target::File { path, expected: actual, .. }
                    if path.as_ref() == Path::new(NIX_CONF_DEST) && *actual == expected
            )));
        }
    }

    #[test]
    fn targets_include_the_group_the_nix_conf_trusts() {
        assert!(targets(None, default_policy()).iter().any(|t| matches!(
            t,
            Target::Group { name, gid } if *name == MIX_USERS_GROUP && *gid == MIX_USERS_GID
        )));
    }

    #[test]
    fn user_targets_returns_exactly_the_seven_per_user_entries() {
        let cfg = sample_user_config();
        assert_eq!(user_targets(&cfg).len(), 7);
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
    fn user_targets_includes_the_rendered_flake_lock() {
        let cfg = sample_user_config();
        assert!(user_targets(&cfg).iter().any(|t| matches!(
            t,
            Target::File { path, expected: Some(expected), owner: Some((1000, 1000)) }
                if path.ends_with("flake.lock") && expected == &cfg.lock
        )));
    }

    #[test]
    fn user_targets_includes_the_state_file_seeded_with_git() {
        let cfg = sample_user_config();
        assert!(user_targets(&cfg).iter().any(|t| matches!(
            t,
            Target::SeededFile { path, seed, owner: Some((1000, 1000)) }
                if path.ends_with("state")
                    && seed.as_ref() == crate::state::StateManifest::seed().render()
        )));
    }

    #[test]
    fn user_targets_enroll_the_user_in_the_managed_group() {
        let cfg = sample_user_config();
        assert!(user_targets(&cfg).iter().any(|t| matches!(
            t,
            Target::GroupMember { group, user }
                if *group == MIX_USERS_GROUP && user == "mix-user"
        )));
    }
}

#[cfg(test)]
mod daemon_unit_tests {
    use super::*;

    #[test]
    fn the_units_name_the_socket_and_the_binary_mix_installs() {
        assert!(MIX_DAEMON_SERVICE.contains(&format!(
            "ExecStart={} serve\n",
            crate::paths::MIX_DAEMON_BIN
        )));
        assert!(MIX_DAEMON_SERVICE.contains(&format!("Requires={MIX_DAEMON_SOCKET_UNIT}\n")));
    }
}
