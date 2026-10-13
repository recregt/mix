use std::path::{Path, PathBuf};

use mix_core::declared::paths::{
    GENERATION_INPUTS, HOME_MANAGER_PROFILE_NAME, NIX_STORE, mix_state_dir, nix_profiles_dir,
};

pub fn built_generation(home: &Path, generations: &[u64]) -> Option<u64> {
    built_generation_in(home, generations, Path::new(NIX_STORE))
}

fn built_generation_in(home: &Path, generations: &[u64], store: &Path) -> Option<u64> {
    let state = mix_state_dir(home);
    let inputs: Vec<Vec<u8>> = GENERATION_INPUTS
        .iter()
        .map(|(file, _)| std::fs::read(state.join(file)).ok())
        .collect::<Option<_>>()?;
    let mut generations = generations.to_vec();
    generations.sort_unstable_by(|a, b| b.cmp(a));
    generations.into_iter().find(|generation| {
        let link =
            nix_profiles_dir(home).join(format!("{HOME_MANAGER_PROFILE_NAME}-{generation}-link"));
        let Some(dir) = store_generation(&link, store) else {
            return false;
        };
        GENERATION_INPUTS
            .iter()
            .zip(&inputs)
            .all(|((_, copy), wanted)| store_input(&dir, copy).as_ref() == Some(wanted))
    })
}

pub fn store_generation(link: &Path, store: &Path) -> Option<PathBuf> {
    let generation = std::fs::canonicalize(link).ok()?;
    (generation.parent() == Some(store)).then_some(generation)
}

pub fn store_input(generation: &Path, file: &str) -> Option<Vec<u8>> {
    use std::io::Read as _;
    use std::os::fd::OwnedFd;
    let nofollow =
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC;
    let dir = rustix::fs::open(
        generation,
        nofollow | rustix::fs::OFlags::DIRECTORY,
        rustix::fs::Mode::empty(),
    )
    .ok()?;
    let found: OwnedFd =
        rustix::fs::openat(&dir, file, nofollow, rustix::fs::Mode::empty()).ok()?;
    if !rustix::fs::FileType::from_raw_mode(rustix::fs::fstat(&found).ok()?.st_mode).is_file() {
        return None;
    }
    let mut contents = Vec::new();
    std::fs::File::from(found).read_to_end(&mut contents).ok()?;
    Some(contents)
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;

    fn write_inputs(dir: &Path, names: impl Fn(usize) -> &'static str, packages: &str) {
        std::fs::create_dir_all(dir).unwrap();
        for (index, _) in GENERATION_INPUTS.iter().enumerate() {
            std::fs::write(dir.join(names(index)), format!("{index} {packages}")).unwrap();
        }
    }

    fn built_in(home: &Path, generations: &[u64], store: &Path) -> Option<u64> {
        built_generation_in(home, generations, store)
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

        let store = std::fs::canonicalize(home.path()).unwrap();
        assert_eq!(built_in(home.path(), &[1, 2, 3, 4], &store), Some(3));
        assert_eq!(built_in(home.path(), &[1, 2, 4], &store), Some(1));
        assert_eq!(built_in(home.path(), &[2, 4], &store), None);
        assert_eq!(built_generation(home.path(), &[1, 2, 3, 4]), None);
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

        let store = std::fs::canonicalize(home.path()).unwrap();
        assert_eq!(built_in(home.path(), &[1], &store), None);
    }
}
