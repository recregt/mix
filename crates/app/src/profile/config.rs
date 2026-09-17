use std::path::Path;

use mix_core::identity::MIX_USERS_GROUP;
use mix_core::models::UserConfig;
use mix_core::paths::{STATE_FILE, mix_state_dir};
use mix_core::privilege::{InvokingUser, invoking_user};
use mix_core::state::StateManifest;
use mix_core::system::{Arch, Os};
use mix_nixgen::{FlakeConfig, HomeManagerConfig, InvalidInput};

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

pub(crate) fn read_state(home: &Path) -> StateManifest {
    std::fs::read_to_string(mix_state_dir(home).join(STATE_FILE))
        .ok()
        .and_then(|raw| StateManifest::parse(&raw).ok())
        .unwrap_or_else(StateManifest::seed)
}

pub(crate) fn render_home<S: AsRef<str>>(
    user: &InvokingUser,
    packages: impl IntoIterator<Item = S>,
) -> Result<String, InvalidInput> {
    let mut home_cfg = HomeManagerConfig::new();
    home_cfg
        .set_str("home.username", &user.name)
        .expect("a real username cannot contain a null byte")
        .set_str("home.homeDirectory", &user.home.to_string_lossy())
        .expect("a real home directory cannot contain a null byte")
        .set_str("home.stateVersion", HOME_MANAGER_STATE_VERSION)
        .expect("state version is a hardcoded literal")
        .packages(packages)?;
    Ok(home_cfg.render())
}

pub fn resolve_user_config() -> Option<UserConfig> {
    let user = invoking_user()?;
    let system = nix_system_double(Arch::current()?, Os::current()?);
    let flake = FlakeConfig::new(system, &user.name, NIXPKGS_REV, HOME_MANAGER_REV)
        .expect("system is a hardcoded literal and a real username cannot contain a null byte")
        .render();
    let state = read_state(&user.home);
    let home = render_home(&user, &state.packages)
        .or_else(|_| render_home(&user, StateManifest::seed().packages))
        .expect("the seed package list is always valid");
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
    fn read_state_returns_the_seed_when_the_file_is_missing() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(read_state(home.path()), StateManifest::seed());
    }

    #[test]
    fn read_state_returns_the_seed_when_the_file_is_malformed() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(mix_state_dir(home.path())).unwrap();
        std::fs::write(mix_state_dir(home.path()).join(STATE_FILE), "not json").unwrap();
        assert_eq!(read_state(home.path()), StateManifest::seed());
    }

    #[test]
    fn read_state_returns_the_parsed_manifest_when_present() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(mix_state_dir(home.path())).unwrap();
        let manifest = StateManifest {
            version: 1,
            packages: vec!["git".to_string(), "ripgrep".to_string()],
        };
        std::fs::write(
            mix_state_dir(home.path()).join(STATE_FILE),
            manifest.render(),
        )
        .unwrap();
        assert_eq!(read_state(home.path()), manifest);
    }

    fn sample_user(home: &Path) -> InvokingUser {
        InvokingUser {
            uid: 1000,
            gid: 1000,
            name: "mix-user".to_string(),
            home: home.to_path_buf(),
        }
    }

    #[test]
    fn render_home_includes_every_requested_package() {
        let home = tempfile::tempdir().unwrap();
        let rendered = render_home(&sample_user(home.path()), ["git", "ripgrep"]).unwrap();
        assert!(rendered.contains("git"));
        assert!(rendered.contains("ripgrep"));
    }

    #[test]
    fn render_home_rejects_an_invalid_package_name() {
        let home = tempfile::tempdir().unwrap();
        assert!(render_home(&sample_user(home.path()), ["not a valid ident"]).is_err());
    }

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
