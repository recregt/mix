use mix_core::declared::identity::InvokingUser;
use mix_core::declared::identity::MIX_USERS_GROUP;
use mix_core::declared::pins;
use mix_core::declared::state::StateManifest;
use mix_core::declared::targets::UserConfig;
use mix_core::ops::bootstrap::system::{Arch, Os};
use mix_core::ops::change::render_home;
use mix_nixgen::lock::{self, LockedInput, NarHash};
use mix_nixgen::{FlakeConfig, Pins, Rev, System};
use mix_pins::{
    HOME_MANAGER_LAST_MODIFIED, HOME_MANAGER_NAR_HASH, HOME_MANAGER_REV, NIXPKGS_LAST_MODIFIED,
    NIXPKGS_NAR_HASH, NIXPKGS_REV,
};

use crate::profile::state::{Settled, Source};
use crate::request::context::Host;
use crate::request::pinned::Pinned;

const NIXPKGS: Rev = Rev::new_static(NIXPKGS_REV);
const HOME_MANAGER: Rev = Rev::new_static(HOME_MANAGER_REV);

const NIXPKGS_LOCK: LockedInput = LockedInput {
    rev: NIXPKGS,
    nar_hash: NarHash::new_static(NIXPKGS_NAR_HASH),
    last_modified: NIXPKGS_LAST_MODIFIED,
};
const HOME_MANAGER_LOCK: LockedInput = LockedInput {
    rev: HOME_MANAGER,
    nar_hash: NarHash::new_static(HOME_MANAGER_NAR_HASH),
    last_modified: HOME_MANAGER_LAST_MODIFIED,
};

pub fn embedded() -> Pins<LockedInput> {
    Pins {
        home_manager: HOME_MANAGER_LOCK,
        nixpkgs: NIXPKGS_LOCK,
    }
}

pub fn current_pins(user: &InvokingUser, host: &Host, pinned: &Pinned) -> Pins<LockedInput> {
    pinned.get_or_load(user.uid, || {
        host.flake_nix(user)
            .zip(host.flake_lock(user))
            .and_then(|(flake, lock)| pins::read(&flake, &lock))
            .unwrap_or_else(embedded)
    })
}

fn rendered(system: System, username: &str, pins: &Pins<LockedInput>) -> (String, String) {
    let flake = FlakeConfig::new(system, username, &pins.nixpkgs.rev, &pins.home_manager.rev)
        .expect("a real username cannot contain a null byte")
        .render();
    (flake, lock::render(pins.clone()))
}

pub fn repinned(cfg: &UserConfig, pins: &Pins<LockedInput>) -> Option<UserConfig> {
    let system = nix_system(Arch::current()?, Os::current()?);
    let (flake, lock) = rendered(system, &cfg.user.name, pins);
    Some(UserConfig {
        flake,
        lock,
        ..cfg.clone()
    })
}

fn nix_system(arch: Arch, os: Os) -> System {
    match (arch, os) {
        (Arch::X86_64, Os::Linux) => System::X86_64Linux,
        (Arch::Aarch64, Os::Linux) => System::Aarch64Linux,
        (Arch::X86_64, Os::MacOs) => System::X86_64Darwin,
        (Arch::Aarch64, Os::MacOs) => System::Aarch64Darwin,
    }
}

fn configured(
    user: InvokingUser,
    state: Option<&str>,
    active: Option<&str>,
    home_nix: Option<String>,
    pins: &Pins<LockedInput>,
) -> Option<UserConfig> {
    let system = nix_system(Arch::current()?, Os::current()?);
    let (flake, lock) = rendered(system, &user.name, pins);
    let (home, restored_state) = match mix_core::ops::change::settle(state, active) {
        Settled::Current { manifest, source } => (
            render_home(&user, &manifest.packages)
                .expect("a settled package list only holds valid names"),
            (source != Source::File).then(|| manifest.render()),
        ),
        Settled::Newer(_) => (
            home_nix.unwrap_or_else(|| {
                render_home(&user, StateManifest::seed().packages)
                    .expect("the seed package list is always valid")
            }),
            None,
        ),
    };
    Some(UserConfig {
        user,
        flake,
        lock,
        home,
        restored_state,
    })
}

pub fn user_config_for(
    user: InvokingUser,
    host: &Host,
    pinned: &Pinned,
    _locked: &crate::request::Locked,
) -> Option<UserConfig> {
    let state = host.state_file(&user);
    let active = host.active_list(&user);
    let home_nix = host.home_nix(&user);
    let pins = current_pins(&user, host, pinned);
    configured(user, state.as_deref(), active.as_deref(), home_nix, &pins)
}

pub async fn observed_user_config(
    user: InvokingUser,
    performer: &mut crate::drive::Performer,
    scope: &mix_exec::Scope,
    pins: &Pins<LockedInput>,
) -> Option<UserConfig> {
    use mix_core::declared::paths::{HOME_NIX, STATE_FILE, mix_state_dir};
    use mix_core::effect::{Fact, Query};
    let state_dir = mix_state_dir(&user.home);
    let facts = performer
        .observe(
            &[
                Query::Contents(state_dir.join(STATE_FILE)),
                Query::ActiveList(user.clone()),
                Query::Contents(state_dir.join(HOME_NIX)),
            ],
            scope,
        )
        .await
        .ok()?;
    let text = |fact: &Fact| match fact {
        Fact::Contents(Some(bytes)) => Some(String::from_utf8_lossy(bytes).into_owned()),
        _ => None,
    };
    configured(
        user,
        text(&facts[0]).as_deref(),
        text(&facts[1]).as_deref(),
        text(&facts[2]),
        pins,
    )
}

pub fn existing_user_config_for(
    user: InvokingUser,
    host: &Host,
    pinned: &Pinned,
    locked: &crate::request::Locked,
) -> Option<UserConfig> {
    managed(
        user_config_for(user, host, pinned, locked)?,
        |group, user| host.is_member(group, user),
    )
}

fn managed(cfg: UserConfig, member: impl Fn(&str, &str) -> bool) -> Option<UserConfig> {
    member(MIX_USERS_GROUP, &cfg.user.name).then_some(cfg)
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
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

    fn config(home: &Path) -> UserConfig {
        UserConfig {
            user: sample_user(home),
            flake: String::new(),
            lock: String::new(),
            home: String::new(),
            restored_state: None,
        }
    }

    #[test]
    fn only_a_member_of_mix_users_is_managed() {
        let home = Path::new("/home/mix-user");
        let enrolled = |group: &str, name: &str| group == MIX_USERS_GROUP && name == "mix-user";

        assert!(managed(config(home), enrolled).is_some());
        assert!(managed(config(home), |_: &str, _: &str| false).is_none());
    }

    #[test]
    fn nix_system_covers_all_four_combinations() {
        assert_eq!(nix_system(Arch::X86_64, Os::Linux), System::X86_64Linux);
        assert_eq!(nix_system(Arch::Aarch64, Os::Linux), System::Aarch64Linux);
        assert_eq!(nix_system(Arch::X86_64, Os::MacOs), System::X86_64Darwin);
        assert_eq!(nix_system(Arch::Aarch64, Os::MacOs), System::Aarch64Darwin);
    }
}
