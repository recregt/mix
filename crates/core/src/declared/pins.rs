use mix_nixgen::lock::{EMBEDDED, LockedInput, NarHash};
use mix_nixgen::{Input, Pin, Pins, Rev};
use serde_json::Value;

pub fn locked(input: &Input, locked: &Value) -> Option<LockedInput> {
    let text = |field: &str| locked.get(field).and_then(Value::as_str);
    if text("type")? != "github" || text("owner")? != input.owner || text("repo")? != input.repo {
        return None;
    }
    Some(LockedInput {
        rev: Rev::parse(text("rev")?).ok()?,
        nar_hash: NarHash::parse(text("narHash")?).ok()?,
        last_modified: locked
            .get("lastModified")?
            .as_u64()
            .filter(|seconds| i64::try_from(*seconds).is_ok())?,
    })
}

pub fn read(lock: &str) -> Option<Pins<LockedInput>> {
    from_lock(&serde_json::from_str(lock).ok()?)
}

pub fn from_lock(lock: &Value) -> Option<Pins<LockedInput>> {
    let nodes = lock.get("nodes")?;
    let root = nodes.get(lock.get("root")?.as_str()?)?.get("inputs")?;
    let pinned = |pin: Pin| {
        let input = pin.input();
        let node = nodes.get(root.get(input.name)?.as_str()?)?;
        locked(input, node.get("locked")?)
    };
    Some(Pins {
        home_manager: pinned(Pin::HomeManager)?,
        nixpkgs: pinned(Pin::Nixpkgs)?,
    })
}

pub fn settle(file: Option<&str>, generation: Option<&str>, mirrored: bool) -> Pins<LockedInput> {
    if mirrored {
        return EMBEDDED;
    }
    let found = match (file.and_then(read), generation.and_then(read)) {
        (Some(file), Some(generation)) if file != generation => Some(generation),
        (Some(file), _) => Some(file),
        (None, generation) => generation,
    };
    match found {
        Some(found) => forward(&found, &EMBEDDED),
        None => EMBEDDED,
    }
}

pub fn forward(pins: &Pins<LockedInput>, to: &Pins<LockedInput>) -> Pins<LockedInput> {
    let newer = |pin: Pin| {
        let (from, to) = (pins.of(pin), to.of(pin));
        if to.last_modified > from.last_modified {
            to.clone()
        } else {
            from.clone()
        }
    };
    Pins {
        home_manager: newer(Pin::HomeManager),
        nixpkgs: newer(Pin::Nixpkgs),
    }
}

#[cfg(test)]
mod tests {
    use mix_nixgen::lock;

    use super::*;

    const NIXPKGS_REV: &str = "0123456789abcdef0123456789abcdef01234567";
    const HOME_MANAGER_REV: &str = "fedcba9876543210fedcba9876543210fedcba98";
    const NIXPKGS_HASH: &str = "sha256-GnotcvKtbTMnoVx6G4E7ZdUX6487Dky74dqq3pU7iRk=";
    const HOME_MANAGER_HASH: &str = "sha256-pD3qVlQ4mUCFoTWrYtxecpSitQMsgJvJIN0FUIDZoEU=";

    fn pins_at(moved: i64) -> Pins<LockedInput> {
        let at = |embedded: u64| embedded.checked_add_signed(moved).unwrap();
        Pins {
            home_manager: LockedInput {
                rev: Rev::parse(HOME_MANAGER_REV).unwrap(),
                nar_hash: NarHash::parse(HOME_MANAGER_HASH).unwrap(),
                last_modified: at(EMBEDDED.home_manager.last_modified),
            },
            nixpkgs: LockedInput {
                rev: Rev::parse(NIXPKGS_REV).unwrap(),
                nar_hash: NarHash::parse(NIXPKGS_HASH).unwrap(),
                last_modified: at(EMBEDDED.nixpkgs.last_modified),
            },
        }
    }

    fn newer() -> Pins<LockedInput> {
        pins_at(1_000)
    }

    fn older() -> Pins<LockedInput> {
        pins_at(-1_000_000)
    }

    fn rendered(pins: &Pins<LockedInput>) -> String {
        lock::render(pins.clone())
    }

    #[test]
    fn the_pins_rendered_into_a_lock_are_the_pins_read_back() {
        let pins = newer();
        assert_eq!(read(&rendered(&pins)), Some(pins));
    }

    #[test]
    fn each_input_keeps_its_own_pin() {
        let read = read(&rendered(&newer())).unwrap();
        assert_eq!(read.nixpkgs.rev.as_str(), NIXPKGS_REV);
        assert_eq!(read.home_manager.rev.as_str(), HOME_MANAGER_REV);
        assert_eq!(read.nixpkgs.nar_hash.as_str(), NIXPKGS_HASH);
        assert_eq!(read.home_manager.nar_hash.as_str(), HOME_MANAGER_HASH);
    }

