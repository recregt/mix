use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use crate::ident::is_identifier;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InvalidInstallable {
    #[error("`{}` is not an absolute path", .0.display())]
    RelativePath(PathBuf),
    #[error("an attribute path segment is empty")]
    EmptySegment,
    #[error("`{0}` contains a quote, which a nix attribute path cannot hold")]
    Quote(String),
    #[error("`{0}` is not a base64 public key")]
    PublicKey(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicKey(String);

impl PublicKey {
    pub fn new(base64: impl Into<String>) -> Result<Self, InvalidInstallable> {
        let base64 = base64.into();
        let valid = !base64.is_empty()
            && base64
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='));
        if valid {
            Ok(Self(base64))
        } else {
            Err(InvalidInstallable::PublicKey(base64))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlakeRef {
    Path {
        dir: PathBuf,
    },
    GitFile {
        dir: PathBuf,
        signer: Option<PublicKey>,
    },
}

impl FlakeRef {
    pub fn path(dir: impl Into<PathBuf>) -> Result<Self, InvalidInstallable> {
        Ok(Self::Path {
            dir: absolute(dir.into())?,
        })
    }

    pub fn git_file(
        dir: impl Into<PathBuf>,
        signer: Option<PublicKey>,
    ) -> Result<Self, InvalidInstallable> {
        Ok(Self::GitFile {
            dir: absolute(dir.into())?,
            signer,
        })
    }

    fn write(&self, out: &mut String) {
        match self {
            Self::Path { dir } => {
                out.push_str("path:");
                write_path(dir, out);
            }
            Self::GitFile { dir, signer } => {
                out.push_str("git+file://");
                write_path(dir, out);
                if let Some(key) = signer {
                    out.push_str("?verifyCommit=1&keytype=ssh-ed25519&publicKey=");
                    percent_encode(key.as_str().as_bytes(), is_unreserved, out);
                }
            }
        }
    }
}

fn absolute(dir: PathBuf) -> Result<PathBuf, InvalidInstallable> {
    if dir.is_absolute() {
        Ok(dir)
    } else {
        Err(InvalidInstallable::RelativePath(dir))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttrPath(Vec<String>);

impl AttrPath {
    pub fn new<I, S>(segments: I) -> Result<Self, InvalidInstallable>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let segments = segments
            .into_iter()
            .map(|segment| {
                let segment = segment.into();
                if segment.is_empty() {
                    Err(InvalidInstallable::EmptySegment)
                } else if segment.contains('"') {
                    Err(InvalidInstallable::Quote(segment))
                } else {
                    Ok(segment)
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        if segments.is_empty() {
            return Err(InvalidInstallable::EmptySegment);
        }
        Ok(Self(segments))
    }

    fn write(&self, out: &mut String) {
        let mut fragment = String::new();
        for (index, segment) in self.0.iter().enumerate() {
            if index > 0 {
                fragment.push('.');
            }
            if is_identifier(segment) {
                fragment.push_str(segment);
            } else {
                fragment.push('"');
                fragment.push_str(segment);
                fragment.push('"');
            }
        }
        percent_encode(fragment.as_bytes(), is_unreserved, out);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installable {
    flake: FlakeRef,
    attr_path: AttrPath,
}

impl Installable {
    pub fn new(flake: FlakeRef, attr_path: AttrPath) -> Self {
        Self { flake, attr_path }
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        self.flake.write(&mut out);
        out.push('#');
        self.attr_path.write(&mut out);
        out
    }
}

fn write_path(dir: &Path, out: &mut String) {
    percent_encode(dir.as_os_str().as_bytes(), is_path_byte, out);
}

fn is_unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
}

fn is_path_byte(byte: u8) -> bool {
    is_unreserved(byte) || byte == b'/'
}

fn percent_encode(bytes: &[u8], keep: fn(u8) -> bool, out: &mut String) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for &byte in bytes {
        if keep(byte) {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push(HEX[usize::from(byte >> 4)] as char);
            out.push(HEX[usize::from(byte & 0x0f)] as char);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use proptest::prelude::*;

    use super::*;

    fn home_manager(dir: &str, user: &str) -> Installable {
        Installable::new(
            FlakeRef::path(dir).unwrap(),
            AttrPath::new(["homeConfigurations", user, "activationPackage"]).unwrap(),
        )
    }

    #[test]
    fn renders_a_plain_path_flake() {
        assert_eq!(
            home_manager("/home/mix/.local/state/mix", "mix").render(),
            "path:/home/mix/.local/state/mix#homeConfigurations.mix.activationPackage"
        );
    }

    #[test]
    fn quotes_a_segment_that_is_not_an_identifier() {
        assert_eq!(
            home_manager("/h", "john.doe").render(),
            "path:/h#homeConfigurations.%22john.doe%22.activationPackage"
        );
    }

    #[test]
    fn quotes_a_segment_that_is_a_keyword() {
        assert_eq!(
            home_manager("/h", "in").render(),
            "path:/h#homeConfigurations.%22in%22.activationPackage"
        );
    }

    #[test]
    fn encodes_every_character_nix_would_read_as_structure() {
        let rendered = home_manager("/home/we ird#dir?x%41^\"", "mix").render();
        assert_eq!(
            rendered,
            "path:/home/we%20ird%23dir%3Fx%2541%5E%22#homeConfigurations.mix.activationPackage"
        );
    }

    #[test]
    fn renders_a_git_flake_with_its_signer() {
        let installable = Installable::new(
            FlakeRef::git_file(
                "/home/mix/.local/state/mix",
                Some(PublicKey::new("AAAA+b/c=").unwrap()),
            )
            .unwrap(),
            AttrPath::new(["x"]).unwrap(),
        );
        assert_eq!(
            installable.render(),
            "git+file:///home/mix/.local/state/mix?verifyCommit=1&keytype=ssh-ed25519&publicKey=AAAA%2Bb%2Fc%3D#x"
        );
    }

    #[test]
    fn renders_a_git_flake_without_a_signer() {
        let installable = Installable::new(
            FlakeRef::git_file("/s", None).unwrap(),
            AttrPath::new(["x"]).unwrap(),
        );
        assert_eq!(installable.render(), "git+file:///s#x");
    }

    #[test]
    fn rejects_a_relative_path() {
        assert_eq!(
            FlakeRef::path("state"),
            Err(InvalidInstallable::RelativePath(PathBuf::from("state")))
        );
    }

    #[test]
    fn rejects_a_quote_and_an_empty_segment() {
        assert!(matches!(
            AttrPath::new(["a\"b"]),
            Err(InvalidInstallable::Quote(_))
        ));
        assert_eq!(AttrPath::new([""]), Err(InvalidInstallable::EmptySegment));
        assert_eq!(
            AttrPath::new(Vec::<String>::new()),
            Err(InvalidInstallable::EmptySegment)
        );
    }

    #[test]
    fn rejects_a_key_that_is_not_base64() {
        assert!(PublicKey::new("").is_err());
        assert!(PublicKey::new("a b").is_err());
    }

    proptest! {
        #[test]
        fn an_arbitrary_path_never_leaves_a_structural_character_raw(
            bytes in proptest::collection::vec(1u8..=255, 0..40)
        ) {
            let mut dir = b"/".to_vec();
            dir.extend(bytes);
            let dir = PathBuf::from(OsStr::from_bytes(&dir));
            let rendered = Installable::new(
                FlakeRef::path(dir).unwrap(),
                AttrPath::new(["x"]).unwrap(),
            )
            .render();
            let (before, after) = rendered.split_once('#').unwrap();
            prop_assert_eq!(after, "x");
            prop_assert!(!before.contains([' ', '?', '^', '"']));
            let mut rest = before;
            while let Some(index) = rest.find('%') {
                let escape = &rest[index + 1..];
                prop_assert!(escape.len() >= 2);
                prop_assert!(escape.as_bytes()[..2].iter().all(u8::is_ascii_hexdigit));
                rest = &escape[2..];
            }
        }
    }
}
