use crate::escape::{NulByte, nix_string_literal};
use crate::ident::Ident;

#[derive(Debug, Clone)]
pub enum Nix {
    Str(String),
    Bool(bool),
    PackageList(Vec<Ident>),
    Attrs(Vec<(Ident, Nix)>),
}

impl Nix {
    pub fn str(raw: &str) -> Result<Self, NulByte> {
        Ok(Nix::Str(nix_string_literal(raw)?))
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        self.write(0, &mut out);
        out
    }

    fn write(&self, depth: usize, out: &mut String) {
        match self {
            Nix::Str(escaped) => out.push_str(escaped),
            Nix::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Nix::PackageList(pkgs) => {
                out.push_str("with pkgs; [");
                for pkg in pkgs {
                    out.push(' ');
                    out.push_str(pkg.as_str());
                }
                out.push_str(" ]");
            }
            Nix::Attrs(entries) => {
                out.push_str("{\n");
                let indent = "  ".repeat(depth + 1);
                for (key, value) in entries {
                    out.push_str(&indent);
                    out.push_str(key.as_str());
                    out.push_str(" = ");
                    value.write(depth + 1, out);
                    out.push_str(";\n");
                }
                out.push_str(&"  ".repeat(depth));
                out.push('}');
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ident(s: &str) -> Ident {
        Ident::new(s).unwrap()
    }

    #[test]
    fn renders_a_bool() {
        assert_eq!(Nix::Bool(true).render(), "true");
        assert_eq!(Nix::Bool(false).render(), "false");
    }

    #[test]
    fn renders_an_escaped_string() {
        assert_eq!(Nix::str("a\"b").unwrap().render(), r#""a\"b""#);
    }

    #[test]
    fn renders_an_empty_package_list() {
        assert_eq!(Nix::PackageList(vec![]).render(), "with pkgs; [ ]");
    }

    #[test]
    fn renders_a_package_list() {
        let list = Nix::PackageList(vec![ident("firefox"), ident("git")]);
        assert_eq!(list.render(), "with pkgs; [ firefox git ]");
    }

    #[test]
    fn renders_a_flat_attrset() {
        let attrs = Nix::Attrs(vec![(ident("enable"), Nix::Bool(true))]);
        assert_eq!(attrs.render(), "{\n  enable = true;\n}");
    }

    #[test]
    fn renders_a_nested_attrset_with_increasing_indent() {
        let attrs = Nix::Attrs(vec![(
            ident("programs"),
            Nix::Attrs(vec![(
                ident("git"),
                Nix::Attrs(vec![(ident("enable"), Nix::Bool(true))]),
            )]),
        )]);

        let expected = "{\n  programs = {\n    git = {\n      enable = true;\n    };\n  };\n}";
        assert_eq!(attrs.render(), expected);
    }
}
