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

pub const NIXOS_MARKER: &str = "/etc/NIXOS";
pub const NIX_OWNERSHIP_MARKER: &str = "/nix/.mix-managed";
pub const NIX_STORE: &str = "/nix/store";
pub const NIX_PROVISIONING_MANIFEST: &str = "/nix/.mix-provisioning-manifest";

pub const DEFAULT_PROFILE_BIN: &str = "/nix/var/nix/profiles/default/bin";
pub const DEFAULT_PROFILE_NIX_ENV: &str = "/nix/var/nix/profiles/default/bin/nix-env";
pub const DEFAULT_PROFILE_NIX: &str = "/nix/var/nix/profiles/default/bin/nix";
pub const DEFAULT_PROFILE_NIX_STORE: &str = "/nix/var/nix/profiles/default/bin/nix-store";

pub const NIX_CONF_DEST: &str = "/etc/nix/nix.conf";
pub const PROFILE_SNIPPET_DEST: &str = "/etc/profile.d/mix-nix.sh";
pub const POLICY_FILE: &str = "/etc/mix/policy.json";

pub const LOCK_FILE: &str = "/var/lib/mix/lock";
pub const JOURNAL_DIR: &str = "/var/lib/mix/journal";
pub const RUNNING_PROGRAM: &str = "/proc/self/exe";
pub const USER_PROFILE_NAME: &str = "profile";
pub const PROFILE_LOCK_SUFFIX: &str = ".lock";
pub const MIX_VAR_DIR: &str = "/var/lib/mix";
pub const MIX_BIN_DIR: &str = "/var/lib/mix/bin";
pub const MIX_DAEMON_BIN: &str = "/var/lib/mix/bin/mix-daemon";
pub const MIX_DAEMON_BIN_MODE: u32 = 0o755;
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
pub const GIT_DIR: &str = ".git";
pub const INDEX_LOCK: &str = "index.lock";
pub const REPOSITORY_HEAD: &str = "HEAD";
pub const REPOSITORY_BRANCH: &str = "refs/heads/main";
pub const REPOSITORY_CONFIG: &str = "config";
pub const REPOSITORY_INDEX: &str = "index";
pub const REPOSITORY_CONFIG_CONTENTS: &str =
    "[core]\n\trepositoryformatversion = 0\n\tfilemode = true\n\tbare = false\n";
pub const GITIGNORE: &str = ".gitignore";
pub const GITIGNORE_CONTENTS: &str = "# Managed by mix -- do not edit, changes are overwritten.\n/*\n!/.gitignore\n!/flake.lock\n!/flake.nix\n!/home.nix\n!/state\n";

pub const MANAGED_FILES: [&str; 5] = [GITIGNORE, FLAKE_LOCK, FLAKE_NIX, HOME_NIX, STATE_FILE];

pub const GENERATION_STATE_FILE: &str = "mix-state";

pub const GENERATION_INPUTS: [(&str, &str); 4] = [
    (STATE_FILE, GENERATION_STATE_FILE),
    (FLAKE_NIX, "mix-flake.nix"),
    (FLAKE_LOCK, "mix-flake.lock"),
    (HOME_NIX, "mix-home.nix"),
];

pub const NIX_PROFILES_DIR: &str = ".local/state/nix/profiles";
pub const NIX_PROFILES_DIR_MODE: u32 = 0o755;
pub const HOME_MANAGER_PROFILE_NAME: &str = "home-manager";

/// What a write mix was interrupted in names the siblings it leaves: `.<name>.mix-<purpose>-...`.
pub const LEFTOVER_PURPOSES: [&str; 5] = ["backup", "aside", "new", "remove", "reclaim"];

/// Whether `name` is a sibling an interrupted write of mix's left behind.
pub fn is_leftover(name: &str) -> bool {
    let Some(rest) = name.strip_prefix('.') else {
        return false;
    };
    rest.match_indices(".mix-").any(|(at, marker)| {
        at > 0
            && LEFTOVER_PURPOSES.iter().any(|purpose| {
                rest[at + marker.len()..]
                    .strip_prefix(purpose)
                    .is_some_and(|tail| tail.starts_with('-') && tail.len() > 1)
            })
    })
}

/// `base.join(rel)` in a single allocation.
pub fn join(base: &std::path::Path, rel: &str) -> std::path::PathBuf {
    let mut path = std::path::PathBuf::with_capacity(base.as_os_str().len() + 1 + rel.len());
    path.push(base);
    path.push(rel);
    path
}

pub fn mix_state_dir(home: &std::path::Path) -> std::path::PathBuf {
    join(home, MIX_STATE_DIR)
}

pub fn repository_dir(home: &std::path::Path) -> std::path::PathBuf {
    join(&mix_state_dir(home), GIT_DIR)
}

pub fn active_list_path(home: &std::path::Path) -> std::path::PathBuf {
    nix_profiles_dir(home)
        .join(HOME_MANAGER_PROFILE_NAME)
        .join(GENERATION_STATE_FILE)
}

pub fn nix_profiles_dir(home: &std::path::Path) -> std::path::PathBuf {
    join(home, NIX_PROFILES_DIR)
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
