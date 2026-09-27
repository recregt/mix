use crate::flake::Rev;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NarHash(&'static str);

impl NarHash {
    pub const fn new_static(s: &'static str) -> Self {
        assert!(is_nar_hash(s.as_bytes()), "not a sha256 SRI nar hash");
        Self(s)
    }

    pub fn as_str(self) -> &'static str {
        self.0
    }
}

const PREFIX: &[u8] = b"sha256-";
const BASE64_LEN: usize = 44;

const fn is_nar_hash(bytes: &[u8]) -> bool {
    if bytes.len() != PREFIX.len() + BASE64_LEN {
        return false;
    }
    let mut i = 0;
    while i < PREFIX.len() {
        if bytes[i] != PREFIX[i] {
            return false;
        }
        i += 1;
    }
    while i < bytes.len() - 1 {
        let b = bytes[i];
        if !(b.is_ascii_alphanumeric() || b == b'+' || b == b'/') {
            return false;
        }
        i += 1;
    }
    bytes[bytes.len() - 1] == b'='
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockedInput {
    pub rev: Rev,
    pub nar_hash: NarHash,
    pub last_modified: u64,
}

pub fn render(nixpkgs: LockedInput, home_manager: LockedInput) -> String {
    format!(
        r#"{{
  "nodes": {{
    "home-manager": {{
      "inputs": {{
        "nixpkgs": [
          "nixpkgs"
        ]
      }},
      "locked": {{
        "lastModified": {hm_last_modified},
        "narHash": "{hm_nar_hash}",
        "owner": "nix-community",
        "repo": "home-manager",
        "rev": "{hm_rev}",
        "type": "github"
      }},
      "original": {{
        "owner": "nix-community",
        "repo": "home-manager",
        "rev": "{hm_rev}",
        "type": "github"
      }}
    }},
    "nixpkgs": {{
      "locked": {{
        "lastModified": {nixpkgs_last_modified},
        "narHash": "{nixpkgs_nar_hash}",
        "owner": "NixOS",
        "repo": "nixpkgs",
        "rev": "{nixpkgs_rev}",
        "type": "github"
      }},
      "original": {{
        "owner": "NixOS",
        "repo": "nixpkgs",
        "rev": "{nixpkgs_rev}",
        "type": "github"
      }}
    }},
    "root": {{
      "inputs": {{
        "home-manager": "home-manager",
        "nixpkgs": "nixpkgs"
      }}
    }}
  }},
  "root": "root",
  "version": 7
}}
"#,
        hm_last_modified = home_manager.last_modified,
        hm_nar_hash = home_manager.nar_hash.as_str(),
        hm_rev = home_manager.rev.as_str(),
        nixpkgs_last_modified = nixpkgs.last_modified,
        nixpkgs_nar_hash = nixpkgs.nar_hash.as_str(),
        nixpkgs_rev = nixpkgs.rev.as_str(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_a_sha256_sri_hash() {
        assert!(is_nar_hash(
            b"sha256-GnotcvKtbTMnoVx6G4E7ZdUX6487Dky74dqq3pU7iRk="
        ));
    }

    #[test]
    fn rejects_anything_else() {
        assert!(!is_nar_hash(b""));
        assert!(!is_nar_hash(
            b"sha512-GnotcvKtbTMnoVx6G4E7ZdUX6487Dky74dqq3pU7iRk="
        ));
        assert!(!is_nar_hash(
            b"sha256-GnotcvKtbTMnoVx6G4E7ZdUX6487Dky74dqq3pU7iRk"
        ));
        assert!(!is_nar_hash(
            b"sha256-Gnotcv\"tbTMnoVx6G4E7ZdUX6487Dky74dqq3pU7iRk="
        ));
    }
}
