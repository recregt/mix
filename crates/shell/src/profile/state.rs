use std::path::Path;

use mix_core::paths::{
    GENERATION_INPUTS, GENERATION_STATE_FILE, HOME_MANAGER_PROFILE_NAME, STATE_FILE, mix_state_dir,
    nix_profiles_dir,
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

pub fn built_generation(home: &Path, generations: &[u64]) -> Option<u64> {
    let state = mix_state_dir(home);
    let inputs: Vec<Vec<u8>> = GENERATION_INPUTS
        .iter()
        .map(|(file, _)| std::fs::read(state.join(file)).ok())
        .collect::<Option<_>>()?;
    let mut generations = generations.to_vec();
    generations.sort_unstable_by(|a, b| b.cmp(a));
    generations.into_iter().find(|generation| {
        let dir =
            nix_profiles_dir(home).join(format!("{HOME_MANAGER_PROFILE_NAME}-{generation}-link"));
        GENERATION_INPUTS
            .iter()
            .zip(&inputs)
            .all(|((_, copy), wanted)| std::fs::read(dir.join(copy)).ok().as_ref() == Some(wanted))
    })
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

    fn write_inputs(dir: &Path, names: impl Fn(usize) -> &'static str, packages: &str) {
        std::fs::create_dir_all(dir).unwrap();
        for (index, _) in GENERATION_INPUTS.iter().enumerate() {
            std::fs::write(dir.join(names(index)), format!("{index} {packages}")).unwrap();
        }
    }

    fn write_built(home: &Path, generation: u64, packages: &str) {
        let out = home.join(format!("store-{generation}"));
        write_inputs(&out, |index| GENERATION_INPUTS[index].1, packages);
        std::fs::create_dir_all(nix_profiles_dir(home)).unwrap();
        std::os::unix::fs::symlink(
            &out,
            nix_profiles_dir(home).join(format!("{HOME_MANAGER_PROFILE_NAME}-{generation}-link")),
        )
        .unwrap();
    }

    #[test]
    fn the_newest_generation_built_from_the_same_files_is_found() {
        let home = tempfile::tempdir().unwrap();
        write_inputs(
            &mix_state_dir(home.path()),
            |index| GENERATION_INPUTS[index].0,
            "git",
        );
        write_built(home.path(), 1, "git");
        write_built(home.path(), 2, "git hello");
        write_built(home.path(), 3, "git");
        write_built(home.path(), 4, "git hello");

        assert_eq!(built_generation(home.path(), &[1, 2, 3, 4]), Some(3));
        assert_eq!(built_generation(home.path(), &[1, 2, 4]), Some(1));
        assert_eq!(built_generation(home.path(), &[2, 4]), None);
    }

    #[test]
    fn a_generation_without_copied_inputs_is_never_reused() {
        let home = tempfile::tempdir().unwrap();
        write_inputs(
            &mix_state_dir(home.path()),
            |index| GENERATION_INPUTS[index].0,
            "git",
        );
        write_built(home.path(), 1, "git");
        let out = home.path().join("store-1");
        std::fs::remove_file(out.join(GENERATION_INPUTS[3].1)).unwrap();

        assert_eq!(built_generation(home.path(), &[1]), None);
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
