pub const NIX_VERSION: &str = "2.35.4";

pub const NIXPKGS_OWNER: &str = "NixOS";
pub const NIXPKGS_REPO: &str = "nixpkgs";
pub const NIXPKGS_BRANCH: &str = "nixos-unstable";
pub const NIXPKGS_REV: &str = "3ec636362e1816bc2f0888fa0fa2c69f1bc9e504";
pub const NIXPKGS_NAR_HASH: &str = "sha256-rZLrzrC6i+jnTAlvPZHogKDulY7WWDEv9hJeHieO7Zw=";
pub const NIXPKGS_LAST_MODIFIED: u64 = 1792773339;
pub const HOME_MANAGER_OWNER: &str = "nix-community";
pub const HOME_MANAGER_REPO: &str = "home-manager";
pub const HOME_MANAGER_BRANCH: &str = "master";
pub const HOME_MANAGER_REV: &str = "c216bf5a1f173b0481bd5c8c489ebe5595ecc68c";
pub const HOME_MANAGER_NAR_HASH: &str = "sha256-/0tELNnhdSnP+bHSdWiEe6KpfSwi6WrTnT9cm/jNkjA=";
pub const HOME_MANAGER_LAST_MODIFIED: u64 = 1792896866;

pub struct TarballPin {
    pub target: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub size: u64,
}

pub const NIX_TARBALLS: &[TarballPin] = &[
    TarballPin {
        target: "x86_64-linux",
        url: "https://releases.nixos.org/nix/nix-2.35.4/nix-2.35.4-x86_64-linux.tar.xz",
        sha256: "ff66236d2f7e2a21f3350e73668e021ac6a8881ef277a38e16e952bfb22718c3",
        size: 27188748,
    },
    TarballPin {
        target: "aarch64-linux",
        url: "https://releases.nixos.org/nix/nix-2.35.4/nix-2.35.4-aarch64-linux.tar.xz",
        sha256: "14a940a2d0ae5b2c623dc2ca168311c9367fd4bc416f6ded0c1087089d6de934",
        size: 25507776,
    },
];

pub fn pin_for(target: &str) -> Option<&'static TarballPin> {
    NIX_TARBALLS.iter().find(|p| p.target == target)
}
