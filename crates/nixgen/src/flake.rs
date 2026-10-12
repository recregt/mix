use std::borrow::Cow;

use crate::ast::Key;
use crate::escape::NulByte;
use crate::print;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum System {
    X86_64Linux,
    Aarch64Linux,
    X86_64Darwin,
    Aarch64Darwin,
}

impl System {
    pub const fn as_str(self) -> &'static str {
        match self {
            System::X86_64Linux => "x86_64-linux",
            System::Aarch64Linux => "aarch64-linux",
            System::X86_64Darwin => "x86_64-darwin",
            System::Aarch64Darwin => "aarch64-darwin",
        }
    }

    pub(crate) fn key(self) -> &'static Key {
        static X86_64_LINUX: Key = Key::new_static("x86_64-linux");
        static AARCH64_LINUX: Key = Key::new_static("aarch64-linux");
        static X86_64_DARWIN: Key = Key::new_static("x86_64-darwin");
        static AARCH64_DARWIN: Key = Key::new_static("aarch64-darwin");
        match self {
            System::X86_64Linux => &X86_64_LINUX,
            System::Aarch64Linux => &AARCH64_LINUX,
            System::X86_64Darwin => &X86_64_DARWIN,
            System::Aarch64Darwin => &AARCH64_DARWIN,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rev(Cow<'static, str>);

impl Rev {
    pub const fn new_static(s: &'static str) -> Self {
        assert!(is_rev(s.as_bytes()), "not a 40 character git revision");
        Self(Cow::Borrowed(s))
    }

    pub fn parse(s: impl Into<String>) -> Result<Self, InvalidRev> {
        let s = s.into();
        if is_rev(s.as_bytes()) {
            Ok(Self(Cow::Owned(s)))
        } else {
            Err(InvalidRev(s))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, thiserror::Error)]
#[error("`{0}` is not a 40 character git revision")]
pub struct InvalidRev(String);

const fn is_rev(bytes: &[u8]) -> bool {
    if bytes.len() != 40 {
        return false;
    }
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if !(b.is_ascii_digit() || (b >= b'a' && b <= b'f')) {
            return false;
        }
        i += 1;
    }
    true
}

use crate::inputs::{Pin, Pins};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlakeHole {
    Username,
    System,
    Rev(Pin),
}

include!(concat!(env!("OUT_DIR"), "/flake_template.rs"));

#[derive(Debug)]
pub struct FlakeConfig<'a> {
    system: System,
    username: Key,
    revs: Pins<&'a str>,
}

impl<'a> FlakeConfig<'a> {
    pub fn new(
        system: System,
        username: &str,
        nixpkgs_rev: &'a Rev,
        home_manager_rev: &'a Rev,
    ) -> Result<Self, NulByte> {
        Ok(Self {
            system,
            username: Key::new(username)?,
            revs: Pins {
                home_manager: home_manager_rev.as_str(),
                nixpkgs: nixpkgs_rev.as_str(),
            },
        })
    }

    pub fn render(&self) -> String {
        let mut out = String::with_capacity(
            FLAKE_TEMPLATE_LEN
                + 2 * self.username.as_str().len()
                + 2
                + self.system.as_str().len()
                + 80,
        );
        for (index, chunk) in FLAKE_CHUNKS.iter().enumerate() {
            out.push_str(chunk);
            match FLAKE_HOLES.get(index) {
                Some(FlakeHole::Username) => print::write_key(&self.username, &mut out),
                Some(FlakeHole::System) => print::write_key(self.system.key(), &mut out),
                Some(FlakeHole::Rev(pin)) => out.push_str(self.revs.of(*pin)),
                None => {}
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::GENERATED_HEADER;
    use crate::ast::Verbatim;
    use crate::flake_write::write_flake;
    use crate::inputs::Pins;
    use crate::print::Writer;

    const SYSTEMS: [System; 4] = [
        System::X86_64Linux,
        System::Aarch64Linux,
        System::X86_64Darwin,
        System::Aarch64Darwin,
    ];

    fn written_in_full(flake: &FlakeConfig) -> String {
        let mut out = String::from(GENERATED_HEADER);
        write_flake(
            &mut Writer::new(&mut out),
            &flake.username,
            flake.system.key(),
            Pins {
                home_manager: Verbatim::unchecked(flake.revs.home_manager),
                nixpkgs: Verbatim::unchecked(flake.revs.nixpkgs),
            },
        );
        out.push('\n');
        out
    }

    proptest! {
        #[test]
        fn the_spliced_template_equals_writing_the_flake_in_full(
            username in "[^\\x00]{0,24}",
            system in 0usize..4,
        ) {
            let flake = FlakeConfig::new(SYSTEMS[system], &username, &NIXPKGS_PIN, &HOME_MANAGER_PIN).unwrap();
            prop_assert_eq!(flake.render(), written_in_full(&flake));
        }
    }

    use mix_pins::{HOME_MANAGER_REV, NIXPKGS_REV};

    static NIXPKGS_PIN: Rev = Rev::new_static(NIXPKGS_REV);
    static HOME_MANAGER_PIN: Rev = Rev::new_static(HOME_MANAGER_REV);

    fn render(username: &str) -> String {
        FlakeConfig::new(
            System::X86_64Linux,
            username,
            &NIXPKGS_PIN,
            &HOME_MANAGER_PIN,
        )
        .unwrap()
        .render()
    }

    #[test]
    fn renders_the_generated_header() {
        assert!(render("mix").starts_with(GENERATED_HEADER));
    }

    #[test]
    fn selects_the_packages_of_the_system() {
        assert!(render("mix").contains("pkgs = nixpkgs.legacyPackages.x86_64-linux;"));
    }

    #[test]
    fn renders_the_username_as_a_home_configuration_key() {
        assert!(render("mix").contains(
            "    homeConfigurations = {\n      mix = home-manager.lib.homeManagerConfiguration {"
        ));
    }

    #[test]
    fn quotes_a_username_that_is_not_an_identifier() {
        assert!(render("john.doe").contains("\"john.doe\" = home-manager.lib"));
        assert!(render("mi\"x").contains("\"mi\\\"x\" = home-manager.lib"));
    }

    #[test]
    fn references_home_nix_as_a_module_path() {
        assert!(render("mix").contains("modules = [\n          ./home.nix\n        ];"));
    }

    #[test]
    fn renders_the_pinned_nixpkgs_and_home_manager_revisions() {
        let rendered = render("mix");
        assert!(rendered.contains(&format!("url = \"github:NixOS/nixpkgs/{NIXPKGS_REV}\";")));
        assert!(rendered.contains(&format!(
            "url = \"github:nix-community/home-manager/{HOME_MANAGER_REV}\";"
        )));
        assert!(rendered.contains("follows = \"nixpkgs\";"));
    }

    #[test]
    fn rejects_a_null_byte_in_the_username() {
        assert!(
            FlakeConfig::new(
                System::X86_64Linux,
                "mi\0x",
                &NIXPKGS_PIN,
                &HOME_MANAGER_PIN
            )
            .is_err()
        );
    }

    #[test]
    fn a_parsed_revision_is_the_one_it_was_parsed_from() {
        let parsed = Rev::parse(NIXPKGS_REV).unwrap();
        assert_eq!(parsed, NIXPKGS_PIN);
        assert_eq!(parsed.as_str(), NIXPKGS_REV);
    }

    #[test]
    fn a_parsed_revision_renders_like_a_static_one() {
        let nixpkgs = Rev::parse(NIXPKGS_REV).unwrap();
        let home_manager = Rev::parse(HOME_MANAGER_REV).unwrap();
        let parsed = FlakeConfig::new(System::X86_64Linux, "mix", &nixpkgs, &home_manager).unwrap();
        assert_eq!(parsed.render(), render("mix"));
    }

    #[test]
    fn a_revision_is_forty_lowercase_hex_digits() {
        assert!(Rev::parse("").is_err());
        assert!(Rev::parse(&NIXPKGS_REV[1..]).is_err());
        assert!(Rev::parse(format!("{NIXPKGS_REV}0")).is_err());
        assert!(Rev::parse(NIXPKGS_REV.to_uppercase()).is_err());
        assert!(Rev::parse(format!("g{}", &NIXPKGS_REV[1..])).is_err());
    }

    #[test]
    fn names_every_system_nix_knows() {
        assert_eq!(System::X86_64Linux.as_str(), "x86_64-linux");
        assert_eq!(System::Aarch64Linux.as_str(), "aarch64-linux");
        assert_eq!(System::X86_64Darwin.as_str(), "x86_64-darwin");
        assert_eq!(System::Aarch64Darwin.as_str(), "aarch64-darwin");
    }
}
