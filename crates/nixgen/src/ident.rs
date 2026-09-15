#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ident(String);

const RESERVED_WORDS: &[&str] = &[
    "assert", "else", "if", "in", "inherit", "let", "or", "rec", "then", "with",
];

impl Ident {
    pub fn new(s: impl Into<String>) -> Result<Self, InvalidIdent> {
        let s = s.into();
        let mut chars = s.chars();
        let first_ok = chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
        let rest_ok = chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '\'' | '-'));

        if first_ok && rest_ok && !RESERVED_WORDS.contains(&s.as_str()) {
            Ok(Self(s))
        } else {
            Err(InvalidIdent(s))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, thiserror::Error)]
#[error("`{0}` is not a valid Nix identifier")]
pub struct InvalidIdent(String);

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::collection::vec;
    use proptest::prelude::*;

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
