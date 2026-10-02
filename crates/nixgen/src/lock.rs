use crate::flake::Rev;
use crate::inputs::{INPUTS, Pins};

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

pub fn render(locked: Pins<LockedInput>) -> String {
    let mut out = String::from("{\n  \"nodes\": {\n");
    for input in &INPUTS {
        let pin = locked.of(input.pin);
        out.push_str(&format!("    \"{}\": {{\n", input.name));
        if let Some(follows) = input.follows {
            out.push_str(&format!(
                "      \"inputs\": {{\n        \"{follows}\": [\n          \"{follows}\"\n        ]\n      }},\n"
            ));
        }
        out.push_str(&format!(
            "      \"locked\": {{\n        \"lastModified\": {},\n        \"narHash\": \"{}\",\n        \"owner\": \"{}\",\n        \"repo\": \"{}\",\n        \"rev\": \"{}\",\n        \"type\": \"github\"\n      }},\n",
            pin.last_modified,
            pin.nar_hash.as_str(),
            input.owner,
            input.repo,
            pin.rev.as_str(),
        ));
        out.push_str(&format!(
            "      \"original\": {{\n        \"owner\": \"{}\",\n        \"repo\": \"{}\",\n        \"rev\": \"{}\",\n        \"type\": \"github\"\n      }}\n    }},\n",
            input.owner,
            input.repo,
            pin.rev.as_str(),
        ));
    }
    out.push_str("    \"root\": {\n      \"inputs\": {\n");
    for (index, input) in INPUTS.iter().enumerate() {
        let comma = if index + 1 < INPUTS.len() { "," } else { "" };
        out.push_str(&format!("        \"{0}\": \"{0}\"{comma}\n", input.name));
    }
    out.push_str("      }\n    }\n  },\n  \"root\": \"root\",\n  \"version\": 7\n}\n");
    out
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
