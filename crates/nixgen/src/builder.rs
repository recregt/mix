use std::collections::BTreeMap;

use crate::GENERATED_HEADER;
use crate::ast::{Expr, Key, NixStr, RelPath, StrPart};
use crate::escape::NulByte;
use crate::ident::{FileName, Ident, InvalidIdent};
use crate::print;

const PKGS: Ident = Ident::new_static("pkgs");

#[derive(Debug)]
pub struct HomeManagerConfig {
    root: Expr,
}

impl Default for HomeManagerConfig {
    fn default() -> Self {
        Self {
            root: Expr::Attrs(BTreeMap::new()),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum InvalidInput {
    #[error("path must not be empty")]
    EmptyPath,
    #[error(transparent)]
    Segment(#[from] InvalidIdent),
    #[error(transparent)]
    Value(#[from] NulByte),
}

impl InvalidInput {
    pub fn rejected(&self) -> Option<&str> {
        match self {
            InvalidInput::Segment(ident) => Some(ident.input()),
            InvalidInput::EmptyPath | InvalidInput::Value(_) => None,
        }
    }
}

impl HomeManagerConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn packages<I, S>(&mut self, pkgs: I) -> Result<&mut Self, InvalidInput>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let selections = pkgs
            .into_iter()
            .map(|p| {
                Ident::new(p.as_ref())
                    .map(|name| Expr::select(Expr::Var(PKGS), Key::from(&name), []))
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.set_at(&["home", "packages"], Expr::List(selections))?;
        Ok(self)
    }

    pub fn copy_into_generation(
        &mut self,
        source: &str,
        target: &str,
    ) -> Result<&mut Self, InvalidInput> {
        let source = FileName::new(source)?;
        let target = FileName::new(target)?;
        let value = Expr::Str(vec![
            StrPart::Lit(NixStr::new_static("cp ")),
            StrPart::Interp(Expr::Path(RelPath::from(&source))),
            StrPart::Lit(NixStr::new(format!(" $out/{}", target.as_str()))?),
        ]);
        self.set_at(&["home", "extraBuilderCommands"], value)?;
        Ok(self)
    }

    pub fn set_bool(&mut self, path: &str, value: bool) -> Result<&mut Self, InvalidInput> {
        self.set(path, Expr::Bool(value))
    }

    pub fn set_str(&mut self, path: &str, value: &str) -> Result<&mut Self, InvalidInput> {
        let value = Expr::string(value)?;
        self.set(path, value)
    }

    pub fn render(&self) -> String {
        let mut out = String::with_capacity(GENERATED_HEADER.len() + 512);
        out.push_str(GENERATED_HEADER);
        print::print_function(&[PKGS], true, &self.root, &mut out);
        out.push('\n');
        out
    }

    fn set(&mut self, path: &str, value: Expr) -> Result<&mut Self, InvalidInput> {
        if path.is_empty() {
            return Err(InvalidInput::EmptyPath);
        }
        let segments: Vec<&str> = path.split('.').collect();
        self.set_at(&segments, value)?;
        Ok(self)
    }

    fn set_at(&mut self, segments: &[&str], value: Expr) -> Result<(), InvalidInput> {
        let keys = segments
            .iter()
            .map(|s| Ident::new(*s).map(|ident| Key::from(&ident)))
            .collect::<Result<Vec<_>, _>>()?;
        let Expr::Attrs(root) = &mut self.root else {
            unreachable!("the module body is always an attribute set")
        };
        insert_nested(root, &keys, value);
        Ok(())
    }
}

fn insert_nested(entries: &mut BTreeMap<Key, Expr>, path: &[Key], value: Expr) {
    let (head, rest) = path.split_first().expect("path segments are never empty");

    if rest.is_empty() {
        entries.insert(head.clone(), value);
        return;
    }

    let child = entries
        .entry(head.clone())
        .or_insert_with(|| Expr::Attrs(BTreeMap::new()));
    if !matches!(child, Expr::Attrs(_)) {
        *child = Expr::Attrs(BTreeMap::new());
    }
    if let Expr::Attrs(children) = child {
        insert_nested(children, rest, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_render_starts_with_the_generated_header() {
        let cfg = HomeManagerConfig::new();
        assert!(cfg.render().starts_with(GENERATED_HEADER));
        assert!(GENERATED_HEADER.contains("DO NOT EDIT"));
    }

    #[test]
    fn packages_renders_under_home_packages() {
        let mut cfg = HomeManagerConfig::new();
        cfg.packages(["firefox", "git"]).unwrap();
        assert!(cfg.render().ends_with(
            "{ pkgs, ... }: {\n  home = {\n    packages = [\n      pkgs.firefox\n      pkgs.git\n    ];\n  };\n}\n"
        ));
    }

    #[test]
    fn copy_into_generation_renders_beside_the_packages() {
        let mut cfg = HomeManagerConfig::new();
        cfg.packages(["git"]).unwrap();
        cfg.copy_into_generation("state", "mix-state").unwrap();
        assert!(cfg.render().ends_with(
            "{ pkgs, ... }: {\n  home = {\n    extraBuilderCommands = \"cp ${./state} $out/mix-state\";\n    \
             packages = [\n      pkgs.git\n    ];\n  };\n}\n"
        ));
    }

    #[test]
    fn a_rejected_package_name_is_handed_back() {
        let mut cfg = HomeManagerConfig::new();
        let error = cfg.packages(["git", "rip grep"]).unwrap_err();

        assert_eq!(error.rejected(), Some("rip grep"));
    }

    #[test]
    fn copy_into_generation_refuses_a_name_that_could_escape() {
        let mut cfg = HomeManagerConfig::new();
        assert!(cfg.copy_into_generation("state} $(rm -rf)", "x").is_err());
        assert!(cfg.copy_into_generation("state", "x; rm -rf ~").is_err());
    }

    #[test]
    fn set_bool_nests_a_dotted_path() {
        let mut cfg = HomeManagerConfig::new();
        cfg.set_bool("programs.git.enable", true).unwrap();
        assert!(cfg.render().ends_with(
            "{ pkgs, ... }: {\n  programs = {\n    git = {\n      enable = true;\n    };\n  };\n}\n"
        ));
    }

    #[test]
    fn two_leaves_under_the_same_prefix_merge_instead_of_overwriting() {
        let mut cfg = HomeManagerConfig::new();
        cfg.set_bool("programs.git.enable", true).unwrap();
        cfg.set_str("programs.git.userName", "mix").unwrap();

        let rendered = cfg.render();
        assert!(rendered.contains("enable = true;"));
        assert!(rendered.contains("userName = \"mix\";"));
    }

    #[test]
    fn setting_the_same_path_twice_overwrites_rather_than_duplicating() {
        let mut cfg = HomeManagerConfig::new();
        cfg.set_bool("programs.git.enable", true).unwrap();
        cfg.set_bool("programs.git.enable", false).unwrap();

        let rendered = cfg.render();
        assert_eq!(rendered.matches("enable").count(), 1);
        assert!(rendered.contains("enable = false;"));
    }

    #[test]
    fn rejects_an_empty_path() {
        let mut cfg = HomeManagerConfig::new();
        assert!(matches!(
            cfg.set_bool("", true),
            Err(InvalidInput::EmptyPath)
        ));
    }

    #[test]
    fn rejects_a_path_with_an_empty_segment() {
        let mut cfg = HomeManagerConfig::new();
        assert!(matches!(
            cfg.set_bool("programs..git", true),
            Err(InvalidInput::Segment(_))
        ));
    }

    #[test]
    fn rejects_an_invalid_package_name() {
        let mut cfg = HomeManagerConfig::new();
        assert!(cfg.packages(["firefox; rm -rf /"]).is_err());
    }

    #[test]
    fn rejects_a_package_name_that_is_a_nix_keyword() {
        let mut cfg = HomeManagerConfig::new();
        assert!(cfg.packages(["firefox", "in"]).is_err());
    }

    #[test]
    fn rejects_a_null_byte_in_a_string_value() {
        let mut cfg = HomeManagerConfig::new();
        assert!(matches!(
            cfg.set_str("programs.git.userName", "a\0b"),
            Err(InvalidInput::Value(_))
        ));
    }
}
