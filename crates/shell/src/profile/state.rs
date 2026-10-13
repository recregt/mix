use mix_core::declared::identity::InvokingUser;

pub use mix_core::ops::change::{Invalid, STATE_VERSION, Settled, Source, validate};

pub fn settle(
    user: &InvokingUser,
    host: &crate::request::context::Host,
    _locked: &crate::request::Locked,
) -> Settled {
    mix_core::ops::change::settle(
        host.state_file(user).as_deref(),
        host.active_list(user).as_deref(),
    )
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use mix_core::declared::state::StateManifest;
    use mix_core::model::testkit::required_with;

    use std::path::Path;

    use super::*;
    use mix_core::declared::paths::{
        GENERATION_STATE_FILE, HOME_MANAGER_PROFILE_NAME, STATE_FILE, mix_state_dir,
        nix_profiles_dir,
    };

    fn owner(home: &Path) -> InvokingUser {
        InvokingUser {
            uid: 1000,
            gid: 1000,
            name: "mix-user".into(),
            home: home.to_path_buf(),
        }
    }

    fn write_file(home: &Path, raw: &str) {
        std::fs::create_dir_all(mix_state_dir(home)).unwrap();
        std::fs::write(mix_state_dir(home).join(STATE_FILE), raw).unwrap();
    }

    fn write_generation(home: &Path, raw: &str) {
        let generation = home.join("store-generation");
        std::fs::create_dir_all(&generation).unwrap();
        std::fs::write(generation.join(GENERATION_STATE_FILE), raw).unwrap();
        std::fs::create_dir_all(nix_profiles_dir(home)).unwrap();
        let link = nix_profiles_dir(home).join(HOME_MANAGER_PROFILE_NAME);
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&generation, link).unwrap();
    }

    #[test]
    fn a_generation_outside_the_nix_store_is_never_read() {
        let home = tempfile::tempdir().unwrap();
        write_file(home.path(), &required_with(&["hello"]).render());
        write_generation(home.path(), &required_with(&[]).render());

        assert_eq!(
            settle(
                &owner(home.path()),
                &crate::request::context::Host::Machine,
                &crate::request::Locked::for_tests(),
            ),
            Settled::Current {
                manifest: required_with(&["hello"]),
                source: Source::File,
            }
        );
    }

    #[test]
    fn a_home_with_neither_file_starts_fresh() {
        let home = tempfile::tempdir().unwrap();

        assert_eq!(
            settle(
                &owner(home.path()),
                &crate::request::context::Host::Machine,
                &crate::request::Locked::for_tests(),
            ),
            Settled::Current {
                manifest: StateManifest::seed(),
                source: Source::Fresh,
            }
        );
    }
}
