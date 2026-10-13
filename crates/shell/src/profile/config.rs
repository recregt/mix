use mix_core::declared::identity::InvokingUser;
use mix_core::declared::identity::MIX_USERS_GROUP;
use mix_core::declared::pins;
use mix_core::declared::state::StateManifest;
use mix_core::declared::targets::UserConfig;
use mix_core::effect::Failure;
use mix_core::ops::bootstrap::system::{Arch, Os};
use mix_core::ops::change::render_home;
use mix_nixgen::lock::{self, LockedInput};
use mix_nixgen::{FlakeConfig, Pins, System};

use crate::profile::state::{Settled, Source};
use crate::request::context::Host;

fn nix_system(arch: Arch, os: Os) -> System {
    match (arch, os) {
        (Arch::X86_64, Os::Linux) => System::X86_64Linux,
        (Arch::Aarch64, Os::Linux) => System::Aarch64Linux,
        (Arch::X86_64, Os::MacOs) => System::X86_64Darwin,
        (Arch::Aarch64, Os::MacOs) => System::Aarch64Darwin,
    }
}

fn system() -> Option<System> {
    Some(nix_system(Arch::current()?, Os::current()?))
}

fn rendered(system: System, username: &str, pins: &Pins<LockedInput>) -> (String, String) {
    let flake = FlakeConfig::new(system, username, &pins.nixpkgs.rev, &pins.home_manager.rev)
        .expect("a real username cannot contain a null byte")
        .render();
    (flake, lock::render(pins.clone()))
}

pub fn repinned(cfg: &UserConfig, pins: Pins<LockedInput>) -> UserConfig {
    let system = system().expect("a user config is only made on a system nix runs on");
    let (flake, lock) = rendered(system, &cfg.user.name, &pins);
    UserConfig {
        pins,
        flake,
        lock,
        ..cfg.clone()
    }
}

struct Found<'a> {
    state: Option<&'a str>,
    active: Option<&'a str>,
    home_nix: Option<String>,
    lock: Option<&'a str>,
    active_lock: Option<&'a str>,
}

fn configured(user: InvokingUser, found: Found<'_>, mirrored: bool) -> Option<UserConfig> {
    let pins = pins::settle(found.lock, found.active_lock, mirrored);
    let (flake, lock) = rendered(system()?, &user.name, &pins);
    let (home, restored_state) = match mix_core::ops::change::settle(found.state, found.active) {
        Settled::Current { manifest, source } => (
            render_home(&user, &manifest.packages)
                .expect("a settled package list only holds valid names"),
            (source != Source::File).then(|| manifest.render()),
        ),
        Settled::Newer(_) => (
            found.home_nix.unwrap_or_else(|| {
                render_home(&user, StateManifest::seed().packages)
                    .expect("the seed package list is always valid")
            }),
            None,
        ),
    };
    Some(UserConfig {
        user,
        pins,
        flake,
        lock,
        home,
        restored_state,
    })
}

pub async fn user_config_for(
    user: InvokingUser,
    host: &Host,
    mirrored: bool,
    _locked: &crate::request::Locked,
    scope: &mix_exec::Scope,
) -> Result<Option<UserConfig>, Failure> {
    let files = host.home_files(&user, scope).await?;
    let found = Found {
        state: files.state.as_deref(),
        active: files.active.as_deref(),
        home_nix: files.home_nix,
        lock: files.lock.as_deref(),
        active_lock: files.active_lock.as_deref(),
    };
    Ok(configured(user, found, mirrored))
}

pub async fn observed_user_config(
    user: InvokingUser,
    performer: &mut crate::drive::Performer,
    scope: &mix_exec::Scope,
    mirrored: bool,
) -> Option<UserConfig> {
    use mix_core::declared::paths::{FLAKE_LOCK, HOME_NIX, STATE_FILE, mix_state_dir};
    use mix_core::effect::{Fact, Query};
    let state_dir = mix_state_dir(&user.home);
    let facts = performer
        .observe(
            &[
                Query::Contents(state_dir.join(STATE_FILE)),
                Query::ActiveList(user.clone()),
                Query::Contents(state_dir.join(HOME_NIX)),
                Query::Contents(state_dir.join(FLAKE_LOCK)),
                Query::ActiveLock(user.clone()),
            ],
            scope,
        )
        .await
        .ok()?;
    let text = |fact: &Fact| match fact {
        Fact::Contents(Some(bytes)) => Some(String::from_utf8_lossy(bytes).into_owned()),
        _ => None,
    };
    let (state, active, lock, active_lock) = (
        text(&facts[0]),
        text(&facts[1]),
        text(&facts[3]),
        text(&facts[4]),
    );
    let found = Found {
        state: state.as_deref(),
        active: active.as_deref(),
        home_nix: text(&facts[2]),
        lock: lock.as_deref(),
        active_lock: active_lock.as_deref(),
    };
    configured(user, found, mirrored)
}

pub async fn existing_user_config_for(
    user: InvokingUser,
    host: &Host,
    mirrored: bool,
    locked: &crate::request::Locked,
    scope: &mix_exec::Scope,
) -> Result<Option<UserConfig>, Failure> {
    if !host.is_member(MIX_USERS_GROUP, &user.name) {
        return Ok(None);
    }
    user_config_for(user, host, mirrored, locked, scope).await
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

    #[tokio::test]
    async fn only_a_member_of_mix_users_is_managed() {
        let user = sample_user(Path::new("/home/mix-user"));
        let locked = crate::request::Locked::for_tests();
        let scope = mix_exec::Scope::root();
        let mut enrolled = mix_core::model::World::default();
        enrolled.seed_group(
            MIX_USERS_GROUP,
            Some(mix_core::effect::GroupFacts {
                gid: 30000,
                members: vec![user.name.clone()],
            }),
        );
        let host = |world| Host::Model(std::sync::Arc::new(std::sync::Mutex::new(world)));

        let member =
            existing_user_config_for(user.clone(), &host(enrolled), false, &locked, &scope)
                .await
                .unwrap();
        let outsider = existing_user_config_for(
            user,
            &host(mix_core::model::World::default()),
            false,
            &locked,
            &scope,
        )
        .await
        .unwrap();

        assert!(member.is_some());
        assert!(outsider.is_none());
    }

    #[test]
    fn nix_system_covers_all_four_combinations() {
        assert_eq!(nix_system(Arch::X86_64, Os::Linux), System::X86_64Linux);
        assert_eq!(nix_system(Arch::Aarch64, Os::Linux), System::Aarch64Linux);
        assert_eq!(nix_system(Arch::X86_64, Os::MacOs), System::X86_64Darwin);
        assert_eq!(nix_system(Arch::Aarch64, Os::MacOs), System::Aarch64Darwin);
    }
}
