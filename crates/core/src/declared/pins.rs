use mix_nixgen::lock::{LockedInput, NarHash};
use mix_nixgen::{INPUTS, Pin, Pins, Rev};
use serde_json::Value;

pub fn locked(locked: &Value) -> Option<LockedInput> {
    Some(LockedInput {
        rev: Rev::parse(locked.get("rev")?.as_str()?).ok()?,
        nar_hash: NarHash::parse(locked.get("narHash")?.as_str()?).ok()?,
        last_modified: locked.get("lastModified")?.as_u64()?,
    })
}

fn pinned(nodes: &Value, pin: Pin) -> Option<LockedInput> {
    let input = INPUTS.iter().find(|input| input.pin == pin)?;
    locked(nodes.get(input.name)?.get("locked")?)
}

pub fn read(flake: &str, lock: &str) -> Option<Pins<LockedInput>> {
    let lock: Value = serde_json::from_str(lock).ok()?;
    let nodes = lock.get("nodes")?;
    let pins = Pins {
        home_manager: pinned(nodes, Pin::HomeManager)?,
        nixpkgs: pinned(nodes, Pin::Nixpkgs)?,
    };
    INPUTS
        .iter()
        .all(|input| {
            flake.contains(&format!(
                "github:{}/{}/{}\"",
                input.owner,
                input.repo,
                pins.of(input.pin).rev.as_str()
            ))
        })
        .then_some(pins)
}

#[cfg(test)]
mod tests {
    use mix_nixgen::{FlakeConfig, System, lock};

    use super::*;

    const NIXPKGS_REV: &str = "0123456789abcdef0123456789abcdef01234567";
    const HOME_MANAGER_REV: &str = "fedcba9876543210fedcba9876543210fedcba98";
    const NIXPKGS_HASH: &str = "sha256-GnotcvKtbTMnoVx6G4E7ZdUX6487Dky74dqq3pU7iRk=";
    const HOME_MANAGER_HASH: &str = "sha256-pD3qVlQ4mUCFoTWrYtxecpSitQMsgJvJIN0FUIDZoEU=";

    fn pins() -> Pins<LockedInput> {
        Pins {
            home_manager: LockedInput {
                rev: Rev::parse(HOME_MANAGER_REV).unwrap(),
                nar_hash: NarHash::parse(HOME_MANAGER_HASH).unwrap(),
                last_modified: 1_789_482_455,
            },
            nixpkgs: LockedInput {
                rev: Rev::parse(NIXPKGS_REV).unwrap(),
                nar_hash: NarHash::parse(NIXPKGS_HASH).unwrap(),
                last_modified: 1_789_426_278,
            },
        }
    }

    fn flake(pins: &Pins<LockedInput>) -> String {
        FlakeConfig::new(
            System::X86_64Linux,
            "mix",
            &pins.nixpkgs.rev,
            &pins.home_manager.rev,
        )
        .unwrap()
        .render()
    }

    #[test]
    fn the_pins_rendered_into_the_files_are_the_pins_read_back() {
        let pins = pins();
        assert_eq!(read(&flake(&pins), &lock::render(pins.clone())), Some(pins));
    }

    #[test]
    fn each_input_keeps_its_own_pin() {
        let read = read(&flake(&pins()), &lock::render(pins())).unwrap();
        assert_eq!(read.nixpkgs.rev.as_str(), NIXPKGS_REV);
        assert_eq!(read.home_manager.rev.as_str(), HOME_MANAGER_REV);
        assert_eq!(read.nixpkgs.nar_hash.as_str(), NIXPKGS_HASH);
        assert_eq!(read.home_manager.last_modified, 1_789_482_455);
    }

    #[test]
    fn a_flake_and_a_lock_naming_different_revisions_are_not_pins() {
        let mut other = pins();
        other.nixpkgs.rev = Rev::parse(HOME_MANAGER_REV).unwrap();
        assert_eq!(read(&flake(&other), &lock::render(pins())), None);
    }

    #[test]
    fn a_lock_that_is_not_json_is_not_pins() {
        assert_eq!(read(&flake(&pins()), ""), None);
        assert_eq!(read(&flake(&pins()), "{"), None);
        assert_eq!(read(&flake(&pins()), "[]"), None);
    }

    #[test]
    fn a_lock_missing_an_input_is_not_pins() {
        let rendered = lock::render(pins());
        let mut lock: Value = serde_json::from_str(&rendered).unwrap();
        lock["nodes"].as_object_mut().unwrap().remove("nixpkgs");
        assert_eq!(read(&flake(&pins()), &lock.to_string()), None);
    }

    #[test]
    fn a_lock_with_a_damaged_value_is_not_pins() {
        let rendered = lock::render(pins());
        for (damaged, with) in [
            (NIXPKGS_REV, "0123456789abcdef"),
            (NIXPKGS_HASH, "sha256-short="),
            ("1789426278", "\"1789426278\""),
        ] {
            assert_eq!(
                read(&flake(&pins()), &rendered.replacen(damaged, with, 1)),
                None,
                "{damaged}"
            );
        }
    }

    #[test]
    fn a_locked_object_is_read_the_way_nix_prints_it() {
        let printed: Value = serde_json::from_str(&format!(
            r#"{{"lastModified":1789426278,"narHash":"{NIXPKGS_HASH}","owner":"NixOS","repo":"nixpkgs","rev":"{NIXPKGS_REV}","type":"github"}}"#
        ))
        .unwrap();
        assert_eq!(locked(&printed), Some(pins().nixpkgs));
        assert_eq!(locked(&Value::Null), None);
    }

    #[test]
    fn a_flake_without_the_pinned_revisions_is_not_pins() {
        assert_eq!(read("{}", &lock::render(pins())), None);
    }
}
