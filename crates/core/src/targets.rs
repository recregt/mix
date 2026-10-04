use std::borrow::Cow;
use std::path::{Path, PathBuf};

use crate::identity::InvokingUser;
use crate::identity::{
    self, MIX_USERS_GID, MIX_USERS_GROUP, NIXBLD_GID, NIXBLD_GROUP, NIXBLD_UID_BASE,
    NIXBLD_USER_COUNT,
};
use crate::paths::{
    DEFAULT_PROFILE_NIX_ENV, FLAKE_LOCK, FLAKE_NIX, GITIGNORE, GITIGNORE_CONTENTS, HOME_NIX,
    MIX_BIN_DIR, MIX_DAEMON_SERVICE_DEST, MIX_DAEMON_SERVICE_UNIT, MIX_DAEMON_SOCKET_DEST,
    MIX_DAEMON_SOCKET_UNIT, MIX_STATE_DIR_MODE, MIX_VAR_DIR, NIX_CONF_DEST,
    NIX_DAEMON_SERVICE_DEST, NIX_DAEMON_SERVICE_SRC, NIX_DAEMON_SERVICE_UNIT,
    NIX_DAEMON_SOCKET_DEST, NIX_DAEMON_SOCKET_SRC, NIX_DAEMON_SOCKET_UNIT, NIX_OWNERSHIP_MARKER,
    NIX_PROFILES_DIR_MODE, NIX_STORE, NIX_TREE_MODE, NIX_TREE_PATHS, POLICY_FILE,
    PROFILE_SNIPPET_DEST, STATE_FILE, mix_state_dir, nix_profiles_dir, repository_dir,
};
use crate::paths::{
    HOME_MANAGER_PROFILE_NAME, INDEX_LOCK, JOURNAL_DIR, MIX_DAEMON_BIN, MIX_DAEMON_BIN_MODE,
    RUNNING_PROGRAM,
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

/// Exit status of a `mix-daemon` that finished its accepted requests so systemd can restart it.
pub const MIX_DAEMON_DRAINED: u8 = 75;

pub const MIX_DAEMON_SERVICE: &str = "[Unit]
Description=mix daemon
Requires=mix-daemon.socket
After=mix-daemon.socket nix-daemon.socket

[Service]
Type=notify
ExecStart=/var/lib/mix/bin/mix-daemon serve
KillMode=mixed
TimeoutStopSec=infinity
Restart=on-failure
RestartForceExitStatus=75
SuccessExitStatus=75
ProtectSystem=yes
PrivateTmp=yes
PrivateDevices=yes
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectKernelLogs=yes
ProtectControlGroups=yes
ProtectClock=yes
LockPersonality=yes
NoNewPrivileges=yes

[Install]
WantedBy=multi-user.target
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
    Parent {
        path: Cow<'a, Path>,
        bits: u32,
        owner: Owner,
    },
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
    Repository {
        path: Cow<'a, Path>,
        user: Cow<'a, InvokingUser>,
    },
    Program {
        path: &'static str,
        source: &'static str,
        mode: u32,
    },
    Journals {
        path: &'static str,
    },
    Leftovers {
        user: Option<Cow<'a, InvokingUser>>,
        journals: &'static str,
    },
    Generations {
        path: Cow<'a, Path>,
        user: Cow<'a, InvokingUser>,
    },
    HomeFiles {
        path: Cow<'a, Path>,
        user: Cow<'a, InvokingUser>,
    },
}

/// Name of the target that collects what interrupted writes left behind.
pub const LEFTOVERS: &str = "leftovers of interrupted writes";

