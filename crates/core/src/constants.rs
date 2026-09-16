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

    pub const LOCK_FILE: &str = "/run/mix.lock";

    pub const MIX_STATE_DIR: &str = ".local/state/mix";
    pub const MIX_STATE_DIR_MODE: u32 = 0o700;
    pub const FLAKE_NIX: &str = "flake.nix";
    pub const HOME_NIX: &str = "home.nix";
    pub const FLAKE_LOCK: &str = "flake.lock";

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

    pub fn group_exists(name: &str) -> bool {
        nix::unistd::Group::from_name(name).ok().flatten().is_some()
    }

    pub fn group_has_gid(name: &str, gid: u32) -> bool {
        nix::unistd::Group::from_name(name)
            .ok()
            .flatten()
            .is_some_and(|group| group.gid.as_raw() == gid)
    }

    pub fn group_has_member(name: &str, user: &str) -> bool {
        let Some(group) = nix::unistd::Group::from_name(name).ok().flatten() else {
            return false;
        };
        group.mem.iter().any(|member| member == user)
            || nix::unistd::User::from_name(user)
                .ok()
                .flatten()
                .is_some_and(|resolved| resolved.gid == group.gid)
    }

    pub fn user_exists(name: &str) -> bool {
        nix::unistd::User::from_name(name).ok().flatten().is_some()
    }

    pub fn user_has_gid(name: &str, gid: u32) -> bool {
        nix::unistd::User::from_name(name)
            .ok()
            .flatten()
            .is_some_and(|user| user.gid.as_raw() == gid)
    }

    pub fn user_has_uid(name: &str, uid: u32) -> bool {
        nix::unistd::User::from_name(name)
            .ok()
            .flatten()
            .is_some_and(|user| user.uid.as_raw() == uid)
    }

    pub fn user_matches(name: &str, uid: u32, gid: u32) -> bool {
        nix::unistd::User::from_name(name)
            .ok()
            .flatten()
            .is_some_and(|user| user.uid.as_raw() == uid && user.gid.as_raw() == gid)
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

        #[test]
        fn group_exists_true_for_a_known_system_group() {
            assert!(group_exists("root"));
        }

        #[test]
        fn group_exists_false_for_a_nonexistent_group() {
            assert!(!group_exists("mix-test-nonexistent-group-xyz"));
        }

        #[test]
        fn group_has_gid_true_for_a_known_system_group() {
            assert!(group_has_gid("root", 0));
        }

        #[test]
        fn group_has_gid_false_for_the_wrong_gid() {
            assert!(!group_has_gid("root", 9999));
        }

        #[test]
        fn group_has_gid_false_for_a_nonexistent_group() {
            assert!(!group_has_gid("mix-test-nonexistent-group-xyz", 0));
        }

        #[test]
        fn group_has_member_counts_a_primary_group_as_membership() {
            assert!(group_has_member("root", "root"));
        }

        #[test]
        fn group_has_member_false_for_a_user_outside_the_group() {
            assert!(!group_has_member("root", "mix-test-nonexistent-user-xyz"));
        }

        #[test]
        fn group_has_member_false_for_a_nonexistent_group() {
            assert!(!group_has_member("mix-test-nonexistent-group-xyz", "root"));
        }

        #[test]
        fn user_exists_true_for_a_known_system_user() {
            assert!(user_exists("root"));
        }

        #[test]
        fn user_exists_false_for_a_nonexistent_user() {
            assert!(!user_exists("mix-test-nonexistent-user-xyz"));
        }

        #[test]
        fn user_has_gid_true_for_a_known_system_user() {
            assert!(user_has_gid("root", 0));
        }

        #[test]
        fn user_has_gid_false_for_a_nonexistent_user() {
            assert!(!user_has_gid("mix-test-nonexistent-user-xyz", 0));
        }

        #[test]
        fn user_has_uid_true_for_a_known_system_user() {
            assert!(user_has_uid("root", 0));
        }

        #[test]
        fn user_has_uid_false_for_a_nonexistent_user() {
            assert!(!user_has_uid("mix-test-nonexistent-user-xyz", 0));
        }

        #[test]
        fn user_matches_true_for_a_known_system_user() {
            assert!(user_matches("root", 0, 0));
        }

        #[test]
        fn user_matches_false_for_the_wrong_uid() {
            assert!(!user_matches("root", 1, 0));
        }

        #[test]
        fn user_matches_false_for_the_wrong_gid() {
            assert!(!user_matches("root", 0, 1));
        }

        #[test]
        fn user_matches_false_for_a_nonexistent_user() {
            assert!(!user_matches("mix-test-nonexistent-user-xyz", 0, 0));
        }
    }
}
