use crate::GENERATED_HEADER;
use crate::escape::NulByte;
use crate::ident::{FileName, Ident, InvalidIdent};
use crate::value::Nix;

#[derive(Debug, Default)]
pub struct HomeManagerConfig {
    root: Vec<(Ident, Nix)>,
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
        let idents = pkgs
            .into_iter()
            .map(|p| Ident::new(p.as_ref()))
            .collect::<Result<Vec<_>, _>>()?;
        self.set_at(&["home", "packages"], Nix::PackageList(idents))?;
        Ok(self)
    }

    pub fn copy_into_generation(
        &mut self,
        source: &str,
        target: &str,
    ) -> Result<&mut Self, InvalidInput> {
        let value = Nix::CopyIntoGeneration {
            source: FileName::new(source)?,
            target: FileName::new(target)?,
        };
        self.set_at(&["home", "extraBuilderCommands"], value)?;
        Ok(self)
    }

    pub fn set_bool(&mut self, path: &str, value: bool) -> Result<&mut Self, InvalidInput> {
        self.set(path, Nix::Bool(value))
    }

    pub fn set_str(&mut self, path: &str, value: &str) -> Result<&mut Self, InvalidInput> {
        let value = Nix::str(value)?;
        self.set(path, value)
    }

    pub fn render(&self) -> String {
        format!(
            "{GENERATED_HEADER}{{ pkgs, ... }}:\n{}\n",
            Nix::Attrs(self.root.clone()).render()
        )
    }

    fn set(&mut self, path: &str, value: Nix) -> Result<&mut Self, InvalidInput> {
        if path.is_empty() {
            return Err(InvalidInput::EmptyPath);
        }
        let segments: Vec<&str> = path.split('.').collect();
        self.set_at(&segments, value)?;
        Ok(self)
    }

    fn set_at(&mut self, segments: &[&str], value: Nix) -> Result<(), InvalidInput> {
        let idents = segments
            .iter()
            .map(|s| Ident::new(*s))
            .collect::<Result<Vec<_>, _>>()?;
        insert_nested(&mut self.root, &idents, value);
        Ok(())
    }
}

fn insert_nested(entries: &mut Vec<(Ident, Nix)>, path: &[Ident], value: Nix) {
    let (head, rest) = path.split_first().expect("path segments are never empty");

    if rest.is_empty() {
        if let Some(existing) = entries.iter_mut().find(|(k, _)| k == head) {
            existing.1 = value;
        } else {
            entries.push((head.clone(), value));
        }
        return;
    }

    if let Some(existing) = entries.iter_mut().find(|(k, _)| k == head) {
        if !matches!(existing.1, Nix::Attrs(_)) {
            existing.1 = Nix::Attrs(Vec::new());
        }
        if let Nix::Attrs(children) = &mut existing.1 {
            insert_nested(children, rest, value);
        }
        return;
    }

    let mut children = Vec::new();
    insert_nested(&mut children, rest, value);
    entries.push((head.clone(), Nix::Attrs(children)));
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
            "{ pkgs, ... }:\n{\n  home = {\n    packages = with pkgs; [ firefox git ];\n  };\n}\n"
        ));
    }

    #[test]
    fn copy_into_generation_renders_beside_the_packages() {
        let mut cfg = HomeManagerConfig::new();
        cfg.packages(["git"]).unwrap();
        cfg.copy_into_generation("state", "mix-state").unwrap();
        assert!(cfg.render().ends_with(
            "{ pkgs, ... }:\n{\n  home = {\n    packages = with pkgs; [ git ];\n    \
             extraBuilderCommands = \"cp ${./state} $out/mix-state\";\n  };\n}\n"
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
            "{ pkgs, ... }:\n{\n  programs = {\n    git = {\n      enable = true;\n    };\n  };\n}\n"
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
