pub struct Term {
    pub text: &'static str,
    pub whole_word: bool,
}

pub const NIX_MECHANICS: [Term; 7] = [
    Term {
        text: "derivation",
        whole_word: false,
    },
    Term {
        text: ".drv",
        whole_word: false,
    },
    Term {
        text: "/nix/store/",
        whole_word: false,
    },
    Term {
        text: "flake",
        whole_word: false,
    },
    Term {
        text: "home-manager",
        whole_word: false,
    },
    Term {
        text: "attribute",
        whole_word: false,
    },
    Term {
        text: "nix build",
        whole_word: true,
    },
];

fn names(lowered: &str, term: &Term) -> bool {
    lowered.match_indices(term.text).any(|(at, _)| {
        !term.whole_word
            || !lowered[at + term.text.len()..]
                .chars()
                .next()
                .is_some_and(char::is_alphanumeric)
    })
}

pub fn nix_mechanics_in(text: &str) -> Vec<&'static str> {
    let lowered = text.to_ascii_lowercase();
    NIX_MECHANICS
        .iter()
        .filter(|term| names(&lowered, term))
        .map(|term| term.text)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_is_named_only_as_a_whole_word_and_a_noun_in_any_form() {
        assert_eq!(nix_mechanics_in("Nix builds packages"), Vec::<&str>::new());
        assert_eq!(nix_mechanics_in("run `nix build` again"), ["nix build"]);
        assert_eq!(nix_mechanics_in("two derivations failed"), ["derivation"]);
    }
}
