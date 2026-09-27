use std::borrow::Cow;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Ident(pub(crate) Cow<'static, str>);

const RESERVED_WORDS: &[&str] = &[
    "assert", "else", "if", "in", "inherit", "let", "or", "rec", "then", "with",
];

const fn bytes_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

const fn is_reserved(bytes: &[u8]) -> bool {
    let mut i = 0;
    while i < RESERVED_WORDS.len() {
        if bytes_eq(RESERVED_WORDS[i].as_bytes(), bytes) {
            return true;
        }
        i += 1;
    }
    false
}

pub(crate) const fn is_identifier_bytes(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    let first = bytes[0];
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return false;
    }
    let mut i = 1;
    while i < bytes.len() {
        let b = bytes[i];
        if !(b.is_ascii_alphanumeric() || b == b'_' || b == b'\'' || b == b'-') {
            return false;
        }
        i += 1;
    }
    !is_reserved(bytes)
}

const fn is_file_name_bytes(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if !(b.is_ascii_lowercase() || b.is_ascii_digit() || (b == b'-' && i > 0)) {
            return false;
        }
        i += 1;
    }
    true
}

impl Ident {
    pub fn new(s: impl Into<String>) -> Result<Self, InvalidIdent> {
        let s = s.into();
        if is_identifier_bytes(s.as_bytes()) {
            Ok(Self(Cow::Owned(s)))
        } else {
            Err(InvalidIdent(s))
        }
    }

    pub const fn new_static(s: &'static str) -> Self {
        assert!(
            is_identifier_bytes(s.as_bytes()),
            "not a valid nix identifier"
        );
        Self(Cow::Borrowed(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, thiserror::Error)]
#[error("`{0}` is not a valid Nix identifier")]
pub struct InvalidIdent(String);

impl InvalidIdent {
    pub fn input(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileName(Cow<'static, str>);

impl FileName {
    pub fn new(s: impl Into<String>) -> Result<Self, InvalidIdent> {
        let s = s.into();
        if is_file_name_bytes(s.as_bytes()) {
            Ok(Self(Cow::Owned(s)))
        } else {
            Err(InvalidIdent(s))
        }
    }

    pub const fn new_static(s: &'static str) -> Self {
        assert!(is_file_name_bytes(s.as_bytes()), "not a valid file name");
        Self(Cow::Borrowed(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub fn is_identifier(s: &str) -> bool {
    is_identifier_bytes(s.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::collection::vec;
    use proptest::prelude::*;

    #[test]
    fn a_file_name_is_lowercase_letters_digits_and_dashes() {
        for name in ["state", "mix-state", "a1", "9"] {
            assert!(FileName::new(name).is_ok(), "{name}");
        }
        for name in [
            "", "-state", "State", "a b", "a/b", "a$b", "a'b", "..", "a.b", "a}",
        ] {
            assert!(FileName::new(name).is_err(), "{name:?}");
        }
    }

    proptest! {
        #[test]
        fn a_file_name_never_holds_a_character_nix_or_a_shell_would_read(s in ".*") {
            if let Ok(name) = FileName::new(s) {
                prop_assert!(name.as_str().bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'));
                prop_assert!(!name.as_str().starts_with('-'));
            }
        }
    }

    const CONST_IDENT: Ident = Ident::new_static("home-manager");
    const CONST_FILE: FileName = FileName::new_static("mix-state");

    #[test]
    fn a_literal_is_checked_at_compile_time() {
        assert_eq!(CONST_IDENT.as_str(), "home-manager");
        assert_eq!(CONST_FILE.as_str(), "mix-state");
    }

    #[test]
    fn accepts_a_plain_identifier() {
        assert!(Ident::new("firefox").is_ok());
    }

    #[test]
    fn accepts_underscore_apostrophe_and_hyphen() {
        assert!(Ident::new("_foo").is_ok());
        assert!(Ident::new("foo'bar").is_ok());
        assert!(Ident::new("node-sass").is_ok());
    }

    #[test]
    fn rejects_an_empty_string() {
        assert!(Ident::new("").is_err());
    }

    #[test]
    fn rejects_a_leading_digit() {
        assert!(Ident::new("1password").is_err());
    }

    #[test]
    fn rejects_a_dot() {
        assert!(Ident::new("programs.git").is_err());
    }

    #[test]
    fn rejects_whitespace_and_quotes() {
        assert!(Ident::new("foo bar").is_err());
        assert!(Ident::new("foo\"bar").is_err());
        assert!(Ident::new("foo;bar").is_err());
    }

    #[test]
    fn rejects_nix_reserved_words() {
        for word in RESERVED_WORDS {
            assert!(Ident::new(*word).is_err(), "{word} should be rejected");
        }
    }

    #[test]
    fn accepts_identifiers_that_merely_contain_a_reserved_word() {
        assert!(Ident::new("inherit-x").is_ok());
        assert!(Ident::new("with_pkgs").is_ok());
    }

    proptest! {
        #[test]
        fn arbitrary_input_never_panics_and_accepted_values_match_the_charset(
            raw in vec(any::<char>(), 0..20).prop_map(|cs| cs.into_iter().collect::<String>())
        ) {
            if let Ok(ident) = Ident::new(raw) {
                let mut chars = ident.as_str().chars();
                let first = chars.next().unwrap();
                prop_assert!(first.is_ascii_alphabetic() || first == '_');
                prop_assert!(
                    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '\'' | '-'))
                );
            }
        }
    }
}
