//! Environment variable configuration for the `mix` CLI client.
//!
//! Note: Terminal display flags (`NO_COLOR`, `CLICOLOR`, `CLICOLOR_FORCE`, `TERM`, `CI`)
//! are handled upstream via `anstyle-query`.

/// Fallback mirror URL used for Nix and package resolution when `--mirror` is omitted.
pub const MIRROR: &str = "MIX_NIX_MIRROR";

/// Public signing key corresponding to the environment mirror (`MIX_NIX_MIRROR`).
pub const MIRROR_KEY: &str = "MIX_NIX_MIRROR_KEY";

/// Disables interactive progress updates. Parsed by `clap` for `--no-progress`.
pub const NO_PROGRESS: &str = "MIX_NO_PROGRESS";

/// Continuous Integration detection flag. When truthy, suppresses interactive progress bars.
pub const CI: &str = "CI";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Environment {
    pub mirror: Option<String>,
    pub mirror_key: Option<String>,
    pub ci: bool,
}

impl Environment {
    pub fn read() -> Self {
        Self::from(|name| {
            #[allow(clippy::disallowed_methods)]
            std::env::var(name).ok()
        })
    }

    fn from(read: impl Fn(&str) -> Option<String>) -> Self {
        Self {
            mirror: read(MIRROR),
            mirror_key: read(MIRROR_KEY),
            ci: ci_asks_for_plain_output(read(CI).as_deref()),
        }
    }
}

fn ci_asks_for_plain_output(ci: Option<&str>) -> bool {
    match ci.map(str::trim) {
        None | Some("") | Some("0") => false,
        Some(value) => !value.eq_ignore_ascii_case("false"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ci_variable_asks_for_plain_output_unless_it_is_unset_empty_or_false() {
        for (value, plain) in [
            (None, false),
            (Some(""), false),
            (Some("  "), false),
            (Some("0"), false),
            (Some("false"), false),
            (Some("FALSE"), false),
            (Some("1"), true),
            (Some("true"), true),
            (Some("yes"), true),
            (Some("github"), true),
        ] {
            assert_eq!(ci_asks_for_plain_output(value), plain, "CI={value:?}");
        }
    }

    #[test]
    fn each_variable_is_read_under_its_own_name() {
        let environment = Environment::from(|name| match name {
            MIRROR => Some("http://mirror.internal".to_string()),
            MIRROR_KEY => Some("mirror:KEY".to_string()),
            CI => Some("true".to_string()),
            _ => None,
        });

        assert_eq!(
            environment,
            Environment {
                mirror: Some("http://mirror.internal".to_string()),
                mirror_key: Some("mirror:KEY".to_string()),
                ci: true,
            }
        );
    }
}