    #[test]
    fn an_input_is_found_through_the_node_the_root_names() {
        let mut lock: Value = serde_json::from_str(&rendered(&newer())).unwrap();
        let nodes = lock["nodes"].as_object_mut().unwrap();
        let node = nodes.remove("nixpkgs").unwrap();
        nodes.insert("nixpkgs_2".to_string(), node);
        lock["nodes"]["root"]["inputs"]["nixpkgs"] = "nixpkgs_2".into();
        assert_eq!(read(&lock.to_string()), Some(newer()));
    }

    #[test]
    fn a_lock_that_is_not_json_is_not_pins() {
        assert_eq!(read(""), None);
        assert_eq!(read("{"), None);
        assert_eq!(read("[]"), None);
    }

    #[test]
    fn a_lock_missing_an_input_is_not_pins() {
        let mut lock: Value = serde_json::from_str(&rendered(&newer())).unwrap();
        lock["nodes"].as_object_mut().unwrap().remove("nixpkgs");
        assert_eq!(read(&lock.to_string()), None);
    }

    #[test]
    fn a_lock_with_a_damaged_value_is_not_pins() {
        let lock = rendered(&newer());
        let last_modified = newer().nixpkgs.last_modified.to_string();
        for (damaged, with) in [
            (NIXPKGS_REV, "0123456789abcdef"),
            (NIXPKGS_HASH, "sha256-short="),
            (last_modified.as_str(), "\"1789426278\""),
            ("\"NixOS\"", "\"someone-else\""),
            ("\"github\"", "\"gitlab\""),
            (last_modified.as_str(), "18446744073709551615"),
        ] {
            assert_eq!(read(&lock.replacen(damaged, with, 1)), None, "{damaged}");
        }
    }

    #[test]
    fn a_locked_object_is_read_the_way_nix_prints_it() {
        let printed: Value = serde_json::from_str(&format!(
            r#"{{"lastModified":1789426278,"narHash":"{NIXPKGS_HASH}","owner":"NixOS","repo":"nixpkgs","rev":"{NIXPKGS_REV}","type":"github"}}"#
        ))
        .unwrap();
        let input = Pin::Nixpkgs.input();
        let expected = LockedInput {
            rev: Rev::parse(NIXPKGS_REV).unwrap(),
            nar_hash: NarHash::parse(NIXPKGS_HASH).unwrap(),
            last_modified: 1_789_426_278,
        };
        assert_eq!(locked(input, &printed), Some(expected));
        assert_eq!(locked(Pin::HomeManager.input(), &printed), None);
        assert_eq!(locked(input, &Value::Null), None);
    }

    #[test]
    fn a_user_without_pins_gets_the_embedded_ones() {
        assert_eq!(settle(None, None, false), EMBEDDED);
        assert_eq!(settle(Some("damaged"), Some("damaged"), false), EMBEDDED);
    }

    #[test]
    fn the_pins_of_the_files_are_kept() {
        let lock = rendered(&newer());
        assert_eq!(settle(Some(&lock), None, false), newer());
        assert_eq!(settle(Some(&lock), Some(&lock), false), newer());
    }

    #[test]
    fn the_active_generation_restores_pins_the_files_lost() {
        let lock = rendered(&newer());
        assert_eq!(settle(None, Some(&lock), false), newer());
        assert_eq!(settle(Some("damaged"), Some(&lock), false), newer());
    }

    #[test]
    fn the_active_generation_wins_over_files_that_disagree_with_it() {
        let generation = rendered(&newer());
        let file = rendered(&pins_at(2_000));
        assert_eq!(settle(Some(&file), Some(&generation), false), newer());
    }

    #[test]
    fn pins_older_than_the_embedded_ones_move_up_to_them() {
        let lock = rendered(&older());
        assert_eq!(settle(Some(&lock), Some(&lock), false), EMBEDDED);
    }

    #[test]
    fn each_input_moves_up_to_the_embedded_pin_on_its_own() {
        let mut mixed = older();
        mixed.home_manager = newer().home_manager;
        let lock = rendered(&mixed);
        let settled = settle(Some(&lock), None, false);
        assert_eq!(settled.home_manager, mixed.home_manager);
        assert_eq!(settled.nixpkgs, EMBEDDED.nixpkgs);
    }

    #[test]
    fn a_mirrored_machine_always_uses_the_embedded_pins() {
        let lock = rendered(&newer());
        assert_eq!(settle(Some(&lock), Some(&lock), true), EMBEDDED);
    }

    #[test]
    fn a_pin_moves_forward_only() {
        assert_eq!(forward(&older(), &EMBEDDED), EMBEDDED);
        assert_eq!(forward(&EMBEDDED, &EMBEDDED), EMBEDDED);
        assert_eq!(forward(&newer(), &EMBEDDED), newer());
    }
}
