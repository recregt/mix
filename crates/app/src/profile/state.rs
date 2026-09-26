use std::path::Path;

use mix_core::paths::{
    GENERATION_STATE_FILE, HOME_MANAGER_PROFILE_NAME, STATE_FILE, mix_state_dir, nix_profiles_dir,
};
use mix_core::state::{REQUIRED_PACKAGES, StateManifest};

pub const STATE_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Invalid {
    #[error("it is not a package list mix can read: {0}")]
    Unreadable(String),

    #[error("it was written by a newer mix (format {0})")]
    Newer(u32),

    #[error("it has an unknown format ({0})")]
    UnknownVersion(u32),

    #[error("`{0}` is not a package name")]
    Package(String),

    #[error("it does not list `{0}`, which mix needs")]
    Missing(&'static str),
}

pub fn validate(raw: &str) -> Result<StateManifest, Invalid> {
    let manifest =
        StateManifest::parse(raw).map_err(|error| Invalid::Unreadable(error.to_string()))?;
    if manifest.version > STATE_VERSION {
        return Err(Invalid::Newer(manifest.version));
    }
    if manifest.version != STATE_VERSION {
        return Err(Invalid::UnknownVersion(manifest.version));
    }
    if let Some(package) = manifest
        .packages
        .iter()
        .find(|package| !mix_nixgen::is_identifier(package))
    {
        return Err(Invalid::Package(package.clone()));
    }
    if let Some(required) = REQUIRED_PACKAGES
        .iter()
        .find(|required| !manifest.packages.iter().any(|package| package == *required))
    {
        return Err(Invalid::Missing(required));
    }
    Ok(manifest)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    File,
    Generation,
    Fresh,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Settled {
    Current {
        manifest: StateManifest,
        source: Source,
    },
    Newer(u32),
}

pub fn settle(home: &Path) -> Settled {
    let file = read(&mix_state_dir(home).join(STATE_FILE)).map(|raw| validate(&raw));
    if let Some(Err(Invalid::Newer(version))) = file {
        return Settled::Newer(version);
    }
    let generation = read(&active_generation_state(home)).and_then(|raw| validate(&raw).ok());

    let (manifest, source) = match (file, generation) {
        (Some(Ok(file)), Some(generation)) if file != generation => {
            (generation, Source::Generation)
        }
        (Some(Ok(file)), _) => (file, Source::File),
        (_, Some(generation)) => (generation, Source::Generation),
        (_, None) => (StateManifest::seed(), Source::Fresh),
    };
    Settled::Current { manifest, source }
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
mod tests {
    use super::*;

    fn manifest(packages: &[&str]) -> StateManifest {
        StateManifest {
            version: STATE_VERSION,
            packages: packages.iter().map(|p| p.to_string()).collect(),
        }
    }

    fn home() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
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

    fn current(manifest: StateManifest, source: Source) -> Settled {
        Settled::Current { manifest, source }
    }

    #[test]
    fn the_seed_is_valid() {
        assert_eq!(
            validate(&StateManifest::seed().render()),
            Ok(StateManifest::seed())
        );
    }

    #[test]
    fn a_list_mix_rendered_is_valid() {
        let list = manifest(&["git", "ripgrep", "node-sass"]);
        assert_eq!(validate(&list.render()), Ok(list));
    }

    #[test]
    fn broken_json_is_invalid() {
        assert!(matches!(validate("{broken"), Err(Invalid::Unreadable(_))));
        assert!(matches!(validate(""), Err(Invalid::Unreadable(_))));
    }

    #[test]
    fn a_newer_format_is_told_apart_from_an_unknown_one() {
        assert_eq!(
            validate(r#"{"version":2,"packages":["git"]}"#),
            Err(Invalid::Newer(2))
        );
        assert_eq!(
            validate(r#"{"version":0,"packages":["git"]}"#),
            Err(Invalid::UnknownVersion(0))
        );
    }

    #[test]
    fn a_name_nix_cannot_read_is_invalid() {
        assert_eq!(
            validate(r#"{"version":1,"packages":["git","rm -rf"]}"#),
            Err(Invalid::Package("rm -rf".to_string()))
        );
        assert_eq!(
            validate(r#"{"version":1,"packages":["git","with"]}"#),
            Err(Invalid::Package("with".to_string()))
        );
    }

    #[test]
    fn a_list_without_git_is_invalid() {
        assert_eq!(
            validate(r#"{"version":1,"packages":["ripgrep"]}"#),
            Err(Invalid::Missing("git"))
        );
    }

    #[test]
    fn a_valid_file_with_no_copy_in_the_profile_is_kept() {
        let home = home();
        write_file(home.path(), &manifest(&["git", "hello"]).render());

        assert_eq!(
            settle(home.path()),
            current(manifest(&["git", "hello"]), Source::File)
        );
    }

    #[test]
    fn a_valid_file_matching_the_profile_is_kept() {
        let home = home();
        let list = manifest(&["git", "hello"]).render();
        write_file(home.path(), &list);
        write_generation(home.path(), &list);

        assert_eq!(
            settle(home.path()),
            current(manifest(&["git", "hello"]), Source::File)
        );
    }

    #[test]
    fn a_file_the_profile_never_switched_to_gives_way_to_the_profile() {
        let home = home();
        write_file(home.path(), &manifest(&["git", "hello"]).render());
        write_generation(home.path(), &manifest(&["git"]).render());

        assert_eq!(
            settle(home.path()),
            current(manifest(&["git"]), Source::Generation)
        );
    }

    #[test]
    fn a_broken_file_is_restored_from_the_profile() {
        let home = home();
        write_file(home.path(), "{broken");
        write_generation(home.path(), &manifest(&["git", "hello"]).render());

        assert_eq!(
            settle(home.path()),
            current(manifest(&["git", "hello"]), Source::Generation)
        );
    }

    #[test]
    fn a_missing_file_is_restored_from_the_profile() {
        let home = home();
        write_generation(home.path(), &manifest(&["git", "hello"]).render());

        assert_eq!(
            settle(home.path()),
            current(manifest(&["git", "hello"]), Source::Generation)
        );
    }

    #[test]
    fn with_nothing_to_restore_from_a_fresh_list_is_started() {
        let home = home();
        write_file(home.path(), "{broken");

        assert_eq!(
            settle(home.path()),
            current(StateManifest::seed(), Source::Fresh)
        );
        assert_eq!(
            settle(tempfile::tempdir().unwrap().path()),
            current(StateManifest::seed(), Source::Fresh)
        );
    }

    #[test]
    fn a_broken_copy_in_the_profile_is_never_restored() {
        let home = home();
        write_file(home.path(), "{broken");
        write_generation(home.path(), "{also broken");

        assert_eq!(
            settle(home.path()),
            current(StateManifest::seed(), Source::Fresh)
        );
    }

    #[test]
    fn a_valid_file_is_kept_over_a_broken_copy_in_the_profile() {
        let home = home();
        write_file(home.path(), &manifest(&["git", "hello"]).render());
        write_generation(home.path(), "{broken");

        assert_eq!(
            settle(home.path()),
            current(manifest(&["git", "hello"]), Source::File)
        );
    }

    #[test]
    fn a_list_from_a_newer_mix_is_left_alone() {
        let home = home();
        write_file(home.path(), r#"{"version":2,"packages":["git"]}"#);
        write_generation(home.path(), &manifest(&["git"]).render());

        assert_eq!(settle(home.path()), Settled::Newer(2));
    }

    #[test]
    fn a_same_list_written_differently_still_matches_the_profile() {
        let home = home();
        write_file(home.path(), r#"{"version":1,"packages":["git","hello"]}"#);
        write_generation(home.path(), &manifest(&["git", "hello"]).render());

        assert_eq!(
            settle(home.path()),
            current(manifest(&["git", "hello"]), Source::File)
        );
    }

    proptest::proptest! {
        #[test]
        fn anything_validate_accepts_renders_to_the_same_list(raw in ".*") {
            if let Ok(parsed) = validate(&raw) {
                proptest::prop_assert_eq!(validate(&parsed.render()), Ok(parsed));
            }
        }

        #[test]
        fn a_valid_list_survives_any_single_byte_change_as_valid_or_rejected(
            packages in proptest::collection::vec("[a-z][a-z0-9-]{0,12}", 0..6),
            position in proptest::prelude::any::<proptest::sample::Index>(),
            byte in proptest::prelude::any::<u8>(),
        ) {
            let mut list = vec!["git".to_string()];
            list.extend(packages);
            let rendered = StateManifest { version: STATE_VERSION, packages: list }.render();
            let mut bytes = rendered.into_bytes();
            let at = position.index(bytes.len());
            bytes[at] = byte;
            if let Ok(raw) = String::from_utf8(bytes)
                && let Ok(parsed) = validate(&raw)
            {
                proptest::prop_assert!(parsed.packages.iter().all(|p| mix_nixgen::is_identifier(p)));
                proptest::prop_assert!(parsed.packages.iter().any(|p| p == "git"));
            }
        }
    }
}
