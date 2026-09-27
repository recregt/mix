use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::GENERATED_HEADER;
use crate::ast::{Expr, Key, NixStr, RelPath, StrPart};
use crate::ident::{FileName, Ident, InvalidIdent};
use crate::print;

const PKGS: Ident = Ident::new_static("pkgs");

#[derive(Debug, thiserror::Error)]
pub enum InvalidInput {
    #[error("`{0}` is not a user name home-manager accepts")]
    UserName(String),
    #[error("`{}` is not an absolute UTF-8 path without a null byte", .0.display())]
    HomeDirectory(PathBuf),
    #[error(transparent)]
    Package(#[from] InvalidIdent),
}

impl InvalidInput {
    pub fn rejected(&self) -> Option<&str> {
        match self {
            InvalidInput::Package(ident) => Some(ident.input()),
            InvalidInput::UserName(_) | InvalidInput::HomeDirectory(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateVersion(&'static str);

impl StateVersion {
    pub const fn new_static(s: &'static str) -> Self {
        assert!(
            is_state_version(s.as_bytes()),
            "not a YY.05 or YY.11 release"
        );
        Self(s)
    }

    pub fn as_str(self) -> &'static str {
        self.0
    }
}

const fn is_state_version(bytes: &[u8]) -> bool {
    bytes.len() == 5
        && bytes[0].is_ascii_digit()
        && bytes[1].is_ascii_digit()
        && bytes[2] == b'.'
        && ((bytes[3] == b'0' && bytes[4] == b'5') || (bytes[3] == b'1' && bytes[4] == b'1'))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UserName(NixStr);

impl UserName {
    fn new(name: &str) -> Result<Self, InvalidInput> {
        let blank = name.chars().all(|c| matches!(c, ' ' | '\t' | '\n'));
        match NixStr::new(name) {
            Ok(text) if !blank => Ok(Self(text)),
            _ => Err(InvalidInput::UserName(name.to_owned())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HomeDir(NixStr);

impl HomeDir {
    fn new(path: &Path) -> Result<Self, InvalidInput> {
        let invalid = || InvalidInput::HomeDirectory(path.to_path_buf());
        if !path.is_absolute() {
            return Err(invalid());
        }
        let text = path.to_str().ok_or_else(invalid)?;
        NixStr::new(text).map(Self).map_err(|_| invalid())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyIntoGeneration {
    pub source: FileName,
    pub target: FileName,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomeModule {
    username: UserName,
    home_directory: HomeDir,
    state_version: StateVersion,
    packages: Vec<Ident>,
    generation_files: Vec<CopyIntoGeneration>,
}

impl HomeModule {
    pub fn new(
        username: &str,
        home_directory: &Path,
        state_version: StateVersion,
    ) -> Result<Self, InvalidInput> {
        Ok(Self {
            username: UserName::new(username)?,
            home_directory: HomeDir::new(home_directory)?,
            state_version,
            packages: Vec::new(),
            generation_files: Vec::new(),
        })
    }

    pub fn packages<I, S>(mut self, names: I) -> Result<Self, InvalidInput>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.packages = names
            .into_iter()
            .map(|name| Ident::new(name.as_ref()))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(self)
    }

    pub fn copy_into_generation(mut self, copy: CopyIntoGeneration) -> Self {
        self.generation_files.push(copy);
        self
    }

    pub fn render(&self) -> String {
        let mut out = String::with_capacity(GENERATED_HEADER.len() + 512);
        out.push_str(GENERATED_HEADER);
        print::print_function(&[PKGS], true, &self.expr(), &mut out);
        out.push('\n');
        out
    }

    fn expr(&self) -> Expr {
        let mut home = BTreeMap::new();
        home.insert(
            Key::new_static("username"),
            Expr::Str(vec![StrPart::Lit(self.username.0.clone())]),
        );
        home.insert(
            Key::new_static("homeDirectory"),
            Expr::Str(vec![StrPart::Lit(self.home_directory.0.clone())]),
        );
        home.insert(
            Key::new_static("stateVersion"),
            Expr::Str(vec![StrPart::Lit(NixStr::new_static(
                self.state_version.as_str(),
            ))]),
        );
        home.insert(
            Key::new_static("packages"),
            Expr::List(
                self.packages
                    .iter()
                    .map(|name| Expr::select(Expr::Var(PKGS), Key::from(name), []))
                    .collect(),
            ),
        );
        if !self.generation_files.is_empty() {
            home.insert(
                Key::new_static("extraBuilderCommands"),
                Expr::Str(builder_commands(&self.generation_files)),
            );
        }
        Expr::attrs([(Key::new_static("home"), Expr::Attrs(home))])
    }
}

fn builder_commands(copies: &[CopyIntoGeneration]) -> Vec<StrPart> {
    let mut parts = Vec::with_capacity(copies.len() * 3);
    for (index, copy) in copies.iter().enumerate() {
        let lead = if index == 0 { "cp " } else { "\ncp " };
        parts.push(StrPart::Lit(NixStr::new_static(lead)));
        parts.push(StrPart::Interp(Expr::Path(RelPath::from(&copy.source))));
        parts.push(StrPart::Lit(
            NixStr::new(format!(" $out/{}", copy.target.as_str()))
                .expect("a file name never holds a null byte"),
        ));
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATE_VERSION: StateVersion = StateVersion::new_static("24.05");

    fn module() -> HomeModule {
        HomeModule::new("mix", Path::new("/home/mix"), STATE_VERSION).unwrap()
    }

    fn copy(source: &'static str, target: &'static str) -> CopyIntoGeneration {
        CopyIntoGeneration {
            source: FileName::new_static(source),
            target: FileName::new_static(target),
        }
    }

    #[test]
    fn every_render_starts_with_the_generated_header() {
        assert!(module().render().starts_with(GENERATED_HEADER));
        assert!(GENERATED_HEADER.contains("DO NOT EDIT"));
    }

    #[test]
    fn renders_every_field_under_home_in_key_order() {
        let rendered = module()
            .packages(["firefox", "git"])
            .unwrap()
            .copy_into_generation(copy("state", "mix-state"))
            .render();
        assert!(rendered.ends_with(
            "{ pkgs, ... }: {\n  home = {\n    \
             extraBuilderCommands = \"cp ${./state} $out/mix-state\";\n    \
             homeDirectory = \"/home/mix\";\n    \
             packages = [\n      pkgs.firefox\n      pkgs.git\n    ];\n    \
             stateVersion = \"24.05\";\n    \
             username = \"mix\";\n  };\n}\n"
        ));
    }

    #[test]
    fn leaves_out_builder_commands_when_nothing_is_copied() {
        assert!(!module().render().contains("extraBuilderCommands"));
    }

    #[test]
    fn joins_several_copies_with_newlines() {
        let rendered = module()
            .copy_into_generation(copy("a", "b"))
            .copy_into_generation(copy("c", "d"))
            .render();
        assert!(rendered.contains("\"cp ${./a} $out/b\ncp ${./c} $out/d\""));
    }

    #[test]
    fn a_rejected_package_name_is_handed_back() {
        let error = module().packages(["git", "rip grep"]).unwrap_err();
        assert_eq!(error.rejected(), Some("rip grep"));
    }

    #[test]
    fn rejects_a_package_name_that_is_a_nix_keyword() {
        assert!(module().packages(["firefox", "in"]).is_err());
    }

    #[test]
    fn rejects_a_blank_or_null_user_name() {
        for name in ["", " \t\n", "mi\0x"] {
            assert!(matches!(
                HomeModule::new(name, Path::new("/home/mix"), STATE_VERSION),
                Err(InvalidInput::UserName(_))
            ));
        }
    }

    #[test]
    fn rejects_a_relative_home_directory() {
        assert!(matches!(
            HomeModule::new("mix", Path::new("home/mix"), STATE_VERSION),
            Err(InvalidInput::HomeDirectory(_))
        ));
    }

    #[test]
    fn rejects_a_home_directory_that_is_not_utf8() {
        use std::os::unix::ffi::OsStrExt;
        let path = Path::new(std::ffi::OsStr::from_bytes(b"/home/\xff"));
        assert!(matches!(
            HomeModule::new("mix", path, STATE_VERSION),
            Err(InvalidInput::HomeDirectory(_))
        ));
    }

    #[test]
    fn escapes_a_user_name_instead_of_breaking_out_of_the_string() {
        let rendered = HomeModule::new("a\"${b}", Path::new("/h"), STATE_VERSION)
            .unwrap()
            .render();
        assert!(rendered.contains(r#"username = "a\"\${b}";"#));
    }

    #[test]
    fn a_state_version_is_a_release() {
        assert!(is_state_version(b"24.05"));
        assert!(is_state_version(b"25.11"));
        assert!(!is_state_version(b"24.5"));
        assert!(!is_state_version(b"24.06"));
    }
}
