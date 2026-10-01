use std::path::Path;

use mix_core::paths::{
    GENERATION_STATE_FILE, HOME_MANAGER_PROFILE_NAME, STATE_FILE, mix_state_dir, nix_profiles_dir,
};

pub use mix_core::change::{Invalid, STATE_VERSION, Settled, Source, validate};

pub fn settle(home: &Path) -> Settled {
    mix_core::change::settle(
        read(&mix_state_dir(home).join(STATE_FILE)).as_deref(),
        read(&active_generation_state(home)).as_deref(),
    )
}

pub fn active_generation_state(home: &Path) -> std::path::PathBuf {
    nix_profiles_dir(home)
        .join(HOME_MANAGER_PROFILE_NAME)
        .join(GENERATION_STATE_FILE)
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use mix_core::state::StateManifest;

    use super::*;

    fn manifest(packages: &[&str]) -> StateManifest {
        StateManifest {
            version: STATE_VERSION,
            packages: packages.iter().map(|p| p.to_string()).collect(),
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
    fn the_list_is_read_from_the_state_file_and_the_active_generation() {
        let home = tempfile::tempdir().unwrap();
        write_file(home.path(), &manifest(&["git", "hello"]).render());
        write_generation(home.path(), &manifest(&["git"]).render());

        assert_eq!(
            settle(home.path()),
            Settled::Current {
                manifest: manifest(&["git"]),
                source: Source::Generation,
            }
        );
    }

    #[test]
    fn a_home_with_neither_file_starts_fresh() {
        let home = tempfile::tempdir().unwrap();

        assert_eq!(
            settle(home.path()),
            Settled::Current {
                manifest: StateManifest::seed(),
                source: Source::Fresh,
            }
        );
    }
}
