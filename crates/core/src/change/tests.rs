use std::path::PathBuf;

use super::*;

fn manifest(packages: &[&str]) -> StateManifest {
    StateManifest {
        version: STATE_VERSION,
        packages: packages.iter().map(|p| p.to_string()).collect(),
    }
}

fn current(manifest: StateManifest, source: Source) -> Settled {
    Settled::Current { manifest, source }
}

fn names(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| name.to_string()).collect()
}

fn user() -> InvokingUser {
    InvokingUser {
        uid: 1000,
        gid: 1000,
        name: "mix-user".to_string(),
        home: PathBuf::from("/home/mix-user"),
    }
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
    let file = manifest(&["git", "hello"]).render();

    assert_eq!(
        settle(Some(&file), None),
        current(manifest(&["git", "hello"]), Source::File)
    );
}

#[test]
fn a_valid_file_matching_the_profile_is_kept() {
    let list = manifest(&["git", "hello"]).render();

    assert_eq!(
        settle(Some(&list), Some(&list)),
        current(manifest(&["git", "hello"]), Source::File)
    );
}

#[test]
fn a_file_the_profile_never_switched_to_gives_way_to_the_profile() {
    let file = manifest(&["git", "hello"]).render();
    let generation = manifest(&["git"]).render();

    assert_eq!(
        settle(Some(&file), Some(&generation)),
        current(manifest(&["git"]), Source::Generation)
    );
}

#[test]
fn a_broken_file_is_restored_from_the_profile() {
    let generation = manifest(&["git", "hello"]).render();

    assert_eq!(
        settle(Some("{broken"), Some(&generation)),
        current(manifest(&["git", "hello"]), Source::Generation)
    );
}

#[test]
fn a_missing_file_is_restored_from_the_profile() {
    let generation = manifest(&["git", "hello"]).render();

    assert_eq!(
        settle(None, Some(&generation)),
        current(manifest(&["git", "hello"]), Source::Generation)
    );
}

#[test]
fn with_nothing_to_restore_from_a_fresh_list_is_started() {
    assert_eq!(
        settle(Some("{broken"), None),
        current(StateManifest::seed(), Source::Fresh)
    );
    assert_eq!(
        settle(None, None),
        current(StateManifest::seed(), Source::Fresh)
    );
}

#[test]
fn a_broken_copy_in_the_profile_is_never_restored() {
    assert_eq!(
        settle(Some("{broken"), Some("{also broken")),
        current(StateManifest::seed(), Source::Fresh)
    );
}

#[test]
fn a_valid_file_is_kept_over_a_broken_copy_in_the_profile() {
    let file = manifest(&["git", "hello"]).render();

    assert_eq!(
        settle(Some(&file), Some("{broken")),
        current(manifest(&["git", "hello"]), Source::File)
    );
}

