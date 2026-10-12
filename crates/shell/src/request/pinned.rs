use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use mix_nixgen::Pins;
use mix_nixgen::lock::LockedInput;

#[derive(Default)]
pub struct Pinned {
    users: Mutex<HashMap<u32, Pins<LockedInput>>>,
}

impl Pinned {
    pub fn get_or_load(
        &self,
        uid: u32,
        load: impl FnOnce() -> Pins<LockedInput>,
    ) -> Pins<LockedInput> {
        self.users
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(uid)
            .or_insert_with(load)
            .clone()
    }

    pub fn set(&self, uid: u32, pins: Pins<LockedInput>) {
        self.users
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(uid, pins);
    }
}

#[cfg(test)]
mod tests {
    use mix_nixgen::Rev;
    use mix_nixgen::lock::NarHash;

    use super::*;

    fn pins(rev: &str) -> Pins<LockedInput> {
        let locked = LockedInput {
            rev: Rev::parse(rev).unwrap(),
            nar_hash: NarHash::parse("sha256-GnotcvKtbTMnoVx6G4E7ZdUX6487Dky74dqq3pU7iRk=")
                .unwrap(),
            last_modified: 1,
        };
        Pins {
            home_manager: locked.clone(),
            nixpkgs: locked,
        }
    }

    const OLD: &str = "0123456789abcdef0123456789abcdef01234567";
    const NEW: &str = "fedcba9876543210fedcba9876543210fedcba98";

    #[test]
    fn a_user_is_loaded_once_and_then_remembered() {
        let pinned = Pinned::default();
        assert_eq!(pinned.get_or_load(1000, || pins(OLD)), pins(OLD));
        assert_eq!(pinned.get_or_load(1000, || pins(NEW)), pins(OLD));
    }

    #[test]
    fn what_is_set_replaces_what_was_loaded() {
        let pinned = Pinned::default();
        pinned.get_or_load(1000, || pins(OLD));
        pinned.set(1000, pins(NEW));
        assert_eq!(pinned.get_or_load(1000, || pins(OLD)), pins(NEW));
    }

    #[test]
    fn each_user_keeps_their_own_pins() {
        let pinned = Pinned::default();
        pinned.set(1000, pins(OLD));
        pinned.set(1001, pins(NEW));
        assert_eq!(pinned.get_or_load(1000, || pins(NEW)), pins(OLD));
        assert_eq!(pinned.get_or_load(1001, || pins(OLD)), pins(NEW));
    }
}
