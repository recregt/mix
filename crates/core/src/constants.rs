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

    pub const DEFAULT_PROFILE_NIX_ENV: &str = "/nix/var/nix/profiles/default/bin/nix-env";

    pub const NIX_CONF_DEST: &str = "/etc/nix/nix.conf";
    pub const PROFILE_SNIPPET_DEST: &str = "/etc/profile.d/mix-nix.sh";

    pub const LOCK_FILE: &str = "/run/mix.lock";
}

pub mod identity {
    pub const NIXBLD_GROUP: &str = "nixbld";
    pub const NIXBLD_GID: u32 = 30_000;
    pub const NIXBLD_USER_COUNT: u32 = 32;
    pub const NIXBLD_UID_BASE: u32 = 30_000;
    pub const NIXBLD_HOME: &str = "/var/empty";
    pub const NIXBLD_SHELL: &str = "/usr/sbin/nologin";

    pub fn user_name(n: u32) -> String {
        format!("{NIXBLD_GROUP}{n}")
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
