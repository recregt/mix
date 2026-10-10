use std::collections::BTreeSet;
use std::path::PathBuf;

use super::*;
use crate::declared::identity::InvokingUser;
use crate::declared::policy::Policy;
use crate::declared::targets::{UserConfig, tree};

fn user_config() -> UserConfig {
    UserConfig {
        user: InvokingUser {
            uid: 1000,
            gid: 1000,
            name: "alice".to_string(),
            home: PathBuf::from("/home/alice"),
        },
        flake: "flake".to_string(),
        lock: "lock".to_string(),
        home: "home".to_string(),
        restored_state: None,
    }
}

fn labels(checks: &[Check<'_>]) -> Vec<String> {
    checks
        .iter()
        .map(|check| check.label().into_owned())
        .collect()
}

fn index_of(checks: &[Check<'_>], label: &str) -> usize {
    labels(checks)
        .iter()
        .position(|found| found == label)
        .unwrap_or_else(|| panic!("no check named {label}"))
}

#[test]
fn every_check_has_its_own_key() {
    let policy = Policy::default();
    let cfg = user_config();
    let tree = tree(Some(&cfg), &policy);
    let labels = labels(tree.checks());
    let unique: BTreeSet<&String> = labels.iter().collect();
    assert_eq!(unique.len(), labels.len(), "{labels:#?}");
}

#[test]
fn a_check_only_waits_on_checks_before_it() {
    let policy = Policy::default();
    let cfg = user_config();
    let tree = tree(Some(&cfg), &policy);
    for (index, check) in tree.checks().iter().enumerate() {
        assert!(
            check.waits_on().all(|before| before < index),
            "{} waits on a later check",
            check.label()
        );
    }
}

#[test]
fn a_path_waits_on_the_nearest_declared_directory_above_it() {
    let policy = Policy::default();
    let cfg = user_config();
    let tree = tree(Some(&cfg), &policy);
    let checks = tree.checks();
    let config = index_of(checks, "/home/alice/.local/state/mix/.git/config");
    let git = index_of(checks, "/home/alice/.local/state/mix/.git");
    assert_eq!(checks[config].after, vec![git]);
    let home = index_of(checks, "/home/alice");
    let local = index_of(checks, "/home/alice/.local");
    assert_eq!(checks[local].after, vec![home]);
}

#[test]
fn the_nix_daemon_units_come_after_what_they_need() {
    let policy = Policy::default();
    let tree = tree(None, &policy);
    let checks = tree.checks();
    let runtime = index_of(checks, "default profile");
    let nix_conf = index_of(checks, "/etc/nix/nix.conf");
    let nixbld = index_of(checks, "nixbld");
    for unit in ["nix-daemon.service", "nix-daemon.socket"] {
        let at = index_of(checks, unit);
        assert!(checks[at].after.contains(&runtime), "{unit}");
        for needed in [runtime, nix_conf, nixbld] {
            assert!(
                needed < at,
                "{unit} comes before {}",
                checks[needed].label()
            );
        }
    }
    let program = index_of(checks, "/var/lib/mix/bin/mix-daemon");
    let service = index_of(checks, "mix-daemon.service");
    assert!(program < service && checks[service].after.contains(&program));
}

#[test]
fn a_check_on_the_way_up_waits_on_everything_below() {
    let policy = Policy::default();
    let cfg = user_config();
    let tree = tree(Some(&cfg), &policy);
    let checks = tree.checks();
    let history = index_of(checks, "/home/alice/.local/state/mix/.git history");
    let git = index_of(checks, "/home/alice/.local/state/mix/.git");
    let config = index_of(checks, "/home/alice/.local/state/mix/.git/config");
    assert_eq!(checks[history].phase, Phase::Up);
    assert!(checks[history].waits_on().any(|before| before == git));
    assert!(checks[history].waits_on().any(|before| before == config));
    let in_the_way = index_of(checks, "/home/alice files in the way");
    let generations = index_of(checks, "/home/alice/.local/state/nix/profiles/home-manager");
    assert!(
        checks[in_the_way]
            .waits_on()
            .any(|before| before == generations)
    );
    assert!(
        checks[in_the_way]
            .waits_on()
            .any(|before| before == history)
    );
}

#[test]
fn leftovers_are_looked_for_only_where_mix_writes() {
    let policy = Policy::default();
    let cfg = user_config();
    let tree = tree(Some(&cfg), &policy);
    let labels = labels(tree.checks());
    for written in [
        "/etc/nix leftovers",
        "/etc/systemd/system leftovers",
        "/var/lib/mix/bin leftovers",
        "/home/alice/.local/state/mix leftovers",
        "/home/alice/.local/state/mix/.git leftovers",
    ] {
        assert!(labels.iter().any(|label| label == written), "{written}");
    }
    for untouched in [
        "/etc/systemd leftovers",
        "/home/alice/.local/state/nix/profiles leftovers",
    ] {
        assert!(
            !labels.iter().any(|label| label == untouched),
            "{untouched}"
        );
    }
}

#[test]
fn the_root_keeps_the_declared_order_and_directories_sort_by_path() {
    let mut builder = Builder::new(None);
    for path in ["/b", "/b/z", "/b/a", "/a"] {
        builder.path(
            path,
            Target::Precondition {
                path: Cow::Owned(PathBuf::from(path)),
            },
        );
    }
    let tree = builder.finish();
    assert_eq!(labels(tree.checks()), vec!["/b", "/b/a", "/b/z", "/a"]);
}