#[test]
fn a_list_from_a_newer_mix_is_left_alone() {
    let generation = manifest(&["git"]).render();

    assert_eq!(
        settle(
            Some(r#"{"version":2,"packages":["git"]}"#),
            Some(&generation)
        ),
        Settled::Newer(2)
    );
}

#[test]
fn a_same_list_written_differently_still_matches_the_profile() {
    let generation = manifest(&["git", "hello"]).render();

    assert_eq!(
        settle(
            Some(r#"{"version":1,"packages":["git","hello"]}"#),
            Some(&generation)
        ),
        current(manifest(&["git", "hello"]), Source::File)
    );
}

#[test]
fn an_install_appends_what_is_missing_and_skips_what_is_there() {
    let request = names(&["git", "ripgrep"]);

    let change = install(&request, current(manifest(&["git"]), Source::File)).unwrap();

    assert_eq!(change.changed, names(&["ripgrep"]));
    assert_eq!(change.skipped, names(&["git"]));
    assert_eq!(change.manifest, manifest(&["git", "ripgrep"]));
    assert_eq!(change.source, Source::File);
}

#[test]
fn a_repeated_package_is_counted_once() {
    let request = names(&["git", "git", "fd", "fd"]);

    let change = install(&request, current(manifest(&["git"]), Source::File)).unwrap();

    assert_eq!(change.changed, names(&["fd"]));
    assert_eq!(change.skipped, names(&["git"]));
    assert_eq!(change.manifest, manifest(&["git", "fd"]));
}

#[test]
fn an_install_keeps_the_list_version() {
    let request = names(&["fd"]);
    let settled = StateManifest {
        version: 7,
        packages: names(&["git"]),
    };

    let change = install(&request, current(settled, Source::File)).unwrap();

    assert_eq!(change.manifest.version, 7);
}

#[test]
fn a_remove_drops_what_is_there_and_skips_what_is_not() {
    let request = names(&["ripgrep", "fd"]);

    let change = remove(
        &request,
        current(manifest(&["git", "ripgrep"]), Source::Generation),
    )
    .unwrap();

    assert_eq!(change.changed, names(&["ripgrep"]));
    assert_eq!(change.skipped, names(&["fd"]));
    assert_eq!(change.manifest, manifest(&["git"]));
    assert_eq!(change.source, Source::Generation);
}

#[test]
fn a_protected_package_is_refused_before_the_list_is_read() {
    let request = names(&["git", "ripgrep"]);

    assert_eq!(
        remove(&request, Settled::Newer(9)),
        Err(Refusal::Protected(names(&["git"])))
    );
}

#[test]
fn a_list_from_a_newer_mix_is_never_changed() {
    let request = names(&["ripgrep"]);

    assert_eq!(install(&request, Settled::Newer(2)), Err(NewerList(2)));
}

#[test]
fn nothing_changes_when_every_package_is_already_there() {
    let request = names(&["git"]);

    let change = install(&request, current(manifest(&["git"]), Source::File)).unwrap();

    assert!(change.changed.is_empty());
    assert_eq!(change.manifest, manifest(&["git"]));
}

#[test]
fn rendering_writes_every_package_into_the_list_and_home_nix() {
    let rendered = render(&user(), &manifest(&["git", "ripgrep"])).unwrap();

    assert_eq!(
        StateManifest::parse(&rendered.state).unwrap().packages,
        names(&["git", "ripgrep"])
    );
    assert!(rendered.home_nix.contains("ripgrep"));
    assert!(rendered.home_nix.contains("git"));
}

#[test]
fn rendering_copies_the_package_list_into_the_generation() {
    let rendered = render(&user(), &manifest(&["git"])).unwrap();

    assert!(
        rendered
            .home_nix
            .contains(r#"extraBuilderCommands = "cp ${./state} $out/mix-state";"#)
    );
}

#[test]
fn rendering_refuses_a_format_it_does_not_write() {
    let list = StateManifest {
        version: 7,
        packages: names(&["git"]),
    };

    assert!(matches!(
        render(&user(), &list),
        Err(Unrenderable::State(Invalid::Newer(7)))
    ));
}

#[test]
fn rendering_refuses_a_list_without_git() {
    assert!(matches!(
        render(&user(), &manifest(&[])),
        Err(Unrenderable::State(Invalid::Missing("git")))
    ));
}

#[test]
fn rendering_rejects_an_invalid_package_name() {
    assert!(matches!(
        render(&user(), &manifest(&["git", "not a valid ident"])),
        Err(Unrenderable::Package(_))
    ));
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

    #[test]
    fn an_install_then_a_remove_of_the_same_packages_gives_the_list_back(
        installed in proptest::collection::vec("[a-z][a-z0-9-]{0,8}", 0..5),
        requested in proptest::collection::vec("[a-z][a-z0-9-]{0,8}", 1..5),
    ) {
        let mut before = vec!["git".to_string()];
        for package in installed {
            if !before.contains(&package) {
                before.push(package);
            }
        }
        let before = StateManifest { version: STATE_VERSION, packages: before };
        let installed = install(&requested, current(before.clone(), Source::File)).unwrap();
        let added = installed.changed.clone();
        let removed = remove(&added, current(installed.manifest, Source::File)).unwrap();

        proptest::prop_assert_eq!(removed.changed, added);
        proptest::prop_assert_eq!(removed.manifest, before);
    }
}
