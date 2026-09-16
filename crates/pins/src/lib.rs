pub const NIX_VERSION: &str = "2.35.2";

pub const NIXPKGS_REV: &str = "efe6f071ede9d21c37462d2d6682d5e670099684";
pub const HOME_MANAGER_REV: &str = "efa3ccb4c3cc90d832eab232976379058fa75aa3";

pub struct TarballPin {
    pub target: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
}

pub const NIX_TARBALLS: &[TarballPin] = &[
    TarballPin {
        target: "x86_64-linux",
        url: "https://releases.nixos.org/nix/nix-2.35.2/nix-2.35.2-x86_64-linux.tar.xz",
        sha256: "0c3960a9792331a22081c3c7a5d8465db9b17c50b3acdf18587fa4c6f2cb1158",
    },
    TarballPin {
        target: "aarch64-linux",
        url: "https://releases.nixos.org/nix/nix-2.35.2/nix-2.35.2-aarch64-linux.tar.xz",
        sha256: "4d0302a2910f5eec1c33b8deef634f04899a75737e7001ec49908d003ae5efda",
    },
];

pub fn pin_for(target: &str) -> Option<&'static TarballPin> {
    NIX_TARBALLS.iter().find(|p| p.target == target)
}
