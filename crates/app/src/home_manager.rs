use mix_core::models::UserConfig;
use mix_core::privilege::invoking_user;
use mix_core::system::{Arch, Os};
use mix_nixgen::{FlakeConfig, HomeManagerConfig};

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
    let flake = FlakeConfig::new(system, &user.name)
        .expect("system is a hardcoded literal and a real username cannot contain a null byte")
        .render();
    let home = HomeManagerConfig::new().render();
    Some(UserConfig { user, flake, home })
}

pub async fn resolve_existing_user_config() -> Option<UserConfig> {
    let cfg = resolve_user_config()?;
    let marker = mix_core::paths::mix_user_marker(cfg.user.uid);
    crate::os::path_exists(&marker).await.then_some(cfg)
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
