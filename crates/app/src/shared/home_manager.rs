use mix_core::identity::MIX_USERS_GROUP;
use mix_core::models::UserConfig;
use mix_core::privilege::invoking_user;
use mix_core::system::{Arch, Os};
use mix_nixgen::{FlakeConfig, HomeManagerConfig};

use mix_pins::{HOME_MANAGER_REV, NIXPKGS_REV};

const HOME_MANAGER_STATE_VERSION: &str = "24.05";

fn nix_system_double(arch: Arch, os: Os) -> &'static str {
    match (arch, os) {
        (Arch::X86_64, Os::Linux) => "x86_64-linux",
        (Arch::Aarch64, Os::Linux) => "aarch64-linux",
        (Arch::X86_64, Os::MacOs) => "x86_64-darwin",
        (Arch::Aarch64, Os::MacOs) => "aarch64-darwin",
    }
}

pub fn resolve_user_config() -> Option<UserConfig> {
    let user = invoking_user()?;
    let system = nix_system_double(Arch::current()?, Os::current()?);
    let flake = FlakeConfig::new(system, &user.name, NIXPKGS_REV, HOME_MANAGER_REV)
        .expect("system is a hardcoded literal and a real username cannot contain a null byte")
        .render();
    let mut home_cfg = HomeManagerConfig::new();
    home_cfg
        .set_str("home.username", &user.name)
        .expect("a real username cannot contain a null byte")
        .set_str("home.homeDirectory", &user.home.to_string_lossy())
        .expect("a real home directory cannot contain a null byte")
        .set_str("home.stateVersion", HOME_MANAGER_STATE_VERSION)
        .expect("state version is a hardcoded literal")
        .packages(["git"])
        .expect("\"git\" is a valid nix identifier");
    let home = home_cfg.render();
    Some(UserConfig { user, flake, home })
}

pub fn resolve_existing_user_config() -> Option<UserConfig> {
    let cfg = resolve_user_config()?;
    mix_core::identity::group_has_member(MIX_USERS_GROUP, &cfg.user.name).then_some(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nix_system_double_covers_all_four_combinations() {
        assert_eq!(nix_system_double(Arch::X86_64, Os::Linux), "x86_64-linux");
        assert_eq!(nix_system_double(Arch::Aarch64, Os::Linux), "aarch64-linux");
        assert_eq!(nix_system_double(Arch::X86_64, Os::MacOs), "x86_64-darwin");
        assert_eq!(
            nix_system_double(Arch::Aarch64, Os::MacOs),
            "aarch64-darwin"
        );
    }
}
