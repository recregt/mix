use std::collections::BTreeMap;

use mix_nixgen::lock::LockedInput;
use mix_nixgen::{INPUTS, Input, Pin, Pins};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Updated,
    Current,
    Kept,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputUpdate {
    pub pin: Pin,
    pub outcome: Outcome,
    pub from: LockedInput,
    pub to: LockedInput,
}

impl InputUpdate {
    pub fn input(&self) -> &'static Input {
        self.pin.input()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub inputs: Vec<InputUpdate>,
    pub pins: Pins<LockedInput>,
}

fn step(from: &LockedInput, newest: &LockedInput) -> (Outcome, LockedInput) {
    if newest.rev == from.rev {
        (Outcome::Current, from.clone())
    } else if newest.last_modified > from.last_modified {
        (Outcome::Updated, newest.clone())
    } else {
        (Outcome::Kept, from.clone())
    }
}

pub fn decide(have: &Pins<LockedInput>, newest: &Pins<LockedInput>) -> Decision {
    let inputs: Vec<InputUpdate> = INPUTS
        .iter()
        .map(|input| {
            let from = have.of(input.pin);
            let (outcome, to) = step(from, newest.of(input.pin));
            InputUpdate {
                pin: input.pin,
                outcome,
                from: from.clone(),
                to,
            }
        })
        .collect();
    let to = |pin: Pin| {
        inputs
            .iter()
            .find(|update| update.pin == pin)
            .map(|update| update.to.clone())
            .expect("every pin has an input")
    };
    let pins = Pins {
        home_manager: to(Pin::HomeManager),
        nixpkgs: to(Pin::Nixpkgs),
    };
    Decision { inputs, pins }
}

impl Decision {
    pub fn moved(&self) -> impl Iterator<Item = &InputUpdate> {
        self.inputs
            .iter()
            .filter(|update| update.outcome == Outcome::Updated)
    }

    pub fn is_current(&self) -> bool {
        self.moved().next().is_none()
    }

    pub fn subject(&self) -> String {
        self.moved()
            .map(|update| update.input().name)
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

pub type Versions = BTreeMap<String, Option<String>>;

pub fn package_changes(before: &Versions, after: &Versions) -> Vec<PackageChange> {
    let mut names: Vec<&String> = before.keys().chain(after.keys()).collect();
    names.sort_unstable();
    names.dedup();
    names
        .into_iter()
        .filter_map(|name| {
            let from = before.get(name).cloned().flatten();
            let to = after.get(name).cloned().flatten();
            (from != to).then(|| PackageChange {
                name: name.clone(),
                from,
                to,
            })
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

    fn pins(home_manager: (&str, u64), nixpkgs: (&str, u64)) -> Pins<LockedInput> {
        Pins {
            home_manager: locked(home_manager.0, home_manager.1),
            nixpkgs: locked(nixpkgs.0, nixpkgs.1),
        }
    }

    fn versions(pairs: &[(&str, Option<&str>)]) -> Versions {
        pairs
            .iter()
            .map(|(name, version)| (name.to_string(), version.map(str::to_string)))
            .collect()
    }

    fn outcomes(decision: &Decision) -> Vec<(&str, Outcome)> {
        decision
            .inputs
            .iter()
            .map(|update| (update.input().name, update.outcome))
            .collect()
    }

    #[test]
    fn every_input_is_reported_whether_or_not_it_moved() {
        let decision = decide(&pins((OLD, 1), (OLD, 1)), &pins((OLD, 1), (NEW, 2)));
        assert_eq!(
            outcomes(&decision),
            vec![
                ("home-manager", Outcome::Current),
                ("nixpkgs", Outcome::Updated)
            ]
        );
    }

    #[test]
    fn pins_that_have_not_moved_need_no_update() {
        let have = pins((OLD, 1), (OLD, 1));
        let decision = decide(&have, &have);
        assert!(decision.is_current());
        assert_eq!(decision.subject(), "");
        assert_eq!(decision.pins, have);
    }

    #[test]
    fn an_input_that_moved_takes_the_newest_pin() {
        let decision = decide(&pins((OLD, 1), (OLD, 1)), &pins((OLD, 1), (NEW, 2)));
        let moved: Vec<&InputUpdate> = decision.moved().collect();
        assert_eq!(moved.len(), 1);
        assert_eq!(moved[0].input().name, "nixpkgs");
        assert_eq!(moved[0].input().branch, "nixos-unstable");
        assert_eq!(moved[0].from, locked(OLD, 1));
        assert_eq!(moved[0].to, locked(NEW, 2));
        assert_eq!(decision.pins, pins((OLD, 1), (NEW, 2)));
        assert_eq!(decision.subject(), "nixpkgs");
    }

    #[test]
    fn both_inputs_can_move_together() {
        let decision = decide(&pins((OLD, 1), (OLD, 1)), &pins((NEW, 2), (NEW, 2)));
        assert!(!decision.is_current());
        assert_eq!(decision.subject(), "home-manager, nixpkgs");
    }

    #[test]
    fn a_pin_never_moves_back_to_an_older_upstream() {
        let have = pins((NEW, 5), (NEW, 5));
        let decision = decide(&have, &pins((OLD, 1), (OLD, 5)));
        assert_eq!(
            outcomes(&decision),
            vec![("home-manager", Outcome::Kept), ("nixpkgs", Outcome::Kept)]
        );
        assert!(decision.is_current());
        assert_eq!(decision.pins, have);
    }

    #[test]
    fn a_package_is_reported_only_when_its_version_changed() {
        let before = versions(&[("curl", Some("8.22.0")), ("qemu", Some("11.1.0"))]);
        let after = versions(&[("curl", Some("8.22.0")), ("qemu", Some("11.1.1"))]);
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
    fn a_package_that_gains_or_loses_a_version_has_one_side_missing() {
        let before = versions(&[("gone", Some("1.0")), ("new", None)]);
        let after = versions(&[("gone", None), ("new", Some("2.0"))]);
        assert_eq!(
            package_changes(&before, &after),
            vec![
                PackageChange {
                    name: "gone".into(),
                    from: Some("1.0".into()),
                    to: None,
                },
                PackageChange {
                    name: "new".into(),
                    from: None,
                    to: Some("2.0".into()),
                },
            ]
        );
    }

    #[test]
    fn a_package_without_a_version_on_either_side_has_no_change() {
        let same = versions(&[("lib", None), ("git", Some("2.55.0"))]);
        assert!(package_changes(&same, &same).is_empty());
        assert!(package_changes(&same, &versions(&[("lib", None)])).len() == 1);
        assert!(package_changes(&Versions::new(), &Versions::new()).is_empty());
    }
}
