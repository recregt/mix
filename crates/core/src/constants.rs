pub mod paths {
    pub const NIX_TREE_MODE: u32 = 0o755;

    pub const NIX_TREE_PATHS: &[&str] = &[
        "/nix/var",
        "/nix/var/log",
        "/nix/var/log/nix",
        "/nix/var/log/nix/drvs",
        "/nix/var/nix",
        "/nix/var/nix/db",
        "/nix/var/nix/gcroots",
        "/nix/var/nix/gcroots/per-user",
        "/nix/var/nix/profiles",
        "/nix/var/nix/profiles/per-user",
        "/nix/var/nix/temproots",
        "/nix/var/nix/userpool",
        "/nix/var/nix/daemon-socket",
    ];

    pub const SYSTEMD_UNIT_DIR: &str = "/etc/systemd/system";
    pub const NIX_DAEMON_SERVICE_UNIT: &str = "nix-daemon.service";
    pub const NIX_DAEMON_SOCKET_UNIT: &str = "nix-daemon.socket";

    pub const NIX_DAEMON_SERVICE_SRC: &str =
        "/nix/var/nix/profiles/default/lib/systemd/system/nix-daemon.service";
    pub const NIX_DAEMON_SERVICE_DEST: &str = "/etc/systemd/system/nix-daemon.service";
    pub const NIX_DAEMON_SOCKET_SRC: &str =
        "/nix/var/nix/profiles/default/lib/systemd/system/nix-daemon.socket";
    pub const NIX_DAEMON_SOCKET_DEST: &str = "/etc/systemd/system/nix-daemon.socket";

    pub const NIX_OWNERSHIP_MARKER: &str = "/nix/.mix-managed";
    pub const NIX_STORE: &str = "/nix/store";
    pub const NIX_PROVISIONING_MANIFEST: &str = "/nix/.mix-provisioning-manifest";

    pub const DEFAULT_PROFILE_BIN: &str = "/nix/var/nix/profiles/default/bin";
    pub const DEFAULT_PROFILE_NIX_ENV: &str = "/nix/var/nix/profiles/default/bin/nix-env";
    pub const DEFAULT_PROFILE_NIX: &str = "/nix/var/nix/profiles/default/bin/nix";

    pub const NIX_CONF_DEST: &str = "/etc/nix/nix.conf";
    pub const PROFILE_SNIPPET_DEST: &str = "/etc/profile.d/mix-nix.sh";
    pub const POLICY_FILE: &str = "/etc/mix/policy.json";

    pub const LOCK_FILE: &str = "/var/lib/mix/lock";
    pub const MIX_VAR_DIR: &str = "/var/lib/mix";
    pub const MIX_BIN_DIR: &str = "/var/lib/mix/bin";
    pub const MIX_DAEMON_BIN: &str = "/var/lib/mix/bin/mix-daemon";
    pub const MIX_DAEMON_BIN_MODE: u32 = 0o755;
    pub const MIX_DAEMON_SOCKET_PATH: &str = "/run/mix/daemon.sock";
    pub const MIX_DAEMON_SOCKET_UNIT: &str = "mix-daemon.socket";
    pub const MIX_DAEMON_SERVICE_UNIT: &str = "mix-daemon.service";
    pub const MIX_DAEMON_SOCKET_DEST: &str = "/etc/systemd/system/mix-daemon.socket";
    pub const MIX_DAEMON_SERVICE_DEST: &str = "/etc/systemd/system/mix-daemon.service";

    pub const MIX_STATE_DIR: &str = ".local/state/mix";
    pub const MIX_STATE_DIR_MODE: u32 = 0o700;
    pub const FLAKE_NIX: &str = "flake.nix";
    pub const HOME_NIX: &str = "home.nix";
    pub const FLAKE_LOCK: &str = "flake.lock";
    pub const STATE_FILE: &str = "state";

    pub const GENERATION_STATE_FILE: &str = "mix-state";

    pub const NIX_PROFILES_DIR: &str = ".local/state/nix/profiles";
    pub const NIX_PROFILES_DIR_MODE: u32 = 0o755;
    pub const HOME_MANAGER_PROFILE_NAME: &str = "home-manager";

    pub fn mix_state_dir(home: &std::path::Path) -> std::path::PathBuf {
        home.join(MIX_STATE_DIR)
    }

    pub fn nix_profiles_dir(home: &std::path::Path) -> std::path::PathBuf {
        home.join(NIX_PROFILES_DIR)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn mix_state_dir_joins_home_and_the_state_dir_fragment() {
            assert_eq!(
                mix_state_dir(std::path::Path::new("/home/mix-user")),
                std::path::PathBuf::from("/home/mix-user/.local/state/mix")
            );
        }
    }
}

pub mod identity {
    pub const NIXBLD_GROUP: &str = "nixbld";
    pub const NIXBLD_GID: u32 = 30_000;
    pub const NIXBLD_USER_COUNT: u32 = 32;
    pub const NIXBLD_UID_BASE: u32 = 30_000;
    pub const NIXBLD_HOME: &str = "/var/empty";
    pub const NIXBLD_SHELL: &str = "/usr/sbin/nologin";

    pub const MIX_USERS_GROUP: &str = "mix-users";
    pub const MIX_USERS_GID: u32 = 30_100;

    const NIXBLD_USER_NAMES: [&str; NIXBLD_USER_COUNT as usize] = [
        "nixbld1", "nixbld2", "nixbld3", "nixbld4", "nixbld5", "nixbld6", "nixbld7", "nixbld8",
        "nixbld9", "nixbld10", "nixbld11", "nixbld12", "nixbld13", "nixbld14", "nixbld15",
        "nixbld16", "nixbld17", "nixbld18", "nixbld19", "nixbld20", "nixbld21", "nixbld22",
        "nixbld23", "nixbld24", "nixbld25", "nixbld26", "nixbld27", "nixbld28", "nixbld29",
        "nixbld30", "nixbld31", "nixbld32",
    ];

    pub fn user_name(n: u32) -> std::borrow::Cow<'static, str> {
        match NIXBLD_USER_NAMES.get((n as usize).wrapping_sub(1)) {
            Some(&name) => std::borrow::Cow::Borrowed(name),
            None => std::borrow::Cow::Owned(format!("{NIXBLD_GROUP}{n}")),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn user_name_borrows_a_static_name_for_every_managed_build_user() {
            for n in 1..=NIXBLD_USER_COUNT {
                let name = user_name(n);
                assert_eq!(name, format!("{NIXBLD_GROUP}{n}"));
                assert!(matches!(name, std::borrow::Cow::Borrowed(_)));
            }
        }

        #[test]
        fn user_name_falls_back_to_formatting_outside_the_managed_range() {
            assert_eq!(user_name(0), "nixbld0");
            assert_eq!(user_name(NIXBLD_USER_COUNT + 1), "nixbld33");
            assert!(matches!(user_name(0), std::borrow::Cow::Owned(_)));
        }
    }
}
