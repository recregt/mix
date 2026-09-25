use mix_core::identity::MIX_USERS_GROUP;
use mix_core::models::UserConfig;
use mix_core::paths::{GENERATION_STATE_FILE, HOME_NIX, STATE_FILE, mix_state_dir};
use mix_core::privilege::{InvokingUser, invoking_user};
use mix_core::state::StateManifest;
use mix_core::system::{Arch, Os};
use mix_nixgen::{FlakeConfig, HomeManagerConfig, InvalidInput};

use mix_pins::{HOME_MANAGER_REV, NIXPKGS_REV};

use crate::profile::state::{Settled, Source, settle};

const HOME_MANAGER_STATE_VERSION: &str = "24.05";

fn nix_system_double(arch: Arch, os: Os) -> &'static str {
    match (arch, os) {
        (Arch::X86_64, Os::Linux) => "x86_64-linux",
        (Arch::Aarch64, Os::Linux) => "aarch64-linux",
        (Arch::X86_64, Os::MacOs) => "x86_64-darwin",
        (Arch::Aarch64, Os::MacOs) => "aarch64-darwin",
    }
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
        .copy_into_generation(STATE_FILE, GENERATION_STATE_FILE)
        .expect("both file names are hardcoded literals")
        .packages(packages)?;
    Ok(home_cfg.render())
}

pub fn resolve_user_config() -> Option<UserConfig> {
    let user = invoking_user()?;
    let system = nix_system_double(Arch::current()?, Os::current()?);
    let flake = FlakeConfig::new(system, &user.name, NIXPKGS_REV, HOME_MANAGER_REV)
        .expect("system is a hardcoded literal and a real username cannot contain a null byte")
        .render();
    let (home, restored_state) = match settle(&user.home) {
        Settled::Current { manifest, source } => (
            render_home(&user, &manifest.packages)
                .expect("a settled package list only holds valid names"),
            (source != Source::File).then(|| manifest.render()),
        ),
        Settled::Newer(_) => (
            std::fs::read_to_string(mix_state_dir(&user.home).join(HOME_NIX)).unwrap_or_else(
                |_| {
                    render_home(&user, StateManifest::seed().packages)
                        .expect("the seed package list is always valid")
                },
            ),
            None,
        ),
    };
    Some(UserConfig {
        user,
        flake,
        home,
        restored_state,
    })
}

pub fn resolve_existing_user_config() -> Option<UserConfig> {
    let cfg = resolve_user_config()?;
    mix_core::identity::group_has_member(MIX_USERS_GROUP, &cfg.user.name).then_some(cfg)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

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
    fn render_home_copies_the_package_list_into_the_generation() {
        let home = tempfile::tempdir().unwrap();
        let rendered = render_home(&sample_user(home.path()), ["git"]).unwrap();
        assert!(rendered.contains(r#"extraBuilderCommands = "cp ${./state} $out/mix-state";"#));
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