impl Target<'_> {
    pub fn into_owned(self) -> Target<'static> {
        let own_path = |path: Cow<'_, Path>| Cow::Owned(path.into_owned());
        match self {
            Target::Parent { path, bits, owner } => Target::Parent {
                path: own_path(path),
                bits,
                owner,
            },
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
            Target::Repository { path, user } => Target::Repository {
                path: own_path(path),
                user: Cow::Owned(user.into_owned()),
            },
            Target::Program { path, source, mode } => Target::Program { path, source, mode },
            Target::Journals { path } => Target::Journals { path },
            Target::Leftovers { user, journals } => Target::Leftovers {
                user: user.map(|user| Cow::Owned(user.into_owned())),
                journals,
            },
            Target::Generations { path, user } => Target::Generations {
                path: own_path(path),
                user: Cow::Owned(user.into_owned()),
            },
            Target::HomeFiles { path, user } => Target::HomeFiles {
                path: own_path(path),
                user: Cow::Owned(user.into_owned()),
            },
        }
    }

    pub fn label(&self) -> Cow<'_, str> {
        match self {
            Target::Parent { path, .. } | Target::Directory { path, .. } => path.to_string_lossy(),
            Target::File { path, .. } => path.to_string_lossy(),
            Target::SeededFile { path, .. } => path.to_string_lossy(),
            Target::Group { name, .. } => Cow::Borrowed(name),
            Target::GroupMember { user, .. } => Cow::Borrowed(user),
            Target::User { n, .. } => identity::user_name(*n),
            Target::SystemdUnit { name, .. } => Cow::Borrowed(name),
            Target::PathExists { name, .. } => Cow::Borrowed(name),
            Target::Repository { path, .. }
            | Target::Generations { path, .. }
            | Target::HomeFiles { path, .. } => path.to_string_lossy(),
            Target::Program { path, .. } | Target::Journals { path } => Cow::Borrowed(path),
            Target::Leftovers { .. } => Cow::Borrowed(LEFTOVERS),
        }
    }

    pub fn category(&self) -> Category {
        match self {
            Target::Parent { .. } | Target::Directory { .. } => Category::Filesystem,
            Target::File { path, .. } if is_configuration(path) => Category::Configuration,
            Target::File { .. } => Category::Filesystem,
            Target::SeededFile { .. } => Category::Filesystem,
            Target::Group { .. } | Target::GroupMember { .. } | Target::User { .. } => {
                Category::Identity
            }
            Target::SystemdUnit { .. } => Category::Services,
            Target::PathExists { .. }
            | Target::Repository { .. }
            | Target::Journals { .. }
            | Target::Leftovers { .. }
            | Target::HomeFiles { .. } => Category::Filesystem,
            Target::Program { .. } => Category::Services,
            Target::Generations { .. } => Category::Configuration,
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
    items.extend(USER_PARENTS.iter().map(|parent| Target::Parent {
        path: Cow::Owned(cfg.user.home.join(parent)),
        bits: USER_PARENT_BITS,
        owner,
    }));
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
    items.push(Target::File {
        path: Cow::Owned(state_dir.join(GITIGNORE)),
        expected: Some(Cow::Borrowed(GITIGNORE_CONTENTS)),
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
    items.push(Target::Repository {
        path: Cow::Owned(repository_dir(&cfg.user.home)),
        user: Cow::Borrowed(&cfg.user),
    });
    items.push(Target::GroupMember {
        group: MIX_USERS_GROUP,
        user: cfg.user.name.clone(),
    });
    items.push(Target::Generations {
        path: Cow::Owned(nix_profiles_dir(&cfg.user.home).join(HOME_MANAGER_PROFILE_NAME)),
        user: Cow::Borrowed(&cfg.user),
    });
    items.push(Target::HomeFiles {
        path: Cow::Borrowed(&cfg.user.home),
        user: Cow::Borrowed(&cfg.user),
    });
}

/// Every directory mix writes into for the machine and `user`: where a write it was
/// interrupted in leaves its siblings.
pub fn written_dirs(user: Option<&InvokingUser>) -> Vec<PathBuf> {
    let policy = Policy::default();
    let config = user.map(|user| UserConfig {
        user: user.clone(),
        flake: String::new(),
        lock: String::new(),
        home: String::new(),
        restored_state: None,
    });
    let items = targets(config.as_ref(), &policy);
    let mut dirs: Vec<PathBuf> = Vec::with_capacity(items.len());
    let mut add = |path: &Path| {
        if let Some(parent) = path.parent() {
            dirs.push(parent.to_path_buf());
        }
    };
    for item in &items {
        match item {
            Target::Parent { path, .. }
            | Target::Directory { path, .. }
            | Target::File { path, .. }
            | Target::SeededFile { path, .. } => add(path),
            Target::SystemdUnit { dest, .. } => add(Path::new(dest)),
            Target::Program { path, .. } => add(Path::new(path)),
            Target::Repository { path, .. } => {
                add(path);
                add(&path.join(INDEX_LOCK));
            }
            Target::Group { .. }
            | Target::GroupMember { .. }
            | Target::User { .. }
            | Target::PathExists { .. }
            | Target::Journals { .. }
            | Target::Leftovers { .. }
            | Target::Generations { .. }
            | Target::HomeFiles { .. } => {}
        }
    }
    dirs.sort();
    dirs.dedup();
    dirs
}

const SYSTEM_PARENTS: [(&str, u32); 5] = [
    ("/etc/mix", 0o755),
    ("/etc/nix", 0o755),
    ("/etc/profile.d", 0o755),
    (MIX_VAR_DIR, 0o700),
    (MIX_BIN_DIR, 0o700),
];
const USER_PARENTS: [&str; 3] = [".local", ".local/state", ".local/state/nix"];
const USER_PARENT_BITS: u32 = 0o700;

const SYSTEM_TARGET_COUNT: usize =
    16 + SYSTEM_PARENTS.len() + NIX_TREE_PATHS.len() + NIXBLD_USER_COUNT as usize;
const USER_TARGET_COUNT: usize = 11 + USER_PARENTS.len();

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
    items.extend(SYSTEM_PARENTS.iter().map(|(path, bits)| Target::Parent {
        path: Cow::Borrowed(Path::new(*path)),
        bits: *bits,
        owner: Some((0, 0)),
    }));
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
    items.push(Target::Program {
        path: MIX_DAEMON_BIN,
        source: RUNNING_PROGRAM,
        mode: MIX_DAEMON_BIN_MODE,
    });
    items.push(Target::Journals { path: JOURNAL_DIR });

    if let Some(cfg) = user_config {
        push_user_targets(&mut items, cfg);
    }
    items.push(Target::Leftovers {
        user: user_config.map(|cfg| Cow::Borrowed(&cfg.user)),
        journals: JOURNAL_DIR,
    });

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
    fn user_targets_returns_exactly_the_fourteen_per_user_entries() {
        let cfg = sample_user_config();
        assert_eq!(user_targets(&cfg).len(), 14);
    }

    #[test]
    fn leftovers_are_looked_for_beside_everything_mix_writes() {
        let cfg = sample_user_config();
        let dirs = written_dirs(Some(&cfg.user));

        for dir in [
            "/etc/nix",
            "/etc/systemd/system",
            "/var/lib/mix/bin",
            "/home/mix-user/.local/state",
            "/home/mix-user/.local/state/mix",
            "/home/mix-user/.local/state/mix/.git",
        ] {
            assert!(dirs.iter().any(|found| found == Path::new(dir)), "{dir}");
        }
    }

    #[test]
    fn user_targets_check_the_repository_after_the_files_it_commits() {
        let cfg = sample_user_config();
        let targets = user_targets(&cfg);
        let position =
            |wanted: &dyn Fn(&Target<'_>) -> bool| targets.iter().position(wanted).unwrap();

        let repository = position(
            &|target| matches!(target, Target::Repository { user, .. } if **user == cfg.user),
        );
        let state = position(&|target| matches!(target, Target::SeededFile { .. }));

        assert!(state < repository);
        assert_eq!(
            targets[repository].label(),
            "/home/mix-user/.local/state/mix/.git"
        );
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

    #[test]
    fn a_drained_daemon_is_restarted_and_not_counted_as_a_failure() {
        for setting in ["RestartForceExitStatus", "SuccessExitStatus"] {
            assert!(
                MIX_DAEMON_SERVICE.contains(&format!("{setting}={MIX_DAEMON_DRAINED}\n")),
                "{setting}"
            );
        }
    }
}
