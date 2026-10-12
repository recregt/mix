use std::collections::BTreeMap;

use mix_nixgen::lock::LockedInput;
use mix_nixgen::{INPUTS, Pins};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputUpdate {
    pub name: &'static str,
    pub owner: &'static str,
    pub repo: &'static str,
    pub branch: &'static str,
    pub before: LockedInput,
    pub after: LockedInput,
}

impl InputUpdate {
    pub fn moved(&self) -> bool {
        self.before.rev != self.after.rev
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub inputs: Vec<InputUpdate>,
}

pub fn decide(have: &Pins<LockedInput>, newest: &Pins<LockedInput>) -> Decision {
    Decision {
        inputs: INPUTS
            .iter()
            .map(|input| InputUpdate {
                name: input.name,
                owner: input.owner,
                repo: input.repo,
                branch: input.branch,
                before: have.of(input.pin).clone(),
                after: newest.of(input.pin).clone(),
            })
            .collect(),
    }
}

impl Decision {
    pub fn moved(&self) -> impl Iterator<Item = &InputUpdate> {
        self.inputs.iter().filter(|input| input.moved())
    }

    pub fn is_current(&self) -> bool {
        self.moved().next().is_none()
    }

    pub fn subject(&self) -> String {
        self.moved()
            .map(|input| input.name)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageChange {
    pub name: String,
    pub from: Option<String>,
    pub to: Option<String>,
}

pub fn package_changes(
    before: &BTreeMap<String, String>,
    after: &BTreeMap<String, String>,
) -> Vec<PackageChange> {
    let mut names: Vec<&String> = before.keys().chain(after.keys()).collect();
    names.sort_unstable();
    names.dedup();
    names
        .into_iter()
        .filter(|name| before.get(*name) != after.get(*name))
        .map(|name| PackageChange {
            name: name.clone(),
            from: before.get(name).cloned(),
            to: after.get(name).cloned(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use mix_nixgen::Rev;
    use mix_nixgen::lock::NarHash;

    use super::*;

    const OLD: &str = "0123456789abcdef0123456789abcdef01234567";
    const NEW: &str = "fedcba9876543210fedcba9876543210fedcba98";
    const HASH: &str = "sha256-GnotcvKtbTMnoVx6G4E7ZdUX6487Dky74dqq3pU7iRk=";

    fn locked(rev: &str, last_modified: u64) -> LockedInput {
        LockedInput {
            rev: Rev::parse(rev).unwrap(),
            nar_hash: NarHash::parse(HASH).unwrap(),
            last_modified,
        }
    }

    fn pins(home_manager: &str, nixpkgs: &str) -> Pins<LockedInput> {
        Pins {
            home_manager: locked(home_manager, 1),
            nixpkgs: locked(nixpkgs, 2),
        }
    }

    fn versions(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(name, version)| (name.to_string(), version.to_string()))
            .collect()
    }

    #[test]
    fn every_input_is_reported_whether_or_not_it_moved() {
        let decision = decide(&pins(OLD, OLD), &pins(OLD, NEW));
        let names: Vec<&str> = decision.inputs.iter().map(|input| input.name).collect();
        assert_eq!(names, vec!["home-manager", "nixpkgs"]);
    }

    #[test]
    fn pins_that_have_not_moved_need_no_update() {
        let decision = decide(&pins(OLD, OLD), &pins(OLD, OLD));
        assert!(decision.is_current());
        assert_eq!(decision.moved().count(), 0);
        assert_eq!(decision.subject(), "");
    }

    #[test]
    fn only_the_input_that_moved_is_reported_as_moved() {
        let decision = decide(&pins(OLD, OLD), &pins(OLD, NEW));
        let moved: Vec<&InputUpdate> = decision.moved().collect();
        assert_eq!(moved.len(), 1);
        assert_eq!(moved[0].name, "nixpkgs");
        assert_eq!(moved[0].owner, "NixOS");
        assert_eq!(moved[0].branch, "nixos-unstable");
        assert_eq!(moved[0].before, locked(OLD, 2));
        assert_eq!(moved[0].after, locked(NEW, 2));
        assert_eq!(decision.subject(), "nixpkgs");
    }

    #[test]
    fn both_inputs_can_move_together() {
        let decision = decide(&pins(OLD, OLD), &pins(NEW, NEW));
        assert!(!decision.is_current());
        assert_eq!(decision.subject(), "home-manager, nixpkgs");
    }

    #[test]
    fn a_package_is_reported_only_when_its_version_changed() {
        let before = versions(&[("curl", "8.22.0"), ("git", "2.55.0"), ("qemu", "11.1.0")]);
        let after = versions(&[("curl", "8.22.0"), ("git", "2.55.0"), ("qemu", "11.1.1")]);
        assert_eq!(
            package_changes(&before, &after),
            vec![PackageChange {
                name: "qemu".into(),
                from: Some("11.1.0".into()),
                to: Some("11.1.1".into()),
            }]
        );
    }

    #[test]
    fn a_package_that_appears_or_vanishes_has_one_side_missing() {
        let before = versions(&[("old", "1.0")]);
        let after = versions(&[("new", "2.0")]);
        assert_eq!(
            package_changes(&before, &after),
            vec![
                PackageChange {
                    name: "new".into(),
                    from: None,
                    to: Some("2.0".into()),
                },
                PackageChange {
                    name: "old".into(),
                    from: Some("1.0".into()),
                    to: None,
                },
            ]
        );
    }

    #[test]
    fn identical_versions_have_no_changes() {
        let same = versions(&[("git", "2.55.0")]);
        assert!(package_changes(&same, &same).is_empty());
        assert!(package_changes(&BTreeMap::new(), &BTreeMap::new()).is_empty());
    }
}
