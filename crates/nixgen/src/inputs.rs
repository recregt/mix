use crate::ast::Key;
use crate::ident::Ident;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pin {
    HomeManager,
    Nixpkgs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pins<T> {
    pub home_manager: T,
    pub nixpkgs: T,
}

impl<T: Copy> Pins<T> {
    pub fn of(&self, pin: Pin) -> T {
        match pin {
            Pin::HomeManager => self.home_manager,
            Pin::Nixpkgs => self.nixpkgs,
        }
    }
}

pub struct Input {
    pub pin: Pin,
    pub name: &'static str,
    pub owner: &'static str,
    pub repo: &'static str,
    pub follows: Option<&'static str>,
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) key: Key,
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) ident: Ident,
}

const fn input(
    pin: Pin,
    name: &'static str,
    owner: &'static str,
    repo: &'static str,
    follows: Option<&'static str>,
) -> Input {
    Input {
        pin,
        name,
        owner,
        repo,
        follows,
        key: Key::new_static(name),
        ident: Ident::new_static(name),
    }
}

pub static INPUTS: [Input; 2] = [
    input(
        Pin::HomeManager,
        "home-manager",
        "nix-community",
        "home-manager",
        Some("nixpkgs"),
    ),
    input(Pin::Nixpkgs, "nixpkgs", "NixOS", "nixpkgs", None),
];

const ROOT: &str = "root";

const fn before(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut i = 0;
    while i < a.len() && i < b.len() {
        if a[i] != b[i] {
            return a[i] < b[i];
        }
        i += 1;
    }
    a.len() < b.len()
}

const fn equal(a: &str, b: &str) -> bool {
    !before(a, b) && !before(b, a)
}

const fn declared(name: &str) -> bool {
    let mut i = 0;
    while i < INPUTS.len() {
        if equal(INPUTS[i].name, name) {
            return true;
        }
        i += 1;
    }
    false
}

const _: () = {
    let mut i = 0;
    while i < INPUTS.len() {
        assert!(
            before(INPUTS[i].name, ROOT),
            "an input name sorts after the lock's root node"
        );
        if i + 1 < INPUTS.len() {
            assert!(
                before(INPUTS[i].name, INPUTS[i + 1].name),
                "inputs are not in Nix's sorted order"
            );
        }
        if let Some(follows) = INPUTS[i].follows {
            assert!(
                declared(follows),
                "an input follows one that is not declared"
            );
        }
        i += 1;
    }
};
