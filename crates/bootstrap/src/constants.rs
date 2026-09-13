pub const NIXBLD_GROUP: &str = "nixbld";
pub const NIXBLD_GID: u32 = 30_000;
pub const NIXBLD_USER_COUNT: u32 = 32;
pub const NIXBLD_UID_BASE: u32 = 30_000;
pub const NIXBLD_HOME: &str = "/var/empty";
pub const NIXBLD_SHELL: &str = "/usr/sbin/nologin";

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

pub const NIX_CONF: &str = include_str!("../assets/nix.conf");
pub const PROFILE_SNIPPET: &str = include_str!("../assets/mix-nix.sh");
pub const NIX_CONF_DEST: &str = "/etc/nix/nix.conf";
pub const PROFILE_SNIPPET_DEST: &str = "/etc/profile.d/mix-nix.sh";
